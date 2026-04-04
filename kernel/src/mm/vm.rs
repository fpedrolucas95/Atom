// Virtual Memory Manager (VMM)
//
// Implements x86_64 paging, page-table management, and virtual-to-physical
// address translation for the kernel. This module is the backbone of memory
// isolation, device mapping, and higher-half kernel operation.
//
// Key responsibilities:
// - Initialize the kernel page table (PML4) from the UEFI memory map
// - Establish identity mappings and higher-half kernel mirrors
// - Manage page tables (PML4/PDPT/PD/PT) dynamically on demand
// - Map, unmap, remap, and query individual virtual memory pages
// - Enforce correct access permissions and cacheability attributes
//
// Address space model:
// - Uses 4-level paging (PML4 → PDPT → PD → PT) with 4 KiB pages
// - Kernel runs in the higher half (`HIGHER_HALF_BASE`) with mirrored RAM
// - User address spaces clone kernel mappings from the active PML4
//
// Design principles:
// - Correctness-first: explicit checks for alignment and initialization
// - Lazy allocation of page tables to minimize memory usage
// - Strong separation between physical allocation (PMM) and mapping logic
// - Explicit accounting of mapped pages and page-table pages
//
// Initialization details:
// - Allocates and zeroes a fresh PML4
// - Identity-maps all usable RAM regions from the UEFI memory map
// - Mirrors low physical memory into the higher half for kernel access
// - Maps critical MMIO regions (VGA, Local APIC, I/O APIC)
// - Activates the new address space by loading CR3
//
// Permission and flag handling:
// - `PageFlags` abstracts hardware PTE bits (P, RW, NX, cache control)
// - UEFI memory attributes are translated into page-level flags
// - Non-code and XP-marked regions are mapped non-executable by default
// - Write-protected regions drop the writable flag automatically
//
// Runtime services:
// - Page mapping APIs for the active PML4 or an explicit PML4 root
// - Translation helpers for debugging and verification
// - Stack safety helper to ensure the current kernel stack is fully mapped
//
// Correctness and safety notes:
// - TLB is explicitly invalidated (`invlpg`) on mapping changes
// - All page-table memory is allocated zeroed to avoid stale entries
// - Failure to keep kernel mappings consistent across address spaces
//   will result in hard-to-debug page faults or triple faults
//
// Diagnostics and testing:
// - Extensive serial logging during initialization
// - Mapping verification helpers for early fault detection
// - Built-in `self_test()` validates core map/remap/unmap logic
//
// Limitations and future work:
// - No support for huge pages (2 MiB / 1 GiB)
// - No per-process ASIDs or PCIDs
// - No copy-on-write or demand paging yet

use core::arch::asm;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

use crate::mm::pmm;
use crate::mm::ValidationError;
use crate::boot::{EfiMemoryDescriptor, MemoryMap};

use crate::{log_debug, log_info, log_warn, log_error};

const EFI_LOADER_CODE: u32 = 1;
const EFI_LOADER_DATA: u32 = 2;
const EFI_BOOT_SERVICES_CODE: u32 = 3;
const EFI_BOOT_SERVICES_DATA: u32 = 4;
const EFI_RUNTIME_SERVICES_CODE: u32 = 5;
const EFI_RUNTIME_SERVICES_DATA: u32 = 6;
const EFI_CONVENTIONAL_MEMORY: u32 = 7;
const EFI_ACPI_RECLAIM_MEMORY: u32 = 9;
const EFI_ACPI_MEMORY_NVS: u32 = 10;
const EFI_PERSISTENT_MEMORY: u32 = 14;
const EFI_MEMORY_UC: u64 = 0x0000_0000_0000_0001;
const EFI_MEMORY_WC: u64 = 0x0000_0000_0000_0002;
const EFI_MEMORY_WT: u64 = 0x0000_0000_0000_0004;
const EFI_MEMORY_WB: u64 = 0x0000_0000_0000_0008;
const EFI_MEMORY_UCE: u64 = 0x0000_0000_0000_0010;
const EFI_MEMORY_WP: u64 = 0x0000_0000_0000_1000;
const EFI_MEMORY_RP: u64 = 0x0000_0000_0000_2000;
const EFI_MEMORY_XP: u64 = 0x8000_0000_0000_0000;
const ENTRIES_PER_TABLE: usize = 512;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
/// Higher-half kernel virtual memory base address
pub const HIGHER_HALF_BASE: usize = 0xFFFF_8000_0000_0000;
/// Higher-half mirror covers up to 16 GiB of physical RAM.
/// This must be >= the PMM's static bitmap coverage (16 GiB)
/// to ensure all tracked physical memory is accessible via the
/// kernel's higher-half virtual addresses.
const HIGHER_HALF_MIRROR_SIZE: usize = 16usize * 1024 * 1024 * 1024;
static ACTIVE_PML4: AtomicUsize = AtomicUsize::new(0);
static MAPPED_PAGES: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_PAGES: AtomicUsize = AtomicUsize::new(0);
const DEFERRED_TLB_FLUSH_SLOTS: usize = 64;
static DEFERRED_TLB_FLUSH_MASK: AtomicU64 = AtomicU64::new(0);
/// Global page-table serialization lock.
///
/// Current kernel targets UP semantics but can still interleave memory
/// operations via preemption/interrupt paths. This lock guarantees that
/// PTE decisions and updates are performed atomically from the MM layer
/// perspective, and establishes a strict lock hierarchy for COW/fork paths.
static PAGE_TABLE_LOCK: Mutex<()> = Mutex::new(());
/// Highest physical address that was identity-mapped during init.
/// Shared memory VA allocation uses this to start above all identity-mapped
/// regions, avoiding costly page-table probing and collision avoidance.
static IDENTITY_MAP_CEILING: AtomicUsize = AtomicUsize::new(0);
/// Set to `true` after `init()` completes and the higher-half mirror is active.
/// Before this flag is set, page-table structures are accessed via their
/// identity-mapped (phys == virt) addresses (UEFI page tables are still active).
/// After this flag is set, all accesses go through the higher-half mirror
/// (`HIGHER_HALF_BASE + phys`), which is shared across every address space
/// (PML4 entries 256-511).  This avoids relying on lower-half identity mappings
/// that user processes may have partially overwritten with their own pages.
static HIGHER_HALF_READY: AtomicBool = AtomicBool::new(false);

