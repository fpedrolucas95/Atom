//! Input — single-line text field with focus border and caret.
//!
//! ## Visual states
//!
//! | State    | Border colour            | Background          |
//! |----------|--------------------------|---------------------|
//! | Normal   | `ATOM_COLOR_BORDER`      | `ATOM_COLOR_SURFACE`|
//! | Focused  | `ATOM_COLOR_ACCENT`      | `ATOM_COLOR_SURFACE`|
//! | Disabled | `ATOM_COLOR_BORDER`      | `ATOM_COLOR_BG`     |
//!
//! When focused a 1 px glow ring is drawn around the border using
//! `fill_rect_alpha` at low alpha.
//!
//! The caret is a 1×FONT_HEIGHT rectangle in `ATOM_COLOR_ACCENT`, shown only
//! when the field is focused.  Caret blinking is driven externally — call
//! `set_caret_visible(bool)` from a timer callback.

extern crate alloc;
use alloc::string::String;

use libgui::color::Color;
use libgui::event::{KeyEvent, MouseEvent, MouseButton};
use libgui::font::{FONT_WIDTH, FONT_HEIGHT};
use libgui::surface::Surface;
use atom_theme::colors::*;
use atom_theme::radius;
use atom_theme::spacing;
use crate::widget::WidgetEvent;

/// Single-line text input field.
pub struct Input {
    pub x:       u32,
    pub y:       u32,
    pub width:   u32,
    pub height:  u32,
    pub text:    String,
    /// Placeholder hint shown when `text` is empty and field is unfocused.
    pub placeholder: String,
    pub focused:     bool,
    pub disabled:    bool,
    /// Cursor position (byte index into `text`).
    pub cursor:      usize,
    /// Whether the caret glyph is currently visible (for blink).
    pub caret_visible: bool,
    /// Horizontal scroll offset in pixels (for overflow).
    pub scroll_x: u32,
}

