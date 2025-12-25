// Userspace Drivers Module
//
// This module contains drivers that run in ring 3 (userspace).
// All interaction with hardware happens through syscalls.
//
// Drivers in this module:
// - keyboard: PS/2 keyboard driver using syscalls for I/O port access
// - mouse: PS/2 mouse driver using syscalls for I/O port access
// - graphics: Framebuffer graphics driver using mapped memory

#![allow(dead_code)]

use crate::syscall::{
    ESUCCESS, EWOULDBLOCK, EINVAL, EPERM,
    SYS_THREAD_YIELD, SYS_MOUSE_POLL, SYS_KEYBOARD_POLL,
    SYS_IO_PORT_READ, SYS_IO_PORT_WRITE,
    SYS_REGISTER_IRQ_HANDLER, SYS_MAP_FRAMEBUFFER,
};

// ============================================================================
// Syscall Helpers
// ============================================================================

#[inline(always)]
unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

// ============================================================================
// Keyboard Driver (Userspace)
// ============================================================================

pub mod keyboard {
    use super::*;

    /// Poll keyboard for next available character
    pub fn poll() -> Option<u8> {
        let result = unsafe { syscall0(SYS_KEYBOARD_POLL) };

        if result == EWOULDBLOCK {
            None
        } else {
            Some(result as u8)
        }
    }

    /// Read PS/2 status register via syscall
    pub fn read_status() -> Result<u8, u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_READ, 0x64, 1) };
        if result == EPERM || result == EINVAL {
            Err(result)
        } else {
            Ok(result as u8)
        }
    }

    /// Read PS/2 data register via syscall
    pub fn read_data() -> Result<u8, u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_READ, 0x60, 1) };
        if result == EPERM || result == EINVAL {
            Err(result)
        } else {
            Ok(result as u8)
        }
    }

    /// Write to PS/2 command register via syscall
    pub fn write_command(cmd: u8) -> Result<(), u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_WRITE, 0x64, cmd as u64) };
        if result == ESUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }

    /// Write to PS/2 data register via syscall
    pub fn write_data(data: u8) -> Result<(), u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_WRITE, 0x60, data as u64) };
        if result == ESUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }

    /// Register as keyboard IRQ handler
    pub fn register_irq_handler(port: u64) -> Result<(), u64> {
        let result = unsafe { syscall2(SYS_REGISTER_IRQ_HANDLER, 1, port) };
        if result == ESUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }

    /// Translate scancode to ASCII (US layout)
    pub fn scancode_to_ascii(scancode: u8, shift: bool) -> Option<char> {
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

        if scancode >= 128 {
            return None; // Key release
        }

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
}

// ============================================================================
// Mouse Driver (Userspace)
// ============================================================================

pub mod mouse {
    use super::*;

    /// Mouse state
    #[derive(Debug, Clone, Copy, Default)]
    pub struct MouseState {
        pub x: i32,
        pub y: i32,
        pub buttons: u8,
    }

    /// Poll mouse for movement delta
    pub fn poll_delta() -> Option<(i32, i32)> {
        let result = unsafe { syscall0(SYS_MOUSE_POLL) };

        if result == EWOULDBLOCK {
            None
        } else {
            let dx = (result >> 32) as i32;
            let dy = result as i32;
            Some((dx, dy))
        }
    }

    /// Read PS/2 mouse status via syscall
    pub fn read_status() -> Result<u8, u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_READ, 0x64, 1) };
        if result == EPERM || result == EINVAL {
            Err(result)
        } else {
            Ok(result as u8)
        }
    }

    /// Read PS/2 mouse data via syscall
    pub fn read_data() -> Result<u8, u64> {
        let result = unsafe { syscall2(SYS_IO_PORT_READ, 0x60, 1) };
        if result == EPERM || result == EINVAL {
            Err(result)
        } else {
            Ok(result as u8)
        }
    }

    /// Register as mouse IRQ handler
    pub fn register_irq_handler(port: u64) -> Result<(), u64> {
        let result = unsafe { syscall2(SYS_REGISTER_IRQ_HANDLER, 12, port) };
        if result == ESUCCESS {
            Ok(())
        } else {
            Err(result)
        }
    }
}

// ============================================================================
// Graphics Driver (Userspace)
// ============================================================================

pub mod graphics {
    use super::*;

    /// Framebuffer information
    #[derive(Debug, Clone, Copy)]
    pub struct FramebufferInfo {
        pub address: usize,
        pub width: u32,
        pub height: u32,
        pub stride: u32,
        pub bytes_per_pixel: u32,
        pub size: usize,
    }

