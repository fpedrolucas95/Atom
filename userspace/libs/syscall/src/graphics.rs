// Framebuffer and graphics syscalls

use crate::error::{ESUCCESS, EPERM, EINVAL, ENOMEM, EBUSY, ENOTSUP, SyscallError, SyscallResult};
use crate::raw::{syscall0, syscall1, syscall2, syscall3, numbers::*};

// ============================================================================
// Framebuffer Information
// ============================================================================

/// Framebuffer information returned by syscall
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FramebufferInfo {
    pub address: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes_per_pixel: u32,
    pub size: usize,
}

impl FramebufferInfo {
    /// Calculate the byte offset for a pixel at (x, y)
    #[inline]
    pub fn pixel_offset(&self, x: u32, y: u32) -> usize {
        (y * self.stride + x) as usize * self.bytes_per_pixel as usize
    }

    /// Get pointer to pixel at (x, y)
    #[inline]
    pub fn pixel_ptr(&self, x: u32, y: u32) -> *mut u32 {
        let offset = self.pixel_offset(x, y);
        (self.address + offset) as *mut u32
    }
}

/// Get framebuffer information for direct graphics access
///
/// Returns Some(FramebufferInfo) on success, None if framebuffer is not available
/// or the process doesn't have permission to access it.
#[inline(never)]
pub fn get_framebuffer() -> Option<FramebufferInfo> {
    // DEBUG: Try just the syscall without any processing
    let mut info = [0u64; 6];
    let result = unsafe {
        syscall1(SYS_GET_FRAMEBUFFER, info.as_mut_ptr() as u64)
    };

    if result == ESUCCESS {
        Some(FramebufferInfo {
            address: info[0] as usize,
            width: info[1] as u32,
            height: info[2] as u32,
            stride: info[3] as u32,
            bytes_per_pixel: info[4] as u32,
            size: (info[3] as usize) * (info[2] as usize) * (info[4] as usize),
        })
    } else {
        None
    }
}

/// Get framebuffer info as tuple (address, width, height, stride, bpp)
///
/// Returns an error if framebuffer is not available
pub fn get_framebuffer_info() -> crate::SyscallResult<(usize, u32, u32, u32, u32)> {
    match get_framebuffer() {
        Some(info) => Ok((info.address, info.width, info.height, info.stride, info.bytes_per_pixel)),
        None => Err(crate::error::SyscallError::PermissionDenied),
    }
}

