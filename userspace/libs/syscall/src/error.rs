// Error codes and result types for syscalls

/// Syscall error codes (must match kernel/src/syscall/mod.rs)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallError {
    Success,
    InvalidArgument,
    NotImplemented,
    OutOfMemory,
    PermissionDenied,
    Busy,
    MessageTooLarge,
    TimedOut,
    WouldBlock,
    Deadlock,
    NotFound,
    Unknown(u64),
}

impl SyscallError {
    /// Convert from raw syscall return value
    pub fn from_raw(value: u64) -> Self {
        match value {
            0 => SyscallError::Success,
            v if v == u64::MAX - 1 => SyscallError::InvalidArgument,
            v if v == u64::MAX - 2 => SyscallError::NotImplemented,
            v if v == u64::MAX - 3 => SyscallError::OutOfMemory,
            v if v == u64::MAX - 4 => SyscallError::PermissionDenied,
            v if v == u64::MAX - 5 => SyscallError::Busy,
            v if v == u64::MAX - 6 => SyscallError::MessageTooLarge,
            v if v == u64::MAX - 7 => SyscallError::TimedOut,
            v if v == u64::MAX - 8 => SyscallError::WouldBlock,
            v if v == u64::MAX - 9 => SyscallError::Deadlock,
            v if v == u64::MAX - 10 => SyscallError::NotFound,
            v => SyscallError::Unknown(v),
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
pub const ENOTFOUND: u64 = u64::MAX - 10;

/// Result type for syscall operations
pub type SyscallResult<T> = Result<T, SyscallError>;
