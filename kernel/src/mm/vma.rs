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

#[inline]
fn page_account_counts_as_shared(account: &PageAccount) -> bool {
    // Shared-accounting policy:
    // - Explicit shared mappings are always charged as shared.
    // - COW pages are shared while still in CowShared state.
    matches!(account.source, PageSource::Shared)
        || (matches!(account.source, PageSource::Cow)
            && matches!(account.share_state, PageShareState::CowShared))
}

#[inline]
fn page_account_charge(account: &PageAccount) -> Option<(ProcessId, usize, usize)> {
    let process_id = account.owner_process?;
    if page_account_counts_as_shared(account) {
        Some((process_id, 0, 1))
    } else {
        Some((process_id, 1, 0))
    }
}

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
pub struct AddressSpaceId(usize);

impl AddressSpaceId {
    pub const fn new(address_space: usize) -> Self {
        Self(address_space)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultContext {
    pub pid: ProcessId,
    pub address_space_id: AddressSpaceId,
    pub fault_addr: VirtAddr,
    pub access_type: AccessType,
    pub is_user: bool,
}

impl FaultContext {
    pub fn from_x86_error(
        pid: ProcessId,
        address_space_id: AddressSpaceId,
        fault_addr: usize,
        error_code: u64,
    ) -> Self {
        // x86 page-fault decode:
        // bit 1 = write access
        // bit 2 = user access
        // bit 4 = instruction fetch
        let access_type = if (error_code & 0x10) != 0 {
            AccessType::Execute
        } else if (error_code & 0x2) != 0 {
            AccessType::Write
        } else {
            AccessType::Read
        };

        Self {
            pid,
            address_space_id,
            fault_addr: VirtAddr::new(fault_addr),
            access_type,
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
pub enum PageSource {
    Anonymous,
    FileBacked,
    Shared,
    Cow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageShareState {
    Exclusive,
    CowShared,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageAccount {
    pub owner_process: Option<ProcessId>,
    pub owner_address_space: usize,
    pub vaddr: VirtAddr,
    pub paddr: usize,
    pub source: PageSource,
    pub share_state: PageShareState,
    pub vma_start: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClassifiedFault {
    InvalidAddress,
    AccessDenied,
    NotPresentAnon,
    CowWrite,
    NotHandled,
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
    /// Materialized (resident) pages keyed by virtual page base
    materialized_pages: BTreeMap<usize, PageAccount>,
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
            materialized_pages: BTreeMap::new(),
            reserved_bytes: 0,
            resident_pages: 0,
            resident_limit: 0,
        }
    }

    fn assert_no_overlap(&self, start: usize, end: usize) -> Result<(), VmaError> {
        if let Some((_, existing)) = self.regions.range(..=start).next_back() {
            if existing.overlaps(start, end) {
                log_warn!(
                    LOG_ORIGIN,
                    "[VMA_ERROR] overlap detected: new=0x{:X}-0x{:X} existing=0x{:X}-0x{:X} label={}",
                    start,
                    end,
                    existing.start,
                    existing.end,
                    existing.label
                );
                return Err(VmaError::Overlap);
            }
        }

        if let Some((_, existing)) = self.regions.range(start..).next() {
            if existing.overlaps(start, end) {
                log_warn!(
                    LOG_ORIGIN,
                    "[VMA_ERROR] overlap detected: new=0x{:X}-0x{:X} existing=0x{:X}-0x{:X} label={}",
                    start,
                    end,
                    existing.start,
                    existing.end,
                    existing.label
                );
                return Err(VmaError::Overlap);
            }
        }

        Ok(())
    }

    fn overlaps_existing(&self, start: usize, end: usize) -> bool {
        self.assert_no_overlap(start, end).is_err()
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

        self.assert_no_overlap(vma.start, vma.end)?;

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

    fn track_page(
        &mut self,
        owner_process: Option<ProcessId>,
        pml4_phys: usize,
        vma: &Vma,
        vaddr: usize,
        paddr: usize,
        source: PageSource,
    ) -> Result<(), VmaError> {
        if !vaddr.is_multiple_of(PAGE_SIZE) || !paddr.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::Unaligned);
        }
        if !vma.contains_addr(vaddr) {
            return Err(VmaError::InvalidRange);
        }
        if self.materialized_pages.contains_key(&vaddr) {
            return Err(VmaError::Overlap);
        }

        let account = PageAccount {
            owner_process,
            owner_address_space: pml4_phys,
            vaddr: VirtAddr::new(vaddr),
            paddr,
            source,
            share_state: PageShareState::Exclusive,
            vma_start: vma.start,
        };

        self.materialized_pages.insert(vaddr, account);
        self.account_map();

        log_debug!(
            LOG_ORIGIN,
            "[PAGE_ALLOC] pid={:?} asid=0x{:X} vaddr=0x{:X} paddr=0x{:X} source={:?}",
            owner_process,
            pml4_phys,
            vaddr,
            paddr,
            source
        );

        Ok(())
    }

    fn untrack_page(&mut self, vaddr: usize) -> Option<PageAccount> {
        let account = self.materialized_pages.remove(&vaddr);
        if account.is_some() {
            self.account_unmap();
        }
        account
    }

    fn drain_materialized_pages(&mut self) -> alloc::vec::Vec<PageAccount> {
        let keys: alloc::vec::Vec<usize> = self.materialized_pages.keys().copied().collect();
        let mut pages = alloc::vec::Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(account) = self.materialized_pages.remove(&key) {
                self.account_unmap();
                pages.push(account);
            }
        }
        pages
    }

    fn get_page_account(&self, vaddr: usize) -> Option<PageAccount> {
        self.materialized_pages.get(&vaddr).copied()
    }

    fn upsert_page_account(&mut self, account: PageAccount) -> Result<Option<PageAccount>, VmaError> {
        let vaddr = account.vaddr.as_usize();
        if !vaddr.is_multiple_of(PAGE_SIZE) || !account.paddr.is_multiple_of(PAGE_SIZE) {
            return Err(VmaError::Unaligned);
        }
        let Some(vma) = self.find(vaddr) else {
            return Err(VmaError::NotFound);
        };
        if vma.start != account.vma_start {
            return Err(VmaError::InvalidRange);
        }

        let previous = self.materialized_pages.insert(vaddr, account);
        if previous.is_none() {
            self.account_map();
        }

        Ok(previous)
    }

    fn list_vmas(&self) -> alloc::vec::Vec<Vma> {
        self.regions.values().cloned().collect()
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

/// Slow-path ground truth for process accounting, used by drift verification.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessVmaAccountingObservation {
    pub map_present: bool,
    pub resident_private_pages: usize,
    pub resident_shared_pages: usize,
    pub reserved_bytes: usize,
    pub owner_mismatch_pages: usize,
    pub owner_missing_pages: usize,
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
/// Coarse-grained address-space operation lock buckets.
///
/// This serializes fault materialization, fork COW marking and teardown-facing
/// VMA/PTE transitions while the kernel has no per-ASID lock table yet.
/// Lock hierarchy:
///   address-space op lock -> VM page-table lock -> PMM/refcount lock.
const ADDRESS_SPACE_LOCK_SLOTS: usize = 64;
static ADDRESS_SPACE_OP_LOCKS: [Mutex<()>; ADDRESS_SPACE_LOCK_SLOTS] =
    [const { Mutex::new(()) }; ADDRESS_SPACE_LOCK_SLOTS];

#[inline]
fn address_space_lock_slot(pml4_phys: usize) -> usize {
    // Lock striping by address-space root. Collisions serialize unrelated
    // spaces but preserve correctness and deterministic ordering.
    (pml4_phys >> 12) & (ADDRESS_SPACE_LOCK_SLOTS - 1)
}

pub fn with_address_space_ops_lock<R>(pml4_phys: usize, f: impl FnOnce() -> R) -> R {
    let _guard = ADDRESS_SPACE_OP_LOCKS[address_space_lock_slot(pml4_phys)].lock();
    f()
}

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
    crate::process::reset_process_memory_accounting(process_id);
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

pub fn process_range_overlaps(
    process_id: ProcessId,
    start: usize,
    end: usize,
) -> Result<bool, VmaError> {
    let pml4_phys = registered_process_pml4(process_id)?;
    range_overlaps_existing(pml4_phys, start, end)
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
    crate::process::account_process_resident_pages_sub(process_id, 1, 0);
    Ok(())
}

pub fn get_process_stats(process_id: ProcessId) -> Option<VmaStats> {
    let pml4_phys = registered_process_pml4(process_id).ok()?;
    get_stats(pml4_phys)
}

/// Recalculate per-process accounting from VMA structures (slow-path verifier).
pub fn recalculate_process_vma_accounting(
    process_id: ProcessId,
) -> ProcessVmaAccountingObservation {
    let pml4_phys = match registered_process_pml4(process_id) {
        Ok(pml4) => pml4,
        Err(_) => return ProcessVmaAccountingObservation::default(),
    };

    let registry = VMA_REGISTRY.lock();
    let Some(map) = registry.get(&pml4_phys) else {
        return ProcessVmaAccountingObservation::default();
    };

    let mut observed = ProcessVmaAccountingObservation {
        map_present: true,
        reserved_bytes: map.reserved_bytes,
        ..ProcessVmaAccountingObservation::default()
    };

    for account in map.materialized_pages.values() {
        match account.owner_process {
            Some(owner) if owner == process_id => {}
            Some(_) => observed.owner_mismatch_pages = observed.owner_mismatch_pages.saturating_add(1),
            None => observed.owner_missing_pages = observed.owner_missing_pages.saturating_add(1),
        }

        if page_account_counts_as_shared(account) {
            observed.resident_shared_pages = observed.resident_shared_pages.saturating_add(1);
        } else {
            observed.resident_private_pages = observed.resident_private_pages.saturating_add(1);
        }
    }

    observed
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
    let owner_process = crate::process::process_id_for_pml4(pml4_phys as u64);
    let reserved_bytes = vma.size();
    let result = {
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
    };

    if result.is_ok() {
        if let Some(process_id) = owner_process {
            crate::process::account_process_reserved_bytes_add(process_id, reserved_bytes);
        }
    }

    result
}

/// Remove a VMA from an address space
pub fn remove_vma(pml4_phys: usize, start: usize) -> Option<Vma> {
    let owner_process = crate::process::process_id_for_pml4(pml4_phys as u64);
    let removed = {
        let mut registry = VMA_REGISTRY.lock();
        if let Some(map) = registry.get_mut(&pml4_phys) {
            map.remove(start)
        } else {
            None
        }
    };

    if let Some(vma) = &removed {
        if let Some(process_id) = owner_process {
            crate::process::account_process_reserved_bytes_sub(process_id, vma.size());
        }
    }

    removed
}

/// Remove all VMAs in a range
pub fn remove_vma_range(pml4_phys: usize, start: usize, end: usize) -> alloc::vec::Vec<Vma> {
    let owner_process = crate::process::process_id_for_pml4(pml4_phys as u64);
    let removed = {
        let mut registry = VMA_REGISTRY.lock();
        if let Some(map) = registry.get_mut(&pml4_phys) {
            map.remove_range(start, end)
        } else {
            alloc::vec::Vec::new()
        }
    };

    if let Some(process_id) = owner_process {
        let mut removed_bytes = 0usize;
        for vma in &removed {
            removed_bytes = removed_bytes.saturating_add(vma.size());
        }
        crate::process::account_process_reserved_bytes_sub(process_id, removed_bytes);
    }

    removed
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

pub fn list_vmas(pml4_phys: usize) -> alloc::vec::Vec<Vma> {
    let registry = VMA_REGISTRY.lock();
    match registry.get(&pml4_phys) {
        Some(map) => map.list_vmas(),
        None => alloc::vec::Vec::new(),
    }
}

/// Find a free virtual region in an address space
pub fn find_free_region(pml4_phys: usize, low: usize, high: usize, size: usize) -> Option<usize> {
    let registry = VMA_REGISTRY.lock();
    let map = registry.get(&pml4_phys)?;
    map.find_free_region(low, high, size, PAGE_SIZE)
}

pub fn range_overlaps_existing(pml4_phys: usize, start: usize, end: usize) -> Result<bool, VmaError> {
    let registry = VMA_REGISTRY.lock();
    let map = registry.get(&pml4_phys).ok_or(VmaError::NotFound)?;
    Ok(map.overlaps_existing(start, end))
}

/// Grow a stack VMA downward
pub fn grow_stack(pml4_phys: usize, vma_start: usize) -> Result<usize, VmaError> {
    let owner_process = crate::process::process_id_for_pml4(pml4_phys as u64);
    let result = {
        let mut registry = VMA_REGISTRY.lock();
        let map = registry.get_mut(&pml4_phys).ok_or(VmaError::NotFound)?;
        map.grow_stack(vma_start)
    };

    if result.is_ok() {
        if let Some(process_id) = owner_process {
            crate::process::account_process_reserved_bytes_add(process_id, PAGE_SIZE);
        }
    }

    result
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

pub fn take_materialized_page(pml4_phys: usize, vaddr: usize) -> Option<PageAccount> {
    let removed = {
        let mut registry = VMA_REGISTRY.lock();
        let map = registry.get_mut(&pml4_phys)?;
        map.untrack_page(vaddr)
    };

    if let Some(account) = removed {
        if let Some((process_id, private_pages, shared_pages)) = page_account_charge(&account) {
            crate::process::account_process_resident_pages_sub(
                process_id,
                private_pages,
                shared_pages,
            );
        }
        Some(account)
    } else {
        None
    }
}

pub fn get_materialized_page(pml4_phys: usize, vaddr: usize) -> Option<PageAccount> {
    let registry = VMA_REGISTRY.lock();
    let map = registry.get(&pml4_phys)?;
    map.get_page_account(vaddr)
}

pub fn upsert_materialized_page(
    pml4_phys: usize,
    account: PageAccount,
) -> Result<(), VmaError> {
    let old_account = {
        let mut registry = VMA_REGISTRY.lock();
        let map = registry.get_mut(&pml4_phys).ok_or(VmaError::NotFound)?;
        map.upsert_page_account(account)?
    };

    let old_charge = old_account.as_ref().and_then(page_account_charge);
    let new_charge = page_account_charge(&account);

    if let Some((process_id, private_pages, shared_pages)) = old_charge {
        crate::process::account_process_resident_pages_sub(process_id, private_pages, shared_pages);
    }
    if let Some((process_id, private_pages, shared_pages)) = new_charge {
        crate::process::account_process_resident_pages_add(process_id, private_pages, shared_pages);
    }

    Ok(())
}

/// Register a pre-mapped range as materialized pages for process accounting.
///
/// Intended for eager mappings (exec image/bootstrap stack) where pages are
/// mapped before fault-driven tracking is engaged.
pub fn account_pre_mapped_range(
    process_id: ProcessId,
    pml4_phys: usize,
    start: usize,
    end: usize,
    source: PageSource,
) -> Result<usize, VmaError> {
    use crate::mm::vm;

    if !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) || start >= end {
        return Err(VmaError::InvalidRange);
    }

    let mut tracked_pages = 0usize;
    let mut page = start;
    while page < end {
        let (paddr, _flags) = vm::query_mapping_in_pml4(pml4_phys, page)
            .map_err(|_| VmaError::NotFound)?;

        let account = PageAccount {
            owner_process: Some(process_id),
            owner_address_space: pml4_phys,
            vaddr: VirtAddr::new(page),
            paddr,
            source,
            share_state: PageShareState::Exclusive,
            vma_start: start,
        };
        upsert_materialized_page(pml4_phys, account)?;

        tracked_pages = tracked_pages.saturating_add(1);
        page = page.saturating_add(PAGE_SIZE);
    }

    Ok(tracked_pages)
}

pub fn clone_vmas_for_fork(
    parent_pml4_phys: usize,
    child_pml4_phys: usize,
    parent_pid: ProcessId,
    child_pid: ProcessId,
) -> Result<usize, VmaError> {
    let (parent_regions, reserved_bytes_added) = {
        let mut registry = VMA_REGISTRY.lock();
        let parent_map = registry.get(&parent_pml4_phys).ok_or(VmaError::NotFound)?;
        let parent_regions = parent_map.list_vmas();
        let child_map = registry
            .get_mut(&child_pml4_phys)
            .ok_or(VmaError::NotFound)?;

        let mut reserved_bytes_added = 0usize;
        for region in parent_regions.iter().cloned() {
            reserved_bytes_added = reserved_bytes_added.saturating_add(region.size());
            child_map.insert(region)?;
        }

        (parent_regions, reserved_bytes_added)
    };

    if reserved_bytes_added > 0 {
        crate::process::account_process_reserved_bytes_add(child_pid, reserved_bytes_added);
    }

    log_info!(
        LOG_ORIGIN,
        "[FORK] parent_pid={} child_pid={} parent_cr3=0x{:X} child_cr3=0x{:X}",
        parent_pid,
        child_pid,
        parent_pml4_phys,
        child_pml4_phys
    );

    for vma in &parent_regions {
        log_debug!(
            LOG_ORIGIN,
            "[FORK_VMA] child_pid={} start=0x{:X} end=0x{:X} prot=0x{:X} label={}",
            child_pid,
            vma.start,
            vma.end,
            vma.perms.bits(),
            vma.label
        );
    }

    Ok(parent_regions.len())
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ForkCloneStats {
    pub scanned_pages: usize,
    pub shared_pages: usize,
    pub cow_pages: usize,
    pub readonly_shared_pages: usize,
}

#[inline]
fn map_vm_error(err: crate::mm::vm::VmError) -> VmaError {
    match err {
        crate::mm::vm::VmError::OutOfMemory => VmaError::OutOfMemory,
        _ => VmaError::InvalidRange,
    }
}

#[inline]
fn is_private_cow_eligible_vma(vma: &Vma) -> bool {
    matches!(vma.backing, VmaBacking::Anonymous | VmaBacking::Stack { .. })
}

pub fn clone_cow_private_mappings_for_fork(
    parent_pml4_phys: usize,
    child_pml4_phys: usize,
    parent_pid: ProcessId,
    child_pid: ProcessId,
) -> Result<ForkCloneStats, VmaError> {
    use crate::mm::pmm;
    use crate::mm::vm::{self, PageFlags, VmError};

    let parent_vmas = list_vmas(parent_pml4_phys);
    let mut stats = ForkCloneStats::default();

    for vma in parent_vmas {
        if !is_private_cow_eligible_vma(&vma) {
            continue;
        }

        let mut page = vma.start;
        while page < vma.end {
            stats.scanned_pages += 1;

            let (paddr, flags) = match vm::query_mapping_in_pml4(parent_pml4_phys, page) {
                Ok(v) => v,
                Err(VmError::NotMapped) => {
                    page = page.saturating_add(PAGE_SIZE);
                    continue;
                }
                Err(err) => return Err(map_vm_error(err)),
            };

            if !flags.contains(PageFlags::USER) {
                page = page.saturating_add(PAGE_SIZE);
                continue;
            }

            let logical_writable = vma.perms.contains(VmaPermissions::WRITE);
            let (parent_new_flags, child_flags, source) = if logical_writable {
                let cow_flags = vm::set_cow(flags);
                (cow_flags, cow_flags, PageSource::Cow)
            } else {
                let ro_flags = vm::clear_cow(flags).without(PageFlags::WRITABLE);
                (ro_flags, ro_flags, PageSource::Shared)
            };

            pmm::phys_ref_inc(paddr).map_err(|_| VmaError::InvalidRange)?;

            if let Err(err) = vm::remap_page_in_pml4(child_pml4_phys, page, paddr, child_flags) {
                let _ = pmm::phys_ref_dec(paddr);
                return Err(map_vm_error(err));
            }

            if parent_new_flags.bits() != flags.bits() {
                if let Err(err) =
                    vm::remap_page_flags_in_pml4(parent_pml4_phys, page, parent_new_flags)
                {
                    let _ = vm::unmap_page_in_pml4(child_pml4_phys, page);
                    let _ = pmm::phys_ref_dec(paddr);
                    return Err(map_vm_error(err));
                }
            }

            let current_ref = pmm::phys_ref_get(paddr);
            stats.shared_pages += 1;
            if logical_writable {
                stats.cow_pages += 1;
            } else {
                stats.readonly_shared_pages += 1;
            }

            let parent_account = PageAccount {
                owner_process: Some(parent_pid),
                owner_address_space: parent_pml4_phys,
                vaddr: VirtAddr::new(page),
                paddr,
                source,
                share_state: PageShareState::CowShared,
                vma_start: vma.start,
            };
            upsert_materialized_page(parent_pml4_phys, parent_account)?;

            let child_account = PageAccount {
                owner_process: Some(child_pid),
                owner_address_space: child_pml4_phys,
                vaddr: VirtAddr::new(page),
                paddr,
                source,
                share_state: PageShareState::CowShared,
                vma_start: vma.start,
            };
            upsert_materialized_page(child_pml4_phys, child_account)?;

            log_info!(
                LOG_ORIGIN,
                "[FORK_COW] parent_pid={} child_pid={} vaddr=0x{:X} paddr=0x{:X} refcount={}",
                parent_pid,
                child_pid,
                page,
                paddr,
                current_ref
            );
            log_debug!(
                LOG_ORIGIN,
                "[PAGE_SHARE] parent_pid={} child_pid={} parent_asid=0x{:X} child_asid=0x{:X} vaddr=0x{:X} paddr=0x{:X} source={:?} share={:?}",
                parent_pid,
                child_pid,
                parent_pml4_phys,
                child_pml4_phys,
                page,
                paddr,
                source,
                PageShareState::CowShared
            );

            page = page.saturating_add(PAGE_SIZE);
        }
    }

    Ok(stats)
}

pub fn drain_materialized_pages(pml4_phys: usize) -> alloc::vec::Vec<PageAccount> {
    let mut resident_subtractions: BTreeMap<ProcessId, (usize, usize)> = BTreeMap::new();
    let pages = {
        let mut registry = VMA_REGISTRY.lock();
        match registry.get_mut(&pml4_phys) {
            Some(map) => map.drain_materialized_pages(),
            None => alloc::vec::Vec::new(),
        }
    };

    for account in &pages {
        let Some((process_id, private_pages, shared_pages)) = page_account_charge(account) else {
            continue;
        };
        let entry = resident_subtractions.entry(process_id).or_insert((0, 0));
        entry.0 = entry.0.saturating_add(private_pages);
        entry.1 = entry.1.saturating_add(shared_pages);
    }

    for (process_id, (private_pages, shared_pages)) in resident_subtractions {
        crate::process::account_process_resident_pages_sub(
            process_id,
            private_pages,
            shared_pages,
        );
    }

    pages
}

pub fn free_page_account(account: PageAccount) -> bool {
    let should_free = matches!(
        account.source,
        PageSource::Anonymous | PageSource::Cow | PageSource::Shared
    );
    let mut freed = false;
    if should_free {
        freed = crate::mm::pmm::free_page(account.paddr).is_ok();
        if freed {
            crate::thread::record_physical_pages_freed(1);
        } else {
            log_warn!(
                LOG_ORIGIN,
                "[PAGE_FREE_FAIL] pid={:?} asid=0x{:X} vaddr=0x{:X} paddr=0x{:X} source={:?}",
                account.owner_process,
                account.owner_address_space,
                account.vaddr.as_usize(),
                account.paddr,
                account.source
            );
        }
    }

    freed
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

fn classify_fault(
    ctx: &FaultContext,
    error_code: u64,
    vma: Option<&Vma>,
) -> ClassifiedFault {
    use crate::mm::vm::{self, PageFlags};

    let address_space_id = ctx.address_space_id.as_usize();

    // x86 PF error bit 3: reserved-bit violation
    if (error_code & 0x8) != 0 {
        return ClassifiedFault::NotHandled;
    }

    let Some(vma) = vma else {
        return ClassifiedFault::InvalidAddress;
    };

    if !vma.permits(ctx.access_type) {
        return ClassifiedFault::AccessDenied;
    }

    let present_fault = (error_code & 0x1) != 0;
    let write_fault = matches!(ctx.access_type, AccessType::Write);

    if present_fault && write_fault {
        let page_addr = ctx.fault_addr.page_base();
        if let Ok((_phys, flags)) = vm::query_mapping_in_pml4(address_space_id, page_addr) {
            if vm::is_cow(flags) && !flags.contains(PageFlags::WRITABLE) {
                return ClassifiedFault::CowWrite;
            }
        }
        return ClassifiedFault::AccessDenied;
    }

    if !present_fault {
        match vma.backing {
            VmaBacking::Anonymous | VmaBacking::Stack { .. } => ClassifiedFault::NotPresentAnon,
            _ => ClassifiedFault::NotHandled,
        }
    } else {
        ClassifiedFault::AccessDenied
    }
}

fn admit_fault(
    ctx: &FaultContext,
    vma: Option<&Vma>,
    classified: ClassifiedFault,
) -> Result<(), FaultResult> {
    let Some(vma) = vma else {
        return Err(FaultResult::InvalidAddress);
    };

    match classified {
        ClassifiedFault::InvalidAddress => Err(FaultResult::InvalidAddress),
        ClassifiedFault::AccessDenied => Err(FaultResult::ProtectionViolation),
        ClassifiedFault::NotHandled => Err(FaultResult::NotHandled),
        ClassifiedFault::NotPresentAnon => {
            if vma.permits(ctx.access_type) {
                log_debug!(
                    LOG_ORIGIN,
                    "[PF] admit=ok addr=0x{:X} access={:?} perms=0x{:X} label={}",
                    ctx.fault_addr.as_usize(),
                    ctx.access_type,
                    vma.perms.bits(),
                    vma.label
                );
                Ok(())
            } else {
                Err(FaultResult::ProtectionViolation)
            }
        }
        ClassifiedFault::CowWrite => {
            if vma.perms.contains(VmaPermissions::WRITE) {
                log_debug!(
                    LOG_ORIGIN,
                    "[PF] admit=ok addr=0x{:X} access={:?} perms=0x{:X} label={}",
                    ctx.fault_addr.as_usize(),
                    ctx.access_type,
                    vma.perms.bits(),
                    vma.label
                );
                Ok(())
            } else {
                Err(FaultResult::ProtectionViolation)
            }
        }
    }
}

fn classify_fault_type_for_admission(classified: ClassifiedFault) -> crate::mm::admission::FaultType {
    match classified {
        ClassifiedFault::NotPresentAnon => crate::mm::admission::FaultType::NotPresentAnonymous,
        ClassifiedFault::CowWrite => crate::mm::admission::FaultType::CowWrite,
        ClassifiedFault::InvalidAddress
        | ClassifiedFault::AccessDenied
        | ClassifiedFault::NotHandled => crate::mm::admission::FaultType::Invalid,
    }
}

fn evaluate_memory_admission(
    ctx: &FaultContext,
    classified: ClassifiedFault,
) -> Result<(), FaultResult> {
    use crate::mm::admission::{AdmissionDecision, MemoryAdmission, MemoryAdmissionContext};

    let admission_ctx = MemoryAdmissionContext {
        pid: ctx.pid,
        requested_pages: 1,
        fault_type: classify_fault_type_for_admission(classified),
    };

    let decision = MemoryAdmission::evaluate(admission_ctx);
    match decision {
        AdmissionDecision::Allow => {
            log_debug!(
                LOG_ORIGIN,
                "[PF] admission=allow pid={} addr=0x{:X} access={:?} decision={:?}",
                ctx.pid,
                ctx.fault_addr.as_usize(),
                ctx.access_type,
                decision
            );
            Ok(())
        }
        AdmissionDecision::DenyProcessLimit | AdmissionDecision::DenyGlobalPressure => {
            log_warn!(
                LOG_ORIGIN,
                "[PF] admission=deny pid={} addr=0x{:X} access={:?} decision={:?} action=fatal",
                ctx.pid,
                ctx.fault_addr.as_usize(),
                ctx.access_type,
                decision
            );
            Err(FaultResult::OutOfMemory)
        }
        AdmissionDecision::TriggerOOM => {
            log_warn!(
                LOG_ORIGIN,
                "[PF] admission=deny pid={} addr=0x{:X} access={:?} decision={:?} action=trigger_oom_then_fatal",
                ctx.pid,
                ctx.fault_addr.as_usize(),
                ctx.access_type,
                decision
            );
            MemoryAdmission::trigger_oom(&admission_ctx);
            Err(FaultResult::OutOfMemory)
        }
        AdmissionDecision::InvalidPolicy => {
            log_warn!(
                LOG_ORIGIN,
                "[PF] admission=deny pid={} addr=0x{:X} access={:?} decision={:?} action=fatal_invalid",
                ctx.pid,
                ctx.fault_addr.as_usize(),
                ctx.access_type,
                decision
            );
            Err(FaultResult::InvalidAddress)
        }
    }
}

fn materialize_fault(
    ctx: &FaultContext,
    classified: ClassifiedFault,
    vma: &Vma,
) -> FaultResult {
    match classified {
        ClassifiedFault::InvalidAddress => FaultResult::InvalidAddress,
        ClassifiedFault::AccessDenied => FaultResult::ProtectionViolation,
        ClassifiedFault::NotHandled => FaultResult::NotHandled,
        ClassifiedFault::NotPresentAnon => materialize_anon(ctx, vma),
        ClassifiedFault::CowWrite => materialize_cow(ctx, vma),
    }
}

fn materialize_anon(
    ctx: &FaultContext,
    vma: &Vma,
) -> FaultResult {
    use crate::mm::pmm;
    use crate::mm::vm::{self, PageFlags, VmError};

    let pml4_phys = ctx.address_space_id.as_usize();
    let page_addr = ctx.fault_addr.page_base();
    let owner_process = Some(ctx.pid);
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

    with_address_space_ops_lock(pml4_phys, || {
        if vm::is_page_present_in_pml4(pml4_phys, page_addr) {
            let _ = pmm::free_page(phys);
            log_debug!(
                LOG_ORIGIN,
                "[PF] materialize=anon pml4=0x{:X} page=0x{:X} result=already_present label={}",
                pml4_phys,
                page_addr,
                vma.label
            );
            return FaultResult::Resolved;
        }

        let mut flags = PageFlags::PRESENT | PageFlags::USER;
        if vma.perms.contains(VmaPermissions::WRITE) {
            flags |= PageFlags::WRITABLE;
        }
        if !vma.perms.contains(VmaPermissions::EXEC) {
            flags |= PageFlags::NO_EXECUTE;
        }

        match vm::remap_page_in_pml4(pml4_phys, page_addr, phys, flags) {
            Ok(()) => {
                let tracked = {
                    let mut registry = VMA_REGISTRY.lock();
                    if let Some(map) = registry.get_mut(&pml4_phys) {
                        map.track_page(
                            owner_process,
                            pml4_phys,
                            vma,
                            page_addr,
                            phys,
                            PageSource::Anonymous,
                        )
                    } else {
                        Err(VmaError::NotFound)
                    }
                };

                if let Err(track_err) = tracked {
                    let _ = vm::unmap_page_in_pml4(pml4_phys, page_addr);
                    let _ = pmm::free_page(phys);
                    log_warn!(
                        LOG_ORIGIN,
                        "[PF] materialize=anon pml4=0x{:X} page=0x{:X} phys=0x{:X} result=not_handled reason=track_failed err={:?} label={}",
                        pml4_phys,
                        page_addr,
                        phys,
                        track_err,
                        vma.label
                    );
                    return FaultResult::NotHandled;
                }

                if let Some(process_id) = owner_process {
                    crate::process::account_process_resident_pages_add(process_id, 1, 0);
                }

                log_debug!(
                    LOG_ORIGIN,
                    "[PF] materialize=anon pml4=0x{:X} page=0x{:X} phys=0x{:X} access={:?} user={} label={} result=mapped",
                    pml4_phys,
                    page_addr,
                    phys,
                    ctx.access_type,
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
    })
}

fn materialize_cow(
    ctx: &FaultContext,
    vma: &Vma,
) -> FaultResult {
    use crate::mm::pmm;
    use crate::mm::vm::{self, PageFlags};

    let pml4_phys = ctx.address_space_id.as_usize();
    let page_addr = ctx.fault_addr.page_base();
    const MAX_COW_RETRIES: usize = 3;

    for _attempt in 0..MAX_COW_RETRIES {
        let (snapshot_paddr, snapshot_flags) = match vm::query_mapping_in_pml4(pml4_phys, page_addr) {
            Ok(mapping) => mapping,
            Err(err) => {
                log_warn!(
                    LOG_ORIGIN,
                    "[PF] materialize=cow pml4=0x{:X} page=0x{:X} result=not_handled err={:?} label={}",
                    pml4_phys,
                    page_addr,
                    err,
                    vma.label
                );
                return FaultResult::NotHandled;
            }
        };

        if !vm::is_cow(snapshot_flags) || snapshot_flags.contains(PageFlags::WRITABLE) {
            // Another CPU may have already completed the COW break.
            if snapshot_flags.contains(PageFlags::WRITABLE) && !vm::is_cow(snapshot_flags) {
                return FaultResult::Resolved;
            }
            log_warn!(
                LOG_ORIGIN,
                "[PF] materialize=cow pml4=0x{:X} page=0x{:X} result=not_handled reason=invalid_flags flags=0x{:X} label={}",
                pml4_phys,
                page_addr,
                snapshot_flags.bits(),
                vma.label
            );
            return FaultResult::NotHandled;
        }

        let snapshot_ref = pmm::phys_ref_get(snapshot_paddr);
        let mut staged_new_paddr = if snapshot_ref > 1 {
            match pmm::alloc_page_zeroed() {
                Some(new_paddr) => {
                    vm::copy_phys_page(new_paddr, snapshot_paddr);
                    Some(new_paddr)
                }
                None => return FaultResult::OutOfMemory,
            }
        } else {
            None
        };

        enum CowCommitResult {
            Resolved,
            Retry,
            OutOfMemory,
            NotHandled,
        }

        let commit = with_address_space_ops_lock(pml4_phys, || {
            let (old_paddr, old_flags) = match vm::query_mapping_in_pml4(pml4_phys, page_addr) {
                Ok(mapping) => mapping,
                Err(_) => {
                    if let Some(staged) = staged_new_paddr.take() {
                        let _ = pmm::free_page(staged);
                    }
                    return CowCommitResult::Retry;
                }
            };

            if old_paddr != snapshot_paddr
                || !vm::is_cow(old_flags)
                || old_flags.contains(PageFlags::WRITABLE)
            {
                if let Some(staged) = staged_new_paddr.take() {
                    let _ = pmm::free_page(staged);
                }
                if old_flags.contains(PageFlags::WRITABLE) && !vm::is_cow(old_flags) {
                    return CowCommitResult::Resolved;
                }
                return CowCommitResult::Retry;
            }

            let old_ref = pmm::phys_ref_get(old_paddr);
            if old_ref <= 1 {
                if let Some(staged) = staged_new_paddr.take() {
                    let _ = pmm::free_page(staged);
                }

                let new_flags = vm::clear_cow(old_flags).with(PageFlags::WRITABLE);
                if vm::remap_page_in_pml4(pml4_phys, page_addr, old_paddr, new_flags).is_err() {
                    return CowCommitResult::NotHandled;
                }

                let owner_process = Some(ctx.pid);
                let account = PageAccount {
                    owner_process,
                    owner_address_space: pml4_phys,
                    vaddr: VirtAddr::new(page_addr),
                    paddr: old_paddr,
                    source: PageSource::Anonymous,
                    share_state: PageShareState::Exclusive,
                    vma_start: vma.start,
                };
                let _ = upsert_materialized_page(pml4_phys, account);

                log_info!(
                    LOG_ORIGIN,
                    "[COW] promote_unique pid={:?} asid=0x{:X} vaddr=0x{:X} paddr=0x{:X}",
                    owner_process,
                    pml4_phys,
                    page_addr,
                    old_paddr
                );
                return CowCommitResult::Resolved;
            }

            let new_paddr = match staged_new_paddr.take() {
                Some(p) => p,
                None => match pmm::alloc_page_zeroed() {
                    Some(p) => {
                        vm::copy_phys_page(p, old_paddr);
                        p
                    }
                    None => return CowCommitResult::OutOfMemory,
                },
            };

            let new_flags = vm::clear_cow(old_flags).with(PageFlags::WRITABLE);
            if vm::remap_page_in_pml4(pml4_phys, page_addr, new_paddr, new_flags).is_err() {
                let _ = pmm::free_page(new_paddr);
                return CowCommitResult::NotHandled;
            }

            let _ = pmm::phys_ref_dec(old_paddr);

            let owner_process = Some(ctx.pid);
            let account = PageAccount {
                owner_process,
                owner_address_space: pml4_phys,
                vaddr: VirtAddr::new(page_addr),
                paddr: new_paddr,
                source: PageSource::Anonymous,
                share_state: PageShareState::Exclusive,
                vma_start: vma.start,
            };
            let _ = upsert_materialized_page(pml4_phys, account);

            log_info!(
                LOG_ORIGIN,
                "[COW] break pid={:?} asid=0x{:X} vaddr=0x{:X} old_paddr=0x{:X} new_paddr=0x{:X} old_ref={}",
                owner_process,
                pml4_phys,
                page_addr,
                old_paddr,
                new_paddr,
                old_ref
            );
            CowCommitResult::Resolved
        });

        match commit {
            CowCommitResult::Resolved => return FaultResult::Resolved,
            CowCommitResult::Retry => continue,
            CowCommitResult::OutOfMemory => return FaultResult::OutOfMemory,
            CowCommitResult::NotHandled => return FaultResult::NotHandled,
        }
    }

    log_warn!(
        LOG_ORIGIN,
        "[PF] materialize=cow pml4=0x{:X} page=0x{:X} result=not_handled reason=retry_exhausted attempts={} label={}",
        pml4_phys,
        page_addr,
        MAX_COW_RETRIES,
        vma.label
    );
    FaultResult::NotHandled
}

/// Attempt to resolve a page fault with a deterministic pipeline:
/// classify -> admit -> materialize.
pub fn handle_page_fault(
    ctx: FaultContext,
    error_code: u64,
) -> FaultResult {
    let pml4_phys = ctx.address_space_id.as_usize();
    let page_addr = ctx.fault_addr.page_base();

    let vma = {
        let registry = VMA_REGISTRY.lock();
        let map = match registry.get(&pml4_phys) {
            Some(map) => map,
            None => {
                log_warn!(
                    LOG_ORIGIN,
                    "[PF] vma_hit=false addr=0x{:X} page=0x{:X} pml4=0x{:X} reason=no_registry",
                    ctx.fault_addr.as_usize(),
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
                    ctx.fault_addr.as_usize(),
                    page_addr,
                    vma.start,
                    vma.end,
                    vma.perms.bits(),
                    vma.backing,
                    vma.label
                );
                Some(vma.clone())
            }
            None => {
                log_warn!(
                    LOG_ORIGIN,
                    "[PF] vma_hit=false addr=0x{:X} page=0x{:X} pml4=0x{:X} reason=miss vma_count={}",
                    ctx.fault_addr.as_usize(),
                    page_addr,
                    pml4_phys,
                    map.len()
                );
                None
            }
        }
    };

    let classified = classify_fault(&ctx, error_code, vma.as_ref());
    log_debug!(
        LOG_ORIGIN,
        "[PF] classify={:?} addr=0x{:X} access={:?} user={} err={:#X} pml4=0x{:X}",
        classified,
        ctx.fault_addr.as_usize(),
        ctx.access_type,
        ctx.is_user,
        error_code,
        pml4_phys
    );

    if let Err(result) = admit_fault(&ctx, vma.as_ref(), classified) {
        log_warn!(
            LOG_ORIGIN,
            "[PF] admit=deny addr=0x{:X} access={:?} result={:?} label={}",
            ctx.fault_addr.as_usize(),
            ctx.access_type,
            result,
            vma.map(|v| v.label).unwrap_or("none")
        );
        return result;
    }

    if let Err(result) = evaluate_memory_admission(&ctx, classified) {
        log_warn!(
            LOG_ORIGIN,
            "[PF] admission=deny pid={} addr=0x{:X} access={:?} result={:?}",
            ctx.pid,
            ctx.fault_addr.as_usize(),
            ctx.access_type,
            result
        );
        return result;
    }

    let Some(vma) = vma else {
        return FaultResult::InvalidAddress;
    };
    let result = materialize_fault(&ctx, classified, &vma);
    log_debug!(
        LOG_ORIGIN,
        "[PF] result={:?} addr=0x{:X} page=0x{:X} access={:?} user={} label={}",
        result,
        ctx.fault_addr.as_usize(),
        page_addr,
        ctx.access_type,
        ctx.is_user,
        vma.label
    );
    result
}

pub fn init() {
    run_fault_pipeline_self_tests();
    run_cow_fork_self_tests();
    log_info!(LOG_ORIGIN, "VMA subsystem initialized — demand paging ready");
}

fn run_fault_pipeline_self_tests() {
    struct FaultPipelineSim {
        vmas: VmaMap,
        present_pages: BTreeMap<usize, usize>,
        next_phys: usize,
        faults: usize,
        allocs: usize,
        maps: usize,
    }

    impl FaultPipelineSim {
        fn new(vmas: alloc::vec::Vec<Vma>) -> Self {
            let mut map = VmaMap::new();
            for vma in vmas {
                map.insert(vma).expect("self-test must build valid VMA map");
            }
            Self {
                vmas: map,
                present_pages: BTreeMap::new(),
                next_phys: 0x1000_0000,
                faults: 0,
                allocs: 0,
                maps: 0,
            }
        }

        fn write_byte(&mut self, addr: usize) -> FaultResult {
            let page = addr & !(PAGE_SIZE - 1);
            let Some(vma) = self.vmas.find(page) else {
                self.faults += 1;
                return FaultResult::InvalidAddress;
            };

            if !vma.permits(AccessType::Write) {
                self.faults += 1;
                return FaultResult::ProtectionViolation;
            }

            if self.present_pages.contains_key(&page) {
                return FaultResult::Resolved;
            }

            self.faults += 1;
            let phys = self.next_phys;
            self.next_phys += PAGE_SIZE;
            self.allocs += 1;
            self.maps += 1;
            self.present_pages.insert(page, phys);
            FaultResult::Resolved
        }
    }

    // Teste 1: Single Page
    {
        let base = 0x2000_0000;
        let mut sim = FaultPipelineSim::new(alloc::vec![
            Vma {
                start: base,
                end: base + PAGE_SIZE,
                perms: VmaPermissions::read_write(),
                backing: VmaBacking::Anonymous,
                label: "pf_test_single",
            },
        ]);

        assert_eq!(sim.write_byte(base), FaultResult::Resolved);
        assert_eq!(sim.write_byte(base), FaultResult::Resolved);
        assert_eq!(sim.faults, 1);
        assert_eq!(sim.allocs, 1);
        assert_eq!(sim.maps, 1);
    }

    // Teste 2: Multi Page
    {
        let base = 0x3000_0000;
        const PAGES: usize = 8;
        let mut sim = FaultPipelineSim::new(alloc::vec![
            Vma {
                start: base,
                end: base + (PAGES * PAGE_SIZE),
                perms: VmaPermissions::read_write(),
                backing: VmaBacking::Anonymous,
                label: "pf_test_multi",
            },
        ]);

        for i in 0..PAGES {
            assert_eq!(
                sim.write_byte(base + i * PAGE_SIZE),
                FaultResult::Resolved
            );
        }
        assert_eq!(sim.faults, PAGES);
        assert_eq!(sim.allocs, PAGES);
        assert_eq!(sim.maps, PAGES);
    }

    // Teste 3: Re-access
    {
        let base = 0x4000_0000;
        let mut sim = FaultPipelineSim::new(alloc::vec![
            Vma {
                start: base,
                end: base + PAGE_SIZE,
                perms: VmaPermissions::read_write(),
                backing: VmaBacking::Anonymous,
                label: "pf_test_reaccess",
            },
        ]);

        for _ in 0..100 {
            assert_eq!(sim.write_byte(base), FaultResult::Resolved);
        }
        assert_eq!(sim.faults, 1);
        assert_eq!(sim.allocs, 1);
    }

    // Teste 4: Invalid Access
    {
        let base = 0x5000_0000;
        let mut sim = FaultPipelineSim::new(alloc::vec![
            Vma {
                start: base,
                end: base + PAGE_SIZE,
                perms: VmaPermissions::read_write(),
                backing: VmaBacking::Anonymous,
                label: "pf_test_invalid",
            },
        ]);

        assert_eq!(
            sim.write_byte(base + PAGE_SIZE * 2),
            FaultResult::InvalidAddress
        );
        assert_eq!(sim.faults, 1);
        assert_eq!(sim.allocs, 0);
    }

    // Teste 5: Protection Fault
    {
        let base = 0x6000_0000;
        let mut sim = FaultPipelineSim::new(alloc::vec![
            Vma {
                start: base,
                end: base + PAGE_SIZE,
                perms: VmaPermissions::READ,
                backing: VmaBacking::Anonymous,
                label: "pf_test_prot",
            },
        ]);

        assert_eq!(sim.write_byte(base), FaultResult::ProtectionViolation);
        assert_eq!(sim.faults, 1);
        assert_eq!(sim.allocs, 0);
    }

    log_info!(LOG_ORIGIN, "[PF_TEST] all 5 fault pipeline tests passed");
}

fn run_cow_fork_self_tests() {
    #[derive(Clone, Copy)]
    enum SimProc {
        Parent,
        Child,
    }

    #[derive(Clone, Copy)]
    struct SimPte {
        paddr: usize,
        writable: bool,
        cow: bool,
    }

    struct SimAddressSpace {
        vmas: VmaMap,
        pages: BTreeMap<usize, SimPte>,
    }

    impl SimAddressSpace {
        fn new(vmas: alloc::vec::Vec<Vma>) -> Self {
            let mut map = VmaMap::new();
            for vma in vmas {
                map.insert(vma).expect("self-test must build valid VMA map");
            }
            Self {
                vmas: map,
                pages: BTreeMap::new(),
            }
        }
    }

    struct CowForkSim {
        parent: SimAddressSpace,
        child: Option<SimAddressSpace>,
        next_phys: usize,
        refcounts: BTreeMap<usize, usize>,
        page_data: BTreeMap<usize, alloc::vec::Vec<u8>>,
        faults: usize,
        allocs: usize,
        copies: usize,
        shares: usize,
    }

    impl CowForkSim {
        fn new(vmas: alloc::vec::Vec<Vma>) -> Self {
            Self {
                parent: SimAddressSpace::new(vmas),
                child: None,
                next_phys: 0x2000_0000,
                refcounts: BTreeMap::new(),
                page_data: BTreeMap::new(),
                faults: 0,
                allocs: 0,
                copies: 0,
                shares: 0,
            }
        }

        fn aspace(&self, who: SimProc) -> &SimAddressSpace {
            match who {
                SimProc::Parent => &self.parent,
                SimProc::Child => self.child.as_ref().expect("child must exist"),
            }
        }

        fn aspace_mut(&mut self, who: SimProc) -> &mut SimAddressSpace {
            match who {
                SimProc::Parent => &mut self.parent,
                SimProc::Child => self.child.as_mut().expect("child must exist"),
            }
        }

        fn alloc_phys_page(&mut self) -> usize {
            let paddr = self.next_phys;
            self.next_phys += PAGE_SIZE;
            self.allocs += 1;
            self.refcounts.insert(paddr, 1);
            self.page_data.insert(paddr, alloc::vec![0; PAGE_SIZE]);
            paddr
        }

        fn ref_get(&self, paddr: usize) -> usize {
            self.refcounts.get(&paddr).copied().unwrap_or(0)
        }

        fn inc_ref(&mut self, paddr: usize) {
            let counter = self.refcounts.entry(paddr).or_insert(0);
            *counter += 1;
        }

        fn dec_ref(&mut self, paddr: usize) {
            if let Some(counter) = self.refcounts.get_mut(&paddr) {
                if *counter > 1 {
                    *counter -= 1;
                } else {
                    self.refcounts.remove(&paddr);
                    self.page_data.remove(&paddr);
                }
            }
        }

        fn ensure_mapping_for_access(
            &mut self,
            who: SimProc,
            addr: usize,
            access: AccessType,
        ) -> Result<(), FaultResult> {
            let page = addr & !(PAGE_SIZE - 1);
            let Some(vma) = self.aspace(who).vmas.find(page).cloned() else {
                self.faults += 1;
                return Err(FaultResult::InvalidAddress);
            };

            if !vma.permits(access) {
                self.faults += 1;
                return Err(FaultResult::ProtectionViolation);
            }

            if !self.aspace(who).pages.contains_key(&page) {
                self.faults += 1;
                let phys = self.alloc_phys_page();
                let pte = SimPte {
                    paddr: phys,
                    writable: vma.permits(AccessType::Write),
                    cow: false,
                };
                self.aspace_mut(who).pages.insert(page, pte);
                return Ok(());
            }

            if matches!(access, AccessType::Write) {
                let pte = self.aspace(who).pages.get(&page).copied().expect("mapped page");
                if pte.writable {
                    return Ok(());
                }

                self.faults += 1;
                if !pte.cow {
                    return Err(FaultResult::ProtectionViolation);
                }

                let old_ref = self.ref_get(pte.paddr);
                if old_ref <= 1 {
                    let mut promoted = pte;
                    promoted.writable = true;
                    promoted.cow = false;
                    self.aspace_mut(who).pages.insert(page, promoted);
                    return Ok(());
                }

                let new_phys = self.alloc_phys_page();
                let old_data = self
                    .page_data
                    .get(&pte.paddr)
                    .cloned()
                    .unwrap_or_else(|| alloc::vec![0; PAGE_SIZE]);
                self.page_data.insert(new_phys, old_data);
                self.copies += 1;
                self.dec_ref(pte.paddr);

                self.aspace_mut(who).pages.insert(
                    page,
                    SimPte {
                        paddr: new_phys,
                        writable: true,
                        cow: false,
                    },
                );
            }

            Ok(())
        }

        fn write_byte(&mut self, who: SimProc, addr: usize, value: u8) -> FaultResult {
            if let Err(err) = self.ensure_mapping_for_access(who, addr, AccessType::Write) {
                return err;
            }

            let page = addr & !(PAGE_SIZE - 1);
            let offset = addr & (PAGE_SIZE - 1);
            let pte = self
                .aspace(who)
                .pages
                .get(&page)
                .copied()
                .expect("write mapping must exist");
            let data = self
                .page_data
                .entry(pte.paddr)
                .or_insert_with(|| alloc::vec![0; PAGE_SIZE]);
            data[offset] = value;
            FaultResult::Resolved
        }

        fn read_byte(&mut self, who: SimProc, addr: usize) -> Result<u8, FaultResult> {
            self.ensure_mapping_for_access(who, addr, AccessType::Read)?;

            let page = addr & !(PAGE_SIZE - 1);
            let offset = addr & (PAGE_SIZE - 1);
            let pte = self
                .aspace(who)
                .pages
                .get(&page)
                .copied()
                .expect("read mapping must exist");
            let value = self
                .page_data
                .get(&pte.paddr)
                .and_then(|buf| buf.get(offset).copied())
                .unwrap_or(0);
            Ok(value)
        }

        fn fork(&mut self) {
            let child_vmas = self.parent.vmas.list_vmas();
            let parent_snapshot: alloc::vec::Vec<(usize, SimPte, bool)> = self
                .parent
                .pages
                .iter()
                .filter_map(|(&vaddr, &pte)| {
                    let vma = self.parent.vmas.find(vaddr)?;
                    Some((vaddr, pte, vma.perms.contains(VmaPermissions::WRITE)))
                })
                .collect();

            let mut child = SimAddressSpace::new(child_vmas);
            for (vaddr, pte, writable_vma) in parent_snapshot {
                let mut parent_pte = pte;
                let mut child_pte = pte;

                if writable_vma {
                    parent_pte.writable = false;
                    parent_pte.cow = true;
                    child_pte.writable = false;
                    child_pte.cow = true;
                } else {
                    parent_pte.writable = false;
                    parent_pte.cow = false;
                    child_pte.writable = false;
                    child_pte.cow = false;
                }

                self.parent.pages.insert(vaddr, parent_pte);
                child.pages.insert(vaddr, child_pte);
                self.inc_ref(pte.paddr);
                self.shares += 1;
            }

            self.child = Some(child);
        }

        fn page_paddr(&self, who: SimProc, addr: usize) -> Option<usize> {
            let page = addr & !(PAGE_SIZE - 1);
            self.aspace(who).pages.get(&page).map(|pte| pte.paddr)
        }

        fn exit_proc(&mut self, who: SimProc) {
            let pages: alloc::vec::Vec<usize> = self
                .aspace(who)
                .pages
                .values()
                .map(|pte| pte.paddr)
                .collect();
            for paddr in pages {
                self.dec_ref(paddr);
            }
            self.aspace_mut(who).pages.clear();
            if matches!(who, SimProc::Child) {
                self.child = None;
            }
        }
    }

    // Teste 1: fork de página única materializada.
    {
        let base = 0x7000_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_single",
        }]);
        assert_eq!(sim.write_byte(SimProc::Parent, base, 0xAA), FaultResult::Resolved);
        sim.fork();
        assert_eq!(sim.read_byte(SimProc::Child, base), Ok(0xAA));
        assert_eq!(sim.write_byte(SimProc::Child, base, 0xBB), FaultResult::Resolved);
        assert_eq!(sim.read_byte(SimProc::Parent, base), Ok(0xAA));
        assert_eq!(sim.read_byte(SimProc::Child, base), Ok(0xBB));
        assert_eq!(sim.copies, 1);
    }

    // Teste 2: lazy page survives fork (sem clone eager).
    {
        let base = 0x7100_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + 2 * PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_lazy",
        }]);
        sim.fork();
        assert_eq!(sim.allocs, 0);
        assert_eq!(sim.shares, 0);
        assert_eq!(sim.write_byte(SimProc::Child, base, 0x5A), FaultResult::Resolved);
        assert_eq!(sim.allocs, 1);
        assert_eq!(sim.copies, 0);
    }

    // Teste 3: read-only sharing does not copy.
    {
        let base = 0x7200_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_readonly",
        }]);
        assert_eq!(sim.write_byte(SimProc::Parent, base, 0x11), FaultResult::Resolved);
        sim.fork();
        let allocs_before = sim.allocs;
        for _ in 0..32 {
            assert_eq!(sim.read_byte(SimProc::Parent, base), Ok(0x11));
            assert_eq!(sim.read_byte(SimProc::Child, base), Ok(0x11));
        }
        assert_eq!(sim.copies, 0);
        assert_eq!(sim.allocs, allocs_before);
    }

    // Teste 4: copy only dirty page.
    {
        let base = 0x7300_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + 4 * PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_dirty_only",
        }]);
        for i in 0..4 {
            let addr = base + i * PAGE_SIZE;
            assert_eq!(
                sim.write_byte(SimProc::Parent, addr, (0x20 + i as u8) as u8),
                FaultResult::Resolved
            );
        }
        sim.fork();

        let dirty_addr = base + 2 * PAGE_SIZE;
        let old_shared = sim.page_paddr(SimProc::Parent, dirty_addr).unwrap();
        assert_eq!(sim.write_byte(SimProc::Child, dirty_addr, 0xEE), FaultResult::Resolved);
        assert_eq!(sim.copies, 1);
        let child_dirty = sim.page_paddr(SimProc::Child, dirty_addr).unwrap();
        assert_ne!(child_dirty, old_shared);

        let untouched = base + PAGE_SIZE;
        let p_parent = sim.page_paddr(SimProc::Parent, untouched).unwrap();
        let p_child = sim.page_paddr(SimProc::Child, untouched).unwrap();
        assert_eq!(p_parent, p_child);
    }

