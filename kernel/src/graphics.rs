// Kernel Graphics Subsystem
//
// This module is responsible for:
//
//   1. Storing and publishing the primary framebuffer obtained from UEFI GOP.
//   2. Driving the Bochs Graphics Adapter (BGA/VBE_DISPI) when available, to
//      allow dynamic resolution changes from userspace.
//   3. Exposing kernel video mode information to the syscall layer.
//   4. Providing minimal emergency output (panic screen, early diagnostics)
//      that bypasses all userspace services.
//
// ## Architecture (Layered)
//
//   ┌─────────────────────────────────────────────────┐
//   │  Userspace: display_settings app                │  ← Settings UI
//   ├─────────────────────────────────────────────────┤
//   │  Userspace: display driver (compositor)         │  ← Rendering
//   ├─────────────────────────────────────────────────┤
//   │  Kernel Syscall Layer (syscall/mod.rs)          │  ← Mode set/query
//   ├─────────────────────────────────────────────────┤
//   │  kernel/src/graphics.rs  ← THIS FILE            │  ← Video manager
//   │   ├── BGA driver (drivers/bga.rs)               │  ← HW driver
//   │   └── GOP framebuffer (boot-provided)           │  ← UEFI fallback
//   └─────────────────────────────────────────────────┘
//
// ## Video Backend Selection
//
//   The kernel supports two graphics backends:
//
//   - BGA (Bochs Graphics Adapter): Primary backend for QEMU.
//     Supports dynamic resolution changes, LFB access, 32 BPP.
//     Initialized by probing I/O port 0x01CE.
//
//   - UEFI GOP: Fallback backend when BGA is not available.
//     Resolution is fixed at boot time by UEFI firmware.
//     Still provides full rendering capability.
//
//   At runtime, `VideoBackend` tracks which backend is active.
//   The active backend determines which framebuffer address is authoritative.
//
// ## Double Buffering Design
//
//   The kernel does NOT maintain a full double-buffer in kernel memory
//   (that would waste precious kernel RAM for a buffer that userspace controls).
//
//   Instead, the double-buffer architecture works as follows:
//
//     Kernel side:  Manages LFB physical address and mode state.
//                   Exposes LFB info via syscalls.
//                   Does NOT perform blits; it only publishes addresses.
//
//     Userspace:    The display driver allocates its own backbuffer using
//                   shared memory or direct malloc.
//                   When a frame is ready, it blits backbuffer → LFB.
//                   The blit is a single memcpy (line-by-line for correct pitch).
//
//   This design follows the same principle as DRM/KMS in Linux: the kernel
//   manages mode state, userspace manages buffer content.
//
// ## Emergency Output
//
//   `panic_write()` draws text directly to the framebuffer without locking,
//   without double-buffering, and without any abstractions. It is designed
//   to work even after the system has entered an inconsistent state.
//
// ## Thread Safety
//
//   - FRAMEBUFFER and VIDEO_MODE_STATE are protected by spinlocks.
//   - Emergency output (panic_write) bypasses locks intentionally.
//   - BGA driver has its own internal spinlock (BGA_STATE in bga.rs).
//   - Mode switch must not be concurrent with rendering; the display driver
//     is responsible for quiescing rendering before requesting a mode switch.

#![allow(dead_code)]

use crate::boot::{FramebufferInfo, PixelFormat};
use crate::drivers::bga;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use spin::Mutex;

// ============================================================================
// Error Type
// ============================================================================

/// Typed errors returned by `set_video_mode`.
///
/// This enum provides a stable, matchable error type that syscall handlers
/// can map to errno codes without inspecting error strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoModeError {
    /// Resolution is zero, or width is not a multiple of 8.
    InvalidResolution,
    /// Bits-per-pixel value is not supported (only 32 BPP accepted).
    UnsupportedBpp,
    /// The requested mode exceeds the available framebuffer memory.
    ExceedsLfbSize,
    /// The BGA device is not present or not initialised.
    DeviceNotAvailable,
    /// The hardware did not accept the programmed mode on readback.
    HardwareRejected,
    /// The active graphics backend does not support dynamic mode changes.
    BackendUnsupported,
}

impl From<bga::BgaError> for VideoModeError {
    fn from(e: bga::BgaError) -> Self {
        match e {
            bga::BgaError::InvalidResolution => VideoModeError::InvalidResolution,
            bga::BgaError::WidthMisaligned => VideoModeError::InvalidResolution,
            bga::BgaError::UnsupportedBpp => VideoModeError::UnsupportedBpp,
            bga::BgaError::ExceedsLfbSize => VideoModeError::ExceedsLfbSize,
            bga::BgaError::NotAvailable => VideoModeError::DeviceNotAvailable,
            bga::BgaError::HardwareRejected => VideoModeError::HardwareRejected,
        }
    }
}