/// Map framebuffer into process address space
///
/// Similar to get_framebuffer but may also perform memory mapping.
pub fn map_framebuffer() -> Option<FramebufferInfo> {
    let mut info = [0u64; 6];
    let result = unsafe {
        syscall1(SYS_MAP_FRAMEBUFFER, info.as_mut_ptr() as u64)
    };

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

// ============================================================================
// Color Types
// ============================================================================

/// RGB color representation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    // Common colors
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

    /// Create a new color from RGB values
    #[inline]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Convert to BGR32 pixel value (common framebuffer format)
    #[inline]
    pub fn to_bgr32(&self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    /// Convert to RGB32 pixel value
    #[inline]
    pub fn to_rgb32(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

// ============================================================================
// Framebuffer Handle
// ============================================================================

/// Framebuffer handle for drawing operations
pub struct Framebuffer {
    info: FramebufferInfo,
}

impl Framebuffer {
    /// Create a new framebuffer handle
    pub fn new() -> Option<Self> {
        // Explicit match to avoid closure that could cause indirect calls
        match get_framebuffer() {
            Some(info) => Some(Self { info }),
            None => None,
        }
    }

    /// Create a custom framebuffer pointing to a specific memory location.
    /// Includes validation to prevent common memory safety issues.
    pub fn new_custom(address: usize, width: u32, height: u32, stride: u32, bpp: u32) -> Option<Self> {
        if address == 0 || width == 0 || height == 0 || stride < width || (bpp != 4 && bpp != 3) {
            return None;
        }

        // Check for potential overflow in size calculation
        let size = (stride as usize).checked_mul(height as usize)?
            .checked_mul(bpp as usize)?;

        Some(Self {
            info: FramebufferInfo {
                address,
                width,
                height,
                stride,
                bytes_per_pixel: bpp,
                size,
            }
        })
    }

    /// Create from mapped framebuffer
    pub fn from_mapped() -> Option<Self> {
        match map_framebuffer() {
            Some(info) => Some(Self { info }),
            None => None,
        }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.info.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.info.height
    }

    #[inline]
    pub fn stride(&self) -> u32 {
        self.info.stride
    }

    #[inline]
    pub fn address(&self) -> usize {
        self.info.address
    }

    #[inline]
    pub fn bytes_per_pixel(&self) -> usize {
        self.info.bytes_per_pixel as usize
    }

    /// Draw a single pixel (bounds checked)
    #[inline]
    pub fn draw_pixel(&self, x: u32, y: u32, color: Color) {
        if x >= self.info.width || y >= self.info.height {
            return;
        }

        let ptr = self.info.pixel_ptr(x, y);
        unsafe {
            core::ptr::write_volatile(ptr, color.to_bgr32());
        }
    }

    /// Fill a rectangle
    pub fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let pixel = color.to_bgr32();
        
        for dy in 0..height {
            let py = y + dy;
            if py >= self.info.height {
                break;
            }
            
            for dx in 0..width {
                let px = x + dx;
                if px >= self.info.width {
                    break;
                }
                
                let ptr = self.info.pixel_ptr(px, py);
                unsafe {
                    core::ptr::write_volatile(ptr, pixel);
                }
            }
        }
    }

    /// Clear the entire screen
    pub fn clear(&self, color: Color) {
        self.fill_rect(0, 0, self.info.width, self.info.height, color);
    }

    /// Draw a character using built-in 8x8 font
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

    /// Draw a horizontal line
    pub fn draw_hline(&self, x: u32, y: u32, width: u32, color: Color) {
        self.fill_rect(x, y, width, 1, color);
    }

    /// Draw a vertical line
    pub fn draw_vline(&self, x: u32, y: u32, height: u32, color: Color) {
        self.fill_rect(x, y, 1, height, color);
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.draw_hline(x, y, width, color);
        self.draw_hline(x, y + height - 1, width, color);
        self.draw_vline(x, y, height, color);
        self.draw_vline(x + width - 1, y, height, color);
    }

    /// Fill a rectangle with rounded corners (radius in pixels)
    pub fn fill_rect_rounded(&self, x: u32, y: u32, width: u32, height: u32, radius: u32, color: Color) {
        if width == 0 || height == 0 { return; }
        let r = radius.min(width / 2).min(height / 2);
        if r == 0 {
            self.fill_rect(x, y, width, height, color);
            return;
        }
        // Main body (excluding top and bottom rounded strips)
        self.fill_rect(x, y + r, width, height.saturating_sub(r * 2), color);
        // Top and bottom rounded strips
        for dy in 0..r {
            let fy = r - dy;
            // Approximate circle: offset = r - sqrt(r*r - fy*fy)
            let offset = r - isqrt(r * r - fy * fy);
            let row_x = x + offset;
            let row_w = width.saturating_sub(offset * 2);
            if row_w > 0 {
                self.fill_rect(row_x, y + dy, row_w, 1, color);
                self.fill_rect(row_x, y + height - 1 - dy, row_w, 1, color);
            }
        }
    }

    /// Draw a rounded rectangle outline
    pub fn draw_rect_rounded(&self, x: u32, y: u32, width: u32, height: u32, radius: u32, color: Color) {
        if width == 0 || height == 0 { return; }
        let r = radius.min(width / 2).min(height / 2);
        if r == 0 {
            self.draw_rect(x, y, width, height, color);
            return;
        }
        // Straight edges
        self.draw_hline(x + r, y, width - r * 2, color);
        self.draw_hline(x + r, y + height - 1, width - r * 2, color);
        self.draw_vline(x, y + r, height - r * 2, color);
        self.draw_vline(x + width - 1, y + r, height - r * 2, color);
        // Corner arcs
        for i in 0..r {
            let fy = r - i;
            let offset = r - isqrt(r * r - fy * fy);
            // Top-left
            self.draw_pixel(x + offset, y + i, color);
            // Top-right
            self.draw_pixel(x + width - 1 - offset, y + i, color);
            // Bottom-left
            self.draw_pixel(x + offset, y + height - 1 - i, color);
            // Bottom-right
            self.draw_pixel(x + width - 1 - offset, y + height - 1 - i, color);
        }
    }

    /// Fill a rectangle with alpha blending (alpha 0-255, 255=opaque)
    pub fn fill_rect_alpha(&self, x: u32, y: u32, width: u32, height: u32, color: Color, alpha: u8) {
        if alpha == 255 {
            self.fill_rect(x, y, width, height, color);
            return;
        }
        if alpha == 0 { return; }
        let a = alpha as u32;
        let inv_a = 255 - a;
        for dy in 0..height {
            let py = y + dy;
            if py >= self.info.height { break; }
            for dx in 0..width {
                let px = x + dx;
                if px >= self.info.width { break; }
                let ptr = self.info.pixel_ptr(px, py);
                unsafe {
                    let existing = core::ptr::read_volatile(ptr);
                    let er = existing & 0xFF;
                    let eg = (existing >> 8) & 0xFF;
                    let eb = (existing >> 16) & 0xFF;
                    let nr = (er * inv_a + color.r as u32 * a) / 255;
                    let ng = (eg * inv_a + color.g as u32 * a) / 255;
                    let nb = (eb * inv_a + color.b as u32 * a) / 255;
                    let blended = (nb << 16) | (ng << 8) | nr;
                    core::ptr::write_volatile(ptr, blended);
                }
            }
        }
    }

    /// Fill a rounded rectangle with alpha blending
    pub fn fill_rect_rounded_alpha(&self, x: u32, y: u32, width: u32, height: u32, radius: u32, color: Color, alpha: u8) {
        if width == 0 || height == 0 { return; }
        let r = radius.min(width / 2).min(height / 2);
        if r == 0 {
            self.fill_rect_alpha(x, y, width, height, color, alpha);
            return;
        }
        self.fill_rect_alpha(x, y + r, width, height.saturating_sub(r * 2), color, alpha);
        for dy in 0..r {
            let fy = r - dy;
            let offset = r - isqrt(r * r - fy * fy);
            let row_x = x + offset;
            let row_w = width.saturating_sub(offset * 2);
            if row_w > 0 {
                self.fill_rect_alpha(row_x, y + dy, row_w, 1, color, alpha);
                self.fill_rect_alpha(row_x, y + height - 1 - dy, row_w, 1, color, alpha);
            }
        }
    }

    /// Blit the contents of this framebuffer to another framebuffer.
    /// This is an optimized line-by-line copy.
    pub fn blit(&self, other: &Framebuffer) {
        let width = self.width().min(other.width());
        let height = self.height().min(other.height());
        let bpp = self.bytes_per_pixel();

        if bpp != other.bytes_per_pixel() {
            // Fallback to pixel-by-pixel if formats differ (rare for compositor)
            for y in 0..height {
                for x in 0..width {
                    // This is slow, but correct for different BPPs
                    // However, for compositor we expect 4 BPP for both.
                    let ptr = self.info.pixel_ptr(x, y);
                    let color_val = unsafe { core::ptr::read_volatile(ptr) };
                    let other_ptr = other.info.pixel_ptr(x, y);
                    unsafe { core::ptr::write_volatile(other_ptr, color_val) };
                }
            }
            return;
        }

        // Optimized line-by-line copy
        for y in 0..height {
            let src_offset = (y * self.stride()) as usize * bpp;
            let dst_offset = (y * other.stride()) as usize * bpp;

            let src_ptr = (self.address() + src_offset) as *const u8;
            let dst_ptr = (other.address() + dst_offset) as *mut u8;

            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, width as usize * bpp);
            }
        }
    }
}

