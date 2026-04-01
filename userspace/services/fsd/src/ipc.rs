//! IPC Request Handler for fsd
//!
//! Receives messages from userspace on PORT_FS_SERVICE (3), dispatches
//! them to the VFS layer, and serialises the replies.
//!
//! ## Wire formats (all little-endian, all responses have a leading u64 error)
//!
//!   open    req: [path_len(4) | path | flags(4) | cap(8) | pid(4)]
//!           rep: [error(8) | fd(8)]
//!   close   req: [fd(8) | pid(4)]
//!           rep: [error(8) | 0(8)]
//!   read    req: [fd(8) | count(8) | pid(4)]
//!           rep: [error(8) | bytes_read(8) | data]
//!   write   req: [fd(8) | count(8) | pid(4) | data]
//!           rep: [error(8) | bytes_written(8)]
//!   seek    req: [fd(8) | offset(8,i64) | whence(4) | pid(4)]
//!           rep: [error(8) | new_offset(8)]
//!   fstat   req: [fd(8) | pid(4)]
//!           rep: [error(8) | stat_buf(80)]
//!   stat    req: [path_len(4) | path]
//!           rep: [error(8) | stat_buf(80)]
//!   readdir req: [fd(8) | pid(4)]
//!           rep: [error(8) | count(8) | [name_len(2) | name | stat(80)]*]
//!   mkdir   req: [path_len(4) | path]
//!           rep: [error(8) | 0(8)]
//!   unlink  req: [path_len(4) | path]
//!           rep: [error(8) | 0(8)]
//!   rmdir   req: [path_len(4) | path]
//!           rep: [error(8) | 0(8)]
//!   rename  req: [old_len(4) | old | new_len(4) | new]
//!           rep: [error(8) | 0(8)]
//!   truncate req:[fd(8) | new_size(8) | pid(4)]
//!           rep: [error(8) | 0(8)]
//!   fsync   req: [fd(8) | pid(4)]
//!           rep: [error(8) | 0(8)]
//!   statvfs req: [path_len(4) | path]
//!           rep: [error(8) | statvfs_buf(64)]

extern crate alloc;

use alloc::vec::Vec;
use libipc::messages::MessageType;
use crate::capabilities::CapabilityEnforcer;
use crate::error::{VfsError, VfsResult};
use crate::vfs::{OpenFlags, Vfs, Whence};

// ── Error helpers ─────────────────────────────────────────────────────────

fn vfs_err_to_abi(e: VfsError) -> u64 { e.to_abi() }

// ── FsdIpcHandler ─────────────────────────────────────────────────────────

/// Stateless IPC dispatcher: owns references to the VFS and capability enforcer.
pub struct FsdIpcHandler<'a> {
    vfs:  &'a mut Vfs,
    caps: &'a mut CapabilityEnforcer,
}

impl<'a> FsdIpcHandler<'a> {
    pub fn new(vfs: &'a mut Vfs, caps: &'a mut CapabilityEnforcer) -> Self {
        FsdIpcHandler { vfs, caps }
    }

    // ── Dispatch ──────────────────────────────────────────────────────────

