// Input device syscalls (keyboard, mouse)

use crate::raw::{numbers::*, syscall0, syscall1};

// ============================================================================
// Mouse Input
// ============================================================================

/// Poll for next raw mouse byte from PS/2 controller.
///
/// Returns Some(byte) if a mouse byte is available, None otherwise.
/// Non-blocking.
#[inline]
pub fn mouse_poll_byte() -> Option<u8> {
    let mut byte = 0u8;
    let result = unsafe { syscall1(SYS_MOUSE_POLL, &mut byte as *mut u8 as u64) };

    if result == crate::error::ESUCCESS {
        Some(byte)
    } else {
        None
    }
}

/// Get the PS/2 mouseID detected at boot.
///
/// Returns:
///   0 = basic 3-button mouse (3-byte packets)
///   3 = scroll wheel mouse (4-byte packets, Z-axis)
///   4 = 5-button mouse (4-byte packets, Z-axis + buttons 4 & 5)
#[inline]
pub fn mouse_get_id() -> u8 {
    unsafe { syscall0(SYS_MOUSE_GET_ID) as u8 }
}

/// Mouse event with position delta, button states, and scroll.
#[derive(Debug, Clone, Copy, Default)]
pub struct MouseEvent {
    pub dx: i32,
    pub dy: i32,
    pub dz: i32,
    pub left_button: bool,
    pub right_button: bool,
    pub middle_button: bool,
    pub button4: bool,
    pub button5: bool,
}

/// PS/2 Mouse driver that processes raw bytes into events.
///
/// Supports all three mouse modes:
///   - mouseID 0: 3-byte packets (basic 3-button)
///   - mouseID 3: 4-byte packets (scroll wheel, signed Z)
///   - mouseID 4: 4-byte packets (scroll wheel + 4th/5th buttons)
///
/// Uses the OSDev-recommended sign extension formula for delta values:
///   rel_x = raw_x - ((flags << 4) & 0x100)
///   rel_y = raw_y - ((flags << 3) & 0x100)
pub struct MouseDriver {
    packet: [u8; 4],
    cycle: u8,
    packet_size: u8, // 3 or 4 bytes
    mouse_id: u8,    // 0, 3, or 4
}

impl Default for MouseDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseDriver {
    pub const fn new() -> Self {
        Self {
            packet: [0; 4],
            cycle: 0,
            packet_size: 3,
            mouse_id: 0,
        }
    }

    /// Initialize the driver by querying the kernel for the detected mouseID.
    /// Must be called before polling.
    pub fn init(&mut self) {
        self.mouse_id = mouse_get_id();
        self.packet_size = if self.mouse_id >= 3 { 4 } else { 3 };
    }

    /// Return the detected mouseID.
    pub fn mouse_id(&self) -> u8 {
        self.mouse_id
    }

    /// Return the packet size (3 or 4).
    pub fn packet_size(&self) -> u8 {
        self.packet_size
    }

    /// Poll for a complete mouse event.
    /// Returns Some(event) when a full packet has been assembled.
    pub fn poll_event(&mut self) -> Option<MouseEvent> {
        while let Some(byte) = mouse_poll_byte() {
            if let Some(event) = self.process_byte(byte) {
                return Some(event);
            }
        }
        None
    }

    /// Process a single raw byte from the PS/2 mouse stream.
    ///
    /// Returns Some(MouseEvent) when a complete packet is assembled.
    /// Uses bit 3 of the first byte (always-1 flag) for packet sync.
    /// Discards packets with X or Y overflow (bits 6-7 of byte 0).
    fn process_byte(&mut self, byte: u8) -> Option<MouseEvent> {
        match self.cycle {
            0 => {
                // First byte: verify bit 3 is set (PS/2 "always 1" bit)
                // This helps maintain packet alignment.
                if byte & 0x08 != 0 {
                    self.packet[0] = byte;
                    self.cycle = 1;
                }
                // If bit 3 is not set, skip this byte (resync)
                None
            }
            1 => {
                self.packet[1] = byte;
                self.cycle = 2;
                None
            }
            2 => {
                self.packet[2] = byte;

                if self.packet_size == 3 {
                    // Complete 3-byte packet
                    self.cycle = 0;
                    return self.decode_packet();
                }

                // Wait for 4th byte
                self.cycle = 3;
                None
            }
            3 => {
                self.packet[3] = byte;
                self.cycle = 0;
                self.decode_packet()
            }
            _ => {
                self.cycle = 0;
                None
            }
        }
    }

