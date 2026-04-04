// Virtual Memory Area (VMA) Management
//
// Implements per-address-space tracking of virtual memory regions with
// support for demand paging and lazy allocation.
//
// Each VMA describes a contiguous virtual address range with uniform
// properties (permissions, backing type, etc.). VMAs are stored in a
// BTreeMap keyed by start address for O(log n) lookup and insertion.
//
// Key features:
// - Track virtual regions per address space (anon, file-backed, shared, device)
// - Support demand paging: reserve virtual ranges without physical backing
// - Stack regions with guard pages
// - Free virtual address allocation (find_free_region)
// - Per-process memory accounting (resident vs reserved)
//
// Design principles:
// - VMAs are metadata only; physical pages are allocated on demand via page faults
// - Guard pages are never mapped and trigger controlled faults

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessType {
    Read,
    Write,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtAddr(usize);

impl VirtAddr {
    pub const fn new(addr: usize) -> Self {
        Self(addr)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn page_base(self) -> usize {
        self.0 & !(PAGE_SIZE - 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultContext {
    pub addr: VirtAddr,
    pub access: AccessType,
    pub is_user: bool,
}

impl FaultContext {
    pub fn from_x86_error(addr: usize, error_code: u64) -> Self {
        // x86 page-fault decode:
        // bit 1 = write access
        // bit 2 = user access
        // bit 4 = instruction fetch
        let access = if (error_code & 0x10) != 0 {
            AccessType::Execute
        } else if (error_code & 0x2) != 0 {
            AccessType::Write
        } else {
            AccessType::Read
        };

        Self {
            addr: VirtAddr::new(addr),
            access,
            is_user: (error_code & 0x4) != 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultResult {
    Resolved,
    InvalidAddress,
    ProtectionViolation,
    OutOfMemory,
    NotHandled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedFault {
    NonPresent,
    Protection,
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

    fn permits(&self, access: AccessType) -> bool {
        match access {
            AccessType::Read => self.perms.contains(VmaPermissions::READ),
            AccessType::Write => self.perms.contains(VmaPermissions::WRITE),
            AccessType::Execute => self.perms.contains(VmaPermissions::EXEC),
        }
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
 
            let candidate_end = candidate.checked_add(size)?;
            if candidate_end <= vma.start {
                // Found a gap before this VMA
                return Some(candidate);
            }
 
            // Move past this VMA
            candidate = align_up(vma.end, align);
        }

        // Check if there's space after the last VMA
        let candidate_end = candidate.checked_add(size)?;
        if candidate_end <= high {
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
    if registry.contains_key(&pml4_phys) {
        log_warn!(
            LOG_ORIGIN,
            "[VMA_FAIL] reason=duplicate_map_create pml4=0x{:X}",
            pml4_phys
        );
        return;
    }
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
    log_debug!(
        LOG_ORIGIN,
        "[VMA_INSERT] pml4=0x{:X} start=0x{:X} end=0x{:X} perms=0x{:X} backing={:?} label={}",
        pml4_phys,
        vma.start,
        vma.end,
        vma.perms.bits(),
        vma.backing,
        vma.label
    );
    let result = map.insert(vma);
    match &result {
        Ok(()) => log_debug!(
            LOG_ORIGIN,
            "[VMA_INSERT] result=ok pml4=0x{:X} vma_count={}",
            pml4_phys,
            map.len()
        ),
        Err(err) => log_warn!(
            LOG_ORIGIN,
            "[VMA_FAIL] reason=insert_failed pml4=0x{:X} err={:?}",
            pml4_phys,
            err
        ),
    }
    result
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
    let map = match registry.get(&pml4_phys) {
        Some(map) => map,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "[VMA_LOOKUP] pml4=0x{:X} addr=0x{:X} result=no_registry",
                pml4_phys,
                addr
            );
            return None;
        }
    };
    let result = map.find(addr).cloned();
    match &result {
        Some(vma) => log_debug!(
            LOG_ORIGIN,
            "[VMA_LOOKUP] pml4=0x{:X} addr=0x{:X} result=hit start=0x{:X} end=0x{:X} perms=0x{:X} backing={:?} label={}",
            pml4_phys,
            addr,
            vma.start,
            vma.end,
            vma.perms.bits(),
            vma.backing,
            vma.label
        ),
        None => log_warn!(
            LOG_ORIGIN,
            "[VMA_LOOKUP] pml4=0x{:X} addr=0x{:X} result=miss vma_count={}",
            pml4_phys,
            addr,
            map.len()
        ),
    }
    result
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

fn classify_fault(error_code: u64) -> ClassifiedFault {
    // x86 PF error bit 0: 0 = non-present, 1 = protection violation.
    if (error_code & 0x1) != 0 {
        ClassifiedFault::Protection
    } else {
        ClassifiedFault::NonPresent
    }
}

fn admit_fault(ctx: &FaultContext, vma: &Vma) -> Result<(), FaultResult> {
    if vma.permits(ctx.access) {
        Ok(())
    } else {
        Err(FaultResult::ProtectionViolation)
    }
}

fn materialize_fault(
    pml4_phys: usize,
    ctx: &FaultContext,
    classified: ClassifiedFault,
    vma: &Vma,
) -> FaultResult {
    match classified {
        ClassifiedFault::Protection => FaultResult::ProtectionViolation,
        ClassifiedFault::NonPresent => match vma.backing {
            VmaBacking::Anonymous | VmaBacking::Stack { .. } => materialize_anon(pml4_phys, ctx, vma),
            _ => FaultResult::NotHandled,
        },
    }
}

fn materialize_anon(
    pml4_phys: usize,
    ctx: &FaultContext,
    vma: &Vma,
) -> FaultResult {
    use crate::mm::pmm;
    use crate::mm::vm::{self, PageFlags, VmError};

    let page_addr = ctx.addr.page_base();

    if vm::is_page_present_in_pml4(pml4_phys, page_addr) {
        log_debug!(
            LOG_ORIGIN,
            "[PF] materialize=anon pml4=0x{:X} page=0x{:X} result=already_present label={}",
            pml4_phys,
            page_addr,
            vma.label
        );
        return FaultResult::Resolved;
    }

    let phys = match pmm::alloc_page_zeroed() {
        Some(p) => p,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "[PF] materialize=anon pml4=0x{:X} page=0x{:X} result=oom label={}",
                pml4_phys,
                page_addr,
                vma.label
            );
            return FaultResult::OutOfMemory;
        }
    };

    let mut flags = PageFlags::PRESENT | PageFlags::USER;
    if vma.perms.contains(VmaPermissions::WRITE) {
        flags |= PageFlags::WRITABLE;
    }
    if !vma.perms.contains(VmaPermissions::EXEC) {
        flags |= PageFlags::NO_EXECUTE;
    }

    match vm::remap_page_in_pml4(pml4_phys, page_addr, phys, flags) {
        Ok(()) => {
            {
                let mut registry = VMA_REGISTRY.lock();
                if let Some(map) = registry.get_mut(&pml4_phys) {
                    map.account_map();
                }
            }

            log_debug!(
                LOG_ORIGIN,
                "[PF] materialize=anon pml4=0x{:X} page=0x{:X} phys=0x{:X} access={:?} user={} label={} result=mapped",
                pml4_phys,
                page_addr,
                phys,
                ctx.access,
                ctx.is_user,
                vma.label
            );
            FaultResult::Resolved
        }
        Err(err) => {
            let _ = pmm::free_page(phys);
            let result = if matches!(err, VmError::OutOfMemory) {
                FaultResult::OutOfMemory
            } else {
                FaultResult::NotHandled
            };
            log_warn!(
                LOG_ORIGIN,
                "[PF] materialize=anon pml4=0x{:X} page=0x{:X} phys=0x{:X} result={:?} err={:?} label={}",
                pml4_phys,
                page_addr,
                phys,
                result,
                err,
                vma.label
            );
            result
        }
    }
}

/// Attempt to resolve a page fault with a deterministic pipeline:
/// classify -> admit -> materialize.
pub fn handle_page_fault(
    pml4_phys: usize,
    ctx: FaultContext,
    error_code: u64,
) -> FaultResult {
    let classified = classify_fault(error_code);
    let page_addr = ctx.addr.page_base();

    log_debug!(
        LOG_ORIGIN,
        "[PF] classify={:?} addr=0x{:X} access={:?} user={} err={:#X} pml4=0x{:X}",
        classified,
        ctx.addr.as_usize(),
        ctx.access,
        ctx.is_user,
        error_code,
        pml4_phys
    );

    // Bit 3 indicates reserved-bit violation in x86 page-fault errors.
    if (error_code & 0x8) != 0 {
        log_warn!(
            LOG_ORIGIN,
            "[PF] classify={:?} addr=0x{:X} result=not_handled reason=reserved_bit pml4=0x{:X}",
            classified,
            ctx.addr.as_usize(),
            pml4_phys
        );
        return FaultResult::NotHandled;
    }

    let vma = {
        let registry = VMA_REGISTRY.lock();
        let map = match registry.get(&pml4_phys) {
            Some(map) => map,
            None => {
                log_warn!(
                    LOG_ORIGIN,
                    "[PF] vma_hit=false addr=0x{:X} page=0x{:X} pml4=0x{:X} reason=no_registry",
                    ctx.addr.as_usize(),
                    page_addr,
                    pml4_phys
                );
                return FaultResult::InvalidAddress;
            }
        };

        match map.find(page_addr) {
            Some(vma) => {
                log_debug!(
                    LOG_ORIGIN,
                    "[PF] vma_hit=true addr=0x{:X} page=0x{:X} start=0x{:X} end=0x{:X} perms=0x{:X} backing={:?} label={}",
                    ctx.addr.as_usize(),
                    page_addr,
                    vma.start,
                    vma.end,
                    vma.perms.bits(),
                    vma.backing,
                    vma.label
                );
                vma.clone()
            }
            None => {
                log_warn!(
                    LOG_ORIGIN,
                    "[PF] vma_hit=false addr=0x{:X} page=0x{:X} pml4=0x{:X} reason=miss vma_count={}",
                    ctx.addr.as_usize(),
                    page_addr,
                    pml4_phys,
                    map.len()
                );
                return FaultResult::InvalidAddress;
            }
        }
    };

    if let Err(result) = admit_fault(&ctx, &vma) {
        log_warn!(
            LOG_ORIGIN,
            "[PF] admit=deny addr=0x{:X} access={:?} perms=0x{:X} result={:?} label={}",
            ctx.addr.as_usize(),
            ctx.access,
            vma.perms.bits(),
            result,
            vma.label
        );
        return result;
    }

    let result = materialize_fault(pml4_phys, &ctx, classified, &vma);
    log_debug!(
        LOG_ORIGIN,
        "[PF] result={:?} addr=0x{:X} page=0x{:X} access={:?} user={} label={}",
        result,
        ctx.addr.as_usize(),
        page_addr,
        ctx.access,
        ctx.is_user,
        vma.label
    );
    result
}

pub fn init() {
    log_info!(LOG_ORIGIN, "VMA subsystem initialized — demand paging ready");
}