// ============================================================================
// Built-in 8x8 Font
// ============================================================================

/// Get font glyph for character (8x8 bitmap)
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

// ============================================================================
// Helper: integer square root
// ============================================================================

fn isqrt(n: u32) -> u32 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

// ============================================================================
// Shared Memory Surface Support
// ============================================================================

/// Shared memory region identifier
pub type SharedRegionId = u64;

/// Flags for shared memory mapping
#[derive(Debug, Clone, Copy)]
pub struct SharedMemFlags {
    pub read: bool,
    pub write: bool,
}

impl SharedMemFlags {
    pub const READ_ONLY: Self = Self { read: true, write: false };
    pub const READ_WRITE: Self = Self { read: true, write: true };

    pub fn to_raw(&self) -> u64 {
        let mut raw = 0u64;
        if self.read { raw |= 0x1; }
        if self.write { raw |= 0x2; }
        raw
    }
}

/// Create a shared memory region
pub fn shared_region_create(size: usize) -> SyscallResult<SharedRegionId> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_CREATE, size as u64) };

    if crate::error::is_syscall_error(result) {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        Ok(result)
    }
}

/// Map a shared memory region into the current process address space.
///
/// If `virt_addr == 0`, the kernel automatically selects a free VA.
/// Returns the virtual address where the region was actually mapped.
pub fn shared_region_map(region_id: SharedRegionId, virt_addr: usize, flags: SharedMemFlags) -> SyscallResult<usize> {
    let result = unsafe {
        syscall3(SYS_SHARED_REGION_MAP, region_id, virt_addr as u64, flags.to_raw())
    };

    // Error codes live near u64::MAX; valid VA addresses are always below
    // SYSCALL_ERROR_THRESHOLD.
    if crate::error::is_syscall_error(result) {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            x if x == EBUSY => Err(SyscallError::ResourceBusy),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        Ok(result as usize)
    }
}

