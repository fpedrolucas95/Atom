// Virtual Memory Area (VMA) Management
//
// Implements per-address-space tracking of virtual memory regions with
// support for demand paging, lazy allocation, and memory policy enforcement.
//
// Each VMA describes a contiguous virtual address range with uniform
// properties (permissions, backing type, etc.). VMAs are stored in a
// BTreeMap keyed by start address for O(log n) lookup and insertion.
//
// Key features:
// - Track virtual regions per address space (anon, file-backed, shared, device)
// - Support demand paging: reserve virtual ranges without physical backing
// - Stack growth with guard pages
// - Free virtual address allocation (find_free_region)
// - Per-process memory accounting (resident vs reserved)
//
// Design principles:
// - VMAs are metadata only; physical pages are allocated on demand via page faults
// - Guard pages are never mapped and trigger controlled faults
// - Stack VMAs grow downward automatically when faults occur near the guard

use alloc::collections::BTreeMap;
use spin::Mutex;

use crate::mm::pmm::PAGE_SIZE;
use crate::process::ProcessId;
use crate::{log_info, log_debug, log_warn};

const LOG_ORIGIN: &str = "vma";

/// How a VMA region is backed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum VmaBacking {
    /// Anonymous memory: zero-filled on first access
    Anonymous,
    /// Stack region: grows downward, has guard page at bottom
    Stack {
        /// Maximum size the stack can grow to (in bytes)
        max_size: usize,
    },
    /// Device/MMIO mapping: physical address is fixed, not demand-paged
    Device {
        phys_base: usize,
    },
}

/// Protection flags for a VMA region
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmaPermissions(u8);

#[allow(dead_code)]
impl VmaPermissions {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXEC: Self = Self(1 << 2);

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn read_write() -> Self {
        Self(Self::READ.0 | Self::WRITE.0)
    }

    pub const fn read_exec() -> Self {
        Self(Self::READ.0 | Self::EXEC.0)
    }

    pub const fn read_write_exec() -> Self {
        Self(Self::READ.0 | Self::WRITE.0 | Self::EXEC.0)
    }
}

/// A single Virtual Memory Area descriptor
#[derive(Debug, Clone)]
pub struct Vma {
    /// Start of the virtual range (page-aligned)
    pub start: usize,
    /// End of the virtual range (exclusive, page-aligned)
    pub end: usize,
    /// Access permissions
    pub perms: VmaPermissions,
    /// Backing type
    pub backing: VmaBacking,
    /// Human-readable label for debugging
    pub label: &'static str,
}

impl Vma {
    pub fn size(&self) -> usize {
        self.end - self.start
    }

    #[allow(dead_code)]
    pub fn pages(&self) -> usize {
        self.size() / PAGE_SIZE
    }

    pub fn contains_addr(&self, addr: usize) -> bool {
        addr >= self.start && addr < self.end
    }

    /// Check if this VMA overlaps with the given range [start, end)
    pub fn overlaps(&self, start: usize, end: usize) -> bool {
        self.start < end && start < self.end
    }
}

/// Per-address-space VMA collection and memory accounting
pub struct VmaMap {
    /// VMAs indexed by start address
    regions: BTreeMap<usize, Vma>,
    /// Total virtual bytes reserved (sum of all VMA sizes)
    reserved_bytes: usize,
    /// Total physical pages currently resident (mapped)
    resident_pages: usize,
    /// Maximum allowed resident pages (0 = unlimited)
    resident_limit: usize,
}

impl VmaMap {
    pub const fn new() -> Self {
        Self {
            regions: BTreeMap::new(),
            reserved_bytes: 0,
            resident_pages: 0,
            resident_limit: 0,
        }
    }

