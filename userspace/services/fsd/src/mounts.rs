// mounts.rs — Mount-point metadata (pure data, no backend logic)
//
// MountEntry is the metadata record stored in Vfs::mounts.
// All backend routing and lifecycle is handled by Vfs itself.
// This module is intentionally thin.

#![allow(dead_code)]

extern crate alloc;
use alloc::string::String;

use crate::vfs::MountFlags;

// ── MountEntry ───────────────────────────────────────────────────────────────

/// Metadata for a single mount point.
///
/// The Vfs stores one MountEntry per mounted filesystem.  The actual
/// filesystem backend lives in `Vfs::backends` keyed by `mount_id`.
#[derive(Debug, Clone)]
pub struct MountEntry {
    /// Unique ID for this mount (monotonically allocated).
    pub mount_id:    u32,
    /// Absolute, normalised mount-point path (e.g. "/" or "/mnt/usb").
    pub mount_point: String,
    /// Mount option flags.
    pub flags:       MountFlags,
    /// Source device path (e.g. "/dev/sda1" or "(ram)").
    pub dev_path:    String,
}

impl MountEntry {
    pub fn root(mount_id: u32, dev_path: &str, read_only: bool) -> Self {
        MountEntry {
            mount_id,
            mount_point: "/".into(),
            flags: MountFlags { read_only, ..MountFlags::default() },
            dev_path: dev_path.into(),
        }
    }
}
