//! Application Framework
//!
//! Provides a high-level framework for creating GUI applications.
//! Applications create surfaces through the Application object and
//! receive events through the event loop.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use crate::surface::Surface;
use crate::event::{Event, MouseEvent};
use atom_syscall::ipc::{PortId, create_port};
use atom_syscall::SyscallResult;
use libipc::protocol::{lookup_service, send_message, get_payload};
use libipc::messages::{MessageType, WmCreateWindowRequest, WmCreateWindowResponse};

/// Application state and context
pub struct Application {
    /// Application name
    name: String,
    /// IPC port for receiving events from compositor
    event_port: PortId,
    /// Compositor WM port
    wm_port: PortId,
    /// Pending events queue
    event_queue: Vec<Event>,
    /// Whether application should quit
    quit_requested: bool,
}

impl Application {
    /// Create a new application
    pub fn new(name: &str) -> SyscallResult<Self> {
        let event_port = create_port()?;
        let wm_port = lookup_service("compositor.wm")?;

        Ok(Self {
            name: String::from(name),
            event_port,
            wm_port,
            event_queue: Vec::new(),
            quit_requested: false,
        })
    }

    /// Get application name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Create a window and get its drawing surface
    pub fn create_window(&mut self, title: &str, width: u32, height: u32) -> SyscallResult<Surface> {
        let req = WmCreateWindowRequest {
            reply_port: self.event_port,
            width,
            height,
            title: String::from(title),
        };

        send_message(self.wm_port, MessageType::WmRequest, &req.to_bytes())?;

        // Wait for response
        let mut buffer = [0u8; 256];
        loop {
            if let Ok(Some((header, len))) = libipc::protocol::try_recv_message(self.event_port, &mut buffer) {
                if header.msg_type == MessageType::WmResponse {
                    let payload = get_payload(&buffer, len);
                    if let Some(resp) = WmCreateWindowResponse::from_bytes(payload) {
                        // Create surface from shared region
                        let surface = Surface::from_wm_response(resp, self.wm_port)?;
                        return Ok(surface);
                    }
                }
            }
            atom_syscall::thread::yield_now();
        }
    }

    /// Create a surface for rendering
    pub fn create_surface(&mut self, width: u32, height: u32) -> SyscallResult<Surface> {
        self.create_window("New Window", width, height)
    }

    /// Create a full-screen surface
    pub fn create_fullscreen_surface(&mut self) -> SyscallResult<Surface> {
        // Fullscreen is just a very large window for now
        self.create_window("Fullscreen", 1024, 768)
    }

