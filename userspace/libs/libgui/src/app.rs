// Application Framework
//
// Provides the main entry point for GUI applications.
// Applications use this to connect to the desktop environment,
// receive a drawing surface, and process events.

extern crate alloc;

use alloc::vec::Vec;
use crate::surface::Surface;
use crate::event::{Event, EventQueue, KeyEvent, MouseEvent, MouseButtonEvent, FocusEvent};
use crate::color::Color;
use libipc::client::DesktopClient;
use libipc::protocol::{Message, MessageType, KeyEventPayload, MouseMovePayload, MouseButtonPayload};

/// Application state and connection to desktop environment
pub struct Application {
    /// Connection to desktop environment
    desktop: Option<DesktopClient>,
    /// The application's drawing surface
    surface: Option<Surface>,
    /// Event queue
    events: EventQueue,
    /// Whether the app has focus
    focused: bool,
    /// Application title
    title: &'static str,
}

impl Application {
    /// Create a new application
    ///
    /// The application is not yet connected to the desktop environment.
    /// Call `connect()` to establish the connection and get a surface.
    pub fn new(title: &'static str) -> Self {
        Self {
            desktop: None,
            surface: None,
            events: EventQueue::new(),
            focused: false,
            title,
        }
    }

    /// Get the application title
    pub fn title(&self) -> &'static str {
        self.title
    }

    /// Connect to the desktop environment and request a surface
    pub fn connect(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        // Connect to desktop environment
        let mut desktop = DesktopClient::connect()
            .map_err(|_| "Failed to connect to desktop environment")?;

        // Request a surface
        let surface_info = desktop.create_surface(width, height)
            .map_err(|_| "Failed to create surface")?;

        // Create the surface
        let surface = Surface::from_allocation(
            surface_info.surface_id,
            surface_info.width,
            surface_info.height,
            surface_info.buffer_addr as *mut u32,
            surface_info.stride,
        );

        self.desktop = Some(desktop);
        self.surface = Some(surface);

        Ok(())
    }

    /// Get the drawing surface
    ///
    /// Returns None if not connected or surface creation failed.
    pub fn surface(&self) -> Option<&Surface> {
        self.surface.as_ref()
    }

    /// Get mutable access to the drawing surface
    pub fn surface_mut(&mut self) -> Option<&mut Surface> {
        self.surface.as_mut()
    }

    /// Check if application has focus
    pub fn has_focus(&self) -> bool {
        self.focused
    }

    /// Poll for events from the desktop environment
    ///
    /// This should be called in the main loop to receive input events.
    pub fn poll_events(&mut self) {
        // Collect messages first to avoid borrow conflict
        let mut messages = Vec::new();
        if let Some(ref mut desktop) = self.desktop {
            while let Ok(Some(msg)) = desktop.client_mut().try_recv() {
                messages.push(msg);
            }
        }

        // Process collected messages
        for msg in messages {
            self.process_message(msg);
        }
    }

    /// Get the next event from the queue
    pub fn next_event(&mut self) -> Option<Event> {
        self.events.pop()
    }

    /// Check if there are pending events
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Present the surface (notify desktop of changes)
    pub fn present(&mut self) {
        if let Some(ref mut surface) = self.surface {
            if surface.is_dirty() {
                // Get dirty region
                let damage = surface.dirty_region();

                // Notify desktop environment
                if let Some(ref desktop) = self.desktop {
                    if let Some((x, y, w, h)) = damage {
                        let _ = desktop.damage_surface(surface.id(), x, y, w, h);
                    }
                    let _ = desktop.present_surface(surface.id());
                }

                surface.clear_dirty();
            }
        }
    }

    /// Process a message from the desktop environment
    fn process_message(&mut self, msg: Message) {
        match msg.message_type() {
            MessageType::KeyPress => {
                if let Some(payload) = KeyEventPayload::from_bytes(&msg.payload) {
                    let event = KeyEvent::new(
                        payload.scancode,
                        payload.ascii as char,
                        payload.shift(),
                        payload.ctrl(),
                        payload.alt(),
                    );
                    self.events.push(Event::KeyPress(event));
                }
            }

            MessageType::KeyRelease => {
                if let Some(payload) = KeyEventPayload::from_bytes(&msg.payload) {
                    let event = KeyEvent::new(
                        payload.scancode,
                        payload.ascii as char,
                        payload.shift(),
                        payload.ctrl(),
                        payload.alt(),
                    );
                    self.events.push(Event::KeyRelease(event));
                }
            }

            MessageType::MouseMove => {
                if let Some(payload) = MouseMovePayload::from_bytes(&msg.payload) {
                    let event = MouseEvent::new(
                        payload.x,
                        payload.y,
                        payload.dx,
                        payload.dy,
                    );
                    self.events.push(Event::MouseMove(event));
                }
            }

            MessageType::MouseButtonPress => {
                if let Some(payload) = MouseButtonPayload::from_bytes(&msg.payload) {
                    let event = MouseButtonEvent::new(
                        crate::event::MouseButton::from_u8(payload.button),
                        payload.x,
                        payload.y,
                    );
                    self.events.push(Event::MouseButtonPress(event));
                }
            }

            MessageType::MouseButtonRelease => {
                if let Some(payload) = MouseButtonPayload::from_bytes(&msg.payload) {
                    let event = MouseButtonEvent::new(
                        crate::event::MouseButton::from_u8(payload.button),
                        payload.x,
                        payload.y,
                    );
                    self.events.push(Event::MouseButtonRelease(event));
                }
            }

            MessageType::WindowFocus => {
                self.focused = true;
                self.events.push(Event::FocusIn(FocusEvent::gained()));
            }

            MessageType::WindowUnfocus => {
                self.focused = false;
                self.events.push(Event::FocusOut(FocusEvent::lost()));
            }

            MessageType::WindowDestroyed => {
                self.events.push(Event::CloseRequested);
            }

            _ => {
                // Ignore unknown messages
            }
        }
    }

    /// Run the application main loop with a callback
    ///
    /// The callback receives the application and should return true to continue
    /// running or false to exit.
    pub fn run<F>(&mut self, mut callback: F)
    where
        F: FnMut(&mut Self) -> bool,
    {
        loop {
            // Poll for events
            self.poll_events();

            // Call the application callback
            if !callback(self) {
                break;
            }

            // Present any changes
            self.present();

            // Yield to other processes
            atom_syscall::thread::yield_now();
        }
    }
}

impl Drop for Application {
    fn drop(&mut self) {
        // Notify desktop environment that we're closing
        // The surface will be cleaned up automatically
    }
}

/// Simple application runner for basic use cases
///
/// Example:
/// ```
/// use libgui::{Application, Event, Color};
///
/// fn main() {
///     let mut app = Application::new("My App");
///     app.connect(640, 480).expect("Failed to connect");
///
///     app.run(|app| {
///         while let Some(event) = app.next_event() {
///             match event {
///                 Event::CloseRequested => return false,
///                 _ => {}
///             }
///         }
///
///         if let Some(surface) = app.surface_mut() {
///             surface.clear(Color::BLACK);
///             // Draw...
///         }
///
///         true
///     });
/// }
/// ```
pub fn run_app<F>(title: &'static str, width: u32, height: u32, callback: F)
where
    F: FnMut(&mut Application) -> bool,
{
    let mut app = Application::new(title);

    if app.connect(width, height).is_ok() {
        app.run(callback);
    }
}
