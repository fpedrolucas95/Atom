//! IPC Request Handler Module
//!
//! Routes incoming IPC messages to filesystem operation handlers.
//! Parses request payloads, delegates to the kernel FAT32 backend via
//! the `kern_fs_*` syscalls, and constructs response byte arrays that
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
use libipc::messages::MessageType;
use crate::mounts::MountsManager;

// ── Error codes (must match atom_abi) ─────────────────────────────────────
const ESUCCESS: u64 = 0;
const ENOENT: u64   = u64::MAX - 11;
const EBADF: u64    = u64::MAX - 8;
const EINVAL: u64   = u64::MAX - 1;
const EIO: u64      = u64::MAX - 4;
const EISDIR: u64   = u64::MAX - 20;
const ENOTSUP: u64  = u64::MAX - 34;

// ── Simple file descriptor table ──────────────────────────────────────────

const MAX_FDS: usize = 128;

/// Tracks a single open "file" (file or directory).
#[derive(Clone)]
struct OpenFile {
    path: String,
    flags: u32,
    is_dir: bool,
    offset: usize,
    /// Cached file data (read once on open if not a directory).
    data: Option<Vec<u8>>,
}

/// The FSD IPC handler.  Owns the fd table and dispatches requests.
pub struct FsdIpcHandler<'a> {
    mounts: &'a mut MountsManager,
    fds: [Option<OpenFile>; MAX_FDS],
    next_fd: u32,
}

