//! Color Representation
//!
//! Provides an RGBA color type for all drawing operations.
//!
//! Named constants in the `ATOM_COLOR_*` family match the **Atom Design System
//! v1.0 (Luminous Dark)** palette exactly.  Import `atom_theme::colors::*`
//! directly when you need the `atom_syscall::graphics::Color` type (e.g. in
//! the compositor).  These constants are the same values expressed as the
//! `libgui::Color` RGBA type for use in application code.

/// RGBA color with 8-bit per-channel components.
///
/// Alpha: 255 = fully opaque, 0 = fully transparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    // ── Basic primaries ────────────────────────────────────────────────────
    pub const BLACK:   Color = Color::rgb(0,   0,   0);
    pub const WHITE:   Color = Color::rgb(255, 255, 255);
    pub const RED:     Color = Color::rgb(255, 0,   0);
    pub const GREEN:   Color = Color::rgb(0,   255, 0);
    pub const BLUE:    Color = Color::rgb(0,   0,   255);
    pub const YELLOW:  Color = Color::rgb(255, 255, 0);
    pub const CYAN:    Color = Color::rgb(0,   255, 255);
    pub const MAGENTA: Color = Color::rgb(255, 0,   255);

    // ── Nord (legacy — kept for backward-compat, do not use in new code) ──
    pub const NORD_BG:        Color = Color::rgb(46,  52,  64);
    pub const NORD_FG:        Color = Color::rgb(216, 222, 233);
    pub const NORD_ACCENT:    Color = Color::rgb(136, 192, 208);
    pub const NORD_PANEL:     Color = Color::rgb(59,  66,  82);
    pub const NORD_HIGHLIGHT: Color = Color::rgb(76,  86,  106);

    // ── Atom Design System v1.0 — Luminous Dark ───────────────────────────
    //
    // These constants are the single source of visual truth for all libgui
    // applications.  Values are kept in sync with `atom_theme::colors`.

    /// Main desktop / app background                       `#0B0E13`
    pub const ATOM_COLOR_BG:            Color = Color::rgb(0x0B, 0x0E, 0x13);
    /// Surface (cards, panels, dialogs)                    `#141922`
    pub const ATOM_COLOR_SURFACE:       Color = Color::rgb(0x14, 0x19, 0x22);
    /// Alternate / elevated surface                        `#1B2130`
    pub const ATOM_COLOR_SURFACE_ALT:   Color = Color::rgb(0x1B, 0x21, 0x30);
    /// Default border                                      `#2A3142`
    pub const ATOM_COLOR_BORDER:        Color = Color::rgb(0x2A, 0x31, 0x42);
    /// Primary text                                        `#E8ECF4`
    pub const ATOM_COLOR_TEXT_PRIMARY:   Color = Color::rgb(0xE8, 0xEC, 0xF4);
    /// Secondary / subheading text                         `#AAB2C5`
    pub const ATOM_COLOR_TEXT_SECONDARY: Color = Color::rgb(0xAA, 0xB2, 0xC5);
    /// Muted / disabled text                               `#6C7486`
    pub const ATOM_COLOR_TEXT_MUTED:     Color = Color::rgb(0x6C, 0x74, 0x86);
    /// Primary accent blue                                 `#4C8DFF`
    pub const ATOM_COLOR_ACCENT:         Color = Color::rgb(0x4C, 0x8D, 0xFF);
    /// Accent glow / hover state                           `#6AA8FF`
    pub const ATOM_COLOR_ACCENT_GLOW:    Color = Color::rgb(0x6A, 0xA8, 0xFF);
    /// Success (green)                                     `#22C55E`
    pub const ATOM_COLOR_SUCCESS:        Color = Color::rgb(0x22, 0xC5, 0x5E);
    /// Warning (amber)                                     `#F59E0B`
    pub const ATOM_COLOR_WARNING:        Color = Color::rgb(0xF5, 0x9E, 0x0B);
    /// Error / danger (red)                                `#EF4444`
    pub const ATOM_COLOR_ERROR:          Color = Color::rgb(0xEF, 0x44, 0x44);
    /// Gradient start (accent blue)                        `#4C8DFF`
    pub const ATOM_GRADIENT_START:       Color = Color::rgb(0x4C, 0x8D, 0xFF);
    /// Gradient end (violet)                               `#7C5CFF`
    pub const ATOM_GRADIENT_END:         Color = Color::rgb(0x7C, 0x5C, 0xFF);

    // ── Legacy ATOM_* aliases (kept for backward compat) ─────────────────
    /// @deprecated — use `ATOM_COLOR_BG`
    pub const ATOM_BG_DEEP:       Color = Color::ATOM_COLOR_BG;
    /// @deprecated — use `ATOM_COLOR_SURFACE`
    pub const ATOM_BG:            Color = Color::ATOM_COLOR_SURFACE;
    /// @deprecated — use `ATOM_COLOR_SURFACE_ALT`
    pub const ATOM_BG_ELEVATED:   Color = Color::ATOM_COLOR_SURFACE_ALT;
    /// @deprecated — use `ATOM_COLOR_SURFACE_ALT`
    pub const ATOM_BG_SURFACE:    Color = Color::ATOM_COLOR_SURFACE_ALT;
    /// @deprecated — use `ATOM_COLOR_BORDER`
    pub const ATOM_BORDER:        Color = Color::ATOM_COLOR_BORDER;
    /// @deprecated — use `ATOM_COLOR_BORDER`
    pub const ATOM_BORDER_SUBTLE: Color = Color::ATOM_COLOR_BORDER;
    /// @deprecated — use `ATOM_COLOR_TEXT_PRIMARY`
    pub const ATOM_TEXT:          Color = Color::ATOM_COLOR_TEXT_PRIMARY;
    /// @deprecated — use `ATOM_COLOR_TEXT_SECONDARY`
    pub const ATOM_TEXT_SECONDARY: Color = Color::ATOM_COLOR_TEXT_SECONDARY;
    /// @deprecated — use `ATOM_COLOR_TEXT_MUTED`
    pub const ATOM_TEXT_MUTED:    Color = Color::ATOM_COLOR_TEXT_MUTED;
    /// @deprecated — use `ATOM_COLOR_ACCENT`
    pub const ATOM_ACCENT:        Color = Color::ATOM_COLOR_ACCENT;
    /// @deprecated — use `ATOM_COLOR_ACCENT_GLOW`
    pub const ATOM_ACCENT_HOVER:  Color = Color::ATOM_COLOR_ACCENT_GLOW;
    /// @deprecated — use `ATOM_COLOR_SUCCESS`
    pub const ATOM_SUCCESS:       Color = Color::ATOM_COLOR_SUCCESS;
    /// @deprecated — use `ATOM_COLOR_WARNING`
    pub const ATOM_WARNING:       Color = Color::ATOM_COLOR_WARNING;
    /// @deprecated — use `ATOM_COLOR_ERROR`
    pub const ATOM_ERROR:         Color = Color::ATOM_COLOR_ERROR;
    /// @deprecated — use `ATOM_COLOR_ACCENT`
    pub const ATOM_INFO:          Color = Color::ATOM_COLOR_ACCENT;

    // ── Constructors ──────────────────────────────────────────────────────

    /// Create a fully-opaque RGB colour.
    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Create an RGBA colour.
    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Create from a 32-bit packed value (`0xAARRGGBB`).
    #[inline]
    pub const fn from_u32(value: u32) -> Self {
        Self {
            a: ((value >> 24) & 0xFF) as u8,
            r: ((value >> 16) & 0xFF) as u8,
            g: ((value >>  8) & 0xFF) as u8,
            b: ( value        & 0xFF) as u8,
        }
    }

    // ── Format conversions ────────────────────────────────────────────────

    /// Convert to 32-bit framebuffer value (BGR layout used by the kernel).
    #[inline]
    pub fn to_bgr32(&self) -> u32 {
        ((self.b as u32) << 16) | ((self.g as u32) << 8) | (self.r as u32)
    }

    /// Convert to 32-bit value (RGB layout).
    #[inline]
    pub fn to_rgb32(&self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }

    // ── Colour operations ─────────────────────────────────────────────────

    /// Alpha-blend `other` over `self`.
    #[inline]
    pub fn blend(&self, other: Color) -> Color {
        if other.a == 255 { return other; }
        if other.a == 0   { return *self; }

        let a   = other.a as u32;
        let ia  = 255 - a;
        Color {
            r: ((self.r as u32 * ia + other.r as u32 * a) / 255) as u8,
            g: ((self.g as u32 * ia + other.g as u32 * a) / 255) as u8,
            b: ((self.b as u32 * ia + other.b as u32 * a) / 255) as u8,
            a: 255,
        }
    }

    /// Return this colour darkened by `factor` (0.0 = black, 1.0 = unchanged).
    #[inline]
    pub fn darken(&self, factor: f32) -> Color {
        Color {
            r: (self.r as f32 * factor) as u8,
            g: (self.g as f32 * factor) as u8,
            b: (self.b as f32 * factor) as u8,
            a: self.a,
        }
    }

    /// Return this colour lightened toward white by `factor`
    /// (0.0 = unchanged, 1.0 = white).
    #[inline]
    pub fn lighten(&self, factor: f32) -> Color {
        Color {
            r: self.r + ((255 - self.r) as f32 * factor) as u8,
            g: self.g + ((255 - self.g) as f32 * factor) as u8,
            b: self.b + ((255 - self.b) as f32 * factor) as u8,
            a: self.a,
        }
    }
}

// ── From conversions ──────────────────────────────────────────────────────────

/// Convert from `atom_syscall::graphics::Color` (RGB, opaque) to `libgui::Color`.
///
/// This allows direct use of `atom_theme` tokens in libgui drawing code:
/// ```ignore
/// use libgui::Color;
/// use atom_theme::colors::ATOM_COLOR_BG;
///
/// let c: Color = ATOM_COLOR_BG.into();
/// surface.fill_rect(0, 0, w, h, c);
/// ```
impl From<atom_syscall::graphics::Color> for Color {
    #[inline]
    fn from(c: atom_syscall::graphics::Color) -> Self {
        Color::rgb(c.r, c.g, c.b)
    }
}
