//! FAT32 Filesystem Driver
//!
//! This module provides the interface to the kernel's FAT32 driver.
//! Since the actual FAT32 implementation lives in the kernel (kernel/src/drivers/fat32.rs),
//! this module provides a userspace-facing wrapper that would eventually:
//! - Invoke syscalls to communicate with the kernel FAT32 driver
//! - Cache filesystem metadata
//! - Manage file handles
//!
//! For now, this is a minimal stub that demonstrates the interface.

use alloc::string::String;
use alloc::vec::Vec;
use crate::vfs::{VfsError, VfsResult, InodeStat};

// ============================================================================
// FAT32 Driver interface
// ============================================================================

/// FAT32 filesystem state
pub struct Fat32Driver {
    /// Device path backing this filesystem
    device: String,
    /// Whether the filesystem is initialized
    initialized: bool,
}

impl Fat32Driver {
    /// Create a new FAT32 driver instance for a device
    pub fn new(device: &str) -> Self {
        Self {
            device: String::from(device),
            initialized: false,
        }
    }

    /// Initialize the FAT32 driver (mount)
    pub fn init(&mut self) -> VfsResult<()> {
        // TODO: Call kernel syscall to initialize FAT32 on this device
        // For now, just mark as initialized
        self.initialized = true;
        Ok(())
    }

    /// Close the driver (unmount)
    pub fn close(&mut self) -> VfsResult<()> {
        if self.initialized {
            // TODO: Call kernel syscall to sync and close
            self.initialized = false;
        }
        Ok(())
    }

    /// Open a file by path
    pub fn open(&self, _path: &str, _flags: u32, _mode: u32) -> VfsResult<u64> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to open file
        // - path: file path within this filesystem
        // - flags: O_RDONLY, O_WRONLY, O_RDWR, O_CREAT, etc.
        // - mode: file permissions
        // Returns inode number or handle
        Err(VfsError::NotSupported)
    }

    /// Close a file
    pub fn close_file(&self, _inode: u64) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to close file
        Err(VfsError::NotSupported)
    }

    /// Read from file
    pub fn read(&self, _inode: u64, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to read file data
        // - inode: file inode
        // - offset: read position
        // - buf: target buffer
        // Returns bytes read
        Err(VfsError::NotSupported)
    }

    /// Write to file
    pub fn write(&self, _inode: u64, _offset: u64, _data: &[u8]) -> VfsResult<usize> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to write file data
        // - inode: file inode
        // - offset: write position
        // - data: source buffer
        // Returns bytes written
        Err(VfsError::NotSupported)
    }

    /// Get file stats
    pub fn stat(&self, _path: &str) -> VfsResult<InodeStat> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to stat file
        // - path: file path within this filesystem
        // Returns InodeStat with metadata
        Err(VfsError::NotSupported)
    }

    /// Get file stats by inode
    pub fn fstat(&self, _inode: u64) -> VfsResult<InodeStat> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to fstat
        // - inode: file inode number
        // Returns InodeStat with metadata
        Err(VfsError::NotSupported)
    }

    /// List directory entries
    pub fn readdir(&self, _path: &str) -> VfsResult<Vec<DirEntry>> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to read directory
        // - path: directory path within this filesystem
        // Returns list of directory entries
        Err(VfsError::NotSupported)
    }

    /// Create directory
    pub fn mkdir(&self, _path: &str, _mode: u32) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to create directory
        // - path: directory path within this filesystem
        // - mode: directory permissions
        Err(VfsError::NotSupported)
    }

    /// Remove directory
    pub fn rmdir(&self, _path: &str) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to remove directory
        // - path: directory path (must be empty)
        Err(VfsError::NotSupported)
    }

    /// Delete file
    pub fn unlink(&self, _path: &str) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to delete file
        // - path: file path within this filesystem
        Err(VfsError::NotSupported)
    }

    /// Rename file
    pub fn rename(&self, _old_path: &str, _new_path: &str) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to rename file
        // - old_path: current path
        // - new_path: new path (same directory for FAT32)
        Err(VfsError::NotSupported)
    }

    /// Truncate file
    pub fn truncate(&self, _inode: u64, _new_size: u64) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to truncate
        // - inode: file to truncate
        // - new_size: new file size
        Err(VfsError::NotSupported)
    }

    /// Get filesystem statistics
    pub fn statvfs(&self) -> VfsResult<(u64, u64, u64)> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to get statvfs
        // Returns (block_size, total_blocks, free_blocks)
        Err(VfsError::NotSupported)
    }

    /// Change file permissions
    pub fn chmod(&self, _path: &str, _mode: u32) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: FAT32 doesn't natively support Unix permissions
        // But we can store them in extended attributes or just return success
        Ok(())
    }

    /// Sync filesystem to disk
    pub fn fsync(&self) -> VfsResult<()> {
        if !self.initialized {
            return Err(VfsError::IoError);
        }

        // TODO: Call kernel syscall to sync filesystem
        Err(VfsError::NotSupported)
    }
}

// ============================================================================
// Directory entry structure
// ============================================================================

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub inode: u64,
    pub file_type: u32, // POSIX file type bits
    pub size: u64,
}

// ============================================================================
// FAT32 specific constants
// ============================================================================

// File type flags (embedded in FAT32 directory attributes)
pub const FAT32_ATTR_READ_ONLY: u8 = 0x01;
pub const FAT32_ATTR_HIDDEN: u8 = 0x02;
pub const FAT32_ATTR_SYSTEM: u8 = 0x04;
pub const FAT32_ATTR_VOLUME_ID: u8 = 0x08;
pub const FAT32_ATTR_DIRECTORY: u8 = 0x10;
pub const FAT32_ATTR_ARCHIVE: u8 = 0x20;

/// Convert FAT32 attributes to file type
pub fn fat32_to_file_type(attr: u8) -> u32 {
    if (attr & FAT32_ATTR_DIRECTORY) != 0 {
        0o040000 // S_IFDIR
    } else {
        0o100000 // S_IFREG
    }
}
