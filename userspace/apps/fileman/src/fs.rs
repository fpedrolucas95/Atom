/// Filesystem abstraction layer
/// 
/// Wraps syscalls with error handling and provides high-level operations.
/// All operations are production-ready with complete error checking.

use atom_syscall::fs;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;
use crate::error::{FilManagerError, Result};

/// File access mode
#[derive(Debug, Clone, Copy)]
pub enum FileMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

impl FileMode {
    fn to_flags(self) -> u32 {
        match self {
            FileMode::ReadOnly => fs::OpenFlags::RDONLY,
            FileMode::WriteOnly => fs::OpenFlags::WRONLY,
            FileMode::ReadWrite => fs::OpenFlags::RDWR,
        }
    }
}

/// High-level file handle
pub struct File {
    fd: u64,
}

impl File {
    /// Open a file with specified mode
    pub fn open(path: &str, mode: FileMode) -> Result<Self> {
        crate::error::validate_path_len(path)?;
        
        let fd = fs::open(path, mode.to_flags(), 0o644)
            .map_err(|e| FilManagerError::fs(e, "open"))?;
        
        Ok(File { fd })
    }

    /// Create a new file, failing if it exists
    pub fn create(path: &str) -> Result<Self> {
        crate::error::validate_path_len(path)?;
        
        let flags = fs::OpenFlags::CREAT | fs::OpenFlags::EXCL | fs::OpenFlags::WRONLY;
        let fd = fs::open(path, flags, 0o644)
            .map_err(|e| FilManagerError::fs(e, "create"))?;
        
        Ok(File { fd })
    }

    /// Truncate and overwrite existing file
    pub fn truncate(path: &str) -> Result<Self> {
        crate::error::validate_path_len(path)?;
        
        let flags = fs::OpenFlags::CREAT | fs::OpenFlags::TRUNC | fs::OpenFlags::WRONLY;
        let fd = fs::open(path, flags, 0o644)
            .map_err(|e| FilManagerError::fs(e, "truncate"))?;
        
        Ok(File { fd })
    }

    /// Read all file contents into memory
    pub fn read_all(&mut self) -> Result<Vec<u8>> {
        fs::read_all(self.fd)
            .map_err(|e| FilManagerError::fs(e, "read"))
    }

    /// Read bytes into buffer
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        fs::read(self.fd, buf)
            .map_err(|e| FilManagerError::fs(e, "read"))
    }

    /// Write all data to file
    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        fs::write_all(self.fd, data)
            .map_err(|e| FilManagerError::fs(e, "write"))
    }

    /// Write bytes from buffer
    pub fn write(&mut self, buf: &[u8]) -> Result<usize> {
        fs::write(self.fd, buf)
            .map_err(|e| FilManagerError::fs(e, "write"))
    }

    /// Seek to position in file
    pub fn seek(&mut self, offset: i64, whence: u32) -> Result<u64> {
        fs::lseek(self.fd, offset, whence)
            .map_err(|e| FilManagerError::fs(e, "seek"))
    }

    /// Get file statistics
    pub fn stat(&self) -> Result<fs::FsStat> {
        fs::fstat(self.fd)
            .map_err(|e| FilManagerError::fs(e, "fstat"))
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let _ = fs::close(self.fd);
    }
}

/// Directory operations
pub struct Dir {
    fd: u64,
    path: String,
    entries: Vec<fs::FsDirent>,
    position: usize,
}

impl Dir {
    /// Open a directory
    pub fn open(path: &str) -> Result<Self> {
        crate::error::validate_path_len(path)?;
        
        let flags = fs::OpenFlags::RDONLY | fs::OpenFlags::DIRECTORY;
        let fd = fs::open(path, flags, 0)
            .map_err(|e| FilManagerError::fs(e, "opendir"))?;
        
        // Read entries immediately to ensure directory is valid
        let entries = fs::readdir(fd)
            .map_err(|e| FilManagerError::fs(e, "readdir"))?;
        
        Ok(Dir {
            fd,
            path: path.to_string(),
            entries,
            position: 0,
        })
    }

    /// Get all entries at once
    pub fn entries(&self) -> &[fs::FsDirent] {
        &self.entries
    }

    /// List formatted entries (for display)
    pub fn list(&self) -> Result<Vec<DirEntry>> {
        let mut result = Vec::new();
        
        for entry in &self.entries {
            // Get full path for stat
            let full_path = if self.path.ends_with('/') {
                format!("{}{}", self.path, entry.name)
            } else {
                format!("{}/{}", self.path, entry.name)
            };

            // Get stat info
            match fs::stat(&full_path) {
                Ok(stat) => {
                    result.push(DirEntry {
                        name: entry.name.clone(),
                        file_type: entry.file_type,
                        size: stat.size,
                        mode: stat.mode,
                    });
                },
                Err(_) => {
                    // If stat fails, still include entry with minimal info
                    result.push(DirEntry {
                        name: entry.name.clone(),
                        file_type: entry.file_type,
                        size: 0,
                        mode: 0,
                    });
                }
            }
        }

        // Sort by name for consistent output
        result.sort_by(|a, b| a.name.cmp(&b.name));
        
        Ok(result)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::close(self.fd);
    }
}

