// fat32_ops.rs — Journal operation wrappers for FAT32
//
// The actual FAT32 I/O is now handled by fat32::FatFs which implements the
// FileSystem trait.  This module keeps the journal coordinator alive so
// that the journal module remains compilable; no new logic is added here.

#![allow(dead_code)]

use crate::journal::JournalManager;
use crate::error::{VfsError, VfsResult};

/// Thin wrapper that records a generic data-write into the WAL before the
/// actual write occurs.  Called by FatFs when journaling mode is enabled.
///
/// The caller (FatFs) is responsible for applying the actual write; this
/// function only records the intent.
pub fn journal_write_data(
    jm:      &mut JournalManager,
    cluster: u32,
    offset:  u32,
    data:    &[u8],
) -> VfsResult<()> {
    use crate::journal::{OpType, WriteDataOp};

    let tx = jm.begin_transaction();

    let hash = compute_hash(data);
    let op = WriteDataOp { cluster, offset, length: data.len() as u32, hash };
    let payload = op.encode();
    if !jm.add_operation(OpType::WriteData, cluster, &payload) {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    if !jm.commit_transaction() {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    Ok(())
}

/// Record a cluster allocation in the WAL.
pub fn journal_alloc_cluster(jm: &mut JournalManager, cluster: u32) -> VfsResult<()> {
    use crate::journal::{OpType, AllocBlockOp};

    let tx = jm.begin_transaction();
    let op  = AllocBlockOp { cluster, count: 1 };
    let payload = op.encode();
    if !jm.add_operation(OpType::AllocBlock, cluster, &payload) {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    if !jm.commit_transaction() {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    Ok(())
}

/// Record a cluster free in the WAL.
pub fn journal_free_cluster(jm: &mut JournalManager, cluster: u32) -> VfsResult<()> {
    use crate::journal::{OpType, AllocBlockOp};

    let tx = jm.begin_transaction();
    let op  = AllocBlockOp { cluster, count: 1 };
    let payload = op.encode();
    if !jm.add_operation(OpType::FreeBlock, cluster, &payload) {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    if !jm.commit_transaction() {
        jm.rollback_transaction();
        return Err(VfsError::Io);
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute a simple 32-byte fingerprint of `data` for the WAL record.
/// (Not a cryptographic hash — only for replay ordering verification.)
fn compute_hash(data: &[u8]) -> [u8; 32] {
    let mut h = [0u8; 32];
    for (i, &b) in data.iter().enumerate() {
        h[i % 32] ^= b.wrapping_add(i as u8);
    }
    h
}

