//! # Atom Design System v1.0 — Luminous Dark
//!
//! Single source of truth for **all visual design tokens** used across the
//! Atom OS UI stack.  Import this crate to guarantee consistency between the
//! compositor (`ui_shell`), the widget toolkit (`atom_ui`), and individual
//! applications.
//!
//! ## Quick Start
//!
//! ```ignore
//! use atom_theme::colors::{ATOM_COLOR_BG, ATOM_COLOR_ACCENT};
//! use atom_theme::{spacing, radius};
//!
//! let padding = spacing::LG;   // 16 px
//! let corner  = radius::MD;    // 12 px
//! ```
//!
//! ## Architecture
//!
//! ```text
//! atom_theme
//! ├── colors        — Luminous Dark palette (all 12 named tokens + gradient pair)
//! ├── spacing       — 4 px grid: XS(4) SM(8) MD(12) LG(16) XL(20) XXL(24) XXXL(32)
//! ├── radius        — corner radii: XS(4) SM(8) MD(12) LG(16)
//! ├── shadows       — ShadowSpec: SOFT and WINDOW
//! ├── animation     — duration tokens: FAST(80) NORMAL(120) SLOW(200) ms
//! ├── typography    — TypographyStyle descriptors (see note below)
//! └── AtomThemeV1   — zero-size typed accessor struct
//! ```
//!
//! ## Typography Note (v1 Limitation)
//!
//! The DS specifies Inter / JetBrains Mono at 24/16/14/13/12 pt.  **In v1,
//! all text is rendered with the 8×8 bitmap font embedded in `atom_syscall`.**
//! The `typography` module records the *design intent* for each style so that
//! a future font-rasterisation phase can slot in without changing call sites.
//!
//! ## Color Types
//!
//! Tokens are typed as `atom_syscall::graphics::Color` (RGB, opaque).
//! Alpha values for blending are provided as separate `u8` constants where
//! needed (e.g. `SHADOW_SOFT_ALPHA`).
//!
//! Converting to `libgui::Color`:
//! ```ignore
//! use libgui::Color as GuiColor;
//! let c = GuiColor::rgb(ATOM_COLOR_BG.r, ATOM_COLOR_BG.g, ATOM_COLOR_BG.b);
//! // or use the From impl if libgui is patched to impl it:
//! // let c: GuiColor = ATOM_COLOR_BG.into();
//! ```

#![no_std]

use atom_syscall::graphics::Color;

// ─────────────────────────────────────────────────────────────────────────────
// Color Tokens
// ─────────────────────────────────────────────────────────────────────────────

/// Luminous Dark palette — all named color tokens.
///
/// Every constant is an opaque `atom_syscall::graphics::Color` (RGB).  For
/// translucent fills use the alpha helpers in the [`shadows`] module or pass
/// an explicit `u8` alpha to `fill_rect_alpha` / `draw_shadow_layers`.
pub mod colors {
    use atom_syscall::graphics::Color;

    // ── Backgrounds ──────────────────────────────────────────────────────────

    /// Main desktop / app background                   `#0B0E13`
    pub const ATOM_COLOR_BG: Color          = Color::new(0x0B, 0x0E, 0x13);
    /// Surface (cards, panels, dialogs)                `#141922`
    pub const ATOM_COLOR_SURFACE: Color     = Color::new(0x14, 0x19, 0x22);
    /// Alternate / elevated surface                    `#1B2130`
    pub const ATOM_COLOR_SURFACE_ALT: Color = Color::new(0x1B, 0x21, 0x30);

    // ── Borders ──────────────────────────────────────────────────────────────

    /// Default border                                  `#2A3142`
    pub const ATOM_COLOR_BORDER: Color      = Color::new(0x2A, 0x31, 0x42);

    // ── Text ─────────────────────────────────────────────────────────────────

