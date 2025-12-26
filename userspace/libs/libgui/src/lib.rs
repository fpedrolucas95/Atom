// libgui - GUI Library for Atom OS Applications
//
// This library provides applications with an abstract drawing surface
// and an event-based input system. It does NOT expose any API for
// window management - that is the exclusive domain of the desktop environment.
//
// Key Design Principles:
// ----------------------
// 1. Applications receive a Surface to draw on
// 2. Applications receive Events with surface-relative coordinates
// 3. Applications CANNOT:
//    - Create, position, resize, focus, or decorate windows
//    - Access the framebuffer directly
//    - Query screen coordinates or window positions
//    - Capture global input or access other surfaces
//
// The desktop environment:
// - Creates and manages windows
// - Assigns surfaces to applications
// - Routes input events to the correct surface
// - Composites all surfaces to the screen
// - Handles window decorations, focus, z-order
//
// This separation ensures:
// - Consistent UI behavior across all applications
// - Security (applications can't snoop on other windows)
// - Desktop environment has full control over presentation

#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod surface;
pub mod event;
pub mod color;
pub mod drawing;
pub mod font;
pub mod app;

pub use surface::Surface;
pub use event::{Event, KeyEvent, MouseEvent, FocusEvent};
pub use color::Color;
pub use drawing::DrawingContext;
pub use app::Application;
