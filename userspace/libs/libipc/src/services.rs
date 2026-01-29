//! Service Protocol Messages
//!
//! This module defines IPC message types for service discovery and management.
//! These are used by the name service and service manager.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================================
// Well-Known Service Ports
// ============================================================================

/// Well-known port for the name service
pub const NAME_SERVICE_PORT: u64 = 100;

/// Well-known port for the service manager
pub const SERVICE_MANAGER_PORT: u64 = 101;

// ============================================================================
// Name Service Messages (600-699)
// ============================================================================

/// Name service message types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsMessageType {
    /// Register a service: name (string), port (u64)
    Register = 600,
    /// Unregister a service: name (string)
    Unregister = 601,
    /// Lookup a service: name (string) -> port (u64)
    Lookup = 602,
    /// List all services -> array of (name, port)
    List = 603,
    /// Response: success with data
    Response = 610,
    /// Response: error with message
    Error = 611,
}

impl NsMessageType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            600 => Some(Self::Register),
            601 => Some(Self::Unregister),
            602 => Some(Self::Lookup),
            603 => Some(Self::List),
            610 => Some(Self::Response),
            611 => Some(Self::Error),
            _ => None,
        }
    }
}

/// Name service error codes
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsError {
    /// Invalid message format
    InvalidMessage = 1,
    /// Registry is full
    RegistryFull = 2,
    /// Service not found
    NotFound = 3,
    /// Unknown message type
    UnknownMessage = 4,
}

/// Register request message
#[derive(Debug, Clone)]
pub struct NsRegisterRequest {
    pub name: String,
    pub port: u64,
}

impl NsRegisterRequest {
    /// Serialize to bytes
    /// Format: [msg_type: u32][port: u64][name_len: u32][name: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.name.len());
        buf.extend_from_slice(&(NsMessageType::Register as u32).to_le_bytes());
        buf.extend_from_slice(&self.port.to_le_bytes());
        buf.extend_from_slice(&(self.name.len() as u32).to_le_bytes());
        buf.extend_from_slice(self.name.as_bytes());
        buf
    }

    /// Parse from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 16 {
            return None;
        }
        let port = u64::from_le_bytes([
            data[4], data[5], data[6], data[7],
            data[8], data[9], data[10], data[11],
        ]);
        let name_len = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
        if data.len() < 16 + name_len {
            return None;
        }
        let name = core::str::from_utf8(&data[16..16 + name_len]).ok()?;
        Some(Self {
            name: String::from(name),
            port,
        })
    }
}

/// Lookup request message
#[derive(Debug, Clone)]
pub struct NsLookupRequest {
    pub name: String,
}

impl NsLookupRequest {
    /// Serialize to bytes
    /// Format: [msg_type: u32][name_len: u32][name: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + self.name.len());
        buf.extend_from_slice(&(NsMessageType::Lookup as u32).to_le_bytes());
        buf.extend_from_slice(&(self.name.len() as u32).to_le_bytes());
        buf.extend_from_slice(self.name.as_bytes());
        buf
    }

    /// Parse from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let name_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if data.len() < 8 + name_len {
            return None;
        }
        let name = core::str::from_utf8(&data[8..8 + name_len]).ok()?;
        Some(Self {
            name: String::from(name),
        })
    }
}

/// Response with port
#[derive(Debug, Clone, Copy)]
pub struct NsPortResponse {
    pub port: u64,
}

impl NsPortResponse {
    /// Format: [msg_type: u32][port: u64]
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut buf = [0u8; 12];
        buf[0..4].copy_from_slice(&(NsMessageType::Response as u32).to_le_bytes());
        buf[4..12].copy_from_slice(&self.port.to_le_bytes());
        buf
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 12 {
            return None;
        }
        let port = u64::from_le_bytes([
            data[4], data[5], data[6], data[7],
            data[8], data[9], data[10], data[11],
        ]);
        Some(Self { port })
    }
}

/// Service entry in list response
#[derive(Debug, Clone)]
pub struct NsServiceEntry {
    pub name: String,
    pub port: u64,
}

/// List response
#[derive(Debug, Clone)]
pub struct NsListResponse {
    pub services: Vec<NsServiceEntry>,
}

impl NsListResponse {
    /// Format: [msg_type: u32][count: u32][entries...]
    /// Entry: [port: u64][name_len: u32][name: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(NsMessageType::Response as u32).to_le_bytes());
        buf.extend_from_slice(&(self.services.len() as u32).to_le_bytes());

        for entry in &self.services {
            buf.extend_from_slice(&entry.port.to_le_bytes());
            buf.extend_from_slice(&(entry.name.len() as u32).to_le_bytes());
            buf.extend_from_slice(entry.name.as_bytes());
        }

        buf
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

        let mut services = Vec::with_capacity(count);
        let mut offset = 8;

        for _ in 0..count {
            if offset + 12 > data.len() {
                return None;
            }
            let port = u64::from_le_bytes([
                data[offset], data[offset + 1], data[offset + 2], data[offset + 3],
                data[offset + 4], data[offset + 5], data[offset + 6], data[offset + 7],
            ]);
            let name_len = u32::from_le_bytes([
                data[offset + 8], data[offset + 9], data[offset + 10], data[offset + 11],
            ]) as usize;
            offset += 12;

            if offset + name_len > data.len() {
                return None;
            }
            let name = core::str::from_utf8(&data[offset..offset + name_len]).ok()?;
            offset += name_len;

            services.push(NsServiceEntry {
                name: String::from(name),
                port,
            });
        }

