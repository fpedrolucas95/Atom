//! TextLabel — single-line text with DS typography hints.
//!
//! Renders text using the 8×8 bitmap font.  The `style` field records the
//! intended `TypographyStyle` for the future font-rasterisation phase.

extern crate alloc;
use alloc::string::String;

use libgui::color::Color;
use libgui::font::{FONT_WIDTH, FONT_HEIGHT};
use libgui::surface::Surface;
use atom_theme::colors::{ATOM_COLOR_TEXT_PRIMARY, ATOM_COLOR_TEXT_SECONDARY, ATOM_COLOR_TEXT_MUTED};
use atom_theme::TypographyStyle;

/// Semantic colour role for labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelColor {
    /// `ATOM_COLOR_TEXT_PRIMARY`   — default
    Primary,
    /// `ATOM_COLOR_TEXT_SECONDARY` — secondary / subheading
    Secondary,
    /// `ATOM_COLOR_TEXT_MUTED`     — disabled / hint
    Muted,
    /// Custom RGBA colour
    Custom(Color),
}

impl LabelColor {
    pub fn to_color(self) -> Color {
        match self {
            Self::Primary           => ATOM_COLOR_TEXT_PRIMARY.into(),
            Self::Secondary         => ATOM_COLOR_TEXT_SECONDARY.into(),
            Self::Muted             => ATOM_COLOR_TEXT_MUTED.into(),
            Self::Custom(c)         => c,
        }
    }
}

/// A single-line text label.
pub struct TextLabel {
    pub x:    u32,
    pub y:    u32,
    pub text: String,
    pub color: LabelColor,
    /// Background color (transparent = fully opaque black fill from draw_string;
    /// set to `None` to skip background rendering and use `draw_string_transparent`
    /// when that method becomes available).
    pub bg: Option<Color>,
    /// Typography style hint (informational in v1 — actual render is 8 px bitmap).
    pub style: TypographyStyle,
}

impl TextLabel {
    /// Create a body-text label at the given position.
    pub fn new(x: u32, y: u32, text: impl Into<String>) -> Self {
        Self {
            x, y,
            text:  text.into(),
            color: LabelColor::Primary,
            bg:    None,
            style: atom_theme::typography::BODY,
        }
    }

    /// Apply `LabelColor::Secondary`.
    pub fn secondary(mut self) -> Self { self.color = LabelColor::Secondary; self }
    /// Apply `LabelColor::Muted`.
    pub fn muted(mut self) -> Self { self.color = LabelColor::Muted; self }
    /// Apply a custom colour.
    pub fn with_color(mut self, c: Color) -> Self { self.color = LabelColor::Custom(c); self }
    /// Set background (used to clear the label area before drawing glyphs).
    pub fn with_bg(mut self, bg: Color) -> Self { self.bg = Some(bg); self }
    /// Set the typography style hint.
    pub fn with_style(mut self, style: TypographyStyle) -> Self { self.style = style; self }

    /// Width of the label in pixels (characters × FONT_WIDTH).
    pub fn pixel_width(&self) -> u32 {
        self.text.len() as u32 * FONT_WIDTH
    }

    /// Height of the label in pixels (always `FONT_HEIGHT` in v1).
    pub fn pixel_height(&self) -> u32 {
        FONT_HEIGHT
    }

    /// Draw the label into `surface`.
    pub fn draw(&self, surface: &mut Surface) {
        let fg = self.color.to_color();
        let bg = self.bg.unwrap_or(Color::rgba(0, 0, 0, 0));

        if self.style.all_caps {
            // Approximate all-caps by upper-casing ASCII; no re-allocation needed
            let mut buf = [0u8; 256];
            let bytes = self.text.as_bytes();
            let n = bytes.len().min(buf.len());
            for (i, &b) in bytes[..n].iter().enumerate() {
                buf[i] = if b.is_ascii_lowercase() { b - 32 } else { b };
            }
            let s = core::str::from_utf8(&buf[..n]).unwrap_or(&self.text);
            surface.draw_string(self.x, self.y, s, fg, bg);
        } else {
            surface.draw_string(self.x, self.y, &self.text, fg, bg);
        }
    }

    /// Returns `true` if `(x, y)` falls on the label glyph area.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.pixel_width())  as i32
            && y < (self.y + self.pixel_height()) as i32
    }
}
