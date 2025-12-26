// Graphics Subsystem - Minimal Framebuffer Management
//
// This module provides minimal framebuffer management for the microkernel.
// All drawing, rendering, font handling, and UI logic belongs in userspace.
//
// The kernel's responsibilities are limited to:
// - Initializing and storing framebuffer parameters from UEFI
// - Providing framebuffer info to userspace via syscalls
// - Ensuring the framebuffer is mapped in the virtual address space
//
// Design principles:
// - Microkernel architecture: mechanism, not policy
// - No rendering code in kernel space
// - Userspace drivers handle all graphics rendering
// - Kernel only provides safe access to hardware framebuffer

#![allow(dead_code)]

use crate::boot::{FramebufferInfo, PixelFormat};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::mutex::Mutex;

static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);
static FRAMEBUFFER_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Framebuffer information structure
///
/// Contains all necessary information for userspace to access the framebuffer.
/// No drawing methods are included - those belong in userspace.
pub struct Framebuffer {
    address: usize,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: PixelFormat,
    bytes_per_pixel: usize,
}

unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    /// Create a new Framebuffer from UEFI boot information
    pub fn new(info: &FramebufferInfo) -> Self {
        let bytes_per_pixel = match info.pixel_format {
            PixelFormat::Rgb | PixelFormat::Bgr | PixelFormat::Bitmask => 4,
            _ => 4,
        };

        Self {
            address: info.address as usize,
            width: info.width,
            height: info.height,
            stride: info.pixels_per_scan_line,
            pixel_format: info.pixel_format,
            bytes_per_pixel,
        }
    }

    /// Get the framebuffer width in pixels
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the framebuffer height in pixels
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the framebuffer physical/virtual address
    pub fn address(&self) -> usize {
        self.address
    }

    /// Get the framebuffer stride (pixels per scanline)
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Get the bytes per pixel
    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    /// Get the pixel format
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Calculate the total framebuffer size in bytes
    pub fn size_bytes(&self) -> usize {
        (self.stride as usize) * (self.height as usize) * self.bytes_per_pixel
    }
}

/// Initialize the graphics subsystem with framebuffer info from UEFI
pub fn init(fb_info: &FramebufferInfo) {
    *FRAMEBUFFER.lock() = Some(Framebuffer::new(fb_info));
    FRAMEBUFFER_INITIALIZED.store(true, Ordering::SeqCst);

    crate::log_info!(
        "graphics",
        "Framebuffer initialized: {}x{} @ {:#X} (stride={}, bpp={})",
        fb_info.width,
        fb_info.height,
        fb_info.address,
        fb_info.pixels_per_scan_line,
        4
    );
}

/// Check if the framebuffer has been initialized
pub fn is_initialized() -> bool {
    FRAMEBUFFER_INITIALIZED.load(Ordering::SeqCst)
}

/// Execute a function with access to the framebuffer
///
/// Returns None if framebuffer is not initialized
pub fn with_framebuffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&Framebuffer) -> R,
{
    if !is_initialized() {
        return None;
    }

    let fb_lock = FRAMEBUFFER.lock();
    if let Some(ref fb) = *fb_lock {
        Some(f(fb))
    } else {
        None
    }
}

/// Get framebuffer dimensions
pub fn get_dimensions() -> Option<(u32, u32)> {
    with_framebuffer(|fb| (fb.width(), fb.height()))
}

/// Get the raw framebuffer address for userspace drivers
pub fn get_framebuffer_address() -> Option<usize> {
    with_framebuffer(|fb| fb.address())
}

/// Get complete framebuffer info: (address, width, height, stride, bytes_per_pixel)
pub fn get_framebuffer_info() -> Option<(usize, u32, u32, u32, usize)> {
    with_framebuffer(|fb| {
        (
            fb.address(),
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

/// Get framebuffer size in bytes
pub fn get_size_bytes() -> usize {
    with_framebuffer(|fb| fb.size_bytes()).unwrap_or(0)
}

// Note: All rendering functions (draw_pixel, fill_rect, draw_char, draw_string)
// have been removed from the kernel. These operations belong in userspace.
//
// The GraphicsTerminal has also been removed - terminal emulation is now
// handled by a dedicated userspace terminal application using libgui.
//
// For early boot debugging, use serial output instead of graphics rendering.
