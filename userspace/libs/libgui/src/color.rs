// Color Representation
//
// Provides RGBA color type for drawing operations.

/// RGBA color with 8 bits per channel
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Create a new opaque color
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Create a new color with alpha
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Convert to 32-bit ARGB format (for framebuffer)
    pub const fn to_argb(&self) -> u32 {
        ((self.a as u32) << 24)
            | ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
    }

    /// Convert to 32-bit BGRA format (common framebuffer format)
    pub const fn to_bgra(&self) -> u32 {
        ((self.b as u32) << 24)
            | ((self.g as u32) << 16)
            | ((self.r as u32) << 8)
            | (self.a as u32)
    }

    /// Convert to 32-bit RGB format (no alpha, for direct framebuffer writes)
    pub const fn to_rgb32(&self) -> u32 {
        ((self.r as u32) << 16)
            | ((self.g as u32) << 8)
            | (self.b as u32)
    }

    /// Create from 32-bit ARGB value
    pub const fn from_argb(value: u32) -> Self {
        Self {
            a: ((value >> 24) & 0xFF) as u8,
            r: ((value >> 16) & 0xFF) as u8,
            g: ((value >> 8) & 0xFF) as u8,
            b: (value & 0xFF) as u8,
        }
    }

    /// Blend this color over another (simple alpha blending)
    pub fn blend_over(&self, background: Color) -> Color {
        if self.a == 255 {
            return *self;
        }
        if self.a == 0 {
            return background;
        }

        let fg_a = self.a as u32;
        let bg_a = 255 - fg_a;

        Color {
            r: ((self.r as u32 * fg_a + background.r as u32 * bg_a) / 255) as u8,
            g: ((self.g as u32 * fg_a + background.g as u32 * bg_a) / 255) as u8,
            b: ((self.b as u32 * fg_a + background.b as u32 * bg_a) / 255) as u8,
            a: 255,
        }
    }

    /// Lighten the color
    pub fn lighten(&self, amount: u8) -> Color {
        Color {
            r: self.r.saturating_add(amount),
            g: self.g.saturating_add(amount),
            b: self.b.saturating_add(amount),
            a: self.a,
        }
    }

    /// Darken the color
    pub fn darken(&self, amount: u8) -> Color {
        Color {
            r: self.r.saturating_sub(amount),
            g: self.g.saturating_sub(amount),
            b: self.b.saturating_sub(amount),
            a: self.a,
        }
    }

    // Common colors
    pub const BLACK: Color = Color::new(0, 0, 0);
    pub const WHITE: Color = Color::new(255, 255, 255);
    pub const RED: Color = Color::new(255, 0, 0);
    pub const GREEN: Color = Color::new(0, 255, 0);
    pub const BLUE: Color = Color::new(0, 0, 255);
    pub const YELLOW: Color = Color::new(255, 255, 0);
    pub const CYAN: Color = Color::new(0, 255, 255);
    pub const MAGENTA: Color = Color::new(255, 0, 255);
    pub const GRAY: Color = Color::new(128, 128, 128);
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    // Nord theme colors (commonly used in Atom)
    pub const NORD0: Color = Color::new(46, 52, 64);    // Polar Night
    pub const NORD1: Color = Color::new(59, 66, 82);
    pub const NORD2: Color = Color::new(67, 76, 94);
    pub const NORD3: Color = Color::new(76, 86, 106);
    pub const NORD4: Color = Color::new(216, 222, 233); // Snow Storm
    pub const NORD5: Color = Color::new(229, 233, 240);
    pub const NORD6: Color = Color::new(236, 239, 244);
    pub const NORD7: Color = Color::new(143, 188, 187); // Frost
    pub const NORD8: Color = Color::new(136, 192, 208);
    pub const NORD9: Color = Color::new(129, 161, 193);
    pub const NORD10: Color = Color::new(94, 129, 172);
    pub const NORD11: Color = Color::new(191, 97, 106); // Aurora
    pub const NORD12: Color = Color::new(208, 135, 112);
    pub const NORD13: Color = Color::new(235, 203, 139);
    pub const NORD14: Color = Color::new(163, 190, 140);
    pub const NORD15: Color = Color::new(180, 142, 173);
}

impl Default for Color {
    fn default() -> Self {
        Color::BLACK
    }
}