/// Convert a physical address to a virtual address suitable for kernel access (safe version).
///
/// This is the safe wrapper that validates the higher-half mirror is ready before
/// performing the translation. It returns an error if called before `vm::init()`
/// completes, preventing undefined behavior from accessing unmapped addresses.
///
/// Before `vm::init()` completes, returns the identity-mapped address (phys == virt)
/// because the UEFI page tables are still active.  After init, returns the
/// higher-half mirror address (`HIGHER_HALF_BASE + phys`), which is guaranteed
/// to be valid in every address space since higher-half PML4 entries are shared.
///
/// # Arguments
/// * `phys` - Physical address to convert
///
/// # Returns
/// * `Ok(virt)` - Virtual address corresponding to the physical address
/// * `Err(ValidationError::NotInitialized)` - If called before higher-half initialization
///
/// # Examples
/// ```
/// // After vm::init() completes
/// let virt = phys_to_virt_ptr_safe(0x1000)?;
/// assert_eq!(virt, HIGHER_HALF_BASE + 0x1000);
/// ```
pub fn phys_to_virt_ptr_safe(phys: usize) -> Result<usize, ValidationError> {
    crate::mm::validate_initialized(HIGHER_HALF_READY.load(Ordering::Relaxed))?;
    Ok(HIGHER_HALF_BASE + phys)
}

/// Convert a physical address to a virtual address suitable for kernel access.
///
/// This is the unsafe convenience wrapper that panics if called incorrectly.
/// Prefer using `phys_to_virt_ptr_safe()` for operations that need error handling.
///
/// Before `vm::init()` completes, returns the identity-mapped address (phys == virt)
/// because the UEFI page tables are still active.  After init, returns the
/// higher-half mirror address (`HIGHER_HALF_BASE + phys`), which is guaranteed
/// to be valid in every address space since higher-half PML4 entries are shared.
///
/// This MUST be used for all page-table structure accesses to prevent corruption
/// when running inside a user process whose lower-half identity mappings have
/// been partially overwritten by user-page remaps.
#[inline]
pub fn phys_to_virt_ptr(phys: usize) -> usize {
    if HIGHER_HALF_READY.load(Ordering::Relaxed) {
        phys_to_virt_ptr_safe(phys).expect("higher-half VM access requires initialized mirror")
    } else {
        phys
    }
}

/// Convert a kernel virtual address back to a physical address.
///
/// This is the inverse of [`phys_to_virt_ptr`].  Before `vm::init()` completes
/// the two are identical (identity map); after init the higher-half mirror is
/// active and `virt = HIGHER_HALF_BASE + phys`.
///
/// Prefer this over the raw `virt - HIGHER_HALF_BASE` arithmetic so that the
/// VA–PA relationship is expressed in one place and callers remain correct if
/// the mapping scheme ever changes.
#[inline]
pub fn virt_to_phys(virt: usize) -> usize {
    if HIGHER_HALF_READY.load(Ordering::Relaxed) {
        virt - HIGHER_HALF_BASE
    } else {
        virt
    }
}
const LOG_ORIGIN: &str = "vmm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    Validation(ValidationError),
    AlreadyMapped,
    NotMapped,
    OutOfMemory,
}

impl From<ValidationError> for VmError {
    fn from(value: ValidationError) -> Self {
        Self::Validation(value)
    }
}

fn map_user_address_validation_error(
    addr: usize,
    err: atom_abi::UserAddressError,
) -> ValidationError {
    match err {
        atom_abi::UserAddressError::EmptyRange => ValidationError::InvalidSize {
            size: 0,
            max_size: usize::MAX,
        },
        atom_abi::UserAddressError::Overflow => ValidationError::OutOfBounds {
            addr,
            min: atom_abi::USER_SPACE_MIN as usize,
            max: atom_abi::USER_SPACE_MAX as usize - 1,
        },
        atom_abi::UserAddressError::NonCanonical
        | atom_abi::UserAddressError::BelowUserMin
        | atom_abi::UserAddressError::AboveUserMax => ValidationError::OutOfBounds {
            addr,
            min: atom_abi::USER_SPACE_MIN as usize,
            max: atom_abi::USER_SPACE_MAX as usize - 1,
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageFlags(u64);

impl PageFlags {
    pub const PRESENT: Self = Self(1 << 0);
    pub const WRITABLE: Self = Self(1 << 1);
    #[allow(dead_code)]
    pub const USER: Self = Self(1 << 2);
    pub const WRITE_THROUGH: Self = Self(1 << 3);
    pub const CACHE_DISABLE: Self = Self(1 << 4);
    pub const GLOBAL: Self = Self(1 << 8);
    /// Software-only COW marker in the leaf PTE.
    /// Uses AVL bit 9 (ignored by x86 hardware for translation).
    pub const SOFT_COW: Self = Self(1 << 9);
    pub const NO_EXECUTE: Self = Self(1u64 << 63);

    pub const fn kernel_rw() -> Self {
        Self(Self::PRESENT.bits() | Self::WRITABLE.bits() | Self::GLOBAL.bits())
    }

    #[allow(dead_code)]
    pub const fn kernel_rw_nx() -> Self {
        Self(Self::kernel_rw().bits() | Self::NO_EXECUTE.bits())
    }

    pub const fn with_nx(self) -> Self {
        Self(self.bits() | Self::NO_EXECUTE.bits())
    }

    pub const fn with(self, other: PageFlags) -> Self {
        Self(self.bits() | other.bits())
    }

    pub const fn without(self, other: PageFlags) -> Self {
        Self(self.bits() & !other.bits())
    }

    pub const fn contains(self, other: PageFlags) -> bool {
        (self.bits() & other.bits()) == other.bits()
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }
}

impl core::ops::BitOr for PageFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.bits() | rhs.bits())
    }
}

impl core::ops::BitOrAssign for PageFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.bits();
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct PageTableEntry(u64);

impl PageTableEntry {
    #[allow(dead_code)]
    const fn empty() -> Self {
        Self(0)
    }

    fn is_present(&self) -> bool {
        self.0 & PageFlags::PRESENT.bits() != 0
    }

    fn addr(&self) -> usize {
        (self.0 & ADDR_MASK) as usize
    }

    fn set(&mut self, addr: usize, flags: PageFlags) {
        self.0 = (addr as u64 & ADDR_MASK) | flags.bits();
    }

    fn clear(&mut self) {
        self.0 = 0;
    }
}

#[repr(align(4096))]
struct PageTable {
    entries: [PageTableEntry; ENTRIES_PER_TABLE],
}

