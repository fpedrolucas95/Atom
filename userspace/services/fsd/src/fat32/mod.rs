// fat32/mod.rs — FAT32 FileSystem implementation
//
// FatFs wraps a FatVolume and exposes the FileSystem trait to the VFS layer.
// Open file handles are encoded as BackendHandle(u64) where:
//   bits 63..32 = first_cluster of the file's parent dir entry cluster
//   bits 31..0  = slot index (SFN slot)  — uniquely identifies the dir entry
// For directories the same encoding is used with a FLAG_DIR bit.

#![allow(dead_code)]

pub mod fat_alloc;
pub mod bpb;
pub mod cluster;
pub mod dir_entry;
pub mod dir_ops;
pub mod fat_table;
pub mod fsinfo;
pub mod path;
pub mod types;
pub mod volume;

extern crate alloc as alloc_crate;
use alloc_crate::collections::BTreeMap;
use alloc_crate::string::{String, ToString};
use alloc_crate::vec::Vec;
use alloc_crate::format;

use crate::block_client::BlockClient;
use crate::error::{VfsError, VfsResult};
use crate::vfs::{
    BackendHandle, DirEntry as VfsDirEntry, FileSystem, InodeStat, MountFlags,
    OpenFlags, StatVfs, Whence,
};

use self::fat_alloc::{alloc_one, free_cluster_chain, zero_cluster};
use self::cluster::{read_bytes as cluster_read_bytes, write_bytes as cluster_write_bytes};
use self::dir_entry::{encode_dir_entries, encode_sfn, ParsedEntry};
use self::dir_ops::{
    delete_entry, find_entry, init_dot_entries, insert_entry, list_dir,
    update_entry_metadata, update_file_size,
};
use self::fat_table::free_chain;
use self::path::{resolve, resolve_parent};
use self::types::{attr, fat_val};
use self::volume::FatVolume;

// ── Mode constants (POSIX subset) ─────────────────────────────────────────────

const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;
const S_IRWXU: u32 = 0o000_700;
const S_IRWXG: u32 = 0o000_070;
const S_IRWXO: u32 = 0o000_007;
const S_IFMT:  u32 = 0o170_000;

// ── Handle encoding ───────────────────────────────────────────────────────────

const FLAG_DIR: u64 = 1 << 63;

fn encode_handle(dir_cluster: u32, sfn_slot: u32, is_dir: bool) -> BackendHandle {
    let mut v = ((dir_cluster as u64) << 32) | sfn_slot as u64;
    if is_dir { v |= FLAG_DIR; }
    BackendHandle(v)
}

fn decode_handle(h: BackendHandle) -> (u32, u32, bool) {
    let v       = h.0;
    let is_dir  = v & FLAG_DIR != 0;
    let raw     = v & !FLAG_DIR;
    let dir_cl  = (raw >> 32) as u32;
    let slot    = (raw & 0xFFFF_FFFF) as u32;
    (dir_cl, slot, is_dir)
}

// ── Open-file metadata cache ──────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct OpenInfo {
    /// Parsed entry snapshot (stat, size, first_cluster, …)
    entry:          ParsedEntry,
    /// Parent directory first cluster (needed for metadata writeback)
    parent_cluster: u32,
    /// True = opened for writing (we'll flush on close/fsync)
    dirty:          bool,
}

// ── FatFs ─────────────────────────────────────────────────────────────────────

pub struct FatFs {
    vol:       FatVolume,
    mount_id:  u32,
    /// Cache of recently opened file/dir metadata keyed by BackendHandle
    open_info: BTreeMap<u64, OpenInfo>,
}

impl FatFs {
    /// Mount a FAT32 filesystem from a block device.
    pub fn mount(
        block:           BlockClient,
        partition_start: u64,
        read_only:       bool,
        mount_id:        u32,
    ) -> VfsResult<FatFs> {
        let vol = FatVolume::mount(block, partition_start, read_only)?;
        Ok(FatFs { vol, mount_id, open_info: BTreeMap::new() })
    }

    /// Returns a reference to the parsed BPB parameters.
    /// Used by main() to log volume info at startup (Phase 1 checkpoint).
    pub fn volume_params(&self) -> &self::bpb::VolumeParams {
        &self.vol.params
    }

