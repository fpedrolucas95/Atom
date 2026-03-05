//! Base widget traits and event types.

use libgui::event::MouseEvent;
use libgui::surface::Surface;

/// Events that widgets can emit after processing input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetEvent {
    /// Nothing happened.
    None,
    /// The widget was clicked (button released inside bounds).
    Clicked,
    /// A text field or similar value changed.
    ValueChanged,
    /// The widget gained keyboard focus.
    FocusGained,
    /// The widget lost keyboard focus.
    FocusLost,
}

/// Common interface for all atom_ui widgets.
///
/// Widgets are plain structs; this trait provides polymorphic drawing and
/// hit-testing for callers that want to loop over a heterogeneous collection.
pub trait Widget {
    /// Draw the widget into `surface`.
    fn draw(&self, surface: &mut Surface);
    /// Return `true` if the point `(x, y)` is inside the widget's bounding box.
    fn contains(&self, x: i32, y: i32) -> bool;
    /// Process a mouse event and return any resulting widget event.
    fn handle_mouse(&mut self, event: &MouseEvent) -> WidgetEvent;
}

/// Interactive widget state machine (Normal → Hover → Pressed → Normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractiveState {
    Normal,
    Hover,
    Pressed,
    Disabled,
    Focused,
}

impl Default for InteractiveState {
    fn default() -> Self { Self::Normal }
}
