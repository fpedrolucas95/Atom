// Filesystem syscall wrappers for Atom OS userspace.
//
// These wrappers invoke the kernel FS syscalls (53–76), which the kernel
// routes to fsd via IPC.  For applications that need high-throughput I/O
// the companion `libfs` library communicates with fsd directly without
// kernel mediation, but for correctness these wrappers are authoritative.
//
// Error handling mirrors the kernel ABI: every non-zero return is one of
// the POSIX-like codes defined in `atom_abi`.

use crate::raw::numbers::*;
use crate::error::is_syscall_error;
use crate::raw::{syscall1, syscall2, syscall3, syscall4, syscall6};
use atom_abi::{
    ENOENT, EEXIST, EISDIR, ENOTDIR, ENOTEMPTY, EBADF,
    EFBIG, ENOSPC, EROFS, ENAMETOOLONG, EIO, EACCES,
    EMFILE, EOVERFLOW, ECORRUPTED, EMLINK, EXDEV, EPIPE,
    ENOTSUP, EAGAIN, EINTR, E2BIG,
    SEEK_SET, SEEK_CUR, SEEK_END,
    O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, O_EXCL, O_TRUNC,
    O_APPEND, O_NONBLOCK, O_SYNC, O_DIRECTORY, O_NOFOLLOW, O_CLOEXEC,
    S_IFMT, S_IFREG, S_IFDIR, S_IFLNK, S_IFBLK, S_IFCHR, S_IFIFO, S_IFSOCK,
};

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;

// ── Error type ──────────────────────────────────────────────────────────────

/// Filesystem-specific error codes, mapped from ABI u64 codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,       // ENOENT
    Exists,         // EEXIST
    IsDir,          // EISDIR
    NotDir,         // ENOTDIR
    NotEmpty,       // ENOTEMPTY
    BadFd,          // EBADF
    FileTooLarge,   // EFBIG
    NoSpace,        // ENOSPC
    ReadOnly,       // EROFS
    NameTooLong,    // ENAMETOOLONG
    Io,             // EIO
    PermissionDenied, // EACCES
    TooManyFiles,   // EMFILE
    Overflow,       // EOVERFLOW
    Corrupted,      // ECORRUPTED
    TooManyLinks,   // EMLINK
    CrossDevice,    // EXDEV
    BrokenPipe,     // EPIPE
    NotSupported,   // ENOTSUP
    WouldBlock,     // EAGAIN
    Interrupted,    // EINTR
    ArgListTooLong, // E2BIG
    InvalidArg,     // EINVAL
    Other(u64),
}

impl FsError {
    pub fn from_raw(code: u64) -> Self {
        match code {
            x if x == ENOENT       => FsError::NotFound,
            x if x == EEXIST       => FsError::Exists,
            x if x == EISDIR       => FsError::IsDir,
            x if x == ENOTDIR      => FsError::NotDir,
            x if x == ENOTEMPTY    => FsError::NotEmpty,
            x if x == EBADF        => FsError::BadFd,
            x if x == EFBIG        => FsError::FileTooLarge,
            x if x == ENOSPC       => FsError::NoSpace,
            x if x == EROFS        => FsError::ReadOnly,
            x if x == ENAMETOOLONG => FsError::NameTooLong,
            x if x == EIO          => FsError::Io,
            x if x == EACCES       => FsError::PermissionDenied,
            x if x == EMFILE       => FsError::TooManyFiles,
            x if x == EOVERFLOW    => FsError::Overflow,
            x if x == ECORRUPTED   => FsError::Corrupted,
            x if x == EMLINK       => FsError::TooManyLinks,
            x if x == EXDEV        => FsError::CrossDevice,
            x if x == EPIPE        => FsError::BrokenPipe,
            x if x == ENOTSUP      => FsError::NotSupported,
            x if x == EAGAIN       => FsError::WouldBlock,
            x if x == EINTR        => FsError::Interrupted,
            x if x == E2BIG        => FsError::ArgListTooLong,
            x if x == atom_abi::EINVAL => FsError::InvalidArg,
            other                  => FsError::Other(other),
        }
    }

