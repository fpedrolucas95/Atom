// vfs.rs — Virtual Filesystem Switch for fsd
//
// Owns:
//   1. FileSystem trait — all FS backends (FAT32, tmpfs, etc.) implement this.
//   2. Vfs struct — routes IPC requests to the correct backend by
//      mount-point prefix matching.  Owns all Box<dyn FileSystem>.
//   3. Integrates with FdTable for per-PID FD isolation.
//
// Design rule: ZERO FAT32 knowledge here.  FAT32 is one impl of FileSystem.

#![allow(dead_code)]

extern crate alloc;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::VfsError;
use crate::fd_table::{FdTable, FdEntry, OpenFile, OpenDir};
use crate::mounts::MountEntry;

// ── VfsResult ────────────────────────────────────────────────────────────────

pub type VfsResult<T> = Result<T, VfsError>;

// ── BackendHandle ────────────────────────────────────────────────────────────

/// Opaque handle issued by a FileSystem backend on open().
/// The VFS never inspects the inner bits; the issuing backend encodes what
/// it needs (e.g. FAT32 uses cluster number + dir entry offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BackendHandle(pub u64);

impl BackendHandle {
    pub const INVALID: BackendHandle = BackendHandle(u64::MAX);
    pub fn is_valid(self) -> bool { self != Self::INVALID }
}

// ── OpenFlags ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    pub fn readable(self) -> bool {
        // O_RDONLY = 0 — readable unless write-only without RDWR
        self.0 & atom_abi::O_WRONLY == 0 || self.0 & atom_abi::O_RDWR != 0
    }
    pub fn writable(self) -> bool {
        self.0 & atom_abi::O_WRONLY != 0 || self.0 & atom_abi::O_RDWR != 0
    }
    pub fn create(self)    -> bool { self.0 & atom_abi::O_CREAT != 0 }
    pub fn excl(self)      -> bool { self.0 & atom_abi::O_EXCL  != 0 }
    pub fn trunc(self)     -> bool { self.0 & atom_abi::O_TRUNC != 0 }
    pub fn append(self)    -> bool { self.0 & atom_abi::O_APPEND != 0 }
    pub fn directory(self) -> bool { self.0 & atom_abi::O_DIRECTORY != 0 }
}

// ── Whence ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whence { Set, Cur, End }

impl Whence {
    pub fn from_raw(v: u32) -> Option<Self> {
        match v {
            0 => Some(Whence::Set),
            1 => Some(Whence::Cur),
            2 => Some(Whence::End),
            _ => None,
        }
    }
}

// ── InodeStat ────────────────────────────────────────────────────────────────

/// Generic inode metadata.  80 bytes, matches FsStatBuf wire format.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct InodeStat {
    /// Unique id within volume (FAT32: cluster number of dir entry).
    pub inode:    u64,
    /// File size in bytes (0 for directories).
    pub size:     u64,
    /// Mode: S_IFREG | S_IFDIR | permission bits.
    pub mode:     u32,
    pub _pad0:    u32,
    /// Last-modification time, ns since FAT epoch 1980-01-01.
    pub mtime_ns: u64,
    /// Last-access time, ns since FAT epoch (date-only for FAT32).
    pub atime_ns: u64,
    /// Creation time, ns since FAT epoch.
    pub ctime_ns: u64,
    pub uid:      u16,
    pub gid:      u16,
    pub nlinks:   u32,
    pub _reserved: [u8; 8],
}

// ── DirEntry ─────────────────────────────────────────────────────────────────

/// A directory listing entry (name + metadata) as returned to the IPC server.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub stat: InodeStat,
}

// ── StatVfs ──────────────────────────────────────────────────────────────────

/// Volume statistics (equivalent to POSIX statvfs).
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct StatVfs {
    pub block_size:   u32,
    pub total_blocks: u64,
    pub free_blocks:  u64,
    pub avail_blocks: u64,
    pub total_inodes: u64,
    pub free_inodes:  u64,
    pub fsid:         u64,
    pub name_max:     u32,
    pub flags:        u32,
}

// ── MountFlags ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
pub struct MountFlags {
    pub read_only:   bool,
    pub no_exec:     bool,
    pub synchronous: bool,
}