/// Unmap a shared memory region from the current process
pub fn shared_region_unmap(region_id: SharedRegionId) -> SyscallResult<()> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_UNMAP, region_id) };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

/// Destroy a shared memory region (only owner can do this)
pub fn shared_region_destroy(region_id: SharedRegionId) -> SyscallResult<()> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_DESTROY, region_id) };

    if result == ESUCCESS {
        Ok(())
    } else {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == EPERM => Err(SyscallError::PermissionDenied),
            x if x == EBUSY => Err(SyscallError::ResourceBusy),
            _ => Err(SyscallError::Unknown(result)),
        }
    }
}

/// A shared surface that can be passed between processes for rendering
///
/// This represents a render target that is backed by shared memory,
/// allowing the compositor to composite application content into windows.
pub struct SharedSurface {
    /// Shared memory region ID
    region_id: SharedRegionId,
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// Stride (bytes per row)
    stride: u32,
    /// Bytes per pixel (usually 4 for BGRA)
    bytes_per_pixel: u32,
    /// Mapped virtual address (if mapped)
    mapped_addr: Option<usize>,
    /// Whether this process owns the region
    owned: bool,
}

impl SharedSurface {
    /// Create a new shared surface owned by this process.
    /// The kernel automatically assigns a free virtual address.
    pub fn create(width: u32, height: u32) -> SyscallResult<Self> {
        let bytes_per_pixel = 4u32; // BGRA format
        let stride = width;
        let size = (stride * height * bytes_per_pixel) as usize;

        let region_id = shared_region_create(size)?;

        // Map into our address space — let kernel pick a free VA (virt_addr = 0)
        let map_addr = shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE)?;

        let surface = Self {
            region_id,
            width,
            height,
            stride,
            bytes_per_pixel,
            mapped_addr: Some(map_addr),
            owned: true,
        };

        // Clear the surface to black
        surface.clear(Color::BLACK);

