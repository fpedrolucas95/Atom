/// Error handling for fileman
/// 
/// Production-ready error handling with complete error codes and messages.
/// All errors are mapped from filesystem syscall errors to user-friendly messages.

use atom_syscall::fs::FsError;
use atom_abi::{EBUSY, ENODEV, ETIMEDOUT};
use core::fmt;

/// Enhanced error type with user messages and exit codes
#[derive(Debug, Clone)]
pub enum FilManagerError {
    /// Filesystem operation failed
    FsOp(FsError, &'static str),
    /// Invalid command syntax
    InvalidCommand(CommandError),
    /// I/O error during read/write
    IO,
    /// Path too long (exceeds limits)
    PathTooLong,
    /// Invalid path format
    InvalidPath,
    /// Copy/move across filesystems not supported
    CrossDevice,
}

#[derive(Debug, Clone)]
pub enum CommandError {
    /// Wrong number of arguments
    WrongArgCount { expected: usize, got: usize },
    /// Unknown command
    UnknownCommand(usize), // stores length of command string
    /// Missing required argument
    MissingArg(&'static str),
    /// Conflicting options
    ConflictingOptions,
}

impl FilManagerError {
    /// Create filesystem error with context message
    pub fn fs(err: FsError, context: &'static str) -> Self {
        FilManagerError::FsOp(err, context)
    }

    /// Get exit code for error (POSIX-like)
    pub fn exit_code(&self) -> u32 {
        match self {
            FilManagerError::FsOp(FsError::NotFound, _) => 2,       // ENOENT
            FilManagerError::FsOp(FsError::Exists, _) => 17,        // EEXIST
            FilManagerError::FsOp(FsError::IsDir, _) => 21,         // EISDIR
            FilManagerError::FsOp(FsError::NotDir, _) => 20,        // ENOTDIR
            FilManagerError::FsOp(FsError::NotEmpty, _) => 39,      // ENOTEMPTY
            FilManagerError::FsOp(FsError::PermissionDenied, _) => 13, // EACCES
            FilManagerError::FsOp(FsError::ReadOnly, _) => 30,      // EROFS
            FilManagerError::FsOp(FsError::BadFd, _) => 9,          // EBADF
            FilManagerError::FsOp(FsError::NameTooLong, _) => 36,   // ENAMETOOLONG
            FilManagerError::FsOp(FsError::NoSpace, _) => 28,       // ENOSPC
            FilManagerError::FsOp(FsError::Io, _) => 5,             // EIO
            FilManagerError::FsOp(FsError::CrossDevice, _) => 18,   // EXDEV
            FilManagerError::FsOp(_, _) => 1,
            FilManagerError::InvalidCommand(_) => 1,
            FilManagerError::IO => 5,
            FilManagerError::PathTooLong => 36,
            FilManagerError::InvalidPath => 22,
            FilManagerError::CrossDevice => 18,
        }
    }

    /// Detailed user-friendly error message
    pub fn message(&self) -> &'static str {
        match self {
            FilManagerError::FsOp(err, _context) => {
                match err {
                    FsError::NotFound => "File or directory not found",
                    FsError::Exists => "File or directory already exists",
                    FsError::IsDir => "Is a directory",
                    FsError::NotDir => "Not a directory",
                    FsError::NotEmpty => "Directory not empty",
                    FsError::PermissionDenied => "Permission denied",
                    FsError::ReadOnly => "Read-only filesystem",
                    FsError::BadFd => "Bad file descriptor",
                    FsError::NameTooLong => "Filename too long",
                    FsError::NoSpace => "No space left on device",
                    FsError::Io => "I/O error",
                    FsError::CrossDevice => "Cross-device link",
                    FsError::FileTooLarge => "File too large",
                    FsError::Corrupted => "Filesystem corrupted",
                    FsError::TooManyFiles => "Too many open files",
                    FsError::TooManyLinks => "Too many links",
                    FsError::Overflow => "Value too large for data type",
                    FsError::BrokenPipe => "Broken pipe",
                    FsError::NotSupported => "Operation not supported",
                    FsError::WouldBlock => "Operation would block",
                    FsError::Interrupted => "Operation interrupted",
                    FsError::ArgListTooLong => "Argument list too long",
                    FsError::InvalidArg => "Invalid argument",
                    FsError::Other(code) if *code == ETIMEDOUT => "Filesystem service timed out",
                    FsError::Other(code) if *code == ENODEV => "Filesystem service unavailable",
                    FsError::Other(code) if *code == EBUSY => "Filesystem service busy",
                    FsError::Other(_) => "Unknown filesystem error",
                }
            },
            FilManagerError::InvalidCommand(CommandError::WrongArgCount { .. }) => {
                "Wrong number of arguments"
            },
            FilManagerError::InvalidCommand(CommandError::UnknownCommand(_)) => {
                "Unknown command"
            },
            FilManagerError::InvalidCommand(CommandError::MissingArg(_)) => {
                "Missing required argument"
            },
            FilManagerError::InvalidCommand(CommandError::ConflictingOptions) => {
                "Conflicting command options"
            },
            FilManagerError::IO => "I/O error",
            FilManagerError::PathTooLong => "Path exceeds maximum length",
            FilManagerError::InvalidPath => "Invalid path format",
            FilManagerError::CrossDevice => "Cross-filesystem operation not allowed",
        }
    }

