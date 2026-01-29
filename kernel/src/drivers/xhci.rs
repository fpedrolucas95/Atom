// xHCI (USB 3.0) driver for Atom OS
// Provides support for USB HID devices (keyboard and mouse)

use crate::mm::pmm;
use crate::mm::vm::{self, PageFlags};
use core::ptr;
use crate::{log_info, log_debug, log_warn, log_error};

// xHCI PCI constants
const XHCI_CLASS: u8 = 0x0C;
const XHCI_SUBCLASS: u8 = 0x03;
const XHCI_PROGIF: u8 = 0x30;

// xHCI Register Offsets (Capability Registers)
const XHCI_CAP_CAPLENGTH: usize = 0x00;
const XHCI_CAP_HCIVERSION: usize = 0x02;
const XHCI_CAP_HCSPARAMS1: usize = 0x04;
const XHCI_CAP_HCSPARAMS2: usize = 0x08;
const XHCI_CAP_HCCPARAMS1: usize = 0x0C;
const XHCI_CAP_DBOFF: usize = 0x14;
const XHCI_CAP_RTSOFF: usize = 0x18;

// Operational Registers (Offset from CAPLENGTH)
const XHCI_OP_USBCMD: usize = 0x00;
const XHCI_OP_USBSTS: usize = 0x04;
const XHCI_OP_PAGESIZE: usize = 0x08;
const XHCI_OP_DNCTRL: usize = 0x14;
const XHCI_OP_CRCR: usize = 0x18;
const XHCI_OP_DCBAAP: usize = 0x30;
const XHCI_OP_CONFIG: usize = 0x38;

// Port Registers
const XHCI_OP_PORTS_BASE: usize = 0x400;

// Operational Register bits
const USBCMD_RS: u32 = 1 << 0;  // Run/Stop
const USBCMD_HCRST: u32 = 1 << 1; // Host Controller Reset
const USBSTS_HCH: u32 = 1 << 0; // HCHalted

static mut XHCI_BASE: usize = 0;
static mut CAP_LENGTH: usize = 0;
static mut MAX_SLOTS: u8 = 0;
static mut MAX_PORTS: u8 = 0;

static mut DCBAAP: usize = 0;
static mut COMMAND_RING: usize = 0;
static mut EVENT_RING: usize = 0;
static mut ERST: usize = 0; // Event Ring Segment Table
static mut DEVICE_CONTEXTS: [usize; 64] = [0; 64];

static mut COMMAND_RING_INDEX: usize = 0;
static mut COMMAND_RING_CYCLE: u8 = 1;

static mut EVENT_RING_INDEX: usize = 0;
static mut EVENT_RING_CYCLE: u8 = 1;

#[repr(C, align(16))]
struct Trb {
    data: u64,
    status: u32,
    control: u32,
}

#[repr(C, align(64))]
struct InputContext {
    control: InputControlContext,
    device: DeviceContext,
}

#[repr(C)]
struct InputControlContext {
    drop_flags: u32,
    add_flags: u32,
    reserved: [u32; 6],
}

#[repr(C)]
struct DeviceContext {
    slot: SlotContext,
    endpoints: [EndpointContext; 31],
}

#[repr(C)]
struct SlotContext {
    info: u32,
    info2: u32,
    tt: u32,
    state: u32,
    reserved: [u32; 4],
}

#[repr(C)]
struct EndpointContext {
    state: u32,
    mult_err_type: u32,
    tr_base: u64,
    avg_trb_len: u16,
    max_esit_payload: u16,
    reserved: [u32; 3],
}

pub fn init() -> bool {
    log_info!("xhci", "Initializing xHCI driver...");

    if let Some(base) = find_xhci_controller() {
        unsafe {
            XHCI_BASE = base;
            log_info!("xhci", "Found xHCI controller at MMIO 0x{:X}", base);

            // Step 2: Map MMIO
            if !map_mmio(base) {
                return false;
            }

            CAP_LENGTH = ptr::read_volatile(base as *const u8) as usize;
            log_debug!("xhci", "Capability register length: {}", CAP_LENGTH);

            let version = ptr::read_volatile((base + XHCI_CAP_HCIVERSION) as *const u16);
            log_info!("xhci", "xHCI version: 0x{:04X}", version);

            if !reset_controller() {
                return false;
            }

            if !init_structures() {
                return false;
            }

            if !start_controller() {
                return false;
            }

            poll_ports();

            return true;
        }
    }

    log_warn!("xhci", "No xHCI controller found");
    false
}