// ── FileSystem trait ─────────────────────────────────────────────────────────
//
// INVARIANTS
// ----------
// 1. `rel_path` is ALWAYS relative to the filesystem's own mount point.
//    The VFS strips the mount-point prefix before calling any method.
//    Example: abs="/apps/foo", mount="/", rel="apps/foo".
//
// 2. Methods taking `&mut self` may mutate in-memory caches (dirty tracking).
//    Methods taking `&self` MUST NOT create new dirty entries.
//
// 3. A BackendHandle is valid until close() is called exactly once.

pub trait FileSystem: Send {

    // ── Path-based (pre-open) ─────────────────────────────────────────────

    fn open(&mut self, rel_path: &str, flags: OpenFlags) -> VfsResult<BackendHandle>;
    fn stat(&self,     rel_path: &str) -> VfsResult<InodeStat>;
    fn readdir(&self,  rel_path: &str) -> VfsResult<Vec<DirEntry>>;
    fn mkdir(&mut self,  rel_path: &str) -> VfsResult<()>;
    fn unlink(&mut self, rel_path: &str) -> VfsResult<()>;
    fn rmdir(&mut self,  rel_path: &str) -> VfsResult<()>;
    fn rename(&mut self, old_rel: &str, new_rel: &str) -> VfsResult<()>;

    // ── Handle-based (post-open) ──────────────────────────────────────────

    fn close(&mut self, h: BackendHandle) -> VfsResult<()>;
    fn read(&mut self,  h: BackendHandle, offset: u64, buf: &mut [u8]) -> VfsResult<usize>;
    fn write(&mut self, h: BackendHandle, offset: u64, data: &[u8]) -> VfsResult<usize>;
    fn seek(&self,      h: BackendHandle, offset: i64, whence: Whence) -> VfsResult<u64>;
    fn fstat(&self,     h: BackendHandle) -> VfsResult<InodeStat>;
    fn readdir_handle(&mut self, h: BackendHandle, out: &mut Vec<DirEntry>)
        -> VfsResult<usize>;
    fn truncate(&mut self, h: BackendHandle, new_size: u64) -> VfsResult<()>;
    fn fsync(&mut self,    h: BackendHandle) -> VfsResult<()>;

    // ── Volume ────────────────────────────────────────────────────────────

    fn statvfs(&self) -> VfsResult<StatVfs>;

    /// Flush the entire volume (FAT + clusters + FSInfo) and set the
    /// clean-shutdown bit.  Called on clean unmount.
    fn sync_volume(&mut self) -> VfsResult<()>;

    // ── Identity / lifecycle ──────────────────────────────────────────────

    fn mount_id(&self) -> u32;
    fn fstype(&self)   -> &'static str;
    fn is_read_only(&self) -> bool;

    /// Journal replay / crash recovery.  Called once before the mount is
    /// made visible.  Returns Err(Corrupted) only if the volume cannot be
    /// recovered safely; the VFS will then mount it read-only.
    fn recover(&mut self) -> VfsResult<()>;
}

// ── Vfs ──────────────────────────────────────────────────────────────────────

/// Virtual Filesystem — the single entry point for all IPC filesystem ops.
///
/// fsd is single-threaded (one IPC loop) so no interior mutability is needed.
pub struct Vfs {
    /// Mount table kept sorted by DESCENDING mount-point length so that the
    /// most-specific prefix is matched first.
    mounts:        Vec<MountEntry>,
    /// Filesystem instances keyed by mount_id.
    backends:      BTreeMap<u32, Box<dyn FileSystem>>,
    /// Per-PID file descriptor table.
    fd_table:      FdTable,
    next_mount_id: u32,
}

impl Vfs {
    pub fn new() -> Self {
        Vfs {
            mounts:        Vec::new(),
            backends:      BTreeMap::new(),
            fd_table:      FdTable::new(),
            next_mount_id: 0,
        }
    }

    // ── Mount / unmount ───────────────────────────────────────────────────

