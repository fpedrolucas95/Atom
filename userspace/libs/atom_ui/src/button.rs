//! Button component — three visual variants, four interaction states.
//!
//! ## Visual variants
//!
//! | Style     | Background                   | Label colour         |
//! |-----------|------------------------------|----------------------|
//! | `Default` | `SURFACE_ALT` flat fill      | `TEXT_PRIMARY`       |
//! | `Primary` | `GRADIENT_START → END` (AA)  | `WHITE`              |
//! | `Danger`  | `ERROR` flat fill            | `WHITE`              |
//!
//! ## States
//! Normal → Hover (brighter) → Pressed (darker) → Disabled (muted)
//!
//! ## Glow
//! Primary and Danger buttons draw a soft glow halo (`shadows::SOFT`) when
//! in the Normal / Hover state.

extern crate alloc;
use alloc::string::String;

use crate::widget::WidgetEvent;
use atom_theme::colors::*;
use atom_theme::radius;
use atom_theme::shadows;
use libgui::color::Color;
use libgui::event::{MouseButton, MouseEvent};
use libgui::font::{FONT_HEIGHT, FONT_WIDTH};
use libgui::surface::Surface;

/// Visual style of the button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// Neutral secondary action.
    Default,
    /// Primary / affirmative action — gradient fill + glow.
    Primary,
    /// Destructive action — solid error colour.
    Danger,
}

/// Interaction state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hover,
    Pressed,
    Disabled,
}

/// A labelled push-button.
pub struct Button {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub label: String,
    pub style: ButtonStyle,
    pub state: ButtonState,
}

impl Button {
    // ── Constructors ──────────────────────────────────────────────────────

    /// Create a Default-style button.
    pub fn new(x: u32, y: u32, width: u32, height: u32, label: impl Into<String>) -> Self {
        Self {
            x,
            y,
            width,
            height,
            label: label.into(),
            style: ButtonStyle::Default,
            state: ButtonState::Normal,
        }
    }

    /// Create a Primary-style button (gradient + glow).
    pub fn primary(x: u32, y: u32, width: u32, height: u32, label: impl Into<String>) -> Self {
        Self {
            x,
            y,
            width,
            height,
            label: label.into(),
            style: ButtonStyle::Primary,
            state: ButtonState::Normal,
        }
    }

    /// Create a Danger-style button.
    pub fn danger(x: u32, y: u32, width: u32, height: u32, label: impl Into<String>) -> Self {
        Self {
            x,
            y,
            width,
            height,
            label: label.into(),
            style: ButtonStyle::Danger,
            state: ButtonState::Normal,
        }
    }

    /// Set the button as disabled.
    pub fn disabled(mut self) -> Self {
        self.state = ButtonState::Disabled;
        self
    }