impl PageTable {
    #[allow(dead_code)]
    const fn new() -> Self {
        Self {
            entries: [PageTableEntry::empty(); ENTRIES_PER_TABLE],
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct VmStats {
    pub active_pml4: usize,
    pub mapped_pages: usize,
    pub page_table_pages: usize,
}

pub fn init(memory_map: &MemoryMap) {
    log_info!(LOG_ORIGIN, "Initializing virtual memory manager...");

    let pml4_phys = pmm::alloc_page_zeroed().expect("Failed to allocate PML4");
    PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);
    ACTIVE_PML4.store(pml4_phys, Ordering::Relaxed);
    log_info!(LOG_ORIGIN, "PML4 allocated at 0x{:X}", pml4_phys);
    log_info!(LOG_ORIGIN, "Starting identity mapping of RAM regions...");
    let mut max_physical_addr = 0usize;
    for desc in memory_map.descriptors() {
        if !is_mappable_ram(desc.typ) {
            continue;
        }

        let region_start = pmm::align_down(desc.physical_start as usize);
        let region_end = pmm::align_up(
            (desc.physical_start as usize) + (desc.number_of_pages as usize * pmm::PAGE_SIZE),
        );

        if region_end > max_physical_addr {
            max_physical_addr = region_end;
        }

        let page_flags = flags_for_descriptor(desc);

        for phys in (region_start..region_end).step_by(pmm::PAGE_SIZE) {
            if let Err(err) = map_page_internal(pml4_phys, phys, phys, page_flags) {
                if err != VmError::AlreadyMapped {
                    log_error!(
                        LOG_ORIGIN,
                        "Failed to map identity page 0x{:X} (err: {:?})",
                        phys,
                        err
                    );
                }
            }

            if phys < HIGHER_HALF_MIRROR_SIZE {
                let higher_half = HIGHER_HALF_BASE + phys;
                if let Err(err) = map_page_internal(
                    pml4_phys,
                    higher_half,
                    phys,
                    page_flags,
                ) {
                    if err != VmError::AlreadyMapped {
                        log_error!(
                            LOG_ORIGIN,
                            "Failed to mirror page 0x{:X} -> 0x{:X} (err: {:?})",
                            phys,
                            higher_half,
                            err
                        );
                    }
                }
            }
        }
    }

    let _ = map_page_internal(pml4_phys, pml4_phys, pml4_phys, PageFlags::kernel_rw());

    log_info!(LOG_ORIGIN, "Mapping VGA text buffer at 0xB8000...");
    let vga_flags = PageFlags(
        PageFlags::PRESENT.bits() |
        PageFlags::WRITABLE.bits() |
        PageFlags::CACHE_DISABLE.bits() |
        PageFlags::GLOBAL.bits()
    );

    for offset in (0..8).map(|i| i * pmm::PAGE_SIZE) {
        let vga_addr = 0xB8000 + offset;
        match map_page_internal(pml4_phys, vga_addr, vga_addr, vga_flags) {
            Ok(()) => {
                log_debug!(LOG_ORIGIN, "Mapped VGA page 0x{:X}", vga_addr);
            }
            Err(err) => {
                log_error!(LOG_ORIGIN, "Failed to map VGA buffer page 0x{:X} (err: {:?})", vga_addr, err);
            }
        }
    }

    log_info!(LOG_ORIGIN, "Mapping Local APIC at 0xFEE00000...");
    let apic_addr = 0xFEE00000;
    let apic_flags = PageFlags(
        PageFlags::PRESENT.bits() |
        PageFlags::WRITABLE.bits() |
        PageFlags::CACHE_DISABLE.bits() |
        PageFlags::GLOBAL.bits()
    );

    match map_page_internal(pml4_phys, apic_addr, apic_addr, apic_flags) {
        Ok(()) => {
            log_info!(LOG_ORIGIN, "Mapped APIC at 0x{:X}", apic_addr);
        }
        Err(err) => {
            log_error!(LOG_ORIGIN, "Failed to map APIC at 0x{:X} (err: {:?})", apic_addr, err);
        }
    }
    
    log_info!(LOG_ORIGIN, "Mapping I/O APIC at 0xFEC00000...");
    let ioapic_addr = 0xFEC00000;
    let ioapic_flags = PageFlags(
        PageFlags::PRESENT.bits() |
        PageFlags::WRITABLE.bits() |
        PageFlags::CACHE_DISABLE.bits() |
        PageFlags::GLOBAL.bits()
    );

    match map_page_internal(pml4_phys, ioapic_addr, ioapic_addr, ioapic_flags) {
        Ok(()) => {
            log_info!(LOG_ORIGIN, "Mapped I/O APIC at 0x{:X}", ioapic_addr);
        }
        Err(err) => {
            log_error!(LOG_ORIGIN, "Failed to map I/O APIC at 0x{:X} (err: {:?})", ioapic_addr, err);
        }
    }

    unsafe {
        load_cr3(pml4_phys as u64);
    }

    // The higher-half mirror is now active.  From this point on, all
    // page-table structure accesses must go through phys_to_virt_ptr()
    // to avoid depending on lower-half identity mappings that user
    // processes will overwrite.
    HIGHER_HALF_READY.store(true, Ordering::Release);

    // Record the identity-map ceiling so that shared-memory VA allocation
    // can start above all identity-mapped pages.
    IDENTITY_MAP_CEILING.store(max_physical_addr, Ordering::Relaxed);

    // Register the kernel PML4 as protected (Req 2.4)
    let _ = pmm::register_active_pml4(pml4_phys);

    log_info!(
        LOG_ORIGIN,
        "New address space active (PML4=0x{:X}, mapped ~{} MiB)",
        pml4_phys,
        max_physical_addr / (1024 * 1024)
    );
}

/// Return the highest physical address that was identity-mapped during
/// `vm::init()`.  The shared memory allocator uses this to place mappings
/// above the identity-mapped region, avoiding collisions entirely.
pub fn identity_map_ceiling() -> usize {
    IDENTITY_MAP_CEILING.load(Ordering::Relaxed)
}

pub fn map_framebuffer(fb_addr: u64, fb_size: usize) -> bool {
    log_info!(LOG_ORIGIN, "Mapping framebuffer at 0x{:X}, size {} bytes...", fb_addr, fb_size);

    // Include USER flag so userspace drivers can access the framebuffer
    let fb_flags = PageFlags(
        PageFlags::PRESENT.bits() |
        PageFlags::WRITABLE.bits() |
        PageFlags::USER.bits() |       // Allow userspace access
        PageFlags::CACHE_DISABLE.bits() |
        PageFlags::GLOBAL.bits() |
        PageFlags::NO_EXECUTE.bits()
    );

    let fb_start = pmm::align_down(fb_addr as usize);
    let fb_end = pmm::align_up((fb_addr as usize) + fb_size);
    let mut mapped_count = 0usize;
    let mut error_count = 0usize;

    for phys in (fb_start..fb_end).step_by(pmm::PAGE_SIZE) {
        match map_page(phys, phys, fb_flags) {
            Ok(()) => {
                mapped_count += 1;
            }
            Err(VmError::AlreadyMapped) => {
                mapped_count += 1;
            }
            Err(err) => {
                log_error!(LOG_ORIGIN, "Failed to map framebuffer page 0x{:X} (err: {:?})", phys, err);
                error_count += 1;
            }
        }
    }

    let total_pages = (fb_end - fb_start) / pmm::PAGE_SIZE;
    log_info!(
        LOG_ORIGIN,
        "Framebuffer mapping complete: {}/{} pages (errors: {})",
        mapped_count,
        total_pages,
        error_count
    );

    error_count == 0
}

pub fn ensure_current_stack_mapped(pages: usize) -> bool {
    if pages == 0 {
        return true;
    }

    let rsp: usize;
    unsafe {
        asm!("mov {}, rsp", out(reg) rsp, options(nomem, nostack, preserves_flags));
    }

    let rsp_translation = translate(rsp);
    let top = pmm::align_down(rsp);
    let start = top.saturating_sub((pages - 1) * pmm::PAGE_SIZE);
    let mut success = true;
    let mut newly_mapped = 0usize;

    log_info!(
        LOG_ORIGIN,
        "Verifying stack mapping from 0x{:X} down to 0x{:X} ({} pages); RSP=0x{:X} (phys={:?})",
        top,
        start,
        pages,
        rsp,
        rsp_translation
    );

    for page in (start..=top).step_by(pmm::PAGE_SIZE) {
        if translate(page).is_some() {
            continue;
        }

        let flags = PageFlags::kernel_rw().with_nx();
        match map_page(page, page, flags) {
            Ok(()) => {
                log_debug!(LOG_ORIGIN, "Mapped missing stack page 0x{:X}", page);
                newly_mapped += 1;
            }
            Err(VmError::AlreadyMapped) => {
            }
            Err(err) => {
                log_error!(
                    LOG_ORIGIN,
                    "Failed to map stack page 0x{:X} (err: {:?})",
                    page,
                    err
                );
                success = false;
            }
        }
    }

    log_info!(
        LOG_ORIGIN,
        "Stack verification complete: {} new mappings; top page phys={:?}, start phys={:?}",
        newly_mapped,
        translate(top),
        translate(start)
    );

    success
}

pub fn map_page(virt: usize, phys: usize, flags: PageFlags) -> Result<(), VmError> {
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_page_alignment(phys)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;
    let _lock = PAGE_TABLE_LOCK.lock();
    let result = map_page_internal(pml4_phys, virt, phys, flags);
    if result.is_ok() {
        invalidate_tlb_for_pml4_page(pml4_phys, virt);
    }
    result
}

pub fn map_page_in_pml4(pml4_phys: usize, virt: usize, phys: usize, flags: PageFlags) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_page_alignment(phys)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;

    // Validate user-space bounds if this is a user-space mapping
    let is_user_mapping = (flags.bits() & PageFlags::USER.bits()) != 0;
    if is_user_mapping {
        atom_abi::validate_user_range(virt, pmm::PAGE_SIZE)
            .map_err(|err| VmError::Validation(map_user_address_validation_error(virt, err)))?;
    }
    let _lock = PAGE_TABLE_LOCK.lock();
    let result = map_page_internal(pml4_phys, virt, phys, flags);
    if result.is_ok() {
        invalidate_tlb_for_pml4_page(pml4_phys, virt);
    }
    result
}

/// Check whether a page is already present (mapped) in a specific PML4.
///
/// This is used by fault handlers and pre-fault paths to avoid replacing
/// an existing mapping with a freshly zeroed page — which would destroy
/// live stack/heap data.
pub fn is_page_present_in_pml4(pml4_phys: usize, virt: usize) -> bool {
    if pml4_phys == 0 || !pmm::is_page_aligned(virt) {
        return false;
    }
    let _lock = PAGE_TABLE_LOCK.lock();
    match walk_to_entry_with_root_user(pml4_phys, virt, false, true) {
        Ok((entry, _)) => entry.is_present(),
        Err(_) => false,
    }
}

/// Map a page in a specific PML4, overwriting any existing mapping.
/// This is used when creating new processes that may share page table structures
/// with the kernel but need their own mappings in user space regions.
pub fn remap_page_in_pml4(pml4_phys: usize, virt: usize, phys: usize, flags: PageFlags) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_page_alignment(phys)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;