    pub fn to_raw(&self) -> u64 {
        match self {
            FsError::NotFound         => ENOENT,
            FsError::Exists           => EEXIST,
            FsError::IsDir            => EISDIR,
            FsError::NotDir           => ENOTDIR,
            FsError::NotEmpty         => ENOTEMPTY,
            FsError::BadFd            => EBADF,
            FsError::FileTooLarge     => EFBIG,
            FsError::NoSpace          => ENOSPC,
            FsError::ReadOnly         => EROFS,
            FsError::NameTooLong      => ENAMETOOLONG,
            FsError::Io               => EIO,
            FsError::PermissionDenied => EACCES,
            FsError::TooManyFiles     => EMFILE,
            FsError::Overflow         => EOVERFLOW,
            FsError::Corrupted        => ECORRUPTED,
            FsError::TooManyLinks     => EMLINK,
            FsError::CrossDevice      => EXDEV,
            FsError::BrokenPipe       => EPIPE,
            FsError::NotSupported     => ENOTSUP,
            FsError::WouldBlock       => EAGAIN,
            FsError::Interrupted      => EINTR,
            FsError::ArgListTooLong   => E2BIG,
            FsError::InvalidArg       => atom_abi::EINVAL,
            FsError::Other(c)         => *c,
        }
    }
}

impl core::fmt::Display for FsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self)
    }
}

pub type FsResult<T> = Result<T, FsError>;

fn check(raw: u64) -> FsResult<u64> {
    if is_syscall_error(raw) {
        Err(FsError::from_raw(raw))
    } else {
        Ok(raw)
    }
}

// ── Open flags re-export ────────────────────────────────────────────────────

pub struct OpenFlags;
impl OpenFlags {
    pub const RDONLY:    u32 = O_RDONLY;
    pub const WRONLY:    u32 = O_WRONLY;
    pub const RDWR:      u32 = O_RDWR;
    pub const CREAT:     u32 = O_CREAT;
    pub const EXCL:      u32 = O_EXCL;
    pub const TRUNC:     u32 = O_TRUNC;
    pub const APPEND:    u32 = O_APPEND;
    pub const NONBLOCK:  u32 = O_NONBLOCK;
    pub const SYNC:      u32 = O_SYNC;
    pub const DIRECTORY: u32 = O_DIRECTORY;
    pub const NOFOLLOW:  u32 = O_NOFOLLOW;
    pub const CLOEXEC:   u32 = O_CLOEXEC;
}

pub struct SeekWhence;
impl SeekWhence {
    pub const SET: u32 = SEEK_SET;
    pub const CUR: u32 = SEEK_CUR;
    pub const END: u32 = SEEK_END;
}

// ── Stat structs ────────────────────────────────────────────────────────────

/// File status — mirrors the 80-byte on-wire stat struct.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStat {
    pub size:     u64,
    pub inode:    u64,
    pub mtime_ns: u64,
    pub atime_ns: u64,
    pub ctime_ns: u64,
    pub mode:     u32,
    pub uid:      u16,
    pub gid:      u16,
    pub nlinks:   u32,
    pub nblocks:  u32,
    pub blksize:  u32,
    pub dev:      u32,
}

