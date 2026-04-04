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
// - Shared region ownership is tracked by ProcessId.
// - Each mapping carries both a logical membership key (`process_id`) and the
//   material page-table identity (`pml4_phys`).
// - PML4 is used only for low-level page-table operations.
// - Cleanup is process-scoped and keyed by ProcessId.
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

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use atom_abi::{UserAddressError, UserRange, UserVAddr};
use crate::mm::{pmm, vm};
use crate::process::{self, ProcessId};
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

static CLEANED_SHARED_MEMORY_PROCESSES: Mutex<BTreeSet<ProcessId>> = Mutex::new(BTreeSet::new());

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

    pub fn page_flags(self) -> vm::PageFlags {
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

    pub fn raw_bits(self) -> u64 {
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

/// A single mapping of a shared region into a process-owned address space.
#[derive(Debug, Clone)]
struct RegionMapping {
    process_id: ProcessId,
    /// Physical address of the process PML4 that materializes this mapping.
    pml4_phys: usize,
    virt_addr: usize,
    flags: RegionFlags,
}

#[derive(Debug)]
struct SharedRegion {
    id: RegionId,
    owner_process: ProcessId,
    size: usize,
    physical_pages: Vec<usize>,
    mappings: Vec<RegionMapping>,
    ref_count: usize,
    is_destroying: bool,
}

impl SharedRegion {
    fn new(id: RegionId, owner_process: ProcessId, size: usize) -> Result<Self, SharedMemError> {
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
                        let _ = pmm::free_page(page);
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
            owner_process,
            size: aligned_size,
            physical_pages,
            mappings: Vec::new(),
            ref_count: 0,
            is_destroying: false,
        })
    }

    fn map(
        &mut self,
        process_id: ProcessId,
        user_range: UserRange,
        flags: RegionFlags,
        pml4_phys: usize,
    ) -> Result<usize, SharedMemError> {
        if self.is_destroying {
            return Err(SharedMemError::RegionInUse);
        }

        let virt_addr = user_range.start();

        debug_assert_process_pml4_alignment(process_id, pml4_phys);
        crate::mm::validate_page_alignment(virt_addr).map_err(map_validation_error)?;

        // Duplicate check: a process may map a region only once.
        let already_mapped = self.mappings.iter().any(|m| m.process_id == process_id);
        if already_mapped {
            return Err(SharedMemError::AlreadyMapped);
        }

        let page_flags = flags.page_flags();
        for (i, &phys_page) in self.physical_pages.iter().enumerate() {
            let virt = virt_addr + (i * pmm::PAGE_SIZE);

            let map_result = vm::map_page_in_pml4(pml4_phys, virt, phys_page, page_flags);

            if let Err(e) = map_result {
                // Rollback on error
                for j in 0..i {
                    let virt_to_unmap = virt_addr + (j * pmm::PAGE_SIZE);
                    let _ = vm::unmap_page_in_pml4(pml4_phys, virt_to_unmap);
                }

                return match e {
                    vm::VmError::AlreadyMapped => Err(SharedMemError::AddressInUse),
                    vm::VmError::OutOfMemory => Err(SharedMemError::OutOfMemory),
                    _ => Err(SharedMemError::MappingFailed),
                };
            }
        }

        self.mappings.push(RegionMapping {
            process_id,
            pml4_phys,
            virt_addr,
            flags,
        });
        self.ref_count += 1;
        debug_assert_eq!(self.ref_count, self.mappings.len());

        if let Some(mapping) = self.mappings.last() {
            debug_assert_mapping_process_alignment(mapping);
        }

        log_debug!(
            LOG_ORIGIN,
            "Mapped region {} into process {} at 0x{:X} ({} pages, pml4=0x{:X})",
            self.id,
            process_id,
            virt_addr,
            self.physical_pages.len(),
            pml4_phys
        );

        Ok(virt_addr)
    }

    fn take_process_mapping(&mut self, process_id: ProcessId) -> Result<RegionMapping, SharedMemError> {
        let mapping_idx = self.mappings
            .iter()
            .position(|m| m.process_id == process_id)
            .ok_or(SharedMemError::NotMapped)?;

        let mapping = self.mappings.remove(mapping_idx);
        debug_assert_eq!(mapping.process_id, process_id);
        debug_assert_mapping_process_alignment(&mapping);
        debug_assert!(
            self.ref_count > 0,
            "shared_mem region {} ref_count underflow while removing process {} mapping",
            self.id,
            process_id
        );

        self.ref_count -= 1;
        debug_assert_eq!(self.ref_count, self.mappings.len());
        Ok(mapping)
    }

    fn mark_destroying(&mut self) -> bool {
        self.is_destroying = true;
        self.mappings.is_empty()
    }

    fn unmap_process_mapping(
        &mut self,
        process_id: ProcessId,
        expected_pml4_phys: Option<usize>,
    ) -> Result<(), SharedMemError> {
        let mapping = self.take_process_mapping(process_id)?;

        if let Some(current_pml4_phys) = expected_pml4_phys {
            debug_assert_eq!(
                mapping.pml4_phys,
                current_pml4_phys,
                "shared_mem mapping for process {} must use the current process PML4 0x{:X}, got 0x{:X}",
                process_id,
                current_pml4_phys,
                mapping.pml4_phys
            );
        }

        unmap_region_mapping_pages(&self.physical_pages, &mapping);

        log_debug!(
            LOG_ORIGIN,
            "Unmapped region {} from process {} (pml4=0x{:X}, ref_count={})",
            self.id,
            process_id,
            mapping.pml4_phys,
            self.ref_count
        );

        Ok(())
    }

    fn unmap(&mut self, process_id: ProcessId, current_pml4_phys: usize) -> Result<(), SharedMemError> {
        self.unmap_process_mapping(process_id, Some(current_pml4_phys))
    }

    fn destroy(&mut self) {
        debug_assert!(
            self.mappings.is_empty(),
            "shared_mem region {} cannot be physically destroyed while mappings remain",
            self.id
        );
        debug_assert_eq!(self.ref_count, 0);

        for &phys_page in &self.physical_pages {
            let _ = pmm::free_page(phys_page);
        }
        self.physical_pages.clear();

        log_debug!(LOG_ORIGIN, "Destroyed region {}", self.id);
    }
}

