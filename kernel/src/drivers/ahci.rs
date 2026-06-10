// AHCI (SATA) driver for Atom OS
// Provides block-level access to SATA disks

#![allow(dead_code, static_mut_refs)]

use crate::mm::pmm;
use alloc::vec::Vec;
use core::ptr;
use spin::Mutex;

// AHCI register offsets
const AHCI_CAP: usize = 0x00; // Host Capabilities
const AHCI_GHC: usize = 0x04; // Global Host Control
const AHCI_IS: usize = 0x08; // Interrupt Status
const AHCI_PI: usize = 0x0C; // Ports Implemented
const AHCI_VS: usize = 0x10; // Version
const AHCI_PORT_BASE: usize = 0x100;
const AHCI_PORT_SIZE: usize = 0x80;

// Port register offsets
const PORT_CLB: usize = 0x00; // Command List Base Address
const PORT_CLBU: usize = 0x04; // Command List Base Address Upper
const PORT_FB: usize = 0x08; // FIS Base Address
const PORT_FBU: usize = 0x0C; // FIS Base Address Upper
const PORT_IS: usize = 0x10; // Interrupt Status
const PORT_IE: usize = 0x14; // Interrupt Enable
const PORT_CMD: usize = 0x18; // Command and Status
const PORT_TFD: usize = 0x20; // Task File Data
const PORT_SIG: usize = 0x24; // Signature
const PORT_SSTS: usize = 0x28; // SATA Status
const PORT_SCTL: usize = 0x2C; // SATA Control
const PORT_SERR: usize = 0x30; // SATA Error
const PORT_SACT: usize = 0x34; // SATA Active
const PORT_CI: usize = 0x38; // Command Issue

// Port CMD bits
const PORT_CMD_ST: u32 = 1 << 0; // Start
const PORT_CMD_FRE: u32 = 1 << 4; // FIS Receive Enable
const PORT_CMD_FR: u32 = 1 << 14; // FIS Receive Running
const PORT_CMD_CR: u32 = 1 << 15; // Command List Running

// GHC bits
const GHC_AE: u32 = 1 << 31; // AHCI Enable
const GHC_IE: u32 = 1 << 1; // Interrupt Enable
const GHC_HR: u32 = 1 << 0; // HBA Reset

// SATA signatures
const SATA_SIG_ATA: u32 = 0x00000101;
const SATA_SIG_ATAPI: u32 = 0xEB140101;

// FIS types
const FIS_TYPE_REG_H2D: u8 = 0x27;

// ATA commands
const ATA_CMD_READ_DMA_EX: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EX: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;
const ATA_CMD_IDENTIFY: u8 = 0xEC;

// PCI configuration for AHCI
const AHCI_CLASS: u8 = 0x01; // Mass Storage
const AHCI_SUBCLASS: u8 = 0x06; // SATA
const AHCI_PROGIF: u8 = 0x01; // AHCI

// Command header
#[repr(C)]
struct CommandHeader {
    flags: u16,
    prdtl: u16, // Physical Region Descriptor Table Length
    prdbc: u32, // PRD Byte Count
    ctba: u32,  // Command Table Base Address
    ctbau: u32, // Command Table Base Address Upper
    reserved: [u32; 4],
}

// Command table
#[repr(C)]
struct CommandTable {
    cfis: [u8; 64], // Command FIS
    acmd: [u8; 16], // ATAPI Command
    reserved: [u8; 48],
    prdt: [PrdtEntry; 8], // Physical Region Descriptor Table
}

// PRDT entry
#[repr(C)]
struct PrdtEntry {
    dba: u32,  // Data Base Address
    dbau: u32, // Data Base Address Upper
    reserved: u32,
    dbc: u32, // Data Byte Count (bit 31 = Interrupt on Completion)
}