    // Se for mapeamento user, precisamos que TODOS os níveis tenham USER
    let user_access = (flags.bits() & PageFlags::USER.bits()) != 0;

    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _created_table) = walk_to_entry_with_root_user(pml4_phys, virt, true, user_access)?;

    // Overwrite existing entry if present (unlike map_page_internal which fails)
    if !entry.is_present() {
        MAPPED_PAGES.fetch_add(1, Ordering::Relaxed);
    }

    entry.set(phys, flags);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);

    Ok(())
}

/// Update only the flags of an existing mapping in a specific PML4.
pub fn remap_page_flags_in_pml4(
    pml4_phys: usize,
    virt: usize,
    new_flags: PageFlags,
) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;
    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _created_table) = walk_to_entry_with_root_user(pml4_phys, virt, false, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }
    let phys = entry.addr();
    entry.set(phys, new_flags);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);
    Ok(())
}

/// Copy one physical page into another.
pub fn copy_phys_page(dst_phys: usize, src_phys: usize) {
    unsafe {
        core::ptr::copy_nonoverlapping(
            phys_to_virt_ptr(src_phys) as *const u8,
            phys_to_virt_ptr(dst_phys) as *mut u8,
            pmm::PAGE_SIZE,
        );
    }
}

pub fn clone_kernel_mappings(dst_pml4_phys: usize) -> Result<(), VmError> {
    let src_pml4 = ACTIVE_PML4.load(Ordering::Relaxed);
    crate::mm::validate_page_alignment(dst_pml4_phys)?;
    crate::mm::validate_initialized(src_pml4 != 0)?;

    let src = unsafe { &*(phys_to_virt_ptr(src_pml4) as *const PageTable) };
    let dst = unsafe { &mut *(phys_to_virt_ptr(dst_pml4_phys) as *mut PageTable) };

    // For higher half (kernel space at 0xFFFF_8000+): share page tables
    // These are kernel mappings that don't change per-process
    for idx in ENTRIES_PER_TABLE / 2..ENTRIES_PER_TABLE {
        dst.entries[idx] = src.entries[idx];
    }

    // For lower half: we need to deep-copy page tables to isolate user space.
    // PML4[0] contains both kernel identity mapping and user code (0x400000).
    // We must deep-copy so each process has its own page tables for this region.
    for idx in 0..ENTRIES_PER_TABLE / 2 {
        if !src.entries[idx].is_present() {
            dst.entries[idx].clear();
            continue;
        }

        // Deep copy PDPT for this PML4 entry
        let src_pdpt_phys = src.entries[idx].addr();
        let dst_pdpt_phys = match pmm::alloc_page_zeroed() {
            Some(p) => p,
            None => return Err(VmError::OutOfMemory),
        };
        PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);

        let src_pdpt = unsafe { &*(phys_to_virt_ptr(src_pdpt_phys) as *const PageTable) };
        let dst_pdpt = unsafe { &mut *(phys_to_virt_ptr(dst_pdpt_phys) as *mut PageTable) };

        for pdpt_idx in 0..ENTRIES_PER_TABLE {
            if !src_pdpt.entries[pdpt_idx].is_present() {
                dst_pdpt.entries[pdpt_idx].clear();
                continue;
            }

            // Deep copy PD for this PDPT entry
            let src_pd_phys = src_pdpt.entries[pdpt_idx].addr();
            let dst_pd_phys = match pmm::alloc_page_zeroed() {
                Some(p) => p,
                None => return Err(VmError::OutOfMemory),
            };
            PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);

            let src_pd = unsafe { &*(phys_to_virt_ptr(src_pd_phys) as *const PageTable) };
            let dst_pd = unsafe { &mut *(phys_to_virt_ptr(dst_pd_phys) as *mut PageTable) };

            for pd_idx in 0..ENTRIES_PER_TABLE {
                if !src_pd.entries[pd_idx].is_present() {
                    dst_pd.entries[pd_idx].clear();
                    continue;
                }

                // Check if this is a 2MB huge page or points to a PT
                if (src_pd.entries[pd_idx].0 & (1 << 7)) != 0 {
                    // 2MB huge page - just copy the entry verbatim
                    dst_pd.entries[pd_idx] = src_pd.entries[pd_idx];
                } else {
                    // Points to a PT - deep copy it
                    let src_pt_phys = src_pd.entries[pd_idx].addr();
                    let dst_pt_phys = match pmm::alloc_page_zeroed() {
                        Some(p) => p,
                        None => return Err(VmError::OutOfMemory),
                    };
                    PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);

                    // Copy all PT entries using volatile writes to prevent
                    // the compiler from optimising away stores to hardware-
                    // walked page-table pages.
                    let src_pt = phys_to_virt_ptr(src_pt_phys) as *const PageTableEntry;
                    let dst_pt = phys_to_virt_ptr(dst_pt_phys) as *mut PageTableEntry;
                    unsafe {
                        for pt_idx in 0..ENTRIES_PER_TABLE {
                            let val = core::ptr::read_volatile(src_pt.add(pt_idx));
                            core::ptr::write_volatile(dst_pt.add(pt_idx), val);
                        }
                    }

                    // Set PD entry: swap physical address, keep ALL flags
                    // (low 12 bits + high bits like NX at bit 63)
                    let raw = src_pd.entries[pd_idx].0;
                    let flags = raw & !ADDR_MASK;
                    unsafe {
                        let dst_pde = &mut (*(phys_to_virt_ptr(dst_pd_phys) as *mut PageTable)).entries[pd_idx];
                        core::ptr::write_volatile(
                            &mut dst_pde.0 as *mut u64,
                            (dst_pt_phys as u64 & ADDR_MASK) | flags,
                        );
                    }
                }
            }

            // Set PDPT entry: swap physical address, keep ALL flags
            let raw = src_pdpt.entries[pdpt_idx].0;
            let flags = raw & !ADDR_MASK;
            unsafe {
                let dst_pdpte = &mut (*(phys_to_virt_ptr(dst_pdpt_phys) as *mut PageTable)).entries[pdpt_idx];
                core::ptr::write_volatile(
                    &mut dst_pdpte.0 as *mut u64,
                    (dst_pd_phys as u64 & ADDR_MASK) | flags,
                );
            }
        }

        // Set PML4 entry: swap physical address, keep ALL flags
        let raw = src.entries[idx].0;
        let flags = raw & !ADDR_MASK;
        unsafe {
            let dst_pml4e = &mut (*(phys_to_virt_ptr(dst_pml4_phys) as *mut PageTable)).entries[idx];
            core::ptr::write_volatile(
                &mut dst_pml4e.0 as *mut u64,
                (dst_pdpt_phys as u64 & ADDR_MASK) | flags,
            );
        }
    }

    // ------------------------------------------------------------------
    // Verification pass: walk the destination PML4 for a few critical
    // kernel addresses.  If any mapping is missing, repair it by copying
    // the leaf PT entry from the source PML4.
    // ------------------------------------------------------------------
    verify_and_repair_clone(src_pml4, dst_pml4_phys);

    // ------------------------------------------------------------------
    // Null-page guard: explicitly unmap VA 0x0 from every new user address
    // space.  The deep copy above replicates the kernel's identity map
    // which may include physical page 0 (first 4 KiB).  Without this guard
    // a null function-pointer call in userspace hits a *present* page
    // (error_code P=1, I/D=1, U=1 → 0x15) instead of the expected
    // not-present fault (0x14), masking null-deref bugs and making them
    // look like mysterious instruction-fetch protection violations
    // (observed: Doom #PF at RIP=0x0, error_code=0x15 immediately after
    // returning from read()).
    //
    // We tolerate VmError::NotMapped silently: if the kernel never mapped
    // page 0 (e.g. UEFI left address 0 unmapped) there is nothing to clear.
    // ------------------------------------------------------------------
    match unmap_page_in_pml4(dst_pml4_phys, 0) {
        Ok(()) => {
            log_debug!(
                LOG_ORIGIN,
                "clone_kernel_mappings: null-page guard applied (cleared VA 0x0 in PML4 0x{:X})",
                dst_pml4_phys
            );
        }
        Err(VmError::NotMapped) => {
            // VA 0 was already absent — nothing to do.
        }
        Err(e) => {
            log_warn!(
                LOG_ORIGIN,
                "clone_kernel_mappings: null-page guard failed for PML4 0x{:X}: {:?}",
                dst_pml4_phys,
                e
            );
        }
    }

    Ok(())
}

