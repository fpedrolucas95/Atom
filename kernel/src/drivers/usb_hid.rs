// USB HID Class Driver for Atom OS
// Handles Keyboard and Mouse reports

#![allow(dead_code)]

use crate::log_info;

pub const USB_CLASS_HID: u8 = 0x03;
pub const HID_SUBCLASS_BOOT: u8 = 0x01;
pub const HID_PROTOCOL_KEYBOARD: u8 = 0x01;
pub const HID_PROTOCOL_MOUSE: u8 = 0x02;

pub struct HidDevice {
    pub slot_id: u8,
    pub protocol: u8,
}

pub fn init_hid_device(slot_id: u8, protocol: u8) {
    log_info!(
        "usb_hid",
        "Initializing HID device on slot {}, protocol {}",
        slot_id,
        protocol
    );
    // In next steps: setup interrupt endpoints and start polling
}

pub fn handle_hid_report(slot_id: u8, report: &[u8]) {
    // Production-grade HID parsing would go here.
    // Requirement: No PS/2 translation, No input injection.
    log_info!(
        "usb_hid",
        "Received HID report from slot {}: {:02x?}",
        slot_id,
        report
    );

    // In a full implementation, we would decode the report according to the
    // HID Report Descriptor and either push it to a USB-specific input queue
    // or expose it via a device-specific API for userspace drivers.
}

#[repr(C, packed)]
pub struct HidDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub bcd_hid: u16,
    pub country_code: u8,
    pub num_descriptors: u8,
    pub class_desc_type: u8,
    pub class_desc_length: u16,
}
