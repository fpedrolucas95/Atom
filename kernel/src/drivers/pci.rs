//! PCI Bus Management
//!
//! This module provides enumeration and access to the PCI bus.
//! It maintains a registry of discovered devices to facilitate
//! capability-based delegation to userspace drivers.

use crate::log_info;
use alloc::vec::Vec;
use spin::Mutex;

/// PCI configuration space registers
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// Information about a discovered PCI device
#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    #[allow(dead_code)]
    pub prog_if: u8,
    pub irq_line: u8,
}

impl PciDevice {
    pub fn bdf(&self) -> u16 {
        ((self.bus as u16) << 8) | ((self.device as u16) << 3) | (self.function as u16)
    }
}

static DISCOVERED_DEVICES: Mutex<Vec<PciDevice>> = Mutex::new(Vec::new());

/// Initialize PCI subsystem by scanning the bus
pub fn init() {
    let mut devices = DISCOVERED_DEVICES.lock();

    // Simple scan: Bus 0 only for now (common in QEMU/simple hardware)
    // Production would use recursive scan or ACPI MCFG
    for dev in 0..32u8 {
        for func in 0..8u8 {
            if let Some(device) = probe_device(0, dev, func) {
                devices.push(device);

                log_info!(
                    "pci",
                    "Device found: {:02x}:{:02x}.{} ID={:04x}:{:04x} Class={:02x}.{:02x} IRQ={}",
                    device.bus,
                    device.device,
                    device.function,
                    device.vendor_id,
                    device.device_id,
                    device.class,
                    device.subclass,
                    device.irq_line
                );

                // If not a multi-function device, don't scan other functions
                if func == 0 {
                    let header_type = read_config_byte(0, dev, 0, 0x0E);
                    if header_type & 0x80 == 0 {
                        break;
                    }
                }
            }
        }
    }

    log_info!(
        "pci",
        "Enumeration complete: {} devices found",
        devices.len()
    );
}

fn probe_device(bus: u8, dev: u8, func: u8) -> Option<PciDevice> {
    let vendor_id = read_config_word(bus, dev, func, 0x00);
    if vendor_id == 0xFFFF {
        return None;
    }

    let device_id = read_config_word(bus, dev, func, 0x02);
    let class_info = read_config_dword(bus, dev, func, 0x08);
    let class = ((class_info >> 24) & 0xFF) as u8;
    let subclass = ((class_info >> 16) & 0xFF) as u8;
    let prog_if = ((class_info >> 8) & 0xFF) as u8;

    let intr_info = read_config_word(bus, dev, func, 0x3C);
    let irq_line = (intr_info & 0xFF) as u8;

    Some(PciDevice {
        bus,
        device: dev,
        function: func,
        vendor_id,
        device_id,
        class,
        subclass,
        prog_if,
        irq_line,
    })
}

/// Find a device by vendor and device ID
#[allow(dead_code)]
pub fn find_device(vendor: u16, device: u16) -> Option<PciDevice> {
    DISCOVERED_DEVICES
        .lock()
        .iter()
        .find(|d| d.vendor_id == vendor && d.device_id == device)
        .copied()
}

/// Find a device by BDF (Bus/Device/Function)
pub fn find_by_bdf(bdf: u16) -> Option<PciDevice> {
    DISCOVERED_DEVICES
        .lock()
        .iter()
        .find(|d| d.bdf() == bdf)
        .copied()
}

/// Return all devices matching a PCI class code.
/// The kernel uses this to grant capabilities without knowing specific drivers.
pub fn get_devices_by_class(class: u8) -> alloc::vec::Vec<PciDevice> {
    DISCOVERED_DEVICES
        .lock()
        .iter()
        .filter(|d| d.class == class)
        .copied()
        .collect()
}

/// Read a 32-bit dword from PCI config space
pub fn read_config_dword(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let address = ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
        | 0x80000000;

    unsafe {
        crate::arch::outl(PCI_CONFIG_ADDRESS, address);
        crate::arch::inl(PCI_CONFIG_DATA)
    }
}

/// Write a 32-bit dword to PCI config space
pub fn write_config_dword(bus: u8, dev: u8, func: u8, offset: u8, value: u32) {
    let address = ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
        | 0x80000000;

    unsafe {
        crate::arch::outl(PCI_CONFIG_ADDRESS, address);
        crate::arch::outl(PCI_CONFIG_DATA, value);
    }
}

pub fn read_config_word(bus: u8, dev: u8, func: u8, offset: u8) -> u16 {
    let val = read_config_dword(bus, dev, func, offset);
    ((val >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

pub fn read_config_byte(bus: u8, dev: u8, func: u8, offset: u8) -> u8 {
    let val = read_config_dword(bus, dev, func, offset);
    ((val >> ((offset & 3) * 8)) & 0xFF) as u8
}

/// Information about a PCI BAR
#[derive(Debug, Clone, Copy)]
pub struct BarInfo {
    pub base: u64,
    pub size: u64,
    pub is_mmio: bool,
    pub is_64bit: bool,
    pub prefetchable: bool,
}

pub fn get_bar_info(bus: u8, dev: u8, func: u8, index: u8) -> Option<BarInfo> {
    if index >= 6 {
        return None;
    }

    let offset = 0x10 + (index * 4);
    let bar_orig = read_config_dword(bus, dev, func, offset);

    // Detect type
    let is_mmio = (bar_orig & 1) == 0;
    let is_64bit = is_mmio && ((bar_orig >> 1) & 3) == 2;
    let prefetchable = is_mmio && ((bar_orig >> 3) & 1) != 0;

    // Size calculation (destructive, must restore)
    write_config_dword(bus, dev, func, offset, 0xFFFFFFFF);
    let size_mask = read_config_dword(bus, dev, func, offset);
    write_config_dword(bus, dev, func, offset, bar_orig);

    let mut base = (bar_orig & if is_mmio { 0xFFFF_FFF0 } else { 0xFFFF_FFFC }) as u64;
    let mut mask = (size_mask & if is_mmio { 0xFFFF_FFF0 } else { 0xFFFF_FFFC }) as u64;

    if is_64bit && index < 5 {
        let bar_hi = read_config_dword(bus, dev, func, offset + 4);
        write_config_dword(bus, dev, func, offset + 4, 0xFFFFFFFF);
        let size_mask_hi = read_config_dword(bus, dev, func, offset + 4);
        write_config_dword(bus, dev, func, offset + 4, bar_hi);

        base |= (bar_hi as u64) << 32;
        // Only extend mask with high bits if they contribute to size
        // (size_mask_hi == 0xFFFFFFFF means no size bits in high dword — 32-bit range)
        if size_mask_hi != 0xFFFF_FFFF {
            mask |= (size_mask_hi as u64) << 32;
        }
    }

    if mask == 0 {
        return None;
    }

    // Size = lowest set bit of mask (two's complement trick)
    let size = mask & mask.wrapping_neg();

    Some(BarInfo {
        base,
        size,
        is_mmio,
        is_64bit,
        prefetchable,
    })
}
