// Block device abstraction layer for journal and filesystem operations
// Handles reading/writing sectors and barrier semantics

extern crate alloc;

use alloc::vec::Vec;

/// Block device interface trait
pub trait BlockDeviceTrait: Send + Sync {
    fn read_sectors(&self, lba: u64, count: u32) -> Option<Vec<u8>>;
    fn write_sectors(&self, lba: u64, data: &[Vec<u8>]) -> Option<bool>;
    fn flush(&self) -> Option<bool>;
    fn identify(&self) -> Option<BlockDeviceInfo>;
}

/// Block device information
#[derive(Clone, Debug)]
pub struct BlockDeviceInfo {
    pub total_sectors: u64,
    pub sector_size: u32,
    pub optimal_xfer: u32,
    pub supports_trim: bool,
    pub read_only: bool,
}

/// Default block device with kernel block-syscall backend
pub struct BlockDevice {
    sector_size: u32,
    total_sectors: u64,
    read_only: bool,
}

impl BlockDevice {
    pub fn new(sector_size: u32, total_sectors: u64) -> Self {
        Self {
            sector_size,
            total_sectors,
            read_only: false,
        }
    }

    pub fn read_sectors(&self, lba: u64, count: u32) -> Option<Vec<u8>> {
        if count == 0 {
            return Some(Vec::new());
        }
        let bytes = (count as usize) * (self.sector_size as usize);
        let mut out = alloc::vec![0u8; bytes];
        match atom_syscall::fs::kern_block_read(lba, count, &mut out) {
            Ok(n) if n == bytes => Some(out),
            Ok(n) if n < bytes => {
                out.truncate(n);
                Some(out)
            }
            Ok(_) => None,
            Err(_) => None,
        }
    }

    pub fn write_sectors(&self, lba: u64, data: &[Vec<u8>]) -> Option<bool> {
        if self.read_only {
            return Some(false);
        }

        let total_bytes: usize = data.iter().map(|v| v.len()).sum();
        let sector_size = self.sector_size as usize;
        if total_bytes == 0 || total_bytes % sector_size != 0 {
            return Some(false);
        }
        let mut flat = Vec::with_capacity(total_bytes);
        for chunk in data {
            flat.extend_from_slice(chunk);
        }
        let sectors = (flat.len() / sector_size) as u32;
        match atom_syscall::fs::kern_block_write(lba, sectors, &flat) {
            Ok(()) => Some(true),
            Err(_) => Some(false),
        }
    }

    pub fn flush(&self) -> Option<bool> {
        match atom_syscall::fs::kern_block_flush() {
            Ok(()) => Some(true),
            Err(_) => Some(false),
        }
    }

    pub fn identify(&self) -> Option<BlockDeviceInfo> {
        Some(BlockDeviceInfo {
            total_sectors: self.total_sectors,
            sector_size: self.sector_size,
            optimal_xfer: 8,
            supports_trim: false,
            read_only: self.read_only,
        })
    }
}
