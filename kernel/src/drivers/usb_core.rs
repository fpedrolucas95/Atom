// USB Core: Device model and enumeration for Atom OS

#![allow(dead_code)]

use crate::drivers::xhci::{self, Trb};
use crate::log_info;

// USB Descriptor Types
pub const USB_DESC_DEVICE: u8 = 1;
pub const USB_DESC_CONFIG: u8 = 2;
pub const USB_DESC_STRING: u8 = 3;
pub const USB_DESC_INTERFACE: u8 = 4;
pub const USB_DESC_ENDPOINT: u8 = 5;

#[repr(C, packed)]
pub struct DeviceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub usb_version: u16,
    pub device_class: u8,
    pub device_subclass: u8,
    pub device_protocol: u8,
    pub max_packet_size0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
    pub manufacturer_idx: u8,
    pub product_idx: u8,
    pub serial_number_idx: u8,
    pub num_configurations: u8,
}

#[derive(Clone, Copy)]
pub struct UsbDevice {
    pub slot_id: u8,
    pub port_id: u8,
    pub speed: u8,
    pub state: DeviceState,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DeviceState {
    Powered,
    Default,
    Address,
    Configured,
}

static mut DEVICES: [Option<UsbDevice>; 64] = [None; 64];

pub fn handle_port_connection(port_id: u8, speed: u8) {
    log_info!(
        "usb_core",
        "New device connected on port {}, speed {}",
        port_id,
        speed
    );

    let mut trb = Trb::new();
    trb.control = 9 << 10; // Enable Slot Command

    if let Some(ref mut controller) = *xhci::get_controller().lock() {
        unsafe {
            controller.send_command(trb);
        }
    }
}

pub fn on_slot_enabled(slot_id: u8) {
    log_info!("usb_core", "Slot {} enabled", slot_id);
    // In a production-grade implementation, we would now allocate Device Context
    // and send Address Device command.
}