    /// Primary text                                    `#E8ECF4`
    pub const ATOM_COLOR_TEXT_PRIMARY: Color   = Color::new(0xE8, 0xEC, 0xF4);
    /// Secondary / subheading text                     `#AAB2C5`
    pub const ATOM_COLOR_TEXT_SECONDARY: Color = Color::new(0xAA, 0xB2, 0xC5);
    /// Muted / disabled text                           `#6C7486`
    pub const ATOM_COLOR_TEXT_MUTED: Color     = Color::new(0x6C, 0x74, 0x86);

    // ── Accent ───────────────────────────────────────────────────────────────

    /// Primary accent blue                             `#4C8DFF`
    pub const ATOM_COLOR_ACCENT: Color      = Color::new(0x4C, 0x8D, 0xFF);
    /// Accent glow / hover state                       `#6AA8FF`
    pub const ATOM_COLOR_ACCENT_GLOW: Color = Color::new(0x6A, 0xA8, 0xFF);

    // ── Status ───────────────────────────────────────────────────────────────

    /// Success (green)                                 `#22C55E`
    pub const ATOM_COLOR_SUCCESS: Color = Color::new(0x22, 0xC5, 0x5E);
    /// Warning (amber)                                 `#F59E0B`
    pub const ATOM_COLOR_WARNING: Color = Color::new(0xF5, 0x9E, 0x0B);
    /// Error / danger (red)                            `#EF4444`
    pub const ATOM_COLOR_ERROR: Color   = Color::new(0xEF, 0x44, 0x44);

    // ── Gradient endpoints ───────────────────────────────────────────────────

    /// Primary gradient — start (accent blue)          `#4C8DFF`
    pub const ATOM_GRADIENT_PRIMARY_START: Color = Color::new(0x4C, 0x8D, 0xFF);
    /// Primary gradient — end (violet)                 `#7C5CFF`
    pub const ATOM_GRADIENT_PRIMARY_END: Color   = Color::new(0x7C, 0x5C, 0xFF);

    // ── Convenience re-exports for common DS usage ────────────────────────────

    /// Window chrome background (matches SURFACE_ALT)
    pub const ATOM_COLOR_WIN_BG: Color     = ATOM_COLOR_SURFACE_ALT;
    /// Window header (slightly lighter than win bg)
    pub const ATOM_COLOR_WIN_HDR: Color    = Color::new(0x1E, 0x26, 0x36);
    /// Window header when focused
    pub const ATOM_COLOR_WIN_HDR_FOC: Color = Color::new(0x22, 0x2C, 0x3E);
    /// Window border (matches BORDER)
    pub const ATOM_COLOR_WIN_BORDER: Color  = ATOM_COLOR_BORDER;
    /// Window border when focused (brighter)
    pub const ATOM_COLOR_WIN_BORDER_FOC: Color = Color::new(0x3E, 0x4E, 0x6E);

    /// Dock background
    pub const ATOM_COLOR_DOCK_BG: Color   = Color::new(0x10, 0x13, 0x1C);
    /// Dock border
    pub const ATOM_COLOR_DOCK_BORDER: Color = Color::new(0x2C, 0x34, 0x48);

    /// Traffic-light close button
    pub const ATOM_COLOR_BTN_CLOSE: Color    = ATOM_COLOR_ERROR;
    /// Traffic-light maximise button
    pub const ATOM_COLOR_BTN_MAX: Color      = ATOM_COLOR_SUCCESS;
    /// Traffic-light minimise button
    pub const ATOM_COLOR_BTN_MIN: Color      = ATOM_COLOR_WARNING;
    /// Traffic-light button when window is not focused
    pub const ATOM_COLOR_BTN_INACTIVE: Color = Color::new(0x38, 0x3E, 0x52);

    /// Context-menu background
    pub const ATOM_COLOR_MENU_BG: Color     = ATOM_COLOR_SURFACE;
    /// Context-menu border
    pub const ATOM_COLOR_MENU_BORDER: Color = ATOM_COLOR_BORDER;
    /// Context-menu text
    pub const ATOM_COLOR_MENU_TEXT: Color   = ATOM_COLOR_TEXT_PRIMARY;
    /// Context-menu hover/selected background
    pub const ATOM_COLOR_MENU_SEL: Color    = Color::new(0x25, 0x30, 0x48);