/// Walk `pml4_phys` for virtual address `virt` and return the leaf PTE,
/// or `None` if any intermediate level is not-present.
pub fn read_pte_in_pml4(pml4_phys: usize, virt: usize) -> Option<u64> {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);

    let pml4 = unsafe { &*(phys_to_virt_ptr(pml4_phys) as *const PageTable) };
    let e = &pml4.entries[pml4_idx];
    if !e.is_present() { return None; }

    let pdpt = unsafe { &*(phys_to_virt_ptr(e.addr()) as *const PageTable) };
    let e = &pdpt.entries[pdpt_idx];
    if !e.is_present() { return None; }

    let pd = unsafe { &*(phys_to_virt_ptr(e.addr()) as *const PageTable) };
    let e = &pd.entries[pd_idx];
    if !e.is_present() { return None; }

    // 2 MiB huge page – treat as present
    if e.0 & (1 << 7) != 0 { return Some(e.0); }

    let pt = unsafe { &*(phys_to_virt_ptr(e.addr()) as *const PageTable) };
    let e = &pt.entries[pt_idx];
    if !e.is_present() { return None; }

    Some(e.0)
}

#[inline]
pub fn is_cow(flags: PageFlags) -> bool {
    flags.contains(PageFlags::SOFT_COW)
}

