// Event Types
//
// Events are delivered to applications by the desktop environment.
// All coordinates are surface-relative - applications do not know
// their screen position.

/// Input and window events delivered to applications
#[derive(Debug, Clone)]
pub enum Event {
    /// Keyboard key pressed
    KeyPress(KeyEvent),
    /// Keyboard key released
    KeyRelease(KeyEvent),
    /// Mouse moved within surface
    MouseMove(MouseEvent),
    /// Mouse button pressed
    MouseButtonPress(MouseButtonEvent),
    /// Mouse button released
    MouseButtonRelease(MouseButtonEvent),
    /// Mouse scroll wheel
    MouseScroll(MouseScrollEvent),
    /// Surface gained focus
    FocusIn(FocusEvent),
    /// Surface lost focus
    FocusOut(FocusEvent),
    /// Surface was resized by desktop environment
    Resize(ResizeEvent),
    /// Close request from desktop environment
    CloseRequested,
    /// Redraw request
    Redraw,
}

/// Keyboard event data
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    /// Raw scancode
    pub scancode: u8,
    /// ASCII character (0 if not printable)
    pub ascii: char,
    /// Shift modifier held
    pub shift: bool,
    /// Ctrl modifier held
    pub ctrl: bool,
    /// Alt modifier held
    pub alt: bool,
    /// Is this a repeat event
    pub repeat: bool,
}

impl KeyEvent {
    pub fn new(scancode: u8, ascii: char, shift: bool, ctrl: bool, alt: bool) -> Self {
        Self {
            scancode,
            ascii,
            shift,
            ctrl,
            alt,
            repeat: false,
        }
    }

    /// Check if this is a printable character
    pub fn is_printable(&self) -> bool {
        self.ascii >= ' ' && self.ascii <= '~'
    }

    /// Check if this is a control character
    pub fn is_control(&self) -> bool {
        self.ctrl && self.ascii >= 'a' && self.ascii <= 'z'
    }
}

/// Mouse movement event data
///
/// All coordinates are relative to the surface, not the screen.
/// Applications do not know where they are positioned on screen.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    /// X position within surface
    pub x: i32,
    /// Y position within surface
    pub y: i32,
    /// Delta X (relative movement)
    pub dx: i32,
    /// Delta Y (relative movement)
    pub dy: i32,
    /// Left button held
    pub left_button: bool,
    /// Right button held
    pub right_button: bool,
    /// Middle button held
    pub middle_button: bool,
}

impl MouseEvent {
    pub fn new(x: i32, y: i32, dx: i32, dy: i32) -> Self {
        Self {
            x,
            y,
            dx,
            dy,
            left_button: false,
            right_button: false,
            middle_button: false,
        }
    }

    /// Check if any button is pressed
    pub fn any_button(&self) -> bool {
        self.left_button || self.right_button || self.middle_button
    }
}

/// Mouse button event data
#[derive(Debug, Clone, Copy)]
pub struct MouseButtonEvent {
    /// Button that was pressed/released
    pub button: MouseButton,
    /// X position within surface
    pub x: i32,
    /// Y position within surface
    pub y: i32,
}

impl MouseButtonEvent {
    pub fn new(button: MouseButton, x: i32, y: i32) -> Self {
        Self { button, x, y }
    }
}

/// Mouse button identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

impl MouseButton {
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => MouseButton::Left,
            1 => MouseButton::Right,
            2 => MouseButton::Middle,
            _ => MouseButton::Left,
        }
    }
}

/// Mouse scroll event data
#[derive(Debug, Clone, Copy)]
pub struct MouseScrollEvent {
    /// Horizontal scroll amount
    pub delta_x: i32,
    /// Vertical scroll amount
    pub delta_y: i32,
    /// X position within surface
    pub x: i32,
    /// Y position within surface
    pub y: i32,
}

/// Focus change event
#[derive(Debug, Clone, Copy)]
pub struct FocusEvent {
    /// Whether the surface now has focus
    pub focused: bool,
}

impl FocusEvent {
    pub fn gained() -> Self {
        Self { focused: true }
    }

    pub fn lost() -> Self {
        Self { focused: false }
    }
}

/// Surface resize event
///
/// The desktop environment has changed the surface size.
/// Applications must update their drawing accordingly.
#[derive(Debug, Clone, Copy)]
pub struct ResizeEvent {
    /// New width
    pub width: u32,
    /// New height
    pub height: u32,
}

impl ResizeEvent {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// Event queue for polling events
pub struct EventQueue {
    events: alloc::collections::VecDeque<Event>,
}

extern crate alloc;
use alloc::collections::VecDeque;

impl EventQueue {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
        }
    }

    /// Push an event to the queue
    pub fn push(&mut self, event: Event) {
        self.events.push_back(event);
    }

    /// Pop an event from the queue
    pub fn pop(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Check if there are pending events
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get the number of pending events
    pub fn len(&self) -> usize {
        self.events.len()
    }
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}