impl<'a> FsdIpcHandler<'a> {
    pub fn new(mounts: &'a mut MountsManager) -> Self {
        Self {
            mounts,
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
            MessageType::FsStat    => self.handle_fs_stat(payload),
            MessageType::FsReaddir => self.handle_fs_readdir(payload),
            MessageType::FsSeek   => self.handle_fs_seek(payload),
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

    fn alloc_fd(&mut self) -> Option<u32> {
        for i in 3..MAX_FDS {
            if self.fds[i].is_none() {
                return Some(i as u32);
            }
        }
        None
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

        let o_directory: u32 = 0x10000; // O_DIRECTORY from atom_abi

        // Check if this is a directory open
        let is_dir = (flags & o_directory) != 0 || path == "/" || path.ends_with('/');

        if is_dir {
            // Validate that the directory exists via kernel backend
            let mut tmp = [0u8; 2048];
            match atom_syscall::fs::kern_fs_list_dir(path, &mut tmp) {
                Ok(_) => {} // directory exists
                Err(_) => {
                    // Maybe it's like "/" that needs special handling
                    if path != "/" {
                        return Self::make_reply(ENOENT, 0);
                    }
                }
            }
        } else {
            // Validate file exists by reading 0-byte check (stat)
            let mut stat_buf = [0u8; 80];
            match atom_syscall::fs::kern_fs_stat_path(path, &mut stat_buf) {
                Ok(()) => {}
                Err(_) => return Self::make_reply(ENOENT, 0),
            }
        }

        let fd = match self.alloc_fd() {
            Some(f) => f,
            None => return Self::make_reply(EIO, 0),
        };

        self.fds[fd as usize] = Some(OpenFile {
            path: String::from(path),
            flags,
            is_dir,
            offset: 0,
            data: None,
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
        if payload.len() < 16 {
            return Self::make_reply(EINVAL, 0);
        }

        let fd    = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as usize;
        let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;

        if fd >= MAX_FDS || self.fds[fd].is_none() {
            return Self::make_reply(EBADF, 0);
        }

        if self.fds[fd].as_ref().unwrap().is_dir {
            return Self::make_reply(EISDIR, 0);
        }

        // Ensure the full file is cached on first access.  This avoids reading
        // from byte 0 on every call and makes offset-based reads correct.
        if self.fds[fd].as_ref().unwrap().data.is_none() {
            let path = self.fds[fd].as_ref().unwrap().path.clone();

            // Get file size via stat so we allocate an exact buffer.
            let mut stat_buf = [0u8; 80];
            if atom_syscall::fs::kern_fs_stat_path(&path, &mut stat_buf).is_err() {
                return Self::make_reply(EIO, 0);
            }
            let file_size =
                u64::from_le_bytes(stat_buf[0..8].try_into().unwrap()) as usize;

            if file_size > 0 {
                let mut data = alloc::vec![0u8; file_size];
                let n = match atom_syscall::fs::kern_fs_read_file(&path, &mut data) {
                    Ok(n) => n,
                    Err(_) => return Self::make_reply(EIO, 0),
                };
                data.truncate(n);
                self.fds[fd].as_mut().unwrap().data = Some(data);
            }
            // Empty file: data stays None; reads will hit the EOF path below.
        }

        // Serve from the in-memory cache.
        let file_len = self.fds[fd].as_ref().unwrap()
            .data.as_ref().map_or(0, |d| d.len());
        let offset = self.fds[fd].as_ref().unwrap().offset;

        if count == 0 || offset >= file_len {
            // EOF — reply with 0 bytes read
            let mut resp = Vec::with_capacity(16);
            resp.extend_from_slice(&ESUCCESS.to_le_bytes());
            resp.extend_from_slice(&0u64.to_le_bytes());
            return resp;
        }

        let available = file_len - offset;
        let to_return = available.min(count);

        // Build response: [error(8) | bytes_read(8) | data]
        let mut resp = Vec::with_capacity(16 + to_return);
        resp.extend_from_slice(&ESUCCESS.to_le_bytes());
        resp.extend_from_slice(&(to_return as u64).to_le_bytes());
        {
            let data = self.fds[fd].as_ref().unwrap().data.as_ref().unwrap();
            resp.extend_from_slice(&data[offset..offset + to_return]);
        }

        self.fds[fd].as_mut().unwrap().offset += to_return;
        resp
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
                let cur = self.fds[fd].as_ref().unwrap().offset as i64;
                cur + off
            }
            2 => {      // SEEK_END — need to know file size
                // Ensure file data is cached so we have an authoritative length.
                if self.fds[fd].as_ref().unwrap().data.is_none() {
                    let path = self.fds[fd].as_ref().unwrap().path.clone();
                    let mut stat_buf = [0u8; 80];
                    if atom_syscall::fs::kern_fs_stat_path(&path, &mut stat_buf).is_err() {
                        return Self::make_reply(EIO, 0);
                    }
                    let file_size =
                        u64::from_le_bytes(stat_buf[0..8].try_into().unwrap()) as usize;
                    if file_size > 0 {
                        let mut data = alloc::vec![0u8; file_size];
                        let n = match atom_syscall::fs::kern_fs_read_file(&path, &mut data) {
                            Ok(n) => n,
                            Err(_) => return Self::make_reply(EIO, 0),
                        };
                        data.truncate(n);
                        self.fds[fd].as_mut().unwrap().data = Some(data);
                    }
                }
                let file_len = self.fds[fd].as_ref().unwrap()
                    .data.as_ref().map_or(0, |d| d.len()) as i64;
                file_len + off
            }
            _ => return Self::make_reply(EINVAL, 0),
        };

        if new_offset < 0 {
            return Self::make_reply(EINVAL, 0);
        }

        self.fds[fd].as_mut().unwrap().offset = new_offset as usize;
        Self::make_reply(ESUCCESS, new_offset as u64)
    }

    // ── Write ─────────────────────────────────────────────────────────────
    //
    // Request: [fd(8) | count(8) | data]
    // Reply:   [error(8) | bytes_written(8)]

    fn handle_fs_write(&mut self, _payload: &[u8]) -> Vec<u8> {
        // FAT32 backend is read-only for now
        Self::make_reply(ENOTSUP, 0)
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
        match atom_syscall::fs::kern_fs_stat_path(path, &mut stat_buf) {
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
        let buf_size = count.min(4096);
        let mut dirent_buf = alloc::vec![0u8; buf_size];
        let bytes_used = match atom_syscall::fs::kern_fs_list_dir(&dir.path, &mut dirent_buf) {
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
}