// FIS Register H2D
#[repr(C)]
struct FisRegH2D {
    fis_type: u8,
    flags: u8, // bit 7: C (command), bits 0-3: PM Port
    command: u8,
    feature_lo: u8,
    lba0: u8,
    lba1: u8,
    lba2: u8,
    device: u8,
    lba3: u8,
    lba4: u8,
    lba5: u8,
    feature_hi: u8,
    count_lo: u8,
    count_hi: u8,
    icc: u8,
    control: u8,
    reserved: [u8; 4],
}

static mut AHCI_BASE: usize = 0;
static mut ACTIVE_PORT: Option<u32> = None;
static mut CMD_LIST_BASE: usize = 0;
static mut FIS_BASE: usize = 0;
static mut CMD_TABLE_BASE: usize = 0;
static mut DATA_BUFFER: usize = 0;
static AHCI_IO_LOCK: Mutex<()> = Mutex::new(());

pub fn init() -> bool {
    // Find AHCI controller via PCI
    if let Some(base) = find_ahci_controller() {
        unsafe {
            crate::log_info!("ahci", "Found AHCI controller at 0x{:X}", base);

            // ABAR is CPU-accessed through the shared kernel higher half.
            // The controller still receives physical addresses for DMA below.
            AHCI_BASE = match crate::mm::vm::map_kernel_mmio(base, 0x2000) {
                Ok(virt) => virt,
                Err(err) => {
                    crate::log_error!("ahci", "Failed to map MMIO BAR 0x{:X}: {:?}", base, err);
                    return false;
                }
            };
            crate::log_debug!(
                "ahci",
                "MMIO mapped: phys=0x{:X} virt=0x{:X}",
                base,
                AHCI_BASE
            );

            // Enable AHCI mode
            let ghc = read_reg(AHCI_GHC);
            write_reg(AHCI_GHC, ghc | GHC_AE);

            // Find first active port
            let pi = read_reg(AHCI_PI);
            crate::log_debug!("ahci", "Ports implemented: 0x{:X}", pi);

            for port in 0..32 {
                if (pi >> port) & 1 != 0 && init_port(port) {
                    ACTIVE_PORT = Some(port);
                    crate::log_info!("ahci", "Initialized port {}", port);
                    return true;
                }
            }
        }
    }

    crate::log_warn!("ahci", "No AHCI controller or SATA device found");
    false
}

fn find_ahci_controller() -> Option<usize> {
    // Scan PCI bus for AHCI controller
    // For Q35 machine, AHCI is typically at bus 0, device 0x1f, function 2

    for bus in 0..256u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = pci_config_address(bus as u8, device, function, 0);
                let vendor_device = pci_read_config(address);

                if vendor_device == 0xFFFFFFFF || vendor_device == 0 {
                    continue;
                }

                // Read class code
                let class_addr = pci_config_address(bus as u8, device, function, 0x08);
                let class_info = pci_read_config(class_addr);
                let class = ((class_info >> 24) & 0xFF) as u8;
                let subclass = ((class_info >> 16) & 0xFF) as u8;
                let progif = ((class_info >> 8) & 0xFF) as u8;

                if class == AHCI_CLASS && subclass == AHCI_SUBCLASS && progif == AHCI_PROGIF {
                    // Found AHCI controller - get BAR5 (ABAR)
                    let bar5_addr = pci_config_address(bus as u8, device, function, 0x24);
                    let bar5 = pci_read_config(bar5_addr) as usize;

                    // Enable bus mastering
                    let cmd_addr = pci_config_address(bus as u8, device, function, 0x04);
                    let cmd = pci_read_config(cmd_addr);
                    pci_write_config(cmd_addr, cmd | 0x06); // Enable memory space + bus master

                    return Some(bar5 & !0xF);
                }
            }
        }
    }
    None
}

fn pci_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
        | 0x80000000
}

fn pci_read_config(address: u32) -> u32 {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xCF8u16,
            in("eax") address,
        );
        let mut value: u32;
        core::arch::asm!(
            "in eax, dx",
            out("eax") value,
            in("dx") 0xCFCu16,
        );
        value
    }
}

fn pci_write_config(address: u32, value: u32) {
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xCF8u16,
            in("eax") address,
        );
        core::arch::asm!(
            "out dx, eax",
            in("dx") 0xCFCu16,
            in("eax") value,
        );
    }
}