    /// Insert a new VMA. Returns error if it overlaps with existing VMAs.
    pub fn insert(&mut self, vma: Vma) -> Result<(), VmaError> {
        // Validate alignment
        if !vma.start.is_multiple_of(PAGE_SIZE) || !vma.end.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::Unaligned);
        }
        if vma.start >= vma.end {
            return Err(VmaError::InvalidRange);
        }

        // Check for overlaps
        for (_, existing) in self.regions.iter() {
            if existing.overlaps(vma.start, vma.end) {
                return Err(VmaError::Overlap);
            }
        }

        let size = vma.size();
        self.regions.insert(vma.start, vma);
        self.reserved_bytes += size;

        Ok(())
    }

    /// Remove a VMA by its start address
    pub fn remove(&mut self, start: usize) -> Option<Vma> {
        if let Some(vma) = self.regions.remove(&start) {
            self.reserved_bytes = self.reserved_bytes.saturating_sub(vma.size());
            Some(vma)
        } else {
            None
        }
    }

    /// Remove all VMAs that overlap with [start, end) and return them
    pub fn remove_range(&mut self, start: usize, end: usize) -> alloc::vec::Vec<Vma> {
        let mut removed = alloc::vec::Vec::new();
        let keys: alloc::vec::Vec<usize> = self.regions.keys().copied().collect();

        for key in keys {
            if let Some(vma) = self.regions.get(&key) {
                if vma.overlaps(start, end) {
                    if let Some(vma) = self.regions.remove(&key) {
                        self.reserved_bytes = self.reserved_bytes.saturating_sub(vma.size());
                        removed.push(vma);
                    }
                }
            }
        }

        removed
    }

    /// Find the VMA containing the given address
    pub fn find(&self, addr: usize) -> Option<&Vma> {
        // Use range to efficiently find the VMA that could contain addr
        // The VMA with the largest start <= addr is the candidate
        if let Some((_, vma)) = self.regions.range(..=addr).next_back() {
            if vma.contains_addr(addr) {
                return Some(vma);
            }
        }
        None
    }

    /// Find a free virtual region of the given size within [low, high)
    /// Returns the start address of the free region.
    /// Uses a simple first-fit strategy scanning from low to high.
    pub fn find_free_region(&self, low: usize, high: usize, size: usize, align: usize) -> Option<usize> {
        let size = align_up(size, PAGE_SIZE);
        let align = align.max(PAGE_SIZE);

        if size == 0 || high <= low || size > high - low {
            return None;
        }

        let mut candidate = align_up(low, align);

        for (_, vma) in self.regions.range(..low).rev().take(1) {
            if vma.end > candidate {
                candidate = align_up(vma.end, align);
            }
        }
 
        for (_, vma) in self.regions.range(low..) {
            if vma.start >= high {
                break;
            }
 
            if candidate + size <= vma.start {
                // Found a gap before this VMA
                return Some(candidate);
            }
 
            // Move past this VMA
            candidate = align_up(vma.end, align);
        }

        // Check if there's space after the last VMA
        if candidate + size <= high {
            Some(candidate)
        } else {
            None
        }
    }

    /// Grow a stack VMA downward by one page. Returns the new start address.
    /// The VMA must be a Stack-backed VMA.
    pub fn grow_stack(&mut self, vma_start: usize) -> Result<usize, VmaError> {
        let vma = self.regions.get(&vma_start).ok_or(VmaError::NotFound)?;

        let max_size = match vma.backing {
            VmaBacking::Stack { max_size } => max_size,
            _ => return Err(VmaError::NotStack),
        };

        let current_size = vma.size();
        if current_size + PAGE_SIZE > max_size {
            return Err(VmaError::StackOverflow);
        }

        let new_start = vma.start - PAGE_SIZE;

        // Check that the new start doesn't overlap with another VMA
        // (the guard page area should be free)
        if let Some(other) = self.find(new_start) {
            if other.start != vma_start {
                return Err(VmaError::Overlap);
            }
        }

        // Remove old VMA, create expanded one, reinsert
        let old_vma = self.regions.remove(&vma_start).unwrap();
        self.reserved_bytes = self.reserved_bytes.saturating_sub(old_vma.size());

        let new_vma = Vma {
            start: new_start,
            end: old_vma.end,
            perms: old_vma.perms,
            backing: old_vma.backing,
            label: old_vma.label,
        };

        let new_size = new_vma.size();
        self.regions.insert(new_start, new_vma);
        self.reserved_bytes += new_size;

        Ok(new_start)
    }

    /// Account for a newly mapped (resident) physical page
    pub fn account_map(&mut self) {
        self.resident_pages += 1;
    }

    /// Account for an unmapped physical page
    pub fn account_unmap(&mut self) {
        self.resident_pages = self.resident_pages.saturating_sub(1);
    }

    /// Get memory statistics
    pub fn stats(&self) -> VmaStats {
        VmaStats {
            vma_count: self.regions.len(),
            reserved_bytes: self.reserved_bytes,
            resident_pages: self.resident_pages,
            resident_bytes: self.resident_pages * PAGE_SIZE,
            resident_limit: self.resident_limit,
        }
    }

    /// Set resident page limit (0 = unlimited)
    pub fn set_resident_limit(&mut self, limit: usize) {
        self.resident_limit = limit;
    }

    /// Check if we can map another page (under limit)
    pub fn can_map_page(&self) -> bool {
        self.resident_limit == 0 || self.resident_pages < self.resident_limit
    }

    /// Iterate all VMAs
    pub fn iter(&self) -> impl Iterator<Item = (&usize, &Vma)> {
        self.regions.iter()
    }

    /// Number of VMAs
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Update permissions on all VMAs that overlap `[start, end)`.
    ///
    /// Supports arbitrary page-aligned ranges: VMAs only partially covered by
    /// `[start, end)` are **split** so that only the overlapping portion
    /// receives the new permissions while the remaining head/tail fragments
    /// keep their original permissions.  This lets callers use the same
    /// patterns expected of a POSIX `mprotect(2)` implementation.
    ///
    /// Returns `VmaError::NotFound` if no VMA overlaps the requested range,
    /// and `VmaError::InvalidRange` if `start`/`end` are not page-aligned or
    /// form an empty interval.
    pub fn set_permissions(&mut self, start: usize, end: usize, perms: VmaPermissions) -> Result<(), VmaError> {
        if !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) || start >= end {
            return Err(VmaError::InvalidRange);
        }

        // Collect the keys of all VMAs that overlap [start, end).
        let keys: alloc::vec::Vec<usize> = self
            .regions
            .iter()
            .filter(|(_, vma)| vma.overlaps(start, end))
            .map(|(k, _)| *k)
            .collect();

        if keys.is_empty() {
            return Err(VmaError::NotFound);
        }

        for key in keys {
            let vma = self.regions.remove(&key).unwrap();
            self.reserved_bytes = self.reserved_bytes.saturating_sub(vma.size());

            // Head fragment: [vma.start, start) — retains original permissions.
            if vma.start < start {
                let head = Vma {
                    start: vma.start,
                    end: start,
                    perms: vma.perms,
                    backing: vma.backing,
                    label: vma.label,
                };
                self.reserved_bytes += head.size();
                self.regions.insert(head.start, head);
            }

            // Middle fragment: [max(vma.start, start), min(vma.end, end)) — new perms.
            let mid_start = vma.start.max(start);
            let mid_end   = vma.end.min(end);
            let mid = Vma {
                start: mid_start,
                end: mid_end,
                perms,
                backing: vma.backing,
                label: vma.label,
            };
            self.reserved_bytes += mid.size();
            self.regions.insert(mid.start, mid);

            // Tail fragment: [end, vma.end) — retains original permissions.
            if vma.end > end {
                let tail = Vma {
                    start: end,
                    end: vma.end,
                    perms: vma.perms,
                    backing: vma.backing,
                    label: vma.label,
                };
                self.reserved_bytes += tail.size();
                self.regions.insert(tail.start, tail);
            }
        }

        Ok(())
    }
}

