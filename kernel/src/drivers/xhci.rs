// xHCI (USB 3.0) Host Controller Driver for Atom OS
// Following xHCI 1.1 Specification

use crate::mm::pmm;
use crate::mm::vm::{self, PageFlags};
use core::ptr;
use spin::Mutex;
use crate::{log_info, log_debug, log_warn, log_error};

// xHCI PCI constants
pub const XHCI_CLASS: u8 = 0x0C;
pub const XHCI_SUBCLASS: u8 = 0x03;
pub const XHCI_PROGIF: u8 = 0x30;

// Register layout structures
#[repr(C)]
pub struct CapabilityRegisters {
    pub caplength: u8,
    pub reserved: u8,
    pub hciversion: u16,
    pub hcsparams1: u32,
    pub hcsparams2: u32,
    pub hcsparams3: u32,
    pub hccparams1: u32,
    pub dboff: u32,
    pub rtsoff: u32,
    pub hccparams2: u32,
}

#[repr(C)]
pub struct OperationalRegisters {
    pub usbcmd: u32,
    pub usbsts: u32,
    pub pagesize: u32,
    pub reserved1: [u32; 2],
    pub dnctrl: u32,
    pub crcr: u64,
    pub reserved2: [u32; 4],
    pub dcbaap: u64,
    pub config: u32,
}

#[repr(C)]
pub struct RuntimeRegisters {
    pub mfindex: u32,
    pub reserved: [u32; 7],
    pub ir: [InterrupterRegisters; 1024],
}

#[repr(C)]
pub struct InterrupterRegisters {
    pub iman: u32,
    pub imod: u32,
    pub erstsz: u32,
    pub reserved: u32,
    pub erstba: u64,
    pub erdp: u64,
}

#[repr(C)]
pub struct PortRegisters {
    pub portsc: u32,
    pub portpmsc: u32,
    pub portli: u32,
    pub porthlpmc: u32,
}

// TRB (Transfer Request Block) structures
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug)]
pub struct Trb {
    pub data: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub fn new() -> Self {
        Self { data: 0, status: 0, control: 0 }
    }

    pub fn get_type(&self) -> u32 {
        (self.control >> 10) & 0x3F
    }

    pub fn set_cycle(&mut self, cycle: bool) {
        if cycle {
            self.control |= 1;
        } else {
            self.control &= !1;
        }
    }

    pub fn get_cycle(&self) -> bool {
        (self.control & 1) != 0
    }
}

// Context structures
#[repr(C, align(64))]
pub struct DeviceContext {
    pub slot: SlotContext,
    pub endpoints: [EndpointContext; 31],
}

#[repr(C, align(64))]
pub struct InputContext {
    pub control: InputControlContext,
    pub device: DeviceContext,
}

#[repr(C)]
pub struct InputControlContext {
    pub drop_flags: u32,
    pub add_flags: u32,
    pub reserved: [u32; 6],
}

#[repr(C)]
pub struct SlotContext {
    pub field1: u32, // Route String, Speed, etc.
    pub field2: u32, // Max Exit Latency, Root Hub Port Number, Number of Ports
    pub field3: u32, // TT Hub Slot ID, TT Port Number, etc.
    pub field4: u32, // Slot State
    pub reserved: [u32; 4],
}

#[repr(C)]
pub struct EndpointContext {
    pub field1: u32, // EP State, Mult, MaxPStreams, LSA, Interval
    pub field2: u32, // Max Packet Size, Max Burst Size, EP Type
    pub tr_base: u64, // Dequeue Cycle State, TR Dequeue Pointer
    pub field4: u32, // Average TRB Length, Max ESIT Payload
    pub reserved: [u32; 3],
}

// xHCI Driver State
pub struct XhciController {
    pub cap_regs: *const CapabilityRegisters,
    pub op_regs: *mut OperationalRegisters,
    pub rt_regs: *mut RuntimeRegisters,
    pub db_regs: *mut u32,
    pub ports: *mut PortRegisters,

    pub max_slots: u8,
    pub max_ports: u8,

    pub dcbaap: *mut u64,
    pub cmd_ring: Ring,
    pub event_ring: EventRing,
}

unsafe impl Send for XhciController {}
unsafe impl Sync for XhciController {}

pub struct Ring {
    pub phys_base: usize,
    pub virt_base: *mut Trb,
    pub index: usize,
    pub cycle: bool,
    pub size: usize,
}

pub struct EventRing {
    pub phys_base: usize,
    pub virt_base: *mut Trb,
    pub erst_phys: usize,
    pub index: usize,
    pub cycle: bool,
    pub size: usize,
}

static CONTROLLER: Mutex<Option<XhciController>> = Mutex::new(None);

pub fn get_controller() -> &'static Mutex<Option<XhciController>> {
    &CONTROLLER
}