impl Input {
    /// Create an input field.
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x, y, width, height,
            text: String::new(),
            placeholder: String::new(),
            focused: false,
            disabled: false,
            cursor: 0,
            caret_visible: true,
            scroll_x: 0,
        }
    }

    /// Set placeholder hint text.
    pub fn placeholder(mut self, s: impl Into<String>) -> Self {
        self.placeholder = s.into(); self
    }

    /// Set the current text value.
    pub fn with_text(mut self, s: impl Into<String>) -> Self {
        let t = s.into();
        self.cursor = t.len();
        self.text = t;
        self
    }

    /// Mark as disabled.
    pub fn disabled(mut self) -> Self { self.disabled = true; self }

    /// Toggle caret visibility (call from a ~500 ms timer for blinking).
    pub fn set_caret_visible(&mut self, v: bool) { self.caret_visible = v; }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.width)  as i32
            && y < (self.y + self.height) as i32
    }

    // ── Input handling ────────────────────────────────────────────────────

    pub fn handle_mouse(&mut self, event: &MouseEvent) -> WidgetEvent {
        if self.disabled { return WidgetEvent::None; }
        match event {
            MouseEvent::ButtonDown { button: MouseButton::Left, x, y } => {
                let was_focused = self.focused;
                self.focused = self.contains(*x, *y);
                if self.focused && !was_focused {
                    self.cursor = self.text.len();
                    return WidgetEvent::FocusGained;
                }
                if !self.focused && was_focused {
                    return WidgetEvent::FocusLost;
                }
                WidgetEvent::None
            }
            _ => WidgetEvent::None,
        }
    }

    /// Process a key event when the field is focused.
    /// Returns `WidgetEvent::ValueChanged` if the text was modified.
    pub fn handle_key(&mut self, event: &KeyEvent) -> WidgetEvent {
        if !self.focused || self.disabled || !event.pressed { return WidgetEvent::None; }

        let ch = event.character;

        // Backspace
        if event.scancode == 0x0E || ch == 0x08 {
            if self.cursor > 0 && !self.text.is_empty() {
                // Remove the byte before cursor (ASCII-only safe)
                self.cursor -= 1;
                self.text.remove(self.cursor);
                self.update_scroll();
                return WidgetEvent::ValueChanged;
            }
            return WidgetEvent::None;
        }

        // Delete
        if event.scancode == 0x53 {
            if self.cursor < self.text.len() {
                self.text.remove(self.cursor);
                self.update_scroll();
                return WidgetEvent::ValueChanged;
            }
            return WidgetEvent::None;
        }

        // Left arrow
        if event.scancode == 0x4B {
            if self.cursor > 0 { self.cursor -= 1; }
            self.update_scroll();
            return WidgetEvent::None;
        }

        // Right arrow
        if event.scancode == 0x4D {
            if self.cursor < self.text.len() { self.cursor += 1; }
            self.update_scroll();
            return WidgetEvent::None;
        }

        // Home
        if event.scancode == 0x47 { self.cursor = 0; self.update_scroll(); return WidgetEvent::None; }
        // End
        if event.scancode == 0x4F { self.cursor = self.text.len(); self.update_scroll(); return WidgetEvent::None; }

        // Printable ASCII
        if ch >= 0x20 && ch < 0x7F {
            self.text.insert(self.cursor, ch as char);
            self.cursor += 1;
            self.update_scroll();
            return WidgetEvent::ValueChanged;
        }

        WidgetEvent::None
    }

    /// Adjust `scroll_x` so the cursor stays visible.
    fn update_scroll(&mut self) {
        let inner_w = self.inner_width();
        let caret_x = self.cursor as u32 * FONT_WIDTH;
        if caret_x < self.scroll_x {
            self.scroll_x = caret_x;
        } else if caret_x + FONT_WIDTH > self.scroll_x + inner_w {
            self.scroll_x = caret_x + FONT_WIDTH - inner_w;
        }
    }

    fn inner_width(&self) -> u32 {
        self.width.saturating_sub(spacing::SM * 2)
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    pub fn draw(&self, surface: &mut Surface) {
        let r = radius::SM;

        // Background
        let bg: Color = if self.disabled {
            ATOM_COLOR_BG.into()
        } else {
            ATOM_COLOR_SURFACE.into()
        };
        surface.fill_rect_rounded_aa(self.x, self.y, self.width, self.height, r, bg);

        // Focus glow ring (drawn before border)
        if self.focused && !self.disabled {
            let glow: Color = ATOM_COLOR_ACCENT.into();
            surface.fill_rect_rounded_alpha(
                self.x.saturating_sub(2),
                self.y.saturating_sub(2),
                self.width + 4, self.height + 4,
                r + 2, glow, 35,
            );
        }

        // Border
        let border: Color = if self.focused && !self.disabled {
            ATOM_COLOR_ACCENT.into()
        } else {
            ATOM_COLOR_BORDER.into()
        };
        surface.draw_rect_rounded_aa(self.x, self.y, self.width, self.height, r, border);

        // Text or placeholder
        let pad = spacing::SM;
        let tx = self.x + pad;
        let ty = self.y + (self.height.saturating_sub(FONT_HEIGHT)) / 2;
        let inner_w = self.inner_width();

        if self.text.is_empty() && !self.placeholder.is_empty() && !self.focused {
            let fg: Color = ATOM_COLOR_TEXT_MUTED.into();
            surface.draw_string(tx, ty, &self.placeholder, fg, bg);
        } else if !self.text.is_empty() {
            let fg: Color = if self.disabled {
                ATOM_COLOR_TEXT_MUTED.into()
            } else {
                ATOM_COLOR_TEXT_PRIMARY.into()
            };
            // Clip text to visible area using scroll offset
            let start_char = (self.scroll_x / FONT_WIDTH) as usize;
            let visible_chars = (inner_w / FONT_WIDTH) as usize;
            let text_bytes = self.text.as_bytes();
            if start_char < text_bytes.len() {
                let end_char = (start_char + visible_chars).min(text_bytes.len());
                if let Ok(slice) = core::str::from_utf8(&text_bytes[start_char..end_char]) {
                    surface.draw_string(tx, ty, slice, fg, bg);
                }
            }
        }

        // Caret
        if self.focused && !self.disabled && self.caret_visible {
            let caret_char = self.cursor.saturating_sub((self.scroll_x / FONT_WIDTH) as usize);
            let cx = tx + caret_char as u32 * FONT_WIDTH;
            if cx < self.x + self.width.saturating_sub(pad) {
                let cc: Color = ATOM_COLOR_ACCENT.into();
                surface.fill_rect(cx, ty, 1, FONT_HEIGHT, cc);
            }
        }
    }
}
