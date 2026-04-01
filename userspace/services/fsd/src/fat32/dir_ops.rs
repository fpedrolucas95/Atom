// fat32/dir_ops.rs — Directory-level operations
//
// Read, search, insert, and delete entries within FAT32 directories.
// All directory data is accessed through the ClusterCache via FatVolume.

#![allow(dead_code)]

use crate::error::{VfsError, VfsResult};
extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;
use alloc_crate::string::String;
use super::fat_alloc::zero_cluster;
use super::cluster::{read_bytes, write_bytes};
use super::dir_entry::{parse_dir_entries, encode_dir_entries, encode_sfn, ParsedEntry};
use super::fat_table::get_chain;
use super::types::attr;
use super::volume::FatVolume;

const ENTRY_SZ: usize = 32;
const FREE_MARKER:   u8 = 0xE5;
const EOD_MARKER:    u8 = 0x00;

// ── Read entire directory ─────────────────────────────────────────────────

/// Read all clusters of a directory into a single flat Vec<u8>.
pub fn read_dir_raw(vol: &mut FatVolume, first_cluster: u32) -> VfsResult<Vec<u8>> {
    let chain = get_chain(vol, first_cluster)?;
    let clus_sz = vol.params.cluster_size as usize;
    let mut buf = vec![0u8; chain.len() * clus_sz];
    for (i, &c) in chain.iter().enumerate() {
        vol.cluster_read(c, &mut buf[i * clus_sz..(i + 1) * clus_sz])?;
    }
    Ok(buf)
}

/// Parse a directory into a Vec of decoded entries.
pub fn list_dir(vol: &mut FatVolume, first_cluster: u32) -> VfsResult<Vec<ParsedEntry>> {
    let raw = read_dir_raw(vol, first_cluster)?;
    Ok(parse_dir_entries(&raw))
}

// ── Find ──────────────────────────────────────────────────────────────────

/// Find a directory entry by name (case-insensitive).
/// Returns `None` if not found.
pub fn find_entry(
    vol:           &mut FatVolume,
    dir_cluster:   u32,
    name:          &str,
) -> VfsResult<Option<ParsedEntry>> {
    let entries = list_dir(vol, dir_cluster)?;
    let target = name.to_lowercase();
    Ok(entries.into_iter().find(|e| e.name.to_lowercase() == target))
}

// ── Insert ────────────────────────────────────────────────────────────────

/// Insert a new directory entry (LFN + SFN) into `dir_cluster`.
///
/// Finds a contiguous run of at least `n_slots` free/deleted slots, or
/// appends a new cluster if none found.
///
/// Write ordering: DATA_BEFORE_META — zero-fills any new cluster before
/// writing entries; caller must flush FAT before updating parent dir.
pub fn insert_entry(
    vol:           &mut FatVolume,
    dir_cluster:   u32,
    long_name:     &str,
    file_attr:     u8,
    first_cluster: u32,
    file_size:     u32,
    ctime_ns:      u64,
    mtime_ns:      u64,
) -> VfsResult<()> {
    let sfn = encode_sfn(long_name)?;
    let encoded = encode_dir_entries(long_name, &sfn, file_attr, first_cluster, file_size, ctime_ns, mtime_ns);
    let n_slots = encoded.len() / ENTRY_SZ;

    let clus_sz   = vol.params.cluster_size as usize;
    let mut raw   = read_dir_raw(vol, dir_cluster)?;
    let total_slots = raw.len() / ENTRY_SZ;

    // Find a contiguous free run of n_slots
    if let Some(start_slot) = find_free_run(&raw, n_slots, total_slots) {
        raw[start_slot * ENTRY_SZ..start_slot * ENTRY_SZ + encoded.len()]
            .copy_from_slice(&encoded);
        write_dir_raw(vol, dir_cluster, &raw)?;
        return Ok(());
    }

    // No run found — extend the directory by one cluster
    let chain = get_chain(vol, dir_cluster)?;
    let last_c = *chain.last().unwrap();
    let new_c  = super::fat_alloc::alloc_one(vol)?;
    zero_cluster(vol, new_c)?;                       // DATA_BEFORE_META
    super::fat_table::append_to_chain(vol, last_c, new_c)?;

    // Reload raw with new cluster
    let mut raw2 = read_dir_raw(vol, dir_cluster)?;
    let new_start = (raw2.len() / ENTRY_SZ - clus_sz / ENTRY_SZ); // first slot of new cluster
    raw2[new_start * ENTRY_SZ..new_start * ENTRY_SZ + encoded.len()]
        .copy_from_slice(&encoded);
    write_dir_raw(vol, dir_cluster, &raw2)
}

// ── Delete ────────────────────────────────────────────────────────────────

/// Mark a directory entry (and its LFN slots) as deleted.
///
/// Locates the entry by `name`, then sets byte[0] = 0xE5 for every slot
/// from `slot_start` through `sfn_slot`.
///
/// Write ordering: FAT_BEFORE_DIR — caller must free the data chain and
/// flush FAT *before* calling this, so that the directory never points to
/// freed clusters.
pub fn delete_entry(
    vol:         &mut FatVolume,
    dir_cluster: u32,
    name:        &str,
) -> VfsResult<()> {
    let entry = find_entry(vol, dir_cluster, name)?
        .ok_or(VfsError::NotFound)?;

    let mut raw = read_dir_raw(vol, dir_cluster)?;
    for slot in entry.slot_start..=entry.sfn_slot {
        raw[slot * ENTRY_SZ] = FREE_MARKER;
    }
    write_dir_raw(vol, dir_cluster, &raw)
}

