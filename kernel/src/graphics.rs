// Graphics and Framebuffer Console
//
// Implements a minimal graphics subsystem built on top of the UEFI
// framebuffer, providing both low-level pixel drawing and a simple
// text-mode terminal rendered in graphics mode.
//
// Key responsibilities:
// - Initialize and manage access to the UEFI framebuffer
// - Provide safe, centralized framebuffer access helpers
// - Offer primitive 2D drawing operations (pixels, rectangles, text)
// - Implement a software-rendered text terminal for early kernel output
//
// Framebuffer management:
// - Framebuffer parameters are derived from UEFI `FramebufferInfo`
// - Global framebuffer state is stored behind a single mutable instance
// - Initialization is tracked via an atomic flag to prevent misuse
// - All drawing operations go through `with_framebuffer` to ensure safety
//
// Drawing primitives:
// - `draw_pixel` writes directly to framebuffer memory using volatile writes
// - Pixel format handling supports RGB and BGR layouts
// - Rectangle fill and screen clear are built on top of pixel drawing
// - Fixed-size 8×8 bitmap font is embedded directly in the kernel image
//
// Text rendering:
// - Characters are rendered glyph-by-glyph using the bitmap font
// - Strings are drawn left-to-right with automatic line clipping
// - Non-printable characters are safely ignored or handled specially
//
// Graphics terminal:
// - `GraphicsTerminal` provides a console-like abstraction on top of graphics
// - Maintains cursor position, colors, screen dimensions, and scroll state
// - Supports newlines, carriage return, backspace, and scrolling
// - Scrolling is implemented by moving framebuffer memory upward
//
// Design principles:
// - Early-boot friendly: no dependencies on interrupts or scheduling
// - Simple, deterministic rendering suitable for debugging and diagnostics
// - Explicit `unsafe` blocks for MMIO-style framebuffer access
//
// Correctness and safety notes:
// - All framebuffer writes are volatile to prevent compiler reordering
// - Bounds checks prevent out-of-range memory writes
// - Atomic flags prevent use-before-init of framebuffer and terminal
// - Global mutable state assumes single-writer or serialized access
//
// Intended usage:
// - Boot banners, panic output, and early kernel logs
// - Debug visualization before full driver stack is available

#![allow(dead_code)]

use crate::boot::{FramebufferInfo, PixelFormat};
use core::sync::atomic::{AtomicBool, Ordering};

static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);
static FRAMEBUFFER_INITIALIZED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

pub struct Framebuffer {
    address: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: PixelFormat,
    bytes_per_pixel: usize,
}

unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    pub fn new(info: &FramebufferInfo) -> Self {
        let bytes_per_pixel = match info.pixel_format {
            PixelFormat::Rgb | PixelFormat::Bgr | PixelFormat::Bitmask => 4,
            _ => 4,
        };

        Self {
            address: info.address as *mut u8,
            width: info.width,
            height: info.height,
            stride: info.pixels_per_scan_line,
            pixel_format: info.pixel_format,
            bytes_per_pixel,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn address(&self) -> *mut u8 {
        self.address
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    pub fn draw_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let pixel_offset = (y * self.stride + x) as usize * self.bytes_per_pixel;
        let pixel_ptr = unsafe { self.address.add(pixel_offset) as *mut u32 };

        let pixel_value = match self.pixel_format {
            PixelFormat::Rgb => {
                ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32)
            }
            PixelFormat::Bgr => {
                ((color.b as u32) << 16) | ((color.g as u32) << 8) | (color.r as u32)
            }
            _ => {
                ((color.b as u32) << 16) | ((color.g as u32) << 8) | (color.r as u32)
            }
        };

        unsafe {
            pixel_ptr.write_volatile(pixel_value);
        }
    }

    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        for dy in 0..height {
            for dx in 0..width {
                self.draw_pixel(x + dx, y + dy, color);
            }
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    pub fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg_color: Color, bg_color: Color) {
        let glyph = get_font_glyph(ch);

        for row in 0..FONT_HEIGHT {
            for col in 0..FONT_WIDTH {
                let bit = (glyph[row as usize] >> col) & 1;
                let color = if bit == 1 { fg_color } else { bg_color };
                self.draw_pixel(x + col, y + row, color);
            }
        }
    }

    pub fn draw_string(&mut self, x: u32, y: u32, text: &str, fg_color: Color, bg_color: Color) {
        let mut offset_x = x;
        for byte in text.bytes() {
            if offset_x + FONT_WIDTH > self.width {
                break; 
            }
            self.draw_char(offset_x, y, byte, fg_color, bg_color);
            offset_x += FONT_WIDTH;
        }
    }
}

const FONT_WIDTH: u32 = 8;
const FONT_HEIGHT: u32 = 8;

