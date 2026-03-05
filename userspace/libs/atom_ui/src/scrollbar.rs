//! ScrollBar — vertical or horizontal scroll indicator.
//!
//! Minimal implementation: renders a track and a thumb sized proportionally
//! to (visible / total) content.  Dragging is tracked via handle_mouse.
//!
//! ## DS colours
//! - Track: `ATOM_COLOR_SURFACE` (low contrast, recessive)
//! - Thumb: `ATOM_COLOR_BORDER` (normal), `ATOM_COLOR_ACCENT` (hover/active)

use libgui::color::Color;
use libgui::event::{MouseEvent, MouseButton};
use libgui::surface::Surface;
use atom_theme::colors::*;
use atom_theme::radius;
use crate::widget::WidgetEvent;

/// Scroll axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAxis { Vertical, Horizontal }

/// A minimal scroll bar.
pub struct ScrollBar {
    pub x:      u32,
    pub y:      u32,
    pub length: u32,  // height for vertical, width for horizontal
    pub axis:   ScrollAxis,
    /// Total scrollable content size (px).
    pub total:  u32,
    /// Visible area size (px).
    pub visible: u32,
    /// Current scroll position (0 ≤ value ≤ total - visible).
    pub value:  u32,
    /// Whether the thumb is being dragged.
    active: bool,
    drag_start: i32,
    drag_value: u32,
}

/// Scrollbar track width / height.
const TRACK_SIZE: u32 = 8;

impl ScrollBar {
    pub fn vertical(x: u32, y: u32, length: u32) -> Self {
        Self { x, y, length, axis: ScrollAxis::Vertical,
               total: 100, visible: 50, value: 0, active: false,
               drag_start: 0, drag_value: 0 }
    }

    pub fn horizontal(x: u32, y: u32, length: u32) -> Self {
        Self { x, y, length, axis: ScrollAxis::Horizontal,
               total: 100, visible: 50, value: 0, active: false,
               drag_start: 0, drag_value: 0 }
    }

    /// Maximum scrollable value.
    pub fn max_value(&self) -> u32 { self.total.saturating_sub(self.visible) }

    /// Clamp and set scroll value.
    pub fn set_value(&mut self, v: u32) {
        self.value = v.min(self.max_value());
    }

    // ── Thumb geometry ────────────────────────────────────────────────────

    fn thumb_length(&self) -> u32 {
        if self.total == 0 { return self.length; }
        let ratio = self.visible as u64 * self.length as u64 / self.total as u64;
        (ratio as u32).max(TRACK_SIZE * 2).min(self.length)
    }

    fn thumb_offset(&self) -> u32 {
        if self.max_value() == 0 { return 0; }
        let travel = self.length.saturating_sub(self.thumb_length());
        (self.value as u64 * travel as u64 / self.max_value() as u64) as u32
    }

    fn thumb_rect(&self) -> (u32, u32, u32, u32) {
        let tl = self.thumb_length();
        let to = self.thumb_offset();
        match self.axis {
            ScrollAxis::Vertical   => (self.x, self.y + to, TRACK_SIZE, tl),
            ScrollAxis::Horizontal => (self.x + to, self.y, tl, TRACK_SIZE),
        }
    }

    fn thumb_contains(&self, px: i32, py: i32) -> bool {
        let (tx, ty, tw, th) = self.thumb_rect();
        px >= tx as i32 && py >= ty as i32
            && px < (tx + tw) as i32 && py < (ty + th) as i32
    }

    // ── Input handling ────────────────────────────────────────────────────

    pub fn handle_mouse(&mut self, event: &MouseEvent) -> WidgetEvent {
        match event {
            MouseEvent::ButtonDown { button: MouseButton::Left, x, y } => {
                if self.thumb_contains(*x, *y) {
                    self.active     = true;
                    self.drag_start = if self.axis == ScrollAxis::Vertical { *y } else { *x };
                    self.drag_value = self.value;
                }
                WidgetEvent::None
            }
            MouseEvent::ButtonUp { button: MouseButton::Left, .. } => {
                self.active = false;
                WidgetEvent::None
            }
            MouseEvent::Move { x, y, .. } => {
                if !self.active { return WidgetEvent::None; }
                let cursor  = if self.axis == ScrollAxis::Vertical { *y } else { *x };
                let delta   = cursor - self.drag_start;
                let travel  = self.length.saturating_sub(self.thumb_length());
                if travel == 0 { return WidgetEvent::None; }
                let dv = (delta as i64 * self.max_value() as i64) / travel as i64;
                let new_v = (self.drag_value as i64 + dv).max(0) as u32;
                self.set_value(new_v);
                WidgetEvent::ValueChanged
            }
            MouseEvent::Scroll { delta, x, y } => {
                let _ = (x, y);
                let step = self.visible / 5;
                if *delta < 0 {
                    self.set_value(self.value + step);
                } else {
                    self.set_value(self.value.saturating_sub(step));
                }
                WidgetEvent::ValueChanged
            }
            _ => WidgetEvent::None,
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    pub fn draw(&self, surface: &mut Surface) {
        let r = radius::XS;

        // Track
        let track_c: Color = ATOM_COLOR_SURFACE.into();
        match self.axis {
            ScrollAxis::Vertical =>
                surface.fill_rect_rounded_aa(self.x, self.y, TRACK_SIZE, self.length, r, track_c),
            ScrollAxis::Horizontal =>
                surface.fill_rect_rounded_aa(self.x, self.y, self.length, TRACK_SIZE, r, track_c),
        }

        // Thumb
        let (tx, ty, tw, th) = self.thumb_rect();
        let thumb_c: Color = if self.active { ATOM_COLOR_ACCENT.into() } else { ATOM_COLOR_BORDER.into() };
        surface.fill_rect_rounded_aa(tx, ty, tw, th, r, thumb_c);
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        match self.axis {
            ScrollAxis::Vertical =>
                x >= self.x as i32 && y >= self.y as i32
                && x < (self.x + TRACK_SIZE) as i32 && y < (self.y + self.length) as i32,
            ScrollAxis::Horizontal =>
                x >= self.x as i32 && y >= self.y as i32
                && x < (self.x + self.length) as i32 && y < (self.y + TRACK_SIZE) as i32,
        }
    }
}
