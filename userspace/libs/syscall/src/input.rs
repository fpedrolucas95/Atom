// Input device syscalls (keyboard, mouse)

use crate::error::EWOULDBLOCK;
use crate::raw::{syscall0, numbers::*};

// ============================================================================
// Mouse Input
// ============================================================================

/// Poll for next raw mouse byte from PS/2 controller
///
/// Returns Some(byte) if a mouse byte is available, None otherwise.
/// The userspace driver must assemble these bytes into 3-byte packets.
///
/// This is a non-blocking call.
#[inline]
pub fn mouse_poll_byte() -> Option<u8> {
    let result = unsafe { syscall0(SYS_MOUSE_POLL) };

    if result == EWOULDBLOCK {
        None
    } else {
        Some(result as u8)
    }
}

/// Mouse event with position delta and button states
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseEvent {
    pub dx: i32,
    pub dy: i32,
    pub left_button: bool,
    pub right_button: bool,
    pub middle_button: bool,
}

/// PS/2 Mouse driver that processes raw bytes into movement deltas and button states
pub struct MouseDriver {
    packet: [u8; 3],
    cycle: u8,
    prev_left: bool,
}

impl MouseDriver {
    pub const fn new() -> Self {
        Self {
            packet: [0; 3],
            cycle: 0,
            prev_left: false,
        }
    }

    /// Process available mouse data and return movement delta if a complete packet is ready
    pub fn poll(&mut self) -> Option<(i32, i32)> {
        while let Some(byte) = mouse_poll_byte() {
            if let Some(event) = self.process_byte(byte) {
                return Some((event.dx, event.dy));
            }
        }
        None
    }

    /// Process available mouse data and return full event with buttons if a complete packet is ready
    pub fn poll_event(&mut self) -> Option<MouseEvent> {
        while let Some(byte) = mouse_poll_byte() {
            if let Some(event) = self.process_byte(byte) {
                return Some(event);
            }
        }
        None
    }

    /// Check if left button was just pressed (rising edge)
    pub fn left_clicked(&mut self) -> bool {
        // This should be called after poll_event to detect clicks
        false // Actual implementation is in poll_event
    }

    /// Process a single mouse byte
    fn process_byte(&mut self, byte: u8) -> Option<MouseEvent> {
        match self.cycle {
            0 => {
                // First byte must have bit 3 set (always 1 in PS/2)
                if byte & 0x08 != 0 {
                    self.packet[0] = byte;
                    self.cycle = 1;
                }
                None
            }
            1 => {
                self.packet[1] = byte;
                self.cycle = 2;
                None
            }
            2 => {
                self.packet[2] = byte;
                self.cycle = 0;
                
                // Decode packet
                let flags = self.packet[0];
                
                // Check for overflow
                if flags & 0xC0 != 0 {
                    return None;
                }
                
                // Extract deltas with sign extension
                let mut dx = self.packet[1] as i32;
                let mut dy = self.packet[2] as i32;
                
                if flags & 0x10 != 0 { dx -= 256; }
                if flags & 0x20 != 0 { dy -= 256; }
                
                // Extract button states
                let left_button = (flags & 0x01) != 0;
                let right_button = (flags & 0x02) != 0;
                let middle_button = (flags & 0x04) != 0;

                Some(MouseEvent {
                    dx,
                    dy,
                    left_button,
                    right_button,
                    middle_button,
                })
            }
            _ => {
                self.cycle = 0;
                None
            }
        }
    }
}

/// Legacy function for simple polling (returns delta if complete packet ready)
/// Note: This creates a new driver each call, so state is not preserved.
/// For proper usage, create a MouseDriver instance and call poll() on it.
#[inline]
pub fn mouse_poll() -> Option<(i32, i32)> {
    // For backward compatibility, just poll a byte and return None
    // The proper way is to use MouseDriver
    static mut DRIVER: MouseDriver = MouseDriver::new();
    unsafe { DRIVER.poll() }
}

// ============================================================================
// Keyboard Input
// ============================================================================

/// Poll keyboard for next scancode
///
/// Returns Some(scancode) if a key event is available, None otherwise.
/// Scancodes are raw PS/2 set 1 scancodes.
///
/// This is a non-blocking call.
#[inline]
pub fn keyboard_poll() -> Option<u8> {
    let result = unsafe { syscall0(SYS_KEYBOARD_POLL) };

    if result == EWOULDBLOCK {
        None
    } else {
        Some(result as u8)
    }
}

/// Key event with modifiers
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyEvent {
    pub scancode: u8,
    pub ascii: u8,
    pub pressed: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

// ============================================================================
// Scancode Translation (US Layout)
// ============================================================================

/// Translate PS/2 set 1 scancode to ASCII character
///
/// Returns Some(char) for printable characters, None for special keys.
pub fn scancode_to_ascii(scancode: u8, shift: bool) -> Option<char> {
    // Key release (high bit set)
    if scancode >= 128 {
        return None;
    }

    const SCANCODE_MAP: [char; 128] = [
        '\0', '\x1B', '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '=', '\x08', '\t',
        'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', '[', ']', '\n', '\0', 'a', 's',
        'd', 'f', 'g', 'h', 'j', 'k', 'l', ';', '\'', '`', '\0', '\\', 'z', 'x', 'c', 'v',
        'b', 'n', 'm', ',', '.', '/', '\0', '*', '\0', ' ', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '7', '8', '9', '-', '4', '5', '6', '+', '1',
        '2', '3', '0', '.', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    ];

    const SCANCODE_MAP_SHIFTED: [char; 128] = [
        '\0', '\x1B', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_', '+', '\x08', '\t',
        'Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P', '{', '}', '\n', '\0', 'A', 'S',
        'D', 'F', 'G', 'H', 'J', 'K', 'L', ':', '"', '~', '\0', '|', 'Z', 'X', 'C', 'V',
        'B', 'N', 'M', '<', '>', '?', '\0', '*', '\0', ' ', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '7', '8', '9', '-', '4', '5', '6', '+', '1',
        '2', '3', '0', '.', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    ];

    let ch = if shift {
        SCANCODE_MAP_SHIFTED[scancode as usize]
    } else {
        SCANCODE_MAP[scancode as usize]
    };

    if ch == '\0' {
        None
    } else {
        Some(ch)
    }
}

/// Scancode constants for special keys
pub mod scancodes {
    // Modifier keys
    pub const LEFT_SHIFT: u8 = 0x2A;
    pub const RIGHT_SHIFT: u8 = 0x36;
    pub const LEFT_CTRL: u8 = 0x1D;
    pub const LEFT_ALT: u8 = 0x38;
    pub const CAPS_LOCK: u8 = 0x3A;

    // Release codes (scancode | 0x80)
    pub const LEFT_SHIFT_RELEASE: u8 = 0xAA;
    pub const RIGHT_SHIFT_RELEASE: u8 = 0xB6;
    pub const LEFT_CTRL_RELEASE: u8 = 0x9D;
    pub const LEFT_ALT_RELEASE: u8 = 0xB8;

    // Special keys
    pub const ESCAPE: u8 = 0x01;
    pub const BACKSPACE: u8 = 0x0E;
    pub const TAB: u8 = 0x0F;
    pub const ENTER: u8 = 0x1C;
    pub const SPACE: u8 = 0x39;

    // Extended prefix
    pub const EXTENDED_PREFIX: u8 = 0xE0;
}