unsafe fn reset_controller() -> bool {
    let op_base = XHCI_BASE + CAP_LENGTH;

    // 1. Stop the controller
    let mut usbcmd = ptr::read_volatile((op_base + XHCI_OP_USBCMD) as *const u32);
    ptr::write_volatile((op_base + XHCI_OP_USBCMD) as *mut u32, usbcmd & !USBCMD_RS);

    // 2. Wait for HCHalted
    if !wait_for_bit(op_base + XHCI_OP_USBSTS, USBSTS_HCH, true) {
        log_error!("xhci", "Controller failed to halt");
        return false;
    }

    // 3. Issue HCRST
    ptr::write_volatile((op_base + XHCI_OP_USBCMD) as *mut u32, USBCMD_HCRST);

    // 4. Wait for HCRST to clear
    if !wait_for_bit(op_base + XHCI_OP_USBCMD, USBCMD_HCRST, false) {
        log_error!("xhci", "Controller reset timeout");
        return false;
    }

    log_info!("xhci", "Controller reset successful");
    true
}

unsafe fn wait_for_bit(addr: usize, bit: u32, set: bool) -> bool {
    for _ in 0..1_000_000 {
        let val = ptr::read_volatile(addr as *const u32);
        if (val & bit != 0) == set {
            return true;
        }
    }
    false
}

fn translate_mouse_report(usb_report: &[u8; 3]) -> [u8; 3] {
    let mut ps2_packet = [0u8; 3];
    let buttons = usb_report[0];
    let dx = usb_report[1] as i8;
    let dy = usb_report[2] as i8;

    // PS/2 Byte 0: [Yovf | Xovf | Ysign | Xsign | 1 | Mid | Right | Left]
    let mut flags = 0x08; // Always 1 bit
    if buttons & 0x01 != 0 { flags |= 0x01; } // Left
    if buttons & 0x02 != 0 { flags |= 0x02; } // Right
    if buttons & 0x04 != 0 { flags |= 0x04; } // Middle

    if dx < 0 { flags |= 0x10; }
    if dy > 0 { flags |= 0x20; } // PS/2 Y is inverted relative to USB

    ps2_packet[0] = flags;
    ps2_packet[1] = dx as u8;
    ps2_packet[2] = (-dy) as u8; // Invert Y

    ps2_packet
}

fn usb_to_ps2_scancode(usb_code: u8) -> u8 {
    match usb_code {
        0x04 => 0x1E, // A
        0x05 => 0x30, // B
        0x06 => 0x2E, // C
        0x07 => 0x20, // D
        0x08 => 0x12, // E
        0x09 => 0x21, // F
        0x0A => 0x22, // G
        0x0B => 0x23, // H
        0x0C => 0x17, // I
        0x0D => 0x24, // J
        0x0E => 0x25, // K
        0x0F => 0x26, // L
        0x10 => 0x32, // M
        0x11 => 0x31, // N
        0x12 => 0x18, // O
        0x13 => 0x19, // P
        0x14 => 0x10, // Q
        0x15 => 0x13, // R
        0x16 => 0x1F, // S
        0x17 => 0x14, // T
        0x18 => 0x16, // U
        0x19 => 0x2F, // V
        0x1A => 0x11, // W
        0x1B => 0x2D, // X
        0x1C => 0x15, // Y
        0x1D => 0x2C, // Z
        0x1E => 0x02, // 1
        0x1F => 0x03, // 2
        0x20 => 0x04, // 3
        0x21 => 0x05, // 4
        0x22 => 0x06, // 5
        0x23 => 0x07, // 6
        0x24 => 0x08, // 7
        0x25 => 0x09, // 8
        0x26 => 0x0A, // 9
        0x27 => 0x0B, // 0
        0x28 => 0x1C, // Enter
        0x29 => 0x01, // Esc
        0x2A => 0x0E, // Backspace
        0x2B => 0x0F, // Tab
        0x2C => 0x39, // Space
        _ => 0,
    }
}