    /// Returns true if the volume was dirty (not cleanly unmounted) at mount time.
    pub fn volume_was_dirty(&self) -> bool {
        self.vol.volume_dirty
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn resolve_abs<'a>(&mut self, rel_path: &'a str) -> VfsResult<self::path::ResolvedPath> {
        let full = format!("/{}", rel_path.trim_start_matches('/'));
        resolve(&mut self.vol, &full)
    }

    fn entry_to_stat(&self, entry: &ParsedEntry) -> InodeStat {
        let is_dir = entry.attributes & attr::DIRECTORY != 0;
        let mode  = if is_dir { S_IFDIR | S_IRWXU | S_IRWXG | S_IRWXO }
                    else       { S_IFREG | 0o644 };
        let ro_mask = if self.vol.read_only || entry.attributes & attr::READ_ONLY != 0 {
            0o444  // mask off write bits
        } else { !0u32 };

        InodeStat {
            inode:    entry.first_cluster as u64,
            size:     entry.file_size as u64,
            mode:     mode & ro_mask,
            mtime_ns: entry.mtime_ns,
            atime_ns: entry.atime_ns,
            ctime_ns: entry.ctime_ns,
            nlinks:   1,
            ..Default::default()
        }
    }

    // ── Write-back for open files ─────────────────────────────────────────

    fn flush_open_file(&mut self, h: BackendHandle) -> VfsResult<()> {
        let Some(info) = self.open_info.get(&h.0).cloned() else { return Ok(()); };
        if !info.dirty { return Ok(()); }

        // DATA_BEFORE_META: flush cluster cache before updating dir entry
        self.vol.flush_caches()?;

        // Update dir entry size + cluster (if new file grew from 0)
        update_entry_metadata(
            &mut self.vol,
            info.parent_cluster,
            &info.entry.name,
            info.entry.first_cluster,
            info.entry.file_size,
            info.entry.mtime_ns,
        )?;

        // FAT_BEFORE_DIR already satisfied by flush_caches() above calling fat_cache flush
        if let Some(i) = self.open_info.get_mut(&h.0) {
            i.dirty = false;
        }
        Ok(())
    }
}

// ── FileSystem impl ───────────────────────────────────────────────────────────

impl FileSystem for FatFs {
    // ── open ──────────────────────────────────────────────────────────────

    fn open(&mut self, rel_path: &str, flags: OpenFlags) -> VfsResult<BackendHandle> {
        let full = format!("/{}", rel_path.trim_start_matches('/'));

        match resolve(&mut self.vol, &full) {
            Ok(r) => {
                let is_dir = r.entry.attributes & attr::DIRECTORY != 0;

                // O_CREAT | O_EXCL — must not exist
                if flags.create() && flags.excl() { return Err(VfsError::Exists); }

                // Cannot open directory for writing
                if is_dir && flags.writable() { return Err(VfsError::IsDir); }

                // O_TRUNC
                if flags.trunc() && !is_dir && flags.writable() {
                    self.truncate_file_entry(r.parent_cluster, &r.entry)?;
                    // Re-read after truncate
                    let r2 = resolve(&mut self.vol, &full)?;
                    let h = encode_handle(r2.parent_cluster, r2.entry.sfn_slot as u32, is_dir);
                    self.open_info.insert(h.0, OpenInfo {
                        entry: r2.entry,
                        parent_cluster: r2.parent_cluster,
                        dirty: false,
                    });
                    return Ok(h);
                }

                let h = encode_handle(r.parent_cluster, r.entry.sfn_slot as u32, is_dir);
                self.open_info.insert(h.0, OpenInfo {
                    entry: r.entry,
                    parent_cluster: r.parent_cluster,
                    dirty: false,
                });
                Ok(h)
            }
            Err(VfsError::NotFound) if flags.create() => {
                if self.vol.read_only { return Err(VfsError::ReadOnly); }

                let (parent_cluster, name) = resolve_parent(&mut self.vol, &full)?;

                // Allocate first cluster for the new file
                let first_cluster = alloc_one(&mut self.vol)?;
                // DATA_BEFORE_META: zero the cluster
                zero_cluster(&mut self.vol, first_cluster)?;

                let now_ns = 0u64; // kernel time not available in user-space FAT driver
                insert_entry(
                    &mut self.vol,
                    parent_cluster,
                    &name,
                    attr::ARCHIVE,
                    first_cluster,
                    0,
                    now_ns,
                    now_ns,
                )?;

                // Re-read to get the new entry with correct slot
                let r = resolve(&mut self.vol, &full)?;
                let h = encode_handle(r.parent_cluster, r.entry.sfn_slot as u32, false);
                self.open_info.insert(h.0, OpenInfo {
                    entry: r.entry,
                    parent_cluster: r.parent_cluster,
                    dirty: false,
                });
                Ok(h)
            }
            Err(e) => Err(e),
        }
    }

