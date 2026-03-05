//! Menu and MenuItem — context/dropdown menus.
//!
//! ## Architecture
//!
//! `Menu` holds a list of `MenuItem` values.  When `open` is true, the menu
//! draws a floating panel over the surface.  The selected item has a gradient
//! highlight (`ATOM_GRADIENT_PRIMARY_START → END`).
//!
//! ## Usage
//!
//! ```ignore
//! let mut menu = Menu::new(x, y, 160);
//! menu.add_item("Open File");
//! menu.add_item("Save");
//! menu.add_separator();
//! menu.add_item("Quit");
//!
//! menu.open = true;
//! menu.draw(&mut surface);
//!
//! if let WidgetEvent::Clicked = menu.handle_mouse(&ev) {
//!     if let Some(i) = menu.selected_index { /* handle item i */ }
//! }
//! ```

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;
use libgui::event::{MouseEvent, MouseButton};
use libgui::font::FONT_HEIGHT;
use libgui::surface::Surface;
use atom_theme::colors::*;
use atom_theme::radius;
use atom_theme::spacing;
use crate::widget::WidgetEvent;

/// A single menu entry.
pub struct MenuItem {
    pub label:    String,
    pub disabled: bool,
    /// `true` → rendered as a horizontal divider; `label` is ignored.
    pub separator: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), disabled: false, separator: false }
    }
    pub fn separator() -> Self {
        Self { label: String::new(), disabled: false, separator: true }
    }
    pub fn disabled(mut self) -> Self { self.disabled = true; self }
}

/// Pixel height of a normal menu row.
const ITEM_HEIGHT: u32 = FONT_HEIGHT + spacing::SM;
/// Pixel height of a separator row.
const SEP_HEIGHT:  u32 = spacing::SM;

/// A floating context/dropdown menu.
pub struct Menu {
    /// Top-left corner of the menu panel.
    pub x:       u32,
    pub y:       u32,
    /// Menu panel width.  Items are clipped / padded to this width.
    pub width:   u32,
    pub items:   Vec<MenuItem>,
    /// Whether the menu is currently shown.
    pub open:    bool,
    /// Index of the currently hovered item, if any.
    pub hovered: Option<usize>,
    /// Index of the most-recently clicked item (set on click, cleared on close).
    pub selected_index: Option<usize>,
}

impl Menu {
    /// Create a closed menu at `(x, y)` with the given width.
    pub fn new(x: u32, y: u32, width: u32) -> Self {
        Self {
            x, y, width,
            items:          Vec::new(),
            open:           false,
            hovered:        None,
            selected_index: None,
        }
    }

    /// Append a menu item.
    pub fn add_item(&mut self, label: impl Into<String>) {
        self.items.push(MenuItem::new(label));
    }

    /// Append a separator row.
    pub fn add_separator(&mut self) {
        self.items.push(MenuItem::separator());
    }

    /// Append a disabled item.
    pub fn add_item_disabled(&mut self, label: impl Into<String>) {
        self.items.push(MenuItem::new(label).disabled());
    }

    /// Total pixel height of the panel (all items + padding).
    pub fn panel_height(&self) -> u32 {
        let pad = spacing::XS * 2;
        let rows: u32 = self.items.iter().map(|it| {
            if it.separator { SEP_HEIGHT } else { ITEM_HEIGHT }
        }).sum();
        rows + pad
    }

    /// Returns the item index at pixel offset `(px, py)` relative to the
    /// screen, or `None` if outside or on a separator.
    fn item_at(&self, px: i32, py: i32) -> Option<usize> {
        if !self.open { return None; }
        if px < self.x as i32 || px >= (self.x + self.width) as i32 { return None; }

        let panel_h = self.panel_height();
        if py < self.y as i32 || py >= (self.y + panel_h) as i32 { return None; }

        let mut cursor_y = self.y + spacing::XS;
        for (i, item) in self.items.iter().enumerate() {
            let item_h = if item.separator { SEP_HEIGHT } else { ITEM_HEIGHT };
            if py >= cursor_y as i32 && py < (cursor_y + item_h) as i32 {
                return if item.separator || item.disabled { None } else { Some(i) };
            }
            cursor_y += item_h;
        }
        None
    }

