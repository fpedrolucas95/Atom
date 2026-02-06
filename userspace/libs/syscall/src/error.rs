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
    ResourceBusy,
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

/// Threshold for detecting syscall error codes in raw return values.
///
/// Kernel error codes are defined as `u64::MAX - N` for small N.  Any raw
/// syscall return value at or above this threshold is an error code, never a
/// valid user-space virtual address.
///
/// This replaces the previously hardcoded `u64::MAX - 100` checks scattered
/// across the codebase.  The margin (256 slots) is generous enough to
/// accommodate future error codes without changing this constant.
///
/// Must be kept in sync with `kernel::mm::addrspace::SYSCALL_ERROR_THRESHOLD`.
pub const SYSCALL_ERROR_THRESHOLD: u64 = u64::MAX - 256;

/// Check whether a raw syscall return value represents an error code.
///
/// This is the single authoritative way to distinguish error returns from
/// valid addresses or counts.  Use this instead of ad-hoc `>= u64::MAX - N`
/// comparisons.
#[inline]
pub fn is_syscall_error(value: u64) -> bool {
    value >= SYSCALL_ERROR_THRESHOLD
}

/// Result type for syscall operations
pub type SyscallResult<T> = Result<T, SyscallError>;