pub fn init() -> bool {
    log_info!("xhci", "Initializing production xHCI driver...");

    if let Some((phys_base, bus, dev, func)) = find_xhci_controller() {
        log_info!("xhci", "Found xHCI controller at Phys 0x{:X} ({:02x}:{:02x}.{})", phys_base, bus, dev, func);

        unsafe {
            // Enable PCI Bus Mastering and Memory Space
            let cmd_addr = pci_config_address(bus, dev, func, 0x04);
            let cmd = pci_read_config(cmd_addr);
            pci_write_config(cmd_addr, cmd | 0x06); // BME | MSE

            // Production mapping: use a safe MMIO virtual address
            let xhci_virt = 0xFFFF_FFFF_A000_0000;
            if !map_mmio(phys_base, xhci_virt, 32) { // Map 32 pages to be safe
                log_error!("xhci", "Failed to map xHCI MMIO");
                return false;
            }

            let cap_regs = xhci_virt as *const CapabilityRegisters;
            let caplength = ptr::read_volatile(&(*cap_regs).caplength) as usize;
            let op_regs = (xhci_virt + caplength) as *mut OperationalRegisters;

            let rtsoff = ptr::read_volatile(&(*cap_regs).rtsoff) as usize;
            let rt_regs = (xhci_virt + rtsoff) as *mut RuntimeRegisters;

            let dboff = ptr::read_volatile(&(*cap_regs).dboff) as usize;
            let db_regs = (xhci_virt + dboff) as *mut u32;

            let ports = (xhci_virt + caplength + 0x400) as *mut PortRegisters;

            let hcsparams1 = ptr::read_volatile(&(*cap_regs).hcsparams1);
            let max_slots = (hcsparams1 & 0xFF) as u8;
            let max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;

            log_info!("xhci", "xHCI Version: 0x{:04X}, Max Slots: {}, Max Ports: {}",
                ptr::read_volatile(&(*cap_regs).hciversion), max_slots, max_ports);

            let mut controller = XhciController {
                cap_regs,
                op_regs,
                rt_regs,
                db_regs,
                ports,
                max_slots,
                max_ports,
                dcbaap: ptr::null_mut(),
                cmd_ring: Ring::new(),
                event_ring: EventRing::new(),
            };

            if !controller.reset() {
                return false;
            }

            if !controller.init_structures() {
                return false;
            }

            if !controller.start() {
                return false;
            }

            *CONTROLLER.lock() = Some(controller);

            if let Some(ref mut ctrl) = *CONTROLLER.lock() {
                ctrl.poll_ports();
                ctrl.handle_events();
            }

            return true;
        }
    }

    log_warn!("xhci", "No xHCI controller found");
    false
}

impl XhciController {
    pub unsafe fn init_structures(&mut self) -> bool {
        // 1. Allocate DCBAAP
        let dcbaap_phys = pmm::alloc_page_zeroed().expect("xHCI: Failed to alloc DCBAAP");
        self.dcbaap = (dcbaap_phys + vm::HIGHER_HALF_BASE) as *mut u64;
        ptr::write_volatile(&mut (*self.op_regs).dcbaap, dcbaap_phys as u64);

        // 2. Setup Command Ring
        if !self.cmd_ring.init(1) { // 1 page
            return false;
        }
        ptr::write_volatile(&mut (*self.op_regs).crcr, self.cmd_ring.phys_base as u64 | 1);

        // 3. Setup Event Ring
        if !self.event_ring.init(1) { // 1 page
            return false;
        }

        // 4. Setup Event Ring Segment Table (ERST)
        let erst_phys = pmm::alloc_page_zeroed().expect("xHCI: Failed to alloc ERST");
        let erst_virt = (erst_phys + vm::HIGHER_HALF_BASE) as *mut u64;
        ptr::write_volatile(erst_virt, self.event_ring.phys_base as u64);
        ptr::write_volatile(erst_virt.add(1), self.event_ring.size as u64);
        self.event_ring.erst_phys = erst_phys;

        // 5. Configure Interrupter 0
        let ir0 = &mut (*self.rt_regs).ir[0];
        ptr::write_volatile(&mut ir0.erstsz, 1);
        ptr::write_volatile(&mut ir0.erdp, self.event_ring.phys_base as u64 | 0x08); // Clear EHB
        ptr::write_volatile(&mut ir0.erstba, erst_phys as u64);
        ptr::write_volatile(&mut ir0.iman, 3); // Enable interrupter

        // 6. Set CONFIG
        ptr::write_volatile(&mut (*self.op_regs).config, self.max_slots as u32);

        log_info!("xhci", "Data structures initialized");
        true
    }