    // Teste 5: child exit does not free parent page.
    {
        let base = 0x7400_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_child_exit",
        }]);
        assert_eq!(sim.write_byte(SimProc::Parent, base, 0x44), FaultResult::Resolved);
        sim.fork();
        let parent_page = sim.page_paddr(SimProc::Parent, base).unwrap();
        sim.exit_proc(SimProc::Child);
        assert_eq!(sim.read_byte(SimProc::Parent, base), Ok(0x44));
        assert_eq!(sim.ref_get(parent_page), 1);
    }

    // Teste 6: parent exit does not kill child page.
    {
        let base = 0x7500_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "cow_test_parent_exit",
        }]);
        assert_eq!(sim.write_byte(SimProc::Parent, base, 0x77), FaultResult::Resolved);
        sim.fork();
        sim.exit_proc(SimProc::Parent);
        assert_eq!(sim.read_byte(SimProc::Child, base), Ok(0x77));
        let child_page = sim.page_paddr(SimProc::Child, base).unwrap();
        assert_eq!(sim.ref_get(child_page), 1);
    }

    // Teste 7: write denied on non-writable VMA.
    {
        let base = 0x7600_0000;
        let mut sim = CowForkSim::new(alloc::vec![Vma {
            start: base,
            end: base + PAGE_SIZE,
            perms: VmaPermissions::READ,
            backing: VmaBacking::Anonymous,
            label: "cow_test_prot",
        }]);
        assert_eq!(sim.read_byte(SimProc::Parent, base), Ok(0));
        sim.fork();
        assert_eq!(
            sim.write_byte(SimProc::Child, base, 0xAB),
            FaultResult::ProtectionViolation
        );
        assert_eq!(sim.copies, 0);
    }

    log_info!(LOG_ORIGIN, "[COW_TEST] all 7 fork/cow tests passed");
}
