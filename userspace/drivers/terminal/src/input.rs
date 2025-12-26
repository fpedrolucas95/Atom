// Terminal Input Handling Module
//
// This module handles keyboard input for the terminal.
// It polls the kernel's input buffer via syscalls, translates
// scancodes to characters, and manages modifier key state.
// All input comes through the userspace input service, not direct hardware access.

use atom_syscall::input::{keyboard_poll, scancodes};

/// Key events produced by the input handler
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEvent {
    /// Printable character with modifiers
    Char(char),
    /// Control + character (Ctrl+A = 1, Ctrl+C = 3, etc.)
    Control(char),
    /// Alt + character
    Alt(char),
    /// Enter/Return key
    Enter,
    /// Backspace key
    Backspace,
    /// Tab key
    Tab,
    /// Escape key
    Escape,
    /// Delete key
    Delete,
    /// Arrow keys
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    /// Home key
    Home,
    /// End key
    End,
    /// Page Up/Down
    PageUp,
    PageDown,
    /// Function keys F1-F12
    Function(u8),
    /// Insert key
    Insert,
}

/// Keyboard input state machine
pub struct InputHandler {
    // Modifier states
    shift_left: bool,
    shift_right: bool,
    ctrl: bool,
    alt: bool,
    caps_lock: bool,

    // Extended scancode handling
    extended: bool,
}

impl InputHandler {
    pub const fn new() -> Self {
        Self {
            shift_left: false,
            shift_right: false,
            ctrl: false,
            alt: false,
            caps_lock: false,
            extended: false,
        }
    }

    /// Check if shift is currently pressed
    pub fn shift(&self) -> bool {
        self.shift_left || self.shift_right
    }

    /// Check if ctrl is currently pressed
    pub fn ctrl(&self) -> bool {
        self.ctrl
    }

    /// Check if alt is currently pressed
    pub fn alt(&self) -> bool {
        self.alt
    }

    /// Poll for the next key event
    /// Returns None if no key event is available
    pub fn poll(&mut self) -> Option<KeyEvent> {
        while let Some(scancode) = keyboard_poll() {
            if let Some(event) = self.process_scancode(scancode) {
                return Some(event);
            }
        }
        None
    }

    /// Process a raw scancode and potentially produce a key event
    pub fn process_scancode(&mut self, scancode: u8) -> Option<KeyEvent> {
        // Handle extended prefix (0xE0)
        if scancode == scancodes::EXTENDED_PREFIX {
            self.extended = true;
            return None;
        }

        let is_extended = self.extended;
        self.extended = false;

        let is_release = (scancode & 0x80) != 0;
        let code = scancode & 0x7F;

        // Handle extended scancodes (arrow keys, etc.)
        if is_extended {
            return self.process_extended_scancode(code, is_release);
        }

        // Handle modifier keys
        match code {
            0x2A => {
                // Left Shift
                self.shift_left = !is_release;
                return None;
            }
            0x36 => {
                // Right Shift
                self.shift_right = !is_release;
                return None;
            }
            0x1D => {
                // Ctrl
                self.ctrl = !is_release;
                return None;
            }
            0x38 => {
                // Alt
                self.alt = !is_release;
                return None;
            }
            0x3A => {
                // Caps Lock (toggle on press)
                if !is_release {
                    self.caps_lock = !self.caps_lock;
                }
                return None;
            }
            _ => {}
        }

        // Only produce events on key press, not release
        if is_release {
            return None;
        }

        // Handle special keys
        match code {
            0x01 => return Some(KeyEvent::Escape),
            0x0E => return Some(KeyEvent::Backspace),
            0x0F => return Some(KeyEvent::Tab),
            0x1C => return Some(KeyEvent::Enter),
            // Function keys
            0x3B => return Some(KeyEvent::Function(1)),
            0x3C => return Some(KeyEvent::Function(2)),
            0x3D => return Some(KeyEvent::Function(3)),
            0x3E => return Some(KeyEvent::Function(4)),
            0x3F => return Some(KeyEvent::Function(5)),
            0x40 => return Some(KeyEvent::Function(6)),
            0x41 => return Some(KeyEvent::Function(7)),
            0x42 => return Some(KeyEvent::Function(8)),
            0x43 => return Some(KeyEvent::Function(9)),
            0x44 => return Some(KeyEvent::Function(10)),
            0x57 => return Some(KeyEvent::Function(11)),
            0x58 => return Some(KeyEvent::Function(12)),
            _ => {}
        }

        // Translate to character
        if let Some(ch) = self.translate_scancode(code) {
            if self.ctrl {
                // Ctrl + letter produces control characters (Ctrl+A = 1, Ctrl+C = 3, etc.)
                let ctrl_char = if ch.is_ascii_alphabetic() {
                    ((ch.to_ascii_lowercase() as u8) - b'a' + 1) as char
                } else {
                    ch
                };
                return Some(KeyEvent::Control(ctrl_char));
            } else if self.alt {
                return Some(KeyEvent::Alt(ch));
            } else {
                return Some(KeyEvent::Char(ch));
            }
        }

        None
    }