    pub unsafe fn start(&mut self) -> bool {
        let usbcmd = ptr::read_volatile(&(*self.op_regs).usbcmd);
        ptr::write_volatile(&mut (*self.op_regs).usbcmd, usbcmd | 1); // Set RS

        if !self.wait_for_status(1, false) { // USBSTS_HCH should be clear
            log_error!("xhci", "Controller failed to start");
            return false;
        }

        log_info!("xhci", "Controller started");
        true
    }

    pub unsafe fn poll_ports(&mut self) {
        for i in 0..self.max_ports {
            let mut portsc = ptr::read_volatile(&(*self.ports.add(i as usize)).portsc);
            if portsc & 0x01 != 0 { // Current Connect Status
                if (portsc & (1 << 1)) == 0 { // Not enabled, needs reset
                    log_info!("xhci", "Port {} needs reset", i + 1);
                    self.reset_port(i + 1);
                    portsc = ptr::read_volatile(&(*self.ports.add(i as usize)).portsc);
                }
                let speed = (portsc >> 10) & 0x0F;
                crate::drivers::usb_core::handle_port_connection(i + 1, speed as u8);
            }
        }
    }

    pub unsafe fn reset_port(&mut self, port_id: u8) {
        let port_idx = (port_id - 1) as usize;
        let mut portsc = ptr::read_volatile(&(*self.ports.add(port_idx)).portsc);

        // Clear change bits and set PR (Port Reset)
        portsc &= 0x0E00_C3E0; // Preserve writable bits
        ptr::write_volatile(&mut (*self.ports.add(port_idx)).portsc, portsc | (1 << 4)); // PR bit

        // Wait for reset to complete (PRC bit)
        for _ in 0..100_000 {
            let val = ptr::read_volatile(&(*self.ports.add(port_idx)).portsc);
            if val & (1 << 21) != 0 { // Port Reset Change
                // Clear PRC
                ptr::write_volatile(&mut (*self.ports.add(port_idx)).portsc, (val & 0x0E00_C3E0) | (1 << 21));
                break;
            }
            core::hint::spin_loop();
        }
    }

    pub unsafe fn handle_events(&mut self) {
        let ring = &mut self.event_ring;
        loop {
            let trb = ptr::read_volatile(ring.virt_base.add(ring.index));
            if trb.get_cycle() != ring.cycle {
                break;
            }

            let trb_type = trb.get_type();
            match trb_type {
                33 => { // Command Completion Event
                    let completion_code = (trb.status >> 24) as u8;
                    let slot_id = (trb.control >> 24) as u8;
                    log_debug!("xhci", "Command Completion: Code {}, Slot {}", completion_code, slot_id);
                    if completion_code == 1 { // Success
                        crate::drivers::usb_core::on_slot_enabled(slot_id);
                    }
                }
                34 => { // Port Status Change Event
                    let port_id = (trb.data >> 24) as u8;
                    log_debug!("xhci", "Port Status Change: Port {}", port_id);
                }
                _ => {
                    log_debug!("xhci", "Unknown Event TRB type: {}", trb_type);
                }
            }

            ring.index += 1;
            if ring.index >= ring.size {
                ring.index = 0;
                ring.cycle = !ring.cycle;
            }

            // Update Event Ring Dequeue Pointer
            let ir0 = &mut (*self.rt_regs).ir[0];
            let erdp = (ring.phys_base + ring.index * 16) as u64 | 0x08;
            ptr::write_volatile(&mut ir0.erdp, erdp);
        }
    }

    pub unsafe fn send_command(&mut self, mut trb: Trb) {
        let ring = &mut self.cmd_ring;

        // Link TRB check before writing
        if ring.index == ring.size - 1 {
            let mut link = Trb::new();
            link.data = ring.phys_base as u64;
            link.control = (6 << 10) | 0x02; // Link TRB, Toggle Cycle
            link.set_cycle(ring.cycle);
            ptr::write_volatile(ring.virt_base.add(ring.index), link);

            ring.index = 0;
            ring.cycle = !ring.cycle;
        }

        trb.set_cycle(ring.cycle);
        ptr::write_volatile(ring.virt_base.add(ring.index), trb);
        ring.index += 1;

        // Ring doorbell for Host Controller (Slot 0)
        ptr::write_volatile(self.db_regs, 0);
    }

    pub unsafe fn reset(&mut self) -> bool {
        // 1. Stop the controller
        let usbcmd = ptr::read_volatile(&(*self.op_regs).usbcmd);
        ptr::write_volatile(&mut (*self.op_regs).usbcmd, usbcmd & !1); // Clear RS

        // 2. Wait for HCHalted
        if !self.wait_for_status(1, true) { // USBSTS_HCH
            log_error!("xhci", "Controller failed to halt");
            return false;
        }

        // 3. Issue HCRST
        ptr::write_volatile(&mut (*self.op_regs).usbcmd, 2); // Set HCRST

        // 4. Wait for HCRST to clear
        if !self.wait_for_cmd_clear(2) {
            log_error!("xhci", "Controller reset timeout");
            return false;
        }

        log_info!("xhci", "Controller reset successful");
        true
    }