    pub fn handle_request(&mut self, msg_type: MessageType, payload: &[u8]) -> Vec<u8> {
        match msg_type {
            MessageType::FsOpen        => self.handle_open(payload),
            MessageType::FsClose       => self.handle_close(payload),
            MessageType::FsRead        => self.handle_read(payload),
            MessageType::FsWrite       => self.handle_write(payload),
            MessageType::FsSeek        => self.handle_seek(payload),
            MessageType::FsFstat       => self.handle_fstat(payload),
            MessageType::FsStat        => self.handle_stat(payload),
            MessageType::FsReaddir     => self.handle_readdir(payload),
            MessageType::FsMkdir       => self.handle_mkdir(payload),
            MessageType::FsUnlink      => self.handle_unlink(payload),
            MessageType::FsRmdir       => self.handle_rmdir(payload),
            MessageType::FsRename      => self.handle_rename(payload),
            MessageType::FsTruncate    => self.handle_truncate(payload),
            MessageType::FsFsync       => self.handle_fsync(payload),
            MessageType::FsStatvfs     => self.handle_statvfs(payload),
            _ => Self::err_reply(VfsError::NotSupported),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn ok_reply(value: u64) -> Vec<u8> {
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(&0u64.to_le_bytes());  // error = 0
        v.extend_from_slice(&value.to_le_bytes());
        v
    }

    fn err_reply(e: VfsError) -> Vec<u8> {
        let mut v = Vec::with_capacity(16);
        v.extend_from_slice(&e.to_abi().to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
        v
    }

    fn read_path(payload: &[u8], offset: usize) -> Option<(&str, usize)> {
        if payload.len() < offset + 4 { return None; }
        let len = u32::from_le_bytes([
            payload[offset], payload[offset+1], payload[offset+2], payload[offset+3],
        ]) as usize;
        let end = offset + 4 + len;
        if payload.len() < end { return None; }
        let s = core::str::from_utf8(&payload[offset+4..end]).ok()?;
        Some((s, end))
    }

    // ── open ──────────────────────────────────────────────────────────────
    // req: [path_len(4) | path | flags(4) | cap(8) | pid(4)]
    // rep: [error(8) | fd(8)]

    fn handle_open(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, after_path)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        if payload.len() < after_path + 16 {
            return Self::err_reply(VfsError::InvalidArgument);
        }
        let flags_raw = u32::from_le_bytes(payload[after_path..after_path+4].try_into().unwrap());
        let cap_handle = u64::from_le_bytes(payload[after_path+4..after_path+12].try_into().unwrap());
        let pid = u32::from_le_bytes(payload[after_path+12..after_path+16].try_into().unwrap());

        let flags = OpenFlags(flags_raw);

        // Capability check (full revalidation on open)
        use crate::capabilities::CapPermissions;
        let required = if flags.writable() {
            CapPermissions::READ | CapPermissions::WRITE
        } else {
            CapPermissions::READ
        };
        if let Err(e) = self.caps.check_full(cap_handle, pid, required) {
            return Self::err_reply(e);
        }

        match self.vfs.open(path, flags, pid, cap_handle) {
            Ok(fd)  => Self::ok_reply(fd as u64),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── close ─────────────────────────────────────────────────────────────
    // req: [fd(8) | pid(4)]
    // rep: [error(8) | 0(8)]

    fn handle_close(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 12 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd  = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let pid = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        match self.vfs.close(fd, pid) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── read ──────────────────────────────────────────────────────────────
    // req: [fd(8) | count(8) | pid(4)]
    // rep: [error(8) | bytes_read(8) | data]

    fn handle_read(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 20 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd    = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
        let pid   = u32::from_le_bytes(payload[16..20].try_into().unwrap());

        // Clamp to 64 KiB per request
        let count = count.min(65536);
        let mut buf = alloc::vec![0u8; count];

        match self.vfs.read(fd, pid, &mut buf) {
            Ok(n) => {
                let mut resp = Vec::with_capacity(16 + n);
                resp.extend_from_slice(&0u64.to_le_bytes());
                resp.extend_from_slice(&(n as u64).to_le_bytes());
                resp.extend_from_slice(&buf[..n]);
                resp
            }
            Err(e) => Self::err_reply(e),
        }
    }

    // ── write ─────────────────────────────────────────────────────────────
    // req: [fd(8) | count(8) | pid(4) | data]
    // rep: [error(8) | bytes_written(8)]

    fn handle_write(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 20 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd    = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let count = u64::from_le_bytes(payload[8..16].try_into().unwrap()) as usize;
        let pid   = u32::from_le_bytes(payload[16..20].try_into().unwrap());

        if payload.len() < 20 + count {
            return Self::err_reply(VfsError::InvalidArgument);
        }
        let data = &payload[20..20 + count];

        match self.vfs.write(fd, pid, data) {
            Ok(n)  => Self::ok_reply(n as u64),
            Err(e) => Self::err_reply(e),
        }
    }

    // ── seek ──────────────────────────────────────────────────────────────
    // req: [fd(8) | offset(8,i64) | whence(4) | pid(4)]
    // rep: [error(8) | new_offset(8)]

    fn handle_seek(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 24 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd     = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let offset = i64::from_le_bytes(payload[8..16].try_into().unwrap());
        let whence_raw = u32::from_le_bytes(payload[16..20].try_into().unwrap());
        let pid    = u32::from_le_bytes(payload[20..24].try_into().unwrap());

        let Some(whence) = Whence::from_raw(whence_raw) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };

        match self.vfs.seek(fd, pid, offset, whence) {
            Ok(pos) => Self::ok_reply(pos),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── fstat ─────────────────────────────────────────────────────────────
    // req: [fd(8) | pid(4)]
    // rep: [error(8) | stat_buf(80)]

    fn handle_fstat(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 12 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd  = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let pid = u32::from_le_bytes(payload[8..12].try_into().unwrap());

        match self.vfs.fstat(fd, pid) {
            Ok(s) => {
                let mut resp = Vec::with_capacity(88);
                resp.extend_from_slice(&0u64.to_le_bytes());
                // Serialize InodeStat (80 bytes, #[repr(C)])
                let bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &s as *const _ as *const u8,
                        core::mem::size_of::<crate::vfs::InodeStat>(),
                    )
                };
                resp.extend_from_slice(bytes);
                resp
            }
            Err(e) => Self::err_reply(e),
        }
    }

    // ── stat ──────────────────────────────────────────────────────────────
    // req: [path_len(4) | path]
    // rep: [error(8) | stat_buf(80)]

    fn handle_stat(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, _)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.stat(path) {
            Ok(s) => {
                let mut resp = Vec::with_capacity(88);
                resp.extend_from_slice(&0u64.to_le_bytes());
                let bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &s as *const _ as *const u8,
                        core::mem::size_of::<crate::vfs::InodeStat>(),
                    )
                };
                resp.extend_from_slice(bytes);
                resp
            }
            Err(e) => Self::err_reply(e),
        }
    }

    // ── readdir ───────────────────────────────────────────────────────────
    // req: [fd(8) | pid(4)]
    // rep: [error(8) | count(8) | [name_len(2) | name | stat(80)]*]

    fn handle_readdir(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 12 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd  = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let pid = u32::from_le_bytes(payload[8..12].try_into().unwrap());

        match self.vfs.readdir_fd(fd, pid) {
            Ok(entries) => {
                let count = entries.len() as u64;
                let mut resp = Vec::new();
                resp.extend_from_slice(&0u64.to_le_bytes());     // error
                resp.extend_from_slice(&count.to_le_bytes());    // count
                for e in &entries {
                    let name_bytes = e.name.as_bytes();
                    let name_len = name_bytes.len().min(255) as u16;
                    resp.extend_from_slice(&name_len.to_le_bytes());
                    resp.extend_from_slice(&name_bytes[..name_len as usize]);
                    let stat_bytes: &[u8] = unsafe {
                        core::slice::from_raw_parts(
                            &e.stat as *const _ as *const u8,
                            core::mem::size_of::<crate::vfs::InodeStat>(),
                        )
                    };
                    resp.extend_from_slice(stat_bytes);
                }
                resp
            }
            Err(e) => Self::err_reply(e),
        }
    }

    // ── mkdir ─────────────────────────────────────────────────────────────
    // req: [path_len(4) | path]
    // rep: [error(8) | 0(8)]

    fn handle_mkdir(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, _)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.mkdir(path) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── unlink ────────────────────────────────────────────────────────────
    // req: [path_len(4) | path]
    // rep: [error(8) | 0(8)]

    fn handle_unlink(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, _)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.unlink(path) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── rmdir ─────────────────────────────────────────────────────────────
    // req: [path_len(4) | path]
    // rep: [error(8) | 0(8)]

    fn handle_rmdir(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, _)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.rmdir(path) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── rename ────────────────────────────────────────────────────────────
    // req: [old_len(4) | old | new_len(4) | new]
    // rep: [error(8) | 0(8)]

    fn handle_rename(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((old_path, after_old)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        let Some((new_path, _)) = Self::read_path(payload, after_old) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.rename(old_path, new_path) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── truncate ──────────────────────────────────────────────────────────
    // req: [fd(8) | new_size(8) | pid(4)]
    // rep: [error(8) | 0(8)]

    fn handle_truncate(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 20 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd       = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let new_size = u64::from_le_bytes(payload[8..16].try_into().unwrap());
        let pid      = u32::from_le_bytes(payload[16..20].try_into().unwrap());
        match self.vfs.truncate(fd, pid, new_size) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── fsync ─────────────────────────────────────────────────────────────
    // req: [fd(8) | pid(4)]
    // rep: [error(8) | 0(8)]

    fn handle_fsync(&mut self, payload: &[u8]) -> Vec<u8> {
        if payload.len() < 12 { return Self::err_reply(VfsError::InvalidArgument); }
        let fd  = u64::from_le_bytes(payload[0..8].try_into().unwrap()) as u32;
        let pid = u32::from_le_bytes(payload[8..12].try_into().unwrap());
        match self.vfs.fsync(fd, pid) {
            Ok(())  => Self::ok_reply(0),
            Err(e)  => Self::err_reply(e),
        }
    }

    // ── statvfs ───────────────────────────────────────────────────────────
    // req: [path_len(4) | path]
    // rep: [error(8) | statvfs_buf(64)]

    fn handle_statvfs(&mut self, payload: &[u8]) -> Vec<u8> {
        let Some((path, _)) = Self::read_path(payload, 0) else {
            return Self::err_reply(VfsError::InvalidArgument);
        };
        match self.vfs.statvfs(path) {
            Ok(s) => {
                let mut resp = Vec::with_capacity(72);
                resp.extend_from_slice(&0u64.to_le_bytes());
                let bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        &s as *const _ as *const u8,
                        core::mem::size_of::<crate::vfs::StatVfs>(),
                    )
                };
                resp.extend_from_slice(bytes);
                resp
            }
            Err(e) => Self::err_reply(e),
        }
    }
}
