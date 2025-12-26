//! Event Handling
//!
//! Provides event types for input handling in applications.

/// Key event from keyboard
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    /// Scancode from hardware
    pub scancode: u8,
    /// ASCII character (if applicable, 0 if not)
    pub character: u8,
    /// Whether this is a key press (true) or release (false)
    pub pressed: bool,
    /// Modifier keys state
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    /// Check if this key event produced a printable character
    pub fn is_printable(&self) -> bool {
        self.character >= 0x20 && self.character < 0x7F
    }

    /// Get the character as a char, if printable
    pub fn as_char(&self) -> Option<char> {
        if self.is_printable() {
            Some(self.character as char)
        } else {
            None
        }
    }
}

/// Modifier keys state
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps_lock: bool,
}

impl KeyModifiers {
    pub fn from_u8(flags: u8) -> Self {
        Self {
            shift: flags & 0x01 != 0,
            ctrl: flags & 0x02 != 0,
            alt: flags & 0x04 != 0,
            caps_lock: flags & 0x08 != 0,
        }
    }

    pub fn to_u8(&self) -> u8 {
        let mut flags = 0u8;
        if self.shift { flags |= 0x01; }
        if self.ctrl { flags |= 0x02; }
        if self.alt { flags |= 0x04; }
        if self.caps_lock { flags |= 0x08; }
        flags
    }
}

/// Mouse button identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Mouse event types
#[derive(Debug, Clone, Copy)]
pub enum MouseEvent {
    /// Mouse moved to new position
    Move {
        x: i32,
        y: i32,
        dx: i16,
        dy: i16,
    },
    /// Mouse button pressed
    ButtonDown {
        button: MouseButton,
        x: i32,
        y: i32,
    },
    /// Mouse button released
    ButtonUp {
        button: MouseButton,
        x: i32,
        y: i32,
    },
    /// Mouse wheel scrolled
    Scroll {
        delta: i16,
        x: i32,
        y: i32,
    },
}

impl MouseEvent {
    /// Get the current mouse position for this event
    pub fn position(&self) -> (i32, i32) {
        match self {
            MouseEvent::Move { x, y, .. } => (*x, *y),
            MouseEvent::ButtonDown { x, y, .. } => (*x, *y),
            MouseEvent::ButtonUp { x, y, .. } => (*x, *y),
            MouseEvent::Scroll { x, y, .. } => (*x, *y),
        }
    }
}

/// Window event types
#[derive(Debug, Clone, Copy)]
pub enum WindowEvent {
    /// Window was resized
    Resize { width: u32, height: u32 },
    /// Window was moved
    Move { x: i32, y: i32 },
    /// Window gained focus
    Focus,
    /// Window lost focus
    Unfocus,
    /// Window should be closed
    Close,
    /// Area needs redraw
    Expose { x: i32, y: i32, width: u32, height: u32 },
}

/// All possible events an application can receive
#[derive(Debug, Clone)]
pub enum Event {
    /// Keyboard event
    Key(KeyEvent),
    /// Mouse event
    Mouse(MouseEvent),
    /// Window event
    Window(WindowEvent),
    /// Application should redraw
    Redraw,
    /// Application should quit
    Quit,
    /// No event available
    None,
}

impl Event {
    /// Check if this is a keyboard event
    pub fn is_key(&self) -> bool {
        matches!(self, Event::Key(_))
    }

    /// Check if this is a mouse event
    pub fn is_mouse(&self) -> bool {
        matches!(self, Event::Mouse(_))
    }

    /// Check if this is a quit event
    pub fn is_quit(&self) -> bool {
        matches!(self, Event::Quit)
    }
}
