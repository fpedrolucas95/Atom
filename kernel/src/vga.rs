// VGA Text Mode Driver
//
// Provides a minimal VGA text-mode output facility for the Atom kernel.
// This driver is primarily intended for early boot, fallback display,
// and panic-time diagnostics when more advanced graphics output
// (UEFI GOP / framebuffer) is unavailable or unsafe to use.
//
// Key responsibilities:
// - Write characters directly to the VGA text buffer (0xB8000)
// - Provide basic text rendering with colors, scrolling, and clearing
// - Serve as a fallback console and panic-safe output path
//
// Design principles:
// - Extremely low-level and deterministic
// - No dynamic allocation and minimal dependencies
// - Direct hardware buffer access using volatile writes
// - Safe to use even in critical or failure contexts
//
// Implementation details:
// - Assumes VGA is already configured by firmware (80×25 text mode)
// - Each cell is a 16-bit value: ASCII byte + color attribute
// - `VgaWriter` tracks cursor position and current colors
// - Automatic scrolling is implemented by copying buffer rows upward
//
// Concurrency and safety:
// - Global writer is protected by a spinlock for normal operation
// - Interrupts are disabled during writes to prevent interleaving
// - Panic paths can bypass higher-level logging and write directly
//
// Integration points:
// - Used by the logging subsystem as an optional output backend
// - Used for boot banners and early diagnostic messages
// - Serves as a fallback when framebuffer graphics are unavailable
//
// Architectural note (future direction):
// - This module is temporary and transitional
// - In the intended design, console rendering and text UI will move
//   to user space, built on top of framebuffer or windowing services
// - The kernel will eventually expose only minimal framebuffer access
//   and logging primitives
//
// Correctness and safety notes:
// - All writes are volatile to prevent compiler reordering
// - Bounds checks prevent writing past screen limits
// - Assumes a single active writer in normal operation
//
// This driver favors robustness and debuggability over abstraction,
// making it suitable as a last-resort output mechanism.

use core::fmt;
use core::ptr;
use spin::Mutex;

const VGA_BUFFER: *mut u16 = 0xB8000 as *mut u16;
const VGA_WIDTH: usize = 80;
const VGA_HEIGHT: usize = 25;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[inline]
fn make_color(fg: Color, bg: Color) -> u8 {
    (bg as u8) << 4 | (fg as u8)
}

#[inline]
fn make_vga_entry(c: u8, color: u8) -> u16 {
    (color as u16) << 8 | c as u16
}

pub struct VgaWriter {
    row: usize,
    col: usize,
    fg_color: Color,
    bg_color: Color,
}

impl VgaWriter {
    pub const fn new() -> Self {
        VgaWriter {
            row: 0,
            col: 0,
            fg_color: Color::White,
            bg_color: Color::Black,
        }
    }

    pub fn set_color(&mut self, fg: Color, bg: Color) {
        self.fg_color = fg;
        self.bg_color = bg;
    }

    pub fn clear_screen(&mut self) {
        let color = make_color(self.fg_color, self.bg_color);
        let blank = make_vga_entry(b' ', color);

        unsafe {
            for i in 0..(VGA_WIDTH * VGA_HEIGHT) {
                ptr::write_volatile(VGA_BUFFER.add(i), blank);
            }
        }

        self.row = 0;
        self.col = 0;
    }

    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            b'\r' => self.col = 0,
            byte => {
                if self.col >= VGA_WIDTH {
                    self.new_line();
                }

                let color = make_color(self.fg_color, self.bg_color);
                let offset = self.row * VGA_WIDTH + self.col;
                let entry = make_vga_entry(byte, color);

                unsafe {
                    ptr::write_volatile(VGA_BUFFER.add(offset), entry);
                }

                self.col += 1;
            }
        }
    }

    pub fn write_string(&mut self, s: &str) {
        for byte in s.bytes() {
            match byte {
                0x20..=0x7e | b'\n' | b'\r' => self.write_byte(byte),
                _ => self.write_byte(0xfe),
            }
        }
    }

    fn new_line(&mut self) {
        self.col = 0;
        self.row += 1;

        if self.row >= VGA_HEIGHT {
            self.scroll();
            self.row = VGA_HEIGHT - 1;
        }
    }

    fn scroll(&mut self) {
        let color = make_color(self.fg_color, self.bg_color);
        let blank = make_vga_entry(b' ', color);

        unsafe {
            for row in 1..VGA_HEIGHT {
                for col in 0..VGA_WIDTH {
                    let src = row * VGA_WIDTH + col;
                    let dst = (row - 1) * VGA_WIDTH + col;
                    let entry = ptr::read_volatile(VGA_BUFFER.add(src));
                    ptr::write_volatile(VGA_BUFFER.add(dst), entry);
                }
            }

            for col in 0..VGA_WIDTH {
                let offset = (VGA_HEIGHT - 1) * VGA_WIDTH + col;
                ptr::write_volatile(VGA_BUFFER.add(offset), blank);
            }
        }
    }

    #[allow(dead_code)]
    pub fn write_string_at(&self, s: &str, row: usize, col: usize, fg: Color, bg: Color) {
        let color = make_color(fg, bg);
        let offset = row * VGA_WIDTH + col;

        unsafe {
            for (i, byte) in s.bytes().enumerate() {
                if col + i >= VGA_WIDTH {
                    break;
                }
                let entry = make_vga_entry(byte, color);
                ptr::write_volatile(VGA_BUFFER.add(offset + i), entry);
            }
        }
    }

    #[allow(dead_code)]
    pub fn get_position(&self) -> (usize, usize) {
        (self.row, self.col)
    }
}

impl fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

pub static WRITER: Mutex<VgaWriter> = Mutex::new(VgaWriter::new());

pub fn init() {
    let mut writer = WRITER.lock();
    writer.clear_screen();
}

#[allow(dead_code)]
pub fn display_boot_message() {
    let mut writer = WRITER.lock();
    writer.clear_screen();
    writer.set_color(Color::LightCyan, Color::Black);
    writer.write_string(crate::build_info::BOOT_BANNER);
    writer.write_string("\n");
    writer.set_color(Color::DarkGray, Color::Black);
    writer.write_string("================================================================\n\n");
    writer.set_color(Color::White, Color::Black);
}

#[allow(dead_code)]
pub fn clear_screen(bg_color: Color) {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    {
        let mut writer = WRITER.lock();
        writer.set_color(Color::White, bg_color);
        writer.clear_screen();
    }

    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
}

#[macro_export]
macro_rules! vga_print {
    ($($arg:tt)*) => ($crate::vga::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! vga_println {
    () => ($crate::vga_print!("\n"));
    ($($arg:tt)*) => ($crate::vga_print!("{}\n", format_args!($($arg)*)));
}

pub fn write_colored(s: &str, fg: Color, bg: Color) {
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    {
        let mut writer = WRITER.lock();
        let old_fg = writer.fg_color;
        let old_bg = writer.bg_color;
        writer.set_color(fg, bg);
        writer.write_string(s);
        writer.set_color(old_fg, old_bg);
    }

    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
}