/// Statistics for an address space's memory usage
#[derive(Debug, Clone, Copy)]
pub struct VmaStats {
    pub vma_count: usize,
    pub reserved_bytes: usize,
    pub resident_pages: usize,
    pub resident_bytes: usize,
    pub resident_limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmaError {
    Unaligned,
    InvalidRange,
    Overlap,
    NotFound,
    NotStack,
    StackOverflow,
    OutOfMemory,
    PermissionDenied,
    OutOfVirtualSpace,
}

// ---------------------------------------------------------------------------
// Global per-address-space VMA tracking
// ---------------------------------------------------------------------------

/// Global registry: maps PML4 physical address -> VmaMap
static VMA_REGISTRY: Mutex<BTreeMap<usize, VmaMap>> = Mutex::new(BTreeMap::new());

fn registered_process_pml4(process_id: ProcessId) -> Result<usize, VmaError> {
    let pml4_phys = crate::process::get_process_pml4(process_id).ok_or(VmaError::NotFound)?;
    debug_assert_ne!(
        pml4_phys,
        0,
        "process {} must resolve to a non-zero primary PML4 for VMA access",
        process_id
    );
    Ok(pml4_phys as usize)
}

fn debug_assert_process_primary_pml4(process_id: ProcessId, pml4_phys: usize) {
    if let Some(registered_pml4) = crate::process::get_process_pml4(process_id) {
        debug_assert_eq!(
            registered_pml4,
            pml4_phys as u64,
            "process {} VMA selection must resolve through the registered primary PML4 0x{:X}, not 0x{:X}",
            process_id,
            registered_pml4,
            pml4_phys
        );
    }
}

pub fn create_bootstrap_process_vma_map(process_id: ProcessId, pml4_phys: usize) {
    debug_assert_process_primary_pml4(process_id, pml4_phys);
    create_vma_map(pml4_phys);
}

pub fn create_process_vma_map(process_id: ProcessId) -> Result<(), VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    create_vma_map(pml4_phys);
    Ok(())
}