unsafe fn read_reg(offset: usize) -> u32 {
    ptr::read_volatile((AHCI_BASE + offset) as *const u32)
}

unsafe fn write_reg(offset: usize, value: u32) {
    ptr::write_volatile((AHCI_BASE + offset) as *mut u32, value);
}

unsafe fn read_port_reg(port: u32, offset: usize) -> u32 {
    let addr = AHCI_BASE + AHCI_PORT_BASE + (port as usize) * AHCI_PORT_SIZE + offset;
    ptr::read_volatile(addr as *const u32)
}

unsafe fn write_port_reg(port: u32, offset: usize, value: u32) {
    let addr = AHCI_BASE + AHCI_PORT_BASE + (port as usize) * AHCI_PORT_SIZE + offset;
    ptr::write_volatile(addr as *mut u32, value);
}

unsafe fn init_port(port: u32) -> bool {
    // Check if device is present
    let ssts = read_port_reg(port, PORT_SSTS);
    let det = ssts & 0x0F;
    let ipm = (ssts >> 8) & 0x0F;

    if det != 3 || ipm != 1 {
        return false; // No device or not active
    }

    // Check device signature
    let sig = read_port_reg(port, PORT_SIG);
    if sig != SATA_SIG_ATA {
        crate::log_debug!("ahci", "Port {} has non-ATA device (sig=0x{:X})", port, sig);
        return false;
    }

    // Stop command engine
    stop_cmd(port);

    // Allocate memory for command structures
    // We need: Command List (1KB), FIS (256B), Command Table (256B), Data Buffer (64KB)
    let pages = pmm::alloc_pages_zeroed(32).expect("Failed to allocate AHCI memory");
    CMD_LIST_BASE = pages;
    FIS_BASE = pages + 1024;
    CMD_TABLE_BASE = pages + 2048;
    DATA_BUFFER = pages + 4096;

    // Set up command list and FIS base
    write_port_reg(port, PORT_CLB, CMD_LIST_BASE as u32);
    write_port_reg(port, PORT_CLBU, 0);
    write_port_reg(port, PORT_FB, FIS_BASE as u32);
    write_port_reg(port, PORT_FBU, 0);

    // Clear interrupt status
    write_port_reg(port, PORT_IS, 0xFFFFFFFF);

    // Set up command header 0
    let cmd_header = crate::mm::vm::phys_to_virt_ptr(CMD_LIST_BASE) as *mut CommandHeader;
    (*cmd_header).ctba = CMD_TABLE_BASE as u32;
    (*cmd_header).ctbau = 0;

    // Start command engine
    start_cmd(port);

    true
}

unsafe fn stop_cmd(port: u32) {
    let cmd = read_port_reg(port, PORT_CMD);
    write_port_reg(port, PORT_CMD, cmd & !(PORT_CMD_ST | PORT_CMD_FRE));

    // Wait for command engine to stop
    for _ in 0..1000000 {
        let cmd = read_port_reg(port, PORT_CMD);
        if (cmd & (PORT_CMD_FR | PORT_CMD_CR)) == 0 {
            break;
        }
    }
}

unsafe fn start_cmd(port: u32) {
    // Wait until CR is cleared
    for _ in 0..1000000 {
        if (read_port_reg(port, PORT_CMD) & PORT_CMD_CR) == 0 {
            break;
        }
    }

    let cmd = read_port_reg(port, PORT_CMD);
    write_port_reg(port, PORT_CMD, cmd | PORT_CMD_FRE | PORT_CMD_ST);
}