    /// Color representation
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
    }

    impl Color {
        pub const BLACK: Color = Color { r: 0, g: 0, b: 0 };
        pub const WHITE: Color = Color { r: 255, g: 255, b: 255 };
        pub const RED: Color = Color { r: 255, g: 0, b: 0 };
        pub const GREEN: Color = Color { r: 0, g: 255, b: 0 };
        pub const BLUE: Color = Color { r: 0, g: 0, b: 255 };
        pub const YELLOW: Color = Color { r: 255, g: 255, b: 0 };
        pub const CYAN: Color = Color { r: 0, g: 255, b: 255 };
        pub const MAGENTA: Color = Color { r: 255, g: 0, b: 255 };
        pub const GRAY: Color = Color { r: 128, g: 128, b: 128 };
        pub const DARK_GRAY: Color = Color { r: 64, g: 64, b: 64 };
        pub const LIGHT_GRAY: Color = Color { r: 192, g: 192, b: 192 };

        pub const fn new(r: u8, g: u8, b: u8) -> Self {
            Self { r, g, b }
        }

        /// Convert to BGR32 pixel value
        pub fn to_bgr32(&self) -> u32 {
            ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
        }

        /// Convert to RGB32 pixel value
        pub fn to_rgb32(&self) -> u32 {
            ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
        }
    }

    /// Get framebuffer information via syscall
    pub fn get_framebuffer_info() -> Option<FramebufferInfo> {
        let mut info = [0u64; 6];
        let result = unsafe { syscall1(SYS_MAP_FRAMEBUFFER, info.as_mut_ptr() as u64) };

        if result == ESUCCESS {
            Some(FramebufferInfo {
                address: info[0] as usize,
                width: info[1] as u32,
                height: info[2] as u32,
                stride: info[3] as u32,
                bytes_per_pixel: info[4] as u32,
                size: info[5] as usize,
            })
        } else {
            None
        }
    }

    /// Framebuffer handle for drawing operations
    pub struct Framebuffer {
        info: FramebufferInfo,
    }

    impl Framebuffer {
        /// Create a new framebuffer handle
        pub fn new() -> Option<Self> {
            get_framebuffer_info().map(|info| Self { info })
        }

        pub fn width(&self) -> u32 {
            self.info.width
        }

        pub fn height(&self) -> u32 {
            self.info.height
        }

        pub fn stride(&self) -> u32 {
            self.info.stride
        }

        /// Draw a single pixel
        pub fn draw_pixel(&self, x: u32, y: u32, color: Color) {
            if x >= self.info.width || y >= self.info.height {
                return;
            }

            let offset = (y * self.info.stride + x) as usize * self.info.bytes_per_pixel as usize;
            let ptr = (self.info.address + offset) as *mut u32;

            unsafe {
                core::ptr::write_volatile(ptr, color.to_bgr32());
            }
        }

        /// Fill a rectangle
        pub fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: Color) {
            for dy in 0..height {
                for dx in 0..width {
                    self.draw_pixel(x + dx, y + dy, color);
                }
            }
        }

        /// Clear the screen
        pub fn clear(&self, color: Color) {
            self.fill_rect(0, 0, self.info.width, self.info.height, color);
        }

        /// Draw a character using 8x8 font
        pub fn draw_char(&self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
            let glyph = get_font_glyph(ch);

            for row in 0..8 {
                for col in 0..8 {
                    let bit = (glyph[row] >> col) & 1;
                    let color = if bit == 1 { fg } else { bg };
                    self.draw_pixel(x + col as u32, y + row as u32, color);
                }
            }
        }

        /// Draw a string
        pub fn draw_string(&self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
            let mut offset_x = x;
            for byte in text.bytes() {
                if offset_x + 8 > self.info.width {
                    break;
                }
                self.draw_char(offset_x, y, byte, fg, bg);
                offset_x += 8;
            }
        }
    }

    /// Simple 8x8 font data
    fn get_font_glyph(ch: u8) -> &'static [u8; 8] {
        const FONT_DATA: [[u8; 8]; 96] = [
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // space
            [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00], // !
            [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // "
            [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00], // #
            [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00], // $
            [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00], // %
            [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00], // &
            [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00], // '
            [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00], // (
            [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00], // )
            [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00], // *
            [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00], // +
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ,
            [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00], // -
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00], // .
            [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // /
            [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00], // 0
            [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00], // 1
            [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00], // 2
            [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00], // 3
            [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00], // 4
            [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00], // 5
            [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00], // 6
            [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00], // 7
            [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00], // 8
            [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00], // 9
            [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00], // :
            [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ;
            [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00], // <
            [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00], // =
            [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00], // >
            [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00], // ?
            [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00], // @
            [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00], // A
            [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00], // B
            [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00], // C
            [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00], // D
            [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00], // E
            [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00], // F
            [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00], // G
            [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // H
            [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // I
            [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00], // J
            [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00], // K
            [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00], // L
            [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00], // M
            [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00], // N
            [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00], // O
            [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00], // P
            [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00], // Q
            [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00], // R
            [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00], // S
            [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // T
            [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00], // U
            [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // V
            [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // W
            [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00], // X
            [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00], // Y
            [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00], // Z
            [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00], // [
            [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00], // backslash
            [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00], // ]
            [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00], // ^
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF], // _
            [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00], // `
            [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00], // a
            [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00], // b
            [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00], // c
            [0x38, 0x30, 0x30, 0x3e, 0x33, 0x33, 0x6E, 0x00], // d
            [0x00, 0x00, 0x1E, 0x33, 0x3f, 0x03, 0x1E, 0x00], // e
            [0x1C, 0x36, 0x06, 0x0f, 0x06, 0x06, 0x0F, 0x00], // f
            [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F], // g
            [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00], // h
            [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // i
            [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E], // j
            [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00], // k
            [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // l
            [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00], // m
            [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00], // n
            [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00], // o
            [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F], // p
            [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78], // q
            [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00], // r
            [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00], // s
            [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00], // t
            [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00], // u
            [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // v
            [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00], // w
            [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00], // x
            [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F], // y
            [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00], // z
            [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00], // {
            [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00], // |
            [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00], // }
            [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ~
            [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // DEL
        ];

        let index = if ch >= 32 && ch < 128 {
            (ch - 32) as usize
        } else {
            0
        };

        &FONT_DATA[index]
    }
}

// ============================================================================
// Userspace Shell Entry Point
// ============================================================================

/// Entry point for userspace shell
/// This function runs in ring 3 and uses only syscalls
pub fn userspace_shell_entry() -> ! {
    // Get framebuffer via syscall
    let fb = match graphics::Framebuffer::new() {
        Some(fb) => fb,
        None => {
            // No framebuffer, just loop
            loop {
                sys_yield();
            }
        }
    };

    // Clear screen
    fb.clear(graphics::Color::BLACK);

    // Draw welcome message
    fb.draw_string(10, 10, "Atom OS - Userspace Shell", graphics::Color::WHITE, graphics::Color::BLACK);
    fb.draw_string(10, 30, "Running in Ring 3 (User Mode)", graphics::Color::GREEN, graphics::Color::BLACK);

    // Cursor position
    let mut cursor_x = 10u32;
    let mut cursor_y = 60u32;
    let mut shift_pressed = false;

    // Draw prompt
    fb.draw_string(cursor_x, cursor_y, "> ", graphics::Color::CYAN, graphics::Color::BLACK);
    cursor_x += 16;

    loop {
        // Poll keyboard via syscall
        if let Some(scancode) = keyboard::poll() {
            // Handle shift keys
            if scancode == 0x2A || scancode == 0x36 {
                shift_pressed = true;
            } else if scancode == 0xAA || scancode == 0xB6 {
                shift_pressed = false;
            } else if scancode < 0x80 {
                // Key press (not release)
                if let Some(ch) = keyboard::scancode_to_ascii(scancode, shift_pressed) {
                    if ch == '\n' {
                        // New line
                        cursor_x = 10;
                        cursor_y += 10;
                        if cursor_y > fb.height() - 20 {
                            cursor_y = 60;
                            // Clear display area
                            fb.fill_rect(0, 50, fb.width(), fb.height() - 50, graphics::Color::BLACK);
                        }
                        // Draw prompt
                        fb.draw_string(cursor_x, cursor_y, "> ", graphics::Color::CYAN, graphics::Color::BLACK);
                        cursor_x += 16;
                    } else if ch == '\x08' {
                        // Backspace
                        if cursor_x > 26 {
                            cursor_x -= 8;
                            fb.draw_char(cursor_x, cursor_y, b' ', graphics::Color::BLACK, graphics::Color::BLACK);
                        }
                    } else {
                        // Regular character
                        fb.draw_char(cursor_x, cursor_y, ch as u8, graphics::Color::WHITE, graphics::Color::BLACK);
                        cursor_x += 8;
                        if cursor_x > fb.width() - 20 {
                            cursor_x = 10;
                            cursor_y += 10;
                        }
                    }
                }
            }
        }

        // Poll mouse and draw cursor
        if let Some((dx, dy)) = mouse::poll_delta() {
            // Could update mouse position here
            let _ = (dx, dy);
        }

        // Yield CPU
        sys_yield();
    }
}

/// Yield CPU via syscall
fn sys_yield() {
    unsafe {
        syscall0(SYS_THREAD_YIELD);
    }
}