    /// Poll for the next event
    pub fn poll_event(&mut self) -> Event {
        if self.quit_requested {
            return Event::Quit;
        }

        if let Some(event) = self.event_queue.pop() {
            return event;
        }

        // Poll IPC port for messages
        let mut buffer = [0u8; 256];
        while let Ok(Some((header, len))) = libipc::protocol::try_recv_message(self.event_port, &mut buffer) {
            let payload = get_payload(&buffer, len);
            match header.msg_type {
                MessageType::KeyPress => {
                    if let Some(key_event) = libipc::messages::KeyEvent::from_bytes(payload) {
                        return Event::Key(crate::event::KeyEvent {
                            scancode: key_event.scancode,
                            character: key_event.character,
                            pressed: (key_event.scancode & 0x80) == 0,
                            modifiers: crate::event::KeyModifiers {
                                shift: key_event.modifiers.shift,
                                ctrl: key_event.modifiers.ctrl,
                                alt: key_event.modifiers.alt,
                                caps_lock: key_event.modifiers.caps_lock,
                            },
                        });
                    }
                }
                MessageType::MouseMove => {
                    if let Some(mouse_event) = libipc::messages::MouseMoveEvent::from_bytes(payload) {
                        return Event::Mouse(MouseEvent::Move {
                            x: mouse_event.x,
                            y: mouse_event.y,
                            dx: mouse_event.dx,
                            dy: mouse_event.dy,
                        });
                    }
                }
                MessageType::MouseButtonDown => {
                    if let Some(mouse_event) = libipc::messages::MouseButtonEvent::from_bytes(payload) {
                        let button = match mouse_event.button {
                            libipc::messages::MouseButton::Left => crate::event::MouseButton::Left,
                            libipc::messages::MouseButton::Right => crate::event::MouseButton::Right,
                            libipc::messages::MouseButton::Middle => crate::event::MouseButton::Middle,
                            _ => crate::event::MouseButton::Left,
                        };
                        return Event::Mouse(MouseEvent::ButtonDown {
                            button,
                            x: mouse_event.x,
                            y: mouse_event.y,
                        });
                    }
                }
                MessageType::MouseButtonUp => {
                    if let Some(mouse_event) = libipc::messages::MouseButtonEvent::from_bytes(payload) {
                        let button = match mouse_event.button {
                            libipc::messages::MouseButton::Left => crate::event::MouseButton::Left,
                            libipc::messages::MouseButton::Right => crate::event::MouseButton::Right,
                            libipc::messages::MouseButton::Middle => crate::event::MouseButton::Middle,
                            _ => crate::event::MouseButton::Left,
                        };
                        return Event::Mouse(MouseEvent::ButtonUp {
                            button,
                            x: mouse_event.x,
                            y: mouse_event.y,
                        });
                    }
                }
                MessageType::TerminateRequest => {
                    self.quit_requested = true;
                    return Event::Quit;
                }
                MessageType::WmEvent => {
                    if let Some(wm_event) = libipc::messages::WmWindowEventMsg::from_bytes(payload) {
                        match wm_event.event_type {
                            libipc::messages::WindowEventType::Close => {
                                self.quit_requested = true;
                                return Event::Quit;
                            }
                            libipc::messages::WindowEventType::Focus => {
                                return Event::Window(crate::event::WindowEvent::Focus);
                            }
                            libipc::messages::WindowEventType::Unfocus => {
                                return Event::Window(crate::event::WindowEvent::Unfocus);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }

        Event::None
    }

    /// Wait for the next event (blocking)
    pub fn wait_event(&mut self) -> Event {
        loop {
            let event = self.poll_event();
            if !matches!(event, Event::None) {
                return event;
            }
            atom_syscall::thread::yield_now();
        }
    }

    /// Request application quit
    pub fn quit(&mut self) {
        self.quit_requested = true;
    }

    /// Check if quit was requested
    pub fn should_quit(&self) -> bool {
        self.quit_requested
    }

    /// Push an event to the queue
    pub fn push_event(&mut self, event: Event) {
        self.event_queue.push(event);
    }
}

/// Simple scancode to ASCII conversion (US keyboard layout)
fn scancode_to_ascii(scancode: u8) -> u8 {
    // Only handle key press (not release)
    if scancode & 0x80 != 0 {
        return 0;
    }

    match scancode {
        0x02 => b'1',
        0x03 => b'2',
        0x04 => b'3',
        0x05 => b'4',
        0x06 => b'5',
        0x07 => b'6',
        0x08 => b'7',
        0x09 => b'8',
        0x0A => b'9',
        0x0B => b'0',
        0x0C => b'-',
        0x0D => b'=',
        0x0E => 0x08, // Backspace
        0x0F => b'\t',
        0x10 => b'q',
        0x11 => b'w',
        0x12 => b'e',
        0x13 => b'r',
        0x14 => b't',
        0x15 => b'y',
        0x16 => b'u',
        0x17 => b'i',
        0x18 => b'o',
        0x19 => b'p',
        0x1A => b'[',
        0x1B => b']',
        0x1C => b'\n', // Enter
        0x1E => b'a',
        0x1F => b's',
        0x20 => b'd',
        0x21 => b'f',
        0x22 => b'g',
        0x23 => b'h',
        0x24 => b'j',
        0x25 => b'k',
        0x26 => b'l',
        0x27 => b';',
        0x28 => b'\'',
        0x29 => b'`',
        0x2B => b'\\',
        0x2C => b'z',
        0x2D => b'x',
        0x2E => b'c',
        0x2F => b'v',
        0x30 => b'b',
        0x31 => b'n',
        0x32 => b'm',
        0x33 => b',',
        0x34 => b'.',
        0x35 => b'/',
        0x39 => b' ', // Space
        _ => 0,
    }
}