/// Read sectors from the disk
/// lba: Starting sector number
/// count: Number of sectors to read (max 128)
/// Returns: Pointer to data buffer on success
pub fn read_sectors(lba: u64, count: u16) -> Option<Vec<u8>> {
    let _guard = AHCI_IO_LOCK.lock();
    unsafe {
        let port = ACTIVE_PORT?;
        if count == 0 || count > 128 {
            return None;
        }

        // Set up command header
        let cmd_header = crate::mm::vm::phys_to_virt_ptr(CMD_LIST_BASE) as *mut CommandHeader;
        (*cmd_header).flags = 5 | (1 << 6); // 5 DWORDs in FIS, clear busy on ok
        (*cmd_header).prdtl = 1;
        (*cmd_header).prdbc = 0;

        // Set up command table
        let cmd_table = crate::mm::vm::phys_to_virt_ptr(CMD_TABLE_BASE) as *mut CommandTable;
        ptr::write_bytes(cmd_table, 0, 1);

        // Set up PRDT entry
        (*cmd_table).prdt[0].dba = DATA_BUFFER as u32;
        (*cmd_table).prdt[0].dbau = 0;
        (*cmd_table).prdt[0].dbc = ((count as u32) * 512 - 1) | (1 << 31);

        // Build FIS
        let fis = &mut (*cmd_table).cfis as *mut [u8; 64] as *mut FisRegH2D;
        (*fis).fis_type = FIS_TYPE_REG_H2D;
        (*fis).flags = 0x80; // Command
        (*fis).command = ATA_CMD_READ_DMA_EX;
        (*fis).device = 0x40; // LBA mode

        (*fis).lba0 = (lba & 0xFF) as u8;
        (*fis).lba1 = ((lba >> 8) & 0xFF) as u8;
        (*fis).lba2 = ((lba >> 16) & 0xFF) as u8;
        (*fis).lba3 = ((lba >> 24) & 0xFF) as u8;
        (*fis).lba4 = ((lba >> 32) & 0xFF) as u8;
        (*fis).lba5 = ((lba >> 40) & 0xFF) as u8;

        (*fis).count_lo = (count & 0xFF) as u8;
        (*fis).count_hi = ((count >> 8) & 0xFF) as u8;

        // Issue command
        write_port_reg(port, PORT_CI, 1);

        // Wait for completion
        for _ in 0..10000000 {
            let ci = read_port_reg(port, PORT_CI);
            if ci == 0 {
                // Check for errors
                let is = read_port_reg(port, PORT_IS);
                if is & 0x40000000 != 0 {
                    crate::log_error!("ahci", "Read error on port {}", port);
                    write_port_reg(port, PORT_IS, is);
                    return None;
                }
                write_port_reg(port, PORT_IS, is);

                let size = (count as usize) * 512;
                let src = core::slice::from_raw_parts(
                    crate::mm::vm::phys_to_virt_ptr(DATA_BUFFER) as *const u8,
                    size,
                );
                return Some(src.to_vec());
            }
        }

        crate::log_error!("ahci", "Read timeout on port {}", port);
        None
    }
}