// ============================================================================
// Video Backend Enumeration
// ============================================================================

/// Which graphics hardware backend is currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoBackend {
    /// No graphics hardware available
    None,
    /// UEFI GOP framebuffer (resolution fixed at boot)
    UefiGop,
    /// Bochs Graphics Adapter via VBE DISPI (supports dynamic mode changes)
    Bga,
}

// ============================================================================
// Primary Framebuffer State (UEFI GOP)
// ============================================================================

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
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
    };
    pub const RED: Color = Color { r: 255, g: 0, b: 0 };

    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// Kernel-owned framebuffer state (from UEFI GOP)
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
        self.stride as usize * self.height as usize * self.bytes_per_pixel
    }
}

// ============================================================================
// Video Mode State Manager
// ============================================================================

/// Published video mode state (updated on every mode change).
/// Fields use atomic types to allow lock-free reads in performance-sensitive paths.
struct VideoModeState {
    backend: Mutex<VideoBackend>,
    lfb_phys: AtomicU32, // Physical address (lower 32 bits; BGA is always < 4 GiB in QEMU)
    lfb_phys_hi: AtomicU32, // High 32 bits (for future 64-bit BAR support)
    width: AtomicU32,
    height: AtomicU32,
    pitch: AtomicU32, // Bytes per scanline
    bytes_per_pixel: AtomicU32,
}

static VIDEO_MODE_STATE: VideoModeState = VideoModeState {
    backend: Mutex::new(VideoBackend::None),
    lfb_phys: AtomicU32::new(0),
    lfb_phys_hi: AtomicU32::new(0),
    width: AtomicU32::new(0),
    height: AtomicU32::new(0),
    pitch: AtomicU32::new(0),
    bytes_per_pixel: AtomicU32::new(0),
};

impl VideoModeState {
    fn set_from_framebuffer(&self, fb: &Framebuffer) {
        let phys = fb.address() as usize;
        self.lfb_phys
            .store((phys & 0xFFFF_FFFF) as u32, Ordering::Release);
        self.lfb_phys_hi
            .store((phys >> 32) as u32, Ordering::Release);
        self.width.store(fb.width(), Ordering::Release);
        self.height.store(fb.height(), Ordering::Release);
        self.pitch
            .store(fb.stride() * fb.bytes_per_pixel() as u32, Ordering::Release);
        self.bytes_per_pixel
            .store(fb.bytes_per_pixel() as u32, Ordering::Release);
        *self.backend.lock() = VideoBackend::UefiGop;
    }

    fn set_from_bga(&self, lfb_phys: usize, width: u16, height: u16, pitch: u32, bpp: u32) {
        self.lfb_phys
            .store((lfb_phys & 0xFFFF_FFFF) as u32, Ordering::Release);
        self.lfb_phys_hi
            .store((lfb_phys >> 32) as u32, Ordering::Release);
        self.width.store(width as u32, Ordering::Release);
        self.height.store(height as u32, Ordering::Release);
        self.pitch.store(pitch, Ordering::Release);
        self.bytes_per_pixel.store(bpp, Ordering::Release);
        *self.backend.lock() = VideoBackend::Bga;
    }

    fn get_lfb_phys(&self) -> usize {
        let lo = self.lfb_phys.load(Ordering::Acquire) as usize;
        let hi = self.lfb_phys_hi.load(Ordering::Acquire) as usize;
        lo | (hi << 32)
    }
}

// ============================================================================
// Initialization
// ============================================================================

/// Initialize the graphics subsystem from UEFI GOP framebuffer info.
///
/// Must be called before `init_bga()`. The GOP framebuffer serves as the
/// initial display until the BGA driver (if available) sets a proper mode.
pub fn init(fb_info: &FramebufferInfo) {
    let fb = Framebuffer::new(fb_info);
    VIDEO_MODE_STATE.set_from_framebuffer(&fb);
    *FRAMEBUFFER.lock() = Some(fb);
    FRAMEBUFFER_INITIALIZED.store(true, Ordering::SeqCst);
    crate::log_info!(
        "graphics",
        "GOP framebuffer: {}x{} at 0x{:X}",
        fb_info.width,
        fb_info.height,
        fb_info.address
    );
}