    /// Shadow base colour (near-black with blue tint)
    pub const ATOM_COLOR_SHADOW: Color = Color::new(0x04, 0x05, 0x0A);
}

// ─────────────────────────────────────────────────────────────────────────────
// Spacing Tokens  (4 px grid)
// ─────────────────────────────────────────────────────────────────────────────

/// Spacing scale based on a **4 px grid**.
///
/// Use these constants wherever padding, gap, or margin values are needed so
/// that the entire UI snaps to the grid automatically.
pub mod spacing {
    /// 4 px  — icon padding, micro gap
    pub const XS:   u32 = 4;
    /// 8 px  — inner padding (tight)
    pub const SM:   u32 = 8;
    /// 12 px — inner padding (comfortable)
    pub const MD:   u32 = 12;
    /// 16 px — section gap, outer padding
    pub const LG:   u32 = 16;
    /// 20 px — large section gap
    pub const XL:   u32 = 20;
    /// 24 px — component separation
    pub const XXL:  u32 = 24;
    /// 32 px — page/panel margin
    pub const XXXL: u32 = 32;
}

// ─────────────────────────────────────────────────────────────────────────────
// Radius Tokens
// ─────────────────────────────────────────────────────────────────────────────

/// Corner-radius scale.
pub mod radius {
    /// 4 px  — tight (badges, small tags)
    pub const XS: u32 = 4;
    /// 8 px  — default (buttons, inputs, tooltips)
    pub const SM: u32 = 8;
    /// 12 px — medium (cards, dialogs, menus)
    pub const MD: u32 = 12;
    /// 16 px — large (floating panels, dock)
    pub const LG: u32 = 16;
}

// ─────────────────────────────────────────────────────────────────────────────
// Shadow / Glow Specifications
// ─────────────────────────────────────────────────────────────────────────────

/// Raster shadow specification for multi-layer alpha expansion.
///
/// ## Rendering strategy
///
/// True Gaussian blur is too expensive for a software rasteriser.  Instead,
/// `draw_shadow_layers` renders `layers` concentric rounded rectangles, each
/// progressively closer to the content rect and more opaque.  The stepped
/// alpha approximation is visually indistinguishable from a shallow blur at
/// 96 dpi.
///
/// ```text
///   layer 0 (outermost, alpha ≈ base/layers × 1)  ────────────
///   layer 1                                        ──────────────
///   layer 2 (innermost, alpha ≈ base)              ────────────────
///   [ content rect ]
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ShadowSpec {
    /// Vertical pixel offset (positive = shadow drops below the widget)
    pub offset_y: i32,
    /// Number of alpha expansion rings
    pub layers: u32,
    /// Alpha of the *outermost* ring (0 = transparent, 255 = opaque)
    pub alpha: u8,
}

/// Shadow / glow presets.
pub mod shadows {
    use super::ShadowSpec;

    /// Soft shadow — card / button elevation.
    ///
    /// DS spec: offset (0, 2), blur 8, global alpha 0.20
    pub const SOFT: ShadowSpec = ShadowSpec {
        offset_y: 2,
        layers:   4,
        alpha:    51,   // 0.20 × 255 ≈ 51
    };