pub fn destroy_process_vma_map(process_id: ProcessId) -> Result<(), VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    destroy_vma_map(pml4_phys);
    Ok(())
}

pub fn insert_bootstrap_process_vma(
    process_id: ProcessId,
    pml4_phys: usize,
    vma: Vma,
) -> Result<(), VmaError> {
    debug_assert_process_primary_pml4(process_id, pml4_phys);
    insert_vma(pml4_phys, vma)
}

pub fn insert_process_vma(process_id: ProcessId, vma: Vma) -> Result<(), VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    insert_vma(pml4_phys, vma)
}

pub fn remove_process_vma(process_id: ProcessId, start: usize) -> Result<Option<Vma>, VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    Ok(remove_vma(pml4_phys, start))
}

pub fn remove_process_vma_range(
    process_id: ProcessId,
    start: usize,
    end: usize,
) -> Result<alloc::vec::Vec<Vma>, VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    Ok(remove_vma_range(pml4_phys, start, end))
}

pub fn find_process_vma(process_id: ProcessId, addr: usize) -> Option<Vma> {
    let pml4_phys = registered_process_pml4(process_id).ok()?;
    find_vma(pml4_phys, addr)
}

pub fn find_process_free_region(
    process_id: ProcessId,
    low: usize,
    high: usize,
    size: usize,
) -> Result<Option<usize>, VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    Ok(find_free_region(pml4_phys, low, high, size))
}

pub fn set_process_permissions(
    process_id: ProcessId,
    start: usize,
    end: usize,
    perms: VmaPermissions,
) -> Result<(), VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    set_permissions(pml4_phys, start, end, perms)
}

pub fn account_process_unmap(process_id: ProcessId) -> Result<(), VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    account_unmap(pml4_phys);
    Ok(())
}

pub fn get_process_stats(process_id: ProcessId) -> Option<VmaStats> {
    let pml4_phys = registered_process_pml4(process_id).ok()?;
    get_stats(pml4_phys)
}

/// Create a new VmaMap for the given address space (PML4 phys addr)
pub fn create_vma_map(pml4_phys: usize) {
    let mut registry = VMA_REGISTRY.lock();
    registry.insert(pml4_phys, VmaMap::new());
    log_debug!(LOG_ORIGIN, "Created VMA map for PML4=0x{:X}", pml4_phys);
}

/// Destroy the VmaMap for the given address space
pub fn destroy_vma_map(pml4_phys: usize) {
    let mut registry = VMA_REGISTRY.lock();
    if registry.remove(&pml4_phys).is_some() {
        log_debug!(LOG_ORIGIN, "Destroyed VMA map for PML4=0x{:X}", pml4_phys);
    }
}

