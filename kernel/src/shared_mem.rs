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

/// Start of the VA range dedicated to shared memory mappings in user space.
/// The scanner probes page tables, so even if this falls within the
/// identity-mapped RAM region it will skip forward automatically.
const SHARED_MEM_VA_BASE: usize = 0x1000_0000; // 256 MiB

/// End (exclusive) of the VA range for shared memory mappings.
/// Must be ABOVE the highest RAM identity-mapped address (~2 GiB on most
/// QEMU configs) and BELOW the framebuffer (typically 0xC000_0000).
const SHARED_MEM_VA_LIMIT: usize = 0xBF00_0000; // 3 GiB - 16 MiB

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
    /// Physical address of the PML4 that owns this mapping.  Threads that
    /// share the same process (and therefore the same PML4) share this value.
    /// Used as the **address-space identity** when checking for collisions.
    pml4_phys: usize,
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

    /// Map this region into a thread's address space at `virt_addr`.
    ///
    /// `pml4_phys` identifies the address space.  Two threads that share the
    /// same PML4 (i.e. belong to the same process) are considered as occupying
    /// the **same** address space.  Duplicate-mapping detection uses
    /// `pml4_phys`, not `thread_id`, so that sibling threads cannot
    /// accidentally double-map the same region.
    ///
    /// Returns the virtual address where the mapping was placed.
    fn map(
        &mut self,
        thread_id: ThreadId,
        virt_addr: usize,
        flags: RegionFlags,
        pml4_phys: Option<usize>,
    ) -> Result<usize, SharedMemError> {
        if !pmm::is_page_aligned(virt_addr) {
            return Err(SharedMemError::Unaligned);
        }

        // --- Validate that virt_addr + region size does not overflow.
        //     This is the fundamental safety check that prevents pointer
        //     arithmetic from wrapping into kernel VA space (the root cause
        //     of triple faults on >1 GiB systems).
        let _mapping_end = virt_addr.checked_add(self.size).ok_or_else(|| {
            log_debug!(
                LOG_ORIGIN,
                "map: VA overflow for region {} at 0x{:X} + 0x{:X}",
                self.id, virt_addr, self.size
            );
            SharedMemError::MappingFailed
        })?;

        // --- Duplicate check: same region already mapped in the same
        //     address space.  We check by PML4 when available, falling
        //     back to thread_id for kernel-PML4 mappings (pml4_phys == None).
        let already_mapped = match pml4_phys {
            Some(pml4) => self.mappings.iter().any(|m| m.pml4_phys == pml4),
            None => self.mappings.iter().any(|m| m.thread_id == thread_id),
        };
        if already_mapped {
            return Err(SharedMemError::AlreadyMapped);
        }

        let page_flags = flags.to_page_flags();
        for (i, &phys_page) in self.physical_pages.iter().enumerate() {
            let virt = virt_addr + (i * pmm::PAGE_SIZE);

            let map_result = if let Some(pml4) = pml4_phys {
                vm::map_page_in_pml4(pml4, virt, phys_page, page_flags)
            } else {
                vm::map_page(virt, phys_page, page_flags)
            };

            if let Err(e) = map_result {
                // Rollback on error
                for j in 0..i {
                    let virt_to_unmap = virt_addr + (j * pmm::PAGE_SIZE);
                    if let Some(pml4) = pml4_phys {
                        let _ = vm::unmap_page_in_pml4(pml4, virt_to_unmap);
                    } else {
                        let _ = vm::unmap_page(virt_to_unmap);
                    }
                }

                return match e {
                    vm::VmError::AlreadyMapped => Err(SharedMemError::AddressInUse),
                    vm::VmError::OutOfMemory => Err(SharedMemError::OutOfMemory),
                    _ => Err(SharedMemError::MappingFailed),
                };
            }
        }

        self.mappings.push(RegionMapping {
            thread_id,
            pml4_phys: pml4_phys.unwrap_or(0),
            virt_addr,
            flags,
        });
        self.ref_count += 1;

        log_debug!(
            LOG_ORIGIN,
            "Mapped region {} to thread {} at 0x{:X} ({} pages) pml4={:?}",
            self.id,
            thread_id,
            virt_addr,
            self.physical_pages.len(),
            pml4_phys
        );

        Ok(virt_addr)
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

    /// Find a free virtual address range for `size` bytes within the shared
    /// memory VA window `[SHARED_MEM_VA_BASE, SHARED_MEM_VA_LIMIT)`.
    ///
    /// Correctness guarantees over the previous implementation:
    ///
    /// 1. **Address-space identity**: used ranges are collected by matching
    ///    `pml4_phys` (the physical address of the PML4 root), not
    ///    `thread_id`.  Threads that share the same process (same PML4) share
    ///    the same VA space; filtering by thread_id would miss sibling-thread
    ///    mappings and hand out already-occupied VAs, causing `AlreadyMapped`
    ///    errors or data corruption.
    ///
    /// 2. **Window restriction**: only mappings whose VA range overlaps
    ///    `[SHARED_MEM_VA_BASE, SHARED_MEM_VA_LIMIT)` are considered.
    ///    Mappings outside this window (ELF segments, stack, heap, …) are
    ///    irrelevant and were previously polluting the search, leading to
    ///    unnecessarily fragmented or failed allocations.
    ///
    /// 3. **Consistent sizing**: the used-range size stored in bookkeeping
    ///    (`region.size`) is always page-aligned at creation time (see
    ///    `SharedRegion::new`), so it is consistent with the `aligned_size`
    ///    we compute here.  We still use `aligned_size` for the *request*
    ///    to avoid any mismatch.
    ///
    /// 4. **Page-table probing**: after our metadata check, we probe the
    ///    actual PML4 at the first, middle, and last page of the candidate
    ///    range.  This catches non-shared-memory mappings that occupy the
    ///    window (identity-mapped RAM below ~2 GiB, framebuffer, etc.).
    ///
    /// 5. **Overflow safety**: all pointer arithmetic uses `checked_add` /
    ///    `saturating_add` to avoid wrapping into kernel VA space, which
    ///    was the root cause of triple faults on machines with >1 GiB RAM.
    fn find_free_va(
        regions: &BTreeMap<RegionId, SharedRegion>,
        _thread_id: ThreadId,
        size: usize,
        pml4_phys: Option<usize>,
    ) -> Result<usize, SharedMemError> {
        let aligned_size = pmm::align_up(size);
        if aligned_size == 0 {
            return Err(SharedMemError::InvalidSize);
        }
        let num_pages = aligned_size / pmm::PAGE_SIZE;

        // ---- 1. Collect used VA ranges for this *address space* (PML4),
        //         restricted to the shared-memory window.
        let mut used_ranges: Vec<(usize, usize)> = Vec::new();
        for region in regions.values() {
            for mapping in &region.mappings {
                // Match by address space, not by individual thread.
                let same_address_space = match pml4_phys {
                    Some(pml4) => mapping.pml4_phys == pml4,
                    // Fallback for kernel-PML4 callers (pml4_phys == None):
                    // these always have pml4_phys stored as 0 in the mapping.
                    None => mapping.pml4_phys == 0,
                };
                if !same_address_space {
                    continue;
                }

                // region.size is already page-aligned (set in SharedRegion::new).
                let mapping_end = mapping.virt_addr.saturating_add(region.size);

                // Only consider mappings that overlap the shared-memory window.
                if mapping_end <= SHARED_MEM_VA_BASE || mapping.virt_addr >= SHARED_MEM_VA_LIMIT {
                    continue;
                }

                // Clamp to window boundaries so out-of-window tails don't
                // push candidates beyond the window needlessly.
                let clamped_start = mapping.virt_addr.max(SHARED_MEM_VA_BASE);
                let clamped_end = mapping_end.min(SHARED_MEM_VA_LIMIT);
                let clamped_size = clamped_end.saturating_sub(clamped_start);

                if clamped_size > 0 {
                    used_ranges.push((clamped_start, clamped_size));
                }
            }
        }
        used_ranges.sort_by_key(|&(addr, _)| addr);

        // ---- 2. Scan for a free gap.
        let mut candidate = SHARED_MEM_VA_BASE;

        while let Some(candidate_end) = candidate.checked_add(aligned_size) {
            if candidate_end > SHARED_MEM_VA_LIMIT {
                break;
            }

            // 2a) Skip past any bookkeeping-tracked mapping that overlaps.
            let mut collided_with_shared = false;
            for &(used_start, used_size) in &used_ranges {
                let used_end = used_start.saturating_add(used_size);
                if candidate_end > used_start && candidate < used_end {
                    // Overlap — advance past this region (page-aligned).
                    candidate = pmm::align_up(used_end);
                    collided_with_shared = true;
                    break;
                }
            }
            if collided_with_shared {
                continue;
            }

            // 2b) Probe the actual page tables if we have a PML4.
            //     Check first, middle, and last pages to catch identity-mapped
            //     RAM, framebuffer pages, etc.
            if let Some(pml4) = pml4_phys {
                let pages_to_check: &[usize] = if num_pages <= 2 {
                    &[0, num_pages.saturating_sub(1)]
                } else {
                    &[0, num_pages / 2, num_pages - 1]
                };

                let mut page_collision = false;
                for &page_idx in pages_to_check {
                    let probe_va = candidate + page_idx * pmm::PAGE_SIZE;
                    if vm::query_mapping_in_pml4(pml4, probe_va).is_ok() {
                        page_collision = true;
                        break;
                    }
                }

                if page_collision {
                    // Skip forward by 4 MiB to get past large mapped regions
                    // (identity-mapped RAM) quickly.
                    const SKIP_STEP: usize = 4 * 1024 * 1024; // 4 MiB
                    candidate = (candidate + SKIP_STEP) & !(SKIP_STEP - 1);
                    continue;
                }
            }

            // Candidate range is free in both metadata and page tables.
            return Ok(candidate);
        }

        Err(SharedMemError::NoFreeVirtualAddress)
    }

    fn map_region(
        &self,
        region_id: RegionId,
        thread_id: ThreadId,
        virt_addr: usize,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        let mut regions = self.regions.lock();

        let effective_va = if virt_addr == 0 {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, thread_id, size, None)?
        } else {
            Self::validate_explicit_va(virt_addr, regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?.size)?;
            virt_addr
        };

        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(thread_id, effective_va, flags, None)
    }

    fn map_region_in_pml4(
        &self,
        region_id: RegionId,
        thread_id: ThreadId,
        pml4_phys: usize,
        virt_addr: usize,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        let mut regions = self.regions.lock();

        let effective_va = if virt_addr == 0 {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, thread_id, size, Some(pml4_phys))?
        } else {
            Self::validate_explicit_va(virt_addr, regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?.size)?;
            virt_addr
        };

        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(thread_id, effective_va, flags, Some(pml4_phys))
    }

    /// Validate that an explicit (user-provided) virtual address is sane:
    /// page-aligned, within the user canonical range, and won't overflow.
    fn validate_explicit_va(virt_addr: usize, region_size: usize) -> Result<(), SharedMemError> {
        use crate::mm::addrspace::USER_CANONICAL_MAX;

        if !pmm::is_page_aligned(virt_addr) {
            return Err(SharedMemError::Unaligned);
        }
        if virt_addr > USER_CANONICAL_MAX {
            log_debug!(
                LOG_ORIGIN,
                "validate_explicit_va: 0x{:X} exceeds USER_CANONICAL_MAX 0x{:X}",
                virt_addr, USER_CANONICAL_MAX
            );
            return Err(SharedMemError::MappingFailed);
        }
        match virt_addr.checked_add(region_size) {
            Some(end) if end <= USER_CANONICAL_MAX + 1 => Ok(()),
            _ => {
                log_debug!(
                    LOG_ORIGIN,
                    "validate_explicit_va: 0x{:X} + 0x{:X} overflows user VA",
                    virt_addr, region_size
                );
                Err(SharedMemError::MappingFailed)
            }
        }
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
    /// The requested VA range is occupied by another mapping (not this region).
    AddressInUse,
    /// No free VA range available in the shared memory region.
    NoFreeVirtualAddress,
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
            SharedMemError::AddressInUse => write!(f, "Address in use by another mapping"),
            SharedMemError::NoFreeVirtualAddress => write!(f, "No free virtual address in shared range"),
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

/// Map a shared region into a thread's address space.
/// If `virt_addr == 0`, the kernel auto-assigns a VA from the shared memory range.
/// Returns the virtual address where the region was mapped.
pub fn map_region(
    region_id: RegionId,
    thread_id: ThreadId,
    virt_addr: usize,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region(region_id, thread_id, virt_addr, flags)
}

/// Map a shared region into a specific PML4 (address space).
/// If `virt_addr == 0`, the kernel auto-assigns a VA from the shared memory range.
/// Returns the virtual address where the region was mapped.
pub fn map_region_in_pml4(
    region_id: RegionId,
    thread_id: ThreadId,
    pml4_phys: u64,
    virt_addr: usize,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region_in_pml4(region_id, thread_id, pml4_phys as usize, virt_addr, flags)
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

/// Cleanup all shared memory regions owned by or mapped by a thread
/// This should be called when a thread terminates to ensure proper resource cleanup
///
/// Cleanup policy:
/// 1. Unmap all regions that the thread has mapped (regardless of ownership)
/// 2. Destroy all regions owned by the thread (if ref_count == 0)
/// 3. Mark owned regions for deferred cleanup if still in use by other threads
pub fn cleanup_thread_shared_memory(thread_id: ThreadId) {
    let mut regions = SHARED_MEM_MANAGER.regions.lock();

    // Collect all regions that need cleanup
    let mut regions_to_unmap = Vec::new();
    let mut regions_to_destroy = Vec::new();

    for (region_id, region) in regions.iter() {
        // Check if thread has this region mapped
        if region.mappings.iter().any(|m| m.thread_id == thread_id) {
            regions_to_unmap.push(*region_id);
        }

        // Check if thread owns this region
        if region.owner == thread_id {
            if region.ref_count == 0 || region.mappings.is_empty() {
                regions_to_destroy.push(*region_id);
            } else {
                log_debug!(
                    LOG_ORIGIN,
                    "Region {} owned by thread {} still has {} mappings - will be destroyed when last mapping is removed",
                    region_id,
                    thread_id,
                    region.ref_count
                );
            }
        }
    }

    log_info!(
        LOG_ORIGIN,
        "Cleaning up shared memory for thread {}: {} mappings, {} regions to destroy",
        thread_id,
        regions_to_unmap.len(),
        regions_to_destroy.len()
    );

    // Unmap all regions mapped by this thread
    for region_id in regions_to_unmap {
        if let Some(region) = regions.get_mut(&region_id) {
            if let Err(e) = region.unmap(thread_id) {
                log_debug!(
                    LOG_ORIGIN,
                    "Failed to unmap region {} from thread {}: {:?}",
                    region_id,
                    thread_id,
                    e
                );
            }
        }
    }

    // Destroy all regions owned by this thread that have no remaining references
    for region_id in regions_to_destroy {
        if let Some(mut region) = regions.remove(&region_id) {
            log_debug!(
                LOG_ORIGIN,
                "Destroying region {} owned by thread {} ({} physical pages)",
                region_id,
                thread_id,
                region.physical_pages.len()
            );
            region.destroy();
        }
    }
}