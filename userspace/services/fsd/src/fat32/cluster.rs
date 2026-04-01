// fat32/cluster.rs — Cluster-chain I/O helpers
//
// This module provides byte-level read and write over a FAT32 cluster chain,
// handling chain traversal, partial first/last clusters, and cross-cluster
// operations.  All data I/O goes through FatVolume's ClusterCache.

#![allow(dead_code)]

use crate::error::{VfsError, VfsResult};
extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;
use super::fat_table::{get_chain, cluster_at};
use super::fat_alloc::extend_chain;
use super::volume::FatVolume;

// ── Reading ────────────────────────────────────────────────────────────────

/// Read `len` bytes from position `offset` in the file whose cluster chain
/// starts at `first_cluster`.  Returns the actual bytes read (may be less
/// than `len` if the end of the chain is reached).
///
/// `file_size` is used to clamp the read to the allocated file data.
pub fn read_bytes(
    vol:           &mut FatVolume,
    first_cluster: u32,
    file_size:     u64,
    offset:        u64,
    buf:           &mut [u8],
) -> VfsResult<usize> {
    let len = buf.len();
    if len == 0 || offset >= file_size { return Ok(0); }

    let clus_size = vol.params.cluster_size as u64;
    let clamped   = len.min((file_size - offset) as usize);
    let mut done  = 0usize;
    let mut pos   = offset;

    while done < clamped {
        // Which cluster holds byte `pos`?
        let clus_idx = (pos / clus_size) as usize;
        let clus_off = (pos % clus_size) as usize;
        let cluster  = cluster_at(vol, first_cluster, clus_idx)?;

        // How many bytes we can read from this cluster
        let avail  = (clus_size as usize) - clus_off;
        let to_read = avail.min(clamped - done);

        // Read the cluster into a temporary buffer
        let mut tmp = vec![0u8; clus_size as usize];
        vol.cluster_read(cluster, &mut tmp)?;

        buf[done..done + to_read].copy_from_slice(&tmp[clus_off..clus_off + to_read]);

        done += to_read;
        pos  += to_read as u64;
    }

    Ok(done)
}

// ── Writing ────────────────────────────────────────────────────────────────

/// Write `data` at byte `offset` in the file whose cluster chain starts at
/// `first_cluster`.  The chain is extended as needed.
///
/// Write ordering: DATA_BEFORE_META — data clusters are written before any
/// directory entry (file size / first cluster) is updated by the caller.
///
/// Returns the new file size after the write.
pub fn write_bytes(
    vol:           &mut FatVolume,
    first_cluster: u32,
    current_size:  u64,
    offset:        u64,
    data:          &[u8],
) -> VfsResult<u64> {
    if data.is_empty() { return Ok(current_size); }
    if vol.read_only { return Err(VfsError::ReadOnly); }

    let clus_size  = vol.params.cluster_size as u64;
    let end_offset = offset + data.len() as u64;

    // Ensure the chain is long enough
    ensure_chain_length(vol, first_cluster, current_size, end_offset)?;

    let mut done = 0usize;
    let mut pos  = offset;

    while done < data.len() {
        let clus_idx = (pos / clus_size) as usize;
        let clus_off = (pos % clus_size) as usize;
        let cluster  = cluster_at(vol, first_cluster, clus_idx)?;

        let avail   = (clus_size as usize) - clus_off;
        let to_write = avail.min(data.len() - done);

        vol.cluster_write_partial(cluster, clus_off, &data[done..done + to_write])?;

        done += to_write;
        pos  += to_write as u64;
    }

    Ok(end_offset.max(current_size))
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Ensure the cluster chain starting at `first_cluster` is long enough to
/// hold `needed_end` bytes.  Allocates new clusters if necessary.
///
/// `current_size` is the current allocation (used to compute existing chain length).
fn ensure_chain_length(
    vol:           &mut FatVolume,
    first_cluster: u32,
    current_size:  u64,
    needed_end:    u64,
) -> VfsResult<()> {
    let clus_size   = vol.params.cluster_size as u64;
    let cur_clusters = clusters_for_size(current_size, clus_size).max(1); // at least 1 (first_cluster)
    let need_clusters = clusters_for_size(needed_end, clus_size).max(1);

    if need_clusters <= cur_clusters { return Ok(()); }

    let extra = (need_clusters - cur_clusters) as usize;
    extend_chain(vol, first_cluster, extra)?;
    Ok(())
}

/// Compute how many clusters are required to hold `size` bytes
/// (rounds up, minimum 1 for size > 0).
#[inline]
fn clusters_for_size(size: u64, cluster_size: u64) -> u64 {
    if size == 0 { 0 } else { (size + cluster_size - 1) / cluster_size }
}
