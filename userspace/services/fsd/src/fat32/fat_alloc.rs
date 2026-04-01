// fat32/alloc.rs — Cluster allocation and de-allocation
//
// Allocation strategy:
//   1. Use FSInfo next_free hint (if ≥ 2 and ≤ max_valid_cluster)
//   2. Linear scan from hint forward, wrap around to cluster 2
//   3. Update FSInfo after every alloc/free
//
// Write ordering: JOURNAL_BEFORE_FAT — journal entry must be written before
// any FAT modification is committed.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;

use crate::error::{VfsError, VfsResult};
use super::fat_table::{append_to_chain, free_chain};
use super::types::fat_val;
use super::volume::FatVolume;

// ── Single cluster allocation ──────────────────────────────────────────────

/// Allocate one free cluster.
///
/// Returns the new cluster number.  The FAT entry is set to EOC.
/// FSInfo hints are updated.
/// Does NOT zero-fill the cluster; caller must do so if needed.
pub fn alloc_one(vol: &mut FatVolume) -> VfsResult<u32> {
    let start = next_free_hint(vol);
    let max   = vol.params.max_valid_cluster;

    // Two-pass: [start..=max] then [2..start)
    for cluster in start..=max {
        if try_claim(vol, cluster)? {
            vol.fsinfo_update_free(-1);
            vol.fsinfo_set_next_free(cluster + 1);
            return Ok(cluster);
        }
    }
    for cluster in 2..start {
        if try_claim(vol, cluster)? {
            vol.fsinfo_update_free(-1);
            vol.fsinfo_set_next_free(cluster + 1);
            return Ok(cluster);
        }
    }
    Err(VfsError::NoSpace)
}

/// Allocate `n` clusters in a contiguous (best-effort) run,
/// returning a Vec of cluster numbers.
///
/// Returned clusters are linked as a chain ending at EOC.
/// If fewer than `n` free clusters exist, returns `ENOSPC`.
pub fn alloc_chain(vol: &mut FatVolume, n: usize) -> VfsResult<Vec<u32>> {
    if n == 0 { return Ok(vec![]); }

    let mut chain = Vec::with_capacity(n);
    let mut prev: Option<u32> = None;

    for _ in 0..n {
        let c = alloc_one(vol)?;
        // Link previous cluster to this one
        if let Some(p) = prev {
            // Overwrite previous EOC with pointer to c
            vol.fat_write(p, c)?;
        }
        chain.push(c);
        prev = Some(c);
    }
    // Last cluster is already EOC (set by alloc_one)
    Ok(chain)
}

/// Extend the chain rooted at `start` by `n` additional clusters.
/// Returns the newly allocated clusters.
pub fn extend_chain(vol: &mut FatVolume, start: u32, n: usize) -> VfsResult<Vec<u32>> {
    if n == 0 { return Ok(vec![]); }
    let new_clusters = alloc_chain(vol, n)?;
    append_to_chain(vol, start, new_clusters[0])?;
    Ok(new_clusters)
}

// ── Free ───────────────────────────────────────────────────────────────────

/// Free the entire chain starting at `start`.
/// FSInfo is updated by `free_chain` via `fat_table`.
pub fn free_cluster_chain(vol: &mut FatVolume, start: u32) -> VfsResult<()> {
    if start < 2 { return Err(VfsError::Corrupted); }
    free_chain(vol, start)
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Atomically check that `cluster` is FREE and mark it as EOC.
/// Returns `true` if claimed, `false` if it was already in use.
fn try_claim(vol: &mut FatVolume, cluster: u32) -> VfsResult<bool> {
    let val = vol.fat_read(cluster)?;
    if !fat_val::is_free(val) { return Ok(false); }
    vol.fat_write(cluster, fat_val::EOC)?;
    Ok(true)
}

/// Return the best starting cluster for the next free-cluster search.
fn next_free_hint(vol: &FatVolume) -> u32 {
    let hint = vol.fsinfo.next_free;
    if hint >= 2 && hint <= vol.params.max_valid_cluster {
        hint
    } else {
        2
    }
}

/// Zero-fill a freshly allocated cluster (required before it holds directory
/// entries, to avoid leaking stale data across unlinks).
pub fn zero_cluster(vol: &mut FatVolume, cluster: u32) -> VfsResult<()> {
    let sz = vol.params.cluster_size as usize;
    let zeros = vec![0u8; sz];
    vol.cluster_write(cluster, &zeros)
}
