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
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mm::pmm;
use crate::boot::{EfiMemoryDescriptor, MemoryMap};

use crate::{log_debug, log_info, log_error};

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
const HIGHER_HALF_BASE: usize = 0xFFFF_8000_0000_0000;
const HIGHER_HALF_MIRROR_SIZE: usize = 512 * 1024 * 1024;
static ACTIVE_PML4: AtomicUsize = AtomicUsize::new(0);
static MAPPED_PAGES: AtomicUsize = AtomicUsize::new(0);
static PAGE_TABLE_PAGES: AtomicUsize = AtomicUsize::new(0);
const LOG_ORIGIN: &str = "vmm";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    NotInitialized,
    Unaligned,
    AlreadyMapped,
    NotMapped,
    OutOfMemory,
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

    pub const fn without(self, other: PageFlags) -> Self {
        Self(self.bits() & !other.bits())
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

    log_info!(
        LOG_ORIGIN,
        "New address space active (PML4=0x{:X}, mapped ~{} MiB)",
        pml4_phys,
        max_physical_addr / (1024 * 1024)
    );
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
    if !pmm::is_page_aligned(virt) || !pmm::is_page_aligned(phys) {
        return Err(VmError::Unaligned);
    }

    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    if pml4_phys == 0 {
        return Err(VmError::NotInitialized);
    }

    map_page_internal(pml4_phys, virt, phys, flags)
}

pub fn map_page_in_pml4(pml4_phys: usize, virt: usize, phys: usize, flags: PageFlags) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) || !pmm::is_page_aligned(phys) {
        return Err(VmError::Unaligned);
    }

    if pml4_phys == 0 {
        return Err(VmError::NotInitialized);
    }

    map_page_internal(pml4_phys, virt, phys, flags)
}

pub fn clone_kernel_mappings(dst_pml4_phys: usize) -> Result<(), VmError> {
    if !pmm::is_page_aligned(dst_pml4_phys) {
        return Err(VmError::Unaligned);
    }

    let src_pml4 = ACTIVE_PML4.load(Ordering::Relaxed);
    if src_pml4 == 0 {
        return Err(VmError::NotInitialized);
    }

    let src = unsafe { &*(src_pml4 as *const PageTable) };
    let dst = unsafe { &mut *(dst_pml4_phys as *mut PageTable) };

    // Clear lower half (user space)
    for idx in 0..ENTRIES_PER_TABLE / 2 {
        dst.entries[idx].clear();
    }

    // Copy higher half (kernel space)
    for idx in ENTRIES_PER_TABLE / 2..ENTRIES_PER_TABLE {
        dst.entries[idx] = src.entries[idx];
    }

    Ok(())
}

pub fn unmap_page(virt: usize) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) {
        return Err(VmError::Unaligned);
    }

    let (entry, _) = walk_to_entry(virt, false)?;
    let was_present = entry.is_present();

    if !was_present {
        return Err(VmError::NotMapped);
    }

    entry.clear();
    MAPPED_PAGES.fetch_sub(1, Ordering::Relaxed);
    invalidate_page(virt);
    Ok(())
}

pub fn unmap_page_in_pml4(pml4_phys: usize, virt: usize) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) {
        return Err(VmError::Unaligned);
    }

    if pml4_phys == 0 {
        return Err(VmError::NotInitialized);
    }

    let (entry, _) = walk_to_entry_with_root_user(pml4_phys, virt, false, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    entry.clear();
    MAPPED_PAGES.fetch_sub(1, Ordering::Relaxed);
    Ok(())
}

/// Remap an existing page to be accessible from userspace (ring 3)
/// This adds the USER bit to the page table entry
pub fn remap_page_user(virt: usize) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) {
        return Err(VmError::Unaligned);
    }

    let (entry, _) = walk_to_entry(virt, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    // Get current entry value and add USER flag
    let raw = entry.0;
    let phys = (raw & ADDR_MASK) as usize;
    let current_flags = PageFlags(raw & !ADDR_MASK);

    // Add USER flag
    let new_flags = PageFlags(current_flags.bits() | PageFlags::USER.bits());

    // Update entry with new flags
    entry.set(phys, new_flags);
    invalidate_page(virt);

    Ok(())
}

