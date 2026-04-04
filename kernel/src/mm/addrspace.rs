// Address Space Management
//
// Implements creation, isolation, and manipulation of virtual address spaces
// for user-space threads. Each address space corresponds to an independent
// page table hierarchy (PML4) and enforces strict ownership and kernel isolation.
//
// Key responsibilities:
// - Create and destroy user address spaces backed by independent PML4 tables
// - Enforce ownership: only the owning thread may modify an address space
// - Safely map, unmap, and remap virtual memory regions
// - Prevent any user mapping from overlapping kernel virtual memory
// - Track active mappings to prevent premature address space destruction
//
// Design principles:
// - Strong isolation: kernel space (higher half) is always shared and protected
// - Capability-like ownership via `ThreadId` checks on every operation
// - Fail-safe behavior: partial mappings are rolled back on errors
// - Explicit accounting of mapped pages to detect leaks and misuse
//
// Implementation details:
// - Each `AddressSpace` owns a single PML4 physical page allocated via the PMM
// - Kernel mappings are cloned into new PML4s at creation time
// - Address spaces are globally managed in a `BTreeMap` protected by a spinlock
// - Virtual regions are validated for alignment, size, and kernel overlap
// - Mapping operations delegate to the lower-level `vm` module for page table work
//
// Correctness and safety notes:
// - Userspace mappings must satisfy the ABI-validated user address contract
// - Mapping size is capped (`MAX_REGION_SIZE`) to limit abuse and fragmentation
// - Rollback logic ensures no silent partial mappings on failure
// - `mapping_count` prevents destroying address spaces still in active use
// - PML4 pages are freed via `Drop` unless ownership is explicitly handed off
//   to thread teardown for a thread's primary address space
//
// Error handling:
// - Rich `AddressSpaceError` enum distinguishes permission, validity,
//   resource exhaustion, and kernel-space violations
// - Errors are logged to serial for early debugging and auditability
//
// Public interface:
// - Thin wrapper functions expose the manager without leaking internal locks
// - Intended to be used by syscalls and higher-level process management code

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::mm::pmm;
use crate::mm::vm::{self, PageFlags, VmError};
use crate::thread::ThreadId;
use crate::{log_info, log_warn, log_error};
use atom_abi::UserRange;

static CLEANED_THREAD_ADDRESS_SPACES: Mutex<BTreeSet<ThreadId>> = Mutex::new(BTreeSet::new());

fn note_thread_address_space_activity(thread_id: ThreadId) {
    CLEANED_THREAD_ADDRESS_SPACES.lock().remove(&thread_id);
}

pub(crate) fn forget_thread_address_space_cleanup(thread_id: ThreadId) {
    CLEANED_THREAD_ADDRESS_SPACES.lock().remove(&thread_id);
}

fn begin_thread_address_space_cleanup(thread_id: ThreadId) -> bool {
    CLEANED_THREAD_ADDRESS_SPACES.lock().insert(thread_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AddressSpaceId(u64);

impl AddressSpaceId {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        AddressSpaceId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn from_raw(value: u64) -> Self {
        AddressSpaceId(value)
    }
}

impl core::fmt::Display for AddressSpaceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AS:{}", self.0)
    }
}

// Re-export from the shared ABI crate — single source of truth.
// These are pub so that other kernel modules (shared_mem, executable, etc.)
// can import them from addrspace if needed.
#[allow(unused_imports)]
pub use atom_abi::{USER_CANONICAL_MAX as USER_CANONICAL_MAX_U64, USER_VA_LIMIT, SYSCALL_ERROR_THRESHOLD};

/// `USER_CANONICAL_MAX` as a `usize`, for kernel-internal address comparisons.
pub const USER_CANONICAL_MAX: usize = atom_abi::USER_CANONICAL_MAX as usize;

const MAX_REGION_SIZE: usize = 256 * 1024 * 1024;
const LOG_ORIGIN: &str = "addrspace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedAddressSpaceKind {
    Auxiliary,
}

#[derive(Debug)]
pub struct AddressSpace {
    id: AddressSpaceId,
    pml4_phys: usize,
    owner: ThreadId,
    kind: ManagedAddressSpaceKind,
    mapping_count: usize,
    release_pml4_on_drop: bool,
}

