//! IPC Request Handler Module
//!
//! Routes incoming IPC messages to filesystem operation handlers.
//! Parses request payloads, delegates filesystem operations to an
//! injected backend, and constructs response byte arrays that
//! the kernel can forward to the requesting thread.
//!
//! ## Response wire formats (all little-endian)
//!
//! Most responses use the generic FsReply layout:
//!   [error(8) | value(8)]            (16 bytes)
//!
//! stat   → [error(8) | stat_buf(80)] (88 bytes)
//! read   → [error(8) | nbytes(8) | data(nbytes)]
//! readdir→ [error(8) | size(8)   | dirent_data(size)]

extern crate alloc;

use alloc::vec::Vec;
use alloc::string::String;
use atom_syscall::fs::FsError;
use libipc::messages::MessageType;
use crate::backend::FsBackend;
use crate::mounts::MountsManager;
use crate::journal::OpType;

// ── Error codes (must match atom_abi) ─────────────────────────────────────
const ESUCCESS: u64 = 0;
const ENOENT: u64   = u64::MAX - 11;
const EBADF: u64    = u64::MAX - 8;
const EINVAL: u64   = u64::MAX - 1;
const EIO: u64      = u64::MAX - 4;
const EISDIR: u64   = u64::MAX - 20;
const ENOTDIR: u64  = u64::MAX - 19;
const EEXIST: u64   = u64::MAX - 16;
const ENOTEMPTY: u64 = u64::MAX - 38;
const ENOTSUP: u64  = u64::MAX - 34;

// ── Simple file descriptor table ──────────────────────────────────────────

const MAX_FDS: usize = 128;
const O_ACCMODE: u32 = 0x0003;
const O_WRONLY: u32 = 0x0001;
const O_RDWR: u32 = 0x0002;
const O_CREAT: u32 = 0x0040;
const O_EXCL: u32 = 0x0080;
const O_TRUNC: u32 = 0x0200;
const O_APPEND: u32 = 0x0400;
const O_DIRECTORY: u32 = 0x10000;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;

// Small-file cache threshold: files at or below this size are cached in full
// on first read so subsequent reads are served from memory without disk I/O.
// Files above this threshold are read directly from FAT32 on every call.
const SMALL_FILE_CACHE_THRESHOLD: u64 = 8 * 1024; // 8 KiB
const FS_REPLY_BASE_SIZE: usize = 16;
const FS_MAX_READ_CHUNK: usize = libipc::MAX_MESSAGE_SIZE - FS_REPLY_BASE_SIZE;

// Scratch buffers for FsRead replies.
//
// Safety: fsd is single-threaded, so these mutable statics are only accessed
// from one execution context at a time.
static mut READ_DATA_SCRATCH: [u8; FS_MAX_READ_CHUNK] = [0u8; FS_MAX_READ_CHUNK];
static mut READ_REPLY_SCRATCH: [u8; libipc::MAX_MESSAGE_SIZE] = [0u8; libipc::MAX_MESSAGE_SIZE];

/// Tracks a single open file or directory.
#[derive(Clone)]
struct OpenFile {
    path: String,
    flags: u32,
    is_dir: bool,
    offset: u64,
    /// File size in bytes (from stat at open time).
    size: u64,
    /// First FAT cluster (0 for empty files or directories).
    first_cluster: u32,
    /// Optional small-file cache: populated on first read for files ≤ SMALL_FILE_CACHE_THRESHOLD.
    /// None means "not cached yet" (or file is too large to cache).
    cache: Option<Vec<u8>>,
}

/// The FSD IPC handler.  Owns the fd table and dispatches requests.
pub struct FsdIpcHandler<'a, 'b> {
    mounts: &'a mut MountsManager,
    backend: &'b dyn FsBackend,
    fds: [Option<OpenFile>; MAX_FDS],
    next_fd: u32,
}