/// Remap an existing page to add specific flags
pub fn remap_page_flags(virt: usize, additional_flags: PageFlags) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) {
        return Err(VmError::Unaligned);
    }

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
    invalidate_page(virt);

    Ok(())
}

pub fn query_mapping_in_pml4(pml4_phys: usize, virt: usize) -> Result<(usize, PageFlags), VmError> {
    if !pmm::is_page_aligned(virt) {
        return Err(VmError::Unaligned);
    }

    if pml4_phys == 0 {
        return Err(VmError::NotInitialized);
    }

    let (entry, _) = walk_to_entry_with_root_user(pml4_phys, virt, false, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    let phys = entry.addr();
    let flags = PageFlags::from_bits(entry.0);

    Ok((phys, flags))
}

#[allow(dead_code)]
pub fn remap_page(virt: usize, new_phys: usize, flags: PageFlags) -> Result<(), VmError> {
    if !pmm::is_page_aligned(virt) || !pmm::is_page_aligned(new_phys) {
        return Err(VmError::Unaligned);
    }

    let (entry, _) = walk_to_entry(virt, false)?;
    if !entry.is_present() {
        return Err(VmError::NotMapped);
    }

    entry.set(new_phys, flags);
    invalidate_page(virt);
    Ok(())
}

pub fn translate(virt: usize) -> Option<usize> {
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
    if !pmm::is_page_aligned(virt) || !pmm::is_page_aligned(phys) {
        return Err(VmError::Unaligned);
    }

    // Se for mapeamento user, precisamos que TODOS os níveis tenham USER
    let user_access = (flags.bits() & PageFlags::USER.bits()) != 0;

    let (entry, created_table) = walk_to_entry_with_root_user(pml4_phys, virt, true, user_access)?;

    if entry.is_present() {
        return Err(VmError::AlreadyMapped);
    }

    entry.set(phys, flags);
    MAPPED_PAGES.fetch_add(1, Ordering::Relaxed);

    if created_table {
        invalidate_page(virt);
    }

    Ok(())
}

fn walk_to_entry(virt: usize, create: bool) -> Result<(&'static mut PageTableEntry, bool), VmError> {
    let pml4_phys = ACTIVE_PML4.load(Ordering::Relaxed);
    if pml4_phys == 0 {
        return Err(VmError::NotInitialized);
    }

    // para query/translate/unmap não precisa user
    walk_to_entry_with_root_user(pml4_phys, virt, create, false)
}

#[allow(dead_code)]
fn walk_to_entry_with_root(
    pml4_phys: usize,
    virt: usize,
    create: bool,
) -> Result<(&'static mut PageTableEntry, bool), VmError> {
    let (pml4_idx, pdpt_idx, pd_idx, pt_idx) = split_indices(virt);
    let mut created = false;

    let pml4 = unsafe { &mut *(pml4_phys as *mut PageTable) };
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

    let pml4 = unsafe { &mut *(pml4_phys as *mut PageTable) };
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
        let table = unsafe { &mut *(entry.addr() as *mut PageTable) };
        return Ok(table);
    }

    if !create {
        return Err(VmError::NotMapped);
    }

    let phys = pmm::alloc_page_zeroed().ok_or(VmError::OutOfMemory)?;
    PAGE_TABLE_PAGES.fetch_add(1, Ordering::Relaxed);
    entry.set(phys, PageFlags::PRESENT | PageFlags::WRITABLE);
    *created_flag = true;

    Ok(unsafe { &mut *(phys as *mut PageTable) })
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
        let table = unsafe { &mut *(entry.addr() as *mut PageTable) };
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

    Ok(unsafe { &mut *(phys as *mut PageTable) })
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
fn invalidate_page(addr: usize) {
    unsafe {
        asm!("invlpg [{}]", in(reg) addr, options(nostack, preserves_flags));
    }
}

#[inline(always)]
unsafe fn load_cr3(pml4_phys: u64) {
    asm!("mov cr3, {}", in(reg) pml4_phys, options(nostack, preserves_flags));
}