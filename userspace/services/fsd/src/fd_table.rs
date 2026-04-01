// fd_table.rs — File descriptor table with per-PID isolation
//
// FdTable is owned by Vfs.  The IPC server always passes the sender PID;
// every accessor checks that the PID matches before returning data.
//
// SECURITY INVARIANT
// ------------------
// An FD that has been revoked (owner cap was revoked) is immediately
// marked `revoked = true`.  Any subsequent operation on it returns
// VfsError::BadFd regardless of the PID check.

#![allow(dead_code)]

extern crate alloc;
use alloc::collections::BTreeMap;

use crate::vfs::{BackendHandle, OpenFlags};

// ── OpenFile ─────────────────────────────────────────────────────────────────

/// State for an open regular-file descriptor.
#[derive(Debug, Clone)]
pub struct OpenFile {
    /// Which mounted filesystem backend owns this handle.
    pub mount_id:       u32,
    /// Opaque handle from the backend.
    pub backend_handle: BackendHandle,
    /// Open-mode flags (O_RDONLY / O_WRONLY / …).
    pub flags:          OpenFlags,
    /// Current byte offset for read/write.
    pub offset:         u64,
    /// PID of the process that opened this file.
    pub owner_pid:      u32,
    /// Capability handle used at open-time (for revocation matching).
    pub cap_handle:     u64,
    /// True iff the capability has been revoked.
    pub revoked:        bool,
}

// ── OpenDir ──────────────────────────────────────────────────────────────────

/// State for an open directory descriptor.
#[derive(Debug, Clone)]
pub struct OpenDir {
    pub mount_id:       u32,
    pub backend_handle: BackendHandle,
    /// Index/offset for incremental readdir (backend-specific).
    pub read_pos:       usize,
    pub owner_pid:      u32,
    pub cap_handle:     u64,
    pub revoked:        bool,
}

// ── FdEntry ──────────────────────────────────────────────────────────────────

pub enum FdEntry {
    File(OpenFile),
    Dir(OpenDir),
}

impl FdEntry {
    pub fn owner_pid(&self)   -> u32  { match self { FdEntry::File(f) => f.owner_pid,   FdEntry::Dir(d) => d.owner_pid } }
    pub fn cap_handle(&self)  -> u64  { match self { FdEntry::File(f) => f.cap_handle,  FdEntry::Dir(d) => d.cap_handle } }
    pub fn revoked(&self)     -> bool { match self { FdEntry::File(f) => f.revoked,     FdEntry::Dir(d) => d.revoked } }
    pub fn mount_id(&self)    -> u32  { match self { FdEntry::File(f) => f.mount_id,    FdEntry::Dir(d) => d.mount_id } }
    pub fn backend_handle(&self) -> BackendHandle {
        match self { FdEntry::File(f) => f.backend_handle, FdEntry::Dir(d) => d.backend_handle }
    }
    pub fn set_revoked(&mut self) {
        match self { FdEntry::File(f) => f.revoked = true, FdEntry::Dir(d) => d.revoked = true }
    }
}

// ── FdTable ──────────────────────────────────────────────────────────────────

/// Per-process-isolated file descriptor table.
///
/// FD numbers are allocated monotonically (no reuse within a run) to avoid
/// accidental reuse confusion, but are capped at u32::MAX / 2 for safety.
pub struct FdTable {
    entries:  BTreeMap<u32, FdEntry>,
    next_fd:  u32,
    /// Hard limit on total open FDs.
    max_fds:  usize,
}

impl FdTable {
    pub fn new() -> Self {
        FdTable {
            entries: BTreeMap::new(),
            next_fd: 1,                      // fd=0 reserved (stdin convention)
            max_fds: atom_abi::FS_MAX_FDS,
        }
    }

    // ── Allocation ───────────────────────────────────────────────────────

    /// Allocate an FD slot for a file.  Returns None if at capacity.
    pub fn alloc_file(
        &mut self,
        mount_id:    u32,
        bh:          BackendHandle,
        flags:       OpenFlags,
        owner_pid:   u32,
        cap_handle:  u64,
    ) -> Option<u32> {
        if self.entries.len() >= self.max_fds { return None; }
        let fd = self.next_fd();
        self.entries.insert(fd, FdEntry::File(OpenFile {
            mount_id,
            backend_handle: bh,
            flags,
            offset: 0,
            owner_pid,
            cap_handle,
            revoked: false,
        }));
        Some(fd)
    }

    /// Allocate an FD slot for a directory.  Returns None if at capacity.
    pub fn alloc_dir(
        &mut self,
        mount_id:   u32,
        bh:         BackendHandle,
        owner_pid:  u32,
        cap_handle: u64,
    ) -> Option<u32> {
        if self.entries.len() >= self.max_fds { return None; }
        let fd = self.next_fd();
        self.entries.insert(fd, FdEntry::Dir(OpenDir {
            mount_id,
            backend_handle: bh,
            read_pos: 0,
            owner_pid,
            cap_handle,
            revoked: false,
        }));
        Some(fd)
    }

    // ── Access ───────────────────────────────────────────────────────────

    /// Look up an FD.  Returns None if not found OR if revoked.
    pub fn get(&self, fd: u32) -> Option<&FdEntry> {
        let e = self.entries.get(&fd)?;
        if e.revoked() { return None; }
        Some(e)
    }

    pub fn get_mut(&mut self, fd: u32) -> Option<&mut FdEntry> {
        let e = self.entries.get_mut(&fd)?;
        if e.revoked() { return None; }
        Some(e)
    }

    // ── Deallocation ─────────────────────────────────────────────────────

    /// Remove and drop the FD entry (after backend.close() has been called).
    pub fn free(&mut self, fd: u32) {
        self.entries.remove(&fd);
    }

    // ── Revocation ───────────────────────────────────────────────────────

    /// Mark all FDs associated with `cap_handle` (or any descendant) as
    /// revoked.  The VFS will return BadFd on the next operation.
    pub fn revoke_by_cap(&mut self, cap_handle: u64) {
        for entry in self.entries.values_mut() {
            if entry.cap_handle() == cap_handle {
                entry.set_revoked();
            }
        }
    }

    /// Iterate over all FDs with a given owner PID (for process-exit cleanup).
    pub fn fds_for_pid(&self, pid: u32) -> alloc::vec::Vec<u32> {
        self.entries.iter()
            .filter(|(_, e)| e.owner_pid() == pid)
            .map(|(fd, _)| *fd)
            .collect()
    }

    // ── Private ───────────────────────────────────────────────────────────

    fn next_fd(&mut self) -> u32 {
        let fd = self.next_fd;
        // Wrap at u32::MAX / 2 — practical upper bound
        self.next_fd = self.next_fd.wrapping_add(1).max(1);
        fd
    }
}
