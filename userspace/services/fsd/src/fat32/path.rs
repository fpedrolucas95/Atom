// fat32/path.rs — Path resolution relative to volume root
//
// Walks a normalized absolute path component-by-component through the
// directory tree, returning the parsed entry for the final component.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::string::{String, ToString};
use alloc_crate::vec::Vec;

use crate::error::{VfsError, VfsResult};
use super::dir_ops::find_entry;
use super::dir_entry::ParsedEntry;
use super::volume::FatVolume;

/// Result of resolving a path.
#[derive(Debug, Clone)]
pub struct ResolvedPath {
    /// The parsed directory entry for the final component.
    pub entry:          ParsedEntry,
    /// First cluster of the *parent* directory.
    pub parent_cluster: u32,
}

/// Resolve `path` from the volume root.
///
/// `path` must be an absolute path with components separated by `/`.
/// Returns `VfsError::NotFound` if any component is missing.
/// Returns `VfsError::NotDirectory` if a non-final component is not a directory.
pub fn resolve(vol: &mut FatVolume, path: &str) -> VfsResult<ResolvedPath> {
    let root = vol.params.root_cluster;
    resolve_from(vol, root, path)
}

/// Resolve `path` starting from `start_cluster`.
pub fn resolve_from(
    vol:           &mut FatVolume,
    start_cluster: u32,
    path:          &str,
) -> VfsResult<ResolvedPath> {
    let components: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();

    if components.is_empty() {
        // Resolving root itself — synthesize a fake entry
        return Ok(ResolvedPath {
            entry: synthetic_root(vol.params.root_cluster),
            parent_cluster: vol.params.root_cluster,
        });
    }

    let mut dir_cluster  = start_cluster;
    let mut parent_cluster = start_cluster;
    let last_idx = components.len() - 1;

    for (i, &component) in components.iter().enumerate() {
        // Handle ".."
        if component == ".." {
            // Read ".." entry from current dir to get parent cluster
            let entry = find_entry(vol, dir_cluster, "..")?
                .ok_or(VfsError::Corrupted)?;
            let parent = if entry.first_cluster == 0 {
                vol.params.root_cluster
            } else {
                entry.first_cluster
            };
            parent_cluster = parent;
            dir_cluster    = parent;
            continue;
        }

        let entry = find_entry(vol, dir_cluster, component)?
            .ok_or(VfsError::NotFound)?;

        if i == last_idx {
            return Ok(ResolvedPath { entry, parent_cluster: dir_cluster });
        }

        // Not the last component → must be a directory
        if entry.attributes & super::types::attr::DIRECTORY == 0 {
            return Err(VfsError::NotDir);
        }

        parent_cluster = dir_cluster;
        dir_cluster = if entry.first_cluster == 0 {
            vol.params.root_cluster
        } else {
            entry.first_cluster
        };
    }

    Err(VfsError::NotFound) // unreachable but satisfies compiler
}

/// Resolve the parent directory of `path`, returning its first cluster and
/// the final component name.
pub fn resolve_parent(
    vol:  &mut FatVolume,
    path: &str,
) -> VfsResult<(u32, String)> {
    let trimmed = path.trim_end_matches('/');
    let slash = trimmed.rfind('/').unwrap_or(0);
    let parent_path = if slash == 0 { "/" } else { &trimmed[..slash] };
    let name = &trimmed[slash + 1..];

    if name.is_empty() { return Err(VfsError::InvalidArgument); }

    let parent_cluster = if parent_path == "/" {
        vol.params.root_cluster
    } else {
        let r = resolve(vol, parent_path)?;
        if r.entry.attributes & super::types::attr::DIRECTORY == 0 {
            return Err(VfsError::NotDir);
        }
        r.entry.first_cluster
    };

    Ok((parent_cluster, name.to_string()))
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn synthetic_root(root_cluster: u32) -> ParsedEntry {
    ParsedEntry {
        name:          "/".to_string(),
        first_cluster: root_cluster,
        file_size:     0,
        attributes:    super::types::attr::DIRECTORY,
        slot_start:    0,
        sfn_slot:      0,
        raw_sfn:       *b"ROOT       ",
        ctime_ns:      0,
        mtime_ns:      0,
        atime_ns:      0,
    }
}