fn unmap_region_mapping_pages(physical_pages: &[usize], mapping: &RegionMapping) {
    for i in 0..physical_pages.len() {
        let virt = mapping.virt_addr + (i * pmm::PAGE_SIZE);
        let _ = vm::unmap_page_in_pml4(mapping.pml4_phys, virt);
    }
}

struct SharedMemManager {
    regions: Mutex<BTreeMap<RegionId, SharedRegion>>,
}

fn debug_assert_process_exists(process_id: ProcessId) {
    debug_assert!(
        process::get_process(process_id).is_some(),
        "shared_mem requires registered owner process {}",
        process_id
    );
}

fn debug_assert_process_pml4_alignment(process_id: ProcessId, pml4_phys: usize) {
    let process = process::get_process(process_id)
        .expect("shared_mem process references must resolve to a registered process");
    debug_assert_eq!(
        process.pml4_phys as usize,
        pml4_phys,
        "shared_mem process {} must map through its registered PML4 0x{:X}, got 0x{:X}",
        process_id,
        process.pml4_phys,
        pml4_phys
    );
}

fn debug_assert_mapping_process_alignment(mapping: &RegionMapping) {
    debug_assert_process_pml4_alignment(mapping.process_id, mapping.pml4_phys);
}

fn debug_assert_no_zombie_regions(regions: &BTreeMap<RegionId, SharedRegion>) {
    let zombie_region = regions
        .iter()
        .find(|(_, region)| region.is_destroying && region.mappings.is_empty())
        .map(|(region_id, _)| *region_id);

    debug_assert!(
        zombie_region.is_none(),
        "shared_mem region {} remained in destroying state with zero mappings",
        zombie_region.unwrap_or_default()
    );
}

