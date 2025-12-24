// Shared Memory Subsystem
//
// Implements shared memory regions to support zero-copy IPC and efficient
// transfer of large payloads between threads. This subsystem integrates
// tightly with the PMM, VMM, and capability system to provide safe,
// controlled memory sharing.
//
// Key responsibilities:
// - Create fixed-size shared memory regions backed by physical pages
// - Map and unmap regions into multiple thread address spaces
// - Enforce access permissions via per-mapping flags
// - Track active mappings and reference counts
// - Ensure safe cleanup when regions are no longer in use
//
// Design principles:
// - Zero-copy by design: data is shared, not copied, between participants
// - Explicit lifecycle management: create → map → unmap → destroy
// - Strong isolation: mappings are per-thread and user-accessible only
// - Fail-safe behavior: partial mappings are rolled back on error
//
// Core abstractions:
// - `RegionId`: opaque, unforgeable identifier for shared regions
// - `RegionFlags`: read/write/execute permissions mapped to page flags
// - `SharedRegion`: internal representation of a region and its mappings
// - `SharedMemManager`: global authority managing all regions
//
// Implementation details:
// - Region sizes are page-aligned and backed by zeroed physical pages
// - Page poke flags enforce user access and NX by default
// - Mapping tracks (thread, virtual address, permissions) tuples
// - Reference counting prevents destruction while regions are mapped
//
// Correctness and safety notes:
// - All global state is protected by spinlocks
// - Virtual addresses must be page-aligned and non-overlapping
// - Owner-only destruction enforces clear responsibility
// - Physical memory is returned to the PMM on final destruction
//
// Observability and diagnostics:
// - Structured logging for create/map/unmap/destroy operations
// - Runtime statistics for region count, total size, and mappings
//
// Intended usage:
// - High-throughput IPC
// - Large data transfer between services
// - Shared buffers for user-space drivers and servers
//
// This subsystem is a cornerstone for efficient, capability-secured IPC
// in a microkernel-oriented design.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::mm::{pmm, vm};
use crate::thread::ThreadId;
use crate::log_info;
use crate::log_debug;

const LOG_ORIGIN: &str = "sharedmem";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegionId(u64);

impl RegionId {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        RegionId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
    
    pub fn from_raw(raw: u64) -> Self {
        RegionId(raw)
    }
    
    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl Default for RegionId {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for RegionId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Region({})", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionFlags {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl RegionFlags {
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            execute: false,
        }
    }

    pub const fn read_write() -> Self {
        Self {
            read: true,
            write: true,
            execute: false,
        }
    }

    pub const fn read_exec() -> Self {
        Self {
            read: true,
            write: false,
            execute: true,
        }
    }

    pub const fn read_write_exec() -> Self {
        Self {
            read: true,
            write: true,
            execute: true,
        }
    }

    pub fn to_page_flags(&self) -> vm::PageFlags {
        let mut flags = vm::PageFlags::PRESENT;

        if self.write {
            flags |= vm::PageFlags::WRITABLE;
        }

        if !self.execute {
            flags = flags.with_nx();
        }

        flags | vm::PageFlags::USER
    }

    pub fn from_raw(raw: u64) -> Self {
        let bits = raw & 0x7;

        let (read, write, execute) = if bits == raw {
            let elf_read = (bits & 0x4) != 0;
            let elf_write = (bits & 0x2) != 0;
            let elf_exec = (bits & 0x1) != 0;

            let custom_read = (bits & 0x1) != 0;
            let custom_write = (bits & 0x2) != 0;
            let custom_exec = (bits & 0x4) != 0;

            let looks_like_elf = elf_exec && !custom_exec;

            if looks_like_elf {
                (elf_read, elf_write, elf_exec)
            } else {
                (custom_read, custom_write, custom_exec)
            }
        } else {
            (
                (raw & 0x1) != 0,
                (raw & 0x2) != 0,
                (raw & 0x4) != 0,
            )
        };

        Self { read, write, execute }
    }

    pub fn to_raw(&self) -> u64 {
        let mut raw = 0u64;

        if self.read {
            raw |= 0x4;
        }
        if self.write {
            raw |= 0x2;
        }
        if self.execute {
            raw |= 0x1;
        }

        raw
    }
}

#[derive(Debug, Clone)]
struct RegionMapping {
    thread_id: ThreadId,
    virt_addr: usize,
    flags: RegionFlags,
}

