// Physical Memory Manager (PMM) — Scalable Bitmap Allocator
//
// Implements a physical page allocator that scales to 16+ GiB of RAM using
// a two-phase bitmap approach:
//
// Phase 1 (Early Boot):
//   A static bitmap in .bss covers up to MAX_STATIC_PAGES (16 GiB).
//   This is used during init() to discover memory and bootstrap the system.
//
// Phase 2 (Dynamic):
//   During init(), we scan the UEFI memory map to find the highest usable
//   physical address, compute the required bitmap size, and carve the dynamic
//   bitmap from a usable RAM region. The static bitmap is then retired.
//   This allows the PMM to track all physical memory the firmware reports,
//   regardless of size.
//
// Design:
// - One bit per page: 0 = free, 1 = allocated
// - Bitmap access is protected by a spinlock for SMP safety
// - Word-at-a-time (u64) scanning for fast free-page searches
// - NEXT_FREE_HINT provides next-fit optimization
// - Respects memory holes: only regions marked usable by firmware are freed
// - Page 0 is never allocated (null pointer detection)
//
// Memory safety:
// - Reserved/MMIO/ACPI/runtime regions are never marked free
// - The bitmap region itself is marked allocated to prevent self-corruption
// - Kernel/bootloader regions are implicitly protected (not in usable set)
//
// Public interface:
// - alloc_page / free_page — single page management
// - alloc_pages / free_pages — contiguous range management
// - Zeroed variants for page tables and heap init
// - get_stats / get_detailed_stats / get_memory_stats — diagnostics

use alloc::collections::BTreeMap;
use crate::boot::{MemoryMap, EFI_CONVENTIONAL_MEMORY, EFI_BOOT_SERVICES_CODE, EFI_BOOT_SERVICES_DATA};
#[allow(unused_imports)]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;
use crate::{log_info, log_debug, log_warn};

/// Page size: 4 KiB
pub const PAGE_SIZE: usize = 4096;

/// Static bitmap covers up to 16 GiB (4,194,304 pages).
/// This is 512 KiB in .bss — acceptable for a kernel.
const MAX_STATIC_PAGES: usize = 4 * 1024 * 1024;
const STATIC_BITMAP_BYTES: usize = MAX_STATIC_PAGES / 8;

/// Hard upper limit: 64 GiB (prevents absurd bitmap sizes).
/// Beyond this, a buddy allocator or zone-based approach is needed.
const MAX_SUPPORTED_PAGES: usize = 64 * 1024 * 1024 / 4; // 16M pages = 64 GiB

// ---------------------------------------------------------------------------
// Static bitmap (Phase 1): lives in .bss, covers up to 16 GiB
// ---------------------------------------------------------------------------
static mut STATIC_BITMAP: [u8; STATIC_BITMAP_BYTES] = [0xFF; STATIC_BITMAP_BYTES];

// ---------------------------------------------------------------------------
// Dynamic bitmap (Phase 2): pointer + length, carved from usable RAM
// ---------------------------------------------------------------------------
static DYNAMIC_BITMAP_PTR: AtomicUsize = AtomicUsize::new(0);
static DYNAMIC_BITMAP_LEN: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// Operational state
// ---------------------------------------------------------------------------

/// Whether the PMM has been initialized
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Whether we are using the dynamic bitmap (Phase 2)
static USING_DYNAMIC: AtomicBool = AtomicBool::new(false);

/// Total pages being tracked (highest address / PAGE_SIZE)
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Number of currently free pages
static FREE_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Next-fit hint: page index to start searching from
static NEXT_FREE_HINT: AtomicUsize = AtomicUsize::new(0);

/// Largest contiguous free run found during init
static LARGEST_FREE_RUN: AtomicUsize = AtomicUsize::new(0);

/// Actual physical RAM available (sum of all usable regions from memory map)
static PHYSICAL_RAM_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Reserved RAM (non-usable regions)
static RESERVED_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Pages used by the bitmap itself
static BITMAP_OVERHEAD_PAGES: AtomicUsize = AtomicUsize::new(0);

/// Spinlock protecting all bitmap mutations.
/// The inner value is unused (unit type) — the lock itself provides exclusion.
static BITMAP_LOCK: Mutex<()> = Mutex::new(());

/// Per-physical-page shared reference counters used by COW.
///
/// Representation:
/// - missing entry => implicit refcount 1 (exclusive mapping)
/// - entry N>0     => explicit shared refcount
static PHYS_REFCOUNTS: Mutex<BTreeMap<usize, usize>> = Mutex::new(BTreeMap::new());

// ---------------------------------------------------------------------------
// PML4 Protection Registry
// ---------------------------------------------------------------------------

/// Registry of active PML4 page tables that must not be freed.
/// Tracks physical addresses of PML4 pages currently in use by address spaces.
/// This prevents premature freeing of active page tables which would corrupt
/// address space structures.
const MAX_PROTECTED_PML4S: usize = 1024;

#[derive(Clone, Copy)]
struct ProtectedPml4Registry {
    entries: [usize; MAX_PROTECTED_PML4S],
    len: usize,
}

