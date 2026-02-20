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

/// Default block device with IPC-based backend
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
        // In real implementation, would send BlockRead IPC message
        // For now, return placeholder
        let bytes = (count as usize) * (self.sector_size as usize);
        Some(alloc::vec![0u8; bytes])
    }

    pub fn write_sectors(&self, lba: u64, data: &[Vec<u8>]) -> Option<bool> {
        // In real implementation, would send BlockWrite IPC message
        if self.read_only {
            return Some(false);
        }

        // Calculate total bytes
        let total_bytes: usize = data.iter().map(|v| v.len()).sum();
        if total_bytes % (self.sector_size as usize) != 0 {
            return Some(false);
        }

        Some(true)
    }

    pub fn flush(&self) -> Option<bool> {
        // In real implementation, would send BlockFlush IPC message
        // Returns only after barriers complete
        Some(true)
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