unsafe fn poll_hid_reports(slot_id: u8) {
    log_info!("xhci", "Starting HID report polling for slot {}", slot_id);
    // This would involve setting up a Transfer Ring for the Interrupt IN endpoint
    // and periodically checking for Data Stage TRBs

    // MOCK: Simulate receiving a keyboard report 'A'
    let mock_report = [0x04, 0, 0, 0, 0, 0];
    handle_keyboard_report(&mock_report);
}

pub unsafe fn handle_keyboard_report(usb_scancodes: &[u8; 6]) {
    for &code in usb_scancodes.iter() {
        if code == 0 { continue; }
        let ps2 = usb_to_ps2_scancode(code);
        if ps2 != 0 {
            crate::input::push_keyboard_scancode(ps2);
        }
    }
}

pub unsafe fn handle_mouse_report(usb_report: &[u8; 3]) {
    let ps2_packet = translate_mouse_report(usb_report);
    for &byte in ps2_packet.iter() {
        crate::input::push_mouse_byte(byte);
    }
}

unsafe fn get_descriptor(slot_id: u8, desc_type: u8, desc_index: u8, length: u16) {
    log_info!("xhci", "Fetching descriptor type {} for slot {}", desc_type, slot_id);
    // This would involve creating a Setup TRB, Data TRB, and Status TRB on the device's EP0 ring
}

unsafe fn set_boot_protocol(slot_id: u8, interface: u8) {
    log_info!("xhci", "Setting Boot Protocol for slot {}, interface {}", slot_id, interface);
    // Setup Stage TRB: RequestType=0x21, Request=0x0B, Value=0, Index=interface, Length=0
}

unsafe fn configure_endpoint(slot_id: u8, endpoint_index: u8) {
    log_info!("xhci", "Configuring endpoint {} for slot {}", endpoint_index, slot_id);
    let trb = Trb {
        data: 0, // Should be Input Context
        status: 0,
        control: (12 << 10) | ((slot_id as u32) << 24), // Configure Endpoint Command
    };
    send_command(trb);
}

unsafe fn address_device(slot_id: u8) {
    let input_ctx_phys = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => return,
    };
    let input_ctx = input_ctx_phys as *mut InputContext;

    // Initialize Input Control Context
    (*input_ctx).control.add_flags = 0x03; // Add Slot Context and Endpoint 0 Context

    // Initialize Slot Context
    let op_base = XHCI_BASE + CAP_LENGTH;
    let portsc_addr = op_base + XHCI_OP_PORTS_BASE; // Simplify: use port 1 for now
    let portsc = ptr::read_volatile(portsc_addr as *const u32);
    let speed = (portsc >> 10) & 0x0F;

    (*input_ctx).device.slot.info = (1 << 27) | (speed << 20); // 1 context, speed
    (*input_ctx).device.slot.info2 = 1; // Root hub port 1

    // Initialize Endpoint 0 Context
    let ep0_ring = pmm::alloc_pages_zeroed(1).expect("Failed EP0 ring");
    (*input_ctx).device.endpoints[0].mult_err_type = (3 << 1) | (4 << 16); // Max burst 0, Max packet size 8 (for low speed)
    (*input_ctx).device.endpoints[0].tr_base = ep0_ring as u64 | 1; // DCS = 1

    let trb = Trb {
        data: input_ctx_phys as u64,
        status: 0,
        control: (11 << 10) | ((slot_id as u32) << 24), // Address Device Command
    };
    send_command(trb);
}

unsafe fn setup_device_context(slot_id: u8) -> bool {
    let device_ctx = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => {
            log_error!("xhci", "Failed to allocate device context");
            return false;
        }
    };
    DEVICE_CONTEXTS[slot_id as usize] = device_ctx;

    let dcbaap = DCBAAP as *mut u64;
    ptr::write_volatile(dcbaap.add(slot_id as usize), device_ctx as u64);
    true
}

