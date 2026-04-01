// fat32/fsinfo.rs — FSInfo sector read/write/validate
//
// The FSInfo sector (typically LBA 1 within the partition) holds two hints:
//   - free cluster count
//   - next free cluster
//
// Both are hints only; callers must never trust them without verification.
// 0xFFFFFFFF means "unknown".

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;

use crate::block_client::BlockClient;
use crate::error::{VfsError, VfsResult};

// FSInfo signatures (spec §3.4)
const FSINFO_SIG1: u32 = 0x4161_5252; // "RRaA"
const FSINFO_SIG2: u32 = 0x6141_7272; // "rrAa"
const FSINFO_SIG3: u32 = 0xAA55_0000; // trailing signature at offset 508

const FSINFO_UNKNOWN: u32 = 0xFFFF_FFFF;

// ── In-memory FSInfo state ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FsInfoState {
    /// Hint: number of free clusters, or UNKNOWN
    pub free_count: u32,
    /// Hint: first cluster to search for free space, or UNKNOWN
    pub next_free:  u32,
    /// Whether the hints are considered valid (loaded from disk successfully)
    pub valid:      bool,
    /// Whether the in-memory state differs from what's on disk
    pub dirty:      bool,
}

impl FsInfoState {
    /// Construct an "unknown" state (no hints).
    pub fn unknown(total_clusters: u32) -> Self {
        FsInfoState {
            free_count: FSINFO_UNKNOWN,
            next_free:  FSINFO_UNKNOWN,
            valid:      false,
            dirty:      false,
        }
    }

    /// Adjust free cluster count by `delta` (positive = freed, negative = allocated).
    /// Clamps to u32 range; if currently unknown, stays unknown.
    pub fn update_free(&mut self, delta: i64) {
        if self.free_count == FSINFO_UNKNOWN { return; }
        let new_val = self.free_count as i64 + delta;
        if new_val < 0 {
            self.free_count = 0;
        } else {
            self.free_count = new_val as u32;
        }
        self.dirty = true;
    }

    /// Update the next-free hint.  Silently ignored if value is 0 or 1.
    pub fn set_next_free(&mut self, cluster: u32) {
        if cluster < 2 { return; }
        self.next_free = cluster;
        self.dirty = true;
    }
}

// ── Disk I/O ──────────────────────────────────────────────────────────────────

/// Load FSInfo from `lba`.  Returns `Err` if signatures are invalid.
pub fn load_fsinfo(block: &mut BlockClient, lba: u64) -> VfsResult<FsInfoState> {
    let mut buf = vec![0u8; 512];
    block.read_bytes(lba * 512, &mut buf).map_err(|_| VfsError::Io)?;

    // Validate all three signatures
    let sig1 = u32::from_le_bytes(buf[0..4].try_into().unwrap());
    let sig2 = u32::from_le_bytes(buf[484..488].try_into().unwrap());
    let sig3_raw = [buf[508], buf[509], buf[510], buf[511]];
    let sig3 = u32::from_le_bytes(sig3_raw);

    if sig1 != FSINFO_SIG1 || sig2 != FSINFO_SIG2 {
        return Err(VfsError::Corrupted);
    }
    // Trailing sig is 0x00000000AA55 (little-endian u32 at 508 = 0xAA550000,
    // but spec uses bytes 510=0x55, 511=0xAA)
    if buf[510] != 0x55 || buf[511] != 0xAA {
        return Err(VfsError::Corrupted);
    }

    let free_count = u32::from_le_bytes(buf[488..492].try_into().unwrap());
    let next_free  = u32::from_le_bytes(buf[492..496].try_into().unwrap());

    Ok(FsInfoState {
        free_count,
        next_free,
        valid: true,
        dirty: false,
    })
}

/// Write FSInfo hints to disk at `lba`.
///
/// Write ordering: DIR_BEFORE_FSINFO (caller must have already flushed FAT and dir
/// changes before calling this function).
pub fn sync_fsinfo(block: &mut BlockClient, lba: u64, state: &FsInfoState) -> VfsResult<()> {
    if !state.dirty { return Ok(()); }

    // Read current sector to preserve reserved bytes
    let mut buf = vec![0u8; 512];
    block.read_bytes(lba * 512, &mut buf).map_err(|_| VfsError::Io)?;

    // Write signatures (idempotent)
    buf[0..4].copy_from_slice(&FSINFO_SIG1.to_le_bytes());
    buf[484..488].copy_from_slice(&FSINFO_SIG2.to_le_bytes());
    buf[510] = 0x55;
    buf[511] = 0xAA;

    // Write hints
    buf[488..492].copy_from_slice(&state.free_count.to_le_bytes());
    buf[492..496].copy_from_slice(&state.next_free.to_le_bytes());

    block.write_bytes(lba * 512, &buf).map_err(|_| VfsError::Io)
}