// ── Update SFN metadata ───────────────────────────────────────────────────

/// Update the first_cluster and file_size fields in the SFN slot for `name`.
pub fn update_entry_metadata(
    vol:           &mut FatVolume,
    dir_cluster:   u32,
    name:          &str,
    first_cluster: u32,
    file_size:     u32,
    mtime_ns:      u64,
) -> VfsResult<()> {
    let entry = find_entry(vol, dir_cluster, name)?
        .ok_or(VfsError::NotFound)?;

    let mut raw = read_dir_raw(vol, dir_cluster)?;
    let off = entry.sfn_slot * ENTRY_SZ;

    // first_cluster hi (offset 20-21)
    let chi = ((first_cluster >> 16) & 0xFFFF) as u16;
    let clo = (first_cluster & 0xFFFF) as u16;
    raw[off + 20] = chi as u8;
    raw[off + 21] = (chi >> 8) as u8;
    // first_cluster lo (offset 26-27)
    raw[off + 26] = clo as u8;
    raw[off + 27] = (clo >> 8) as u8;
    // file size (offset 28-31)
    raw[off + 28..off + 32].copy_from_slice(&file_size.to_le_bytes());
    // mtime is not updated here (caller may pass it via a separate field later)

    write_dir_raw(vol, dir_cluster, &raw)
}

/// Update (only) the file size in the SFN slot.
pub fn update_file_size(
    vol:         &mut FatVolume,
    dir_cluster: u32,
    name:        &str,
    file_size:   u32,
) -> VfsResult<()> {
    let entry = find_entry(vol, dir_cluster, name)?
        .ok_or(VfsError::NotFound)?;

    let mut raw = read_dir_raw(vol, dir_cluster)?;
    let off = entry.sfn_slot * ENTRY_SZ;
    raw[off + 28..off + 32].copy_from_slice(&file_size.to_le_bytes());
    write_dir_raw(vol, dir_cluster, &raw)
}

// ── Dot/dotdot initialization ──────────────────────────────────────────────

/// Write the mandatory "." and ".." entries into a freshly allocated
/// directory cluster.
///
/// `dir_cluster`: the cluster of the new directory
/// `parent_cluster`: the first cluster of the parent directory (0 for root)
pub fn init_dot_entries(
    vol:            &mut FatVolume,
    dir_cluster:    u32,
    parent_cluster: u32,
    ctime_ns:       u64,
) -> VfsResult<()> {
    let mut buf = vec![0u8; vol.params.cluster_size as usize];

    // "." entry
    let dot_sfn = *b".          ";
    let dot_slot = build_dot_slot(&dot_sfn, attr::DIRECTORY, dir_cluster, ctime_ns);
    buf[..32].copy_from_slice(&dot_slot);

    // ".." entry
    let dotdot_sfn = *b"..         ";
    let dotdot_slot = build_dot_slot(&dotdot_sfn, attr::DIRECTORY, parent_cluster, ctime_ns);
    buf[32..64].copy_from_slice(&dotdot_slot);

    vol.cluster_write(dir_cluster, &buf)
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn build_dot_slot(sfn: &[u8; 11], attr_byte: u8, cluster: u32, ctime_ns: u64) -> [u8; 32] {
    use super::types::{encode_date, encode_time};
    let mut s = [0u8; 32];
    s[..11].copy_from_slice(sfn);
    s[11] = attr_byte;
    let chi = ((cluster >> 16) & 0xFFFF) as u16;
    let clo = (cluster & 0xFFFF) as u16;
    s[20] = chi as u8;
    s[21] = (chi >> 8) as u8;
    // ctime: simplified — store epoch
    s[14] = 0; s[15] = 0;
    s[16..18].copy_from_slice(&encode_date(1980, 1, 1).to_le_bytes());
    s[18..20].copy_from_slice(&encode_date(1980, 1, 1).to_le_bytes());
    s[22..24].copy_from_slice(&encode_time(0, 0, 0).to_le_bytes());
    s[24..26].copy_from_slice(&encode_date(1980, 1, 1).to_le_bytes());
    s[26] = clo as u8;
    s[27] = (clo >> 8) as u8;
    s
}

fn find_free_run(raw: &[u8], n: usize, total: usize) -> Option<usize> {
    let mut run_start = 0;
    let mut run_len   = 0;

    for i in 0..total {
        let first_byte = raw[i * ENTRY_SZ];
        if first_byte == FREE_MARKER || first_byte == EOD_MARKER {
            if run_len == 0 { run_start = i; }
            run_len += 1;
            if run_len >= n { return Some(run_start); }
        } else {
            run_len = 0;
        }
    }
    None
}

/// Write the flat raw buffer back to the directory cluster chain.
fn write_dir_raw(vol: &mut FatVolume, first_cluster: u32, raw: &[u8]) -> VfsResult<()> {
    let clus_sz = vol.params.cluster_size as usize;
    let chain = get_chain(vol, first_cluster)?;
    for (i, &c) in chain.iter().enumerate() {
        let off = i * clus_sz;
        if off >= raw.len() { break; }
        let end = (off + clus_sz).min(raw.len());
        vol.cluster_write_partial(c, 0, &raw[off..end])?;
    }
    Ok(())
}
