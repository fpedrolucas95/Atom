// IPC Error Types
//
// This module defines error types for the libipc library.

use core::fmt;

/// IPC error types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    /// Connection failed
    ConnectionFailed,
    /// Service not found
    ServiceNotFound,
    /// Send failed
    SendFailed,
    /// Receive failed
    ReceiveFailed,
    /// Message too large
    MessageTooLarge,
    /// Invalid message format
    InvalidMessage,
    /// Permission denied
    PermissionDenied,
    /// Timeout waiting for response
    Timeout,
    /// No message available (non-blocking)
    WouldBlock,
    /// Port is closed
    PortClosed,
    /// Out of memory
    OutOfMemory,
    /// Buffer too small
    BufferTooSmall,
    /// Unknown error
    Unknown(u64),
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IpcError::ConnectionFailed => write!(f, "Connection failed"),
            IpcError::ServiceNotFound => write!(f, "Service not found"),
            IpcError::SendFailed => write!(f, "Send failed"),
            IpcError::ReceiveFailed => write!(f, "Receive failed"),
            IpcError::MessageTooLarge => write!(f, "Message too large"),
            IpcError::InvalidMessage => write!(f, "Invalid message"),
            IpcError::PermissionDenied => write!(f, "Permission denied"),
            IpcError::Timeout => write!(f, "Timeout"),
            IpcError::WouldBlock => write!(f, "Would block"),
            IpcError::PortClosed => write!(f, "Port closed"),
            IpcError::OutOfMemory => write!(f, "Out of memory"),
            IpcError::BufferTooSmall => write!(f, "Buffer too small"),
            IpcError::Unknown(code) => write!(f, "Unknown error: {}", code),
        }
    }
}

/// Result type for IPC operations
pub type IpcResult<T> = Result<T, IpcError>;

impl From<atom_syscall::error::SyscallError> for IpcError {
    fn from(err: atom_syscall::error::SyscallError) -> Self {
        match err {
            atom_syscall::error::SyscallError::InvalidArgument => IpcError::InvalidMessage,
            atom_syscall::error::SyscallError::PermissionDenied => IpcError::PermissionDenied,
            atom_syscall::error::SyscallError::OutOfMemory => IpcError::OutOfMemory,
            atom_syscall::error::SyscallError::WouldBlock => IpcError::WouldBlock,
            atom_syscall::error::SyscallError::MessageTooLarge => IpcError::MessageTooLarge,
            atom_syscall::error::SyscallError::TimedOut => IpcError::Timeout,
            _ => IpcError::Unknown(0),
        }
    }
}
