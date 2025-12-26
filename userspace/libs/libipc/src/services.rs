// Service Discovery and Well-Known Ports
//
// This module defines well-known service ports and provides
// service discovery mechanisms.
//
// In the Atom microkernel architecture:
// - Core services register on well-known port IDs
// - Applications connect to services by these IDs
// - The desktop environment is the central authority for UI policy

use atom_syscall::ipc::PortId;

/// Well-known service port numbers
///
/// These are reserved port IDs for core system services.
/// Services register on these ports during startup.
pub mod ports {
    /// Desktop Environment service
    /// Handles: window management, compositing, input routing
    pub const DESKTOP: u64 = 1;

    /// Input Driver Hub
    /// Receives raw input from drivers, forwards to desktop
    pub const INPUT: u64 = 2;

    /// Display/Compositor service
    /// Handles framebuffer access and surface composition
    pub const DISPLAY: u64 = 3;

    /// Terminal service
    /// Provides terminal emulation for applications
    pub const TERMINAL: u64 = 4;

    /// First dynamically assigned port
    pub const DYNAMIC_START: u64 = 100;
}

/// Re-exports for convenience
pub const SERVICE_DESKTOP: u64 = ports::DESKTOP;
pub const SERVICE_INPUT: u64 = ports::INPUT;
pub const SERVICE_DISPLAY: u64 = ports::DISPLAY;
pub const SERVICE_TERMINAL: u64 = ports::TERMINAL;

/// Service identification
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServicePort {
    /// Desktop environment (window manager + compositor)
    Desktop,
    /// Input driver hub
    Input,
    /// Display service
    Display,
    /// Terminal service
    Terminal,
    /// Custom service with dynamic port
    Custom(PortId),
}

impl ServicePort {
    /// Get the port ID for this service
    pub fn port_id(&self) -> PortId {
        match self {
            ServicePort::Desktop => ports::DESKTOP,
            ServicePort::Input => ports::INPUT,
            ServicePort::Display => ports::DISPLAY,
            ServicePort::Terminal => ports::TERMINAL,
            ServicePort::Custom(id) => *id,
        }
    }

    /// Get service name for debugging
    pub fn name(&self) -> &'static str {
        match self {
            ServicePort::Desktop => "Desktop",
            ServicePort::Input => "Input",
            ServicePort::Display => "Display",
            ServicePort::Terminal => "Terminal",
            ServicePort::Custom(_) => "Custom",
        }
    }
}

impl From<u64> for ServicePort {
    fn from(id: u64) -> Self {
        match id {
            ports::DESKTOP => ServicePort::Desktop,
            ports::INPUT => ServicePort::Input,
            ports::DISPLAY => ServicePort::Display,
            ports::TERMINAL => ServicePort::Terminal,
            id => ServicePort::Custom(id),
        }
    }
}

/// Service capabilities required/provided
#[derive(Debug, Clone, Copy)]
pub struct ServiceCapabilities {
    /// Can handle input events
    pub input: bool,
    /// Can create surfaces
    pub surfaces: bool,
    /// Can composite/render
    pub rendering: bool,
    /// Is a system service
    pub system: bool,
}

impl ServiceCapabilities {
    pub const fn new() -> Self {
        Self {
            input: false,
            surfaces: false,
            rendering: false,
            system: false,
        }
    }

    pub const fn desktop() -> Self {
        Self {
            input: true,
            surfaces: true,
            rendering: true,
            system: true,
        }
    }

    pub const fn application() -> Self {
        Self {
            input: true,
            surfaces: true,
            rendering: false,
            system: false,
        }
    }
}
