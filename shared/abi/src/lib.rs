//! Atom ABI — single source of truth for kernel ↔ userspace constants.
//!
//! Every constant that must agree across the kernel / userspace boundary
//! lives here.  Both the kernel crate and the `atom_syscall` userspace
//! library depend on `atom_abi`, making divergence structurally impossible.
//!
//! # Layout
//! - Error codes (`EINVAL`, `ENOSYS`, …)
//! - Error detection threshold and helper
//! - User virtual-address limits
//! - Syscall numbers

#![no_std]

// ---------------------------------------------------------------------------
// User virtual-address limits
// ---------------------------------------------------------------------------

/// Last valid byte in the lower-half canonical range on x86-64.
/// Any user-space pointer must be ≤ this value.
pub const USER_CANONICAL_MAX: u64 = 0x0000_7FFF_FFFF_FFFF;

/// Alias for `USER_CANONICAL_MAX`.  Syscall return values strictly above
/// this are guaranteed to be kernel error codes (which live near `u64::MAX`).
pub const USER_VA_LIMIT: u64 = USER_CANONICAL_MAX;

// ---------------------------------------------------------------------------
// Syscall error codes
// ---------------------------------------------------------------------------

/// Threshold for detecting syscall error codes in raw return values.
///
/// Kernel error codes are defined as `u64::MAX - N` for small N.  Any raw
/// syscall return value **at or above** this threshold is an error code,
/// never a valid user-space virtual address.
///
/// The margin (256 slots) is generous enough to accommodate future error
/// codes without changing user-space detection logic.
pub const SYSCALL_ERROR_THRESHOLD: u64 = u64::MAX - 256;

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

/// Check whether a raw syscall return value represents an error code.
///
/// This is the **single authoritative way** to distinguish error returns
/// from valid addresses or counts.  Use this instead of ad-hoc
/// `>= u64::MAX - N` comparisons.
#[inline]
pub fn is_syscall_error(value: u64) -> bool {
    value >= SYSCALL_ERROR_THRESHOLD
}