    /// Register a filesystem backend at `mount_point`.
    ///
    /// Calls backend.recover() before the mount becomes visible.
    /// If recovery returns Err(Corrupted) the mount proceeds read-only.
    pub fn mount(
        &mut self,
        mount_point: &str,
        flags: MountFlags,
        dev_path: &str,
        mut backend: Box<dyn FileSystem>,
    ) -> VfsResult<u32> {
        if !mount_point.starts_with('/') {
            return Err(VfsError::InvalidArgument);
        }
        let norm = normalize_path(mount_point);
        if self.mounts.iter().any(|m| m.mount_point == norm) {
            return Err(VfsError::Exists);
        }

        let mut effective_flags = flags;
        match backend.recover() {
            Ok(()) => {}
            Err(VfsError::Corrupted) => { effective_flags.read_only = true; }
            Err(e) => return Err(e),
        }

        let id = self.next_mount_id;
        self.next_mount_id += 1;
        self.backends.insert(id, backend);

        self.mounts.push(MountEntry {
            mount_id:    id,
            mount_point: norm,
            flags:       effective_flags,
            dev_path:    dev_path.to_string(),
        });
        // Most-specific (longest) prefix first.
        self.mounts.sort_by(|a, b| b.mount_point.len().cmp(&a.mount_point.len()));

        Ok(id)
    }