impl FsStat {
    pub const WIRE_SIZE: usize = 80;

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::WIRE_SIZE { return None; }
        Some(Self {
            size:     u64::from_le_bytes(b[0..8].try_into().ok()?),
            inode:    u64::from_le_bytes(b[8..16].try_into().ok()?),
            mtime_ns: u64::from_le_bytes(b[16..24].try_into().ok()?),
            atime_ns: u64::from_le_bytes(b[24..32].try_into().ok()?),
            ctime_ns: u64::from_le_bytes(b[32..40].try_into().ok()?),
            mode:     u32::from_le_bytes(b[40..44].try_into().ok()?),
            uid:      u16::from_le_bytes(b[44..46].try_into().ok()?),
            gid:      u16::from_le_bytes(b[46..48].try_into().ok()?),
            nlinks:   u32::from_le_bytes(b[48..52].try_into().ok()?),
            nblocks:  u32::from_le_bytes(b[52..56].try_into().ok()?),
            blksize:  u32::from_le_bytes(b[56..60].try_into().ok()?),
            dev:      u32::from_le_bytes(b[60..64].try_into().ok()?),
        })
    }

    pub fn to_bytes(&self) -> [u8; Self::WIRE_SIZE] {
        let mut b = [0u8; Self::WIRE_SIZE];
        b[0..8].copy_from_slice(&self.size.to_le_bytes());
        b[8..16].copy_from_slice(&self.inode.to_le_bytes());
        b[16..24].copy_from_slice(&self.mtime_ns.to_le_bytes());
        b[24..32].copy_from_slice(&self.atime_ns.to_le_bytes());
        b[32..40].copy_from_slice(&self.ctime_ns.to_le_bytes());
        b[40..44].copy_from_slice(&self.mode.to_le_bytes());
        b[44..46].copy_from_slice(&self.uid.to_le_bytes());
        b[46..48].copy_from_slice(&self.gid.to_le_bytes());
        b[48..52].copy_from_slice(&self.nlinks.to_le_bytes());
        b[52..56].copy_from_slice(&self.nblocks.to_le_bytes());
        b[56..60].copy_from_slice(&self.blksize.to_le_bytes());
        b[60..64].copy_from_slice(&self.dev.to_le_bytes());
        b
    }

    pub fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode as u16)
    }

    pub fn is_dir(&self) -> bool { self.file_type() == FileType::Directory }
    pub fn is_file(&self) -> bool { self.file_type() == FileType::Regular }
    pub fn is_symlink(&self) -> bool { self.file_type() == FileType::Symlink }
    pub fn is_readable(&self, uid: u16, gid: u16) -> bool { self.mode_check(uid, gid, 4) }
    pub fn is_writable(&self, uid: u16, gid: u16) -> bool { self.mode_check(uid, gid, 2) }
    pub fn is_executable(&self, uid: u16, gid: u16) -> bool { self.mode_check(uid, gid, 1) }

    fn mode_check(&self, uid: u16, gid: u16, want: u32) -> bool {
        let perm = self.mode & 0o777;
        if uid == self.uid {
            (perm >> 6) & want != 0
        } else if gid == self.gid {
            (perm >> 3) & want != 0
        } else {
            perm & want != 0
        }
    }
}

/// File type decoded from mode bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
    Unknown,
}

impl FileType {
    pub fn from_mode(mode: u16) -> Self {
        let mode = mode as u32;
        match mode & S_IFMT {
            x if x == S_IFREG  => FileType::Regular,
            x if x == S_IFDIR  => FileType::Directory,
            x if x == S_IFLNK  => FileType::Symlink,
            x if x == S_IFBLK  => FileType::BlockDevice,
            x if x == S_IFCHR  => FileType::CharDevice,
            x if x == S_IFIFO  => FileType::Fifo,
            x if x == S_IFSOCK => FileType::Socket,
            _                  => FileType::Unknown,
        }
    }

    pub fn as_dirent_type(self) -> u8 {
        match self {
            FileType::Regular      => 1,
            FileType::Directory    => 2,
            FileType::Symlink      => 3,
            FileType::BlockDevice  => 4,
            FileType::CharDevice   => 5,
            FileType::Fifo         => 6,
            FileType::Socket       => 7,
            FileType::Unknown      => 0,
        }
    }
}

/// Filesystem statistics.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStatvfs {
    pub bsize:   u64,
    pub frsize:  u64,
    pub blocks:  u64,
    pub bfree:   u64,
    pub bavail:  u64,
    pub files:   u64,
    pub ffree:   u64,
    pub favail:  u64,
    pub namemax: u32,
    pub flags:   u32,
}