        Some(Self { services })
    }
}

// ============================================================================
// Service Manager Messages (700-799)
// ============================================================================

/// Service manager message types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmMessageType {
    /// Start a service: name (string)
    StartService = 700,
    /// Stop a service: name (string)
    StopService = 701,
    /// Restart a service: name (string)
    RestartService = 702,
    /// List all services
    ListServices = 703,
    /// Get service status: name (string)
    ServiceStatus = 704,
    /// Register service port: name (string), port (u64)
    RegisterPort = 705,
    /// Response: success with data
    Response = 710,
    /// Response: error
    Error = 711,
}

impl SmMessageType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            700 => Some(Self::StartService),
            701 => Some(Self::StopService),
            702 => Some(Self::RestartService),
            703 => Some(Self::ListServices),
            704 => Some(Self::ServiceStatus),
            705 => Some(Self::RegisterPort),
            710 => Some(Self::Response),
            711 => Some(Self::Error),
            _ => None,
        }
    }
}

/// Service state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceState {
    /// Service is not running
    Stopped = 0,
    /// Service is starting
    Starting = 1,
    /// Service is running
    Running = 2,
    /// Service crashed
    Crashed = 3,
    /// Service is stopping
    Stopping = 4,
    /// Service failed to start
    Failed = 5,
}

impl ServiceState {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Stopped),
            1 => Some(Self::Starting),
            2 => Some(Self::Running),
            3 => Some(Self::Crashed),
            4 => Some(Self::Stopping),
            5 => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Crashed => "crashed",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }
}

/// Start service request
#[derive(Debug, Clone)]
pub struct SmStartRequest {
    pub name: String,
}

impl SmStartRequest {
    /// Format: [msg_type: u32][name_len: u32][name: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + self.name.len());
        buf.extend_from_slice(&(SmMessageType::StartService as u32).to_le_bytes());
        buf.extend_from_slice(&(self.name.len() as u32).to_le_bytes());
        buf.extend_from_slice(self.name.as_bytes());
        buf
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let name_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        if data.len() < 8 + name_len {
            return None;
        }
        let name = core::str::from_utf8(&data[8..8 + name_len]).ok()?;
        Some(Self {
            name: String::from(name),
        })
    }
}

/// Service status response
#[derive(Debug, Clone, Copy)]
pub struct SmStatusResponse {
    pub state: ServiceState,
    pub pid: u64,
    pub restart_count: u32,
}

impl SmStatusResponse {
    /// Format: [msg_type: u32][state: u8][pid: u64][restart_count: u32]
    pub fn to_bytes(&self) -> [u8; 17] {
        let mut buf = [0u8; 17];
        buf[0..4].copy_from_slice(&(SmMessageType::Response as u32).to_le_bytes());
        buf[4] = self.state as u8;
        buf[5..13].copy_from_slice(&self.pid.to_le_bytes());
        buf[13..17].copy_from_slice(&self.restart_count.to_le_bytes());
        buf
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 17 {
            return None;
        }
        Some(Self {
            state: ServiceState::from_u8(data[4])?,
            pid: u64::from_le_bytes([
                data[5], data[6], data[7], data[8],
                data[9], data[10], data[11], data[12],
            ]),
            restart_count: u32::from_le_bytes([data[13], data[14], data[15], data[16]]),
        })
    }
}

/// Service list entry
#[derive(Debug, Clone)]
pub struct SmServiceEntry {
    pub name: String,
    pub state: ServiceState,
    pub pid: u64,
}

/// List services response
#[derive(Debug, Clone)]
pub struct SmListResponse {
    pub services: Vec<SmServiceEntry>,
}

impl SmListResponse {
    /// Format: [msg_type: u32][count: u32][entries...]
    /// Entry: [state: u8][pid: u64][name_len: u32][name: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(SmMessageType::Response as u32).to_le_bytes());
        buf.extend_from_slice(&(self.services.len() as u32).to_le_bytes());

        for entry in &self.services {
            buf.push(entry.state as u8);
            buf.extend_from_slice(&entry.pid.to_le_bytes());
            buf.extend_from_slice(&(entry.name.len() as u32).to_le_bytes());
            buf.extend_from_slice(entry.name.as_bytes());
        }

        buf
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 8 {
            return None;
        }
        let count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;

        let mut services = Vec::with_capacity(count);
        let mut offset = 8;

        for _ in 0..count {
            if offset + 13 > data.len() {
                return None;
            }
            let state = ServiceState::from_u8(data[offset])?;
            let pid = u64::from_le_bytes([
                data[offset + 1], data[offset + 2], data[offset + 3], data[offset + 4],
                data[offset + 5], data[offset + 6], data[offset + 7], data[offset + 8],
            ]);
            let name_len = u32::from_le_bytes([
                data[offset + 9], data[offset + 10], data[offset + 11], data[offset + 12],
            ]) as usize;
            offset += 13;

            if offset + name_len > data.len() {
                return None;
            }
            let name = core::str::from_utf8(&data[offset..offset + name_len]).ok()?;
            offset += name_len;

            services.push(SmServiceEntry {
                name: String::from(name),
                state,
                pid,
            });
        }

        Some(Self { services })
    }
}
