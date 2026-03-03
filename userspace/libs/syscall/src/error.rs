// Error codes and result types for syscalls
//
// All numeric constants are re-exported from `atom_abi` — the single source
// of truth shared with the kernel.  This makes accidental divergence
// structurally impossible.

// Re-export every ABI constant so existing `crate::error::EINVAL` paths
// keep compiling without change.
pub use atom_abi::{
    ESUCCESS, EINVAL, ENOSYS, ENOMEM, EPERM, EBUSY,
    EMSGSIZE, ETIMEDOUT, EWOULDBLOCK, EDEADLK, ENOTFOUND,
    SYSCALL_ERROR_THRESHOLD, USER_VA_LIMIT,
    is_syscall_error,
    // Filesystem / IO error codes — used by spawn_from_path and similar wrappers.
    ENOENT, ENOTSUP, EIO,
    ENODEV,
};

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
    /// Operation not supported (ENOTSUP) — e.g. wrong file type for a syscall.
    NotSupported,
    /// I/O error (EIO) — e.g. block device or filesystem driver unavailable.
    IoError,
    /// No such device (ENODEV) — hardware is absent or not initialised.
    NoDevice,
    Unknown(u64),
}

impl SyscallError {
    /// Convert from raw syscall return value
    pub fn from_raw(value: u64) -> Self {
        match value {
            0 => SyscallError::Success,
            v if v == EINVAL => SyscallError::InvalidArgument,
            v if v == ENOSYS => SyscallError::NotImplemented,
            v if v == ENOMEM => SyscallError::OutOfMemory,
            v if v == EPERM => SyscallError::PermissionDenied,
            v if v == EBUSY => SyscallError::Busy,
            v if v == EMSGSIZE => SyscallError::MessageTooLarge,
            v if v == ETIMEDOUT => SyscallError::TimedOut,
            v if v == EWOULDBLOCK => SyscallError::WouldBlock,
            v if v == EDEADLK => SyscallError::Deadlock,
            v if v == ENOTFOUND => SyscallError::NotFound,
            v if v == ENODEV => SyscallError::NoDevice,
            v => SyscallError::Unknown(v),
        }
    }

    /// Check if this is the WouldBlock error
    pub fn is_would_block(value: u64) -> bool {
        value == EWOULDBLOCK
    }
}

/// Result type for syscall operations
pub type SyscallResult<T> = Result<T, SyscallError>;