#[derive(Debug)]
struct SharedRegion {
    id: RegionId,
    owner: ThreadId,
    size: usize,
    physical_pages: Vec<usize>,
    mappings: Vec<RegionMapping>,
    ref_count: usize,
}

impl SharedRegion {
    fn new(id: RegionId, owner: ThreadId, size: usize) -> Result<Self, SharedMemError> {
        let aligned_size = pmm::align_up(size);
        let num_pages = aligned_size / pmm::PAGE_SIZE;

        if num_pages == 0 {
            return Err(SharedMemError::InvalidSize);
        }

        let mut physical_pages = Vec::new();
        for _ in 0..num_pages {
            match pmm::alloc_page_zeroed() {
                Some(phys) => physical_pages.push(phys),
                None => {
                    for &page in &physical_pages {
                        pmm::free_page(page);
                    }
                    return Err(SharedMemError::OutOfMemory);
                }
            }
        }

        log_debug!(
            LOG_ORIGIN,
            "Created region {} with {} pages ({} bytes)",
            id,
            num_pages,
            aligned_size
        );

        Ok(Self {
            id,
            owner,
            size: aligned_size,
            physical_pages,
            mappings: Vec::new(),
            ref_count: 0,
        })
    }

    fn map(&mut self, thread_id: ThreadId, virt_addr: usize, flags: RegionFlags)
        -> Result<(), SharedMemError>
    {
        if !pmm::is_page_aligned(virt_addr) {
            return Err(SharedMemError::Unaligned);
        }

        if self.mappings.iter().any(|m| m.thread_id == thread_id) {
            return Err(SharedMemError::AlreadyMapped);
        }

        let page_flags = flags.to_page_flags();
        for (i, &phys_page) in self.physical_pages.iter().enumerate() {
            let virt = virt_addr + (i * pmm::PAGE_SIZE);

            if let Err(e) = vm::map_page(virt, phys_page, page_flags) {
                for j in 0..i {
                    let virt_to_unmap = virt_addr + (j * pmm::PAGE_SIZE);
                    let _ = vm::unmap_page(virt_to_unmap);
                }

                return match e {
                    vm::VmError::AlreadyMapped => Err(SharedMemError::AlreadyMapped),
                    vm::VmError::OutOfMemory => Err(SharedMemError::OutOfMemory),
                    _ => Err(SharedMemError::MappingFailed),
                };
            }
        }

        self.mappings.push(RegionMapping {
            thread_id,
            virt_addr,
            flags,
        });
        self.ref_count += 1;

        log_debug!(
            LOG_ORIGIN,
            "Mapped region {} to thread {} at 0x{:X} ({} pages)",
            self.id,
            thread_id,
            virt_addr,
            self.physical_pages.len()
        );

        Ok(())
    }

    fn unmap(&mut self, thread_id: ThreadId) -> Result<(), SharedMemError> {
        let mapping_idx = self.mappings
            .iter()
            .position(|m| m.thread_id == thread_id)
            .ok_or(SharedMemError::NotMapped)?;

        let mapping = self.mappings.remove(mapping_idx);

        for i in 0..self.physical_pages.len() {
            let virt = mapping.virt_addr + (i * pmm::PAGE_SIZE);
            let _ = vm::unmap_page(virt);
        }

        self.ref_count -= 1;

        log_debug!(
            LOG_ORIGIN,
            "Unmapped region {} from thread {} (ref_count={})",
            self.id,
            thread_id,
            self.ref_count
        );

        Ok(())
    }

    fn can_destroy(&self) -> bool {
        self.ref_count == 0
    }

    fn destroy(&mut self) {
        for &phys_page in &self.physical_pages {
            pmm::free_page(phys_page);
        }
        self.physical_pages.clear();

        log_debug!(LOG_ORIGIN, "Destroyed region {}", self.id);
    }
}

struct SharedMemManager {
    regions: Mutex<BTreeMap<RegionId, SharedRegion>>,
}

impl SharedMemManager {
    const fn new() -> Self {
        Self {
            regions: Mutex::new(BTreeMap::new()),
        }
    }

    fn create_region(&self, owner: ThreadId, size: usize) -> Result<RegionId, SharedMemError> {
        let region_id = RegionId::new();
        let region = SharedRegion::new(region_id, owner, size)?;

        self.regions.lock().insert(region_id, region);

        log_info!(
            LOG_ORIGIN,
            "Created region {} with size {} bytes (owner: {})",
            region_id,
            size,
            owner
        );

        Ok(region_id)
    }