    /// Process extended scancodes (0xE0 prefix)
    fn process_extended_scancode(&mut self, code: u8, is_release: bool) -> Option<KeyEvent> {
        // Only produce events on key press
        if is_release {
            return None;
        }

        match code {
            0x48 => Some(KeyEvent::ArrowUp),
            0x50 => Some(KeyEvent::ArrowDown),
            0x4B => Some(KeyEvent::ArrowLeft),
            0x4D => Some(KeyEvent::ArrowRight),
            0x47 => Some(KeyEvent::Home),
            0x4F => Some(KeyEvent::End),
            0x49 => Some(KeyEvent::PageUp),
            0x51 => Some(KeyEvent::PageDown),
            0x52 => Some(KeyEvent::Insert),
            0x53 => Some(KeyEvent::Delete),
            // Extended Ctrl (right ctrl)
            0x1D => {
                self.ctrl = true;
                None
            }
            // Extended Alt (right alt / AltGr)
            0x38 => {
                self.alt = true;
                None
            }
            _ => None,
        }
    }

    /// Translate a scancode to its corresponding character
    fn translate_scancode(&self, code: u8) -> Option<char> {
        let shift = self.shift();
        let caps = self.caps_lock;

        // Basic scancode to character mapping (US keyboard layout)
        let ch = match code {
            // Number row
            0x02 => Some(if shift { '!' } else { '1' }),
            0x03 => Some(if shift { '@' } else { '2' }),
            0x04 => Some(if shift { '#' } else { '3' }),
            0x05 => Some(if shift { '$' } else { '4' }),
            0x06 => Some(if shift { '%' } else { '5' }),
            0x07 => Some(if shift { '^' } else { '6' }),
            0x08 => Some(if shift { '&' } else { '7' }),
            0x09 => Some(if shift { '*' } else { '8' }),
            0x0A => Some(if shift { '(' } else { '9' }),
            0x0B => Some(if shift { ')' } else { '0' }),
            0x0C => Some(if shift { '_' } else { '-' }),
            0x0D => Some(if shift { '+' } else { '=' }),

            // Top row (QWERTY)
            0x10 => Some(self.letter('q', shift, caps)),
            0x11 => Some(self.letter('w', shift, caps)),
            0x12 => Some(self.letter('e', shift, caps)),
            0x13 => Some(self.letter('r', shift, caps)),
            0x14 => Some(self.letter('t', shift, caps)),
            0x15 => Some(self.letter('y', shift, caps)),
            0x16 => Some(self.letter('u', shift, caps)),
            0x17 => Some(self.letter('i', shift, caps)),
            0x18 => Some(self.letter('o', shift, caps)),
            0x19 => Some(self.letter('p', shift, caps)),
            0x1A => Some(if shift { '{' } else { '[' }),
            0x1B => Some(if shift { '}' } else { ']' }),

            // Home row (ASDF)
            0x1E => Some(self.letter('a', shift, caps)),
            0x1F => Some(self.letter('s', shift, caps)),
            0x20 => Some(self.letter('d', shift, caps)),
            0x21 => Some(self.letter('f', shift, caps)),
            0x22 => Some(self.letter('g', shift, caps)),
            0x23 => Some(self.letter('h', shift, caps)),
            0x24 => Some(self.letter('j', shift, caps)),
            0x25 => Some(self.letter('k', shift, caps)),
            0x26 => Some(self.letter('l', shift, caps)),
            0x27 => Some(if shift { ':' } else { ';' }),
            0x28 => Some(if shift { '"' } else { '\'' }),
            0x29 => Some(if shift { '~' } else { '`' }),

            // Bottom row (ZXCV)
            0x2B => Some(if shift { '|' } else { '\\' }),
            0x2C => Some(self.letter('z', shift, caps)),
            0x2D => Some(self.letter('x', shift, caps)),
            0x2E => Some(self.letter('c', shift, caps)),
            0x2F => Some(self.letter('v', shift, caps)),
            0x30 => Some(self.letter('b', shift, caps)),
            0x31 => Some(self.letter('n', shift, caps)),
            0x32 => Some(self.letter('m', shift, caps)),
            0x33 => Some(if shift { '<' } else { ',' }),
            0x34 => Some(if shift { '>' } else { '.' }),
            0x35 => Some(if shift { '?' } else { '/' }),

            // Space
            0x39 => Some(' '),
            _ => None,
        };

        ch
    }

    /// Handle letter case with shift and caps lock
    fn letter(&self, base: char, shift: bool, caps: bool) -> char {
        let upper = shift ^ caps;
        if upper {
            base.to_ascii_uppercase()
        } else {
            base
        }
    }
}

impl Default for InputHandler {
    fn default() -> Self {
        Self::new()
    }
}