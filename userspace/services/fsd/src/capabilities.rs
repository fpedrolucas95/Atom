// capabilities.rs — Capability enforcement for fsd
//
// Implements the two-level validation strategy from the architecture plan:
//
//   Level 0: PID ownership check (O(1), no syscall)
//   Level 1: CapCache with TTL — amortises SYS_CAP_CHECK to at most one
//            syscall per revalidation window (default 500 ms).
//   Level 2: CapRevoked push notification from kernel, stored in CapCache.
//            Propagated transitively via CapDeriveTree.
//
// SYS_CAP_CHECK  = syscall 9   (existing)
// SYS_CAP_DERIVE = syscall 11  (existing)
// SYS_CAP_REVOKE = syscall 10  (existing — sent BY kernel TO fsd as message)
//
// There is NO SYS_CAP_GENERATION syscall yet (planned for Revision 2).
// Until it is added, the generation field is always 0 and only TTL-based
// re-validation is used.

#![allow(dead_code)]

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use atom_abi::{ESUCCESS, EACCES};
use atom_syscall::raw::{syscall2, syscall3, numbers::*};

use crate::error::{VfsError, VfsResult};

// ── Revalidation window ───────────────────────────────────────────────────────

/// Number of "ticks" a capability check result is considered fresh.
/// fsd calls atom_syscall::thread::sleep() between messages and uses a
/// simple tick counter — 1 tick ≈ 1 IPC round-trip.
/// 500 ticks should correspond to roughly 500 ms under normal load.
pub const CAP_CACHE_TTL_TICKS: u64 = 500;

// ── CapPermissions ────────────────────────────────────────────────────────────

/// Permission bits — mirrors kernel CapPermissions bitflags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapPermissions(pub u32);

impl CapPermissions {
    pub const READ:    CapPermissions = CapPermissions(0x01);
    pub const WRITE:   CapPermissions = CapPermissions(0x02);
    pub const EXECUTE: CapPermissions = CapPermissions(0x04);
    pub const GRANT:   CapPermissions = CapPermissions(0x08);
    pub const ALL:     CapPermissions = CapPermissions(0x0F);

