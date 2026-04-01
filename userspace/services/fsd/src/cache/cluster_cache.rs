// cache/cluster_cache.rs — LRU cache for data cluster contents
//
// Each entry holds a full cluster worth of bytes.  Dirty entries are flushed
// before eviction or on explicit flush_dirty() / sync_volume() calls.
//
// Write policy: the same WRITE-BACK policy as FatCache.
// The ordering rule DATA_BEFORE_META is enforced by Fat32JournalOps, which
// calls cluster_cache.flush_dirty() for data sectors BEFORE updating FAT or
// directory entries.

#![allow(dead_code)]

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use crate::block_client::BlockClient;
use crate::cache::lru::LruCache;
use crate::error::{VfsError, VfsResult};

/// Default cluster cache capacity.
pub const CLUSTER_CACHE_ENTRIES: usize = 32;

/// Cache for cluster data.
///
/// Key = cluster number (u32).
/// Value = entire cluster contents (Vec<u8> of exactly cluster_size bytes).
pub struct ClusterCache {
    lru:          LruCache<u32, Vec<u8>>,
    cluster_size: usize,
}

impl ClusterCache {
    pub fn new(cluster_size: usize, capacity: usize) -> Self {
        ClusterCache {
            lru: LruCache::new(capacity),
            cluster_size,
        }
    }

    // ── Public API ──────────────────────────────────────────────────────────

    /// Read the full contents of cluster `n` into a caller-supplied buffer.
    /// Fetches from block device on cache miss.
    pub fn read_cluster(
        &mut self,
        block:           &BlockClient,
        cluster_n:       u32,
        first_data_lba:  u64,
        sectors_per_clus: u32,
        buf:             &mut [u8],
    ) -> VfsResult<()> {
        if buf.len() != self.cluster_size {
            return Err(VfsError::InvalidArgument);
        }
        self.fetch(block, cluster_n, first_data_lba, sectors_per_clus)?;
        let data = self.lru.get(cluster_n).ok_or(VfsError::Io)?;
        buf.copy_from_slice(data.as_slice());
        Ok(())
    }

    /// Write data into the cache for cluster `n`.
    ///
    /// `data` must be exactly `cluster_size` bytes.
    /// Does NOT immediately write to disk — caller must call flush_dirty().
    pub fn write_cluster(
        &mut self,
        block:           &BlockClient,
        cluster_n:       u32,
        first_data_lba:  u64,
        sectors_per_clus: u32,
        data:            &[u8],
    ) -> VfsResult<()> {
        if data.len() != self.cluster_size {
            return Err(VfsError::InvalidArgument);
        }
        // Ensure the entry is in the cache (may do partial read)
        self.fetch(block, cluster_n, first_data_lba, sectors_per_clus)?;
        if let Some(entry) = self.lru.get_mut(cluster_n) {
            entry.data.copy_from_slice(data);
            entry.dirty = true;
        }
        Ok(())
    }

    /// Write a sub-range within cluster `n`.
    pub fn write_cluster_partial(
        &mut self,
        block:           &BlockClient,
        cluster_n:       u32,
        first_data_lba:  u64,
        sectors_per_clus: u32,
        offset_in_cluster: usize,
        data:            &[u8],
    ) -> VfsResult<()> {
        if offset_in_cluster + data.len() > self.cluster_size {
            return Err(VfsError::InvalidArgument);
        }
        self.fetch(block, cluster_n, first_data_lba, sectors_per_clus)?;
        if let Some(entry) = self.lru.get_mut(cluster_n) {
            let end = offset_in_cluster + data.len();
            entry.data[offset_in_cluster..end].copy_from_slice(data);
            entry.dirty = true;
        }
        Ok(())
    }

    /// Flush all dirty clusters to the block device.
    ///
    /// `first_data_lba` and `sectors_per_clus` are needed to compute the LBA
    /// of each cluster.  Must be called BEFORE FAT entries are updated
    /// (DATA_BEFORE_META rule from architecture plan).
    pub fn flush_dirty(
        &mut self,
        block:           &BlockClient,
        first_data_lba:  u64,
        sectors_per_clus: u32,
    ) -> VfsResult<()> {
        let dirty: Vec<u32> = self.lru.dirty_keys();
        for cluster_n in dirty {
            let entry = match self.lru.get_mut(cluster_n) {
                Some(e) => e,
                None    => continue,
            };
            if !entry.dirty { continue; }
            let data = entry.data.clone();

            let lba = cluster_to_lba(cluster_n, first_data_lba, sectors_per_clus);
            block.write_sectors(lba, sectors_per_clus, &data)?;

            if let Some(e) = self.lru.get_mut(cluster_n) {
                e.dirty = false;
            }
        }
        Ok(())
    }

    /// Flush a single specific cluster.  Used by fsync().
    pub fn flush_cluster(
        &mut self,
        block:           &BlockClient,
        cluster_n:       u32,
        first_data_lba:  u64,
        sectors_per_clus: u32,
    ) -> VfsResult<()> {
        let entry = match self.lru.get_mut(cluster_n) {
            Some(e) if e.dirty => e,
            _ => return Ok(()),
        };
        let data = entry.data.clone();
        let lba  = cluster_to_lba(cluster_n, first_data_lba, sectors_per_clus);
        block.write_sectors(lba, sectors_per_clus, &data)?;
        if let Some(e) = self.lru.get_mut(cluster_n) {
            e.dirty = false;
        }
        Ok(())
    }

    /// Remove a cluster from the cache (e.g. after it is freed).
    /// Does NOT write dirty data — caller must flush before calling this.
    pub fn invalidate(&mut self, cluster_n: u32) {
        // LruCache has no remove(); just mark clean so it evicts normally
        self.lru.mark_clean(cluster_n);
    }

    // ── Private ─────────────────────────────────────────────────────────────

    fn fetch(
        &mut self,
        block:           &BlockClient,
        cluster_n:       u32,
        first_data_lba:  u64,
        sectors_per_clus: u32,
    ) -> VfsResult<()> {
        if self.lru.contains(cluster_n) { return Ok(()); }

        let mut buf = vec![0u8; self.cluster_size];
        let lba = cluster_to_lba(cluster_n, first_data_lba, sectors_per_clus);
        block.read_sectors(lba, sectors_per_clus, &mut buf)?;

        self.lru.insert(cluster_n, buf, false);
        Ok(())
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Compute the first LBA of cluster `n`.
/// Spec formula: FirstSectorOfCluster = ((N - 2) * BPB_SecPerClus) + FirstDataSector
#[inline]
pub fn cluster_to_lba(cluster_n: u32, first_data_lba: u64, sectors_per_clus: u32) -> u64 {
    first_data_lba + (cluster_n as u64 - 2) * sectors_per_clus as u64
}