#[inline]
pub fn set_cow(flags: PageFlags) -> PageFlags {
    flags.with(PageFlags::SOFT_COW).without(PageFlags::WRITABLE)
}

#[inline]
pub fn clear_cow(flags: PageFlags) -> PageFlags {
    flags.without(PageFlags::SOFT_COW)
}

/// Verify several critical kernel-code pages in `dst` against `src`.
/// For every page that is mapped in `src` but missing in `dst`, walk
/// the hierarchy and repair at the deepest missing level.
fn verify_and_repair_clone(src_pml4: usize, dst_pml4: usize) {
    // Get the current kernel RIP – this is the most critical address
    // that must be identity-mapped in every process PML4, since the
    // kernel runs from identity-mapped code after syscall entry.
    let current_rip: usize;
    unsafe { asm!("lea {}, [rip]", out(reg) current_rip, options(nomem, nostack)); }
    let rip_page = current_rip & !(0xFFF);

    // Sample a range of pages around the kernel binary.  Identity
    // mappings for these pages must survive the deep copy.
    let test_addrs: [usize; 8] = [
        rip_page,
        rip_page.wrapping_sub(0x1000),
        rip_page.wrapping_add(0x1000),
        rip_page.wrapping_add(0x10000),
        // VGA text buffer
        0xB8000,
        // APIC
        0xFEE00000,
        // Low identity-mapped pages
        0x1000,
        0x100000,
    ];

    for &virt in &test_addrs {
        let virt_page = virt & !(0xFFF);
        let src_pte = read_pte_in_pml4(src_pml4, virt_page);
        let dst_pte = read_pte_in_pml4(dst_pml4, virt_page);

        match (src_pte, dst_pte) {
            (Some(s), None) => {
                // Mapping exists in source but missing in destination → repair
                log_error!(
                    LOG_ORIGIN,
                    "clone_kernel_mappings: VA 0x{:X} present in src PML4 but MISSING in dst PML4 0x{:X} – repairing",
                    virt_page,
                    dst_pml4
                );
                let phys = (s & ADDR_MASK) as usize;
                let flags = PageFlags::from_bits(s & !ADDR_MASK);
                let _ = map_page_internal(dst_pml4, virt_page, phys, flags);
            }
            (Some(s), Some(d)) => {
                // Both present – verify physical address matches
                let s_phys = s & ADDR_MASK;
                let d_phys = d & ADDR_MASK;
                if s_phys != d_phys {
                    log_error!(
                        LOG_ORIGIN,
                        "clone_kernel_mappings: VA 0x{:X} phys mismatch! src=0x{:X} dst=0x{:X} – repairing",
                        virt_page,
                        s_phys,
                        d_phys
                    );
                    // Overwrite the leaf entry in destination
                    repair_leaf_pte(dst_pml4, virt_page, s);
                }
            }
            _ => {} // not mapped in source → nothing to do
        }
    }
}

/// Force the leaf PTE for `virt` in `pml4_phys` to `raw_pte`.
/// The intermediate entries must already exist (they were deep-copied).
fn repair_leaf_pte(pml4_phys: usize, virt: usize, raw_pte: u64) {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);

    let pml4 = unsafe { &*(phys_to_virt_ptr(pml4_phys) as *const PageTable) };
    if !pml4.entries[pml4_idx].is_present() { return; }

    let pdpt = unsafe { &*(phys_to_virt_ptr(pml4.entries[pml4_idx].addr()) as *const PageTable) };
    if !pdpt.entries[pdpt_idx].is_present() { return; }

    let pd = unsafe { &*(phys_to_virt_ptr(pdpt.entries[pdpt_idx].addr()) as *const PageTable) };
    if !pd.entries[pd_idx].is_present() { return; }
    if pd.entries[pd_idx].0 & (1 << 7) != 0 { return; } // huge page

    let pt = unsafe { &mut *(phys_to_virt_ptr(pd.entries[pd_idx].addr()) as *mut PageTable) };
    unsafe {
        core::ptr::write_volatile(&mut pt.entries[pt_idx].0 as *mut u64, raw_pte);
    }
}

pub fn unmap_page(virt: usize) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _) = walk_to_entry(virt, false)?;
    let was_present = entry.is_present();

    if !was_present {
        return Err(VmError::NotMapped);
    }

    entry.clear();
    MAPPED_PAGES.fetch_sub(1, Ordering::Relaxed);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);
    Ok(())
}

pub fn unmap_page_in_pml4(pml4_phys: usize, virt: usize) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;
    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _) = walk_to_entry_with_root_user(pml4_phys, virt, false, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    entry.clear();
    MAPPED_PAGES.fetch_sub(1, Ordering::Relaxed);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);
    Ok(())
}

