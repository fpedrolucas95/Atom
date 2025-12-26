//! libgui - GUI Library for Atom OS Applications
//!
//! This library provides an abstract drawing interface for applications
//! running in the Atom desktop environment. Applications do NOT directly
//! manage windows or layout - they render to surfaces assigned by the
//! desktop compositor and receive mapped input events.
//!
//! # Architecture
//!
//! ```text
//! Application
//!     │
//!     ├──> Surface (drawing)
//!     │
//!     └──> Event Loop (input)
//!             │
//!             v
//!     Desktop Environment (compositor)
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use libgui::{Application, Surface, Event};
//!
//! let app = Application::new("My App")?;
//! let surface = app.create_surface(640, 480)?;
//!
//! loop {
//!     match app.poll_event()? {
//!         Event::Key(key) => handle_key(key),
//!         Event::Mouse(mouse) => handle_mouse(mouse),
//!         Event::Redraw => {
//!             surface.clear(Color::BLACK);
//!             surface.draw_text(10, 10, "Hello!", Color::WHITE);
//!             surface.present();
//!         }
//!         Event::Quit => break,
//!     }
//! }
//! ```

#![no_std]

extern crate alloc;

pub mod surface;
pub mod event;
pub mod color;
pub mod font;
pub mod application;

// Re-exports
pub use surface::Surface;
pub use event::{Event, KeyEvent, MouseEvent};
pub use color::Color;
pub use application::Application;