impl AddressSpace {
    pub fn new(owner: ThreadId) -> Result<Self, AddressSpaceError> {
        let pml4_phys = pmm::alloc_page_zeroed().ok_or(AddressSpaceError::OutOfMemory)?;
        
        if let Err(err) = vm::clone_kernel_mappings(pml4_phys).map_err(|err| {
            log_error!(
                LOG_ORIGIN,
                "Failed to clone kernel mappings into PML4 0x{:X}: {:?}",
                pml4_phys,
                err
            );
            AddressSpaceError::KernelMappingSetupFailed
        }) {
            let _ = pmm::free_page(pml4_phys);
            return Err(err);
        }

        // Register the PML4 as protected (Req 2.4)
        pmm::register_active_pml4(pml4_phys)
            .map_err(|_| AddressSpaceError::KernelMappingSetupFailed)?;

        log_info!(
            LOG_ORIGIN,
            "Created new auxiliary address space with PML4 at 0x{:X} for thread {}",
            pml4_phys,
            owner
        );

        Ok(Self {
            id: AddressSpaceId::new(),
            pml4_phys,
            owner,
            kind: ManagedAddressSpaceKind::Auxiliary,
            mapping_count: 0,
            release_pml4_on_drop: true,
        })
    }

    pub fn id(&self) -> AddressSpaceId {
        self.id
    }

    pub fn pml4_phys(&self) -> usize {
        self.pml4_phys
    }

    pub fn is_owned_by(&self, thread: ThreadId) -> bool {
        self.owner == thread
    }

    pub fn mapping_count(&self) -> usize {
        self.mapping_count
    }

    pub fn kind(&self) -> ManagedAddressSpaceKind {
        self.kind
    }

    fn inc_mappings(&mut self, count: usize) {
        self.mapping_count = self.mapping_count.saturating_add(count);
    }

    fn dec_mappings(&mut self, count: usize) {
        self.mapping_count = self.mapping_count.saturating_sub(count);
    }

