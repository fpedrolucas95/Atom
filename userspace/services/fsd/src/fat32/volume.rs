// fat32/volume.rs — FatVolume: runtime state for a mounted FAT32 volume
//
// FatVolume holds the parsed VolumeParams, the three caches, and the
// dirty-volume flag.  It is the central object through which all FAT32
// operations are executed.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;

use crate::block_client::BlockClient;
use crate::cache::fat_cache::FatCache;
use crate::cache::cluster_cache::ClusterCache;
use crate::error::{VfsError, VfsResult};
use super::bpb::{VolumeParams, parse_bpb, cluster_to_lba};
use super::fsinfo::{FsInfoState, load_fsinfo, sync_fsinfo};
use super::types::{FsInfoSector, fat_val, FAT32_CLN_SHUT_BIT};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default LRU capacity for the FAT sector cache (number of FAT sectors)
const FAT_CACHE_SECTORS: usize = 128;

/// Default LRU capacity for the cluster data cache (number of clusters)
const CLUSTER_CACHE_SIZE: usize = 64;

// ── FatVolume ─────────────────────────────────────────────────────────────────

/// Runtime state for one mounted FAT32 volume.
pub struct FatVolume {
    /// Block device client (IPC to PORT_BLOCK_SERVICE)
    pub block:      BlockClient,
    /// Immutable parsed parameters from the BPB
    pub params:     VolumeParams,
    /// Cached FAT sectors (dirty-writeback LRU)
    pub fat_cache:  FatCache,
    /// Cached cluster data (dirty-writeback LRU)
    pub data_cache: ClusterCache,
    /// FSInfo in-memory state
    pub fsinfo:     FsInfoState,
    /// Volume is mounted read-only
    pub read_only:  bool,
    /// Volume is dirty (CLN_SHUT bit was clear at mount time)
    pub volume_dirty: bool,
}

impl FatVolume {
    // ── Construction ──────────────────────────────────────────────────────

    /// Mount a FAT32 volume.
    ///
    /// Reads the boot sector, validates the BPB, sets the ClnShut bit to
    /// indicate the volume is mounted (dirty), and loads FSInfo.
    ///
    /// `partition_start`: absolute LBA of sector 0 of this partition.
    pub fn mount(
        mut block: BlockClient,
        partition_start: u64,
        read_only: bool,
    ) -> VfsResult<FatVolume> {
        // Read primary boot sector (sector 0 of partition)
        let mut buf = vec![0u8; 512];
        block.read_bytes(partition_start * 512, &mut buf)
            .map_err(|_| VfsError::Io)?;
        let sector0: &[u8; 512] = buf.as_slice().try_into().map_err(|_| VfsError::Io)?;

        // Parse + validate BPB.  On failure, fall back to the backup boot
        // sector (BPB_BkBootSec, typically sector 6 within the partition).
        // The backup is tried only when the primary is unreadable or fails
        // validation; we never write to the backup here.
        let params = match parse_bpb(sector0, partition_start) {
            Ok(p) => p,
            Err(primary_err) => {
                // Try backup boot sector.  We read it at the well-known
                // offset of 6 sectors from the partition start because the
                // primary BPB failed — we cannot trust BPB_BkBootSec from a
                // corrupt primary.  Sector 6 is the spec-recommended location.
                let backup_lba = partition_start + 6;
                let mut backup_buf = vec![0u8; 512];
                block.read_bytes(backup_lba * 512, &mut backup_buf)
                    .map_err(|_| primary_err)?; // propagate original error if read fails
                let backup_sec: &[u8; 512] = backup_buf
                    .as_slice()
                    .try_into()
                    .map_err(|_| VfsError::Io)?;
                // parse_bpb uses partition_start so LBA arithmetic is correct
                parse_bpb(backup_sec, partition_start)?
            }
        };

        // Check CLN_SHUT bit in FAT[1] entry (bit 27 of cluster 1)
        // We load it directly from block (cache not warm yet)
        let fat1_off = params.fat0_start_lba * 512 + 4; // cluster 1 = FAT[1]
        let mut fat1_buf = [0u8; 4];
        block.read_bytes(fat1_off, &mut fat1_buf)
            .map_err(|_| VfsError::Io)?;
        let fat1_val = u32::from_le_bytes(fat1_buf);
        let was_dirty = (fat1_val & FAT32_CLN_SHUT_BIT) == 0;

        // Build caches
        let fat_cache  = FatCache::new(params.bytes_per_sec as usize, FAT_CACHE_SECTORS);
        let data_cache = ClusterCache::new(CLUSTER_CACHE_SIZE, params.cluster_size as usize);

        // Load FSInfo
        let fsinfo = load_fsinfo(&mut block, params.fsinfo_lba)
            .unwrap_or_else(|_| FsInfoState::unknown(params.count_of_clusters));

        let mut vol = FatVolume {
            block,
            params,
            fat_cache,
            data_cache,
            fsinfo,
            read_only,
            volume_dirty: was_dirty,
        };

        // Set CLN_SHUT = 0 (volume in use) unless read-only
        if !read_only {
            vol.set_cln_shut(false)?;
        }

        Ok(vol)
    }

    // ── FAT entry access ──────────────────────────────────────────────────

    /// Read the FAT value for `cluster`.
    pub fn fat_read(&mut self, cluster: u32) -> VfsResult<u32> {
        let p = &self.params;
        let val = self.fat_cache.read_entry(
            &mut self.block,
            p.fat0_start_lba,
            p.bytes_per_sec,
            cluster,
        ).map_err(|_| VfsError::Io)?;
        Ok(val)
    }