unsafe fn init_structures() -> bool {
    let op_base = XHCI_BASE + CAP_LENGTH;

    // 1. Read MaxSlots and MaxPorts
    let hcsparams1 = ptr::read_volatile((XHCI_BASE + XHCI_CAP_HCSPARAMS1) as *const u32);
    MAX_SLOTS = (hcsparams1 & 0xFF) as u8;
    MAX_PORTS = ((hcsparams1 >> 24) & 0xFF) as u8;
    log_debug!("xhci", "Max slots: {}, Max ports: {}", MAX_SLOTS, MAX_PORTS);

    // 2. Allocate DCBAAP
    // Size = (MaxSlots + 1) * 8 bytes
    DCBAAP = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => return false,
    };
    ptr::write_volatile((op_base + XHCI_OP_DCBAAP) as *mut u64, DCBAAP as u64);

    // 3. Allocate Command Ring (one page)
    COMMAND_RING = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => return false,
    };
    // CRCR: bit 0 = RCS (Ring Cycle State), bits 6-63 = Pointer
    ptr::write_volatile((op_base + XHCI_OP_CRCR) as *mut u64, (COMMAND_RING as u64) | 1);

    // 4. Allocate Event Ring and ERST
    EVENT_RING = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => return false,
    };
    ERST = match pmm::alloc_pages_zeroed(1) {
        Some(addr) => addr,
        None => return false,
    };

    // ERST entry: 64-bit address, 32-bit size, 32-bit reserved
    let erst_entry = ERST as *mut u64;
    ptr::write_volatile(erst_entry, EVENT_RING as u64);
    ptr::write_volatile(erst_entry.add(1), 256); // 256 TRBs (4096 / 16)

    // Set up Interrupter 0
    let rts_off = ptr::read_volatile((XHCI_BASE + XHCI_CAP_RTSOFF) as *const u32) as usize;
    let int_base = XHCI_BASE + rts_off + 0x20; // Runtime Regs + Interrupter 0

    ptr::write_volatile((int_base + 0x08) as *mut u32, 1); // ERSTSZ = 1
    ptr::write_volatile((int_base + 0x10) as *mut u64, ERST as u64); // ERSTBA
    ptr::write_volatile((int_base + 0x18) as *mut u64, EVENT_RING as u64); // ERDP (Deq pointer)

    // 5. Enable slots
    let mut config = ptr::read_volatile((op_base + XHCI_OP_CONFIG) as *const u32);
    ptr::write_volatile((op_base + XHCI_OP_CONFIG) as *mut u32, (config & !0xFF) | MAX_SLOTS as u32);

    log_info!("xhci", "Data structures initialized");
    true
}

unsafe fn handle_events() {
    let ring = EVENT_RING as *mut Trb;
    loop {
        let trb = ptr::read_volatile(ring.add(EVENT_RING_INDEX));
        let cycle = (trb.control & 0x01) as u8;

        if cycle != EVENT_RING_CYCLE {
            break;
        }

        let trb_type = (trb.control >> 10) & 0x3F;
        match trb_type {
            33 => { // Command Completion Event
                let slot_id = (trb.control >> 24) as u8;
                let completion_code = (trb.status >> 24) as u8;
                log_debug!("xhci", "Command completed: code={}, slot={}", completion_code, slot_id);

                let cmd_trb_ptr = trb.data as *const Trb;
                let cmd_trb = ptr::read_volatile(cmd_trb_ptr);
                let cmd_type = (cmd_trb.control >> 10) & 0x3F;

                if cmd_type == 9 && completion_code == 1 { // Enable Slot Success
                    log_info!("xhci", "Slot {} enabled", slot_id);
                    if setup_device_context(slot_id) {
                        address_device(slot_id);
                    }
                } else if cmd_type == 11 && completion_code == 1 { // Address Device Success
                    log_info!("xhci", "Device on slot {} addressed", slot_id);
                    get_descriptor(slot_id, 1, 0, 18); // DEVICE Descriptor
                }
            }
            34 => { // Port Status Change Event
                let port_id = (trb.data >> 24) as u8;
                log_info!("xhci", "Port status change: port={}", port_id);
            }
            _ => {
                log_debug!("xhci", "Unknown event TRB type: {}", trb_type);
            }
        }

        EVENT_RING_INDEX += 1;
        if EVENT_RING_INDEX >= 256 {
            EVENT_RING_INDEX = 0;
            EVENT_RING_CYCLE ^= 1;
        }

        // Update ERDP
        let rts_off = ptr::read_volatile((XHCI_BASE + XHCI_CAP_RTSOFF) as *const u32) as usize;
        let int_base = XHCI_BASE + rts_off + 0x20;
        ptr::write_volatile((int_base + 0x18) as *mut u64, ring.add(EVENT_RING_INDEX) as u64 | 0x08); // Clear EHB
    }
}

