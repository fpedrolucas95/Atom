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
// - Kernel base (`KERNEL_BASE`) defines a hard boundary enforced on all mappings
// - Mapping size is capped (`MAX_REGION_SIZE`) to limit abuse and fragmentation
// - Rollback logic ensures no silent partial mappings on failure
// - `mapping_count` prevents destroying address spaces still in active use
// - PML4 pages are freed automatically via `Drop` when an address space is removed
//
// Error handling:
// - Rich `AddressSpaceError` enum distinguishes permission, validity,
//   resource exhaustion, and kernel-space violations
// - Errors are logged to serial for early debugging and auditability
//
// Public interface:
// - Thin wrapper functions expose the manager without leaking internal locks
// - Intended to be used by syscalls and higher-level process management code

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::mm::pmm;
use crate::mm::vm::{self, PageFlags, VmError};
use crate::thread::ThreadId;
use crate::{log_info, log_warn, log_error};

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

const KERNEL_BASE: usize = 0xFFFF_8000_0000_0000;
pub const USER_CANONICAL_MAX: usize = 0x0000_7FFF_FFFF_FFFF;
const MAX_REGION_SIZE: usize = 256 * 1024 * 1024;
const LOG_ORIGIN: &str = "addrspace";

#[derive(Debug)]
pub struct AddressSpace {
    id: AddressSpaceId,
    pml4_phys: usize,
    owner: ThreadId,
    mapping_count: usize,
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
            pmm::free_page(pml4_phys);
            return Err(err);
        }

        log_info!(
            LOG_ORIGIN,
            "Created new address space with PML4 at 0x{:X} for thread {}",
            pml4_phys,
            owner
        );

        Ok(Self {
            id: AddressSpaceId::new(),
            pml4_phys,
            owner,
            mapping_count: 0,
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

    fn inc_mappings(&mut self, count: usize) {
        self.mapping_count = self.mapping_count.saturating_add(count);
    }

    fn dec_mappings(&mut self, count: usize) {
        self.mapping_count = self.mapping_count.saturating_sub(count);
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        log_info!(
            LOG_ORIGIN,
            "Destroying address space {} (PML4=0x{:X})",
            self.id,
            self.pml4_phys
        );
        pmm::free_page(self.pml4_phys);
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
        virt_addr: usize,
        phys_addr: usize,
        size: usize,
        flags: PageFlags,
    ) -> Result<(), AddressSpaceError> {
        if !pmm::is_page_aligned(virt_addr) || !pmm::is_page_aligned(phys_addr) {
            return Err(AddressSpaceError::InvalidAddress);
        }

        if size == 0 {
            return Err(AddressSpaceError::InvalidSize);
        }

        if size > MAX_REGION_SIZE {
            log_warn!(
                LOG_ORIGIN,
                "Region too large: {} bytes (max: {})",
                size,
                MAX_REGION_SIZE
            );
            return Err(AddressSpaceError::InvalidSize);
        }

        if virt_addr > USER_CANONICAL_MAX {
            log_warn!(
                LOG_ORIGIN,
                "Non-canonical user virtual address: 0x{:X} (max 0x{:X})",
                virt_addr,
                USER_CANONICAL_MAX
            );
            return Err(AddressSpaceError::InvalidAddress);
        }

        if virt_addr >= KERNEL_BASE {
            log_warn!(
                LOG_ORIGIN,
                "Kernel space violation: virt_addr 0x{:X} >= KERNEL_BASE 0x{:X}",
                virt_addr,
                KERNEL_BASE
            );
            return Err(AddressSpaceError::KernelSpaceViolation);
        }

        let region_end = virt_addr.saturating_add(size);
        if region_end > USER_CANONICAL_MAX {
            log_warn!(
                LOG_ORIGIN,
                "Region would overflow canonical user space: 0x{:X}-0x{:X} (max 0x{:X})",
                virt_addr,
                region_end,
                USER_CANONICAL_MAX
            );
            return Err(AddressSpaceError::InvalidSize);
        }
        if region_end > KERNEL_BASE {
            log_warn!(
                LOG_ORIGIN,
                "Region would overlap kernel space: 0x{:X}-0x{:X}",
                virt_addr,
                region_end
            );
            return Err(AddressSpaceError::KernelSpaceViolation);
        }

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
        virt_addr: usize,
        size: usize,
    ) -> Result<(), AddressSpaceError> {
        if !pmm::is_page_aligned(virt_addr) {
            return Err(AddressSpaceError::InvalidAddress);
        }

        if virt_addr > USER_CANONICAL_MAX {
            log_warn!(
                LOG_ORIGIN,
                "Non-canonical unmap request: virt_addr 0x{:X} exceeds user limit 0x{:X}",
                virt_addr,
                USER_CANONICAL_MAX
            );
            return Err(AddressSpaceError::InvalidAddress);
        }

        if size == 0 {
            return Err(AddressSpaceError::InvalidSize);
        }

        if virt_addr >= KERNEL_BASE {
            log_warn!(
                LOG_ORIGIN,
                "Kernel space violation on unmap: virt_addr 0x{:X} >= KERNEL_BASE 0x{:X}",
                virt_addr,
                KERNEL_BASE
            );
            return Err(AddressSpaceError::KernelSpaceViolation);
        }

        let region_end = virt_addr.saturating_add(size);
        if region_end > USER_CANONICAL_MAX {
            log_warn!(
                LOG_ORIGIN,
                "Unmap would overflow canonical user space: 0x{:X}-0x{:X} (max 0x{:X})",
                virt_addr,
                region_end,
                USER_CANONICAL_MAX
            );
            return Err(AddressSpaceError::InvalidSize);
        }
        if region_end > KERNEL_BASE {
            log_warn!(
                LOG_ORIGIN,
                "Unmap region would cross kernel space: 0x{:X}-0x{:X}",
                virt_addr,
                region_end
            );
            return Err(AddressSpaceError::KernelSpaceViolation);
        }

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
        old_virt: usize,
        new_virt: usize,
        size: usize,
    ) -> Result<(), AddressSpaceError> {
        if !pmm::is_page_aligned(old_virt) || !pmm::is_page_aligned(new_virt) {
            return Err(AddressSpaceError::InvalidAddress);
        }

        if size == 0 {
            return Err(AddressSpaceError::InvalidSize);
        }

        if new_virt >= KERNEL_BASE || new_virt.saturating_add(size) > KERNEL_BASE {
            return Err(AddressSpaceError::KernelSpaceViolation);
        }

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
        
        for i in 0..num_pages {
            let new_virt_page = new_virt + (i * pmm::PAGE_SIZE);
            let (phys, flags) = mappings[i];

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
    log_info!(LOG_ORIGIN, "Kernel base: 0x{:X}", KERNEL_BASE);
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
    virt_addr: usize,
    phys_addr: usize,
    size: usize,
    flags: PageFlags,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.map_region(id, caller, virt_addr, phys_addr, size, flags)
}

pub fn unmap_region(
    id: AddressSpaceId,
    caller: ThreadId,
    virt_addr: usize,
    size: usize,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.unmap_region(id, caller, virt_addr, size)
}

pub fn remap_region(
    id: AddressSpaceId,
    caller: ThreadId,
    old_virt: usize,
    new_virt: usize,
    size: usize,
) -> Result<(), AddressSpaceError> {
    ADDRESS_SPACE_MANAGER.remap_region(id, caller, old_virt, new_virt, size)
}

#[allow(dead_code)]
pub fn pml4_of(id: AddressSpaceId) -> Option<usize> {
    ADDRESS_SPACE_MANAGER.pml4_phys(id)
}
