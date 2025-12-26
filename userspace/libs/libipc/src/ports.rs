//! Well-Known Port Definitions
//!
//! This module defines well-known port names and provides utilities
//! for service discovery.

extern crate alloc;

use alloc::string::String;
use atom_syscall::ipc::{PortId, create_port};
use atom_syscall::SyscallResult;

/// Well-known port identifiers
///
/// These are reserved port IDs that services register with.
/// Port 0 is invalid, ports 1-255 are reserved for system services.
pub mod well_known {
    /// Desktop environment service port
    pub const DESKTOP_SERVICE: u64 = 1;
    /// Keyboard driver input port
    pub const KEYBOARD_INPUT: u64 = 2;
    /// Mouse driver input port
    pub const MOUSE_INPUT: u64 = 3;
    /// Graphics service port
    pub const GRAPHICS_SERVICE: u64 = 4;
    /// Terminal service port
    pub const TERMINAL_SERVICE: u64 = 5;
}

/// Port configuration for a service
#[derive(Debug, Clone)]
pub struct ServicePort {
    /// Human-readable name
    pub name: String,
    /// Port ID (assigned by kernel)
    pub port_id: PortId,
    /// Service type
    pub service_id: crate::ServiceId,
}

impl ServicePort {
    /// Create a new service port
    pub fn new(name: &str, service_id: crate::ServiceId) -> SyscallResult<Self> {
        let port_id = create_port()?;
        Ok(Self {
            name: String::from(name),
            port_id,
            service_id,
        })
    }

    /// Get the port ID for syscall use
    pub fn id(&self) -> PortId {
        self.port_id
    }
}

/// Port set for waiting on multiple ports
pub struct PortSet {
    ports: alloc::vec::Vec<PortId>,
}

impl PortSet {
    pub fn new() -> Self {
        Self {
            ports: alloc::vec::Vec::new(),
        }
    }

    pub fn add(&mut self, port: PortId) {
        if !self.ports.contains(&port) {
            self.ports.push(port);
        }
    }

    pub fn remove(&mut self, port: PortId) {
        self.ports.retain(|&p| p != port);
    }

    pub fn as_slice(&self) -> &[PortId] {
        &self.ports
    }

    pub fn len(&self) -> usize {
        self.ports.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ports.is_empty()
    }

    /// Wait for any port in the set to have data
    pub fn wait_any(&self, timeout_ms: u64) -> SyscallResult<usize> {
        atom_syscall::ipc::wait_any(&self.ports, timeout_ms)
    }

    /// Get port at index
    pub fn get(&self, index: usize) -> Option<PortId> {
        self.ports.get(index).copied()
    }
}

impl Default for PortSet {
    fn default() -> Self {
        Self::new()
    }
}
