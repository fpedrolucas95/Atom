//! Color Representation
//!
//! Provides a simple RGB color type for drawing operations.

/// RGB color with 8-bit components
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,  // Alpha (255 = opaque)
}

impl Color {
    // Basic colors
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const RED: Color = Color::rgb(255, 0, 0);
    pub const GREEN: Color = Color::rgb(0, 255, 0);
    pub const BLUE: Color = Color::rgb(0, 0, 255);
    pub const YELLOW: Color = Color::rgb(255, 255, 0);
    pub const CYAN: Color = Color::rgb(0, 255, 255);
    pub const MAGENTA: Color = Color::rgb(255, 0, 255);

    // Nord theme colors
    pub const NORD_BG: Color = Color::rgb(46, 52, 64);
    pub const NORD_FG: Color = Color::rgb(216, 222, 233);
    pub const NORD_ACCENT: Color = Color::rgb(136, 192, 208);
    pub const NORD_PANEL: Color = Color::rgb(59, 66, 82);
    pub const NORD_HIGHLIGHT: Color = Color::rgb(76, 86, 106);

    /// Create an RGB color (fully opaque)
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Create an RGBA color
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Create color from 32-bit value (0xRRGGBB or 0xAARRGGBB)
    pub const fn from_u32(value: u32) -> Self {
        Self {
            a: ((value >> 24) & 0xFF) as u8,
            r: ((value >> 16) & 0xFF) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: (value & 0xFF) as u8,
        }
    }

    /// Convert to 32-bit value for framebuffer (BGR format, common for UEFI)
    pub fn to_bgr32(&self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    /// Convert to 32-bit value (RGB format)
    pub fn to_rgb32(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    /// Blend this color with another using alpha
    pub fn blend(&self, other: Color) -> Color {
        if other.a == 255 {
            return other;
        }
        if other.a == 0 {
            return *self;
        }

        let alpha = other.a as u32;
        let inv_alpha = 255 - alpha;

        Color {
            r: ((self.r as u32 * inv_alpha + other.r as u32 * alpha) / 255) as u8,
            g: ((self.g as u32 * inv_alpha + other.g as u32 * alpha) / 255) as u8,
            b: ((self.b as u32 * inv_alpha + other.b as u32 * alpha) / 255) as u8,
            a: 255,
        }
    }

    /// Darken the color by a factor (0.0 = black, 1.0 = unchanged)
    pub fn darken(&self, factor: f32) -> Color {
        Color {
            r: (self.r as f32 * factor) as u8,
            g: (self.g as f32 * factor) as u8,
            b: (self.b as f32 * factor) as u8,
            a: self.a,
        }
    }

    /// Lighten the color by a factor (0.0 = unchanged, 1.0 = white)
    pub fn lighten(&self, factor: f32) -> Color {
        Color {
            r: self.r + ((255 - self.r) as f32 * factor) as u8,
            g: self.g + ((255 - self.g) as f32 * factor) as u8,
            b: self.b + ((255 - self.b) as f32 * factor) as u8,
            a: self.a,
        }
    }
}
