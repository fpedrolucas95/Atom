// Surface - Abstract Drawing Area
//
// A Surface is an abstract drawing area assigned to an application by
// the desktop environment. Applications draw to their surface, and the
// desktop environment composites surfaces to the screen.
//
// Key points:
// - Applications receive surfaces from the desktop environment
// - The buffer is a shared memory region
// - Applications draw to the buffer and notify the desktop of changes
// - Applications do NOT know their screen position

use crate::color::Color;

/// An abstract drawing surface assigned to an application
///
/// The surface provides a pixel buffer for drawing and methods to
/// notify the desktop environment of changes. Applications should
/// not try to determine their screen position or manipulate window
/// properties - they only draw to this surface.
pub struct Surface {
    /// Surface ID assigned by desktop environment
    surface_id: u32,
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// Pointer to pixel buffer (shared memory)
    buffer: *mut u32,
    /// Stride in bytes (may be > width * 4 due to alignment)
    stride: u32,
    /// Dirty region tracking
    dirty: DirtyRegion,
}

/// Tracks which region of the surface has been modified
#[derive(Debug, Clone, Copy)]
struct DirtyRegion {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    is_dirty: bool,
}

impl DirtyRegion {
    const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            is_dirty: false,
        }
    }

    fn mark(&mut self, x: u32, y: u32, w: u32, h: u32) {
        if !self.is_dirty {
            self.x = x;
            self.y = y;
            self.width = w;
            self.height = h;
            self.is_dirty = true;
        } else {
            // Expand dirty region to include new area
            let x2 = (self.x + self.width).max(x + w);
            let y2 = (self.y + self.height).max(y + h);
            self.x = self.x.min(x);
            self.y = self.y.min(y);
            self.width = x2 - self.x;
            self.height = y2 - self.y;
        }
    }

    fn clear(&mut self) {
        self.is_dirty = false;
        self.x = 0;
        self.y = 0;
        self.width = 0;
        self.height = 0;
    }
}

unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    /// Create a surface from desktop environment allocation
    ///
    /// This is called by libgui internally when the desktop environment
    /// creates a surface for an application. Applications do not call this.
    pub(crate) fn from_allocation(
        surface_id: u32,
        width: u32,
        height: u32,
        buffer: *mut u32,
        stride: u32,
    ) -> Self {
        Self {
            surface_id,
            width,
            height,
            buffer,
            stride,
            dirty: DirtyRegion::new(),
        }
    }

    /// Get the surface ID
    pub fn id(&self) -> u32 {
        self.surface_id
    }

    /// Get the surface width
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Get the surface height
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Get the stride in bytes
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Get the stride in pixels
    pub fn stride_pixels(&self) -> u32 {
        self.stride / 4
    }

    /// Check if a point is within bounds
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height
    }

    /// Draw a single pixel
    pub fn draw_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x < self.width && y < self.height {
            unsafe {
                let offset = (y * self.stride_pixels() + x) as isize;
                self.buffer.offset(offset).write_volatile(color.to_rgb32());
            }
            self.dirty.mark(x, y, 1, 1);
        }
    }

    /// Fill a rectangle with a color
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let x_end = (x + width).min(self.width);
        let y_end = (y + height).min(self.height);
        let x_start = x.min(self.width);
        let y_start = y.min(self.height);

        let pixel = color.to_rgb32();
        let stride = self.stride_pixels();

        for py in y_start..y_end {
            for px in x_start..x_end {
                unsafe {
                    let offset = (py * stride + px) as isize;
                    self.buffer.offset(offset).write_volatile(pixel);
                }
            }
        }

        if x_end > x_start && y_end > y_start {
            self.dirty.mark(x_start, y_start, x_end - x_start, y_end - y_start);
        }
    }

    /// Clear the entire surface with a color
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Draw a horizontal line
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        if y >= self.height {
            return;
        }

        let x_end = (x + length).min(self.width);
        let pixel = color.to_rgb32();
        let stride = self.stride_pixels();

        for px in x..x_end {
            unsafe {
                let offset = (y * stride + px) as isize;
                self.buffer.offset(offset).write_volatile(pixel);
            }
        }

        if x_end > x {
            self.dirty.mark(x, y, x_end - x, 1);
        }
    }

    /// Draw a vertical line
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        if x >= self.width {
            return;
        }

        let y_end = (y + length).min(self.height);
        let pixel = color.to_rgb32();
        let stride = self.stride_pixels();

        for py in y..y_end {
            unsafe {
                let offset = (py * stride + x) as isize;
                self.buffer.offset(offset).write_volatile(pixel);
            }
        }

        if y_end > y {
            self.dirty.mark(x, y, 1, y_end - y);
        }
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        if width == 0 || height == 0 {
            return;
        }

        // Top and bottom
        self.draw_hline(x, y, width, color);
        if height > 1 {
            self.draw_hline(x, y + height - 1, width, color);
        }

        // Left and right (excluding corners)
        if height > 2 {
            self.draw_vline(x, y + 1, height - 2, color);
            if width > 1 {
                self.draw_vline(x + width - 1, y + 1, height - 2, color);
            }
        }
    }

    /// Check if the surface has been modified since last present
    pub fn is_dirty(&self) -> bool {
        self.dirty.is_dirty
    }

    /// Get the dirty region
    pub fn dirty_region(&self) -> Option<(u32, u32, u32, u32)> {
        if self.dirty.is_dirty {
            Some((self.dirty.x, self.dirty.y, self.dirty.width, self.dirty.height))
        } else {
            None
        }
    }

    /// Mark the entire surface as dirty
    pub fn mark_dirty(&mut self) {
        self.dirty.mark(0, 0, self.width, self.height);
    }

    /// Clear the dirty flag (called after presenting)
    pub fn clear_dirty(&mut self) {
        self.dirty.clear();
    }

    /// Get a raw pointer to the buffer (for advanced operations)
    ///
    /// # Safety
    /// The caller must ensure writes stay within bounds and use proper
    /// volatile operations.
    pub unsafe fn buffer_ptr(&self) -> *mut u32 {
        self.buffer
    }

    /// Read a pixel value
    pub fn read_pixel(&self, x: u32, y: u32) -> Option<u32> {
        if x < self.width && y < self.height {
            unsafe {
                let offset = (y * self.stride_pixels() + x) as isize;
                Some(self.buffer.offset(offset).read_volatile())
            }
        } else {
            None
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // Notify desktop environment that this surface is being destroyed
        // In a real implementation, this would send a SurfaceDestroy message
    }
}