impl ProtectedPml4Registry {
    const fn new() -> Self {
        Self {
            entries: [0; MAX_PROTECTED_PML4S],
            len: 0,
        }
    }

    fn insert(&mut self, pml4_phys: usize) -> Result<(), crate::mm::ValidationError> {
        if self.contains(pml4_phys) {
            return Ok(());
        }

        if self.len >= self.entries.len() {
            crate::log_error!(
                "[pmm]",
                "ProtectedPml4Registry exhausted: cannot register pml4=0x{:X}, capacity={}",
                pml4_phys,
                self.entries.len()
            );
            return Err(crate::mm::ValidationError::RegistryExhausted {
                current_capacity: self.entries.len(),
            });
        }

        self.entries[self.len] = pml4_phys;
        self.len += 1;
        Ok(())
    }

    fn remove(&mut self, pml4_phys: usize) -> bool {
        if let Some(index) = self.entries[..self.len]
            .iter()
            .position(|&entry| entry == pml4_phys)
        {
            self.len -= 1;
            self.entries[index] = self.entries[self.len];
            self.entries[self.len] = 0;
            return true;
        }

        false
    }

    fn contains(&self, pml4_phys: usize) -> bool {
        self.entries[..self.len].contains(&pml4_phys)
    }
}

static PROTECTED_PML4S: Mutex<ProtectedPml4Registry> =
    Mutex::new(ProtectedPml4Registry::new());

#[cfg(debug_assertions)]
#[allow(dead_code)]
static ALLOC_TRACE: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// EFI memory type helpers
// ---------------------------------------------------------------------------

/// Check if an EFI memory type is usable by the kernel after ExitBootServices()
#[inline]
fn is_usable_memory(typ: u32) -> bool {
    typ == EFI_CONVENTIONAL_MEMORY
        || typ == EFI_BOOT_SERVICES_CODE
        || typ == EFI_BOOT_SERVICES_DATA
}

/// Get human-readable name for EFI memory type
fn memory_type_name(typ: u32) -> &'static str {
    match typ {
        0 => "Reserved",
        1 => "LoaderCode",
        2 => "LoaderData",
        3 => "BootServicesCode",
        4 => "BootServicesData",
        5 => "RuntimeServicesCode",
        6 => "RuntimeServicesData",
        7 => "Conventional",
        8 => "Unusable",
        9 => "ACPIReclaim",
        10 => "ACPINVS",
        11 => "MemoryMappedIO",
        12 => "MemoryMappedIOPortSpace",
        13 => "PalCode",
        14 => "PersistentMemory",
        _ => "Unknown",
    }
}

// ---------------------------------------------------------------------------
// Bitmap access layer — abstracts static vs dynamic bitmap
// ---------------------------------------------------------------------------

/// Get a mutable reference to the active bitmap slice.
/// SAFETY: caller must hold BITMAP_LOCK or be in single-threaded init.
#[inline]
unsafe fn bitmap_slice() -> &'static [u8] {
    if USING_DYNAMIC.load(Ordering::Relaxed) {
        let ptr = DYNAMIC_BITMAP_PTR.load(Ordering::Relaxed) as *const u8;
        let len = DYNAMIC_BITMAP_LEN.load(Ordering::Relaxed);
        core::slice::from_raw_parts(ptr, len)
    } else {
        &STATIC_BITMAP[..]
    }
}

/// Get a mutable reference to the active bitmap slice.
/// SAFETY: caller must hold BITMAP_LOCK.
#[inline]
unsafe fn bitmap_slice_mut() -> &'static mut [u8] {
    if USING_DYNAMIC.load(Ordering::Relaxed) {
        let ptr = DYNAMIC_BITMAP_PTR.load(Ordering::Relaxed) as *mut u8;
        let len = DYNAMIC_BITMAP_LEN.load(Ordering::Relaxed);
        core::slice::from_raw_parts_mut(ptr, len)
    } else {
        &mut STATIC_BITMAP[..]
    }
}

/// Check if a page is free (bit 0 = free, 1 = allocated)
/// SAFETY: caller must hold BITMAP_LOCK or be in single-threaded init.
#[inline]
unsafe fn is_page_free(page: usize) -> bool {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return false;
    }
    let bm = bitmap_slice();
    let byte_idx = page / 8;
    if byte_idx >= bm.len() {
        return false;
    }
    let bit = page % 8;
    (bm[byte_idx] & (1 << bit)) == 0
}

/// Mark a page as free
/// SAFETY: caller must hold BITMAP_LOCK or be in single-threaded init.
#[inline]
unsafe fn set_page_free(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }
    let bm = bitmap_slice_mut();
    let byte_idx = page / 8;
    if byte_idx >= bm.len() {
        return;
    }
    let bit = page % 8;
    bm[byte_idx] &= !(1 << bit);
}

