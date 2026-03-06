//! Panel — background card / container component.
//!
//! A Panel is a rectangular region that visually groups content.  It draws:
//! - A solid fill using `ATOM_COLOR_SURFACE` or `ATOM_COLOR_SURFACE_ALT`
//!   for elevated panels.
//! - A 1-pixel border in `ATOM_COLOR_BORDER`.
//! - Optional drop-shadow (DS `shadows::SOFT`).
//! - Anti-aliased rounded corners (DS `radius::MD` = 12 px).

use libgui::color::Color;
use libgui::surface::Surface;
use atom_theme::colors::{
    ATOM_COLOR_SURFACE, ATOM_COLOR_SURFACE_ALT, ATOM_COLOR_BORDER, ATOM_COLOR_SHADOW,
};
use atom_theme::radius;
use atom_theme::shadows;

/// A rectangular background panel.
pub struct Panel {
    pub x:      u32,
    pub y:      u32,
    pub width:  u32,
    pub height: u32,
    /// When `true`, use `ATOM_COLOR_SURFACE_ALT` and draw a drop-shadow.
    pub elevated: bool,
    /// Corner radius (defaults to `radius::MD`).
    pub corner_radius: u32,
    /// Whether to draw the 1-px border.
    pub show_border: bool,
    /// Whether to draw the drop-shadow.
    pub show_shadow: bool,
}

impl Panel {
    /// Create a standard surface panel.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x, y, width, height,
            elevated: false,
            corner_radius: radius::MD,
            show_border: true,
            show_shadow: false,
        }
    }

    /// Create an elevated panel (SURFACE_ALT background + shadow).
    pub fn elevated(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x, y, width, height,
            elevated: true,
            corner_radius: radius::MD,
            show_border: true,
            show_shadow: true,
        }
    }

    /// Override the corner radius.
    pub fn with_radius(mut self, r: u32) -> Self { self.corner_radius = r; self }
    /// Enable / disable the 1-px border.
    pub fn with_border(mut self, v: bool) -> Self { self.show_border = v; self }
    /// Enable / disable the drop-shadow.
    pub fn with_shadow(mut self, v: bool) -> Self { self.show_shadow = v; self }

    /// Draw the panel into `surface`.
    pub fn draw(&self, surface: &mut Surface) {
        let spec = shadows::SOFT;

        // ── Drop-shadow ───────────────────────────────────────────────────
        if self.show_shadow {
            let sc: Color = ATOM_COLOR_SHADOW.into();
            surface.draw_shadow_layers(
                self.x as i32, self.y as i32,
                self.width, self.height,
                self.corner_radius,
                sc,
                spec.offset_y,
                spec.layers,
                spec.alpha,
            );
        }

        // ── Background fill ───────────────────────────────────────────────
        let bg: Color = if self.elevated {
            ATOM_COLOR_SURFACE_ALT.into()
        } else {
            ATOM_COLOR_SURFACE.into()
        };
        surface.fill_rect_rounded_aa(
            self.x, self.y, self.width, self.height,
            self.corner_radius,
            bg,
        );

        // ── Border ────────────────────────────────────────────────────────
        if self.show_border {
            let bc: Color = ATOM_COLOR_BORDER.into();
            surface.draw_rect_rounded_aa(
                self.x, self.y, self.width, self.height,
                self.corner_radius,
                bc,
            );
        }
    }

    /// Returns `true` if `(x, y)` is within the panel bounds.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.width)  as i32
            && y < (self.y + self.height) as i32
    }
}