        Ok(surface)
    }

    /// Create a surface from an existing shared region (for client processes).
    /// The kernel automatically assigns a free virtual address.
    pub fn from_region(region_id: SharedRegionId, width: u32, height: u32) -> SyscallResult<Self> {
        let bytes_per_pixel = 4u32;
        let stride = width;

        // Map into our address space — let kernel pick a free VA (virt_addr = 0)
        let map_addr = shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE)?;

        Ok(Self {
            region_id,
            width,
            height,
            stride,
            bytes_per_pixel,
            mapped_addr: Some(map_addr),
            owned: false,
        })
    }

    /// Get the shared region ID for passing to other processes
    pub fn region_id(&self) -> SharedRegionId {
        self.region_id
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[inline]
    pub fn stride(&self) -> u32 {
        self.stride
    }

    #[inline]
    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel as usize
    }

    /// Get the mapped address (if mapped)
    pub fn address(&self) -> Option<usize> {
        self.mapped_addr
    }

    /// Calculate pixel offset
    #[inline]
    fn pixel_offset(&self, x: u32, y: u32) -> usize {
        (y * self.stride + x) as usize * self.bytes_per_pixel as usize
    }

    /// Get pointer to pixel at (x, y)
    #[inline]
    fn pixel_ptr(&self, x: u32, y: u32) -> Option<*mut u32> {
        self.mapped_addr.map(|addr| {
            let offset = self.pixel_offset(x, y);
            (addr + offset) as *mut u32
        })
    }

    /// Draw a single pixel (bounds checked)
    #[inline]
    pub fn draw_pixel(&self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        if let Some(ptr) = self.pixel_ptr(x, y) {
            unsafe {
                core::ptr::write_volatile(ptr, color.to_bgr32());
            }
        }
    }

    /// Fill a rectangle
    pub fn fill_rect(&self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let pixel = color.to_bgr32();

        for dy in 0..height {
            let py = y + dy;
            if py >= self.height {
                break;
            }

            for dx in 0..width {
                let px = x + dx;
                if px >= self.width {
                    break;
                }

                if let Some(ptr) = self.pixel_ptr(px, py) {
                    unsafe {
                        core::ptr::write_volatile(ptr, pixel);
                    }
                }
            }
        }
    }

    /// Clear the entire surface
    pub fn clear(&self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Draw a character using built-in 8x8 font
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
            if offset_x + 8 > self.width {
                break;
            }
            self.draw_char(offset_x, y, byte, fg, bg);
            offset_x += 8;
        }
    }

    /// Draw a horizontal line
    pub fn draw_hline(&self, x: u32, y: u32, width: u32, color: Color) {
        self.fill_rect(x, y, width, 1, color);
    }

    /// Draw a vertical line
    pub fn draw_vline(&self, x: u32, y: u32, height: u32, color: Color) {
        self.fill_rect(x, y, 1, height, color);
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.draw_hline(x, y, width, color);
        self.draw_hline(x, y + height.saturating_sub(1), width, color);
        self.draw_vline(x, y, height, color);
        self.draw_vline(x + width.saturating_sub(1), y, height, color);
    }

    /// Fill a rectangle with rounded corners
    pub fn fill_rect_rounded(&self, x: u32, y: u32, width: u32, height: u32, radius: u32, color: Color) {
        if width == 0 || height == 0 { return; }
        let r = radius.min(width / 2).min(height / 2);
        if r == 0 {
            self.fill_rect(x, y, width, height, color);
            return;
        }
        self.fill_rect(x, y + r, width, height.saturating_sub(r * 2), color);
        for dy in 0..r {
            let fy = r - dy;
            let offset = r - isqrt(r * r - fy * fy);
            let row_x = x + offset;
            let row_w = width.saturating_sub(offset * 2);
            if row_w > 0 {
                self.fill_rect(row_x, y + dy, row_w, 1, color);
                self.fill_rect(row_x, y + height - 1 - dy, row_w, 1, color);
            }
        }
    }

    /// Blit this surface to a framebuffer at the specified position.
    /// Uses optimized line-by-line copy.
    pub fn blit_to_framebuffer(&self, fb: &Framebuffer, dest_x: u32, dest_y: u32) {
        let Some(src_addr) = self.mapped_addr else { return };

        let fb_addr = fb.address();
        let fb_stride = fb.stride();
        let fb_bpp = fb.bytes_per_pixel();

        if fb_bpp != self.bytes_per_pixel as usize {
            // Fallback if formats differ
            for sy in 0..self.height {
                let dy = dest_y + sy;
                if dy >= fb.height() { break; }
                for sx in 0..self.width {
                    let dx = dest_x + sx;
                    if dx >= fb.width() { break; }
                    let src_ptr = (src_addr + self.pixel_offset(sx, sy)) as *const u32;
                    let pixel = unsafe { src_ptr.read_volatile() };
                    let dst_ptr = fb.info.pixel_ptr(dx, dy);
                    unsafe { dst_ptr.write_volatile(pixel) };
                }
            }
            return;
        }

        let copy_width = self.width.min(fb.width().saturating_sub(dest_x));
        if copy_width == 0 { return; }

        for sy in 0..self.height {
            let dy = dest_y + sy;
            if dy >= fb.height() {
                break;
            }

            let src_offset = self.pixel_offset(0, sy);
            let dst_offset = (dy * fb_stride + dest_x) as usize * fb_bpp;

            let src_ptr = (src_addr + src_offset) as *const u8;
            let dst_ptr = (fb_addr + dst_offset) as *mut u8;

            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_width as usize * fb_bpp);
            }
        }
    }
}