/// Remap an existing page to be accessible from userspace (ring 3)
/// This adds the USER bit to ALL levels of the page table hierarchy
#[allow(dead_code)]
pub fn remap_page_user(virt: usize) -> Result<(), VmError> {
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;
    let _lock = PAGE_TABLE_LOCK.lock();

    // Get indices for all levels
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);

    // Walk through each level and add USER bit
    let pml4 = unsafe { &mut *(phys_to_virt_ptr(pml4_phys) as *mut PageTable) };
    let pml4e = &mut pml4.entries[pml4_idx];
    if !pml4e.is_present() {
        return Err(VmError::NotMapped);
    }
    pml4e.0 |= PageFlags::USER.bits();

    let pdpt = unsafe { &mut *(phys_to_virt_ptr(pml4e.addr()) as *mut PageTable) };
    let pdpte = &mut pdpt.entries[pdpt_idx];
    if !pdpte.is_present() {
        return Err(VmError::NotMapped);
    }
    pdpte.0 |= PageFlags::USER.bits();

    let pd = unsafe { &mut *(phys_to_virt_ptr(pdpte.addr()) as *mut PageTable) };
    let pde = &mut pd.entries[pd_idx];
    if !pde.is_present() {
        return Err(VmError::NotMapped);
    }
    pde.0 |= PageFlags::USER.bits();

    let pt = unsafe { &mut *(phys_to_virt_ptr(pde.addr()) as *mut PageTable) };
    let pte = &mut pt.entries[pt_idx];
    if !pte.is_present() {
        return Err(VmError::NotMapped);
    }
    pte.0 |= PageFlags::USER.bits();

    invalidate_tlb_for_pml4_page(pml4_phys, virt);

    Ok(())
}

/// Remap an existing page to add specific flags
#[allow(dead_code)]
pub fn remap_page_flags(virt: usize, additional_flags: PageFlags) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    let _lock = PAGE_TABLE_LOCK.lock();

    let (entry, _) = walk_to_entry(virt, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    // Get current entry value and add new flags
    let raw = entry.0;
    let phys = (raw & ADDR_MASK) as usize;
    let current_flags = PageFlags(raw & !ADDR_MASK);

    let new_flags = PageFlags(current_flags.bits() | additional_flags.bits());

    entry.set(phys, new_flags);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);

    Ok(())
}

pub fn query_mapping_in_pml4(pml4_phys: usize, virt: usize) -> Result<(usize, PageFlags), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_initialized(pml4_phys != 0)?;
    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _) = walk_to_entry_with_root_user(pml4_phys, virt, false, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    let phys = entry.addr();
    let flags = PageFlags::from_bits(entry.0 & !ADDR_MASK);

    Ok((phys, flags))
}

#[allow(dead_code)]
pub fn remap_page(virt: usize, new_phys: usize, flags: PageFlags) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_page_alignment(new_phys)?;
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    let _lock = PAGE_TABLE_LOCK.lock();

    let (entry, _) = walk_to_entry(virt, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    entry.set(new_phys, flags);
    invalidate_tlb_for_pml4_page(pml4_phys, virt);
    Ok(())
}

pub fn translate(virt: usize) -> Option<usize> {
    let _lock = PAGE_TABLE_LOCK.lock();
    let (entry, _) = walk_to_entry(virt, false).ok()?;
    if !entry.is_present() {
        return None;
    }

    Some(entry.addr())
}

fn map_page_internal(
    pml4_phys: usize,
    virt: usize,
    phys: usize,
    flags: PageFlags,
) -> Result<(), VmError> {
    crate::mm::validate_page_alignment(virt)?;
    crate::mm::validate_page_alignment(phys)?;

    // Se for mapeamento user, precisamos que TODOS os níveis tenham USER
    let user_access = (flags.bits() & PageFlags::USER.bits()) != 0;

    let (entry, created_table) = walk_to_entry_with_root_user(pml4_phys, virt, true, user_access)?;

    if entry.is_present() {
        return Err(VmError::AlreadyMapped);
    }

    entry.set(phys, flags);
    MAPPED_PAGES.fetch_add(1, Ordering::Relaxed);
    let _ = created_table;

    Ok(())
}

fn walk_to_entry(virt: usize, create: bool) -> Result<(&'static mut PageTableEntry, bool), VmError> {
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    crate::mm::validate_initialized(pml4_phys != 0)?;

    // para query/translate/unmap não precisa user
    walk_to_entry_with_root_user(pml4_phys, virt, create, false)
}

#[cfg(test)]
mod tests {
    use super::{phys_to_virt_ptr_safe, HIGHER_HALF_READY, HIGHER_HALF_BASE};
    use crate::mm::ValidationError;
    use core::sync::atomic::Ordering;

    #[test]
    fn phys_to_virt_ptr_safe_requires_initialization() {
        HIGHER_HALF_READY.store(false, Ordering::Relaxed);
        assert_eq!(
            phys_to_virt_ptr_safe(0x1000),
            Err(ValidationError::NotInitialized)
        );
    }

    #[test]
    fn phys_to_virt_ptr_safe_uses_higher_half_after_initialization() {
        HIGHER_HALF_READY.store(true, Ordering::Relaxed);
        assert_eq!(
            phys_to_virt_ptr_safe(0x2000),
            Ok(HIGHER_HALF_BASE + 0x2000)
        );
        HIGHER_HALF_READY.store(false, Ordering::Relaxed);
    }
}

#[allow(dead_code)]
fn walk_to_entry_with_root(
    pml4_phys: usize,
    virt: usize,
    create: bool,
) -> Result<(&'static mut PageTableEntry, bool), VmError> {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);
    let mut created = false;

    let pml4 = unsafe { &mut *(phys_to_virt_ptr(pml4_phys) as *mut PageTable) };
    let pdpt = ensure_table(&mut pml4.entries[pml4_idx], create, &mut created)?;
    let pd = ensure_table(&mut pdpt.entries[pdpt_idx], create, &mut created)?;
    let pt = ensure_table(&mut pd.entries[pd_idx], create, &mut created)?;

    Ok((&mut pt.entries[pt_idx], created))
}

fn walk_to_entry_with_root_user(
    pml4_phys: usize,
    virt: usize,
    create: bool,
    user_access: bool,
) -> Result<(&'static mut PageTableEntry, bool), VmError> {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);
    let mut created = false;

    let pml4 = unsafe { &mut *(phys_to_virt_ptr(pml4_phys) as *mut PageTable) };
    let pdpt = ensure_table_user(&mut pml4.entries[pml4_idx], create, &mut created, user_access)?;
    let pd = ensure_table_user(&mut pdpt.entries[pdpt_idx], create, &mut created, user_access)?;
    let pt = ensure_table_user(&mut pd.entries[pd_idx], create, &mut created, user_access)?;

    Ok((&mut pt.entries[pt_idx], created))
}

fn ensure_table(
    entry: &mut PageTableEntry,
    create: bool,
    created_flag: &mut bool,
) -> Result<&'static mut PageTable, VmError> {
    if entry.is_present() {
        let table = unsafe { &mut *(phys_to_virt_ptr(entry.addr()) as *mut PageTable) };
        return Ok(table);
    }

    if !create {
        return Err(VmError::NotMapped);
    }

    let phys = pmm::alloc_page_zeroed().ok_or(VmError::OutOfMemory)?;
    PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);
    entry.set(phys, PageFlags::PRESENT | PageFlags::WRITABLE);
    *created_flag = true;

    Ok(unsafe { &mut *(phys_to_virt_ptr(phys) as *mut PageTable) })
}