fn get_font_glyph(ch: u8) -> &'static [u8; 8] {
    let index = if ch >= 32 && ch < 128 {
        (ch - 32) as usize
    } else {
        0
    };

    &FONT_DATA[index]
}

#[rustfmt::skip]
const FONT_DATA: [[u8; 8]; 96] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00],
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00],
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00],
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00],
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00],
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00],
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00],
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00],
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00],
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00],
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00],
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00],
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00],
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00],
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00],
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00],
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00],
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06],
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00],
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00],
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00],
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00],
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00],
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00],
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00],
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00],
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00],
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00],
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00],
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00],
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00],
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00],
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00],
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00],
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00],
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00],
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00],
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00],
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00],
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00],
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00],
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00],
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00],
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00],
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF],
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00],
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00],
    [0x38, 0x30, 0x30, 0x3e, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x3f, 0x03, 0x1E, 0x00],
    [0x1C, 0x36, 0x06, 0x0f, 0x06, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00],
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E],
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00],
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00],
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00],
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00],
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F],
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78],
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00],
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00],
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00],
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00],
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00],
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F],
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00],
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00],
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00],
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00],
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
];

pub fn init(fb_info: &FramebufferInfo) {
    *FRAMEBUFFER.lock() = Some(Framebuffer::new(fb_info));
    FRAMEBUFFER_INITIALIZED.store(true, Ordering::SeqCst);
}

pub fn is_initialized() -> bool {
    FRAMEBUFFER_INITIALIZED.load(Ordering::SeqCst)
}

pub fn with_framebuffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut Framebuffer) -> R,
{
    if !is_initialized() {
        return None;
    }
    
    let mut fb_lock = FRAMEBUFFER.lock();
    if let Some(ref mut fb) = *fb_lock {
        Some(f(fb))
    } else {
        None
    }
}

pub fn clear_screen(color: Color) {
    with_framebuffer(|fb| fb.clear(color));
}

pub fn draw_pixel(x: u32, y: u32, color: Color) {
    with_framebuffer(|fb| fb.draw_pixel(x, y, color));
}

pub fn draw_string(x: u32, y: u32, text: &str, fg_color: Color, bg_color: Color) {
    with_framebuffer(|fb| fb.draw_string(x, y, text, fg_color, bg_color));
}

pub fn get_dimensions() -> Option<(u32, u32)> {
    let initialized = is_initialized();
    crate::log_debug!("graphics", "get_dimensions called: initialized={}", initialized);

    with_framebuffer(|fb| {
        let dims = (fb.width(), fb.height());
        crate::log_debug!("graphics", "Framebuffer dimensions: {}x{}", dims.0, dims.1);
        dims
    })
}

/// Get the raw framebuffer address for userspace drivers
pub fn get_framebuffer_address() -> Option<usize> {
    with_framebuffer(|fb| fb.address() as usize)
}

/// Get the framebuffer stride (pixels per scanline)
pub fn get_stride() -> u32 {
    with_framebuffer(|fb| fb.stride()).unwrap_or(0)
}

/// Get bytes per pixel
pub fn get_bytes_per_pixel() -> usize {
    with_framebuffer(|fb| fb.bytes_per_pixel()).unwrap_or(4)
}

use alloc::vec::Vec;
use alloc::string::String;
use spin::mutex::Mutex;

pub struct GraphicsTerminal {
    cursor_x: u32,
    cursor_y: u32,
    fg_color: Color,
    bg_color: Color,
    width: u32,
    height: u32,
    max_cols: u32,
    max_rows: u32,
    lines: Vec<String>,
    show_cursor: bool,
}

impl GraphicsTerminal {
    pub fn new(width: u32, height: u32, fg_color: Color, bg_color: Color) -> Self {
        let max_cols = width / FONT_WIDTH;
        let max_rows = height / FONT_HEIGHT;

        Self {
            cursor_x: 0,
            cursor_y: 0,
            fg_color,
            bg_color,
            width,
            height,
            max_cols,
            max_rows,
            lines: Vec::new(),
            show_cursor: true,
        }
    }

    pub fn cursor_pos(&self) -> (u32, u32) {
        (self.cursor_x, self.cursor_y)
    }

    pub fn set_cursor(&mut self, x: u32, y: u32) {
        self.cursor_x = x.min(self.max_cols - 1);
        self.cursor_y = y.min(self.max_rows - 1);
    }

    pub fn set_colors(&mut self, fg: Color, bg: Color) {
        self.fg_color = fg;
        self.bg_color = bg;
    }

    pub fn clear(&mut self) {
        with_framebuffer(|fb| {
            fb.clear(self.bg_color);
        });
        self.cursor_x = 0;
        self.cursor_y = 0;
        self.lines.clear();
    }