impl FsStatvfs {
    pub const WIRE_SIZE: usize = 72;

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < Self::WIRE_SIZE { return None; }
        Some(Self {
            bsize:   u64::from_le_bytes(b[0..8].try_into().ok()?),
            frsize:  u64::from_le_bytes(b[8..16].try_into().ok()?),
            blocks:  u64::from_le_bytes(b[16..24].try_into().ok()?),
            bfree:   u64::from_le_bytes(b[24..32].try_into().ok()?),
            bavail:  u64::from_le_bytes(b[32..40].try_into().ok()?),
            files:   u64::from_le_bytes(b[40..48].try_into().ok()?),
            ffree:   u64::from_le_bytes(b[48..56].try_into().ok()?),
            favail:  u64::from_le_bytes(b[56..64].try_into().ok()?),
            namemax: u32::from_le_bytes(b[64..68].try_into().ok()?),
            flags:   u32::from_le_bytes(b[68..72].try_into().ok()?),
        })
    }
}

/// Directory entry returned by readdir.
#[derive(Debug, Clone)]
pub struct FsDirent {
    pub ino:       u64,
    pub file_type: FileType,
    pub name:      String,
}

// ── Syscall wrappers ─────────────────────────────────────────────────────────

/// Open or create a file.
///
/// # Arguments
/// * `path`  — absolute or relative path
/// * `flags` — combination of `OpenFlags::*` constants
/// * `mode`  — permission bits for newly created files (e.g. 0o644)
///
/// Returns a file descriptor (handle) on success.
pub fn open(path: &str, flags: u32, mode: u32) -> FsResult<u64> {
    let p = path.as_bytes();
    let raw = unsafe {
        syscall4(SYS_FS_OPEN, p.as_ptr() as u64, p.len() as u64, flags as u64, mode as u64)
    };
    check(raw)
}

/// Close a file descriptor previously returned by `open`.
pub fn close(fd: u64) -> FsResult<()> {
    let raw = unsafe { syscall1(SYS_FS_CLOSE, fd) };
    check(raw).map(|_| ())
}

/// Read up to `buf.len()` bytes from `fd` into `buf`.
///
/// Returns the number of bytes actually read (0 = EOF).
pub fn read(fd: u64, buf: &mut [u8]) -> FsResult<usize> {
    if buf.is_empty() { return Ok(0); }
    let raw = unsafe {
        syscall3(SYS_FS_READ, fd, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    check(raw).map(|n| n as usize)
}

/// Write `buf` to `fd`.
///
/// Returns the number of bytes written.
pub fn write(fd: u64, buf: &[u8]) -> FsResult<usize> {
    if buf.is_empty() { return Ok(0); }
    let raw = unsafe {
        syscall3(SYS_FS_WRITE, fd, buf.as_ptr() as u64, buf.len() as u64)
    };
    check(raw).map(|n| n as usize)
}

/// Read all remaining bytes from `fd` into a `Vec<u8>`.
pub fn read_all(fd: u64) -> FsResult<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = read(fd, &mut buf)?;
        if n == 0 { break; }
        out.extend_from_slice(&buf[..n]);
    }
    Ok(out)
}

/// Write all of `data` to `fd`, retrying short writes.
pub fn write_all(fd: u64, data: &[u8]) -> FsResult<()> {
    let mut written = 0;
    while written < data.len() {
        let n = write(fd, &data[written..])?;
        if n == 0 { return Err(FsError::Io); }
        written += n;
    }
    Ok(())
}

/// Reposition the file offset.
pub fn lseek(fd: u64, offset: i64, whence: u32) -> FsResult<u64> {
    let raw = unsafe {
        syscall3(SYS_FS_SEEK, fd, offset as u64, whence as u64)
    };
    check(raw)
}

