//! Defensive per-request limits for the filesystem daemon.
//!
//! Every value a *client* can influence is bounded here so that a malicious or
//! buggy caller cannot:
//!   * cause an arbitrarily large allocation by lying about a size field;
//!   * exhaust file descriptors / mount points / cache buffers;
//!   * make `fsd` read or write more than a single bounded request worth of
//!     data per IPC round-trip.
//!
//! A request that violates a limit is rejected with a clear error
//! (`EINVAL`/`EMSGSIZE`) and the daemon keeps running — it is never a panic and
//! never an unbounded allocation. These constants are the single source of
//! truth; the IPC handlers consult them before touching the heap.
//!
//! IPC messages are themselves capped at `libipc::MAX_MESSAGE_SIZE` (4 KiB), so
//! several of these limits are tighter expressions of that envelope; declaring
//! them explicitly documents intent and keeps the validation independent of the
//! transport size.

/// Error codes (match `atom_abi`). Re-declared locally so the limits module has
/// no dependency on the IPC handler internals.
pub const EINVAL: u64 = u64::MAX - 1;
/// Message too long for a single request (`EMSGSIZE`).
pub const EMSGSIZE: u64 = u64::MAX - 6;

/// Maximum byte length of a path accepted in any request. FAT32 long paths stay
/// well under this; anything larger is a protocol abuse.
pub const MAX_PATH_LEN: usize = 1024;

/// Maximum request payload `fsd` will parse (excludes the IPC/reply-port
/// framing). Bounded by the IPC envelope.
pub const MAX_REQUEST_PAYLOAD: usize = libipc::MAX_MESSAGE_SIZE;

/// Maximum number of bytes a single read may return. The wire format also caps
/// this, but we clamp explicitly so a huge `count` field never drives an
/// allocation.
pub const MAX_READ_CHUNK: usize = libipc::MAX_MESSAGE_SIZE - 16;

/// Maximum number of bytes a single write request may carry.
pub const MAX_WRITE_CHUNK: usize = libipc::MAX_MESSAGE_SIZE - 16;

/// Maximum size of a file image `fsd` will materialise in memory for a
/// read-modify-write. Bounds the largest single heap allocation in the daemon.
pub const MAX_FILE_IMAGE: usize = 1024 * 1024; // 1 MiB

/// Maximum number of simultaneously open handles (fd table size).
pub const MAX_OPEN_HANDLES: usize = 128;

/// Maximum number of in-flight (pending) requests buffered before the daemon
/// applies back-pressure by processing them.
pub const MAX_PENDING_REQUESTS: usize = 64;

/// Maximum number of directory entries returned by a single `readdir`.
pub const MAX_DIR_ENTRIES: usize = 256;

/// Maximum number of mount points.
pub const MAX_MOUNT_POINTS: usize = 16;

/// Maximum number of small-file cache buffers held at once.
pub const MAX_CACHE_BUFFERS: usize = 64;

/// Validate a client-supplied path length. Returns the error code to reply with
/// on violation, or `Ok(())`.
#[inline]
pub fn check_path_len(path_len: usize) -> Result<(), u64> {
    if path_len == 0 || path_len > MAX_PATH_LEN {
        Err(EINVAL)
    } else {
        Ok(())
    }
}

/// Clamp a client-supplied read `count` to a safe bound. Never returns more
/// than [`MAX_READ_CHUNK`], so the caller cannot force an oversized allocation
/// by claiming a huge count.
#[inline]
pub fn clamp_read_count(count: usize) -> usize {
    count.min(MAX_READ_CHUNK)
}

/// Validate a client-supplied write `count`. Returns the error to reply with on
/// violation.
#[inline]
pub fn check_write_count(count: usize) -> Result<(), u64> {
    if count > MAX_WRITE_CHUNK {
        Err(EMSGSIZE)
    } else {
        Ok(())
    }
}

/// Validate the in-memory file-image size for a read-modify-write.
#[inline]
pub fn check_file_image(len: usize) -> Result<(), u64> {
    if len > MAX_FILE_IMAGE {
        Err(EMSGSIZE)
    } else {
        Ok(())
    }
}

/// Clamp a client-supplied directory-entry count.
#[inline]
pub fn clamp_dir_entries(count: usize) -> usize {
    count.min(MAX_DIR_ENTRIES)
}