    /// Write the FAT value for `cluster`, updating all FAT copies.
    pub fn fat_write(&mut self, cluster: u32, value: u32) -> VfsResult<()> {
        if self.read_only { return Err(VfsError::ReadOnly); }
        let p = &self.params;
        self.fat_cache.write_entry(
            &mut self.block,
            p.fat0_start_lba,
            p.bytes_per_sec,
            cluster,
            value,
        ).map_err(|_| VfsError::Io)?;
        Ok(())
    }

    // ── Cluster data I/O ──────────────────────────────────────────────────

    /// Read the full contents of `cluster` into `buf`.
    /// `buf` must be exactly `params.cluster_size` bytes.
    pub fn cluster_read(&mut self, cluster: u32, buf: &mut [u8]) -> VfsResult<()> {
        let lba = cluster_to_lba(&self.params, cluster);
        self.data_cache.read_cluster(
            &mut self.block,
            cluster,
            lba,
            self.params.secs_per_clus,
            buf,
        ).map_err(|_| VfsError::Io)
    }

    /// Write `data` into `cluster` at byte `offset_in_cluster`.
    pub fn cluster_write_partial(
        &mut self,
        cluster: u32,
        offset: usize,
        data: &[u8],
    ) -> VfsResult<()> {
        if self.read_only { return Err(VfsError::ReadOnly); }
        let lba = cluster_to_lba(&self.params, cluster);
        self.data_cache.write_cluster_partial(
            &mut self.block,
            cluster,
            lba,
            self.params.secs_per_clus,
            offset,
            data,
        ).map_err(|_| VfsError::Io)
    }

    /// Write a full cluster.
    pub fn cluster_write(&mut self, cluster: u32, data: &[u8]) -> VfsResult<()> {
        if self.read_only { return Err(VfsError::ReadOnly); }
        let lba = cluster_to_lba(&self.params, cluster);
        self.data_cache.write_cluster(
            &mut self.block,
            cluster,
            lba,
            self.params.secs_per_clus,
            data,
        ).map_err(|_| VfsError::Io)
    }

    // ── Flush / sync ──────────────────────────────────────────────────────

    /// Flush all dirty FAT sectors and cluster data to the block device.
    /// Does NOT update FSInfo or the CLN_SHUT bit.
    pub fn flush_caches(&mut self) -> VfsResult<()> {
        let p = &self.params;
        self.fat_cache.flush_dirty(
            &mut self.block,
            p.fat_size_secs,
            p.num_fats,
        ).map_err(|_| VfsError::Io)?;
        self.data_cache.flush_dirty(
            &mut self.block,
            p.data_start_lba,
            p.secs_per_clus,
        ).map_err(|_| VfsError::Io)
    }

    /// Full sync: flush caches, write FSInfo, flush block device.
    pub fn sync(&mut self) -> VfsResult<()> {
        if self.read_only { return Ok(()); }
        self.flush_caches()?;
        let params = &self.params;
        sync_fsinfo(&mut self.block, params.fsinfo_lba, &self.fsinfo)
            .map_err(|_| VfsError::Io)?;
        self.block.flush().map_err(|_| VfsError::Io)
    }

    /// Unmount: set CLN_SHUT = 1 (clean), sync, flush.
    pub fn unmount(&mut self) -> VfsResult<()> {
        if !self.read_only {
            self.flush_caches()?;
            self.set_cln_shut(true)?;
            let params = &self.params;
            sync_fsinfo(&mut self.block, params.fsinfo_lba, &self.fsinfo)
                .map_err(|_| VfsError::Io)?;
            self.block.flush().map_err(|_| VfsError::Io)?;
        }
        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    /// Set or clear FAT32_CLN_SHUT_BIT in FAT[1] (cluster 1 of FAT0).
    ///
    /// Write ordering: FAT_BEFORE_DIR (we update FAT metadata first).
    fn set_cln_shut(&mut self, clean: bool) -> VfsResult<()> {
        let p = &self.params;
        let byte_off = p.fat0_start_lba * 512 + 4; // cluster 1 entry
        let mut buf = [0u8; 4];
        self.block.read_bytes(byte_off, &mut buf).map_err(|_| VfsError::Io)?;
        let mut val = u32::from_le_bytes(buf);
        if clean {
            val |= FAT32_CLN_SHUT_BIT;
        } else {
            val &= !FAT32_CLN_SHUT_BIT;
        }
        buf = val.to_le_bytes();
        self.block.write_bytes(byte_off, &buf).map_err(|_| VfsError::Io)?;
        // Mirror to all FAT copies
        for n in 1..p.num_fats {
            let mirror_off = byte_off + n as u64 * p.fat_size_secs * p.bytes_per_sec as u64;
            let mut mb = [0u8; 4];
            self.block.read_bytes(mirror_off, &mut mb).map_err(|_| VfsError::Io)?;
            let mut mv = u32::from_le_bytes(mb);
            if clean { mv |= FAT32_CLN_SHUT_BIT; } else { mv &= !FAT32_CLN_SHUT_BIT; }
            self.block.write_bytes(mirror_off, &mv.to_le_bytes()).map_err(|_| VfsError::Io)?;
        }
        Ok(())
    }

    // ── FSInfo helpers ────────────────────────────────────────────────────

    /// Update the in-memory FSInfo hint for free-cluster count.
    /// Call this whenever clusters are allocated or freed.
    pub fn fsinfo_update_free(&mut self, delta: i64) {
        self.fsinfo.update_free(delta);
    }

    /// Update the next-free hint.
    pub fn fsinfo_set_next_free(&mut self, cluster: u32) {
        self.fsinfo.set_next_free(cluster);
    }
}