fn process_pml4(process_id: ProcessId) -> Result<usize, SharedMemError> {
    let process = process::get_process(process_id).ok_or(SharedMemError::PermissionDenied)?;
    let pml4_phys = process.pml4_phys as usize;
    debug_assert_ne!(
        pml4_phys,
        0,
        "userspace shared_mem process {} must have a non-zero PML4",
        process_id
    );
    Ok(pml4_phys)
}

impl SharedMemManager {
    const fn new() -> Self {
        Self {
            regions: Mutex::new(BTreeMap::new()),
        }
    }

    fn create_region(&self, owner_process: ProcessId, size: usize) -> Result<RegionId, SharedMemError> {
        debug_assert_process_exists(owner_process);

        let region_id = RegionId::new();
        let region = SharedRegion::new(region_id, owner_process, size)?;

        self.regions.lock().insert(region_id, region);

        log_info!(
            LOG_ORIGIN,
            "Created region {} with size {} bytes (owner_process: {})",
            region_id,
            size,
            owner_process
        );

        Ok(region_id)
    }

    fn destroy_region_internal(
        regions: &mut BTreeMap<RegionId, SharedRegion>,
        region_id: RegionId,
    ) {
        let mut region = regions
            .remove(&region_id)
            .expect("shared_mem destroy_region_internal requires a live region");

        debug_assert!(
            region.is_destroying,
            "shared_mem internal destruction requires region {} to be marked destroying",
            region_id
        );
        debug_assert!(
            region.mappings.is_empty(),
            "shared_mem internal destruction requires region {} to have no mappings",
            region_id
        );
        debug_assert_eq!(
            region.ref_count,
            0,
            "shared_mem internal destruction requires region {} ref_count to be zero",
            region_id
        );

        region.destroy();

        debug_assert!(
            !regions.contains_key(&region_id),
            "shared_mem region {} must be removed from registry after destruction",
            region_id
        );
    }