pub struct DirEntry {
    pub name: String,
    pub file_type: fs::FileType,
    pub size: u64,
    pub mode: u32,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.file_type == fs::FileType::Directory
    }

    pub fn is_file(&self) -> bool {
        self.file_type == fs::FileType::Regular
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type == fs::FileType::Symlink
    }

    pub fn type_char(&self) -> char {
        match self.file_type {
            fs::FileType::Regular => '-',
            fs::FileType::Directory => 'd',
            fs::FileType::Symlink => 'l',
            fs::FileType::BlockDevice => 'b',
            fs::FileType::CharDevice => 'c',
            fs::FileType::Fifo => 'p',
            fs::FileType::Socket => 's',
            fs::FileType::Unknown => '?',
        }
    }

    pub fn size_string(&self) -> String {
        if self.is_dir() {
            String::from("-")
        } else {
            format_size(self.size)
        }
    }

    pub fn mode_string(&self) -> String {
        format!("{:04o}", self.mode & 0o7777)
    }
}

/// Format file size for human reading
fn format_size(bytes: u64) -> String {
    
    const UNITS: &[&str] = &["B", "K", "M", "G", "T"];
    
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    
    if unit_idx == 0 {
        format!("{}{}", bytes, UNITS[0])
    } else {
        // Format with 1 decimal place
        let truncated = (size * 10.0) as u64;
        format!("{}.{}{}", truncated / 10, truncated % 10, UNITS[unit_idx])
    }
}

/// File system query operations
pub struct FsQuery;

impl FsQuery {
    /// Check if path exists
    pub fn exists(path: &str) -> Result<bool> {
        crate::error::validate_path_len(path)?;
        
        match fs::stat(path) {
            Ok(_) => Ok(true),
            Err(fs::FsError::NotFound) => Ok(false),
            Err(e) => Err(FilManagerError::fs(e, "stat")),
        }
    }

    /// Get file status
    pub fn stat(path: &str) -> Result<fs::FsStat> {
        crate::error::validate_path_len(path)?;
        
        fs::stat(path)
            .map_err(|e| FilManagerError::fs(e, "stat"))
    }

    /// Check if path is directory
    pub fn is_dir(path: &str) -> Result<bool> {
        match Self::stat(path) {
            Ok(stat) => Ok(stat.is_dir()),
            Err(e) => {
                // NotFound is not an error for existence check
                if matches!(e, FilManagerError::FsOp(fs::FsError::NotFound, _)) {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Check if path is regular file
    pub fn is_file(path: &str) -> Result<bool> {
        match Self::stat(path) {
            Ok(stat) => Ok(stat.is_file()),
            Err(e) => {
                if matches!(e, FilManagerError::FsOp(fs::FsError::NotFound, _)) {
                    Ok(false)
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// Path-based filesystem operations
pub struct FsOps;

impl FsOps {
    /// Create directory
    pub fn mkdir(path: &str) -> Result<()> {
        crate::error::validate_path_len(path)?;
        
        fs::mkdir(path, 0o755)
            .map_err(|e| FilManagerError::fs(e, "mkdir"))
    }

    /// Remove empty directory
    pub fn rmdir(path: &str) -> Result<()> {
        crate::error::validate_path_len(path)?;
        
        fs::rmdir(path)
            .map_err(|e| FilManagerError::fs(e, "rmdir"))
    }

    /// Remove file
    pub fn unlink(path: &str) -> Result<()> {
        crate::error::validate_path_len(path)?;
        
        fs::unlink(path)
            .map_err(|e| FilManagerError::fs(e, "unlink"))
    }

    /// Rename/move file or directory
    pub fn rename(from: &str, to: &str) -> Result<()> {
        crate::error::validate_path_len(from)?;
        crate::error::validate_path_len(to)?;
        
        fs::rename(from, to)
            .map_err(|e| FilManagerError::fs(e, "rename"))
    }

    /// Copy file (read source, write destination)
    pub fn copy(from: &str, to: &str) -> Result<()> {
        crate::error::validate_path_len(from)?;
        crate::error::validate_path_len(to)?;

        // Check if destination is directory
        if FsQuery::is_dir(to)? {
            // Copy into directory with same filename
            let filename = from.split('/').last().unwrap_or("file");
            let target = if to.ends_with('/') {
                format!("{}{}", to, filename)
            } else {
                format!("{}/{}", to, filename)
            };
            return Self::copy_file(from, &target);
        }

        Self::copy_file(from, to)
    }

    fn copy_file(from: &str, to: &str) -> Result<()> {
        let mut src = File::open(from, FileMode::ReadOnly)?;
        let mut dst = File::truncate(to)?;

        const BUF_SIZE: usize = 65536;
        let mut buf = [0u8; BUF_SIZE];

        loop {
            let n = src.read(&mut buf)?;
            if n == 0 { break; }
            dst.write_all(&buf[..n])?;
        }

        Ok(())
    }

    /// Recursively remove directory
    pub fn rm_recursive(path: &str) -> Result<()> {
        crate::error::validate_path_len(path)?;

        let stat = FsQuery::stat(path)?;
        
        if !stat.is_dir() {
            // It's a file, just unlink it
            return FsOps::unlink(path);
        }

        // It's a directory, remove contents recursively
        let dir = Dir::open(path)?;
        let entries = dir.entries().to_vec();
        drop(dir); // Close directory before deleting contents

        for entry in entries {
            // Skip . and ..
            if entry.name == "." || entry.name == ".." {
                continue;
            }

            let full_path = if path.ends_with('/') {
                format!("{}{}", path, entry.name)
            } else {
                format!("{}/{}", path, entry.name)
            };

            if entry.file_type == fs::FileType::Directory {
                Self::rm_recursive(&full_path)?;
            } else {
                FsOps::unlink(&full_path)?;
            }
        }

        // Remove the now-empty directory
        FsOps::rmdir(path)?;
        Ok(())
    }
}
