// Drawing Context
//
// Provides higher-level drawing operations on a surface.

use crate::color::Color;
use crate::surface::Surface;
use crate::font::{Font, BUILTIN_FONT};

/// Drawing context wrapping a surface
///
/// Provides convenient drawing methods and state management.
pub struct DrawingContext<'a> {
    surface: &'a mut Surface,
    /// Current foreground color
    fg: Color,
    /// Current background color
    bg: Color,
    /// Current font
    font: &'static Font,
    /// Clip rectangle (x, y, width, height)
    clip: Option<(u32, u32, u32, u32)>,
}

impl<'a> DrawingContext<'a> {
    /// Create a drawing context for a surface
    pub fn new(surface: &'a mut Surface) -> Self {
        Self {
            surface,
            fg: Color::WHITE,
            bg: Color::BLACK,
            font: &BUILTIN_FONT,
            clip: None,
        }
    }

    /// Get the surface width
    pub fn width(&self) -> u32 {
        self.surface.width()
    }

    /// Get the surface height
    pub fn height(&self) -> u32 {
        self.surface.height()
    }

    /// Set foreground color
    pub fn set_fg(&mut self, color: Color) {
        self.fg = color;
    }

    /// Set background color
    pub fn set_bg(&mut self, color: Color) {
        self.bg = color;
    }

    /// Get foreground color
    pub fn fg(&self) -> Color {
        self.fg
    }

    /// Get background color
    pub fn bg(&self) -> Color {
        self.bg
    }

    /// Set clip rectangle
    pub fn set_clip(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.clip = Some((x, y, width, height));
    }

    /// Clear clip rectangle
    pub fn clear_clip(&mut self) {
        self.clip = None;
    }

    /// Check if a point is within the clip region
    fn in_clip(&self, x: u32, y: u32) -> bool {
        if let Some((cx, cy, cw, ch)) = self.clip {
            x >= cx && x < cx + cw && y >= cy && y < cy + ch
        } else {
            true
        }
    }

    /// Clear the surface with background color
    pub fn clear(&mut self) {
        self.surface.clear(self.bg);
    }

    /// Draw a pixel
    pub fn draw_pixel(&mut self, x: u32, y: u32) {
        if self.in_clip(x, y) {
            self.surface.draw_pixel(x, y, self.fg);
        }
    }

    /// Fill a rectangle
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        // Apply clipping
        if let Some((cx, cy, cw, ch)) = self.clip {
            let x1 = x.max(cx);
            let y1 = y.max(cy);
            let x2 = (x + width).min(cx + cw);
            let y2 = (y + height).min(cy + ch);

            if x2 > x1 && y2 > y1 {
                self.surface.fill_rect(x1, y1, x2 - x1, y2 - y1, self.fg);
            }
        } else {
            self.surface.fill_rect(x, y, width, height, self.fg);
        }
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32) {
        self.surface.draw_rect(x, y, width, height, self.fg);
    }

    /// Draw a horizontal line
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32) {
        self.surface.draw_hline(x, y, length, self.fg);
    }

    /// Draw a vertical line
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32) {
        self.surface.draw_vline(x, y, length, self.fg);
    }

    /// Draw a character at the given position
    pub fn draw_char(&mut self, x: u32, y: u32, c: char) {
        let glyph = self.font.glyph(c as u8);
        let char_width = self.font.width;
        let char_height = self.font.height;

        for row in 0..char_height {
            let bits = glyph[row as usize];
            for col in 0..char_width {
                let px = x + col as u32;
                let py = y + row as u32;

                if bits & (0x80 >> col) != 0 {
                    self.surface.draw_pixel(px, py, self.fg);
                } else {
                    self.surface.draw_pixel(px, py, self.bg);
                }
            }
        }
    }

    /// Draw a character with transparent background
    pub fn draw_char_transparent(&mut self, x: u32, y: u32, c: char) {
        let glyph = self.font.glyph(c as u8);
        let char_width = self.font.width;
        let char_height = self.font.height;

        for row in 0..char_height {
            let bits = glyph[row as usize];
            for col in 0..char_width {
                if bits & (0x80 >> col) != 0 {
                    let px = x + col as u32;
                    let py = y + row as u32;
                    self.surface.draw_pixel(px, py, self.fg);
                }
            }
        }
    }

    /// Draw a string at the given position
    pub fn draw_string(&mut self, x: u32, y: u32, s: &str) {
        let char_width = self.font.width as u32;
        let mut cx = x;

        for c in s.chars() {
            if c == '\n' {
                continue; // Skip newlines (no multi-line support here)
            }
            self.draw_char(cx, y, c);
            cx += char_width;
        }
    }

    /// Draw a string with transparent background
    pub fn draw_string_transparent(&mut self, x: u32, y: u32, s: &str) {
        let char_width = self.font.width as u32;
        let mut cx = x;

        for c in s.chars() {
            if c == '\n' {
                continue;
            }
            self.draw_char_transparent(cx, y, c);
            cx += char_width;
        }
    }

    /// Get font character width
    pub fn char_width(&self) -> u32 {
        self.font.width as u32
    }

    /// Get font character height
    pub fn char_height(&self) -> u32 {
        self.font.height as u32
    }

    /// Calculate string width in pixels
    pub fn string_width(&self, s: &str) -> u32 {
        s.len() as u32 * self.font.width as u32
    }

    /// Draw a filled rectangle with a specific color
    pub fn fill_rect_color(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.surface.fill_rect(x, y, width, height, color);
    }

    /// Present the surface (notify desktop environment of changes)
    ///
    /// This should be called after drawing is complete for a frame.
    pub fn present(&mut self) {
        // In a real implementation, this would send a Present message
        // to the desktop environment via IPC
        self.surface.clear_dirty();
    }
}