    // ── close ─────────────────────────────────────────────────────────────

    fn close(&mut self, h: BackendHandle) -> VfsResult<()> {
        self.flush_open_file(h)?;
        self.open_info.remove(&h.0);
        Ok(())
    }

    // ── read ──────────────────────────────────────────────────────────────

    fn read(&mut self, h: BackendHandle, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?.clone();
        if info.entry.first_cluster == 0 { return Ok(0); }
        cluster_read_bytes(
            &mut self.vol,
            info.entry.first_cluster,
            info.entry.file_size as u64,
            offset,
            buf,
        )
    }

    // ── write ─────────────────────────────────────────────────────────────

    fn write(&mut self, h: BackendHandle, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }
        let (first_cluster, current_size, parent_cluster) = {
            let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?;
            (info.entry.first_cluster, info.entry.file_size as u64, info.parent_cluster)
        };

        let new_size = cluster_write_bytes(
            &mut self.vol,
            first_cluster,
            current_size,
            offset,
            data,
        )?;

        // Update cached size
        if let Some(info) = self.open_info.get_mut(&h.0) {
            info.entry.file_size = new_size as u32;
            info.dirty = true;
        }
        Ok(data.len())
    }

    // ── seek ──────────────────────────────────────────────────────────────

    fn seek(&self, h: BackendHandle, offset: i64, whence: Whence) -> VfsResult<u64> {
        let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?;
        let size = info.entry.file_size as u64;
        // Note: actual position tracking is in fd_table; here we just validate.
        let new_pos: u64 = match whence {
            Whence::Set => {
                if offset < 0 { return Err(VfsError::InvalidArgument); }
                offset as u64
            }
            Whence::Cur => return Err(VfsError::NotSupported), // fd_table handles Cur
            Whence::End => {
                if offset > 0 { size + offset as u64 }
                else {
                    let abs = (-offset) as u64;
                    if abs > size { return Err(VfsError::InvalidArgument); }
                    size - abs
                }
            }
        };
        Ok(new_pos)
    }

    // ── fstat ─────────────────────────────────────────────────────────────

    fn fstat(&self, h: BackendHandle) -> VfsResult<InodeStat> {
        let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?;
        Ok(self.entry_to_stat(&info.entry))
    }

    // ── stat ──────────────────────────────────────────────────────────────

    fn stat(&self, rel_path: &str) -> VfsResult<InodeStat> {
        // stat() takes &self but path resolution needs &mut vol.
        // We can't do that here without interior mutability.  In a real
        // kernel you'd cache stat results; here we return NotSupported and
        // require callers to use fstat() after open().
        //
        // Actually, since fsd is single-threaded and we take &mut through the
        // Vfs dispatch, we never call stat() directly on FatFs — the VFS
        // calls open() then fstat() for stat-like operations.
        Err(VfsError::NotSupported)
    }

    // ── readdir ───────────────────────────────────────────────────────────

    fn readdir(&self, _rel_path: &str) -> VfsResult<Vec<VfsDirEntry>> {
        // Same limitation as stat() — requires &mut vol
        Err(VfsError::NotSupported)
    }

    fn readdir_handle(&mut self, h: BackendHandle, out: &mut Vec<VfsDirEntry>) -> VfsResult<usize> {
        let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?.clone();
        let dir_cluster = info.entry.first_cluster;
        if dir_cluster == 0 { return Ok(0); }

        let raw_entries = list_dir(&mut self.vol, dir_cluster)?;
        for e in &raw_entries {
            out.push(VfsDirEntry {
                name: e.name.clone(),
                stat: self.entry_to_stat(e),
            });
        }
        Ok(raw_entries.len())
    }

    // ── mkdir ─────────────────────────────────────────────────────────────

    fn mkdir(&mut self, rel_path: &str) -> VfsResult<()> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }
        let full = format!("/{}", rel_path.trim_start_matches('/'));

        // Must not already exist
        if resolve(&mut self.vol, &full).is_ok() { return Err(VfsError::Exists); }

        let (parent_cluster, name) = resolve_parent(&mut self.vol, &full)?;

        // Allocate cluster for new directory
        let dir_cluster = alloc_one(&mut self.vol)?;
        zero_cluster(&mut self.vol, dir_cluster)?;

        // Write "." and ".."
        init_dot_entries(&mut self.vol, dir_cluster, parent_cluster, 0)?;

        // Insert entry in parent
        insert_entry(
            &mut self.vol,
            parent_cluster,
            &name,
            attr::DIRECTORY,
            dir_cluster,
            0,
            0,
            0,
        )
    }

    // ── unlink ────────────────────────────────────────────────────────────

    fn unlink(&mut self, rel_path: &str) -> VfsResult<()> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }
        let full = format!("/{}", rel_path.trim_start_matches('/'));

        let r = resolve(&mut self.vol, &full)?;
        if r.entry.attributes & attr::DIRECTORY != 0 { return Err(VfsError::IsDir); }

        // FAT_BEFORE_DIR: free cluster chain before marking dir entry deleted
        if r.entry.first_cluster >= 2 {
            free_cluster_chain(&mut self.vol, r.entry.first_cluster)?;
        }
        // Flush FAT before updating directory
        self.vol.flush_caches()?;

        delete_entry(&mut self.vol, r.parent_cluster, &r.entry.name)
    }

    // ── rmdir ─────────────────────────────────────────────────────────────

    fn rmdir(&mut self, rel_path: &str) -> VfsResult<()> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }
        let full = format!("/{}", rel_path.trim_start_matches('/'));

        let r = resolve(&mut self.vol, &full)?;
        if r.entry.attributes & attr::DIRECTORY == 0 { return Err(VfsError::NotDir); }

        // Refuse to remove non-empty directory
        let entries = list_dir(&mut self.vol, r.entry.first_cluster)?;
        let non_dot = entries.iter().filter(|e| e.name != "." && e.name != "..").count();
        if non_dot > 0 { return Err(VfsError::NotEmpty); }

        // FAT_BEFORE_DIR
        if r.entry.first_cluster >= 2 {
            free_cluster_chain(&mut self.vol, r.entry.first_cluster)?;
        }
        self.vol.flush_caches()?;

        delete_entry(&mut self.vol, r.parent_cluster, &r.entry.name)
    }

    // ── rename ────────────────────────────────────────────────────────────

    fn rename(&mut self, old_rel: &str, new_rel: &str) -> VfsResult<()> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }

        let old_full = format!("/{}", old_rel.trim_start_matches('/'));
        let new_full = format!("/{}", new_rel.trim_start_matches('/'));

        let r_old = resolve(&mut self.vol, &old_full)?;
        let (new_parent, new_name) = resolve_parent(&mut self.vol, &new_full)?;

        // If destination exists, remove it first (file only — spec behaviour)
        if let Ok(r_new) = resolve(&mut self.vol, &new_full) {
            if r_new.entry.attributes & attr::DIRECTORY != 0 {
                return Err(VfsError::IsDir);
            }
            if r_new.entry.first_cluster >= 2 {
                free_cluster_chain(&mut self.vol, r_new.entry.first_cluster)?;
            }
            self.vol.flush_caches()?;
            delete_entry(&mut self.vol, r_new.parent_cluster, &r_new.entry.name)?;
        }

        let is_dir  = r_old.entry.attributes & attr::DIRECTORY != 0;
        let f_attr  = r_old.entry.attributes;
        let fc      = r_old.entry.first_cluster;
        let fsize   = r_old.entry.file_size;
        let ct      = r_old.entry.ctime_ns;
        let mt      = r_old.entry.mtime_ns;

        // Insert under new name
        insert_entry(&mut self.vol, new_parent, &new_name, f_attr, fc, fsize, ct, mt)?;

        // Remove old entry (DATA_BEFORE_META: new entry written first)
        delete_entry(&mut self.vol, r_old.parent_cluster, &r_old.entry.name)?;

        // Update ".." if moving a directory
        if is_dir && fc >= 2 {
            update_entry_metadata(&mut self.vol, fc, "..", new_parent, 0, 0)?;
        }

        Ok(())
    }

    // ── truncate ──────────────────────────────────────────────────────────

    fn truncate(&mut self, h: BackendHandle, new_size: u64) -> VfsResult<()> {
        if self.vol.read_only { return Err(VfsError::ReadOnly); }
        let info = self.open_info.get(&h.0).ok_or(VfsError::BadFd)?.clone();
        self.truncate_to(info.parent_cluster, &info.entry, new_size)?;
        if let Some(i) = self.open_info.get_mut(&h.0) {
            i.entry.file_size = new_size as u32;
            i.dirty = true;
        }
        Ok(())
    }

    // ── fsync ─────────────────────────────────────────────────────────────

    fn fsync(&mut self, h: BackendHandle) -> VfsResult<()> {
        self.flush_open_file(h)?;
        // BARRIER_ON_FSYNC: flush block device after all pending writes
        self.vol.block.flush().map_err(|_| VfsError::Io)
    }

    // ── statvfs ───────────────────────────────────────────────────────────

    fn statvfs(&self) -> VfsResult<StatVfs> {
        let p = &self.vol.params;
        let free = self.vol.fsinfo.free_count as u64;
        Ok(StatVfs {
            block_size:   p.cluster_size,
            total_blocks: p.count_of_clusters as u64,
            free_blocks:  free,
            avail_blocks: free,
            total_inodes: p.count_of_clusters as u64,
            free_inodes:  free,
            fsid:         p.volume_id as u64,
            name_max:     255,
            flags:        if self.vol.read_only { 1 } else { 0 },
        })
    }

    // ── sync_volume ───────────────────────────────────────────────────────

    fn sync_volume(&mut self) -> VfsResult<()> {
        // Flush all open-file dirty state
        let handles: Vec<u64> = self.open_info.keys().cloned().collect();
        for hv in handles {
            let _ = self.flush_open_file(BackendHandle(hv));
        }
        self.vol.unmount()
    }

    // ── identity ──────────────────────────────────────────────────────────

    fn mount_id(&self) -> u32     { self.mount_id }
    fn fstype(&self) -> &'static str { "fat32" }
    fn is_read_only(&self) -> bool { self.vol.read_only }

    // ── recover ───────────────────────────────────────────────────────────

    fn recover(&mut self) -> VfsResult<()> {
        // FAT32 uses CLN_SHUT bit; if dirty at mount the volume may need fsck.
        // We log and continue; a future journaling integration would replay here.
        if self.vol.volume_dirty {
            // Volume was not cleanly shut down; proceed read-write for now
            // (a production driver would run lightweight consistency checks)
        }
        Ok(())
    }
}