    /// True when fsd is absent/not responding via kernel FS forwarding.
    pub fn is_fs_service_unavailable(&self) -> bool {
        matches!(
            self,
            FilManagerError::FsOp(FsError::Other(code), _) if *code == ENODEV || *code == ETIMEDOUT
        )
    }

    /// True when the kernel->fsd RPC hit timeout.
    pub fn is_fs_service_timeout(&self) -> bool {
        matches!(self, FilManagerError::FsOp(FsError::Other(code), _) if *code == ETIMEDOUT)
    }
}

impl fmt::Display for FilManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilManagerError::FsOp(_, context) => {
                write!(f, "fileman: {}: {}", context, self.message())
            },
            FilManagerError::InvalidCommand(ce) => {
                match ce {
                    CommandError::WrongArgCount { expected, got } => {
                        write!(f, "fileman: wrong number of arguments (expected {}, got {})", expected, got)
                    },
                    CommandError::UnknownCommand(_len) => {
                        write!(f, "fileman: unknown command")
                    },
                    CommandError::MissingArg(arg) => {
                        write!(f, "fileman: missing argument: {}", arg)
                    },
                    CommandError::ConflictingOptions => {
                        write!(f, "fileman: conflicting command options")
                    },
                }
            },
            _ => write!(f, "fileman: {}", self.message()),
        }
    }
}

pub type Result<T> = core::result::Result<T, FilManagerError>;

/// Verify path length is within limits
pub fn validate_path_len(path: &str) -> Result<()> {
    const MAX_PATH: usize = 4096;
    if path.len() > MAX_PATH {
        Err(FilManagerError::PathTooLong)
    } else {
        Ok(())
    }
}

/// Clean and normalize a path
/// Returns canonical path for consistent handling
pub fn normalize_path(path: &str) -> Result<alloc::string::String> {
    use alloc::string::String;
    use alloc::vec::Vec;

    if path.is_empty() {
        return Err(FilManagerError::InvalidPath);
    }

    validate_path_len(path)?;

    // Split by '/', filter empty parts, rejoin
    let parts: Vec<&str> = path.split('/').collect();
    
    // Keep leading slash if absolute
    let is_absolute = path.starts_with('/');
    
    // Remove dot and dot-dot segments (simple approach)
    let mut result = if is_absolute {
        String::from("/")
    } else {
        String::new()
    };

    for (i, part) in parts.iter().enumerate() {
        if i == 0 && is_absolute {
            continue; // skip leading empty from splitting
        }
        
        if part.is_empty() || *part == "." {
            continue;
        }
        
        if *part == ".." {
            // Go up one level (simplified: just remove last component)
            if let Some(last_slash) = result.rfind('/') {
                if last_slash == 0 {
                    result = String::from("/");
                } else {
                    result.truncate(last_slash);
                }
            }
        } else {
            if !result.ends_with('/') && !result.is_empty() {
                result.push('/');
            }
            result.push_str(part);
        }
    }

    // Ensure root path at least
    if result.is_empty() {
        result.push('/');
    }

    Ok(result)
}
