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

// Filesystem error codes
pub const ENOENT: u64 = u64::MAX - 11;
pub const EEXIST: u64 = u64::MAX - 12;
pub const EISDIR: u64 = u64::MAX - 13;
pub const ENOTDIR: u64 = u64::MAX - 14;
pub const ENOTEMPTY: u64 = u64::MAX - 15;
pub const EBADF: u64 = u64::MAX - 16;
pub const EFBIG: u64 = u64::MAX - 17;
pub const ENOSPC: u64 = u64::MAX - 18;
pub const EROFS: u64 = u64::MAX - 19;
pub const ENAMETOOLONG: u64 = u64::MAX - 20;
pub const EIO: u64 = u64::MAX - 21;
pub const EACCES: u64 = u64::MAX - 22;
pub const EMFILE: u64 = u64::MAX - 23;
pub const EOVERFLOW: u64 = u64::MAX - 24;
pub const ECORRUPTED: u64 = u64::MAX - 25;
pub const EMLINK: u64 = u64::MAX - 26;
pub const EXDEV: u64 = u64::MAX - 27;
pub const EPIPE: u64 = u64::MAX - 28;
pub const ENOTSUP: u64 = u64::MAX - 29;
pub const EAGAIN: u64 = u64::MAX - 30;
pub const EINTR: u64 = u64::MAX - 31;
pub const E2BIG: u64 = u64::MAX - 32;
pub const ENODEV: u64 = u64::MAX - 33;  // No such device

/// Check whether a raw syscall return value represents an error code.
///
/// This is the **single authoritative way** to distinguish error returns
/// from valid addresses or counts.  Use this instead of ad-hoc
/// `>= u64::MAX - N` comparisons.
#[inline]
pub fn is_syscall_error(value: u64) -> bool {
    value >= SYSCALL_ERROR_THRESHOLD
}

// ---------------------------------------------------------------------------
// Virtual memory syscall constants (mmap/munmap/mprotect/brk)
// ---------------------------------------------------------------------------

/// mmap protection flags
pub const PROT_NONE: u64 = 0;
pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

/// mmap flags
pub const MAP_ANONYMOUS: u64 = 0x20;
pub const MAP_PRIVATE: u64 = 0x02;
pub const MAP_FIXED: u64 = 0x10;

/// Default user heap start (above typical text/data/bss)
pub const USER_HEAP_START: u64 = 0x0000_0010_0000_0000;

/// Default mmap region for dynamic allocations
pub const USER_MMAP_START: u64 = 0x0000_2000_0000_0000;
pub const USER_MMAP_END: u64 = 0x0000_7000_0000_0000;

// ---------------------------------------------------------------------------
// Graphics / video mode constants
// ---------------------------------------------------------------------------

/// Maximum number of video modes the BGA driver supports and the kernel
/// will report via SYS_GET_VIDEO_MODES.  Both the kernel (bga.rs) and
/// userspace callers (e.g. display_settings) must size their mode buffers
/// to at least this value; keeping the constant here prevents silent drift.
pub const VIDEO_MAX_MODES: usize = 16;

// ---------------------------------------------------------------------------
// IPC Port constants
// ---------------------------------------------------------------------------

/// Well-known ports for system services:
/// Port 1 = NAME_SERVICE (namesvc)
/// Port 2 = SERVICE_MANAGER (service_manager)
/// Port 3 = FS_SERVICE (fsd - Filesystem Daemon)
pub const PORT_FS_SERVICE: u64 = 3;
pub const PORT_BLOCK_SERVICE: u64 = 4;

// ---------------------------------------------------------------------------
// Filesystem limits
// ---------------------------------------------------------------------------

pub const FS_MAX_PATH_LEN: usize = 4096;
pub const FS_MAX_NAME_LEN: usize = 256;
pub const FS_MAX_FDS: usize = 1024;

// ---------------------------------------------------------------------------
// File open flags (O_* constants)
// ---------------------------------------------------------------------------

pub const O_RDONLY: u32 = 0x0000;
pub const O_WRONLY: u32 = 0x0001;
pub const O_RDWR: u32 = 0x0002;
pub const O_CREAT: u32 = 0x0040;
pub const O_EXCL: u32 = 0x0080;
pub const O_TRUNC: u32 = 0x0200;
pub const O_APPEND: u32 = 0x0400;
pub const O_NONBLOCK: u32 = 0x0800;
pub const O_SYNC: u32 = 0x1000;
pub const O_DIRECTORY: u32 = 0x10000;
pub const O_NOFOLLOW: u32 = 0x20000;
pub const O_CLOEXEC: u32 = 0x80000;

// ---------------------------------------------------------------------------
// Seek constants
// ---------------------------------------------------------------------------

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;

// ---------------------------------------------------------------------------
// File type masks (S_* constants for stat)
// ---------------------------------------------------------------------------

pub const S_IFMT: u32 = 0o170000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;
pub const S_IFSOCK: u32 = 0o140000;