/// Get file status by path.
pub fn stat(path: &str) -> FsResult<FsStat> {
    let mut buf = [0u8; FsStat::WIRE_SIZE];
    let p = path.as_bytes();
    let raw = unsafe {
        syscall3(SYS_FS_STAT, p.as_ptr() as u64, p.len() as u64, buf.as_mut_ptr() as u64)
    };
    check(raw)?;
    FsStat::from_bytes(&buf).ok_or(FsError::Io)
}

/// Get file status by file descriptor.
pub fn fstat(fd: u64) -> FsResult<FsStat> {
    let mut buf = [0u8; FsStat::WIRE_SIZE];
    let raw = unsafe {
        syscall2(SYS_FS_FSTAT, fd, buf.as_mut_ptr() as u64)
    };
    check(raw)?;
    FsStat::from_bytes(&buf).ok_or(FsError::Io)
}

/// Create a directory.
pub fn mkdir(path: &str, mode: u32) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe {
        syscall3(SYS_FS_MKDIR, p.as_ptr() as u64, p.len() as u64, mode as u64)
    };
    check(raw).map(|_| ())
}

/// Remove an empty directory.
pub fn rmdir(path: &str) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe {
        syscall2(SYS_FS_RMDIR, p.as_ptr() as u64, p.len() as u64)
    };
    check(raw).map(|_| ())
}

/// Remove a file (unlink).
pub fn unlink(path: &str) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe {
        syscall2(SYS_FS_UNLINK, p.as_ptr() as u64, p.len() as u64)
    };
    check(raw).map(|_| ())
}

/// Rename (move) a file or directory.
pub fn rename(old: &str, new: &str) -> FsResult<()> {
    let op = old.as_bytes();
    let np = new.as_bytes();
    let raw = unsafe {
        crate::raw::syscall4(
            SYS_FS_RENAME,
            op.as_ptr() as u64,
            op.len() as u64,
            np.as_ptr() as u64,
            np.len() as u64,
        )
    };
    check(raw).map(|_| ())
}

/// Read directory entries into a buffer, returning parsed `FsDirent` list.
///
/// The kernel fills a shared buffer with packed on-wire dirent records;
/// we parse them here.
pub fn readdir(fd: u64) -> FsResult<Vec<FsDirent>> {
    // Allocate a reasonably large buffer for directory entries.
    let mut buf = alloc::vec![0u8; 65536];
    let raw = unsafe {
        syscall3(SYS_FS_READDIR, fd, buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    let total = check(raw)? as usize;

    let mut entries = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= total {
        let b = &buf[pos..];
        let ino = u32::from_le_bytes(b[0..4].try_into().map_err(|_| FsError::Io)?);
        let rec_len = u16::from_le_bytes(b[4..6].try_into().map_err(|_| FsError::Io)?) as usize;
        let name_len = b[6] as usize;
        let ftype_raw = b[7];

        if rec_len < 8 || rec_len % 4 != 0 || pos + rec_len > total { break; }
        if 8 + name_len > rec_len { break; }
        if ino != 0 {
            let name_bytes = &b[8..8 + name_len];
            let name = core::str::from_utf8(name_bytes)
                .map_err(|_| FsError::Io)
                .map(|s| String::from(s))?;
            let file_type = match ftype_raw {
                1 => FileType::Regular,
                2 => FileType::Directory,
                3 => FileType::Symlink,
                4 => FileType::BlockDevice,
                5 => FileType::CharDevice,
                6 => FileType::Fifo,
                7 => FileType::Socket,
                _ => FileType::Unknown,
            };
            entries.push(FsDirent { ino: ino as u64, file_type, name });
        }
        pos += rec_len;
    }
    Ok(entries)
}

/// Truncate a file to `new_size` bytes.
pub fn truncate(fd: u64, new_size: u64) -> FsResult<()> {
    let raw = unsafe { syscall2(SYS_FS_TRUNCATE, fd, new_size) };
    check(raw).map(|_| ())
}

/// Flush in-memory data and metadata to persistent storage.
pub fn fsync(fd: u64) -> FsResult<()> {
    let raw = unsafe { syscall1(SYS_FS_FSYNC, fd) };
    check(raw).map(|_| ())
}

/// Mount a filesystem.
///
/// * `device`    — path to the block device (e.g. `/dev/sda1`)
/// * `mountpoint` — directory to mount on (must exist)
/// * `fstype`    — filesystem type string (e.g. `"atomfs"`)
/// * `_flags`    — mount flags (currently unused, pass 0)
pub fn mount(device: &str, mountpoint: &str, fstype: &str, _flags: u32) -> FsResult<()> {
    let dp = device.as_bytes();
    let mp = mountpoint.as_bytes();
    let ft = fstype.as_bytes();
    let raw = unsafe {
        syscall6(
            SYS_FS_MOUNT,
            dp.as_ptr() as u64, dp.len() as u64,
            mp.as_ptr() as u64, mp.len() as u64,
            ft.as_ptr() as u64, ft.len() as u64,
        )
    };
    check(raw).map(|_| ())
}

/// Unmount the filesystem mounted at `path`.
pub fn umount(path: &str) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe { syscall2(SYS_FS_UMOUNT, p.as_ptr() as u64, p.len() as u64) };
    check(raw).map(|_| ())
}

/// Change file permissions.
pub fn chmod(path: &str, mode: u32) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe {
        syscall3(SYS_FS_CHMOD, p.as_ptr() as u64, p.len() as u64, mode as u64)
    };
    check(raw).map(|_| ())
}