    fn defer_pml4_release_to_thread_teardown(&mut self) {
        self.release_pml4_on_drop = false;
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        if !self.release_pml4_on_drop {
            log_info!(
                LOG_ORIGIN,
                "Dropping address space {} metadata without freeing PML4 0x{:X} (thread teardown owns final release)",
                self.id,
                self.pml4_phys
            );
            return;
        }

        log_info!(
            LOG_ORIGIN,
            "Destroying address space {} (PML4=0x{:X})",
            self.id,
            self.pml4_phys
        );
        
        // Unregister the PML4 before freeing (Req 2.5)
        let _ = pmm::unregister_active_pml4(self.pml4_phys);
        let _ = pmm::free_page(self.pml4_phys);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceError {
    NotFound,
    OutOfMemory,
    PermissionDenied,
    InvalidAddress,
    InvalidSize,
    KernelSpaceViolation,
    InUse,
    AlreadyMapped,
    NotMapped,
    KernelMappingSetupFailed,
}

fn map_validation_error(err: crate::mm::ValidationError) -> AddressSpaceError {
    match err {
        crate::mm::ValidationError::Unaligned { .. } => AddressSpaceError::InvalidAddress,
        crate::mm::ValidationError::OutOfBounds { .. } => AddressSpaceError::KernelSpaceViolation,
        crate::mm::ValidationError::ProtectedResource { .. } => AddressSpaceError::PermissionDenied,
        crate::mm::ValidationError::NotInitialized => AddressSpaceError::KernelMappingSetupFailed,
        crate::mm::ValidationError::InvalidSize { .. } => AddressSpaceError::InvalidSize,
    }
}

pub struct AddressSpaceManager {
    spaces: Mutex<BTreeMap<AddressSpaceId, AddressSpace>>,
}

impl AddressSpaceManager {
    pub const fn new() -> Self {
        Self {
            spaces: Mutex::new(BTreeMap::new()),
        }
    }
    
    pub fn create(&self, owner: ThreadId) -> Result<AddressSpaceId, AddressSpaceError> {
        let addrspace = AddressSpace::new(owner)?;
        let id = addrspace.id();

        note_thread_address_space_activity(owner);
        let mut spaces = self.spaces.lock();
        spaces.insert(id, addrspace);

        log_info!(LOG_ORIGIN, "Registered address space {}", id);
        Ok(id)
    }
    
    pub fn destroy(
        &self,
        id: AddressSpaceId,
        caller: ThreadId,
    ) -> Result<(), AddressSpaceError> {
        let mut spaces = self.spaces.lock();
        let addrspace = spaces.get(&id).ok_or(AddressSpaceError::NotFound)?;

        if !addrspace.is_owned_by(caller) {
            log_warn!(
                LOG_ORIGIN,
                "Destroy denied: {} not owned by thread {}",
                id,
                caller
            );
            return Err(AddressSpaceError::PermissionDenied);
        }

        if addrspace.mapping_count() > 0 {
            log_warn!(
                LOG_ORIGIN,
                "Destroy denied: {} still has {} active mappings",
                id,
                addrspace.mapping_count()
            );
            return Err(AddressSpaceError::InUse);
        }

        spaces.remove(&id);

        log_info!(LOG_ORIGIN, "Destroyed address space {}", id);
        Ok(())
    }
    
    pub fn map_region(
        &self,
        id: AddressSpaceId,
        caller: ThreadId,
        user_range: UserRange,
        phys_addr: usize,
        flags: PageFlags,
    ) -> Result<(), AddressSpaceError> {
        let virt_addr = user_range.start();
        let size = user_range.len();

        crate::mm::validate_page_alignment(virt_addr).map_err(map_validation_error)?;
        crate::mm::validate_page_alignment(phys_addr).map_err(map_validation_error)?;
        crate::mm::validate_size(size, MAX_REGION_SIZE).map_err(map_validation_error)?;

        let mut spaces = self.spaces.lock();
        let addrspace = spaces.get_mut(&id).ok_or(AddressSpaceError::NotFound)?;

        if !addrspace.is_owned_by(caller) {
            log_warn!(
                LOG_ORIGIN,
                "Map denied: {} not owned by thread {}",
                id,
                caller
            );
            return Err(AddressSpaceError::PermissionDenied);
        }

        let pml4_phys = addrspace.pml4_phys();
        let num_pages = pmm::align_up(size) / pmm::PAGE_SIZE;

        log_info!(
            LOG_ORIGIN,
            "Mapping region in {}: virt=0x{:X} phys=0x{:X} size={} ({} pages)",
            id,
            virt_addr,
            phys_addr,
            size,
            num_pages
        );

        let mut mapped_pages = 0;
        for i in 0..num_pages {
            let virt = virt_addr + (i * pmm::PAGE_SIZE);
            let phys = phys_addr + (i * pmm::PAGE_SIZE);
            
            if let Err(e) = self.map_page_in_pml4(pml4_phys, virt, phys, flags) {
                log_error!(
                    LOG_ORIGIN,
                    "Failed to map page {} of {}: {:?}",
                    i + 1,
                    num_pages,
                    e
                );

                let mut rolled_back = 0;
                for rollback_index in 0..mapped_pages {
                    let rollback_virt = virt_addr + (rollback_index * pmm::PAGE_SIZE);
                    match self.unmap_page_in_pml4(pml4_phys, rollback_virt) {
                        Ok(_) => rolled_back += 1,
                        Err(unmap_err) => log_error!(
                            LOG_ORIGIN,
                            "Failed to rollback page at 0x{:X}: {:?}",
                            rollback_virt,
                            unmap_err
                        ),
                    }
                }

                let remaining = mapped_pages.saturating_sub(rolled_back);
                if remaining > 0 {
                    addrspace.inc_mappings(remaining);
                    log_warn!(
                        LOG_ORIGIN,
                        "{} pages remain mapped after rollback (count updated)",
                        remaining
                    );
                }

                return Err(AddressSpaceError::AlreadyMapped);
            }

            mapped_pages += 1;
        }

        addrspace.inc_mappings(mapped_pages);

        log_info!(
            LOG_ORIGIN,
            "Successfully mapped {} pages (total mappings: {})",
            num_pages,
            addrspace.mapping_count()
        );

        Ok(())
    }
    
    pub fn unmap_region(
        &self,
        id: AddressSpaceId,
        caller: ThreadId,
        user_range: UserRange,
    ) -> Result<(), AddressSpaceError> {
        let virt_addr = user_range.start();
        let size = user_range.len();

        crate::mm::validate_page_alignment(virt_addr).map_err(map_validation_error)?;
        crate::mm::validate_size(size, MAX_REGION_SIZE).map_err(map_validation_error)?;

        let mut spaces = self.spaces.lock();
        let addrspace = spaces.get_mut(&id).ok_or(AddressSpaceError::NotFound)?;

        if !addrspace.is_owned_by(caller) {
            log_warn!(
                LOG_ORIGIN,
                "Unmap denied: {} not owned by thread {}",
                id,
                caller
            );
            return Err(AddressSpaceError::PermissionDenied);
        }

        let pml4_phys = addrspace.pml4_phys();
        let num_pages = pmm::align_up(size) / pmm::PAGE_SIZE;

        log_info!(
            LOG_ORIGIN,
            "Unmapping region in {}: virt=0x{:X} size={} ({} pages)",
            id,
            virt_addr,
            size,
            num_pages
        );

        for i in 0..num_pages {
            let virt = virt_addr + (i * pmm::PAGE_SIZE);

            if let Err(e) = self.unmap_page_in_pml4(pml4_phys, virt) {
                log_error!(
                    LOG_ORIGIN,
                    "Failed to unmap page {} of {}: {:?}",
                    i + 1,
                    num_pages,
                    e
                );
            }
        }

        addrspace.dec_mappings(num_pages);

        log_info!(
            LOG_ORIGIN,
            "Successfully unmapped {} pages (total mappings: {})",
            num_pages,
            addrspace.mapping_count()
        );

        Ok(())
    }
    
    pub fn remap_region(
        &self,
        id: AddressSpaceId,
        caller: ThreadId,
        old_range: UserRange,
        new_range: UserRange,
    ) -> Result<(), AddressSpaceError> {
        if old_range.len() != new_range.len() {
            return Err(AddressSpaceError::InvalidSize);
        }

        let old_virt = old_range.start();
        let new_virt = new_range.start();
        let size = old_range.len();

        crate::mm::validate_page_alignment(old_virt).map_err(map_validation_error)?;
        crate::mm::validate_page_alignment(new_virt).map_err(map_validation_error)?;
        crate::mm::validate_size(size, MAX_REGION_SIZE).map_err(map_validation_error)?;

        let spaces = self.spaces.lock();
        let addrspace = spaces.get(&id).ok_or(AddressSpaceError::NotFound)?;

        if !addrspace.is_owned_by(caller) {
            return Err(AddressSpaceError::PermissionDenied);
        }

        let pml4_phys = addrspace.pml4_phys();

        let num_pages = pmm::align_up(size) / pmm::PAGE_SIZE;

        log_info!(
            LOG_ORIGIN,
            "Remapping region in {}: 0x{:X} -> 0x{:X}, {} pages",
            id,
            old_virt,
            new_virt,
            num_pages
        );
        
        let mut mappings = alloc::vec::Vec::with_capacity(num_pages);
        for i in 0..num_pages {
            let old_virt_page = old_virt + (i * pmm::PAGE_SIZE);

            match vm::query_mapping_in_pml4(pml4_phys, old_virt_page) {
                Ok((phys, flags)) => {
                    mappings.push((phys, flags));
                }
                Err(_) => {
                    log_warn!(
                        LOG_ORIGIN,
                        "Remap failed: page {} at 0x{:X} not mapped",
                        i,
                        old_virt_page
                    );
                    return Err(AddressSpaceError::NotMapped);
                }
            }
        }

        for i in 0..num_pages {
            let old_virt_page = old_virt + (i * pmm::PAGE_SIZE);
            if let Err(e) = self.unmap_page_in_pml4(pml4_phys, old_virt_page) {
                log_error!(
                    LOG_ORIGIN,
                    "Remap: failed to unmap old page {}: {:?}",
                    i,
                    e
                );
            }
        }
        
        for (i, &(phys, flags)) in mappings.iter().enumerate().take(num_pages) {
            let new_virt_page = new_virt + (i * pmm::PAGE_SIZE);

            if let Err(e) = self.map_page_in_pml4(pml4_phys, new_virt_page, phys, flags) {
                log_error!(
                    LOG_ORIGIN,
                    "Remap: failed to map new page {}: {:?}",
                    i,
                    e
                );
                return Err(AddressSpaceError::AlreadyMapped);
            }
        }

        log_info!(LOG_ORIGIN, "Successfully remapped {} pages", num_pages);

        Ok(())
    }
    
    fn map_page_in_pml4(
        &self,
        pml4_phys: usize,
        virt: usize,
        phys: usize,
        flags: PageFlags,
    ) -> Result<(), VmError> {
        vm::map_page_in_pml4(pml4_phys, virt, phys, flags)
    }

    fn unmap_page_in_pml4(&self, pml4_phys: usize, virt: usize) -> Result<(), VmError> {
        vm::unmap_page_in_pml4(pml4_phys, virt)
    }

    #[allow(dead_code)]
    pub fn pml4_phys(&self, id: AddressSpaceId) -> Option<usize> {
        let spaces = self.spaces.lock();
        spaces.get(&id).map(|space| space.pml4_phys())
    }

}

static ADDRESS_SPACE_MANAGER: AddressSpaceManager = AddressSpaceManager::new();

pub fn init() {
    log_info!(LOG_ORIGIN, "Address space management initialized (Phase 5.1)");
    log_info!(
        LOG_ORIGIN,
        "User VA window (ABI): 0x{:X}..0x{:X}",
        atom_abi::USER_SPACE_MIN,
        atom_abi::USER_SPACE_MAX
    );
    log_info!(LOG_ORIGIN, "Max region size: {} MB", MAX_REGION_SIZE / (1024 * 1024));
}

pub fn create_address_space(owner: ThreadId) -> Result<AddressSpaceId, AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.create(owner)
}

pub fn destroy_address_space(
    id: AddressSpaceId,
    caller: ThreadId,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.destroy(id, caller)
}

pub fn map_region(
    id: AddressSpaceId,
    caller: ThreadId,
    user_range: UserRange,
    phys_addr: usize,
    flags: PageFlags,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.map_region(id, caller, user_range, phys_addr, flags)
}

pub fn unmap_region(
    id: AddressSpaceId,
    caller: ThreadId,
    user_range: UserRange,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.unmap_region(id, caller, user_range)
}

pub fn remap_region(
    id: AddressSpaceId,
    caller: ThreadId,
    old_range: UserRange,
    new_range: UserRange,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.remap_region(id, caller, old_range, new_range)
}

#[allow(dead_code)]
pub fn pml4_of(id: AddressSpaceId) -> Option<usize> {
    ADDRESS_SPACE_MANAGER.pml4_phys(id)
}

/// Cleanup all address spaces owned by a thread
/// This should be called when a thread terminates to free all memory resources
pub fn cleanup_thread_address_spaces(thread_id: ThreadId, thread_primary_pml4: u64) {
    if !begin_thread_address_space_cleanup(thread_id) {
        log_info!(
            LOG_ORIGIN,
            "Address-space manager cleanup already ran for thread {} - skipping duplicate cleanup_thread_address_spaces",
            thread_id
        );
        return;
    }

    let mut spaces = ADDRESS_SPACE_MANAGER.spaces.lock();

    // Collect all address space IDs owned by this thread
    let owned_spaces: Vec<AddressSpaceId> = spaces
        .iter()
        .filter(|(_, space)| space.is_owned_by(thread_id))
        .map(|(id, _)| *id)
        .collect();

    log_info!(
        LOG_ORIGIN,
        "Cleaning up {} address spaces for thread {}",
        owned_spaces.len(),
        thread_id
    );

    // Remove each address space.
    // The thread's primary PML4 is handed off to thread teardown so only one
    // path can release it. Standalone address spaces still free their PML4 on Drop.
    for id in owned_spaces {
        if let Some(mut space) = spaces.remove(&id) {
            debug_assert!(
                matches!(space.kind(), ManagedAddressSpaceKind::Auxiliary),
                "AddressSpaceManager must only manage auxiliary address spaces"
            );
            let pml4_phys = space.pml4_phys();
            if thread_primary_pml4 != 0 && pml4_phys as u64 == thread_primary_pml4 {
                space.defer_pml4_release_to_thread_teardown();
            }

            log_info!(
                LOG_ORIGIN,
                "Removed address space {} (PML4=0x{:X}, {} mappings)",
                id,
                pml4_phys,
                space.mapping_count()
            );
            // `space` is dropped here. Whether the PML4 is freed now or later is
            // determined by `release_pml4_on_drop`.
        }
    }
}