    fn finalize_deferred_destruction_if_ready(
        regions: &mut BTreeMap<RegionId, SharedRegion>,
        region_id: RegionId,
    ) -> bool {
        let should_destroy = matches!(
            regions.get(&region_id),
            Some(region) if region.is_destroying && region.mappings.is_empty()
        );

        if should_destroy {
            log_info!(
                LOG_ORIGIN,
                "Finalizing deferred destruction of region {}",
                region_id
            );
            Self::destroy_region_internal(regions, region_id);
        }

        should_destroy
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
    /// 1. **Process identity**: used ranges are collected by `process_id`.
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
        process_id: ProcessId,
        pml4_phys: usize,
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

        debug_assert_process_pml4_alignment(process_id, pml4_phys);

        // ---- 1. Collect used VA ranges for this process, restricted to the
        //         shared-memory window.
        let mut used_ranges: Vec<(usize, usize)> = Vec::new();
        for region in regions.values() {
            for mapping in &region.mappings {
                if mapping.process_id != process_id {
                    continue;
                }

                debug_assert_mapping_process_alignment(mapping);

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
            let mut page_collision = false;
            let mut collision_end = candidate;

            for page_idx in 0..num_pages {
                let probe_va = candidate + page_idx * pmm::PAGE_SIZE;
                if vm::query_mapping_in_pml4(pml4_phys, probe_va).is_ok() {
                    page_collision = true;
                    collision_end = probe_va + pmm::PAGE_SIZE;
                    // Scan ahead to find the end of this mapped region
                    // so we can skip past it entirely.
                    for lookahead in (page_idx + 1)..num_pages {
                        let la_va = candidate + lookahead * pmm::PAGE_SIZE;
                        if vm::query_mapping_in_pml4(pml4_phys, la_va).is_ok() {
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

            // Candidate range is free in both metadata and page tables.
            return Ok(candidate);
        }

        Err(SharedMemError::NoFreeVirtualAddress)
    }

    fn map_region(
        &self,
        region_id: RegionId,
        process_id: ProcessId,
        virt_addr: Option<UserVAddr>,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        let pml4_phys = process_pml4(process_id)?;
        let mut regions = self.regions.lock();

        let region_size = regions
            .get(&region_id)
            .ok_or(SharedMemError::InvalidRegion)?
            .size;

        let effective_va = if let Some(base) = virt_addr {
            Self::validate_explicit_va(base, region_size)?
        } else {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, size, process_id, pml4_phys)?
        };

        let effective_range = atom_abi::validate_user_range(effective_va, region_size)
            .map_err(map_user_address_error)?;
        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(process_id, effective_range, flags, pml4_phys)
    }

    fn map_region_in_pml4(
        &self,
        region_id: RegionId,
        process_id: ProcessId,
        pml4_phys: usize,
        virt_addr: Option<UserVAddr>,
        flags: RegionFlags,
    ) -> Result<usize, SharedMemError> {
        debug_assert_process_pml4_alignment(process_id, pml4_phys);
        let mut regions = self.regions.lock();

        let region_size = regions
            .get(&region_id)
            .ok_or(SharedMemError::InvalidRegion)?
            .size;

        let effective_va = if let Some(base) = virt_addr {
            Self::validate_explicit_va(base, region_size)?
        } else {
            let size = regions.get(&region_id)
                .ok_or(SharedMemError::InvalidRegion)?
                .size;
            Self::find_free_va(&regions, size, process_id, pml4_phys)?
        };

        let effective_range = atom_abi::validate_user_range(effective_va, region_size)
            .map_err(map_user_address_error)?;
        let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
        region.map(process_id, effective_range, flags, pml4_phys)
    }

    /// Validate that an explicit (user-provided) virtual address is sane:
    /// page-aligned, non-null, within the user canonical range, and won't
    /// overflow.
    ///
    /// Note: callers pass `None` to `map_region_in_pml4` as the auto-assign
    /// sentinel; that case is intercepted *before* this function is called.
    fn validate_explicit_va(virt_addr: UserVAddr, region_size: usize) -> Result<usize, SharedMemError> {
        let base = virt_addr.as_usize();
        crate::mm::validate_page_alignment(base).map_err(map_validation_error)?;
        atom_abi::validate_user_range(base, region_size).map_err(map_user_address_error)?;
        Ok(base)
    }

    /// Unmap a region from the given process.
    fn unmap_region(&self, region_id: RegionId, process_id: ProcessId, pml4_phys: usize) -> Result<(), SharedMemError> {
        let mut regions = self.regions.lock();
        {
            let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;
            region.unmap(process_id, pml4_phys)?;
        }

        Self::finalize_deferred_destruction_if_ready(&mut regions, region_id);
        debug_assert_no_zombie_regions(&regions);

        Ok(())
    }

    fn destroy_region(&self, region_id: RegionId, caller_process: ProcessId) -> Result<(), SharedMemError> {
        debug_assert_process_exists(caller_process);
        let mut regions = self.regions.lock();

        let should_destroy = {
            let region = regions.get_mut(&region_id).ok_or(SharedMemError::InvalidRegion)?;

            if region.owner_process != caller_process {
                return Err(SharedMemError::PermissionDenied);
            }

            let destroy_now = region.mark_destroying();
            debug_assert_eq!(region.ref_count, region.mappings.len());

            if destroy_now {
                log_info!(
                    LOG_ORIGIN,
                    "Destroying unmapped region {} immediately for owner process {}",
                    region_id,
                    caller_process
                );
            } else {
                log_info!(
                    LOG_ORIGIN,
                    "Deferring destruction of region {} for owner process {} until {} mappings are removed",
                    region_id,
                    caller_process,
                    region.mappings.len()
                );
            }

            destroy_now
        };

        if should_destroy {
            Self::destroy_region_internal(&mut regions, region_id);
        }

        debug_assert_no_zombie_regions(&regions);

        Ok(())
    }

    fn get_region_info(&self, region_id: RegionId) -> Result<RegionInfo, SharedMemError> {
        let regions = self.regions.lock();
        let region = regions.get(&region_id).ok_or(SharedMemError::InvalidRegion)?;

        Ok(RegionInfo {
            id: region.id,
            owner_process: region.owner_process,
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
    pub owner_process: ProcessId,
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

fn map_validation_error(err: crate::mm::ValidationError) -> SharedMemError {
    match err {
        crate::mm::ValidationError::Unaligned { .. } => SharedMemError::Unaligned,
        crate::mm::ValidationError::OutOfBounds { .. } => SharedMemError::MappingFailed,
        crate::mm::ValidationError::ProtectedResource { .. } => SharedMemError::PermissionDenied,
        crate::mm::ValidationError::NotInitialized => SharedMemError::MappingFailed,
        crate::mm::ValidationError::InvalidSize { .. } => SharedMemError::InvalidSize,
    }
}

fn map_user_address_error(err: UserAddressError) -> SharedMemError {
    match err {
        UserAddressError::NonCanonical
        | UserAddressError::BelowUserMin
        | UserAddressError::AboveUserMax
        | UserAddressError::Overflow => SharedMemError::MappingFailed,
        UserAddressError::EmptyRange => SharedMemError::InvalidSize,
    }
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

pub fn create_region(owner_process: ProcessId, size: usize) -> Result<RegionId, SharedMemError> {
    SHARED_MEM_MANAGER.create_region(owner_process, size)
}

/// Map a shared region into a thread's address space.
/// If `virt_addr` is `None`, the kernel auto-assigns a VA from the shared memory range.
/// Returns the virtual address where the region was mapped.
pub fn map_region(
    region_id: RegionId,
    process_id: ProcessId,
    virt_addr: Option<UserVAddr>,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region(region_id, process_id, virt_addr, flags)
}

/// Map a shared region into a specific PML4 (address space).
/// If `virt_addr` is `None`, the kernel auto-assigns a VA from the shared memory range.
/// Returns the virtual address where the region was mapped.
pub fn map_region_in_pml4(
    region_id: RegionId,
    process_id: ProcessId,
    pml4_phys: u64,
    virt_addr: Option<UserVAddr>,
    flags: RegionFlags,
) -> Result<usize, SharedMemError> {
    SHARED_MEM_MANAGER.map_region_in_pml4(region_id, process_id, pml4_phys as usize, virt_addr, flags)
}

pub fn unmap_region(region_id: RegionId, process_id: ProcessId, pml4_phys: u64) -> Result<(), SharedMemError> {
    SHARED_MEM_MANAGER.unmap_region(region_id, process_id, pml4_phys as usize)
}

pub fn destroy_region(region_id: RegionId, caller_process: ProcessId) -> Result<(), SharedMemError> {
    SHARED_MEM_MANAGER.destroy_region(region_id, caller_process)
}

pub fn get_region_info(region_id: RegionId) -> Result<RegionInfo, SharedMemError> {
    SHARED_MEM_MANAGER.get_region_info(region_id)
}

pub fn get_stats() -> SharedMemStats {
    SHARED_MEM_MANAGER.get_stats()
}

/// Return the physical address of the first page of a region.
///
/// The kernel uses this to access region data via the identity map
/// (`phys_to_virt_ptr`) without needing to map the region into any
/// user-space address space.  This is safe only from kernel context.
pub fn region_first_phys(region_id: RegionId) -> Option<usize> {
    let mgr = SHARED_MEM_MANAGER.regions.lock();
    mgr.get(&region_id)
        .and_then(|r| r.physical_pages.first().copied())
}

/// Return the size (bytes) of a shared region without acquiring region_info.
pub fn region_size(region_id: RegionId) -> Option<usize> {
    let mgr = SHARED_MEM_MANAGER.regions.lock();
    mgr.get(&region_id).map(|r| r.size)
}

/// Cleanup all shared-memory state owned by or mapped into a process.
///
/// This is the single authoritative cleanup path for shared memory. The
/// caller must invoke it only after final process cleanup has been claimed.
pub fn cleanup_process_shared_memory(process_id: ProcessId) {
    debug_assert!(
        process::is_process_cleaned(process_id),
        "shared_mem cleanup for process {} must run only after final cleanup is claimed",
        process_id
    );
    debug_assert_process_exists(process_id);

    {
        let mut cleaned = CLEANED_SHARED_MEMORY_PROCESSES.lock();
        let inserted = cleaned.insert(process_id);
        debug_assert!(
            inserted,
            "shared_mem cleanup for process {} must run exactly once",
            process_id
        );
        if !inserted {
            return;
        }
    }

    let mut regions = SHARED_MEM_MANAGER.regions.lock();

    let regions_to_destroy: Vec<RegionId> = regions
        .iter()
        .filter_map(|(region_id, region)| {
            if region.owner_process == process_id {
                Some(*region_id)
            } else {
                None
            }
        })
        .collect();

    let regions_to_unmap: Vec<RegionId> = regions
        .iter()
        .filter_map(|(region_id, region)| {
            if region
                .mappings
                .iter()
                .any(|mapping| mapping.process_id == process_id)
            {
                Some(*region_id)
            } else {
                None
            }
        })
        .collect();

    log_info!(
        LOG_ORIGIN,
        "Cleaning up shared memory for process {}: {} owned regions, {} total mappings to remove",
        process_id,
        regions_to_destroy.len(),
        regions_to_unmap.len()
    );

    for region_id in &regions_to_destroy {
        if let Some(region) = regions.get_mut(region_id) {
            debug_assert_eq!(
                region.owner_process,
                process_id,
                "shared_mem marked wrong owner for region {} during process {} cleanup",
                region_id,
                process_id
            );
            region.mark_destroying();
        }
    }

    for region_id in regions_to_unmap {
        if let Some(region) = regions.get_mut(&region_id) {
            if let Err(e) = region.unmap_process_mapping(process_id, None) {
                log_debug!(
                    LOG_ORIGIN,
                    "Failed to unmap region {} from process {} during process cleanup: {:?}",
                    region_id,
                    process_id,
                    e
                );
            }
        }

        SharedMemManager::finalize_deferred_destruction_if_ready(&mut regions, region_id);
    }

    for region_id in regions_to_destroy {
        let region_belongs_to_process = if let Some(region) = regions.get(&region_id) {
            debug_assert_eq!(
                region.owner_process,
                process_id,
                "shared_mem cleanup observed wrong owner for region {} during process {} cleanup",
                region_id,
                process_id
            );
            debug_assert!(
                region.is_destroying,
                "shared_mem owned region {} must be marked destroying during process {} cleanup",
                region_id,
                process_id
            );
            true
        } else {
            false
        };

        if region_belongs_to_process {
            SharedMemManager::finalize_deferred_destruction_if_ready(&mut regions, region_id);
        }
    }

    debug_assert_no_zombie_regions(&regions);

    debug_assert!(
        !regions
            .values()
            .any(|region| region.mappings.iter().any(|mapping| mapping.process_id == process_id)),
        "shared_mem cleanup left mappings behind for process {}",
        process_id
    );
    debug_assert!(
        !regions.values().any(|region| {
            region.owner_process == process_id && (!region.is_destroying || region.mappings.is_empty())
        }),
        "shared_mem cleanup left owner process {} with regions that were not either destroyed or waiting on foreign mappings",
        process_id
    );
}

pub fn forget_process_shared_memory_cleanup(process_id: ProcessId) {
    CLEANED_SHARED_MEMORY_PROCESSES.lock().remove(&process_id);
}