    /// Flush and remove a mounted filesystem.
    pub fn umount(&mut self, mount_point: &str) -> VfsResult<()> {
        let norm = normalize_path(mount_point);
        let pos = self.mounts.iter().position(|m| m.mount_point == norm)
            .ok_or(VfsError::NotFound)?;
        let id = self.mounts[pos].mount_id;
        self.mounts.remove(pos);
        if let Some(mut b) = self.backends.remove(&id) {
            let _ = b.sync_volume();
        }
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn resolve(&mut self, abs: &str) -> VfsResult<(u32, String)> {
        let norm = normalize_path(abs);
        for m in &self.mounts {
            if let Some(rel) = strip_prefix(&norm, &m.mount_point) {
                return Ok((m.mount_id, rel));
            }
        }
        Err(VfsError::NotFound)
    }

    fn resolve_ro(&self, abs: &str) -> VfsResult<(u32, String)> {
        let norm = normalize_path(abs);
        for m in &self.mounts {
            if let Some(rel) = strip_prefix(&norm, &m.mount_point) {
                return Ok((m.mount_id, rel));
            }
        }
        Err(VfsError::NotFound)
    }

    fn is_ro(&self, mid: u32) -> bool {
        self.mounts.iter()
            .find(|m| m.mount_id == mid)
            .map(|m| m.flags.read_only)
            .unwrap_or(false)
    }

    fn be(&mut self, mid: u32) -> VfsResult<&mut Box<dyn FileSystem>> {
        self.backends.get_mut(&mid).ok_or(VfsError::NotFound)
    }

    fn be_ro(&self, mid: u32) -> VfsResult<&Box<dyn FileSystem>> {
        self.backends.get(&mid).ok_or(VfsError::NotFound)
    }

    // ── Path-based dispatch ───────────────────────────────────────────────

    pub fn open(
        &mut self,
        abs_path: &str,
        flags: OpenFlags,
        owner_pid: u32,
        cap_handle: u64,
    ) -> VfsResult<u32> {
        let (mid, rel) = self.resolve(abs_path)?;

        if (flags.writable() || flags.create() || flags.trunc()) && self.is_ro(mid) {
            return Err(VfsError::ReadOnly);
        }

        let bh = self.backends.get_mut(&mid)
            .ok_or(VfsError::NotFound)?
            .open(&rel, flags)?;

        // Determine dir vs file
        let is_dir = self.backends.get(&mid)
            .ok_or(VfsError::NotFound)?
            .fstat(bh)
            .map(|s| s.mode & atom_abi::S_IFMT == atom_abi::S_IFDIR)
            .unwrap_or(false);

        let fd = if is_dir {
            self.fd_table.alloc_dir(mid, bh, owner_pid, cap_handle)
        } else {
            self.fd_table.alloc_file(mid, bh, flags, owner_pid, cap_handle)
        }.ok_or(VfsError::TooManyFiles)?;

        Ok(fd)
    }

    pub fn close(&mut self, fd: u32, pid: u32) -> VfsResult<()> {
        let (mid, bh) = self.pid_check_handle(fd, pid)?;
        self.backends.get_mut(&mid).ok_or(VfsError::NotFound)?.close(bh)?;
        self.fd_table.free(fd);
        Ok(())
    }

    pub fn read(
        &mut self, fd: u32, pid: u32, buf: &mut [u8],
    ) -> VfsResult<usize> {
        let (mid, bh, offset) = {
            let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
            match e {
                FdEntry::File(f) => {
                    if f.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                    if !f.flags.readable() { return Err(VfsError::PermissionDenied); }
                    (f.mount_id, f.backend_handle, f.offset)
                }
                FdEntry::Dir(_) => return Err(VfsError::IsDir),
            }
        };
        let n = self.backends.get_mut(&mid)
            .ok_or(VfsError::NotFound)?
            .read(bh, offset, buf)?;
        if let Some(FdEntry::File(f)) = self.fd_table.get_mut(fd) {
            f.offset += n as u64;
        }
        Ok(n)
    }

    pub fn write(
        &mut self, fd: u32, pid: u32, data: &[u8],
    ) -> VfsResult<usize> {
        let (mid, bh, offset) = {
            let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
            match e {
                FdEntry::File(f) => {
                    if f.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                    if !f.flags.writable() { return Err(VfsError::PermissionDenied); }
                    let off = if f.flags.append() { u64::MAX } else { f.offset };
                    (f.mount_id, f.backend_handle, off)
                }
                FdEntry::Dir(_) => return Err(VfsError::IsDir),
            }
        };
        if self.is_ro(mid) { return Err(VfsError::ReadOnly); }
        let n = self.backends.get_mut(&mid)
            .ok_or(VfsError::NotFound)?
            .write(bh, offset, data)?;
        if let Some(FdEntry::File(f)) = self.fd_table.get_mut(fd) {
            if !f.flags.append() { f.offset += n as u64; }
        }
        Ok(n)
    }

    pub fn seek(
        &mut self, fd: u32, pid: u32, offset: i64, whence: Whence,
    ) -> VfsResult<u64> {
        let (mid, bh) = {
            let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
            match e {
                FdEntry::File(f) => {
                    if f.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                    (f.mount_id, f.backend_handle)
                }
                FdEntry::Dir(_) => return Err(VfsError::IsDir),
            }
        };
        let new_off = self.backends.get(&mid)
            .ok_or(VfsError::NotFound)?
            .seek(bh, offset, whence)?;
        if let Some(FdEntry::File(f)) = self.fd_table.get_mut(fd) {
            f.offset = new_off;
        }
        Ok(new_off)
    }

    pub fn fstat(&self, fd: u32, pid: u32) -> VfsResult<InodeStat> {
        let (mid, bh) = self.pid_check_handle_ro(fd, pid)?;
        self.be_ro(mid)?.fstat(bh)
    }

    pub fn stat(&self, abs: &str) -> VfsResult<InodeStat> {
        let (mid, rel) = self.resolve_ro(abs)?;
        self.be_ro(mid)?.stat(&rel)
    }

    pub fn readdir_path(&self, abs: &str) -> VfsResult<Vec<DirEntry>> {
        let (mid, rel) = self.resolve_ro(abs)?;
        self.be_ro(mid)?.readdir(&rel)
    }

    pub fn readdir_fd(&mut self, fd: u32, pid: u32) -> VfsResult<Vec<DirEntry>> {
        let (mid, bh) = {
            let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
            match e {
                FdEntry::Dir(d) => {
                    if d.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                    (d.mount_id, d.backend_handle)
                }
                FdEntry::File(_) => return Err(VfsError::NotDir),
            }
        };
        let mut out = Vec::new();
        self.backends.get_mut(&mid)
            .ok_or(VfsError::NotFound)?
            .readdir_handle(bh, &mut out)?;
        Ok(out)
    }

    pub fn mkdir(&mut self, abs: &str) -> VfsResult<()> {
        let (mid, rel) = self.resolve(abs)?;
        if self.is_ro(mid) { return Err(VfsError::ReadOnly); }
        self.be(mid)?.mkdir(&rel)
    }

    pub fn unlink(&mut self, abs: &str) -> VfsResult<()> {
        let (mid, rel) = self.resolve(abs)?;
        if self.is_ro(mid) { return Err(VfsError::ReadOnly); }
        self.be(mid)?.unlink(&rel)
    }

    pub fn rmdir(&mut self, abs: &str) -> VfsResult<()> {
        let (mid, rel) = self.resolve(abs)?;
        if self.is_ro(mid) { return Err(VfsError::ReadOnly); }
        self.be(mid)?.rmdir(&rel)
    }

    pub fn rename(&mut self, old: &str, new: &str) -> VfsResult<()> {
        let (mid_o, rel_o) = self.resolve(old)?;
        let (mid_n, rel_n) = self.resolve(new)?;
        if mid_o != mid_n { return Err(VfsError::CrossDevice); }
        if self.is_ro(mid_o) { return Err(VfsError::ReadOnly); }
        self.be(mid_o)?.rename(&rel_o, &rel_n)
    }

    pub fn truncate(&mut self, fd: u32, pid: u32, new_size: u64) -> VfsResult<()> {
        let (mid, bh) = {
            let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
            match e {
                FdEntry::File(f) => {
                    if f.owner_pid != pid  { return Err(VfsError::PermissionDenied); }
                    if !f.flags.writable() { return Err(VfsError::PermissionDenied); }
                    (f.mount_id, f.backend_handle)
                }
                FdEntry::Dir(_) => return Err(VfsError::IsDir),
            }
        };
        if self.is_ro(mid) { return Err(VfsError::ReadOnly); }
        self.be(mid)?.truncate(bh, new_size)
    }

    pub fn fsync(&mut self, fd: u32, pid: u32) -> VfsResult<()> {
        let (mid, bh) = self.pid_check_handle(fd, pid)?;
        self.be(mid)?.fsync(bh)
    }

    pub fn statvfs(&self, abs: &str) -> VfsResult<StatVfs> {
        let (mid, _) = self.resolve_ro(abs)?;
        self.be_ro(mid)?.statvfs()
    }

    // ── Capability revocation ─────────────────────────────────────────────

    /// Invalidate all FDs tied to a revoked capability handle.
    pub fn revoke_cap(&mut self, cap_handle: u64) {
        self.fd_table.revoke_by_cap(cap_handle);
    }

    pub fn fd_table(&self)     -> &FdTable     { &self.fd_table }
    pub fn fd_table_mut(&mut self) -> &mut FdTable { &mut self.fd_table }

    // ── Private helpers ───────────────────────────────────────────────────

    fn pid_check_handle(&mut self, fd: u32, pid: u32)
        -> VfsResult<(u32, BackendHandle)>
    {
        let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
        match e {
            FdEntry::File(f) => {
                if f.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                Ok((f.mount_id, f.backend_handle))
            }
            FdEntry::Dir(d) => {
                if d.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                Ok((d.mount_id, d.backend_handle))
            }
        }
    }

    fn pid_check_handle_ro(&self, fd: u32, pid: u32)
        -> VfsResult<(u32, BackendHandle)>
    {
        let e = self.fd_table.get(fd).ok_or(VfsError::BadFd)?;
        match e {
            FdEntry::File(f) => {
                if f.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                Ok((f.mount_id, f.backend_handle))
            }
            FdEntry::Dir(d) => {
                if d.owner_pid != pid { return Err(VfsError::PermissionDenied); }
                Ok((d.mount_id, d.backend_handle))
            }
        }
    }
}

// ── Path utilities ────────────────────────────────────────────────────────────

/// Collapse double slashes and remove trailing slash (except for root "/").
pub fn normalize_path(path: &str) -> String {
    if path.is_empty() { return "/".to_string(); }
    let mut out = String::with_capacity(path.len());
    let mut prev = false;
    for ch in path.chars() {
        if ch == '/' {
            if !prev { out.push('/'); }
            prev = true;
        } else {
            prev = false;
            out.push(ch);
        }
    }
    if out.len() > 1 && out.ends_with('/') { out.pop(); }
    if out.is_empty() { "/".to_string() } else { out }
}

/// Strip `mount_point` prefix from `norm`.
/// Returns the relative path (no leading '/') or None if no match.
fn strip_prefix<'a>(norm: &'a str, mount_point: &str) -> Option<String> {
    if mount_point == "/" {
        // Everything lives under "/"
        return Some(norm.trim_start_matches('/').to_string());
    }
    if norm.starts_with(mount_point) {
        let remainder = &norm[mount_point.len()..];
        Some(remainder.trim_start_matches('/').to_string())
    } else {
        None
    }
}