/// Duplicate a file descriptor.
pub fn dup(fd: u64) -> FsResult<u64> {
    let raw = unsafe { syscall1(SYS_FS_DUP, fd) };
    check(raw)
}

/// Duplicate `old_fd` as `new_fd`, closing `new_fd` first if it's open.
pub fn dup2(old_fd: u64, new_fd: u64) -> FsResult<u64> {
    let raw = unsafe { syscall2(SYS_FS_DUP2, old_fd, new_fd) };
    check(raw)
}

/// Create a hard link.
pub fn link(old: &str, new: &str) -> FsResult<()> {
    let op = old.as_bytes();
    let np = new.as_bytes();
    let raw = unsafe {
        crate::raw::syscall4(SYS_FS_LINK, op.as_ptr() as u64, op.len() as u64,
                             np.as_ptr() as u64, np.len() as u64)
    };
    check(raw).map(|_| ())
}

/// Create a symbolic link.
pub fn symlink(target: &str, link_path: &str) -> FsResult<()> {
    let tp = target.as_bytes();
    let lp = link_path.as_bytes();
    let raw = unsafe {
        crate::raw::syscall4(SYS_FS_SYMLINK, tp.as_ptr() as u64, tp.len() as u64,
                             lp.as_ptr() as u64, lp.len() as u64)
    };
    check(raw).map(|_| ())
}

/// Read the target of a symbolic link.
pub fn readlink(path: &str) -> FsResult<String> {
    let p = path.as_bytes();
    let mut buf = alloc::vec![0u8; 4096];
    let raw = unsafe {
        crate::raw::syscall4(SYS_FS_READLINK, p.as_ptr() as u64, p.len() as u64,
                             buf.as_mut_ptr() as u64, buf.len() as u64)
    };
    let n = check(raw)? as usize;
    core::str::from_utf8(&buf[..n]).map(|s| String::from(s)).map_err(|_| FsError::Io)
}

/// Update access and modification timestamps on `path`.
///
/// Pass `-1` to leave a timestamp unchanged (same semantics as `UTIME_NOW`
/// is out of scope for this MVP; pass actual nanosecond timestamps).
pub fn utimes(path: &str, atime_ns: i64, mtime_ns: i64) -> FsResult<()> {
    let p = path.as_bytes();
    let raw = unsafe {
        crate::raw::syscall4(SYS_FS_UTIMES, p.as_ptr() as u64, p.len() as u64,
                             atime_ns as u64, mtime_ns as u64)
    };
    check(raw).map(|_| ())
}