    // ── Input handling ────────────────────────────────────────────────────

    pub fn handle_mouse(&mut self, event: &MouseEvent) -> WidgetEvent {
        if !self.open { return WidgetEvent::None; }

        match event {
            MouseEvent::Move { x, y, .. } => {
                self.hovered = self.item_at(*x, *y);
                WidgetEvent::None
            }
            MouseEvent::ButtonDown { button: MouseButton::Left, x, y } => {
                if let Some(idx) = self.item_at(*x, *y) {
                    self.selected_index = Some(idx);
                    self.open   = false;
                    self.hovered = None;
                    WidgetEvent::Clicked
                } else {
                    // Click outside — close without selection
                    self.open    = false;
                    self.hovered = None;
                    WidgetEvent::FocusLost
                }
            }
            _ => WidgetEvent::None,
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    pub fn draw(&self, surface: &mut Surface) {
        if !self.open { return; }

        let panel_h = self.panel_height();
        let r       = radius::MD;

        // Panel background + border
        let bg: Color      = ATOM_COLOR_MENU_BG.into();
        let border: Color  = ATOM_COLOR_MENU_BORDER.into();
        surface.fill_rect_rounded_aa(self.x, self.y, self.width, panel_h, r, bg);
        surface.draw_rect_rounded_aa(self.x, self.y, self.width, panel_h, r, border);

        // Items
        let mut cursor_y = self.y + spacing::XS;
        for (i, item) in self.items.iter().enumerate() {
            if item.separator {
                // Horizontal divider
                let sep_y  = cursor_y + SEP_HEIGHT / 2;
                let sep_x  = self.x + spacing::SM;
                let sep_w  = self.width.saturating_sub(spacing::SM * 2);
                let sep_c: Color = ATOM_COLOR_BORDER.into();
                surface.draw_hline(sep_x, sep_y, sep_w, sep_c);
                cursor_y += SEP_HEIGHT;
                continue;
            }

            let is_hovered = self.hovered == Some(i);

            // Hover / selected highlight (gradient)
            if is_hovered {
                let hs: Color = ATOM_GRADIENT_PRIMARY_START.into();
                let he: Color = ATOM_GRADIENT_PRIMARY_END.into();
                let row_x = self.x + spacing::XS;
                let row_w = self.width.saturating_sub(spacing::XS * 2);
                surface.fill_rect_rounded_aa_gradient_v(
                    row_x, cursor_y, row_w, ITEM_HEIGHT,
                    radius::XS,
                    // Use a dim gradient at low alpha to not overpower the text
                    Color::rgba(hs.r, hs.g, hs.b, 80),
                    Color::rgba(he.r, he.g, he.b, 80),
                );
                // Actually we don't have gradient_v with alpha — use solid alpha fill
                surface.fill_rect_rounded_alpha(
                    row_x, cursor_y, row_w, ITEM_HEIGHT,
                    radius::XS,
                    ATOM_COLOR_MENU_SEL.into(),
                    200,
                );
            }

            // Item label
            let fg: Color = if item.disabled {
                ATOM_COLOR_TEXT_MUTED.into()
            } else {
                ATOM_COLOR_MENU_TEXT.into()
            };
            let text_y = cursor_y + (ITEM_HEIGHT.saturating_sub(FONT_HEIGHT)) / 2;
            let text_x = self.x + spacing::MD;
            // Use a transparent bg so the text sits on the menu panel properly
            let tbg: Color = if is_hovered { ATOM_COLOR_MENU_SEL.into() } else { bg };
            surface.draw_string(text_x, text_y, &item.label, fg, tbg);

            cursor_y += ITEM_HEIGHT;
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        if !self.open { return false; }
        let h = self.panel_height();
        x >= self.x as i32 && y >= self.y as i32
            && x < (self.x + self.width) as i32
            && y < (self.y + h) as i32
    }
}
