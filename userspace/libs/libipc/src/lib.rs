//! libipc - Standardized IPC Message Protocols for Atom OS
//!
//! This library provides type-safe message definitions for inter-process
//! communication between userspace components. All messages are serialized
//! to byte arrays for transmission via the kernel IPC primitives.
//!
//! # Architecture
//!
//! The desktop environment uses a hub-and-spoke IPC model:
//! - Desktop compositor is the central hub
//! - Input drivers send events to the desktop
//! - Applications receive events from the desktop
//! - Graphics service handles framebuffer access
//!
//! # Message Flow
//!
//! ```text
//! Keyboard Driver ──┐
//!                   ├──> Desktop Environment ──> Applications
//! Mouse Driver ─────┘          │
//!                              │
//!                              v
//!                       Graphics Service
//! ```

#![no_std]

extern crate alloc;

use alloc::vec::Vec;

pub mod messages;
pub mod protocol;
pub mod ports;
pub mod serialization;

// Re-exports for convenience
pub use messages::*;
pub use protocol::*;
pub use ports::*;

/// Maximum message size in bytes
pub const MAX_MESSAGE_SIZE: usize = 4096;

/// Service identifier for port discovery
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ServiceId {
    /// Desktop environment compositor
    Desktop = 1,
    /// Keyboard input driver
    Keyboard = 2,
    /// Mouse input driver
    Mouse = 3,
    /// Graphics/display service
    Graphics = 4,
    /// Terminal application
    Terminal = 5,
}

impl ServiceId {
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1 => Some(ServiceId::Desktop),
            2 => Some(ServiceId::Keyboard),
            3 => Some(ServiceId::Mouse),
            4 => Some(ServiceId::Graphics),
            5 => Some(ServiceId::Terminal),
            _ => None,
        }
    }
}
