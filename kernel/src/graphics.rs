// Minimal Graphics Subsystem - Framebuffer Exposure
//
// This module provides minimal kernel-side framebuffer management following
// microkernel principles. The kernel only:
// - Initializes and stores framebuffer parameters from UEFI
// - Exposes framebuffer address and dimensions via syscalls
// - Provides emergency boot/panic output (minimal, for kernel diagnostics only)
//
// All rendering, compositing, windowing, and UI logic is handled in userspace:
// - Display driver manages framebuffer access and compositing
// - Desktop environment handles window management and input routing
// - Applications render through libgui abstractions
//
// Syscall interface:
// - SYS_GET_FRAMEBUFFER: Get framebuffer info (address, width, height, stride, bpp)
// - SYS_MAP_FRAMEBUFFER: Map framebuffer to userspace address space

#![allow(dead_code)]

use crate::boot::{FramebufferInfo, PixelFormat};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);
static FRAMEBUFFER_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Minimal color representation for early boot diagnostics
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

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Kernel-owned framebuffer state
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

    /// Calculate framebuffer size in bytes
    pub fn size(&self) -> usize {
        (self.stride as usize) * (self.height as usize) * self.bytes_per_pixel
    }
}

// ============================================================================
// Kernel-Only Early Boot/Panic Output (Minimal)
// ============================================================================

/// Minimal 8x8 font for early boot messages (panic output only)
/// Only includes essential ASCII characters
const FONT_HEIGHT: u32 = 8;
const FONT_WIDTH: u32 = 8;

fn get_minimal_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'!' => [0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'A'..=b'Z' => get_uppercase_glyph(ch),
        b'a'..=b'z' => get_lowercase_glyph(ch),
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

fn get_uppercase_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b'A' => [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        b'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

fn get_lowercase_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b'a' => [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00],
        b'c' => [0x00, 0x00, 0x3C, 0x60, 0x60, 0x60, 0x3C, 0x00],
        b'e' => [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00],
        b'i' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'k' => [0x60, 0x60, 0x66, 0x6C, 0x78, 0x6C, 0x66, 0x00],
        b'l' => [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'n' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        b'o' => [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'p' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60],
        b'r' => [0x00, 0x00, 0x6E, 0x70, 0x60, 0x60, 0x60, 0x00],
        b's' => [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00],
        b't' => [0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x1C, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

/// Emergency panic output - kernel-only, minimal implementation
pub fn panic_write(x: u32, y: u32, text: &str, color: Color) {
    with_framebuffer(|fb| {
        let pixel_value = match fb.pixel_format {
            PixelFormat::Rgb => {
                ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32)
            }
            _ => {
                ((color.b as u32) << 16) | ((color.g as u32) << 8) | (color.r as u32)
            }
        };

        let mut cx = x;
        for ch in text.bytes() {
            if cx + FONT_WIDTH > fb.width {
                break;
            }
            let glyph = get_minimal_glyph(ch);
            for row in 0..FONT_HEIGHT {
                for col in 0..FONT_WIDTH {
                    if glyph[row as usize] & (0x80 >> col) != 0 {
                        let px = cx + col;
                        let py = y + row;
                        if px < fb.width && py < fb.height {
                            let offset = (py * fb.stride + px) as usize * fb.bytes_per_pixel;
                            unsafe {
                                let ptr = fb.address.add(offset) as *mut u32;
                                ptr.write_volatile(pixel_value);
                            }
                        }
                    }
                }
            }
            cx += FONT_WIDTH;
        }
    });
}

// ============================================================================
// Public API for Kernel and Syscalls
// ============================================================================

pub fn init(fb_info: &FramebufferInfo) {
    *FRAMEBUFFER.lock() = Some(Framebuffer::new(fb_info));
    FRAMEBUFFER_INITIALIZED.store(true, Ordering::SeqCst);
    crate::log_info!("graphics", "Framebuffer initialized: {}x{}", fb_info.width, fb_info.height);
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

pub fn get_dimensions() -> Option<(u32, u32)> {
    with_framebuffer(|fb| (fb.width(), fb.height()))
}

/// Get the raw framebuffer address for userspace drivers
pub fn get_framebuffer_address() -> Option<usize> {
    with_framebuffer(|fb| fb.address() as usize)
}

/// Get complete framebuffer info: (address, width, height, stride, bytes_per_pixel)
pub fn get_framebuffer_info() -> Option<(usize, u32, u32, u32, usize)> {
    with_framebuffer(|fb| {
        (
            fb.address() as usize,
            fb.width(),
            fb.height(),
            fb.stride(),
            fb.bytes_per_pixel(),
        )
    })
}

/// Get the framebuffer stride (pixels per scanline)
pub fn get_stride() -> u32 {
    with_framebuffer(|fb| fb.stride()).unwrap_or(0)
}

/// Get bytes per pixel
pub fn get_bytes_per_pixel() -> usize {
    with_framebuffer(|fb| fb.bytes_per_pixel()).unwrap_or(4)
}

// ============================================================================
// Removed from kernel (now in userspace):
// - GraphicsTerminal and all terminal state
// - Full 96-glyph font data
// - draw_pixel, fill_rect, draw_string, draw_char
// - Scrolling and cursor management
// - Color theme definitions
// ============================================================================

// Stub functions for compatibility during transition
// These will be removed once all kernel code stops using them

pub fn init_terminal() -> bool {
    // Terminal initialization is now a no-op in the kernel
    // Terminal runs entirely in userspace
    true
}

pub fn terminal_write_str(_s: &str) {
    // No-op: Terminal output goes through userspace
}

pub fn terminal_put_char(_ch: u8) {
    // No-op: Terminal output goes through userspace
}

pub fn terminal_clear() {
    // No-op: Terminal is userspace only
}

pub fn clear_screen(_color: Color) {
    // No-op: Screen clearing is userspace only
}

pub fn draw_string(_x: u32, _y: u32, _text: &str, _fg: Color, _bg: Color) {
    // No-op: Drawing is userspace only
}
