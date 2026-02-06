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
// - Explicit lifecycle management: create -> map -> unmap -> destroy
// - Strong isolation: mappings are per-address-space and user-accessible only
// - Fail-safe behavior: partial mappings are rolled back on error
//
// Core abstractions:
// - `RegionId`: opaque, unforgeable identifier for shared regions
// - `RegionFlags`: read/write/execute permissions mapped to page flags
// - `SharedRegion`: internal representation of a region and its mappings
// - `SharedMemManager`: global authority managing all regions
//
// Bookkeeping model:
// - Mappings are tracked **per address space** (identified by PML4 physical
//   address), not per thread.  Threads sharing the same PML4 (sibling
//   threads in the same process) see the same mapping.
// - `Option<usize>` is used for the PML4 identity: `None` represents kernel
//   space, `Some(addr)` a specific user-space address space.
// - Unmap uses `unmap_page_in_pml4()` to target the correct page tables,
//   regardless of which address space is currently active.
// - Cleanup on thread termination checks whether sibling threads still
//   share the address space before unmapping page-table entries.
//
// VA window:
// - The shared memory VA window spans from above the identity-map ceiling
//   up to a configurable limit within the 64-bit user canonical range.
// - This eliminates the previous 32-bit 0xBF00_0000 hard cap that caused
//   triple faults on systems with >1 GiB RAM.
//
// Correctness and safety notes:
// - All global state is protected by spinlocks
// - Virtual addresses must be page-aligned and non-overlapping
// - Owner-only destruction enforces clear responsibility
// - Physical memory is returned to the PMM on final destruction

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::mm::{pmm, vm};
use crate::thread::ThreadId;
use crate::log_info;
use crate::log_debug;

const LOG_ORIGIN: &str = "sharedmem";

/// Fallback lower bound when the identity-map ceiling is unknown.
const SHARED_MEM_VA_FALLBACK_BASE: usize = 0x1000_0000; // 256 MiB

/// Hard upper limit for shared memory VA allocations.
///
/// This is a 64-bit-clean limit well inside the user canonical range
/// (0x0000_7FFF_FFFF_FFFF), leaving room above for user stacks and
/// program text.  On x86-64 this gives ~112 TiB of shared-memory VA
/// space — sufficient for any realistic workload.
const SHARED_MEM_VA_HARD_LIMIT: usize = 0x0000_7000_0000_0000;

/// Dynamic VA base, computed at init time from the identity-map ceiling.
/// Shared memory allocations start here, always above all identity-mapped
/// RAM.  Align to 4 MiB boundary for efficient page-table walk.
static SHARED_MEM_VA_BASE: AtomicUsize = AtomicUsize::new(0);

/// Dynamic VA limit, also set at init time.
static SHARED_MEM_VA_LIMIT: AtomicUsize = AtomicUsize::new(0);

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

/// A single mapping of a shared region into an address space.
///
/// Keyed by **address space** (`pml4_phys`), not by thread.  Sibling threads
/// that share the same PML4 share the mapping and should not duplicate it.
#[derive(Debug, Clone)]
struct RegionMapping {
    /// Physical address of the PML4 that owns this mapping.
    ///
    /// `None` means the mapping lives in the kernel's active address space
    /// (rare — only for kernel-internal shared regions).
    /// `Some(addr)` identifies a specific user-space address space.
    pml4_phys: Option<usize>,
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

