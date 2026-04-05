//! Filesystem backend abstraction for fsd.
//!
//! `ipc.rs` depends on this trait so the active filesystem implementation
//! is selected in one place (`main.rs`) while IPC/wire behavior stays stable.

use alloc::string::String;
use atom_syscall::fs::FsError;

pub trait FsBackend {
    fn stat_path(&self, path: &str, stat_buf: &mut [u8; 80]) -> Result<(), FsError>;
    /// Return (first_cluster, file_size, is_dir) without reading data.
    fn open_info(&self, path: &str) -> Result<(u32, u64, bool), FsError>;
    /// Read up to buf.len() bytes from a file at the given offset.
    fn read_range(&self, first_cluster: u32, file_size: u64, offset: u64, buf: &mut [u8]) -> Result<usize, FsError>;
    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FsError>;
    fn list_dir(&self, path: &str, buf: &mut [u8]) -> Result<usize, FsError>;
    fn mkdir(&self, path: &str) -> Result<(), FsError>;
    fn rmdir(&self, path: &str) -> Result<(), FsError>;
    fn unlink(&self, path: &str) -> Result<(), FsError>;
    fn rename(&self, old_path: &str, new_path: &str) -> Result<(), FsError>;
    fn sync_fs(&self) -> Result<(), FsError>;
    fn flush_block_cache(&self) -> Result<(), FsError>;
}

fn encode_stat(path: &str, stat_buf: &mut [u8; 80], size: u64, is_dir: bool) {
    stat_buf.fill(0);

    if path == "/" || path.is_empty() {
        stat_buf[8..16].copy_from_slice(&2u64.to_le_bytes()); // inode
        let mode: u32 = 0o040755;
        stat_buf[40..44].copy_from_slice(&mode.to_le_bytes());
        stat_buf[48..52].copy_from_slice(&2u32.to_le_bytes()); // nlinks
        stat_buf[56..60].copy_from_slice(&512u32.to_le_bytes()); // blksize
        return;
    }

    stat_buf[0..8].copy_from_slice(&size.to_le_bytes()); // size
    stat_buf[8..16].copy_from_slice(&1u64.to_le_bytes()); // inode
    let mode: u32 = if is_dir { 0o040755 } else { 0o100644 };
    stat_buf[40..44].copy_from_slice(&mode.to_le_bytes());
    let nlinks: u32 = if is_dir { 2 } else { 1 };
    stat_buf[48..52].copy_from_slice(&nlinks.to_le_bytes());
    stat_buf[56..60].copy_from_slice(&512u32.to_le_bytes()); // blksize
}

fn pack_dirents(entries: &[String], out: &mut [u8]) -> usize {
    let mut pos = 0usize;
    let mut ino_counter: u32 = 1;

    for entry_name in entries {
        let (name, ftype) = if entry_name.ends_with('/') {
            (&entry_name[..entry_name.len().saturating_sub(1)], 2u8)
        } else {
            (entry_name.as_str(), 1u8)
        };

        let name_bytes = name.as_bytes();
        let name_len = name_bytes.len().min(255);
        let raw_len = 8 + name_len;
        let rec_len = (raw_len + 3) & !3; // 4-byte align

        if pos + rec_len > out.len() {
            break;
        }

        out[pos..pos + rec_len].fill(0);
        out[pos..pos + 4].copy_from_slice(&ino_counter.to_le_bytes());
        out[pos + 4..pos + 6].copy_from_slice(&(rec_len as u16).to_le_bytes());
        out[pos + 6] = name_len as u8;
        out[pos + 7] = ftype;
        out[pos + 8..pos + 8 + name_len].copy_from_slice(&name_bytes[..name_len]);

        pos += rec_len;
        ino_counter = ino_counter.saturating_add(1);
    }

    pos
}

/// Userspace FAT32 backend: fsd owns filesystem state and accesses disk via
/// raw block syscalls (`kern_block_*`) through `fat32.rs`.
pub struct UserspaceFatBackend;

impl FsBackend for UserspaceFatBackend {
    fn stat_path(&self, path: &str, stat_buf: &mut [u8; 80]) -> Result<(), FsError> {
        if path == "/" || path.is_empty() {
            encode_stat(path, stat_buf, 0, true);
            return Ok(());
        }
        let st = crate::fat32::stat_path(path).ok_or(FsError::NotFound)?;
        encode_stat(path, stat_buf, st.size, st.is_dir);
        Ok(())
    }

    fn open_info(&self, path: &str) -> Result<(u32, u64, bool), FsError> {
        if path == "/" || path.is_empty() {
            return Ok((0, 0, true));
        }
        let info = crate::fat32::open_info(path).ok_or(FsError::NotFound)?;
        Ok((info.first_cluster, info.size, info.is_dir))
    }

    fn read_range(&self, first_cluster: u32, file_size: u64, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        crate::fat32::read_file_range(first_cluster, file_size, offset, buf)
    }

    fn read_file(&self, path: &str, buf: &mut [u8]) -> Result<usize, FsError> {
        let data = crate::fat32::open(path).ok_or(FsError::NotFound)?;
        let to_copy = core::cmp::min(data.len(), buf.len());
        buf[..to_copy].copy_from_slice(&data[..to_copy]);
        Ok(to_copy)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> Result<(), FsError> {
        if let Some(st) = crate::fat32::stat_path(path) {
            if st.is_dir {
                return Err(FsError::IsDir);
            }
        }
        if crate::fat32::write_file(path, data) {
            Ok(())
        } else {
            Err(FsError::Io)
        }
    }

    fn list_dir(&self, path: &str, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Err(FsError::InvalidArg);
        }
        let entries = crate::fat32::list_directory(path).ok_or(FsError::NotFound)?;
        Ok(pack_dirents(&entries, buf))
    }

    fn mkdir(&self, path: &str) -> Result<(), FsError> {
        if crate::fat32::stat_path(path).is_some() {
            return Err(FsError::Exists);
        }
        if crate::fat32::mkdir(path) {
            Ok(())
        } else {
            Err(FsError::Io)
        }
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        let st = crate::fat32::stat_path(path).ok_or(FsError::NotFound)?;
        if !st.is_dir {
            return Err(FsError::NotDir);
        }
        if crate::fat32::rmdir(path) {
            Ok(())
        } else {
            Err(FsError::NotEmpty)
        }
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let st = crate::fat32::stat_path(path).ok_or(FsError::NotFound)?;
        if st.is_dir {
            return Err(FsError::IsDir);
        }
        if crate::fat32::unlink(path) {
            Ok(())
        } else {
            Err(FsError::Io)
        }
    }

    fn rename(&self, old_path: &str, new_path: &str) -> Result<(), FsError> {
        if crate::fat32::stat_path(old_path).is_none() {
            return Err(FsError::NotFound);
        }
        if crate::fat32::stat_path(new_path).is_some() {
            return Err(FsError::Exists);
        }
        if crate::fat32::rename(old_path, new_path) {
            Ok(())
        } else {
            Err(FsError::Io)
        }
    }

    fn sync_fs(&self) -> Result<(), FsError> {
        if crate::fat32::sync() {
            Ok(())
        } else {
            Err(FsError::Io)
        }
    }

    fn flush_block_cache(&self) -> Result<(), FsError> {
        let Some(bd) = crate::get_block_device() else {
            return Err(FsError::Io);
        };
        match bd.flush() {
            Some(true) => Ok(()),
            _ => Err(FsError::Io),
        }
    }
}