impl Drop for SharedSurface {
    fn drop(&mut self) {
        // Unmap the region from this process
        if self.mapped_addr.is_some() {
            let _ = shared_region_unmap(self.region_id);
            self.mapped_addr = None;
        }

        // If we own the region, destroy it
        if self.owned {
            let _ = shared_region_destroy(self.region_id);
        }
    }
}

/// Information about a shared surface for IPC transfer
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct SharedSurfaceInfo {
    pub region_id: SharedRegionId,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes_per_pixel: u32,
}

impl SharedSurfaceInfo {
    pub const SIZE: usize = 24; // 8 + 4 + 4 + 4 + 4

    pub fn from_surface(surface: &SharedSurface) -> Self {
        Self {
            region_id: surface.region_id,
            width: surface.width,
            height: surface.height,
            stride: surface.stride,
            bytes_per_pixel: surface.bytes_per_pixel,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&self.region_id.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.width.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.height.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.stride.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.bytes_per_pixel.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            region_id: u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]),
            width: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            height: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            stride: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            bytes_per_pixel: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        })
    }
}

// ============================================================================
// Video Mode Management API
// ============================================================================
//
// These wrappers expose the kernel's BGA/VBE_DISPI video mode management
// syscalls to userspace. They are used primarily by the display_settings app.

/// A video mode entry as returned by SYS_GET_VIDEO_MODES.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VideoModeEntry {
    pub width:        u32,
    pub height:       u32,
    pub bpp:          u32,
    pub refresh_rate: u32,
    /// Flags: bit 0 = LFB available (always 1 for BGA modes)
    pub flags:        u32,
}

impl VideoModeEntry {
    /// Human-readable display string (for UI rendering)
    pub fn label_bytes(&self) -> ([u8; 32], usize) {
        let mut buf = [0u8; 32];
        let len = format_mode_label(&mut buf, self.width, self.height, self.bpp);
        (buf, len)
    }
}

/// Format "WIDTHxHEIGHTxBPP" into a byte buffer.
/// Returns the number of bytes written (not including null terminator).
fn format_mode_label(buf: &mut [u8; 32], width: u32, height: u32, bpp: u32) -> usize {
    let mut pos = 0;
    pos += write_u32_decimal(buf, pos, width);
    if pos < 32 { buf[pos] = b'x'; pos += 1; }
    pos += write_u32_decimal(buf, pos, height);
    if pos < 32 { buf[pos] = b'x'; pos += 1; }
    pos += write_u32_decimal(buf, pos, bpp);
    pos
}

fn write_u32_decimal(buf: &mut [u8; 32], start: usize, mut n: u32) -> usize {
    if start >= 32 { return 0; }
    if n == 0 {
        buf[start] = b'0';
        return 1;
    }
    // Write digits in reverse, then flip
    let mut tmp = [0u8; 12];
    let mut tmp_len = 0;
    while n > 0 {
        tmp[tmp_len] = b'0' + (n % 10) as u8;
        tmp_len += 1;
        n /= 10;
    }
    let available = 32 - start;
    let write_len = tmp_len.min(available);
    for i in 0..write_len {
        buf[start + i] = tmp[tmp_len - 1 - i];
    }
    write_len
}

