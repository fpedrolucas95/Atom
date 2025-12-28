// Framebuffer and graphics syscalls

use crate::error::{ESUCCESS, EPERM, EINVAL, ENOMEM, EBUSY, SyscallError, SyscallResult};
use crate::raw::{syscall1, syscall3, numbers::*};

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

    if result >= u64::MAX - 100 {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        Ok(result)
    }
}

/// Map a shared memory region into the current process address space
pub fn shared_region_map(region_id: SharedRegionId, virt_addr: usize, flags: SharedMemFlags) -> SyscallResult<()> {
    let result = unsafe {
        syscall3(SYS_SHARED_REGION_MAP, region_id, virt_addr as u64, flags.to_raw())
    };

    if result == ESUCCESS {
        Ok(())
    } else {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            x if x == EBUSY => Err(SyscallError::ResourceBusy),
            _ => Err(SyscallError::Unknown(result)),
        }
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
    /// Default virtual address for surface mapping (in userspace range)
    const DEFAULT_MAP_ADDR: usize = 0x0000_2000_0000;

    /// Create a new shared surface owned by this process
    pub fn create(width: u32, height: u32) -> SyscallResult<Self> {
        let bytes_per_pixel = 4u32; // BGRA format
        let stride = width;
        let size = (stride * height * bytes_per_pixel) as usize;

        let region_id = shared_region_create(size)?;

        // Map into our address space
        let map_addr = Self::DEFAULT_MAP_ADDR;
        shared_region_map(region_id, map_addr, SharedMemFlags::READ_WRITE)?;

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

    /// Create a surface from an existing shared region (for client processes)
    pub fn from_region(region_id: SharedRegionId, width: u32, height: u32) -> SyscallResult<Self> {
        let bytes_per_pixel = 4u32;
        let stride = width;

        // Map into our address space at a different location for clients
        let map_addr = Self::DEFAULT_MAP_ADDR + 0x0100_0000; // Offset for client
        shared_region_map(region_id, map_addr, SharedMemFlags::READ_WRITE)?;

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

    /// Blit this surface to a framebuffer at the specified position
    pub fn blit_to_framebuffer(&self, fb: &Framebuffer, dest_x: u32, dest_y: u32) {
        let Some(src_addr) = self.mapped_addr else { return };

        let fb_addr = fb.address();
        let fb_stride = fb.stride();
        let fb_bpp = fb.bytes_per_pixel();

        for sy in 0..self.height {
            let dy = dest_y + sy;
            if dy >= fb.height() {
                break;
            }

            for sx in 0..self.width {
                let dx = dest_x + sx;
                if dx >= fb.width() {
                    break;
                }

                // Read from surface
                let src_offset = self.pixel_offset(sx, sy);
                let src_ptr = (src_addr + src_offset) as *const u32;
                let pixel = unsafe { src_ptr.read_volatile() };

                // Write to framebuffer
                let dst_offset = (dy * fb_stride + dx) as usize * fb_bpp;
                let dst_ptr = (fb_addr + dst_offset) as *mut u32;
                unsafe {
                    dst_ptr.write_volatile(pixel);
                }
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