/// Get filesystem statistics for the filesystem containing `path`.
pub fn statvfs(path: &str) -> FsResult<FsStatvfs> {
    let mut buf = [0u8; FsStatvfs::WIRE_SIZE];
    let p = path.as_bytes();
    let raw = unsafe {
        syscall3(SYS_FS_STATVFS, p.as_ptr() as u64, p.len() as u64, buf.as_mut_ptr() as u64)
    };
    check(raw)?;
    FsStatvfs::from_bytes(&buf).ok_or(FsError::Io)
}

/// High-level helper: check whether a path exists.
pub fn exists(path: &str) -> bool {
    stat(path).is_ok()
}

/// High-level helper: read an entire file into a `Vec<u8>`.
pub fn read_file(path: &str) -> FsResult<Vec<u8>> {
    let fd = open(path, OpenFlags::RDONLY, 0)?;
    let data = read_all(fd);
    let _ = close(fd);
    data
}

/// High-level helper: write a slice to a file (creating or truncating it).
pub fn write_file(path: &str, data: &[u8]) -> FsResult<()> {
    let flags = OpenFlags::WRONLY | OpenFlags::CREAT | OpenFlags::TRUNC;
    let fd = open(path, flags, 0o644)?;
    let result = write_all(fd, data);
    let _ = close(fd);
    result
}

/// High-level helper: append a slice to a file.
pub fn append_file(path: &str, data: &[u8]) -> FsResult<()> {
    let flags = OpenFlags::WRONLY | OpenFlags::CREAT | OpenFlags::APPEND;
    let fd = open(path, flags, 0o644)?;
    let result = write_all(fd, data);
    let _ = close(fd);
    result
}

/// Recursively create directory and all parents (like `mkdir -p`).
pub fn mkdir_all(path: &str, mode: u32) -> FsResult<()> {
    if path.is_empty() || path == "/" { return Ok(()); }
    match mkdir(path, mode) {
        Ok(()) => Ok(()),
        Err(FsError::Exists) => Ok(()),
        Err(FsError::NotFound) => {
            // Parent doesn't exist — create it first.
            let parent = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
            if !parent.is_empty() && parent != path {
                mkdir_all(parent, mode)?;
            }
            mkdir(path, mode)
        }
        Err(e) => Err(e),
    }
}

/// Copy a file from `src` to `dst`, optionally reporting progress via a callback.
///
/// `progress`: called with `(bytes_copied, total)` every 4KB.
pub fn copy_file<F>(src: &str, dst: &str, mut progress: F) -> FsResult<u64>
where
    F: FnMut(u64, u64),
{
    let src_fd = open(src, OpenFlags::RDONLY, 0)?;
    let src_stat = fstat(src_fd).map_err(|e| { let _ = close(src_fd); e })?;
    let total = src_stat.size;

    let dst_fd = match open(dst, OpenFlags::WRONLY | OpenFlags::CREAT | OpenFlags::TRUNC, 0o644) {
        Ok(f) => f,
        Err(e) => { let _ = close(src_fd); return Err(e); }
    };

    let mut buf = alloc::vec![0u8; 65536];
    let mut copied = 0u64;
    let result = loop {
        let n = match read(src_fd, &mut buf) {
            Ok(0) => break Ok(copied),
            Ok(n) => n,
            Err(e) => break Err(e),
        };
        match write_all(dst_fd, &buf[..n]) {
            Ok(()) => {}
            Err(e) => break Err(e),
        }
        copied += n as u64;
        progress(copied, total);
    };

    let _ = fsync(dst_fd);
    let _ = close(src_fd);
    let _ = close(dst_fd);
    result
}

// ── Kernel FS Backend syscalls (used by fsd) ──────────────────────────────