    unsafe fn wait_for_status(&self, bit: u32, set: bool) -> bool {
        for _ in 0..1_000_000 {
            let val = ptr::read_volatile(&(*self.op_regs).usbsts);
            if (val & bit != 0) == set {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    unsafe fn wait_for_cmd_clear(&self, bit: u32) -> bool {
        for _ in 0..1_000_000 {
            let val = ptr::read_volatile(&(*self.op_regs).usbcmd);
            if (val & bit) == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

impl Ring {
    pub fn new() -> Self {
        Self {
            phys_base: 0,
            virt_base: ptr::null_mut(),
            index: 0,
            cycle: true,
            size: 0,
        }
    }

    pub unsafe fn init(&mut self, pages: usize) -> bool {
        self.size = (pages * 4096) / 16; // 16 bytes per TRB
        self.phys_base = pmm::alloc_pages_zeroed(pages).expect("xHCI: Failed to alloc Ring");
        self.virt_base = (self.phys_base + vm::HIGHER_HALF_BASE) as *mut Trb;
        self.index = 0;
        self.cycle = true;
        true
    }
}

impl EventRing {
    pub fn new() -> Self {
        Self {
            phys_base: 0,
            virt_base: ptr::null_mut(),
            erst_phys: 0,
            index: 0,
            cycle: true,
            size: 0,
        }
    }

    pub unsafe fn init(&mut self, pages: usize) -> bool {
        self.size = (pages * 4096) / 16; // 16 bytes per TRB
        self.phys_base = pmm::alloc_pages_zeroed(pages).expect("xHCI: Failed to alloc EventRing");
        self.virt_base = (self.phys_base + vm::HIGHER_HALF_BASE) as *mut Trb;
        self.index = 0;
        self.cycle = true;
        true
    }
}

fn find_xhci_controller() -> Option<(usize, u8, u8, u8)> {
    for bus in 0..256u16 {
        for device in 0..32u8 {
            for function in 0..8u8 {
                let address = pci_config_address(bus as u8, device, function, 0);
                let vendor_device = pci_read_config(address);

                if vendor_device == 0xFFFFFFFF || vendor_device == 0 {
                    continue;
                }

                let class_addr = pci_config_address(bus as u8, device, function, 0x08);
                let class_info = pci_read_config(class_addr);
                let class = ((class_info >> 24) & 0xFF) as u8;
                let subclass = ((class_info >> 16) & 0xFF) as u8;
                let progif = ((class_info >> 8) & 0xFF) as u8;

                if class == XHCI_CLASS && subclass == XHCI_SUBCLASS && progif == XHCI_PROGIF {
                    let bar0_addr = pci_config_address(bus as u8, device, function, 0x10);
                    let bar0 = pci_read_config(bar0_addr);

                    if bar0 == 0 || (bar0 & 0x1) != 0 {
                        continue;
                    }

                    let mut base = (bar0 & !0xF) as u64;

                    if (bar0 & 0x4) != 0 {
                        let bar1_addr = pci_config_address(bus as u8, device, function, 0x14);
                        let bar1 = pci_read_config(bar1_addr);
                        base |= (bar1 as u64) << 32;
                    }

                    if base == 0 {
                        continue;
                    }

                    return Some((base as usize, bus as u8, device, function));
                }
            }
        }
    }
    None
}

fn map_mmio(phys_base: usize, virt_base: usize, page_count: usize) -> bool {
    let phys_page = phys_base & !0xFFF;
    let virt_page = virt_base & !0xFFF;

    for i in 0..page_count {
        let paddr = phys_page + i * 0x1000;
        let vaddr = virt_page + i * 0x1000;

        if let Err(e) = vm::map_page(
            vaddr,
            paddr,
            PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::CACHE_DISABLE,
        ) {
            log_error!("xhci", "Failed to map MMIO page Phys:0x{:X} to Virt:0x{:X}: {:?}", paddr, vaddr, e);
            return false;
        }
    }
    true
}

fn pci_config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    ((bus as u32) << 16) |
    ((device as u32) << 11) |
    ((function as u32) << 8) |
    ((offset as u32) & 0xFC) |
    0x80000000
}

fn pci_read_config(address: u32) -> u32 {
    #[cfg(target_arch = "x86_64")]
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

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = address;
        0xFFFFFFFF
    }
}

fn pci_write_config(address: u32, value: u32) {
    #[cfg(target_arch = "x86_64")]
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

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (address, value);
    }
}
