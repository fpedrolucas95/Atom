// Error codes and result types for syscalls

/// Syscall error codes (must match kernel/src/syscall/mod.rs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallError {
    Success = 0,
    InvalidArgument = u64::MAX - 1,
    NotImplemented = u64::MAX - 2,
    OutOfMemory = u64::MAX - 3,
    PermissionDenied = u64::MAX - 4,
    Busy = u64::MAX - 5,
    MessageTooLarge = u64::MAX - 6,
    TimedOut = u64::MAX - 7,
    WouldBlock = u64::MAX - 8,
    Deadlock = u64::MAX - 9,
}

impl SyscallError {
    /// Convert from raw syscall return value
    pub fn from_raw(value: u64) -> Option<Self> {
        match value {
            0 => Some(SyscallError::Success),
            v if v == u64::MAX - 1 => Some(SyscallError::InvalidArgument),
            v if v == u64::MAX - 2 => Some(SyscallError::NotImplemented),
            v if v == u64::MAX - 3 => Some(SyscallError::OutOfMemory),
            v if v == u64::MAX - 4 => Some(SyscallError::PermissionDenied),
            v if v == u64::MAX - 5 => Some(SyscallError::Busy),
            v if v == u64::MAX - 6 => Some(SyscallError::MessageTooLarge),
            v if v == u64::MAX - 7 => Some(SyscallError::TimedOut),
            v if v == u64::MAX - 8 => Some(SyscallError::WouldBlock),
            v if v == u64::MAX - 9 => Some(SyscallError::Deadlock),
            _ => None,
        }
    }

    /// Check if this is the WouldBlock error
    pub fn is_would_block(value: u64) -> bool {
        value == u64::MAX - 8
    }
}

/// Convenient constants for direct comparison
pub const ESUCCESS: u64 = 0;
pub const EINVAL: u64 = u64::MAX - 1;
pub const ENOSYS: u64 = u64::MAX - 2;
pub const ENOMEM: u64 = u64::MAX - 3;
pub const EPERM: u64 = u64::MAX - 4;
pub const EBUSY: u64 = u64::MAX - 5;
pub const EMSGSIZE: u64 = u64::MAX - 6;
pub const ETIMEDOUT: u64 = u64::MAX - 7;
pub const EWOULDBLOCK: u64 = u64::MAX - 8;
pub const EDEADLK: u64 = u64::MAX - 9;

/// Result type for syscall operations
pub type SyscallResult<T> = Result<T, SyscallError>;