    // ── Hit-test ─────────────────────────────────────────────────────────

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.width) as i32
            && y < (self.y + self.height) as i32
    }

    // ── Input handling ────────────────────────────────────────────────────

    /// Update state from a mouse event.  Returns `WidgetEvent::Clicked` when
    /// a left-button press+release sequence completes inside the button.
    pub fn handle_mouse(&mut self, event: &MouseEvent) -> WidgetEvent {
        if self.state == ButtonState::Disabled {
            return WidgetEvent::None;
        }

        match event {
            MouseEvent::Move { x, y, .. } => {
                let inside = self.contains(*x, *y);
                self.state = match self.state {
                    ButtonState::Pressed => ButtonState::Pressed,
                    _ if inside => ButtonState::Hover,
                    _ => ButtonState::Normal,
                };
                WidgetEvent::None
            }
            MouseEvent::ButtonDown {
                button: MouseButton::Left,
                x,
                y,
            } => {
                if self.contains(*x, *y) {
                    self.state = ButtonState::Pressed;
                }
                WidgetEvent::None
            }
            MouseEvent::ButtonUp {
                button: MouseButton::Left,
                x,
                y,
            } => {
                let inside = self.contains(*x, *y);
                let was_down = self.state == ButtonState::Pressed;
                self.state = if inside {
                    ButtonState::Hover
                } else {
                    ButtonState::Normal
                };
                if was_down && inside {
                    WidgetEvent::Clicked
                } else {
                    WidgetEvent::None
                }
            }
            _ => WidgetEvent::None,
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Draw the button into `surface`.
    pub fn draw(&self, surface: &mut Surface) {
        let r = radius::SM; // 8 px — DS default for buttons

        match self.style {
            ButtonStyle::Default => self.draw_default(surface, r),
            ButtonStyle::Primary => self.draw_primary(surface, r),
            ButtonStyle::Danger => self.draw_danger(surface, r),
        }
    }

    fn draw_default(&self, surface: &mut Surface, r: u32) {
        let (bg, label_color, border_color) = match self.state {
            ButtonState::Normal => (
                ATOM_COLOR_SURFACE_ALT,
                ATOM_COLOR_TEXT_PRIMARY,
                ATOM_COLOR_BORDER,
            ),
            ButtonState::Hover => (
                ATOM_COLOR_WIN_HDR,
                ATOM_COLOR_TEXT_PRIMARY,
                ATOM_COLOR_ACCENT,
            ),
            ButtonState::Pressed => (ATOM_COLOR_BG, ATOM_COLOR_TEXT_SECONDARY, ATOM_COLOR_BORDER),
            ButtonState::Disabled => (ATOM_COLOR_SURFACE, ATOM_COLOR_TEXT_MUTED, ATOM_COLOR_BORDER),
        };

        // Background
        let bg_c: Color = bg.into();
        surface.fill_rect_rounded_aa(self.x, self.y, self.width, self.height, r, bg_c);

        // Border
        let border_c: Color = border_color.into();
        surface.draw_rect_rounded_aa(self.x, self.y, self.width, self.height, r, border_c);

        // Label (centred)
        self.draw_label(surface, label_color.into());
    }

    fn draw_primary(&self, surface: &mut Surface, r: u32) {
        let spec = shadows::SOFT;

        // Glow halo (only for Normal / Hover)
        if self.state == ButtonState::Normal || self.state == ButtonState::Hover {
            let glow_color: Color = ATOM_COLOR_ACCENT.into();
            surface.draw_shadow_layers(
                self.x as i32,
                self.y as i32,
                self.width,
                self.height,
                r,
                glow_color,
                spec.offset_y,
                spec.layers,
                spec.alpha,
            );
        }

        // Gradient fill
        let (start, end) = match self.state {
            ButtonState::Normal | ButtonState::Hover => (
                ATOM_GRADIENT_PRIMARY_START.into(),
                ATOM_GRADIENT_PRIMARY_END.into(),
            ),
            ButtonState::Pressed => (Color::rgb(0x38, 0x6A, 0xCC), Color::rgb(0x5C, 0x42, 0xCC)),
            ButtonState::Disabled => (
                Color::from(ATOM_COLOR_SURFACE_ALT),
                Color::from(ATOM_COLOR_SURFACE_ALT),
            ),
        };
        surface.fill_rect_rounded_aa_gradient_v(
            self.x,
            self.y,
            self.width,
            self.height,
            r,
            start,
            end,
        );

        // Accent border on hover
        if self.state == ButtonState::Hover {
            let bc: Color = ATOM_COLOR_ACCENT_GLOW.into();
            surface.draw_rect_rounded_aa(self.x, self.y, self.width, self.height, r, bc);
        }

        // Label
        let label_color = if self.state == ButtonState::Disabled {
            ATOM_COLOR_TEXT_MUTED.into()
        } else {
            Color::WHITE
        };
        self.draw_label(surface, label_color);
    }

    fn draw_danger(&self, surface: &mut Surface, r: u32) {
        // All arms must be the same type (atom_syscall::Color).
        // We convert to libgui::Color at the draw call below.
        use atom_syscall::graphics::Color as SysColor;
        let (bg, alpha) = match self.state {
            ButtonState::Normal => (ATOM_COLOR_ERROR, 220u8),
            ButtonState::Hover => (ATOM_COLOR_ERROR, 255u8),
            ButtonState::Pressed => (SysColor::new(0xBB, 0x33, 0x33), 255u8),
            ButtonState::Disabled => (ATOM_COLOR_SURFACE_ALT, 255u8),
        };

        // Glow on hover
        if self.state == ButtonState::Hover {
            let spec = shadows::SOFT;
            let gc: Color = ATOM_COLOR_ERROR.into();
            surface.draw_shadow_layers(
                self.x as i32,
                self.y as i32,
                self.width,
                self.height,
                r,
                gc,
                spec.offset_y,
                spec.layers,
                spec.alpha,
            );
        }

        // Background
        let bg_c: Color = bg.into();
        if alpha < 255 {
            surface.fill_rect_rounded_alpha(
                self.x,
                self.y,
                self.width,
                self.height,
                r,
                bg_c,
                alpha,
            );
        } else {
            surface.fill_rect_rounded_aa(self.x, self.y, self.width, self.height, r, bg_c);
        }

        // Label
        let label_color = if self.state == ButtonState::Disabled {
            ATOM_COLOR_TEXT_MUTED.into()
        } else {
            Color::WHITE
        };
        self.draw_label(surface, label_color);
    }

    /// Draw the button label centred inside the button rect.
    fn draw_label(&self, surface: &mut Surface, fg: Color) {
        let text_w = self.label.len() as u32 * FONT_WIDTH;
        let text_h = FONT_HEIGHT;
        let tx = self.x.saturating_add(self.width.saturating_sub(text_w) / 2);
        let ty = self
            .y
            .saturating_add(self.height.saturating_sub(text_h) / 2);
        // Use SURFACE bg so glyph background matches button fill at the seam;
        // this is acceptable with the current draw_string API.
        let bg = Color::rgba(0, 0, 0, 0); // transparent intent (draw_string uses solid bg)
        surface.draw_string(tx, ty, &self.label, fg, bg);
    }
}