    pub fn put_char(&mut self, ch: u8) {
        match ch {
            b'\n' => {
                self.newline();
            }
            b'\r' => {
                self.cursor_x = 0;
            }
            0x08 => {
                if self.cursor_x > 0 {
                    self.cursor_x -= 1;
                    let pixel_x = self.cursor_x * FONT_WIDTH;
                    let pixel_y = self.cursor_y * FONT_HEIGHT;
                    with_framebuffer(|fb| {
                        fb.draw_char(pixel_x, pixel_y, b' ', self.fg_color, self.bg_color);
                    });
                }
            }
            ch if ch >= 0x20 && ch <= 0x7E => {
                let pixel_x = self.cursor_x * FONT_WIDTH;
                let pixel_y = self.cursor_y * FONT_HEIGHT;

                with_framebuffer(|fb| {
                    fb.draw_char(pixel_x, pixel_y, ch, self.fg_color, self.bg_color);
                });

                self.cursor_x += 1;
                if self.cursor_x >= self.max_cols {
                    self.newline();
                }
            }
            _ => {
            }
        }
    }

    pub fn write_str(&mut self, s: &str) {
        for byte in s.bytes() {
            self.put_char(byte);
        }
    }

    pub fn write_colored(&mut self, s: &str, fg: Color, bg: Color) {
        let old_fg = self.fg_color;
        let old_bg = self.bg_color;
        self.fg_color = fg;
        self.bg_color = bg;
        self.write_str(s);
        self.fg_color = old_fg;
        self.bg_color = old_bg;
    }

    fn newline(&mut self) {
        self.cursor_x = 0;
        self.cursor_y += 1;

        if self.cursor_y >= self.max_rows {
            self.scroll_up();
            self.cursor_y = self.max_rows - 1;
        }
    }

    fn scroll_up(&mut self) {
        with_framebuffer(|fb| {
            let line_height = FONT_HEIGHT as usize;
            let bytes_per_pixel = fb.bytes_per_pixel;
            let stride = fb.stride as usize;

            unsafe {
                for y in 1..self.max_rows as usize {
                    let src_y = y * line_height;
                    let dst_y = (y - 1) * line_height;

                    for line in 0..line_height {
                        let src_offset =
                            (src_y + line) * stride * bytes_per_pixel;
                        let dst_offset =
                            (dst_y + line) * stride * bytes_per_pixel;

                        let src = fb.address.add(src_offset);
                        let dst = fb.address.add(dst_offset);

                        core::ptr::copy(src, dst, stride * bytes_per_pixel);
                    }
                }
            }

            let last_line_y = (self.max_rows - 1) * FONT_HEIGHT;
            fb.fill_rect(0, last_line_y, fb.width, FONT_HEIGHT, self.bg_color);
        });
    }

    pub fn draw_cursor(&self) {
        if !self.show_cursor {
            return;
        }

        let pixel_x = self.cursor_x * FONT_WIDTH;
        let pixel_y = self.cursor_y * FONT_HEIGHT;

        with_framebuffer(|fb| {
            fb.draw_char(pixel_x, pixel_y, b'_', self.fg_color, self.bg_color);
        });
    }

    pub fn set_cursor_visible(&mut self, visible: bool) {
        self.show_cursor = visible;
    }
}

static GRAPHICS_TERMINAL: Mutex<Option<GraphicsTerminal>> = Mutex::new(None);

pub fn init_terminal() -> bool {
    if !is_initialized() {
        return false;
    }

    let (width, height) = match get_dimensions() {
        Some(dims) => dims,
        None => return false,
    };

    let mut lock = GRAPHICS_TERMINAL.lock();
    *lock = Some(GraphicsTerminal::new(
        width,
        height,
        Color::WHITE,
        Color::BLACK,
    ));

    true
}

pub fn with_terminal<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut GraphicsTerminal) -> R,
{
    let mut lock = GRAPHICS_TERMINAL.lock();
    if let Some(ref mut term) = *lock {
        Some(f(term))
    } else {
        None
    }
}

pub fn terminal_put_char(ch: u8) {
    with_terminal(|term| term.put_char(ch));
}

pub fn terminal_write_str(s: &str) {
    with_terminal(|term| term.write_str(s));
}

pub fn terminal_write_colored(s: &str, fg: Color, bg: Color) {
    with_terminal(|term| term.write_colored(s, fg, bg));
}

pub fn terminal_clear() {
    with_terminal(|term| term.clear());
}

pub fn terminal_set_colors(fg: Color, bg: Color) {
    with_terminal(|term| term.set_colors(fg, bg));
}

pub fn terminal_cursor_pos() -> Option<(u32, u32)> {
    with_terminal(|term| term.cursor_pos())
}

pub fn terminal_draw_cursor() {
    with_terminal(|term| term.draw_cursor());
}
