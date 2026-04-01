// fat32/fat_table.rs — FAT chain traversal and manipulation
//
// All FAT reads/writes go through the FatVolume's FatCache; this module
// provides the chaining-level operations on top of that primitive.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::vec::Vec;

use crate::error::{VfsError, VfsResult};
use super::types::fat_val;
use super::volume::FatVolume;

// ── Chain traversal ────────────────────────────────────────────────────────

/// Return all clusters in the chain starting at `first_cluster`,
/// in order.  Returns `Err` if a FAT read fails or the chain is circular.
pub fn get_chain(vol: &mut FatVolume, first_cluster: u32) -> VfsResult<Vec<u32>> {
    let max = vol.params.count_of_clusters as usize + 2; // sanity limit
    let mut chain = Vec::with_capacity(8);
    let mut cur = first_cluster;

    loop {
        if cur < 2 || cur > vol.params.max_valid_cluster {
            return Err(VfsError::Corrupted);
        }
        chain.push(cur);
        if chain.len() > max {
            // Circular chain detected
            return Err(VfsError::Corrupted);
        }
        let next = vol.fat_read(cur)?;
        if fat_val::is_eoc(next) {
            break;
        }
        if fat_val::is_free(next) || fat_val::is_bad(next) {
            return Err(VfsError::Corrupted);
        }
        cur = next;
    }
    Ok(chain)
}

/// Return the next cluster after `cluster`, or `None` if EOC.
pub fn next_cluster(vol: &mut FatVolume, cluster: u32) -> VfsResult<Option<u32>> {
    let val = vol.fat_read(cluster)?;
    if fat_val::is_eoc(val) {
        Ok(None)
    } else if fat_val::is_data(val) {
        Ok(Some(val))
    } else {
        Err(VfsError::Corrupted)
    }
}

/// Follow the chain from `start` exactly `n` steps (0-indexed),
/// returning the cluster at position `n`.
pub fn cluster_at(vol: &mut FatVolume, start: u32, n: usize) -> VfsResult<u32> {
    let mut cur = start;
    for _ in 0..n {
        match next_cluster(vol, cur)? {
            Some(c) => cur = c,
            None    => return Err(VfsError::InvalidArgument),
        }
    }
    Ok(cur)
}

/// Return the last cluster in the chain starting at `start`.
pub fn last_cluster(vol: &mut FatVolume, start: u32) -> VfsResult<u32> {
    let mut cur = start;
    loop {
        match next_cluster(vol, cur)? {
            Some(c) => cur = c,
            None    => return Ok(cur),
        }
    }
}

/// Count the number of clusters in the chain starting at `start`.
pub fn chain_length(vol: &mut FatVolume, start: u32) -> VfsResult<u32> {
    let mut cur = start;
    let mut len = 0u32;
    let max = vol.params.count_of_clusters + 2;

    loop {
        len += 1;
        if len > max {
            return Err(VfsError::Corrupted);
        }
        match next_cluster(vol, cur)? {
            Some(c) => cur = c,
            None    => return Ok(len),
        }
    }
}

// ── Chain modification ─────────────────────────────────────────────────────

/// Append `cluster` (already allocated, FAT entry = free) to the end of the
/// chain rooted at `start`.  Sets the former last cluster's FAT entry to
/// `cluster` and marks `cluster` as EOC.
///
/// Write ordering: FAT_BEFORE_DIR — caller must flush FAT before updating dir.
pub fn append_to_chain(vol: &mut FatVolume, start: u32, new_cluster: u32) -> VfsResult<()> {
    let tail = last_cluster(vol, start)?;
    // new_cluster → EOC
    vol.fat_write(new_cluster, fat_val::EOC)?;
    // tail → new_cluster  (DATA_BEFORE_META: data written before FAT pointer update)
    vol.fat_write(tail, new_cluster)?;
    Ok(())
}

/// Truncate the chain so that `last_keep` is the new EOC.
/// All cluster entries after `last_keep` are marked as FREE.
///
/// Write ordering: FAT_BEFORE_DIR — caller must flush FAT before updating dir.
pub fn truncate_chain(vol: &mut FatVolume, last_keep: u32) -> VfsResult<()> {
    // Read the next entry BEFORE overwriting last_keep
    let mut cur = vol.fat_read(last_keep)?;
    if fat_val::is_eoc(cur) {
        return Ok(()); // nothing to do
    }
    // Mark last_keep as EOC
    vol.fat_write(last_keep, fat_val::EOC)?;
    // Free everything after it
    loop {
        if !fat_val::is_data(cur) { break; }
        let next = vol.fat_read(cur)?;
        vol.fat_write(cur, fat_val::FREE)?;
        vol.fsinfo_update_free(1);
        if fat_val::is_eoc(next) { break; }
        cur = next;
    }
    Ok(())
}

/// Free an entire chain starting at `start` (sets all entries to FREE).
///
/// Write ordering: FAT_BEFORE_DIR — caller must flush FAT before updating dir.
pub fn free_chain(vol: &mut FatVolume, start: u32) -> VfsResult<()> {
    let mut cur = start;
    let max = vol.params.count_of_clusters + 2;
    let mut count = 0u32;

    loop {
        if cur < 2 || cur > vol.params.max_valid_cluster {
            break;
        }
        count += 1;
        if count > max {
            return Err(VfsError::Corrupted);
        }
        let next = vol.fat_read(cur)?;
        vol.fat_write(cur, fat_val::FREE)?;
        if fat_val::is_eoc(next) { break; }
        cur = next;
    }
    vol.fsinfo_update_free(count as i64);
    Ok(())
}