    /// Map this region into an address space at `virt_addr`.
    ///
    /// `pml4_phys` identifies the address space.  Two threads that share the
    /// same PML4 (i.e. belong to the same process) are considered as occupying
    /// the **same** address space.  Duplicate-mapping detection uses
    /// `pml4_phys` so that sibling threads cannot accidentally double-map.
    ///
    /// Returns the virtual address where the mapping was placed.
    fn map(
        &mut self,
        virt_addr: usize,
        flags: RegionFlags,
        pml4_phys: Option<usize>,
    ) -> Result<usize, SharedMemError> {
        if !pmm::is_page_aligned(virt_addr) {
            return Err(SharedMemError::Unaligned);
        }

        // Validate that virt_addr + region size does not overflow.
        let _mapping_end = virt_addr.checked_add(self.size).ok_or_else(|| {
            log_debug!(
                LOG_ORIGIN,
                "map: VA overflow for region {} at 0x{:X} + 0x{:X}",
                self.id, virt_addr, self.size
            );
            SharedMemError::MappingFailed
        })?;

        // Duplicate check: same region already mapped in the same address space.
        let already_mapped = self.mappings.iter().any(|m| m.pml4_phys == pml4_phys);
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
            pml4_phys,
            virt_addr,
            flags,
        });
        self.ref_count += 1;

        log_debug!(
            LOG_ORIGIN,
            "Mapped region {} at 0x{:X} ({} pages) pml4={:?}",
            self.id,
            virt_addr,
            self.physical_pages.len(),
            pml4_phys
        );

        Ok(virt_addr)
    }

    /// Unmap this region from the given address space.
    ///
    /// Uses `unmap_page_in_pml4()` to target the correct page tables,
    /// regardless of which address space is currently active in CR3.
    fn unmap(&mut self, pml4_phys: Option<usize>) -> Result<(), SharedMemError> {
        let mapping_idx = self.mappings
            .iter()
            .position(|m| m.pml4_phys == pml4_phys)
            .ok_or(SharedMemError::NotMapped)?;

        let mapping = self.mappings.remove(mapping_idx);

        for i in 0..self.physical_pages.len() {
            let virt = mapping.virt_addr + (i * pmm::PAGE_SIZE);
            if let Some(pml4) = mapping.pml4_phys {
                let _ = vm::unmap_page_in_pml4(pml4, virt);
            } else {
                let _ = vm::unmap_page(virt);
            }
        }

        self.ref_count -= 1;

        log_debug!(
            LOG_ORIGIN,
            "Unmapped region {} from pml4={:?} (ref_count={})",
            self.id,
            pml4_phys,
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
    /// memory VA window `[va_base, va_limit)`.
    ///
    /// The window is computed dynamically at init time to start **above** all
    /// identity-mapped RAM, so the common case (no pre-existing mappings in the
    /// window) returns in O(1) without probing.
    ///
    /// Key design decisions:
    ///
    /// 1. **Address-space identity**: used ranges are collected by matching
    ///    `pml4_phys` (an `Option<usize>`).  Sibling threads within the same
    ///    process share the same PML4 and VA space.
    ///
    /// 2. **Window restriction**: only mappings within `[va_base, va_limit)`
    ///    are considered.
    ///
    /// 3. **Full page probing**: after the bookkeeping check, we verify
    ///    **every** page in the candidate range against the actual page tables.
    ///
    /// 4. **Overflow safety**: all arithmetic uses `checked_add`.
    fn find_free_va(
        regions: &BTreeMap<RegionId, SharedRegion>,
        size: usize,
        pml4_phys: Option<usize>,
    ) -> Result<usize, SharedMemError> {
        let aligned_size = pmm::align_up(size);
        if aligned_size == 0 {
            return Err(SharedMemError::InvalidSize);
        }
        let num_pages = aligned_size / pmm::PAGE_SIZE;

        let va_base = SHARED_MEM_VA_BASE.load(Ordering::Relaxed);
        let va_limit = SHARED_MEM_VA_LIMIT.load(Ordering::Relaxed);

        if va_base == 0 || va_limit == 0 || va_base >= va_limit {
            log_debug!(
                LOG_ORIGIN,
                "find_free_va: invalid window base=0x{:X} limit=0x{:X}",
                va_base, va_limit
            );
            return Err(SharedMemError::NoFreeVirtualAddress);
        }

        // ---- 1. Collect used VA ranges for this *address space* (PML4),
        //         restricted to the shared-memory window.
        let mut used_ranges: Vec<(usize, usize)> = Vec::new();
        for region in regions.values() {
            for mapping in &region.mappings {
                if mapping.pml4_phys != pml4_phys {
                    continue;
                }

                let mapping_end = mapping.virt_addr.saturating_add(region.size);

                if mapping_end <= va_base || mapping.virt_addr >= va_limit {
                    continue;
                }

                let clamped_start = mapping.virt_addr.max(va_base);
                let clamped_end = mapping_end.min(va_limit);
                let clamped_size = clamped_end.saturating_sub(clamped_start);

                if clamped_size > 0 {
                    used_ranges.push((clamped_start, clamped_size));
                }
            }
        }
        used_ranges.sort_by_key(|&(addr, _)| addr);

        // ---- 2. Scan for a free gap.
        let mut candidate = va_base;

        while let Some(candidate_end) = candidate.checked_add(aligned_size) {
            if candidate_end > va_limit {
                break;
            }

            // 2a) Skip past any bookkeeping-tracked mapping that overlaps.
            let mut collided_with_shared = false;
            for &(used_start, used_size) in &used_ranges {
                let used_end = used_start.saturating_add(used_size);
                if candidate_end > used_start && candidate < used_end {
                    candidate = pmm::align_up(used_end);
                    collided_with_shared = true;
                    break;
                }
            }
            if collided_with_shared {
                continue;
            }

            // 2b) Probe the actual page tables for the ENTIRE candidate range.
            //     Since the dynamic base is above the identity-map ceiling,
            //     this loop typically finds zero collisions and runs quickly.
            //     In the rare case that a stray mapping (framebuffer, MMIO,
            //     user stack) falls within the window, we skip past it.
            if let Some(pml4) = pml4_phys {
                let mut page_collision = false;
                let mut collision_end = candidate;

                for page_idx in 0..num_pages {
                    let probe_va = candidate + page_idx * pmm::PAGE_SIZE;
                    if vm::query_mapping_in_pml4(pml4, probe_va).is_ok() {
                        page_collision = true;
                        collision_end = probe_va + pmm::PAGE_SIZE;
                        // Scan ahead to find the end of this mapped region
                        // so we can skip past it entirely.
                        for lookahead in (page_idx + 1)..num_pages {
                            let la_va = candidate + lookahead * pmm::PAGE_SIZE;
                            if vm::query_mapping_in_pml4(pml4, la_va).is_ok() {
                                collision_end = la_va + pmm::PAGE_SIZE;
                            } else {
                                break;
                            }
                        }
                        break;
                    }
                }

                if page_collision {
                    // Skip past the mapped region, aligned to 4 KiB.
                    candidate = pmm::align_up(collision_end);
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
        virt_addr: usize,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        let mut regions = self.regions.lock();

        let effective_va = if virt_addr == 0 {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, size, None)?
        } else {
            Self::validate_explicit_va(virt_addr, regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?.size)?;
            virt_addr
        };

        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(effective_va, flags, None)
    }

    fn map_region_in_pml4(
        &self,
        region_id: RegionId,
        pml4_phys: usize,
        virt_addr: usize,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        let mut regions = self.regions.lock();

        let effective_va = if virt_addr == 0 {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, size, Some(pml4_phys))?
        } else {
            Self::validate_explicit_va(virt_addr, regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?.size)?;
            virt_addr
        };

        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(effective_va, flags, Some(pml4_phys))
    }

    /// Validate that an explicit (user-provided) virtual address is sane:
    /// page-aligned, within the user canonical range, and won't overflow.
    fn validate_explicit_va(virt_addr: usize, region_size: usize) -> Result<(), SharedMemError> {
        let user_canonical_max = atom_abi::USER_CANONICAL_MAX as usize;

        if !pmm::is_page_aligned(virt_addr) {
            return Err(SharedMemError::Unaligned);
        }
        if virt_addr > user_canonical_max {
            log_debug!(
                LOG_ORIGIN,
                "validate_explicit_va: 0x{:X} exceeds USER_CANONICAL_MAX 0x{:X}",
                virt_addr, user_canonical_max
            );
            return Err(SharedMemError::MappingFailed);
        }
        match virt_addr.checked_add(region_size) {
            Some(end) if end <= user_canonical_max + 1 => Ok(()),
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

    /// Unmap a region from the given address space.
    fn unmap_region(&self, region_id: RegionId, pml4_phys: Option<usize>) -> Result<(), SharedMemError> {
        let mut regions = self.regions.lock();
        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        region.unmap(pml4_phys)
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
    // Compute the dynamic VA base from the identity-map ceiling reported by
    // the VMM.  This ensures shared memory allocations start above all
    // identity-mapped RAM, eliminating collisions without needing to probe
    // every page.  Align up to 4 MiB boundary for clean PD-level alignment.
    const ALIGN_4M: usize = 4 * 1024 * 1024;
    let ceiling = vm::identity_map_ceiling();
    let dynamic_base = if ceiling > 0 {
        (ceiling + ALIGN_4M - 1) & !(ALIGN_4M - 1)
    } else {
        SHARED_MEM_VA_FALLBACK_BASE
    };

    // Clamp the base to stay within the valid VA window.  On systems where
    // the identity-map ceiling exceeds our hard limit (very large RAM or
    // scattered RuntimeServices), bump the base just above the ceiling
    // and let the page-table prober handle stray mappings.  Because the
    // hard limit is now in the 64-bit range (0x7000_0000_0000), this
    // condition is effectively unreachable for any real hardware.
    let effective_base = if dynamic_base < SHARED_MEM_VA_HARD_LIMIT {
        dynamic_base
    } else {
        // Extremely large identity map — start right above it, capped
        // to the user canonical range.
        let user_max = atom_abi::USER_CANONICAL_MAX as usize;
        let capped = dynamic_base.min(user_max - ALIGN_4M);
        (capped + ALIGN_4M - 1) & !(ALIGN_4M - 1)
    };
    let effective_limit = SHARED_MEM_VA_HARD_LIMIT;

    SHARED_MEM_VA_BASE.store(effective_base, Ordering::Relaxed);
    SHARED_MEM_VA_LIMIT.store(effective_limit, Ordering::Relaxed);

    log_info!(
        LOG_ORIGIN,
        "Shared memory subsystem initialized (Phase 4.3)"
    );
    log_info!(
        LOG_ORIGIN,
        "VA window: 0x{:X} - 0x{:X} (identity-map ceiling: 0x{:X})",
        effective_base,
        effective_limit,
        ceiling
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
    _thread_id: ThreadId,
    virt_addr: usize,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region(region_id, virt_addr, flags)
}

/// Map a shared region into a specific PML4 (address space).
/// If `virt_addr == 0`, the kernel auto-assigns a VA from the shared memory range.
/// Returns the virtual address where the region was mapped.
pub fn map_region_in_pml4(
    region_id: RegionId,
    _thread_id: ThreadId,
    pml4_phys: u64,
    virt_addr: usize,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region_in_pml4(region_id, pml4_phys as usize, virt_addr, flags)
}

/// Unmap a shared region from a thread's address space.
///
/// The caller's PML4 is resolved from the thread's `address_space` field.
/// If the thread shares an address space with siblings, they all lose the
/// mapping (because they share the same page tables).
pub fn unmap_region(region_id: RegionId, thread_id: ThreadId) -> Result<(), SharedMemError> {
    let pml4_phys = resolve_thread_pml4(thread_id);
    SHARED_MEM_MANAGER.unmap_region(region_id, pml4_phys)
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

/// Resolve a thread's PML4 physical address into an `Option<usize>`.
///
/// Returns `None` if the thread uses the kernel's own PML4 (address_space == 0
/// or matches the current kernel CR3).  Returns `Some(pml4_phys)` for
/// user-space threads that have their own address space.
fn resolve_thread_pml4(thread_id: ThreadId) -> Option<usize> {
    let addr_space = crate::thread::get_thread_address_space(thread_id).unwrap_or(0);
    if addr_space == 0 || addr_space == crate::arch::read_cr3() {
        None
    } else {
        Some(addr_space as usize)
    }
}

/// Check whether any other live threads share the same address space (PML4)
/// as the given thread.
fn other_threads_share_address_space(thread_id: ThreadId, addr_space: u64) -> bool {
    let mut buf = [crate::thread::ProcessInfo {
        pid: 0,
        state: 0,
        name: [0u8; 32],
    }; 64];
    let count = crate::thread::list_processes(&mut buf);

    for i in 0..count {
        let tid = crate::thread::ThreadId::from_raw(buf[i].pid);
        if tid == thread_id {
            continue;
        }
        // Skip exited threads — they don't hold mappings.
        if buf[i].state == 3 {
            continue;
        }
        if let Some(other_as) = crate::thread::get_thread_address_space(tid) {
            if other_as == addr_space {
                return true;
            }
        }
    }

    false
}

/// Cleanup all shared memory regions owned by or mapped by a thread.
///
/// Called when a thread terminates.  Address-space aware: page-table entries
/// are only unmapped when no sibling threads still share the PML4.
///
/// Cleanup policy:
/// 1. If other threads share this address space, skip page-table unmapping
///    (the mappings are still reachable by siblings).
/// 2. If this is the last thread in the address space, unmap all regions.
/// 3. Destroy all owned regions whose ref_count has reached 0.
pub fn cleanup_thread_shared_memory(thread_id: ThreadId) {
    let addr_space = crate::thread::get_thread_address_space(thread_id).unwrap_or(0);
    let pml4_opt = if addr_space == 0 || addr_space == crate::arch::read_cr3() {
        None
    } else {
        Some(addr_space as usize)
    };

    // Check if sibling threads share this address space.
    let siblings_alive = if addr_space != 0 && addr_space != crate::arch::read_cr3() {
        other_threads_share_address_space(thread_id, addr_space)
    } else {
        false
    };

    let mut regions = SHARED_MEM_MANAGER.regions.lock();

    // Collect regions that have a mapping in this address space.
    let mut regions_to_unmap = Vec::new();
    let mut regions_to_destroy = Vec::new();

    for (region_id, region) in regions.iter() {
        if region.mappings.iter().any(|m| m.pml4_phys == pml4_opt) {
            regions_to_unmap.push(*region_id);
        }
        if region.owner == thread_id {
            if region.ref_count == 0 || region.mappings.is_empty() {
                regions_to_destroy.push(*region_id);
            } else {
                log_debug!(
                    LOG_ORIGIN,
                    "Region {} owned by thread {} still has {} mappings - deferred cleanup",
                    region_id,
                    thread_id,
                    region.ref_count
                );
            }
        }
    }

    log_info!(
        LOG_ORIGIN,
        "Cleaning up shared memory for thread {}: {} mappings (siblings_alive={}), {} regions to destroy",
        thread_id,
        regions_to_unmap.len(),
        siblings_alive,
        regions_to_destroy.len()
    );

    // Only unmap page-table entries if no sibling threads share the address space.
    if !siblings_alive {
        for region_id in regions_to_unmap {
            if let Some(region) = regions.get_mut(&region_id) {
                if let Err(e) = region.unmap(pml4_opt) {
                    log_debug!(
                        LOG_ORIGIN,
                        "Failed to unmap region {} from pml4={:?}: {:?}",
                        region_id,
                        pml4_opt,
                        e
                    );
                }
            }
        }
    }

    // Destroy all regions owned by this thread that have no remaining references.
    for region_id in regions_to_destroy {
        if let Some(region) = regions.get(&region_id) {
            if region.can_destroy() {
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
    }
}