    /// Decode a complete PS/2 mouse packet into a MouseEvent.
    ///
    /// Packet format (first 3 bytes, always present):
    ///   byte 0: [Y_overflow | X_overflow | Y_sign | X_sign | 1 | Mid | Right | Left]
    ///   byte 1: X movement (unsigned, sign in byte 0 bit 4)
    ///   byte 2: Y movement (unsigned, sign in byte 0 bit 5)
    ///
    /// 4th byte (mouseID 3): Z movement (signed 8-bit, -8 to +7 for scroll)
    /// 4th byte (mouseID 4): [0 | 0 | btn5 | btn4 | Z3 | Z2 | Z1 | Z0] (Z is signed 4-bit)
    fn decode_packet(&self) -> Option<MouseEvent> {
        let flags = self.packet[0];

        // Discard packets with X or Y overflow (bits 6-7)
        if flags & 0xC0 != 0 {
            return None;
        }

        // Extract deltas using OSDev recommended formula:
        //   rel_x = raw_x - ((flags << 4) & 0x100)
        //   rel_y = raw_y - ((flags << 3) & 0x100)
        // This correctly produces a signed value from the 9-bit two's complement.
        let dx = (self.packet[1] as i32) - (((flags as i32) << 4) & 0x100);
        let dy = (self.packet[2] as i32) - (((flags as i32) << 3) & 0x100);

        // Button states from first byte
        let left_button = flags & 0x01 != 0;
        let right_button = flags & 0x02 != 0;
        let middle_button = flags & 0x04 != 0;

        // 4th byte: scroll and extra buttons
        let mut dz: i32 = 0;
        let mut button4 = false;
        let mut button5 = false;

        if self.packet_size == 4 {
            let byte4 = self.packet[3];

            match self.mouse_id {
                3 => {
                    // mouseID 3: byte 4 is signed 8-bit Z movement
                    // Values: 0 = no scroll, 1 = up, 0xFF = down,
                    //         2 = right, 0xFE = left
                    dz = byte4 as i8 as i32;
                }
                4 => {
                    // mouseID 4: low nibble is signed 4-bit Z, bits 4-5 are buttons
                    button4 = byte4 & 0x10 != 0;
                    button5 = byte4 & 0x20 != 0;

                    // Sign-extend 4-bit Z value
                    let z_raw = byte4 & 0x0F;
                    dz = if z_raw & 0x08 != 0 {
                        // Negative: sign-extend from 4 bits
                        (z_raw | 0xF0) as i8 as i32
                    } else {
                        z_raw as i32
                    };
                }
                _ => {}
            }
        }

        Some(MouseEvent {
            dx,
            dy,
            dz,
            left_button,
            right_button,
            middle_button,
            button4,
            button5,
        })
    }
}

/// Simple polling function for backward compatibility.
/// Creates a persistent driver instance internally.
#[inline]
#[allow(static_mut_refs)]
pub fn mouse_poll() -> Option<(i32, i32)> {
    static mut DRIVER: MouseDriver = MouseDriver::new();
    static mut INITIALIZED: bool = false;
    unsafe {
        if !INITIALIZED {
            DRIVER.init();
            INITIALIZED = true;
        }
        DRIVER.poll_event().map(|e| (e.dx, e.dy))
    }
}

// ============================================================================
// Keyboard Input
// ============================================================================

/// Poll keyboard for next scancode.
///
/// Returns Some(scancode) if a key event is available, None otherwise.
/// Scancodes are raw PS/2 set 1 scancodes.
/// Non-blocking.
#[inline]
pub fn keyboard_poll() -> Option<u8> {
    let mut scancode = 0u8;
    let result = unsafe { syscall1(SYS_KEYBOARD_POLL, &mut scancode as *mut u8 as u64) };

    if result == crate::error::ESUCCESS {
        Some(scancode)
    } else {
        None
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

/// Translate PS/2 set 1 scancode to ASCII character.
///
/// Returns Some(char) for printable characters, None for special keys.
pub fn scancode_to_ascii(scancode: u8, shift: bool) -> Option<char> {
    if scancode >= 128 {
        return None;
    }

    const SCANCODE_MAP: [char; 128] = [
        '\0', '\x1B', '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '-', '=', '\x08', '\t',
        'q', 'w', 'e', 'r', 't', 'y', 'u', 'i', 'o', 'p', '[', ']', '\n', '\0', 'a', 's', 'd', 'f',
        'g', 'h', 'j', 'k', 'l', ';', '\'', '`', '\0', '\\', 'z', 'x', 'c', 'v', 'b', 'n', 'm',
        ',', '.', '/', '\0', '*', '\0', ' ', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '7', '8', '9', '-', '4', '5', '6', '+', '1', '2', '3', '0', '.',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
    ];

    const SCANCODE_MAP_SHIFTED: [char; 128] = [
        '\0', '\x1B', '!', '@', '#', '$', '%', '^', '&', '*', '(', ')', '_', '+', '\x08', '\t',
        'Q', 'W', 'E', 'R', 'T', 'Y', 'U', 'I', 'O', 'P', '{', '}', '\n', '\0', 'A', 'S', 'D', 'F',
        'G', 'H', 'J', 'K', 'L', ':', '"', '~', '\0', '|', 'Z', 'X', 'C', 'V', 'B', 'N', 'M', '<',
        '>', '?', '\0', '*', '\0', ' ', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '7', '8', '9', '-', '4', '5', '6', '+', '1', '2', '3', '0', '.', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
        '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0', '\0',
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
    pub const LEFT_SHIFT: u8 = 0x2A;
    pub const RIGHT_SHIFT: u8 = 0x36;
    pub const LEFT_CTRL: u8 = 0x1D;
    pub const LEFT_ALT: u8 = 0x38;
    pub const CAPS_LOCK: u8 = 0x3A;

    pub const LEFT_SHIFT_RELEASE: u8 = 0xAA;
    pub const RIGHT_SHIFT_RELEASE: u8 = 0xB6;
    pub const LEFT_CTRL_RELEASE: u8 = 0x9D;
    pub const LEFT_ALT_RELEASE: u8 = 0xB8;

    pub const ESCAPE: u8 = 0x01;
    pub const BACKSPACE: u8 = 0x0E;
    pub const TAB: u8 = 0x0F;
    pub const ENTER: u8 = 0x1C;
    pub const SPACE: u8 = 0x39;

    pub const EXTENDED_PREFIX: u8 = 0xE0;
}