impl<'a, 'b> FsdIpcHandler<'a, 'b> {
    pub fn new(mounts: &'a mut MountsManager, backend: &'b dyn FsBackend) -> Self {
        Self {
            mounts,
            backend,
            fds: core::array::from_fn(|_| None),
            next_fd: 3, // 0/1/2 reserved for stdin/stdout/stderr
        }
    }

    // ── Dispatch ──────────────────────────────────────────────────────────

    /// Dispatch incoming request to handler based on message type.
    /// Returns a byte vector that will be sent verbatim to the reply port.
    pub fn handle_request(&mut self, msg_type: MessageType, payload: &[u8]) -> Vec<u8> {
        match msg_type {
            MessageType::FsOpen    => self.handle_fs_open(payload),
            MessageType::FsClose   => self.handle_fs_close(payload),
            MessageType::FsRead    => self.handle_fs_read(payload),
            MessageType::FsWrite   => self.handle_fs_write(payload),
            MessageType::FsSeek    => self.handle_fs_seek(payload),
            MessageType::FsStat    => self.handle_fs_stat(payload),
            MessageType::FsFstat   => self.handle_fs_fstat(payload),
            MessageType::FsMkdir   => self.handle_fs_mkdir(payload),
            MessageType::FsRmdir   => self.handle_fs_rmdir(payload),
            MessageType::FsUnlink  => self.handle_fs_unlink(payload),
            MessageType::FsRename  => self.handle_fs_rename(payload),
            MessageType::FsReaddir => self.handle_fs_readdir(payload),
            MessageType::FsTruncate => self.handle_fs_truncate(payload),
            MessageType::FsFsync  => self.handle_fs_fsync(payload),
            _ => {
                atom_syscall::debug::log("fsd: unsupported msg_type, returning ENOTSUP");
                Self::make_reply(ENOTSUP, 0)
            }
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn log(msg: &str) {
        atom_syscall::debug::log(msg);
    }

    /// Build a generic 16-byte FsReply: [error(8) | value(8)]
    fn make_reply(error: u64, value: u64) -> Vec<u8> {
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(&error.to_le_bytes());
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    #[inline]
    fn encode_reply_header(buf: &mut [u8], error: u64, value: u64) {
        buf[0..8].copy_from_slice(&error.to_le_bytes());
        buf[8..16].copy_from_slice(&value.to_le_bytes());
    }

    fn alloc_fd(&mut self) -> Option<u32> {
        for i in 3..MAX_FDS {
            if self.fds[i].is_none() {
                return Some(i as u32);
            }
        }
        None
    }

    fn invalidate_file_caches(&mut self, path: &str) {
        for slot in self.fds.iter_mut() {
            let Some(open_file) = slot.as_mut() else { continue; };
            if !open_file.is_dir && open_file.path == path {
                open_file.cache = None;
            }
        }
    }

    fn retarget_open_paths(&mut self, old_path: &str, new_path: &str) {
        for slot in self.fds.iter_mut() {
            let Some(open_file) = slot.as_mut() else { continue; };
            if open_file.path == old_path {
                open_file.path = String::from(new_path);
                open_file.cache = None;
            }
        }
    }

    #[inline]
    fn stat_buf_is_dir(stat_buf: &[u8; 80]) -> bool {
        let mode = u32::from_le_bytes(stat_buf[40..44].try_into().unwrap());
        (mode & S_IFMT) == S_IFDIR
    }

    #[inline]
    fn is_open_readable(flags: u32) -> bool {
        (flags & O_ACCMODE) != O_WRONLY
    }

    #[inline]
    fn is_open_writable(flags: u32) -> bool {
        let acc = flags & O_ACCMODE;
        acc == O_WRONLY || acc == O_RDWR
    }

    /// Ensure the small-file cache is populated for `fd`.
    /// Only called for files ≤ SMALL_FILE_CACHE_THRESHOLD.
    /// Returns Err(EIO) if the read fails.
    fn ensure_small_cache(&mut self, fd: usize) -> Result<(), u64> {
        if self.fds[fd].as_ref().unwrap().cache.is_some() {
            return Ok(());
        }
        let of = self.fds[fd].as_ref().unwrap();
        let first_cluster = of.first_cluster;
        let file_size = of.size as usize;

        if file_size == 0 {
            self.fds[fd].as_mut().unwrap().cache = Some(Vec::new());
            return Ok(());
        }

        let mut data = alloc::vec![0u8; file_size];
        let n = self.backend.read_range(first_cluster, of.size, 0, &mut data)
            .map_err(|_| EIO)?;
        data.truncate(n);
        self.fds[fd].as_mut().unwrap().cache = Some(data);
        Ok(())
    }

    fn journal_record(op_type: OpType, inode: u32, payload: &[u8]) {
        let Some(jm) = crate::get_journal_manager() else {
            return;
        };

        let _tx_id = jm.begin_transaction();
        if !jm.add_operation(op_type, inode, payload) {
            let _ = jm.rollback_transaction();
            Self::log("fsd: journal add_operation failed; rollback issued");
            return;
        }
        if !jm.commit_transaction() {
            let _ = jm.rollback_transaction();
            Self::log("fsd: journal commit failed; rollback issued");
        }
    }

    // ── Open ──────────────────────────────────────────────────────────────
    //
    // Request: [path_len(4) | path_bytes | flags(4) | mode(4)]
    // Reply:   [error(8) | fd(8)]

    fn handle_fs_open(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 4 {
            return Self::make_reply(EINVAL, 0);
        }

        let path_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + path_len + 8 {
            return Self::make_reply(EINVAL, 0);
        }

        let path = match core::str::from_utf8(&payload[4..4 + path_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };
        let flags = u32::from_le_bytes([
            payload[4 + path_len],
            payload[5 + path_len],
            payload[6 + path_len],
            payload[7 + path_len],
        ]);

        Self::log(&alloc::format!("fsd: open path=\"{}\" flags={:#x}", path, flags));

        let mut stat_buf = [0u8; 80];
        let path_exists = self.backend.stat_path(path, &mut stat_buf).is_ok();
        let path_is_dir = path_exists && Self::stat_buf_is_dir(&stat_buf);
        if path_exists && (flags & O_CREAT) != 0 && (flags & O_EXCL) != 0 {
            return Self::make_reply(EEXIST, 0);
        }

        let hinted_dir = (flags & O_DIRECTORY) != 0 || path == "/" || path.ends_with('/');
        if hinted_dir {
            if !path_exists {
                return Self::make_reply(ENOENT, 0);
            }
            if !path_is_dir {
                return Self::make_reply(ENOTDIR, 0);
            }
        }

        // Actual type comes from stat when the path already exists.
        let is_dir = if path_exists { path_is_dir } else { hinted_dir };

        if is_dir {
            // Keep directory descriptors read-only.
            if Self::is_open_writable(flags) {
                return Self::make_reply(EISDIR, 0);
            }
        } else {
            // File path: create/trunc semantics.
            if !path_exists {
                if (flags & O_CREAT) == 0 {
                    return Self::make_reply(ENOENT, 0);
                }
                if self.backend.write_file(path, &[]).is_err() {
                    return Self::make_reply(ENOENT, 0);
                }
            }

            if (flags & O_TRUNC) != 0 {
                if !Self::is_open_writable(flags) {
                    return Self::make_reply(EBADF, 0);
                }
                if self.backend.write_file(path, &[]).is_err() {
                    return Self::make_reply(EIO, 0);
                }
                Self::journal_record(OpType::WriteData, 0, &[0u8; 1]);
                self.invalidate_file_caches(path);
            }
        }

        let fd = match self.alloc_fd() {
            Some(f) => f,
            None => return Self::make_reply(EIO, 0),
        };

        // Resolve first_cluster and size via open_info (no data allocation).
        let (first_cluster, file_size) = if !is_dir {
            match self.backend.open_info(path) {
                Ok((fc, sz, _)) => (fc, sz),
                Err(_) => (0, 0),
            }
        } else {
            (0, 0)
        };

        let mut initial_offset: u64 = 0;
        if !is_dir && (flags & O_APPEND) != 0 {
            initial_offset = file_size;
        }

        self.fds[fd as usize] = Some(OpenFile {
            path: String::from(path),
            flags,
            is_dir,
            offset: initial_offset,
            size: file_size,
            first_cluster,
            cache: if !is_dir && (flags & O_TRUNC) != 0 { Some(Vec::new()) } else { None },
        });

        Self::log(&alloc::format!("fsd: opened fd={} for \"{}\"", fd, path));
        Self::make_reply(ESUCCESS, fd as u64)
    }

    // ── Close ─────────────────────────────────────────────────────────────
    //
    // Request: [fd(8)]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_close(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 8 {
            return Self::make_reply(EINVAL, 0);
        }

        let fd = u64::from_le_bytes([
            payload[0], payload[1], payload[2], payload[3],
            payload[4], payload[5], payload[6], payload[7],
        ]) as usize;

        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }

        self.fds[fd] = None;
        Self::log(&alloc::format!("fsd: closed fd={}", fd));
        Self::make_reply(ESUCCESS, 0)
    }

    // ── Read ──────────────────────────────────────────────────────────────
    //
    // Request: [fd(8) | count(8)]
    // Reply:   [error(8) | bytes_read(8) | data]

    fn handle_fs_read(&mut self, payload: &[u8]) -> Vec<u8> {
        self.handle_fs_read_noalloc(payload).to_vec()
    }

    /// Heap-free FsRead handler used by the fsd main loop.
    ///
    /// The returned slice points to a static scratch buffer that is reused on
    /// every call. It is valid until the next call to this method.
    pub fn handle_fs_read_noalloc(&mut self, payload: &[u8]) -> &'static [u8] {
        unsafe {
            let reply = &mut READ_REPLY_SCRATCH;

            if payload.len() < 16 {
                Self::encode_reply_header(reply, EINVAL, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }

            let fd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
            let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
            let count = count.min(FS_MAX_READ_CHUNK);

            if fd >= MAX_FDS || self.fds[fd].is_none() {
                Self::encode_reply_header(reply, EBADF, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }

            let (is_dir, flags, file_size, offset, first_cluster) = {
                let open_file = self.fds[fd].as_ref().unwrap();
                (
                    open_file.is_dir,
                    open_file.flags,
                    open_file.size,
                    open_file.offset,
                    open_file.first_cluster,
                )
            };

            if is_dir {
                Self::encode_reply_header(reply, EISDIR, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }
            if !Self::is_open_readable(flags) {
                Self::encode_reply_header(reply, EBADF, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }

            if offset >= file_size {
                Self::encode_reply_header(reply, ESUCCESS, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }

            let available = (file_size - offset) as usize;
            let to_return = available.min(count);

            if file_size <= SMALL_FILE_CACHE_THRESHOLD {
                if let Err(err) = self.ensure_small_cache(fd) {
                    Self::encode_reply_header(reply, err, 0);
                    return &reply[..FS_REPLY_BASE_SIZE];
                }

                let off = offset as usize;
                let n = {
                    let cache = self.fds[fd].as_ref().unwrap().cache.as_ref().unwrap();
                    let n = to_return.min(cache.len().saturating_sub(off));
                    Self::encode_reply_header(reply, ESUCCESS, n as u64);
                    reply[FS_REPLY_BASE_SIZE..FS_REPLY_BASE_SIZE + n]
                        .copy_from_slice(&cache[off..off + n]);
                    n
                };

                self.fds[fd].as_mut().unwrap().offset += n as u64;
                return &reply[..FS_REPLY_BASE_SIZE + n];
            }

            let read_buf = &mut READ_DATA_SCRATCH[..to_return];
            let n = match self.backend.read_range(first_cluster, file_size, offset, read_buf) {
                Ok(n) => n.min(to_return),
                Err(_) => {
                    Self::encode_reply_header(reply, EIO, 0);
                    return &reply[..FS_REPLY_BASE_SIZE];
                }
            };

            // Offset is in-range and we requested bytes, so n==0 here means
            // backend failure/corruption. Return EIO and stop the read loop.
            if n == 0 && to_return > 0 {
                Self::encode_reply_header(reply, EIO, 0);
                return &reply[..FS_REPLY_BASE_SIZE];
            }

            Self::encode_reply_header(reply, ESUCCESS, n as u64);
            reply[FS_REPLY_BASE_SIZE..FS_REPLY_BASE_SIZE + n]
                .copy_from_slice(&read_buf[..n]);

            self.fds[fd].as_mut().unwrap().offset += n as u64;
            &reply[..FS_REPLY_BASE_SIZE + n]
        }
    }

    // ── Seek ──────────────────────────────────────────────────────────────
    //
    // Request: [fd(8) | offset(8, i64) | whence(4)]
    // Reply:   [error(8) | new_offset(8)]

    fn handle_fs_seek(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 20 {
            return Self::make_reply(EINVAL, 0);
        }

        let fd     = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        let off    = i64::from_le_bytes(payload[8..16].try_into().unwrap());
        let whence = u32::from_le_bytes(payload[16..20].try_into().unwrap());

        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }
        if self.fds[fd].as_ref().unwrap().is_dir {
            return Self::make_reply(EISDIR, 0);
        }

        let new_offset: i64 = match whence {
            0 => off,   // SEEK_SET
            1 => {      // SEEK_CUR
                self.fds[fd].as_ref().unwrap().offset as i64 + off
            }
            2 => {      // SEEK_END
                self.fds[fd].as_ref().unwrap().size as i64 + off
            }
            _ => return Self::make_reply(EINVAL, 0),
        };

        if new_offset < 0 {
            return Self::make_reply(EINVAL, 0);
        }

        self.fds[fd].as_mut().unwrap().offset = new_offset as u64;
        Self::make_reply(ESUCCESS, new_offset as u64)
    }

    // ── Write ─────────────────────────────────────────────────────────────
    //
    // Request: [fd(8) | count(8) | data]
    // Reply:   [error(8) | bytes_written(8)]

    fn handle_fs_write(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 16 {
            return Self::make_reply(EINVAL, 0);
        }

        let fd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
        if payload.len() < 16 + count {
            return Self::make_reply(EINVAL, 0);
        }
        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }
        if self.fds[fd].as_ref().unwrap().is_dir {
            return Self::make_reply(EISDIR, 0);
        }
        if !Self::is_open_writable(self.fds[fd].as_ref().unwrap().flags) {
            return Self::make_reply(EBADF, 0);
        }

        let write_offset = {
            let of = self.fds[fd].as_ref().unwrap();
            if (of.flags & O_APPEND) != 0 { of.size } else { of.offset }
        };

        let path = self.fds[fd].as_ref().unwrap().path.clone();
        let data_in = &payload[16..16 + count];
        let new_len = write_offset.saturating_add(count as u64) as usize;

        Self::log(&alloc::format!(
            "fsd: WRITE fd={} path=\"{}\" off={} size={} new_len={}",
            fd, path, write_offset, count, new_len
        ));

        // Build the new file image: read existing content, splice in new data.
        let existing_size = self.fds[fd].as_ref().unwrap().size as usize;
        let image_len = new_len.max(existing_size);
        let mut new_image = alloc::vec![0u8; image_len];

        // Read existing content into image (only if there's something to preserve).
        if existing_size > 0 {
            let fc = self.fds[fd].as_ref().unwrap().first_cluster;
            let sz = self.fds[fd].as_ref().unwrap().size;
            let _ = self.backend.read_range(fc, sz, 0, &mut new_image[..existing_size]);
        }

        // Splice new data at write_offset.
        let wo = write_offset as usize;
        new_image[wo..wo + count].copy_from_slice(data_in);
        new_image.truncate(new_len);

        match self.backend.write_file(&path, &new_image) {
            Ok(()) => {
                let mut jpl = [0u8; 16];
                jpl[0..8].copy_from_slice(&write_offset.to_le_bytes());
                jpl[8..16].copy_from_slice(&(count as u64).to_le_bytes());
                Self::journal_record(OpType::WriteData, 0, &jpl);

                // Refresh first_cluster and size from disk, then invalidate caches.
                let (new_fc, new_sz) = match self.backend.open_info(&path) {
                    Ok((fc, sz, _)) => (fc, sz),
                    Err(_) => (0, new_len as u64),
                };
                self.invalidate_file_caches(&path);

                let of = self.fds[fd].as_mut().unwrap();
                of.offset = write_offset + count as u64;
                of.size = new_sz;
                of.first_cluster = new_fc;
                // Populate small-file cache with the data we just wrote.
                if new_sz <= SMALL_FILE_CACHE_THRESHOLD {
                    of.cache = Some(new_image);
                }
                Self::make_reply(ESUCCESS, count as u64)
            }
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Stat ──────────────────────────────────────────────────────────────
    //
    // Request: [path_len(4) | path_bytes]
    // Reply:   [error(8) | stat_buf(80)]

    fn handle_fs_stat(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 4 {
            return Self::make_reply(EINVAL, 0);
        }

        let path_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + path_len {
            return Self::make_reply(EINVAL, 0);
        }

        let path = match core::str::from_utf8(&payload[4..4 + path_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        Self::log(&alloc::format!("fsd: stat path=\"{}\"", path));

        let mut stat_buf = [0u8; 80];
        match self.backend.stat_path(path, &mut stat_buf) {
            Ok(()) => {
                // Reply: [error(8) | stat_buf(80)]
                let mut resp = Vec::with_capacity(88);
                resp.extend_from_slice(&ESUCCESS.to_le_bytes());
                resp.extend_from_slice(&stat_buf);
                resp
            }
            Err(_) => Self::make_reply(ENOENT, 0),
        }
    }

    // ── Fstat ─────────────────────────────────────────────────────────────
    //
    // Request: [fd(8)]
    // Reply:   [error(8) | stat_buf(80)]

    fn handle_fs_fstat(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 8 {
            return Self::make_reply(EINVAL, 0);
        }

        let fd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }

        let path = self.fds[fd].as_ref().unwrap().path.clone();
        let mut stat_buf = [0u8; 80];
        match self.backend.stat_path(&path, &mut stat_buf) {
            Ok(()) => {
                let mut resp = Vec::with_capacity(88);
                resp.extend_from_slice(&ESUCCESS.to_le_bytes());
                resp.extend_from_slice(&stat_buf);
                resp
            }
            Err(_) => Self::make_reply(ENOENT, 0),
        }
    }

    // ── Mkdir ─────────────────────────────────────────────────────────────
    //
    // Request: [path_len(4) | path_bytes | mode(4)]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_mkdir(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 8 {
            return Self::make_reply(EINVAL, 0);
        }
        let path_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + path_len + 4 {
            return Self::make_reply(EINVAL, 0);
        }
        let path = match core::str::from_utf8(&payload[4..4 + path_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        match self.backend.mkdir(path) {
            Ok(()) => {
                Self::journal_record(OpType::AllocInode, 0, path.as_bytes());
                Self::make_reply(ESUCCESS, 0)
            }
            Err(FsError::Exists) => Self::make_reply(EEXIST, 0),
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Rmdir ─────────────────────────────────────────────────────────────
    //
    // Request: [path_len(4) | path_bytes]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_rmdir(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 4 {
            return Self::make_reply(EINVAL, 0);
        }
        let path_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + path_len {
            return Self::make_reply(EINVAL, 0);
        }
        let path = match core::str::from_utf8(&payload[4..4 + path_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        match self.backend.rmdir(path) {
            Ok(()) => {
                Self::journal_record(OpType::FreeInode, 0, path.as_bytes());
                Self::make_reply(ESUCCESS, 0)
            }
            Err(FsError::NotFound) => Self::make_reply(ENOENT, 0),
            Err(FsError::NotDir) => Self::make_reply(ENOTDIR, 0),
            Err(FsError::NotEmpty) => Self::make_reply(ENOTEMPTY, 0),
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Unlink ────────────────────────────────────────────────────────────
    //
    // Request: [path_len(4) | path_bytes]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_unlink(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 4 {
            return Self::make_reply(EINVAL, 0);
        }
        let path_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + path_len {
            return Self::make_reply(EINVAL, 0);
        }
        let path = match core::str::from_utf8(&payload[4..4 + path_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        match self.backend.unlink(path) {
            Ok(()) => {
                Self::journal_record(OpType::FreeInode, 0, path.as_bytes());
                self.invalidate_file_caches(path);
                Self::make_reply(ESUCCESS, 0)
            }
            Err(FsError::NotFound) => Self::make_reply(ENOENT, 0),
            Err(FsError::IsDir) => Self::make_reply(EISDIR, 0),
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Rename ────────────────────────────────────────────────────────────
    //
    // Request: [old_len(4) | old_path | new_len(4) | new_path]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_rename(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 8 {
            return Self::make_reply(EINVAL, 0);
        }
        let old_len = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
        if payload.len() < 4 + old_len + 4 {
            return Self::make_reply(EINVAL, 0);
        }
        let old_path = match core::str::from_utf8(&payload[4..4 + old_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };
        let new_len_off = 4 + old_len;
        let new_len = u32::from_le_bytes([
            payload[new_len_off],
            payload[new_len_off + 1],
            payload[new_len_off + 2],
            payload[new_len_off + 3],
        ]) as usize;
        if payload.len() < new_len_off + 4 + new_len {
            return Self::make_reply(EINVAL, 0);
        }
        let new_path = match core::str::from_utf8(&payload[new_len_off + 4..new_len_off + 4 + new_len]) {
            Ok(s) => s,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        match self.backend.rename(old_path, new_path) {
            Ok(()) => {
                let mut payload = Vec::with_capacity(old_path.len() + 1 + new_path.len());
                payload.extend_from_slice(old_path.as_bytes());
                payload.push(0);
                payload.extend_from_slice(new_path.as_bytes());
                Self::journal_record(OpType::UpdateInode, 0, &payload);
                self.retarget_open_paths(old_path, new_path);
                Self::make_reply(ESUCCESS, 0)
            }
            Err(FsError::NotFound) => Self::make_reply(ENOENT, 0),
            Err(FsError::Exists) => Self::make_reply(EEXIST, 0),
            Err(FsError::CrossDevice) => Self::make_reply(ENOTSUP, 0),
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Readdir ───────────────────────────────────────────────────────────
    //
    // Request: [dirfd(8) | count(8)]
    // Reply:   [error(8) | size(8) | dirent_data(size)]

    fn handle_fs_readdir(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 16 {
            return Self::make_reply(EINVAL, 0);
        }

        let dirfd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;

        if dirfd >= MAX_FDS {
            return Self::make_reply(EBADF, 0);
        }

        let dir = match &self.fds[dirfd] {
            Some(f) if f.is_dir => f.clone(),
            Some(_) => return Self::make_reply(EINVAL, 0), // not a directory
            None    => return Self::make_reply(EBADF, 0),
        };

        Self::log(&alloc::format!("fsd: readdir fd={} path=\"{}\"", dirfd, dir.path));

        // Ask kernel backend for directory listing (packed dirent format)
        let buf_size = count.min(64 * 1024);
        if buf_size == 0 {
            return Self::make_reply(EINVAL, 0);
        }
        let mut dirent_buf = alloc::vec![0u8; buf_size];
        let bytes_used = match self.backend.list_dir(&dir.path, &mut dirent_buf) {
            Ok(n) => n,
            Err(_) => return Self::make_reply(ENOENT, 0),
        };

        // Reply: [error(8) | size(8) | dirent_data]
        let mut resp = Vec::with_capacity(16 + bytes_used);
        resp.extend_from_slice(&ESUCCESS.to_le_bytes());
        resp.extend_from_slice(&(bytes_used as u64).to_le_bytes());
        resp.extend_from_slice(&dirent_buf[..bytes_used]);

        Self::log(&alloc::format!("fsd: readdir returned {} bytes", bytes_used));
        resp
    }

    // ── Truncate ──────────────────────────────────────────────────────────
    //
    // Request: [fd(8) | new_size(8)]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_truncate(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 16 {
            return Self::make_reply(EINVAL, 0);
        }
        let fd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        let new_size = u64::from_le_bytes(payload[8..16].try_into().unwrap());

        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }
        if self.fds[fd].as_ref().unwrap().is_dir {
            return Self::make_reply(EISDIR, 0);
        }
        if !Self::is_open_writable(self.fds[fd].as_ref().unwrap().flags) {
            return Self::make_reply(EBADF, 0);
        }

        let target_len = match usize::try_from(new_size) {
            Ok(v) => v,
            Err(_) => return Self::make_reply(EINVAL, 0),
        };

        let path = self.fds[fd].as_ref().unwrap().path.clone();
        let existing_size = self.fds[fd].as_ref().unwrap().size as usize;

        // Build truncated image.
        let mut new_image = alloc::vec![0u8; target_len];
        if existing_size > 0 && target_len > 0 {
            let fc = self.fds[fd].as_ref().unwrap().first_cluster;
            let sz = self.fds[fd].as_ref().unwrap().size;
            let copy_len = target_len.min(existing_size);
            let _ = self.backend.read_range(fc, sz, 0, &mut new_image[..copy_len]);
        }

        Self::log(&alloc::format!("fsd: TRUNCATE fd={} path=\"{}\" new_size={}", fd, path, target_len));

        match self.backend.write_file(&path, &new_image) {
            Ok(()) => {
                let mut jpl = [0u8; 16];
                jpl[0..8].copy_from_slice(&new_size.to_le_bytes());
                jpl[8..16].copy_from_slice(&(target_len as u64).to_le_bytes());
                Self::journal_record(OpType::UpdateInode, 0, &jpl);

                let (new_fc, new_sz) = match self.backend.open_info(&path) {
                    Ok((fc, sz, _)) => (fc, sz),
                    Err(_) => (0, target_len as u64),
                };
                self.invalidate_file_caches(&path);

                let of = self.fds[fd].as_mut().unwrap();
                if of.offset > new_sz {
                    of.offset = new_sz;
                }
                of.size = new_sz;
                of.first_cluster = new_fc;
                if new_sz <= SMALL_FILE_CACHE_THRESHOLD {
                    of.cache = Some(new_image);
                }
                Self::make_reply(ESUCCESS, 0)
            }
            Err(_) => Self::make_reply(EIO, 0),
        }
    }

    // ── Fsync ─────────────────────────────────────────────────────────────
    //
    // Request: [fd(8)]
    // Reply:   [error(8) | 0(8)]

    fn handle_fs_fsync(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 8 {
            return Self::make_reply(EINVAL, 0);
        }
        let fd = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }
        let path = self.fds[fd].as_ref().map(|of| of.path.as_str()).unwrap_or("<invalid>");
        Self::log(&alloc::format!("fsd: FSYNC fd={} path=\"{}\"", fd, path));
        Self::journal_record(OpType::Fsync, 0, &[0u8; 1]);
        match self.backend.sync_fs() {
            Ok(()) => match self.backend.flush_block_cache() {
                Ok(()) => Self::make_reply(ESUCCESS, 0),
                Err(_) => Self::make_reply(EIO, 0),
            },
            Err(_) => Self::make_reply(EIO, 0),
        }
    }
}
