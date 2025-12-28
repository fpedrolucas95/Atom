//! Application Framework
//!
//! Provides a high-level framework for creating GUI applications.
//! Applications create surfaces through the Application object and
//! receive events through the event loop.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use crate::surface::Surface;
use crate::event::Event;
use atom_syscall::ipc::PortId;
use atom_syscall::SyscallResult;

/// Application state and context
pub struct Application {
    /// Application name
    name: String,
    /// IPC port for receiving events from compositor
    event_port: Option<PortId>,
    /// Pending events queue
    event_queue: Vec<Event>,
    /// Whether application should quit
    quit_requested: bool,
}

impl Application {
    /// Create a new application
    ///
    /// In the full implementation, this would:
    /// 1. Register with the desktop compositor
    /// 2. Create an IPC port for receiving events
    /// 3. Request initial window/surface allocation
    pub fn new(name: &str) -> SyscallResult<Self> {
        Ok(Self {
            name: String::from(name),
            event_port: None,
            event_queue: Vec::new(),
            quit_requested: false,
        })
    }

    /// Get application name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Create a surface for rendering
    ///
    /// In the full implementation, this would request a surface
    /// from the desktop compositor via IPC.
    pub fn create_surface(&mut self, width: u32, height: u32) -> SyscallResult<Surface> {
        // For now, directly use the framebuffer
        use atom_syscall::graphics::get_framebuffer_info;

        let info = get_framebuffer_info()?;

        // Calculate effective dimensions (clamp to framebuffer size)
        let effective_width = width.min(info.1);
        let effective_height = height.min(info.2);

        Ok(Surface::new(
            1, // Surface ID
            effective_width,
            effective_height,
            info.3, // stride
            info.4 as usize, // bpp
            info.0 as *mut u8, // buffer address
        ))
    }

    /// Create a full-screen surface
    pub fn create_fullscreen_surface(&mut self) -> SyscallResult<Surface> {
        use atom_syscall::graphics::get_framebuffer_info;

        let info = get_framebuffer_info()?;

        Ok(Surface::new(
            0, // Surface ID 0 = fullscreen
            info.1, // width
            info.2, // height
            info.3, // stride
            info.4 as usize, // bpp
            info.0 as *mut u8, // buffer address
        ))
    }

    /// Poll for the next event
    ///
    /// Returns None if no events are pending.
    /// In the full implementation, this would check the IPC port.
    pub fn poll_event(&mut self) -> Event {
        if self.quit_requested {
            return Event::Quit;
        }

        if let Some(event) = self.event_queue.pop() {
            return event;
        }

        // Check for keyboard input
        if let Some(scancode) = atom_syscall::input::keyboard_poll() {
            return Event::Key(crate::event::KeyEvent {
                scancode,
                character: scancode_to_ascii(scancode),
                pressed: scancode & 0x80 == 0,
                modifiers: crate::event::KeyModifiers::default(),
            });
        }

        // Check for mouse input
        // Note: This would need proper packet assembly in real implementation
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