    /// Window drop shadow.
    ///
    /// DS spec: offset (0, 6), blur 18, global alpha 0.35
    pub const WINDOW: ShadowSpec = ShadowSpec {
        offset_y: 6,
        layers:   9,
        alpha:    89,   // 0.35 × 255 ≈ 89
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// Animation Tokens
// ─────────────────────────────────────────────────────────────────────────────

/// Easing durations in milliseconds.  All DS transitions use **ease-out**.
pub mod animation {
    /// 80 ms — fast micro-interaction (button press, icon tap)
    pub const FAST:   u32 = 80;
    /// 120 ms — default transition (hover, focus, menu open)
    pub const NORMAL: u32 = 120;
    /// 200 ms — slow reveal (panel open, page transition)
    pub const SLOW:   u32 = 200;
}

// ─────────────────────────────────────────────────────────────────────────────
// Typography Tokens
// ─────────────────────────────────────────────────────────────────────────────

/// Typography style descriptor.
///
/// ## v1 constraint
///
/// All text in Atom OS v1 is rendered with the **8×8 bitmap font** built into
/// `atom_syscall`.  The `desired_size_pt` field records the *intended* point
/// size for the future typography phase (Inter / JetBrains Mono).  Until then,
/// callers should treat `all_caps` and `letter_spacing_x100` as visual hints
/// that can be approximated with the bitmap font (e.g. draw the label in
/// upper-case).
///
/// **Planned typography phase:** Add a font rasteriser (FreeType-lite or a
/// small Rust TTF renderer), update `Surface::draw_string_styled` to accept a
/// `TypographyStyle`, and eliminate the 8×8 limitation without changing any
/// call sites.
#[derive(Debug, Clone, Copy)]
pub struct TypographyStyle {
    /// Target size in pt (informational in v1 — actual render is 8 px)
    pub desired_size_pt: u32,
    /// Letter-spacing multiplier × 100  (100 = normal, 110 = +10 % tracking)
    pub letter_spacing_x100: u32,
    /// Render in ALL CAPS presentation
    pub all_caps: bool,
    /// Line-height multiplier (1 = single, 2 = double, …)
    pub line_height_mul: u32,
}

/// Typography scale.
pub mod typography {
    use super::TypographyStyle;

    /// Display — hero text, splash titles.  Target: Inter Bold 24 pt
    pub const DISPLAY: TypographyStyle = TypographyStyle {
        desired_size_pt: 24, letter_spacing_x100: 100, all_caps: false, line_height_mul: 1,
    };

    /// Window / panel title.  Target: Inter SemiBold 16 pt
    pub const WINDOW_TITLE: TypographyStyle = TypographyStyle {
        desired_size_pt: 16, letter_spacing_x100: 100, all_caps: false, line_height_mul: 1,
    };

    /// Body — main UI text.  Target: Inter Regular 14 pt
    pub const BODY: TypographyStyle = TypographyStyle {
        desired_size_pt: 14, letter_spacing_x100: 100, all_caps: false, line_height_mul: 1,
    };

    /// Small — secondary labels, hints.  Target: Inter Regular 12 pt
    pub const SMALL: TypographyStyle = TypographyStyle {
        desired_size_pt: 12, letter_spacing_x100: 100, all_caps: false, line_height_mul: 1,
    };

    /// Mono — terminal, code blocks.  Target: JetBrains Mono Regular 13 pt
    pub const MONO: TypographyStyle = TypographyStyle {
        desired_size_pt: 13, letter_spacing_x100: 100, all_caps: false, line_height_mul: 1,
    };

    /// Label — tiny caption, badge.  Target: Inter Medium 11 pt, tracked
    pub const LABEL: TypographyStyle = TypographyStyle {
        desired_size_pt: 11, letter_spacing_x100: 110, all_caps: true, line_height_mul: 1,
    };
}

// ─────────────────────────────────────────────────────────────────────────────
// Shell Metrics
// ─────────────────────────────────────────────────────────────────────────────

/// Layout metrics for the desktop shell chrome.
pub mod shell {
    /// Top bar height (px)
    pub const TOP_BAR_HEIGHT: u32 = 32;
    /// Top bar horizontal padding (px)
    pub const TOP_BAR_PADDING: u32 = 16;
    /// Top bar icon size (px)
    pub const TOP_BAR_ICON_SIZE: u32 = 18;

    /// Dock total height (px)
    pub const DOCK_HEIGHT: u32 = 64;
    /// Dock item size (px × px)
    pub const DOCK_ITEM_SIZE: u32 = 48;
    /// Dock item corner radius (px)
    pub const DOCK_ITEM_RADIUS: u32 = 10;

    /// Window corner radius (px)
    pub const WINDOW_RADIUS: u32 = 12;
    /// Window title bar height (px)
    pub const WINDOW_TITLE_HEIGHT: u32 = 36;
    /// Window control button size (px × px)
    pub const WINDOW_BTN_SIZE: u32 = 28;
}

// ─────────────────────────────────────────────────────────────────────────────
// AtomThemeV1 — zero-size typed accessor
// ─────────────────────────────────────────────────────────────────────────────

/// Complete typed accessor for all Atom Design System v1.0 tokens.
///
/// Use the module-level constants for the most ergonomic experience.  This
/// struct is useful when you need to pass the theme as a value (e.g. to a
/// generic rendering function).
pub struct AtomThemeV1;

impl AtomThemeV1 {
    // ── Colors ────────────────────────────────────────────────────────────────
    #[inline] pub const fn bg()                     -> Color { colors::ATOM_COLOR_BG }
    #[inline] pub const fn surface()                -> Color { colors::ATOM_COLOR_SURFACE }
    #[inline] pub const fn surface_alt()            -> Color { colors::ATOM_COLOR_SURFACE_ALT }
    #[inline] pub const fn border()                 -> Color { colors::ATOM_COLOR_BORDER }
    #[inline] pub const fn text_primary()           -> Color { colors::ATOM_COLOR_TEXT_PRIMARY }
    #[inline] pub const fn text_secondary()         -> Color { colors::ATOM_COLOR_TEXT_SECONDARY }
    #[inline] pub const fn text_muted()             -> Color { colors::ATOM_COLOR_TEXT_MUTED }
    #[inline] pub const fn accent()                 -> Color { colors::ATOM_COLOR_ACCENT }
    #[inline] pub const fn accent_glow()            -> Color { colors::ATOM_COLOR_ACCENT_GLOW }
    #[inline] pub const fn success()                -> Color { colors::ATOM_COLOR_SUCCESS }
    #[inline] pub const fn warning()                -> Color { colors::ATOM_COLOR_WARNING }
    #[inline] pub const fn error()                  -> Color { colors::ATOM_COLOR_ERROR }
    #[inline] pub const fn gradient_start()         -> Color { colors::ATOM_GRADIENT_PRIMARY_START }
    #[inline] pub const fn gradient_end()           -> Color { colors::ATOM_GRADIENT_PRIMARY_END }

    // ── Spacing ───────────────────────────────────────────────────────────────
    #[inline] pub const fn spacing_xs()   -> u32 { spacing::XS }
    #[inline] pub const fn spacing_sm()   -> u32 { spacing::SM }
    #[inline] pub const fn spacing_md()   -> u32 { spacing::MD }
    #[inline] pub const fn spacing_lg()   -> u32 { spacing::LG }
    #[inline] pub const fn spacing_xl()   -> u32 { spacing::XL }
    #[inline] pub const fn spacing_xxl()  -> u32 { spacing::XXL }
    #[inline] pub const fn spacing_xxxl() -> u32 { spacing::XXXL }

    // ── Radius ────────────────────────────────────────────────────────────────
    #[inline] pub const fn radius_xs() -> u32 { radius::XS }
    #[inline] pub const fn radius_sm() -> u32 { radius::SM }
    #[inline] pub const fn radius_md() -> u32 { radius::MD }
    #[inline] pub const fn radius_lg() -> u32 { radius::LG }

    // ── Shadows ───────────────────────────────────────────────────────────────
    #[inline] pub const fn shadow_soft()   -> ShadowSpec { shadows::SOFT }
    #[inline] pub const fn shadow_window() -> ShadowSpec { shadows::WINDOW }

    // ── Animation ─────────────────────────────────────────────────────────────
    #[inline] pub const fn anim_fast()   -> u32 { animation::FAST }
    #[inline] pub const fn anim_normal() -> u32 { animation::NORMAL }
    #[inline] pub const fn anim_slow()   -> u32 { animation::SLOW }
}