fn ensure_table_user(
    entry: &mut PageTableEntry,
    create: bool,
    created_flag: &mut bool,
    user_access: bool,
) -> Result<&'static mut PageTable, VmError> {
    // Se já existe, mas precisamos de USER e ela não tem, "promove" a entrada
    if entry.is_present() {
        if user_access && (entry.0 & PageFlags::USER.bits()) == 0 {
            entry.0 |= PageFlags::USER.bits();
        }
        let table = unsafe { &mut *(phys_to_virt_ptr(entry.addr()) as *mut PageTable) };
        return Ok(table);
    }

    if !create {
        return Err(VmError::NotMapped);
    }

    let phys = pmm::alloc_page_zeroed().ok_or(VmError::OutOfMemory)?;
    PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);

    // IMPORTANTÍSSIMO: tabelas intermediárias precisam USER quando mapeando user pages
    let mut table_flags = PageFlags::PRESENT | PageFlags::WRITABLE;
    if user_access {
        table_flags |= PageFlags::USER;
    }

    entry.set(phys, table_flags);
    *created_flag = true;

    Ok(unsafe { &mut *(phys_to_virt_ptr(phys) as *mut PageTable) })
}

fn is_mappable_ram(typ: u32) -> bool {
    matches!(
        typ,
        EFI_LOADER_CODE
            | EFI_LOADER_DATA
            | EFI_BOOT_SERVICES_CODE
            | EFI_BOOT_SERVICES_DATA
            | EFI_RUNTIME_SERVICES_CODE
            | EFI_RUNTIME_SERVICES_DATA
            | EFI_CONVENTIONAL_MEMORY
            | EFI_ACPI_RECLAIM_MEMORY
            | EFI_ACPI_MEMORY_NVS
            | EFI_PERSISTENT_MEMORY
    )
}

fn flags_for_descriptor(desc: &EfiMemoryDescriptor) -> PageFlags {
    let mut flags = PageFlags::kernel_rw();

    if desc.attribute & EFI_MEMORY_XP != 0 || !is_code_descriptor(desc.typ) {
        flags = flags.with_nx();
    }

    if desc.attribute & (EFI_MEMORY_WP | EFI_MEMORY_RP) != 0 {
        flags = flags.without(PageFlags::WRITABLE);
    }

    apply_cacheability_flags(flags, desc.attribute)
}

fn is_code_descriptor(typ: u32) -> bool {
    matches!(typ, EFI_LOADER_CODE | EFI_BOOT_SERVICES_CODE | EFI_RUNTIME_SERVICES_CODE)
}

fn apply_cacheability_flags(mut flags: PageFlags, attribute: u64) -> PageFlags {
    if attribute & (EFI_MEMORY_UC | EFI_MEMORY_UCE) != 0 {
        flags |= PageFlags::CACHE_DISABLE;
        return flags;
    }

    if attribute & EFI_MEMORY_WT != 0 {
        flags |= PageFlags::WRITE_THROUGH;
        return flags;
    }

    if attribute & EFI_MEMORY_WC != 0 {
        flags |= PageFlags::CACHE_DISABLE;
        return flags;
    }

    if attribute & EFI_MEMORY_WB != 0 {
        return flags;
    }

    flags
}

fn split_indices(virt: usize) -> (usize, usize, usize, usize) {
    let pml4 = (virt >> 39) & 0x1FF;
    let pdpt = (virt >> 30) & 0x1FF;
    let pd = (virt >> 21) & 0x1FF;
    let pt = (virt >> 12) & 0x1FF;
    (pml4, pdpt, pd, pt)
}

#[inline(always)]
fn current_cr3_pml4() -> usize {
    let cr3: u64;
    unsafe {
        asm!("mov {}, cr3", out(reg) cr3, options(nostack, preserves_flags));
    }
    (cr3 & ADDR_MASK) as usize
}

#[inline(always)]
fn deferred_tlb_flush_slot(pml4_phys: usize) -> u32 {
    (((pml4_phys >> 12) as u64) & ((DEFERRED_TLB_FLUSH_SLOTS as u64) - 1)) as u32
}

#[inline(always)]
fn mark_deferred_tlb_flush(pml4_phys: usize) {
    let bit = 1u64 << deferred_tlb_flush_slot(pml4_phys);
    DEFERRED_TLB_FLUSH_MASK.fetch_or(bit, Ordering::Release);
}

#[inline(always)]
pub fn maybe_flush_deferred_tlb_for_pml4(target_pml4_phys: usize) {
    if target_pml4_phys == 0 {
        return;
    }

    let bit = 1u64 << deferred_tlb_flush_slot(target_pml4_phys);
    let was_pending = (DEFERRED_TLB_FLUSH_MASK.fetch_and(!bit, Ordering::AcqRel) & bit) != 0;
    if !was_pending {
        return;
    }

    // If we are already running in this CR3, force a full local TLB flush.
    // If we're about to switch to this CR3, switch_context's CR3 load will
    // flush non-global entries and complete the deferred invalidation.
    if current_cr3_pml4() == target_pml4_phys {
        unsafe {
            load_cr3(target_pml4_phys as u64);
        }
    }
}

/// Invalidate stale translations for a page that had its PTE updated.
///
/// Current implementation guarantees correctness for the local CPU and
/// establishes the single call site for future SMP shootdown integration.
/// Remote-shootdown wiring (IPI to CPUs running the same address space) must
/// hook here when AP scheduling lands.
#[inline(always)]
fn invalidate_tlb_for_pml4_page(target_pml4_phys: usize, addr: usize) {
    // Ensure PTE writes are globally visible before any invalidate/shootdown.
    fence(Ordering::Release);

    if current_cr3_pml4() == target_pml4_phys {
        invalidate_page(addr);
        fence(Ordering::SeqCst);
    } else {
        // On UP this becomes relevant when an inactive address-space is updated
        // and then rescheduled. On future SMP this bit tracks pending remote
        // shootdown work for this address-space bucket.
        mark_deferred_tlb_flush(target_pml4_phys);
    }
}

#[inline(always)]
fn invalidate_page(addr: usize) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
    }
}

#[inline(always)]
unsafe fn load_cr3(pml4_phys: u64) {
    asm!("mov cr3, {}", in(reg) pml4_phys, options(nostack, preserves_flags));
}