    fn map_region(
        &self,
        region_id: RegionId,
        thread_id: ThreadId,
        virt_addr: usize,
        flags: RegionFlags,
    ) -> Result<(), SharedMemError> {
        let mut regions = self.regions.lock();
        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        region.map(thread_id, virt_addr, flags)
    }

    fn unmap_region(&self, region_id: RegionId, thread_id: ThreadId) -> Result<(), SharedMemError> {
        let mut regions = self.regions.lock();
        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        region.unmap(thread_id)
    }

    fn destroy_region(&self, region_id: RegionId, caller: ThreadId) -> Result<(), SharedMemError> {
        let mut regions = self.regions.lock();

        let region = regions.get(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        if region.owner != caller {
            return Err(SharedMemError::PermissionDenied);
        }

        if !region.can_destroy() {
            return Err(SharedMemError::RegionInUse);
        }

        if let Some(mut region) = regions.remove(&region_id) {
            region.destroy();
        }

        log_info!(LOG_ORIGIN, "Destroyed region {} by thread {}", region_id, caller);

        Ok(())
    }

    fn get_region_info(&self, region_id: RegionId) -> Result<RegionInfo, SharedMemError> {
        let regions = self.regions.lock();
        let region = regions.get(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        Ok(RegionInfo {
            id: region.id,
            owner: region.owner,
            size: region.size,
            ref_count: region.ref_count,
        })
    }

    fn get_stats(&self) -> SharedMemStats {
        let regions = self.regions.lock();
        let total_size: usize = regions.values().map(|r| r.size).sum();
        let total_mappings: usize = regions.values().map(|r| r.ref_count).sum();

        SharedMemStats {
            total_regions: regions.len(),
            total_size,
            total_mappings,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RegionInfo {
    pub id: RegionId,
    pub owner: ThreadId,
    pub size: usize,
    pub ref_count: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SharedMemStats {
    pub total_regions: usize,
    pub total_size: usize,
    pub total_mappings: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedMemError {
    InvalidRegion,
    InvalidSize,
    PermissionDenied,
    OutOfMemory,
    Unaligned,
    AlreadyMapped,
    NotMapped,
    MappingFailed,
    RegionInUse,
}

impl core::fmt::Display for SharedMemError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SharedMemError::InvalidRegion => write!(f, "Invalid region"),
            SharedMemError::InvalidSize => write!(f, "Invalid size"),
            SharedMemError::PermissionDenied => write!(f, "Permission denied"),
            SharedMemError::OutOfMemory => write!(f, "Out of memory"),
            SharedMemError::Unaligned => write!(f, "Address not aligned"),
            SharedMemError::AlreadyMapped => write!(f, "Already mapped"),
            SharedMemError::NotMapped => write!(f, "Not mapped"),
            SharedMemError::MappingFailed => write!(f, "Mapping failed"),
            SharedMemError::RegionInUse => write!(f, "Region in use"),
        }
    }
}

static SHARED_MEM_MANAGER: SharedMemManager = SharedMemManager::new();

pub fn init() {
    log_info!(
        LOG_ORIGIN,
        "Shared memory subsystem initialized (Phase 4.3)"
    );
    log_info!(LOG_ORIGIN, "Zero-copy IPC via shared regions enabled");
}

pub fn create_region(owner: ThreadId, size: usize) -> Result<RegionId, SharedMemError> {
    SHARED_MEM_MANAGER.create_region(owner, size)
}

pub fn map_region(
    region_id: RegionId,
    thread_id: ThreadId,
    virt_addr: usize,
    flags: RegionFlags,
) -> Result<(), SharedMemError> {
    SHARED_MEM_MANAGER.map_region(region_id, thread_id, virt_addr, flags)
}

pub fn unmap_region(region_id: RegionId, thread_id: ThreadId) -> Result<(), SharedMemError> {
    SHARED_MEM_MANAGER.unmap_region(region_id, thread_id)
}

pub fn destroy_region(region_id: RegionId, caller: ThreadId) -> Result<(), SharedMemError> {
    SHARED_MEM_MANAGER.destroy_region(region_id, caller)
}

pub fn get_region_info(region_id: RegionId) -> Result<RegionInfo, SharedMemError> {
    SHARED_MEM_MANAGER.get_region_info(region_id)
}

pub fn get_stats() -> SharedMemStats {
    SHARED_MEM_MANAGER.get_stats()
}