/// Insert a VMA into an address space's map
pub fn insert_vma(pml4_phys: usize, vma: Vma) -> Result<(), VmaError> {
    let mut registry = VMA_REGISTRY.lock();
    let map = registry.get_mut(&pml4_phys).ok_or(VmaError::NotFound)?;
    map.insert(vma)
}

/// Remove a VMA from an address space
pub fn remove_vma(pml4_phys: usize, start: usize) -> Option<Vma> {
    let mut registry = VMA_REGISTRY.lock();
    if let Some(map) = registry.get_mut(&pml4_phys) {
        map.remove(start)
    } else {
        None
    }
}

/// Remove all VMAs in a range
pub fn remove_vma_range(pml4_phys: usize, start: usize, end: usize) -> alloc::vec::Vec<Vma> {
    let mut registry = VMA_REGISTRY.lock();
    if let Some(map) = registry.get_mut(&pml4_phys) {
        map.remove_range(start, end)
    } else {
        alloc::vec::Vec::new()
    }
}

/// Find the VMA containing a given address
pub fn find_vma(pml4_phys: usize, addr: usize) -> Option<Vma> {
    let registry = VMA_REGISTRY.lock();
    let map = registry.get(&pml4_phys)?;
    map.find(addr).cloned()
}

/// Find a free virtual region in an address space
pub fn find_free_region(pml4_phys: usize, low: usize, high: usize, size: usize) -> Option<usize> {
    let registry = VMA_REGISTRY.lock();
    let map = registry.get(&pml4_phys)?;
    map.find_free_region(low, high, size, PAGE_SIZE)
}

/// Grow a stack VMA downward
pub fn grow_stack(pml4_phys: usize, vma_start: usize) -> Result<usize, VmaError> {
    let mut registry = VMA_REGISTRY.lock();
    let map = registry.get_mut(&pml4_phys).ok_or(VmaError::NotFound)?;
    map.grow_stack(vma_start)
}

/// Account for mapping a page
pub fn account_map(pml4_phys: usize) {
    let mut registry = VMA_REGISTRY.lock();
    if let Some(map) = registry.get_mut(&pml4_phys) {
        map.account_map();
    }
}

/// Account for unmapping a page
pub fn account_unmap(pml4_phys: usize) {
    let mut registry = VMA_REGISTRY.lock();
    if let Some(map) = registry.get_mut(&pml4_phys) {
        map.account_unmap();
    }
}

/// Get memory stats for an address space
pub fn get_stats(pml4_phys: usize) -> Option<VmaStats> {
    let registry = VMA_REGISTRY.lock();
    registry.get(&pml4_phys).map(|m| m.stats())
}

/// Check if a page can be mapped (under resident limit)
pub fn can_map_page(pml4_phys: usize) -> bool {
    let registry = VMA_REGISTRY.lock();
    registry.get(&pml4_phys).map(|m| m.can_map_page()).unwrap_or(false)
}

