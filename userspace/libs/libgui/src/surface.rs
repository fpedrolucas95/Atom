//! Drawing Surface
//!
//! Provides an abstract drawing surface for applications.
//! Applications draw to their assigned surface, and the desktop
//! compositor handles actual screen rendering.

extern crate alloc;

use crate::color::Color;
use crate::font::{get_glyph, FONT_WIDTH, FONT_HEIGHT};

/// Surface handle (assigned by desktop compositor)
pub type SurfaceId = u32;

/// A drawing surface for an application window
pub struct Surface {
    /// Surface ID (from compositor)
    id: SurfaceId,
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// Stride (bytes per row)
    stride: u32,
    /// Bytes per pixel
    bpp: usize,
    /// Framebuffer address (memory-mapped)
    buffer: *mut u8,
    /// Whether buffer is owned (allocated by us)
    owned: bool,
    /// Dirty flag for damage tracking
    dirty: bool,
}

unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    /// Create a new surface with the given dimensions
    ///
    /// Note: In the full implementation, this would request a surface
    /// from the compositor. For now, it works with a raw framebuffer.
    pub fn new(
        id: SurfaceId,
        width: u32,
        height: u32,
        stride: u32,
        bpp: usize,
        buffer: *mut u8,
    ) -> Self {
        Self {
            id,
            width,
            height,
            stride,
            bpp,
            buffer,
            owned: false,
            dirty: false,
        }
    }

    /// Get surface ID
    pub fn id(&self) -> SurfaceId {
        self.id
    }

    /// Get surface width
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get surface height
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get surface stride
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Check if point is within surface bounds
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    /// Set a pixel at the given coordinates
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = (y * self.stride + x) as usize * self.bpp;
        unsafe {
            let ptr = self.buffer.add(offset) as *mut u32;
            ptr.write_volatile(color.to_bgr32());
        }
        self.dirty = true;
    }

    /// Get a pixel at the given coordinates
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let offset = (y * self.stride + x) as usize * self.bpp;
        unsafe {
            let ptr = self.buffer.add(offset) as *const u32;
            let value = ptr.read_volatile();
            // Convert from BGR format
            Some(Color::rgb(
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
            ))
        }
    }

    /// Fill the entire surface with a color
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Fill a rectangle with a color
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let x_end = (x + width).min(self.width);
        let y_end = (y + height).min(self.height);
        let pixel_value = color.to_bgr32();

        for py in y..y_end {
            for px in x..x_end {
                let offset = (py * self.stride + px) as usize * self.bpp;
                unsafe {
                    let ptr = self.buffer.add(offset) as *mut u32;
                    ptr.write_volatile(pixel_value);
                }
            }
        }
        self.dirty = true;
    }

    /// Draw a horizontal line
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        if y >= self.height {
            return;
        }
        let x_end = (x + length).min(self.width);
        let pixel_value = color.to_bgr32();

        for px in x..x_end {
            let offset = (y * self.stride + px) as usize * self.bpp;
            unsafe {
                let ptr = self.buffer.add(offset) as *mut u32;
                ptr.write_volatile(pixel_value);
            }
        }
        self.dirty = true;
    }

    /// Draw a vertical line
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        if x >= self.width {
            return;
        }
        let y_end = (y + length).min(self.height);
        let pixel_value = color.to_bgr32();

        for py in y..y_end {
            let offset = (py * self.stride + x) as usize * self.bpp;
            unsafe {
                let ptr = self.buffer.add(offset) as *mut u32;
                ptr.write_volatile(pixel_value);
            }
        }
        self.dirty = true;
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.draw_hline(x, y, width, color);
        self.draw_hline(x, y + height.saturating_sub(1), width, color);
        self.draw_vline(x, y, height, color);
        self.draw_vline(x + width.saturating_sub(1), y, height, color);
    }

    /// Draw a single character at the given position
    pub fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let glyph = get_glyph(ch);
        let fg_value = fg.to_bgr32();
        let bg_value = bg.to_bgr32();

        for row in 0..FONT_HEIGHT {
            for col in 0..FONT_WIDTH {
                let px = x + col;
                let py = y + row;
                if px >= self.width || py >= self.height {
                    continue;
                }

                let bit = (glyph[row as usize] >> (7 - col)) & 1;
                let pixel_value = if bit == 1 { fg_value } else { bg_value };

                let offset = (py * self.stride + px) as usize * self.bpp;
                unsafe {
                    let ptr = self.buffer.add(offset) as *mut u32;
                    ptr.write_volatile(pixel_value);
                }
            }
        }
        self.dirty = true;
    }

    /// Draw a string at the given position
    pub fn draw_string(&mut self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for ch in text.bytes() {
            if cx + FONT_WIDTH > self.width {
                break;
            }
            self.draw_char(cx, y, ch, fg, bg);
            cx += FONT_WIDTH;
        }
    }

    /// Draw a string with transparent background (only draw foreground pixels)
    pub fn draw_string_transparent(&mut self, x: u32, y: u32, text: &str, fg: Color) {
        let fg_value = fg.to_bgr32();
        let mut cx = x;

        for ch in text.bytes() {
            if cx + FONT_WIDTH > self.width {
                break;
            }

            let glyph = get_glyph(ch);
            for row in 0..FONT_HEIGHT {
                for col in 0..FONT_WIDTH {
                    let px = cx + col;
                    let py = y + row;
                    if px >= self.width || py >= self.height {
                        continue;
                    }

                    let bit = (glyph[row as usize] >> (7 - col)) & 1;
                    if bit == 1 {
                        let offset = (py * self.stride + px) as usize * self.bpp;
                        unsafe {
                            let ptr = self.buffer.add(offset) as *mut u32;
                            ptr.write_volatile(fg_value);
                        }
                    }
                }
            }
            cx += FONT_WIDTH;
        }
        self.dirty = true;
    }

    /// Copy a region from another surface
    pub fn blit(&mut self, src: &Surface, src_x: u32, src_y: u32, dst_x: u32, dst_y: u32, width: u32, height: u32) {
        for y in 0..height {
            for x in 0..width {
                if let Some(color) = src.get_pixel(src_x + x, src_y + y) {
                    self.set_pixel(dst_x + x, dst_y + y, color);
                }
            }
        }
    }

    /// Check if surface needs redraw
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Clear dirty flag
    pub fn clear_dirty(&mut self) {
        self.dirty = false;
    }

    /// Mark surface as needing redraw
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Get raw buffer pointer (for advanced use)
    pub fn buffer(&self) -> *mut u8 {
        self.buffer
    }

    /// Present the surface (signal compositor to display)
    pub fn present(&mut self) {
        // In a full implementation, this would send a message to the compositor
        // For now, since we're writing directly to the framebuffer, this is a no-op
        self.dirty = false;
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // In full implementation, notify compositor to destroy surface
    }
}