    pub fn contains(self, other: CapPermissions) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for CapPermissions {
    type Output = CapPermissions;
    fn bitor(self, rhs: CapPermissions) -> CapPermissions {
        CapPermissions(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for CapPermissions {
    type Output = CapPermissions;
    fn bitand(self, rhs: CapPermissions) -> CapPermissions {
        CapPermissions(self.0 & rhs.0)
    }
}

// ── CapCacheEntry ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct CapCacheEntry {
    /// Permissions recorded at last full validation.
    permissions:  CapPermissions,
    /// PID that owns this capability handle.
    owner_pid:    u32,
    /// Generation counter from SYS_CAP_GENERATION (0 until kernel supports it).
    generation:   u32,
    /// Tick counter at which the entry was last (re)validated.
    validated_at: u64,
    /// Set to true when a CapRevoked notification arrives for this handle
    /// or any ancestor.  Fast-path rejection without syscall.
    revoked:      bool,
}

// ── CapDeriveTree ─────────────────────────────────────────────────────────────

/// Local record of capability derivation relationships.
///
/// When a CapRevoked notification arrives for a parent handle, we walk
/// children[] recursively to mark all descendants revoked in the cache.
///
/// The tree is shallow in practise: root-volume → dir-handle → file-handle.
struct CapDeriveTree {
    /// parent_handle → list of child_handles
    children: BTreeMap<u64, Vec<u64>>,
}

impl CapDeriveTree {
    fn new() -> Self {
        CapDeriveTree { children: BTreeMap::new() }
    }

    /// Record that `child` was derived from `parent`.
    fn record(&mut self, parent: u64, child: u64) {
        self.children.entry(parent).or_default().push(child);
    }

    /// Collect `root` and all descendants recursively.
    fn descendants(&self, root: u64) -> Vec<u64> {
        let mut out = Vec::new();
        self.collect(root, &mut out);
        out
    }

    fn collect(&self, node: u64, out: &mut Vec<u64>) {
        out.push(node);
        if let Some(kids) = self.children.get(&node) {
            for &kid in kids {
                self.collect(kid, out);
            }
        }
    }

    /// Remove all child records for `handle` (after cleanup).
    fn remove(&mut self, handle: u64) {
        self.children.remove(&handle);
        for kids in self.children.values_mut() {
            kids.retain(|&k| k != handle);
        }
    }
}

// ── CapabilityEnforcer ────────────────────────────────────────────────────────

/// Owns the CapCache and CapDeriveTree.
/// One instance lives inside the IPC server (FsdIpcHandler).
pub struct CapabilityEnforcer {
    cache:       BTreeMap<u64, CapCacheEntry>,
    derive_tree: CapDeriveTree,
    /// Monotonic tick counter — incremented by the IPC dispatch loop.
    tick:        u64,
}

impl CapabilityEnforcer {
    pub fn new() -> Self {
        CapabilityEnforcer {
            cache:       BTreeMap::new(),
            derive_tree: CapDeriveTree::new(),
            tick:        0,
        }
    }

    /// Advance the tick counter.  Called once per IPC message processed.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    // ── Core check ─────────────────────────────────────────────────────────

    /// Check that `cap_handle` grants `required` permissions to `pid`.
    ///
    /// Fast path (O(1)):
    ///   - Entry exists AND not revoked AND TTL not expired → return cached result.
    ///
    /// Slow path (1 syscall):
    ///   - Entry absent OR TTL expired → call SYS_CAP_CHECK, update cache.
    ///
    /// Revoked path (O(1) no syscall):
    ///   - Entry exists AND revoked → EACCES immediately.
    pub fn check(
        &mut self,
        cap_handle: u64,
        pid: u32,
        required: CapPermissions,
    ) -> VfsResult<()> {
        // Level 0: revoked fast-path
        if let Some(entry) = self.cache.get(&cap_handle) {
            if entry.revoked {
                return Err(VfsError::PermissionDenied);
            }

            // Level 1a: TTL not expired — use cache
            if self.tick.wrapping_sub(entry.validated_at) < CAP_CACHE_TTL_TICKS {
                if entry.owner_pid == pid && entry.permissions.contains(required) {
                    return Ok(());
                } else {
                    return Err(VfsError::PermissionDenied);
                }
            }
        }

        // Level 1b: Full revalidation via SYS_CAP_CHECK
        self.revalidate(cap_handle, pid, required)
    }

    /// Always perform a full SYS_CAP_CHECK regardless of cache state.
    /// Used for FsOpen, FsMkdir, FsUnlink, FsRename.
    pub fn check_full(
        &mut self,
        cap_handle: u64,
        pid: u32,
        required: CapPermissions,
    ) -> VfsResult<()> {
        self.revalidate(cap_handle, pid, required)
    }

    // ── Derivation ─────────────────────────────────────────────────────────

    /// Derive a new capability from `parent_handle` with `perms`.
    ///
    /// Calls SYS_CAP_DERIVE and records the parent→child relationship in the
    /// derive tree.  Returns the new child handle.
    pub fn derive(
        &mut self,
        parent_handle: u64,
        perms: CapPermissions,
        owner_pid: u32,
    ) -> VfsResult<u64> {
        // SYS_CAP_DERIVE(parent_handle, perms_u64) → child_handle | error
        let child = unsafe {
            syscall2(SYS_CAP_DERIVE, parent_handle, perms.0 as u64)
        };

        if atom_abi::is_syscall_error(child) {
            return Err(VfsError::PermissionDenied);
        }

        // Seed the cache for the child
        self.cache.insert(child, CapCacheEntry {
            permissions:  perms,
            owner_pid,
            generation:   0,
            validated_at: self.tick,
            revoked:      false,
        });

        self.derive_tree.record(parent_handle, child);
        Ok(child)
    }

    // ── Revocation notification ────────────────────────────────────────────

    /// Called when a CapRevoked IPC message arrives from the kernel.
    ///
    /// Marks `cap_handle` and all descendants as revoked in the local cache.
    /// The VFS::revoke_cap() call happens separately (VFS is notified by the
    /// IPC server, which calls both this method and Vfs::revoke_cap()).
    pub fn handle_revoked(&mut self, cap_handle: u64) {
        let affected = self.derive_tree.descendants(cap_handle);
        for handle in affected {
            self.cache.entry(handle)
                .or_insert_with(|| CapCacheEntry {
                    permissions:  CapPermissions(0),
                    owner_pid:    0,
                    generation:   0,
                    validated_at: 0,
                    revoked:      false,
                })
                .revoked = true;
        }
        self.derive_tree.remove(cap_handle);
    }

    /// Forget a cap entirely (after explicit close/cleanup).
    pub fn forget(&mut self, cap_handle: u64) {
        self.cache.remove(&cap_handle);
        self.derive_tree.remove(cap_handle);
    }

    // ── Private ────────────────────────────────────────────────────────────

    fn revalidate(
        &mut self,
        cap_handle: u64,
        pid: u32,
        required: CapPermissions,
    ) -> VfsResult<()> {
        // SYS_CAP_CHECK(handle, required_perms) → ESUCCESS or EACCES
        let result = unsafe {
            syscall3(SYS_CAP_CHECK, cap_handle, pid as u64, required.0 as u64)
        };

        if result != ESUCCESS {
            // Mark as revoked if kernel says no — may be transient (EACCES from
            // insufficient perms) or permanent (handle dead).  We conservatively
            // mark revoked only if the handle is confirmed dead.
            // For now treat any failure as permission denied without caching.
            return Err(VfsError::PermissionDenied);
        }

        // Update / insert cache entry
        self.cache.insert(cap_handle, CapCacheEntry {
            permissions:  required, // minimum confirmed; actual may be higher
            owner_pid:    pid,
            generation:   0,        // SYS_CAP_GENERATION not yet in kernel
            validated_at: self.tick,
            revoked:      false,
        });

        Ok(())
    }
}
