//! Application Framework
//!
//! Provides a high-level framework for creating GUI applications.
//! Applications create surfaces through the Application object and
//! receive events through the event loop.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use crate::surface::Surface;
use crate::event::{Event, MouseEvent, WindowEvent};
use atom_syscall::ipc::{PortId, create_port};
use atom_syscall::SyscallResult;
use libipc::protocol::{lookup_service, send_message, get_payload};
use libipc::messages::{MessageType, WmCreateWindowRequest, WmCreateWindowResponse, SurfaceAssignMsg};

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
    /// Surfaces managed by this application
    surfaces: Vec<Surface>,
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
            surfaces: Vec::new(),
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
                        self.surfaces.push(surface.clone());
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
                MessageType::SurfaceAssign => {
                    if let Some(msg) = SurfaceAssignMsg::from_bytes(payload) {
                        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.window_id() == msg.window_id) {
                            // Update shared surface transparently
                            if let Ok(new_shared) = atom_syscall::graphics::SharedSurface::from_region(msg.region_id, msg.width, msg.height) {
                                let mut inner = surface.inner.borrow_mut();
                                inner.shared = new_shared;
                                inner.scale_factor = msg.scale_factor;
                                // Reset dirty so the app must explicitly redraw into the new surface
                                inner.dirty = false;
                                // Signal resize to application
                                return Event::Window(WindowEvent::Resize {
                                    width: msg.width,
                                    height: msg.height,
                                });
                            }
                        }
                    }
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
                            libipc::messages::WindowEventType::Resize => {
                                return Event::Window(WindowEvent::Resize {
                                    width: wm_event.width,
                                    height: wm_event.height,
                                });
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