/// Update permissions on a VMA
pub fn set_permissions(pml4_phys: usize, start: usize, end: usize, perms: VmaPermissions) -> Result<(), VmaError> {
    let mut registry = VMA_REGISTRY.lock();
    let map = registry.get_mut(&pml4_phys).ok_or(VmaError::NotFound)?;
    map.set_permissions(start, end, perms)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

// ---------------------------------------------------------------------------
// Demand paging: page fault resolution
// ---------------------------------------------------------------------------

/// Attempt to resolve a user-space page fault via demand paging.
///
/// Returns `true` if the fault was resolved (page mapped, execution can resume).
/// Returns `false` if the fault is unresolvable (invalid address, permission
/// violation, out of memory, etc.).
///
/// The lock on `VMA_REGISTRY` is held only for the VMA lookup and validation
/// phase.  PMM allocation and page-table manipulation happen *outside* the
/// lock so that future changes to those subsystems cannot deadlock against
/// VMA_REGISTRY, and interrupt latency is kept low during bitmap scans.
///
/// # Arguments
/// * `pml4_phys` - The faulting process's PML4 physical address
/// * `fault_addr` - The virtual address that caused the fault (CR2)
/// * `error_code` - The x86_64 page fault error code
pub fn handle_page_fault(pml4_phys: usize, fault_addr: usize, error_code: u64) -> bool {
    let page_addr = fault_addr & !(PAGE_SIZE - 1);

    // Error code bits
    let present = error_code & 0x1 != 0;  // page was present
    let write = error_code & 0x2 != 0;    // write access
    let _user = error_code & 0x4 != 0;    // user-mode access
    let reserved = error_code & 0x8 != 0; // reserved bit set in PTE

    // Reserved bit faults are always hardware errors — never resolvable
    if reserved {
        return false;
    }

    // If the page was already present, this is a permission violation
    // (e.g., write to read-only). We don't handle COW yet.
    if present {
        return false;
    }

    // ── Phase 1: hold VMA_REGISTRY only for the lookup / validation ──
    let vma = {
        let mut registry = VMA_REGISTRY.lock();
        let map = match registry.get_mut(&pml4_phys) {
            Some(m) => m,
            None => return false,
        };

        // Find which VMA covers the fault address
        let vma = match map.find(page_addr) {
            Some(v) => v.clone(),
            None => {
                // Check if this is a stack growth fault:
                // Look for a stack VMA just above the fault address
                // (stacks grow downward, so the fault is below the current VMA start)
                let grew = try_grow_stack(map, fault_addr);
                if grew {
                    // Stack grew — now the VMA covers fault_addr.
                    match map.find(page_addr) {
                        Some(v) => v.clone(),
                        None => return false,
                    }
                } else {
                    return false;
                }
            }
        };

        // Validate permissions
        if write && !vma.perms.contains(VmaPermissions::WRITE) {
            log_debug!(LOG_ORIGIN, "PF denied: write to non-writable VMA at 0x{:X}", fault_addr);
            return false;
        }
        if !write && !vma.perms.contains(VmaPermissions::READ) {
            log_debug!(LOG_ORIGIN, "PF denied: read on non-readable VMA at 0x{:X}", fault_addr);
            return false;
        }

        // Validate backing type
        match vma.backing {
            VmaBacking::Anonymous | VmaBacking::Stack { .. } => {}
            VmaBacking::Device { .. } => {
                // Device mappings should already be mapped. If we get a fault,
                // something is wrong.
                log_warn!(LOG_ORIGIN, "PF on device VMA at 0x{:X} — unexpected", fault_addr);
                return false;
            }
        }

        // Check resident limit while we still hold the lock
        if !map.can_map_page() {
            log_warn!(LOG_ORIGIN, "Resident page limit reached for PML4=0x{:X}", pml4_phys);
            return false;
        }

        vma
        // ── lock dropped here ──
    };

    // ── Phase 2: allocate + map without holding VMA_REGISTRY ──
    resolve_anon_fault(pml4_phys, page_addr, &vma)
}

/// Resolve an anonymous (zero-fill) page fault.
///
/// Called *after* the VMA_REGISTRY lock has been released.  Re-acquires the
/// lock only briefly at the end to update resident-page accounting.
fn resolve_anon_fault(
    pml4_phys: usize,
    page_addr: usize,
    vma: &Vma,
) -> bool {
    use crate::mm::pmm;
    use crate::mm::vm::{self, PageFlags};

    // ── Guard: do NOT replace an already-present page ──────────────────
    // This can happen when handle_page_fault is called with a synthetic
    // error code (e.g. the pre-fault path in sys_fs_read passes
    // "not-present" even though the page may already be mapped).
    // Replacing a live page with a zeroed page would destroy stack data
    // (return addresses, local variables) and cause RIP=0x0 crashes.
    if vm::is_page_present_in_pml4(pml4_phys, page_addr) {
        return true; // already mapped — nothing to do
    }

    // ── Check per-process memory limits before allocation (Req 7.2, 7.3) ──
    if let Some(process_id) = crate::process::process_id_for_pml4(pml4_phys as u64) {
        if let Some(usage) = crate::process::get_process_memory_usage(process_id) {
            // Check hard limit (0 = unlimited)
            if usage.limit_pages > 0 && usage.resident_pages >= usage.limit_pages {
                log_warn!(
                    LOG_ORIGIN,
                    "Process {} memory hard limit exceeded: {} >= {} pages, denying allocation at 0x{:X}",
                    process_id,
                    usage.resident_pages,
                    usage.limit_pages,
                    page_addr
                );
                return false;
            }

            // Check soft limit (80% of hard limit) - warn but allow
            if usage.limit_pages > 0 {
                let soft_limit = (usage.limit_pages * 80) / 100;
                if usage.resident_pages >= soft_limit && usage.resident_pages < usage.limit_pages {
                    log_warn!(
                        LOG_ORIGIN,
                        "Process {} memory soft limit exceeded: {} >= {} pages (hard limit: {})",
                        process_id,
                        usage.resident_pages,
                        soft_limit,
                        usage.limit_pages
                    );
                }
            }
        }
    }

    // Allocate a zeroed physical page (no VMA lock held)
    let phys = match pmm::alloc_page_zeroed() {
        Some(p) => p,
        None => {
            log_warn!(LOG_ORIGIN, "OOM: cannot allocate page for demand fault at 0x{:X}", page_addr);
            return false;
        }
    };

    // Build page flags from VMA permissions
    let mut flags = PageFlags::PRESENT | PageFlags::USER;
    if vma.perms.contains(VmaPermissions::WRITE) {
        flags |= PageFlags::WRITABLE;
    }
    if !vma.perms.contains(VmaPermissions::EXEC) {
        flags |= PageFlags::NO_EXECUTE;
    }

    // Map the page into the process's address space (no VMA lock held)
    match vm::remap_page_in_pml4(pml4_phys, page_addr, phys, flags) {
        Ok(()) => {
            // ── Phase 3: briefly re-acquire lock for accounting ──
            {
                let mut registry = VMA_REGISTRY.lock();
                if let Some(map) = registry.get_mut(&pml4_phys) {
                    map.account_map();
                }
            }
            log_debug!(
                LOG_ORIGIN,
                "Demand-paged: virt=0x{:X} -> phys=0x{:X} ({})",
                page_addr,
                phys,
                vma.label
            );
            true
        }
        Err(e) => {
            // Failed to map — free the page we just allocated
            let _ = pmm::free_page(phys);
            log_warn!(
                LOG_ORIGIN,
                "Failed to map demand page at 0x{:X}: {:?}",
                page_addr,
                e
            );
            false
        }
    }
}

/// Try to grow a stack VMA to cover `fault_addr`.
/// Returns true if the stack was successfully grown.
fn try_grow_stack(map: &mut VmaMap, fault_addr: usize) -> bool {
    let page_addr = fault_addr & !(PAGE_SIZE - 1);

    // Look for a stack VMA whose start is just above the fault address.
    // Stacks grow downward, so the fault will be at an address just below
    // the current VMA start.
    //
    // We check VMAs within a reasonable window (one guard page = one page below).
    // The "real" guard page is the unmapped page at VMA.start - PAGE_SIZE.

    // Find candidate stack VMAs
    let mut candidate_start: Option<usize> = None;

    for (&start, vma) in map.regions.iter() {
        if let VmaBacking::Stack { .. } = vma.backing {
            // Check if fault_addr is in the growth window:
            // Between (start - PAGE_SIZE) and start (the guard page area)
            if page_addr < start && page_addr >= start.saturating_sub(PAGE_SIZE) {
                candidate_start = Some(start);
                break;
            }
        }
    }

    if let Some(old_start) = candidate_start {
        match map.grow_stack(old_start) {
            Ok(new_start) => {
                log_debug!(
                    LOG_ORIGIN,
                    "Stack grown: old_start=0x{:X} -> new_start=0x{:X}",
                    old_start,
                    new_start
                );
                true
            }
            Err(e) => {
                log_warn!(
                    LOG_ORIGIN,
                    "Stack growth failed at 0x{:X}: {:?}",
                    fault_addr,
                    e
                );
                false
            }
        }
    } else {
        false
    }
}

pub fn init() {
    log_info!(LOG_ORIGIN, "VMA subsystem initialized — demand paging ready");
}