/// Mark a page as allocated
/// SAFETY: caller must hold BITMAP_LOCK or be in single-threaded init.
#[inline]
unsafe fn set_page_allocated(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }
    let bm = bitmap_slice_mut();
    let byte_idx = page / 8;
    if byte_idx >= bm.len() {
        return;
    }
    let bit = page % 8;
    bm[byte_idx] |= 1 << bit;
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the Physical Memory Manager from the UEFI memory map.
///
/// Two-phase approach:
/// 1. Use static bitmap to track memory up to 16 GiB
/// 2. If physical memory > 16 GiB, attempt to carve a dynamic bitmap from RAM
///
/// After init, all usable memory is tracked and free for allocation.
pub unsafe fn init(memory_map: &MemoryMap) {
    // -----------------------------------------------------------------------
    // Pass 0: Clear the static bitmap (all pages marked allocated)
    // -----------------------------------------------------------------------
    core::ptr::write_bytes(
        core::ptr::addr_of_mut!(STATIC_BITMAP).cast::<u8>(),
        0xFF,
        STATIC_BITMAP_BYTES,
    );

    // -----------------------------------------------------------------------
    // Pass 1: Scan memory map for totals and highest address
    // -----------------------------------------------------------------------
    let mut highest_usable_addr: usize = 0;
    let mut physical_ram_pages: usize = 0;
    let mut reserved_pages: usize = 0;
    let mut region_count: usize = 0;
    let mut usable_region_count: usize = 0;

    for d in memory_map.descriptors() {
        region_count += 1;
        let start = d.physical_start as usize;
        let num_pages = d.number_of_pages as usize;
        let end = start.saturating_add(num_pages * PAGE_SIZE);

        if is_usable_memory(d.typ) {
            physical_ram_pages += num_pages;
            usable_region_count += 1;
            if end > highest_usable_addr {
                highest_usable_addr = end;
            }
        } else {
            reserved_pages += num_pages;
        }
    }

    let highest_page = highest_usable_addr / PAGE_SIZE;

    // Clamp to our hard upper limit
    let tracked_pages = if highest_page > MAX_SUPPORTED_PAGES {
        log_warn!(
            "[pmm]",
            "Physical memory exceeds 64 GiB limit: highest page {} > {}. Clamping.",
            highest_page,
            MAX_SUPPORTED_PAGES
        );
        MAX_SUPPORTED_PAGES
    } else {
        highest_page
    };

    log_info!(
        "[pmm]",
        "Memory map: {} regions ({} usable), highest_addr=0x{:X}, tracked_pages={}",
        region_count,
        usable_region_count,
        highest_usable_addr,
        tracked_pages
    );

    // -----------------------------------------------------------------------
    // Phase selection: static bitmap or dynamic bitmap?
    // -----------------------------------------------------------------------
    let bitmap_bytes_needed = tracked_pages.div_ceil(8);

    // effective_tracked_pages: the page count actually supported by the
    // chosen bitmap.  Set exactly once and stored into TOTAL_PAGES once.
    let effective_tracked_pages: usize;

    if tracked_pages <= MAX_STATIC_PAGES {
        // Static bitmap is sufficient
        effective_tracked_pages = tracked_pages;
        log_info!(
            "[pmm]",
            "Using static bitmap: {} bytes for {} pages ({} MiB addressable)",
            bitmap_bytes_needed,
            effective_tracked_pages,
            (effective_tracked_pages * PAGE_SIZE) / (1024 * 1024)
        );
        USING_DYNAMIC.store(false, Ordering::Relaxed);
    } else {
        // Need dynamic bitmap — carve from a usable RAM region
        log_info!(
            "[pmm]",
            "Need dynamic bitmap: {} bytes ({} KiB) for {} pages ({} GiB addressable)",
            bitmap_bytes_needed,
            bitmap_bytes_needed / 1024,
            tracked_pages,
            (tracked_pages * PAGE_SIZE) / (1024 * 1024 * 1024)
        );

        let bitmap_pages = align_up_val(bitmap_bytes_needed, PAGE_SIZE) / PAGE_SIZE;
        let bitmap_alloc_size = bitmap_pages * PAGE_SIZE;

        // Find a usable region large enough to hold the bitmap.
        // Prefer low memory to keep it accessible via identity mapping.
        let mut best_region: Option<usize> = None;
        for d in memory_map.descriptors() {
            if !is_usable_memory(d.typ) {
                continue;
            }
            let region_start = d.physical_start as usize;
            let region_size = d.number_of_pages as usize * PAGE_SIZE;

            // Skip page 0 region
            let effective_start = if region_start == 0 { PAGE_SIZE } else { region_start };
            let effective_size = region_size.saturating_sub(effective_start - region_start);

            if effective_size >= bitmap_alloc_size {
                // Use the lowest suitable region
                if best_region.is_none() || effective_start < best_region.unwrap() {
                    best_region = Some(effective_start);
                }
            }
        }

        if let Some(bitmap_phys) = best_region {
            // Dynamic bitmap allocated — track the full range
            effective_tracked_pages = tracked_pages;

            log_info!(
                "[pmm]",
                "Dynamic bitmap at phys 0x{:X}, {} pages ({} KiB)",
                bitmap_phys,
                bitmap_pages,
                bitmap_alloc_size / 1024
            );

            // Zero the dynamic bitmap region (mark all allocated)
            let ptr = bitmap_phys as *mut u8;
            core::ptr::write_bytes(ptr, 0xFF, bitmap_alloc_size);

            DYNAMIC_BITMAP_PTR.store(bitmap_phys, Ordering::Relaxed);
            DYNAMIC_BITMAP_LEN.store(bitmap_alloc_size, Ordering::Relaxed);
            USING_DYNAMIC.store(true, Ordering::Relaxed);
            BITMAP_OVERHEAD_PAGES.store(bitmap_pages, Ordering::Relaxed);
        } else {
            // Fallback: use static bitmap, clamp to MAX_STATIC_PAGES
            effective_tracked_pages = tracked_pages.min(MAX_STATIC_PAGES);

            log_warn!(
                "[pmm]",
                "No region large enough for dynamic bitmap. Falling back to static ({} GiB max, {} pages).",
                (MAX_STATIC_PAGES * PAGE_SIZE) / (1024 * 1024 * 1024),
                effective_tracked_pages
            );
            USING_DYNAMIC.store(false, Ordering::Relaxed);
        }
    }

    // Store effective page count exactly once — never exceeds bitmap capacity.
    TOTAL_PAGES.store(effective_tracked_pages, Ordering::Relaxed);
    PHYSICAL_RAM_PAGES.store(physical_ram_pages, Ordering::Relaxed);
    RESERVED_PAGES.store(reserved_pages, Ordering::Relaxed);
    NEXT_FREE_HINT.store(2, Ordering::Relaxed); // Skip page 1 (Kernel PML4) // Skip page 0

    // -----------------------------------------------------------------------
    // Pass 2: Mark usable regions as free in the bitmap
    // -----------------------------------------------------------------------
    let mut free_pages: usize = 0;

    log_debug!("[pmm]", "Memory map regions:");
    for d in memory_map.descriptors() {
        let start_page = (d.physical_start as usize) / PAGE_SIZE;
        let num_pages = d.number_of_pages as usize;
        let end_page = start_page.saturating_add(num_pages).min(effective_tracked_pages);
        let size_mb = (num_pages * PAGE_SIZE) / (1024 * 1024);

        if size_mb > 0 && is_usable_memory(d.typ) {
            log_debug!(
                "[pmm]",
                "  0x{:016X}-0x{:016X} {} MB [{}] - USABLE",
                d.physical_start,
                d.physical_start + (num_pages as u64 * PAGE_SIZE as u64),
                size_mb,
                memory_type_name(d.typ)
            );
        }

        if !is_usable_memory(d.typ) {
            continue;
        }

        if start_page >= effective_tracked_pages {
            continue;
        }

        for page in start_page..end_page {
            set_page_free(page);
            free_pages += 1;
        }
    }

    // -----------------------------------------------------------------------
    // If using dynamic bitmap, mark the bitmap's own pages as allocated
    // -----------------------------------------------------------------------
    if USING_DYNAMIC.load(Ordering::Relaxed) {
        let bitmap_phys = DYNAMIC_BITMAP_PTR.load(Ordering::Relaxed);
        let bitmap_pages = BITMAP_OVERHEAD_PAGES.load(Ordering::Relaxed);
        let bitmap_start_page = bitmap_phys / PAGE_SIZE;

        for page in bitmap_start_page..(bitmap_start_page + bitmap_pages) {
            if is_page_free(page) {
                set_page_allocated(page);
                free_pages = free_pages.saturating_sub(1);
            }
        }

        log_info!(
            "[pmm]",
            "Bitmap self-reservation: pages {}-{} ({} pages, {} KiB)",
            bitmap_start_page,
            bitmap_start_page + bitmap_pages,
            bitmap_pages,
            bitmap_pages * PAGE_SIZE / 1024
        );
    }

    // -----------------------------------------------------------------------
    // Page 0 reservation (single consolidation point).
    // Page 0 must never be allocatable — it serves as the null pointer trap.
    // -----------------------------------------------------------------------
    if is_page_free(0) {
        set_page_allocated(0);
        free_pages = free_pages.saturating_sub(1);
    }

    FREE_PAGES.store(free_pages, Ordering::Relaxed);

    // -----------------------------------------------------------------------
    // Pass 3: Compute largest free run (for contiguous allocation insight)
    // -----------------------------------------------------------------------
    let mut current_run = 0usize;
    let mut max_run = 0usize;

    for page in 0..effective_tracked_pages {
        if is_page_free(page) {
            current_run += 1;
            if current_run > max_run {
                max_run = current_run;
            }
        } else {
            current_run = 0;
        }
    }

    LARGEST_FREE_RUN.store(max_run, Ordering::Relaxed);
    INITIALIZED.store(true, Ordering::Relaxed);

    // -----------------------------------------------------------------------
    // Hardening: validate bitmap capacity vs tracked pages
    // -----------------------------------------------------------------------
    {
        let bitmap_bits_capacity = if USING_DYNAMIC.load(Ordering::Relaxed) {
            DYNAMIC_BITMAP_LEN.load(Ordering::Relaxed) * 8
        } else {
            STATIC_BITMAP_BYTES * 8
        };

        if bitmap_bits_capacity < effective_tracked_pages {
            // FATAL: bitmap cannot represent all tracked pages.
            // This would cause out-of-bounds bitmap access and memory corruption.
            log_warn!(
                "[pmm]",
                "FATAL: bitmap capacity ({} bits) < effective_tracked_pages ({})! Clamping to bitmap capacity.",
                bitmap_bits_capacity,
                effective_tracked_pages
            );
            // Emergency clamp — should never happen if the above logic is correct
            let clamped = bitmap_bits_capacity;
            TOTAL_PAGES.store(clamped, Ordering::Relaxed);
        }

        // Validate that the bitmap slice is actually accessible
        let bm = bitmap_slice();
        let required_bytes = effective_tracked_pages.div_ceil(8);
        if bm.len() < required_bytes {
            log_warn!(
                "[pmm]",
                "FATAL: bitmap slice len ({}) < required bytes ({})! Clamping.",
                bm.len(),
                required_bytes
            );
            let clamped = bm.len() * 8;
            TOTAL_PAGES.store(clamped, Ordering::Relaxed);
        }

        // If using dynamic bitmap, verify that the bitmap's own pages are
        // allocated and won't be handed out as free memory.
        if USING_DYNAMIC.load(Ordering::Relaxed) {
            let bitmap_phys = DYNAMIC_BITMAP_PTR.load(Ordering::Relaxed);
            let bitmap_pages = BITMAP_OVERHEAD_PAGES.load(Ordering::Relaxed);
            let bitmap_start_page = bitmap_phys / PAGE_SIZE;

            for page in bitmap_start_page..(bitmap_start_page + bitmap_pages) {
                if is_page_free(page) {
                    log_warn!(
                        "[pmm]",
                        "HARDENING: bitmap page {} was free after init! Force-allocating.",
                        page
                    );
                    set_page_allocated(page);
                    let fp = FREE_PAGES.load(Ordering::Relaxed);
                    if fp > 0 {
                        FREE_PAGES.store(fp - 1, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Boot telemetry
    // -----------------------------------------------------------------------
    let effective_total = TOTAL_PAGES.load(Ordering::Relaxed);
    let effective_free = FREE_PAGES.load(Ordering::Relaxed);
    let physical_ram_mb = (physical_ram_pages * PAGE_SIZE) / (1024 * 1024);
    let free_mb = (effective_free * PAGE_SIZE) / (1024 * 1024);
    let reserved_mb = (reserved_pages * PAGE_SIZE) / (1024 * 1024);
    let tracked_mb = (effective_total * PAGE_SIZE) / (1024 * 1024);
    let bitmap_kb = if USING_DYNAMIC.load(Ordering::Relaxed) {
        DYNAMIC_BITMAP_LEN.load(Ordering::Relaxed) / 1024
    } else {
        (effective_total / 8 + 1) / 1024
    };

    log_info!("[pmm]", "========================================");
    log_info!("[pmm]", "PMM INITIALIZED — Memory Summary");
    log_info!("[pmm]", "========================================");
    log_info!("[pmm]", "  Physical RAM:   {} MiB ({} pages)", physical_ram_mb, physical_ram_pages);
    log_info!("[pmm]", "  Reserved/MMIO:  {} MiB ({} pages)", reserved_mb, reserved_pages);
    log_info!("[pmm]", "  Tracked range:  {} MiB ({} pages)", tracked_mb, effective_total);
    log_info!("[pmm]", "  Free pages:     {} ({} MiB)", effective_free, free_mb);
    log_info!("[pmm]", "  Bitmap type:    {}", if USING_DYNAMIC.load(Ordering::Relaxed) { "dynamic" } else { "static (.bss)" });
    log_info!("[pmm]", "  Bitmap size:    {} KiB", bitmap_kb);
    log_info!("[pmm]", "  Largest run:    {} pages ({} MiB)", max_run, (max_run * PAGE_SIZE) / (1024 * 1024));
    log_info!("[pmm]", "========================================");
}

/// Helper: align a value up to alignment boundary
#[inline]
fn align_up_val(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

#[inline]
fn validate_page_addr_for_refcount(addr: usize) -> Result<(), crate::mm::ValidationError> {
    crate::mm::validate_page_alignment(addr)?;

    let page = addr / PAGE_SIZE;
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total || page <= 1 {
        return Err(crate::mm::ValidationError::OutOfBounds {
            addr,
            min: 2 * PAGE_SIZE,
            max: total.saturating_sub(1) * PAGE_SIZE,
        });
    }

    Ok(())
}

#[inline]
fn inc_ref_locked(refs: &mut BTreeMap<usize, usize>, addr: usize) -> usize {
    match refs.get_mut(&addr) {
        Some(counter) => {
            *counter = counter.saturating_add(1);
            *counter
        }
        None => {
            // Missing entry means implicit refcount=1. First share promotes to 2.
            refs.insert(addr, 2);
            2
        }
    }
}

#[inline]
fn dec_ref_locked(refs: &mut BTreeMap<usize, usize>, addr: usize) -> usize {
    match refs.get_mut(&addr) {
        Some(counter) => {
            if *counter > 1 {
                *counter -= 1;
                *counter
            } else {
                refs.remove(&addr);
                0
            }
        }
        None => {
            // Missing entry means exclusive refcount=1.
            0
        }
    }
}

/// Increase physical-page reference count for shared (COW) ownership.
/// Returns the new refcount.
pub fn phys_ref_inc(addr: usize) -> Result<usize, crate::mm::ValidationError> {
    validate_page_addr_for_refcount(addr)?;
    let mut refs = PHYS_REFCOUNTS.lock();
    Ok(inc_ref_locked(&mut refs, addr))
}

/// Decrease physical-page reference count.
/// Returns the remaining refcount after decrement.
pub fn phys_ref_dec(addr: usize) -> Result<usize, crate::mm::ValidationError> {
    validate_page_addr_for_refcount(addr)?;
    let mut refs = PHYS_REFCOUNTS.lock();
    Ok(dec_ref_locked(&mut refs, addr))
}

/// Get current physical-page reference count.
///
/// Missing entries mean implicit refcount=1 (exclusive ownership).
pub fn phys_ref_get(addr: usize) -> usize {
    if validate_page_addr_for_refcount(addr).is_err() {
        return 0;
    }
    let refs = PHYS_REFCOUNTS.lock();
    refs.get(&addr).copied().unwrap_or(1)
}

// ---------------------------------------------------------------------------
// Allocation / deallocation
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn enable_alloc_trace() {
    #[cfg(debug_assertions)]
    ALLOC_TRACE.store(true, Ordering::Relaxed);
}

/// Allocate a single physical page. Returns the physical address, or None.
pub fn alloc_page() -> Option<usize> {
    let _lock = BITMAP_LOCK.lock();

    let free = FREE_PAGES.load(Ordering::Relaxed);
    if free == 0 {
        return None;
    }

    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let hint = NEXT_FREE_HINT.load(Ordering::Relaxed).max(1);

    unsafe {
        // Search from hint to end
        if let Some(page) = find_free_page(hint, total) {
            set_page_allocated(page);
            FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
            NEXT_FREE_HINT.store(page + 1, Ordering::Relaxed);
            PHYS_REFCOUNTS.lock().remove(&(page * PAGE_SIZE));
            return Some(page * PAGE_SIZE);
        }

        // Wrap around: search from page 1 to hint
        if hint > 1 {
            if let Some(page) = find_free_page(1, hint) {
                set_page_allocated(page);
                FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
                NEXT_FREE_HINT.store(page + 1, Ordering::Relaxed);
                PHYS_REFCOUNTS.lock().remove(&(page * PAGE_SIZE));
                return Some(page * PAGE_SIZE);
            }
        }
    }

    None
}

/// Reserve a specific physical page so it cannot be allocated later.
/// Returns true if the page is now reserved/allocated.
pub fn reserve_page(addr: usize) -> bool {
    if (addr & (PAGE_SIZE - 1)) != 0 {
        return false;
    }

    let page = addr / PAGE_SIZE;
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return false;
    }

    let _lock = BITMAP_LOCK.lock();

    unsafe {
        if is_page_free(page) {
            set_page_allocated(page);
            FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
            PHYS_REFCOUNTS.lock().remove(&addr);
        }
    }

    true
}

/// Fast free-page search using word-at-a-time scanning.
/// Scans bitmap from `start_page` to `end_page` (exclusive).
/// Returns the first free page index, or None.
///
/// SAFETY: caller must hold BITMAP_LOCK.
unsafe fn find_free_page(start_page: usize, end_page: usize) -> Option<usize> {
    let bm = bitmap_slice();
    let bm_len = bm.len();

    let mut page = start_page;

    // Align to byte boundary first (check individual bits)
    while page < end_page && !page.is_multiple_of(8) {
        let byte_idx = page / 8;
        if byte_idx >= bm_len {
            return None;
        }
        let bit = page % 8;
        if (bm[byte_idx] & (1 << bit)) == 0 {
            return Some(page);
        }
        page += 1;
    }

    // Word-at-a-time scan (8 pages per byte)
    let mut byte_idx = page / 8;
    while byte_idx < bm_len && page + 8 <= end_page {
        if bm[byte_idx] != 0xFF {
            // At least one free bit in this byte
            for bit in 0..8 {
                let p = byte_idx * 8 + bit;
                if p >= end_page {
                    break;
                }
                if (bm[byte_idx] & (1 << bit)) == 0 {
                    return Some(p);
                }
            }
        }
        byte_idx += 1;
        page = byte_idx * 8;
    }

    // Check remaining pages that don't fill a full byte
    while page < end_page {
        let byte_idx = page / 8;
        if byte_idx >= bm_len {
            return None;
        }
        let bit = page % 8;
        if (bm[byte_idx] & (1 << bit)) == 0 {
            return Some(page);
        }
        page += 1;
    }

    None
}

/// Free a single physical page.
pub fn free_page(addr: usize) -> Result<(), crate::mm::ValidationError> {
    validate_page_addr_for_refcount(addr)?;

    // Keep lock order consistent with alloc path: BITMAP -> REFCOUNTS.
    let _lock = BITMAP_LOCK.lock();

    let remaining_refs = {
        let mut refs = PHYS_REFCOUNTS.lock();
        dec_ref_locked(&mut refs, addr)
    };

    // Still shared elsewhere: physical frame remains allocated.
    if remaining_refs > 0 {
        return Ok(());
    }

    #[cfg(not(test))]
    crate::mm::validate_unprotected_resource(addr as u64, is_pml4_protected(addr))?;

    let page = addr / PAGE_SIZE;
    unsafe {
        if !is_page_free(page) {
            set_page_free(page);
            FREE_PAGES.fetch_add(1, Ordering::Relaxed);

            // Update hint if this page is before current hint
            let hint = NEXT_FREE_HINT.load(Ordering::Relaxed);
            if page < hint {
                NEXT_FREE_HINT.store(page, Ordering::Relaxed);
            }
        }
    }

    Ok(())
}

/// Allocate `count` contiguous physical pages. Returns the base physical address.
pub fn alloc_pages(count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }

    if count == 1 {
        return alloc_page();
    }

    let _lock = BITMAP_LOCK.lock();

    let free = FREE_PAGES.load(Ordering::Relaxed);
    if free < count {
        return None;
    }

    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let max_start = total.checked_sub(count)?;

    unsafe {
        // Try from hint first
        let hint = NEXT_FREE_HINT.load(Ordering::Relaxed).max(1);
        if let Some(start) = find_contiguous_run(hint, max_start, count) {
            mark_range_allocated(start, count);
            FREE_PAGES.fetch_sub(count, Ordering::Relaxed);
            NEXT_FREE_HINT.store(start + count, Ordering::Relaxed);
            let mut refs = PHYS_REFCOUNTS.lock();
            for i in 0..count {
                refs.remove(&((start + i) * PAGE_SIZE));
            }
            return Some(start * PAGE_SIZE);
        }

        // Wrap around
        if hint > 1 {
            if let Some(start) = find_contiguous_run(1, hint.min(max_start), count) {
                mark_range_allocated(start, count);
                FREE_PAGES.fetch_sub(count, Ordering::Relaxed);
                NEXT_FREE_HINT.store(start + count, Ordering::Relaxed);
                let mut refs = PHYS_REFCOUNTS.lock();
                for i in 0..count {
                    refs.remove(&((start + i) * PAGE_SIZE));
                }
                return Some(start * PAGE_SIZE);
            }
        }
    }

    None
}

/// Find a contiguous run of `count` free pages starting between `from` and `max_start` (inclusive).
/// SAFETY: caller must hold BITMAP_LOCK.
unsafe fn find_contiguous_run(from: usize, max_start: usize, count: usize) -> Option<usize> {
    let mut start = from;

    while start <= max_start {
        // Quick skip: if the first page of a potential run is allocated,
        // use byte-level skip for faster scanning
        let byte_idx = start / 8;
        let bm = bitmap_slice();
        if byte_idx < bm.len() && start.is_multiple_of(8) && bm[byte_idx] == 0xFF {
            // All 8 pages in this byte are allocated, skip them
            start += 8;
            continue;
        }

        if !is_page_free(start) {
            start += 1;
            continue;
        }

        // Found a free page — check if we have `count` contiguous free pages
        let mut run_len = 1;
        while run_len < count {
            let page = start + run_len;
            if page >= TOTAL_PAGES.load(Ordering::Relaxed) || !is_page_free(page) {
                break;
            }
            run_len += 1;
        }

        if run_len >= count {
            return Some(start);
        }

        // Skip past the failed run
        start += run_len + 1;
    }

    None
}

/// Mark a range of pages as allocated.
/// SAFETY: caller must hold BITMAP_LOCK.
unsafe fn mark_range_allocated(start: usize, count: usize) {
    for i in 0..count {
        set_page_allocated(start + i);
    }
}

/// Free `count` contiguous physical pages starting at `addr`.
#[allow(dead_code)]
pub fn free_pages(addr: usize, count: usize) -> Result<(), crate::mm::ValidationError> {
    crate::mm::validate_page_alignment(addr)?;
    crate::mm::validate_size(count, TOTAL_PAGES.load(Ordering::Relaxed))?;

    for i in 0..count {
        let page_addr = addr + i * PAGE_SIZE;
        // Ignore out-of-range slots in partially valid ranges, preserving
        // historical free_pages behaviour.
        let page = page_addr / PAGE_SIZE;
        let total = TOTAL_PAGES.load(Ordering::Relaxed);
        if page >= total || page <= 1 {
            continue;
        }
        free_page(page_addr)?;
    }

    Ok(())
}

/// Allocate a single zeroed page.
pub fn alloc_page_zeroed() -> Option<usize> {
    let addr = alloc_page()?;

    unsafe {
        let ptr = crate::mm::vm::phys_to_virt_ptr(addr) as *mut u8;
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }

    Some(addr)
}

/// Allocate `count` contiguous zeroed pages.
#[allow(dead_code)]
pub fn alloc_pages_zeroed(count: usize) -> Option<usize> {
    let count = count.max(1);
    let addr = alloc_pages(count)?;

    unsafe {
        core::ptr::write_bytes(
            crate::mm::vm::phys_to_virt_ptr(addr) as *mut u8,
            0,
            count * PAGE_SIZE,
        );
    }

    Some(addr)
}

// ---------------------------------------------------------------------------
// PML4 Protection Registry Functions
// ---------------------------------------------------------------------------

/// Register a PML4 page table as active and protected from freeing.
/// This should be called when a PML4 is created or becomes active in an address space.
///
/// # Arguments
/// * `pml4_phys` - Physical address of the PML4 page table
///
/// # Requirements
/// Implements Req 2.2, Req 2.4
pub fn register_active_pml4(pml4_phys: usize) -> Result<(), crate::mm::ValidationError> {
    crate::mm::validate_page_alignment(pml4_phys)?;

    let mut guard = PROTECTED_PML4S.lock();

    let was_present = guard.contains(pml4_phys);
    if let Err(e) = guard.insert(pml4_phys) {
        crate::log_error!(
            "[pmm]",
            "register_active_pml4 failed for pml4=0x{:X}: {:?}",
            pml4_phys,
            e
        );
        return Err(e);
    }

    if !was_present {
        log_debug!("[pmm]", "Registered protected PML4 at phys 0x{:X}", pml4_phys);
    }

    Ok(())
}

/// Unregister a PML4 page table, allowing it to be freed.
/// This should be called when an address space is destroyed and its PML4 is no longer needed.
///
/// # Arguments
/// * `pml4_phys` - Physical address of the PML4 page table
///
/// # Requirements
/// Implements Req 2.5
pub fn unregister_active_pml4(pml4_phys: usize) -> Result<(), crate::mm::ValidationError> {
    crate::mm::validate_page_alignment(pml4_phys)?;

    let mut guard = PROTECTED_PML4S.lock();

    if guard.remove(pml4_phys) {
        log_debug!("[pmm]", "Unregistered protected PML4 at phys 0x{:X}", pml4_phys);
    }

    Ok(())
}

/// Check if a physical page is a protected PML4 page table.
/// Returns true if the page is registered as an active PML4 and must not be freed.
///
/// # Arguments
/// * `pml4_phys` - Physical address to check
///
/// # Returns
/// `true` if the page is a protected PML4, `false` otherwise
///
/// # Requirements
/// Implements Req 2.1, Req 2.3
pub fn is_pml4_protected(pml4_phys: usize) -> bool {
    if !pml4_phys.is_multiple_of(PAGE_SIZE) {
        return false;
    }

    let guard = PROTECTED_PML4S.lock();
    guard.contains(pml4_phys)
}

// ---------------------------------------------------------------------------
// Alignment helpers
// ---------------------------------------------------------------------------

pub fn is_page_aligned(addr: usize) -> bool {
    addr.is_multiple_of(PAGE_SIZE)
}

pub fn align_down(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

pub fn align_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

#[allow(dead_code)]
pub fn addr_to_page(addr: usize) -> usize {
    addr / PAGE_SIZE
}

#[allow(dead_code)]
pub fn page_to_addr(page: usize) -> usize {
    page * PAGE_SIZE
}

// ---------------------------------------------------------------------------
// Statistics & diagnostics
// ---------------------------------------------------------------------------

pub fn get_stats() -> (usize, usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);
    (total, free)
}

#[allow(dead_code)]
pub fn get_detailed_stats() -> MemoryStats {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);
    let used = total.saturating_sub(free);

    MemoryStats {
        total_pages: total,
        free_pages: free,
        used_pages: used,
        total_bytes: total * PAGE_SIZE,
        free_bytes: free * PAGE_SIZE,
        used_bytes: used * PAGE_SIZE,
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct MemoryStats {
    pub total_pages: usize,
    pub free_pages: usize,
    pub used_pages: usize,
    pub total_bytes: usize,
    pub free_bytes: usize,
    pub used_bytes: usize,
}

/// Get memory statistics in KB for userspace.
/// Returns (total_kb, free_kb).
/// Note: total_kb is the actual physical RAM available, not the address space size.
pub fn get_memory_stats() -> (u64, u64) {
    let total = PHYSICAL_RAM_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);

    let total_kb = (total * PAGE_SIZE / 1024) as u64;
    let free_kb = (free * PAGE_SIZE / 1024) as u64;

    (total_kb, free_kb)
}

/// Get extended memory information for boot diagnostics.
#[allow(dead_code)]
pub fn get_boot_diagnostics() -> BootMemoryDiagnostics {
    BootMemoryDiagnostics {
        physical_ram_pages: PHYSICAL_RAM_PAGES.load(Ordering::Relaxed),
        reserved_pages: RESERVED_PAGES.load(Ordering::Relaxed),
        tracked_pages: TOTAL_PAGES.load(Ordering::Relaxed),
        free_pages: FREE_PAGES.load(Ordering::Relaxed),
        largest_free_run: LARGEST_FREE_RUN.load(Ordering::Relaxed),
        bitmap_overhead_pages: BITMAP_OVERHEAD_PAGES.load(Ordering::Relaxed),
        using_dynamic_bitmap: USING_DYNAMIC.load(Ordering::Relaxed),
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct BootMemoryDiagnostics {
    pub physical_ram_pages: usize,
    pub reserved_pages: usize,
    pub tracked_pages: usize,
    pub free_pages: usize,
    pub largest_free_run: usize,
    pub bitmap_overhead_pages: usize,
    pub using_dynamic_bitmap: bool,
}
