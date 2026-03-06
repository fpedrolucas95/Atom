//! # atom_ui — Atom Design System v1.0 Widget Toolkit
//!
//! Production-ready, `no_std` widget library for Atom OS applications.
//!
//! All widgets render into a `libgui::Surface` using the primitives in
//! `atom_syscall::graphics::SharedSurface` (rounded rects, gradients, glow).
//! Colors, spacing, and radius values come exclusively from `atom_theme` — no
//! hard-coded magic numbers in widget code.
//!
//! ## Component catalogue
//!
//! | Component    | States                              | Notes                         |
//! |-------------|--------------------------------------|-------------------------------|
//! | `Panel`      | —                                   | Background card / container   |
//! | `TextLabel`  | —                                   | Single-line text with DS style|
//! | `Button`     | Normal / Hover / Pressed / Disabled  | Default + Primary + Danger    |
//! | `Input`      | Normal / Focused / Disabled          | Text field with caret         |
//! | `Menu`       | Open / Closed                        | Context / dropdown menu       |
//! | `MenuItem`   | Normal / Hover / Selected / Disabled | Menu row                      |
//! | `ScrollBar`  | Normal / Active                      | Vertical or horizontal        |
//!
//! ## Typography
//!
//! All text currently uses the 8×8 bitmap font via `Surface::draw_string`.
//! The `TypographyStyle` tokens in `atom_theme::typography` record the
//! intended Inter/JetBrains Mono sizes for the future typography phase.
//!
//! ## Usage
//!
//! ```ignore
//! use atom_ui::button::{Button, ButtonStyle};
//! use atom_ui::panel::Panel;
//!
//! let panel = Panel::new(10, 10, 300, 200);
//! let btn   = Button::primary(20, 160, 120, 32, "Apply");
//!
//! // In your render loop:
//! panel.draw(&mut surface);
//! btn.draw(&mut surface);
//!
//! // In your event loop:
//! if btn.handle_mouse(&mouse_event) == WidgetEvent::Clicked { /* … */ }
//! ```

#![no_std]
extern crate alloc;

pub mod widget;
pub mod panel;
pub mod label;
pub mod button;
pub mod input;
pub mod menu;
pub mod scrollbar;

pub use widget::WidgetEvent;
pub use panel::Panel;
pub use label::TextLabel;
pub use button::{Button, ButtonStyle};
pub use input::Input;
pub use menu::{Menu, MenuItem};
pub use scrollbar::ScrollBar;