/// Read a file's contents directly from the kernel's FAT32 driver.
/// Returns the number of bytes read into `buf`.
pub fn kern_fs_read_file(path: &str, buf: &mut [u8]) -> FsResult<usize> {
    let r = unsafe {
        syscall4(
            SYS_KERN_FS_READ_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// List a directory directly from the kernel's FAT32 driver.
/// Returns bytes written into `buf` (packed dirent records).
pub fn kern_fs_list_dir(path: &str, buf: &mut [u8]) -> FsResult<usize> {
    let r = unsafe {
        syscall4(
            SYS_KERN_FS_LIST_DIR,
            path.as_ptr() as u64,
            path.len() as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// Stat a path directly from the kernel's FAT32 driver.
/// Writes an 80-byte stat buffer.
pub fn kern_fs_stat_path(path: &str, stat_buf: &mut [u8; 80]) -> FsResult<()> {
    let r = unsafe {
        syscall3(
            SYS_KERN_FS_STAT_PATH,
            path.as_ptr() as u64,
            path.len() as u64,
            stat_buf.as_mut_ptr() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Write a full file payload directly via the kernel FAT32 backend.
pub fn kern_fs_write_file(path: &str, data: &[u8]) -> FsResult<()> {
    let r = unsafe {
        syscall4(
            SYS_KERN_FS_WRITE_FILE,
            path.as_ptr() as u64,
            path.len() as u64,
            data.as_ptr() as u64,
            data.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Create a directory directly via the kernel FAT32 backend.
pub fn kern_fs_mkdir(path: &str) -> FsResult<()> {
    let r = unsafe {
        syscall2(
            SYS_KERN_FS_MKDIR,
            path.as_ptr() as u64,
            path.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Remove an empty directory directly via the kernel FAT32 backend.
pub fn kern_fs_rmdir(path: &str) -> FsResult<()> {
    let r = unsafe {
        syscall2(
            SYS_KERN_FS_RMDIR,
            path.as_ptr() as u64,
            path.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Remove a file directly via the kernel FAT32 backend.
pub fn kern_fs_unlink(path: &str) -> FsResult<()> {
    let r = unsafe {
        syscall2(
            SYS_KERN_FS_UNLINK,
            path.as_ptr() as u64,
            path.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Rename a path directly via the kernel FAT32 backend.
pub fn kern_fs_rename(old_path: &str, new_path: &str) -> FsResult<()> {
    let r = unsafe {
        syscall4(
            SYS_KERN_FS_RENAME,
            old_path.as_ptr() as u64,
            old_path.len() as u64,
            new_path.as_ptr() as u64,
            new_path.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Flush FAT32 metadata and device cache directly via kernel backend.
pub fn kern_fs_sync() -> FsResult<()> {
    let r = unsafe { crate::raw::syscall0(SYS_KERN_FS_SYNC) };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Read raw disk sectors from the kernel block layer.
/// Returns the number of bytes written into `buf`.
pub fn kern_block_read(lba: u64, sector_count: u32, buf: &mut [u8]) -> FsResult<usize> {
    let r = unsafe {
        syscall4(
            SYS_KERN_BLOCK_READ,
            lba,
            sector_count as u64,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(r as usize)
    }
}

/// Write raw disk sectors through the kernel block layer.
pub fn kern_block_write(lba: u64, sector_count: u32, data: &[u8]) -> FsResult<()> {
    let r = unsafe {
        syscall4(
            SYS_KERN_BLOCK_WRITE,
            lba,
            sector_count as u64,
            data.as_ptr() as u64,
            data.len() as u64,
        )
    };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}

/// Flush device write cache through the kernel block layer.
pub fn kern_block_flush() -> FsResult<()> {
    let r = unsafe { crate::raw::syscall0(SYS_KERN_BLOCK_FLUSH) };
    if is_syscall_error(r) {
        Err(FsError::from_raw(r))
    } else {
        Ok(())
    }
}