/// Write sectors to the disk.
/// `data.len()` must be a non-zero multiple of 512 bytes.
pub fn write_sectors(lba: u64, data: &[u8]) -> bool {
    let _guard = AHCI_IO_LOCK.lock();
    unsafe {
        let port = match ACTIVE_PORT {
            Some(port) => port,
            None => return false,
        };

        if data.is_empty() || !data.len().is_multiple_of(512) {
            return false;
        }

        let sector_count = data.len() / 512;
        if sector_count == 0 || sector_count > 128 {
            return false;
        }
        let byte_count = sector_count * 512;
        crate::log_info!(
            "ahci",
            "AHCI WRITE: lba={} sectors={} bytes={}",
            lba,
            sector_count,
            byte_count
        );

        core::ptr::copy_nonoverlapping(
            data.as_ptr(),
            crate::mm::vm::phys_to_virt_ptr(DATA_BUFFER) as *mut u8,
            byte_count,
        );

        let cmd_header = crate::mm::vm::phys_to_virt_ptr(CMD_LIST_BASE) as *mut CommandHeader;
        (*cmd_header).flags = 5 | (1 << 6);
        (*cmd_header).prdtl = 1;
        (*cmd_header).prdbc = 0;

        let cmd_table = crate::mm::vm::phys_to_virt_ptr(CMD_TABLE_BASE) as *mut CommandTable;
        ptr::write_bytes(cmd_table, 0, 1);

        (*cmd_table).prdt[0].dba = DATA_BUFFER as u32;
        (*cmd_table).prdt[0].dbau = 0;
        (*cmd_table).prdt[0].dbc = ((byte_count as u32) - 1) | (1 << 31);

        let fis = &mut (*cmd_table).cfis as *mut [u8; 64] as *mut FisRegH2D;
        (*fis).fis_type = FIS_TYPE_REG_H2D;
        (*fis).flags = 0x80;
        (*fis).command = ATA_CMD_WRITE_DMA_EX;
        (*fis).device = 0x40;

        (*fis).lba0 = (lba & 0xFF) as u8;
        (*fis).lba1 = ((lba >> 8) & 0xFF) as u8;
        (*fis).lba2 = ((lba >> 16) & 0xFF) as u8;
        (*fis).lba3 = ((lba >> 24) & 0xFF) as u8;
        (*fis).lba4 = ((lba >> 32) & 0xFF) as u8;
        (*fis).lba5 = ((lba >> 40) & 0xFF) as u8;

        (*fis).count_lo = (sector_count as u16 & 0xFF) as u8;
        (*fis).count_hi = (((sector_count as u16) >> 8) & 0xFF) as u8;

        write_port_reg(port, PORT_CI, 1);

        for _ in 0..10000000 {
            let ci = read_port_reg(port, PORT_CI);
            if ci == 0 {
                let is = read_port_reg(port, PORT_IS);
                if is & 0x40000000 != 0 {
                    crate::log_error!("ahci", "Write error on port {}", port);
                    write_port_reg(port, PORT_IS, is);
                    return false;
                }
                write_port_reg(port, PORT_IS, is);
                crate::log_info!(
                    "ahci",
                    "AHCI WRITE OK: lba={} sectors={}",
                    lba,
                    sector_count
                );
                return true;
            }
        }

        crate::log_error!("ahci", "Write timeout on port {}", port);
        false
    }
}

/// Flush the device write cache to persistent media.
pub fn flush_cache() -> bool {
    let _guard = AHCI_IO_LOCK.lock();
    unsafe {
        let port = match ACTIVE_PORT {
            Some(port) => port,
            None => return false,
        };
        crate::log_info!("ahci", "AHCI FLUSH: begin");

        let cmd_header = crate::mm::vm::phys_to_virt_ptr(CMD_LIST_BASE) as *mut CommandHeader;
        (*cmd_header).flags = 5; // 5 DWORDs in FIS, no data transfer
        (*cmd_header).prdtl = 0;
        (*cmd_header).prdbc = 0;

        let cmd_table = crate::mm::vm::phys_to_virt_ptr(CMD_TABLE_BASE) as *mut CommandTable;
        ptr::write_bytes(cmd_table, 0, 1);

        let fis = &mut (*cmd_table).cfis as *mut [u8; 64] as *mut FisRegH2D;
        (*fis).fis_type = FIS_TYPE_REG_H2D;
        (*fis).flags = 0x80;
        (*fis).command = ATA_CMD_FLUSH_CACHE_EXT;
        (*fis).device = 0;

        write_port_reg(port, PORT_CI, 1);

        for _ in 0..10000000 {
            let ci = read_port_reg(port, PORT_CI);
            if ci == 0 {
                let is = read_port_reg(port, PORT_IS);
                if is & 0x40000000 != 0 {
                    crate::log_error!("ahci", "Flush error on port {}", port);
                    write_port_reg(port, PORT_IS, is);
                    return false;
                }
                write_port_reg(port, PORT_IS, is);
                crate::log_info!("ahci", "AHCI FLUSH: completed");
                return true;
            }
        }

        crate::log_error!("ahci", "Flush timeout on port {}", port);
        false
    }
}

/// Check if AHCI is initialized and available
pub fn is_available() -> bool {
    unsafe { ACTIVE_PORT.is_some() }
}