unsafe fn send_command(trb: Trb) {
    let ring = COMMAND_RING as *mut Trb;
    let mut current = trb;

    // Set cycle bit
    if COMMAND_RING_CYCLE != 0 {
        current.control |= 0x01;
    } else {
        current.control &= !0x01;
    }

    ptr::write_volatile(ring.add(COMMAND_RING_INDEX), current);

    COMMAND_RING_INDEX += 1;
    if COMMAND_RING_INDEX >= 255 { // Last TRB is Link TRB (simplified)
        // Set link TRB
        let link_trb = Trb {
            data: COMMAND_RING as u64,
            status: 0,
            control: (6 << 10) | 0x02 | (COMMAND_RING_CYCLE as u32), // Link TRB type, toggle cycle
        };
        ptr::write_volatile(ring.add(COMMAND_RING_INDEX), link_trb);
        COMMAND_RING_INDEX = 0;
        COMMAND_RING_CYCLE ^= 1;
    }

    // Ring the doorbell for host controller
    let db_off = ptr::read_volatile((XHCI_BASE + XHCI_CAP_DBOFF) as *const u32) as usize;
    ptr::write_volatile((XHCI_BASE + db_off) as *mut u32, 0); // Doorbell 0 for Command Ring
}

unsafe fn start_controller() -> bool {
    let op_base = XHCI_BASE + CAP_LENGTH;
    let usbcmd = ptr::read_volatile((op_base + XHCI_OP_USBCMD) as *const u32);
    ptr::write_volatile((op_base + XHCI_OP_USBCMD) as *mut u32, usbcmd | USBCMD_RS);

    if !wait_for_bit(op_base + XHCI_OP_USBSTS, USBSTS_HCH, false) {
        log_error!("xhci", "Controller failed to start");
        return false;
    }

    log_info!("xhci", "Controller started");
    true
}

unsafe fn enable_slot() {
    let trb = Trb {
        data: 0,
        status: 0,
        control: (9 << 10), // Enable Slot Command
    };
    send_command(trb);
}

unsafe fn poll_ports() {
    let op_base = XHCI_BASE + CAP_LENGTH;
    for i in 0..MAX_PORTS {
        let portsc_addr = op_base + XHCI_OP_PORTS_BASE + (i as usize * 0x10);
        let portsc = ptr::read_volatile(portsc_addr as *const u32);

        if portsc & 0x01 != 0 { // Current Connect Status
            log_info!("xhci", "Device detected on port {}", i + 1);
            enable_slot();
            poll_hid_reports(1); // MOCK: assume slot 1
        }
    }

    // Process events to see the slot assignment
    handle_events();
}

fn find_xhci_controller() -> Option<usize> {
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
                    // Found xHCI controller - get BAR0
                    let bar0_addr = pci_config_address(bus as u8, device, function, 0x10);
                    let bar0 = pci_read_config(bar0_addr) as usize;

                    if bar0 == 0 {
                        continue;
                    }

                    // Enable bus mastering and memory space
                    let cmd_addr = pci_config_address(bus as u8, device, function, 0x04);
                    let cmd = pci_read_config(cmd_addr);
                    pci_write_config(cmd_addr, cmd | 0x06);

                    return Some(bar0 & !0xF);
                }
            }
        }
    }
    None
}

fn map_mmio(base: usize) -> bool {
    let page_base = base & !0xFFF;
    // xHCI MMIO space can be quite large, but 8KB (2 pages) covers standard registers
    for i in 0..4 {
        let addr = page_base + i * 0x1000;
        let _ = vm::unmap_page(addr);
        if let Err(e) = vm::map_page(
            addr,
            addr,
            PageFlags::PRESENT | PageFlags::WRITABLE | PageFlags::CACHE_DISABLE,
        ) {
            log_error!("xhci", "Failed to map MMIO page 0x{:X}: {:?}", addr, e);
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