/// Current video mode info as returned by SYS_GET_CURRENT_VIDEO_MODE.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CurrentVideoMode {
    /// Physical address of the Linear Framebuffer (may be 0 if BGA not active)
    pub lfb_phys:         u64,
    pub width:            u32,
    pub height:           u32,
    /// Bytes per scanline (pitch)
    pub pitch:            u32,
    pub bytes_per_pixel:  u32,
    /// 1 if BGA is present and initialized
    pub bga_available:    u32,
    /// 1 if a mode has been actively set via SYS_SET_VIDEO_MODE
    pub mode_active:      u32,
}

/// Request a video mode change.
///
/// # Parameters
/// - `width`: horizontal resolution (must be divisible by 8)
/// - `height`: vertical resolution
/// - `bpp`: bits per pixel (must be 32 for current driver)
///
/// # Returns
/// `Ok(())` on success, `Err(SyscallError)` on failure.
pub fn set_video_mode(width: u16, height: u16, bpp: u8) -> SyscallResult<()> {
    let packed = (width as u64) | ((height as u64) << 16);
    let result = unsafe { syscall2(SYS_SET_VIDEO_MODE, packed, bpp as u64) };

    if result == ESUCCESS {
        Ok(())
    } else {
        match result {
            x if x == EINVAL   => Err(SyscallError::InvalidArgument),
            x if x == ENOTSUP  => Err(SyscallError::NotSupported),
            x if x == ENOMEM   => Err(SyscallError::OutOfMemory),
            _                   => Err(SyscallError::Unknown(result)),
        }
    }
}

/// Get the total count of available video modes.
pub fn video_mode_count() -> usize {
    let result = unsafe { syscall0(SYS_VIDEO_MODE_COUNT) };
    result as usize
}

/// Enumerate all available video modes into the provided slice.
///
/// Returns the number of modes actually written.
pub fn get_video_modes(out: &mut [VideoModeEntry]) -> usize {
    if out.is_empty() {
        return 0;
    }

    // The kernel writes 5 u32 values per mode (20 bytes each).
    // We pass a u32 slice pointer and mode count.
    let buf_ptr = out.as_mut_ptr() as *mut u32;
    let max_modes = out.len() as u64;

    let result = unsafe {
        crate::raw::syscall2(SYS_GET_VIDEO_MODES, buf_ptr as u64, max_modes)
    };

    if result > 0x8000_0000_0000_0000u64 {
        // Error code
        return 0;
    }
    result as usize
}

/// Get current video mode information from the kernel.
///
/// Returns `Some(CurrentVideoMode)` on success, `None` if the syscall fails.
pub fn get_current_video_mode() -> Option<CurrentVideoMode> {
    let mut buf = [0u64; 4];
    let result = unsafe {
        crate::raw::syscall1(SYS_GET_CURRENT_VIDEO_MODE, buf.as_mut_ptr() as u64)
    };

    if result != ESUCCESS {
        return None;
    }

    // Unpack the four 64-bit words into the CurrentVideoMode struct.
    // Layout matches what sys_get_current_video_mode() writes:
    //   buf[0] = lfb_phys
    //   buf[1] = (height << 32) | width
    //   buf[2] = (bytes_per_pixel << 32) | pitch
    //   buf[3] = (mode_active << 32) | bga_available
    Some(CurrentVideoMode {
        lfb_phys:        buf[0],
        width:           (buf[1] & 0xFFFF_FFFF) as u32,
        height:          ((buf[1] >> 32) & 0xFFFF_FFFF) as u32,
        pitch:           (buf[2] & 0xFFFF_FFFF) as u32,
        bytes_per_pixel: ((buf[2] >> 32) & 0xFFFF_FFFF) as u32,
        bga_available:   (buf[3] & 0xFFFF_FFFF) as u32,
        mode_active:     ((buf[3] >> 32) & 0xFFFF_FFFF) as u32,
    })
}