/// Initialize the BGA driver.
///
/// Attempts to probe and initialize the Bochs Graphics Adapter. If successful,
/// sets an initial video mode matching the GOP resolution to preserve the
/// current display content. If BGA is not available, GOP remains the backend.
///
/// This should be called after `init()` and after the VMM is fully initialized
/// (because BGA init maps the LFB into the kernel address space).
pub fn init_bga() {
    crate::log_info!("graphics", "Attempting BGA driver initialization...");

    if !bga::init() {
        crate::log_info!("graphics", "BGA not available; using UEFI GOP only");
        return;
    }

    // Try to match the current GOP resolution so there's no visual disruption.
    let (gop_w, gop_h) = get_dimensions().unwrap_or((1024, 768));

    // Round down to BGA alignment (width must be divisible by 8)
    let bga_w = (gop_w as u16) & !7u16;
    let bga_h = gop_h as u16;

    match bga::set_video_mode(bga_w, bga_h, 32, false) {
        Ok(()) => {
            if let Some((lfb_phys, w, h, pitch, bpp)) = bga::get_current_mode_info() {
                VIDEO_MODE_STATE.set_from_bga(lfb_phys, w, h, pitch, bpp);
                crate::log_info!(
                    "graphics",
                    "BGA active: {}x{}x32 at LFB 0x{:X}",
                    w,
                    h,
                    lfb_phys
                );
            }
        }
        Err(e) => {
            crate::log_warn!("graphics", "BGA mode set failed ({:?}); keeping GOP", e);
        }
    }
}

pub fn is_initialized() -> bool {
    FRAMEBUFFER_INITIALIZED.load(Ordering::SeqCst)
}

// ============================================================================
// Video Mode Management (Syscall Interface)
// ============================================================================

/// Attempt to set a new video mode. Only BGA backend supports mode changes.
///
/// Returns `Ok(())` on success. Updates VIDEO_MODE_STATE atomically.
/// Returns `Err(VideoModeError)` on failure; callers can match on the variant
/// to determine the appropriate errno code without string matching.
pub fn set_video_mode(width: u16, height: u16, bpp: u8) -> Result<(), VideoModeError> {
    let backend = *VIDEO_MODE_STATE.backend.lock();

    match backend {
        VideoBackend::Bga => {
            bga::set_video_mode(width, height, bpp, true).map_err(VideoModeError::from)?;

            if let Some((lfb_phys, w, h, pitch, bpp_bytes)) = bga::get_current_mode_info() {
                VIDEO_MODE_STATE.set_from_bga(lfb_phys, w, h, pitch, bpp_bytes);
            }
            Ok(())
        }
        VideoBackend::UefiGop | VideoBackend::None => Err(VideoModeError::BackendUnsupported),
    }
}

/// Get all available video modes (delegates to BGA mode table).
/// Returns the number of modes written into `buf`.
pub fn get_available_modes(buf: &mut [bga::VideoMode]) -> usize {
    bga::get_supported_modes(buf)
}

/// Get the total count of available video modes.
pub fn available_mode_count() -> usize {
    bga::mode_count()
}

/// Find the best-fitting mode for the given resolution and BPP.
pub fn find_best_mode(width: u16, height: u16, bpp: u8) -> Option<&'static bga::VideoMode> {
    bga::find_best_mode(width, height, bpp)
}

/// Get the current video backend.
pub fn get_backend() -> VideoBackend {
    *VIDEO_MODE_STATE.backend.lock()
}

// ============================================================================
// Current State Accessors (used by syscalls)
// ============================================================================

/// Get current framebuffer dimensions.
pub fn get_dimensions() -> Option<(u32, u32)> {
    if !is_initialized() {
        return None;
    }
    let w = VIDEO_MODE_STATE.width.load(Ordering::Relaxed);
    let h = VIDEO_MODE_STATE.height.load(Ordering::Relaxed);
    if w == 0 || h == 0 {
        return None;
    }
    Some((w, h))
}

/// Get the authoritative framebuffer address.
///
/// When BGA is active, this returns the LFB physical address (which is identity-
/// mapped and user-accessible). When only GOP is available, this returns the
/// GOP framebuffer address provided by UEFI.
pub fn get_framebuffer_address() -> Option<usize> {
    if !is_initialized() {
        return None;
    }
    let phys = VIDEO_MODE_STATE.get_lfb_phys();
    if phys == 0 {
        // Fallback: read from GOP framebuffer struct
        with_framebuffer(|fb| fb.address() as usize)
    } else {
        Some(phys)
    }
}

/// Get full framebuffer info tuple: (address, width, height, stride, bytes_per_pixel).
pub fn get_framebuffer_info() -> Option<(usize, u32, u32, u32, usize)> {
    if !is_initialized() {
        return None;
    }

    let backend = *VIDEO_MODE_STATE.backend.lock();

    match backend {
        VideoBackend::Bga => {
            let addr = VIDEO_MODE_STATE.get_lfb_phys();
            let w = VIDEO_MODE_STATE.width.load(Ordering::Relaxed);
            let h = VIDEO_MODE_STATE.height.load(Ordering::Relaxed);
            let pitch = VIDEO_MODE_STATE.pitch.load(Ordering::Relaxed);
            let bpp = VIDEO_MODE_STATE.bytes_per_pixel.load(Ordering::Relaxed) as usize;
            // stride (pixels per scanline) = pitch / bpp
            let stride = if bpp > 0 { pitch / bpp as u32 } else { w };
            Some((addr, w, h, stride, bpp))
        }
        VideoBackend::UefiGop | VideoBackend::None => with_framebuffer(|fb| {
            (
                fb.address() as usize,
                fb.width(),
                fb.height(),
                fb.stride(),
                fb.bytes_per_pixel(),
            )
        }),
    }
}

