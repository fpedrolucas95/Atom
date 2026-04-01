// error.rs — Canonical VfsError type for fsd
//
// Maps bidirectionally to atom_abi error codes (u64::MAX - N).
// Every VFS and FAT32 internal operation returns VfsResult<T>.

#![allow(dead_code)]

use atom_abi;

/// Unified error type for all fsd operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    // ── Path / name ──────────────────────────────────────────────────────
    NotFound,         // ENOENT
    Exists,           // EEXIST
    NameTooLong,      // ENAMETOOLONG
    InvalidArgument,  // EINVAL
    CrossDevice,      // EXDEV  (rename across mounts)

    // ── File-type mismatches ──────────────────────────────────────────────
    IsDir,            // EISDIR
    NotDir,           // ENOTDIR
    NotEmpty,         // ENOTEMPTY

    // ── Descriptor / capability ───────────────────────────────────────────
    BadFd,            // EBADF
    TooManyFiles,     // EMFILE
    PermissionDenied, // EACCES
    ReadOnly,         // EROFS

    // ── Space / size ─────────────────────────────────────────────────────
    NoSpace,          // ENOSPC
    FileTooLarge,     // EFBIG
    Overflow,         // EOVERFLOW

    // ── I/O ──────────────────────────────────────────────────────────────
    Io,               // EIO
    Corrupted,        // ECORRUPTED  (bad BPB, bad checksum, …)

    // ── Misc ─────────────────────────────────────────────────────────────
    NotSupported,     // ENOTSUP
    NoDevice,         // ENODEV
    Other(u64),
}

pub type VfsResult<T> = Result<T, VfsError>;

impl VfsError {
    /// Convert a raw ABI error code to VfsError.
    pub fn from_abi(code: u64) -> Self {
        match code {
            x if x == atom_abi::ENOENT       => VfsError::NotFound,
            x if x == atom_abi::EEXIST       => VfsError::Exists,
            x if x == atom_abi::ENAMETOOLONG => VfsError::NameTooLong,
            x if x == atom_abi::EINVAL       => VfsError::InvalidArgument,
            x if x == atom_abi::EXDEV        => VfsError::CrossDevice,
            x if x == atom_abi::EISDIR       => VfsError::IsDir,
            x if x == atom_abi::ENOTDIR      => VfsError::NotDir,
            x if x == atom_abi::ENOTEMPTY    => VfsError::NotEmpty,
            x if x == atom_abi::EBADF        => VfsError::BadFd,
            x if x == atom_abi::EMFILE       => VfsError::TooManyFiles,
            x if x == atom_abi::EACCES       => VfsError::PermissionDenied,
            x if x == atom_abi::EROFS        => VfsError::ReadOnly,
            x if x == atom_abi::ENOSPC       => VfsError::NoSpace,
            x if x == atom_abi::EFBIG        => VfsError::FileTooLarge,
            x if x == atom_abi::EOVERFLOW    => VfsError::Overflow,
            x if x == atom_abi::EIO          => VfsError::Io,
            x if x == atom_abi::ECORRUPTED   => VfsError::Corrupted,
            x if x == atom_abi::ENOTSUP      => VfsError::NotSupported,
            x if x == atom_abi::ENODEV       => VfsError::NoDevice,
            other                            => VfsError::Other(other),
        }
    }

    /// Convert VfsError to the canonical ABI error code.
    pub fn to_abi(self) -> u64 {
        match self {
            VfsError::NotFound         => atom_abi::ENOENT,
            VfsError::Exists           => atom_abi::EEXIST,
            VfsError::NameTooLong      => atom_abi::ENAMETOOLONG,
            VfsError::InvalidArgument  => atom_abi::EINVAL,
            VfsError::CrossDevice      => atom_abi::EXDEV,
            VfsError::IsDir            => atom_abi::EISDIR,
            VfsError::NotDir           => atom_abi::ENOTDIR,
            VfsError::NotEmpty         => atom_abi::ENOTEMPTY,
            VfsError::BadFd            => atom_abi::EBADF,
            VfsError::TooManyFiles     => atom_abi::EMFILE,
            VfsError::PermissionDenied => atom_abi::EACCES,
            VfsError::ReadOnly         => atom_abi::EROFS,
            VfsError::NoSpace          => atom_abi::ENOSPC,
            VfsError::FileTooLarge     => atom_abi::EFBIG,
            VfsError::Overflow         => atom_abi::EOVERFLOW,
            VfsError::Io               => atom_abi::EIO,
            VfsError::Corrupted        => atom_abi::ECORRUPTED,
            VfsError::NotSupported     => atom_abi::ENOTSUP,
            VfsError::NoDevice         => atom_abi::ENODEV,
            VfsError::Other(c)         => c,
        }
    }
}