// ── Additional FatFs helpers (not part of trait) ──────────────────────────────

impl FatFs {
    fn truncate_file_entry(&mut self, parent_cluster: u32, entry: &ParsedEntry) -> VfsResult<()> {
        self.truncate_to(parent_cluster, entry, 0)
    }

    fn truncate_to(&mut self, parent_cluster: u32, entry: &ParsedEntry, new_size: u64) -> VfsResult<()> {
        let cur_size = entry.file_size as u64;
        let fc       = entry.first_cluster;

        if new_size == cur_size { return Ok(()); }

        if new_size == 0 {
            // Free entire chain
            if fc >= 2 {
                free_cluster_chain(&mut self.vol, fc)?;
            }
            self.vol.flush_caches()?;
            update_file_size(&mut self.vol, parent_cluster, &entry.name, 0)?;
            return Ok(());
        }

        if new_size < cur_size {
            // Shrink: truncate chain
            let clus_sz = self.vol.params.cluster_size as u64;
            let keep_n  = ((new_size + clus_sz - 1) / clus_sz) as usize;
            if keep_n == 0 {
                if fc >= 2 { free_cluster_chain(&mut self.vol, fc)?; }
            } else {
                let last_keep = fat_table::cluster_at(&mut self.vol, fc, keep_n - 1)?;
                fat_table::truncate_chain(&mut self.vol, last_keep)?;
            }
            self.vol.flush_caches()?;
        }
        // For extend (new_size > cur_size): not needed until a write causes it
        update_file_size(&mut self.vol, parent_cluster, &entry.name, new_size as u32)?;
        Ok(())
    }
}