/// Get the framebuffer stride (pixels per scanline)
pub fn get_stride() -> u32 {
    VIDEO_MODE_STATE.pitch.load(Ordering::Relaxed)
        / VIDEO_MODE_STATE
            .bytes_per_pixel
            .load(Ordering::Relaxed)
            .max(1)
}

/// Get bytes per pixel
pub fn get_bytes_per_pixel() -> usize {
    VIDEO_MODE_STATE.bytes_per_pixel.load(Ordering::Relaxed) as usize
}

/// Get KernelVideoInfo struct for SYS_GET_CURRENT_VIDEO_MODE.
pub fn get_kernel_video_info() -> bga::KernelVideoInfo {
    bga::KernelVideoInfo::from_state()
}

// ============================================================================
// Internal Helpers
// ============================================================================

pub fn with_framebuffer<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut Framebuffer) -> R,
{
    if !is_initialized() {
        return None;
    }
    let mut fb_lock = FRAMEBUFFER.lock();
    (*fb_lock).as_mut().map(f)
}

// ============================================================================
// Emergency Boot/Panic Output
// ============================================================================
//
// These functions bypass all abstractions to draw directly to the framebuffer.
// They are designed to work even after the system has entered an inconsistent
// state (e.g., deadlocked, panicking, or with corrupted state).
//
// Safety: Uses raw pointer writes. Intentionally does not acquire any locks.

const FONT_HEIGHT: u32 = 8;
const FONT_WIDTH: u32 = 8;

/// Minimal 8x8 bitmap font for emergency output (panic screen, early diagnostics).
fn get_minimal_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'!' => [0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'2' => [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00],
        b'3' => [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00],
        b'4' => [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00],
        b'5' => [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00],
        b'6' => [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00],
        b'7' => [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00],
        b'8' => [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00],
        b'9' => [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00],
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
        b'D' => [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00],
        b'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'J' => [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00],
        b'K' => [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'M' => [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00],
        b'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'Q' => [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'W' => [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00],
        b'X' => [0x63, 0x36, 0x1C, 0x1C, 0x1C, 0x36, 0x63, 0x00],
        b'Y' => [0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00],
        b'Z' => [0x7F, 0x63, 0x30, 0x18, 0x0C, 0x63, 0x7F, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

fn get_lowercase_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b'a' => [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00],
        b'b' => [0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x7C, 0x00],
        b'c' => [0x00, 0x00, 0x3C, 0x60, 0x60, 0x60, 0x3C, 0x00],
        b'd' => [0x06, 0x06, 0x3E, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'e' => [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00],
        b'f' => [0x1C, 0x36, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x00],
        b'g' => [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'h' => [0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        b'i' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'j' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x1B, 0x0E],
        b'k' => [0x60, 0x60, 0x66, 0x6C, 0x78, 0x6C, 0x66, 0x00],
        b'l' => [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'm' => [0x00, 0x00, 0x66, 0x7F, 0x7F, 0x6B, 0x63, 0x00],
        b'n' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        b'o' => [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'p' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60],
        b'q' => [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x06],
        b'r' => [0x00, 0x00, 0x6E, 0x70, 0x60, 0x60, 0x60, 0x00],
        b's' => [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00],
        b't' => [0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x1C, 0x00],
        b'u' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'v' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'w' => [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00],
        b'x' => [0x00, 0x00, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x00],
        b'y' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'z' => [0x00, 0x00, 0x7E, 0x18, 0x0C, 0x06, 0x7E, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

/// Emergency panic output - draws text directly to the framebuffer.
/// Bypasses all locking and abstractions.
pub fn panic_write(x: u32, y: u32, text: &str, color: Color) {
    with_framebuffer(|fb| {
        let pixel_value = match fb.pixel_format {
            PixelFormat::Rgb => {
                ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32)
            }
            _ => ((color.b as u32) << 16) | ((color.g as u32) << 8) | (color.r as u32),
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
// Compatibility Stubs
// ============================================================================

pub fn init_terminal() -> bool {
    true
}
pub fn terminal_write_str(_s: &str) {}
pub fn terminal_put_char(_ch: u8) {}
pub fn terminal_clear() {}
