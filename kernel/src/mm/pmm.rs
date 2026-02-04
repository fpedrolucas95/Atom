// Physical Memory Manager (PMM)
//
// Implements a low-level physical page allocator based on a bitmap.
// This module is responsible for tracking and allocating physical memory
// pages discovered from the UEFI memory map during early boot.
//
// Key responsibilities:
// - Parse the UEFI memory map to discover usable physical memory
// - Track free and allocated pages using a compact bitmap
// - Allocate and free single pages or contiguous page ranges
// - Provide zero-initialized page allocations for higher-level subsystems
// - Expose memory usage statistics for diagnostics
//
// Design principles:
// - Simplicity and determinism suitable for early kernel initialization
// - Dynamic physical memory tracking based on UEFI memory map
// - Lock-free operation using atomics, assuming early-boot or coarse-grained use
// - Page-granular allocation with a fixed page size (4 KiB)
//
// Implementation details:
// - One bit per page: 0 = free, 1 = allocated
// - Bitmap is statically allocated and initialized to “all allocated”
// - Only EFI_CONVENTIONAL_MEMORY regions are marked free
// - `NEXT_FREE_HINT` provides a simple next-fit optimization for allocations
// - Contiguous allocation scans linearly for free runs of pages
//
// Correctness and safety notes:
// - All bitmap manipulation is `unsafe` and must respect bounds
// - No protection against double-free beyond bitmap state checks
// - `Relaxed` atomics are sufficient because strict ordering is unnecessary
// - Linear scans make large allocations potentially expensive
//
// Limitations and future considerations:
// - No NUMA awareness or memory zones (DMA, highmem, etc.)
// - No defragmentation or advanced allocation strategies
//
// Public interface:
// - `alloc_page` / `free_page` for single-page management
// - `alloc_pages` / `free_pages` for contiguous ranges
// - Zeroed variants for safe page table and heap initialization
// - Utility helpers for alignment and statistics reporting

use crate::boot::{MemoryMap, EFI_CONVENTIONAL_MEMORY, EFI_BOOT_SERVICES_CODE, EFI_BOOT_SERVICES_DATA};
#[allow(unused_imports)]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use crate::{log_info, log_debug};

static mut BITMAP: *mut u8 = core::ptr::null_mut();
static BITMAP_SIZE_BYTES: AtomicUsize = AtomicUsize::new(0);
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(0);
static FREE_PAGES: AtomicUsize = AtomicUsize::new(0);
static NEXT_FREE_HINT: AtomicUsize = AtomicUsize::new(0);
static LARGEST_FREE_RUN: AtomicUsize = AtomicUsize::new(0);
/// Actual physical RAM available (sum of all conventional memory regions)
static PHYSICAL_RAM_PAGES: AtomicUsize = AtomicUsize::new(0);
pub const PAGE_SIZE: usize = 4096;

#[cfg(debug_assertions)]
#[allow(dead_code)]
static ALLOC_TRACE: AtomicBool = AtomicBool::new(false);

/// Check if an EFI memory type is usable by the kernel after ExitBootServices()
#[inline]
fn is_usable_memory(typ: u32) -> bool {
    // After ExitBootServices(), these memory types become available for kernel use:
    // - EFI_CONVENTIONAL_MEMORY (7): standard free memory
    // - EFI_BOOT_SERVICES_CODE (3): UEFI boot services code (no longer needed)
    // - EFI_BOOT_SERVICES_DATA (4): UEFI boot services data (no longer needed)
    typ == EFI_CONVENTIONAL_MEMORY
        || typ == EFI_BOOT_SERVICES_CODE
        || typ == EFI_BOOT_SERVICES_DATA
}

/// Get human-readable name for EFI memory type (for debug logging)
#[allow(dead_code)]
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

pub unsafe fn init(memory_map: &MemoryMap) {
    use core::sync::atomic::Ordering;

    let mut tracked_end_page: usize = 0;
    let mut physical_ram_pages: usize = 0;

    // First pass: calculate total physical RAM and highest address
    for d in memory_map.descriptors() {
        let start_page = (d.physical_start as usize) / PAGE_SIZE;
        let num_pages = d.number_of_pages as usize;
        let end_page = start_page.saturating_add(num_pages);

        // Only track up to the highest USABLE page to keep the bitmap size reasonable
        if is_usable_memory(d.typ) {
            if end_page > tracked_end_page {
                tracked_end_page = end_page;
            }
            physical_ram_pages += num_pages;
        }
    }

    let total_pages = tracked_end_page;
    let bitmap_size_bytes = (total_pages + 7) / 8;

    // Second pass: Find a suitable region for the bitmap in conventional memory
    let mut bitmap_phys_addr = 0usize;
    for d in memory_map.descriptors() {
        if d.typ == EFI_CONVENTIONAL_MEMORY {
            let reg_size = (d.number_of_pages as usize) * PAGE_SIZE;
            // Ensure we have enough space and we are not at address 0
            if reg_size >= bitmap_size_bytes && d.physical_start > 0 {
                bitmap_phys_addr = d.physical_start as usize;
                break;
            }
        }
    }

    if bitmap_phys_addr == 0 {
        panic!("[pmm] Failed to find a suitable memory region for the PMM bitmap ({} bytes)", bitmap_size_bytes);
    }

    BITMAP = bitmap_phys_addr as *mut u8;
    BITMAP_SIZE_BYTES.store(bitmap_size_bytes, Ordering::Relaxed);
    TOTAL_PAGES.store(total_pages, Ordering::Relaxed);
    PHYSICAL_RAM_PAGES.store(physical_ram_pages, Ordering::Relaxed);
    NEXT_FREE_HINT.store(0, Ordering::Relaxed);

    // Initialize bitmap to 0xFF (all allocated)
    core::ptr::write_bytes(BITMAP, 0xFF, bitmap_size_bytes);

    let bitmap_start_page = bitmap_phys_addr / PAGE_SIZE;
    let bitmap_num_pages = (bitmap_size_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut free_pages: usize = 0;

    // Third pass: mark usable regions as free and log them
    log_debug!("[pmm]", "Memory map regions:");
    for d in memory_map.descriptors() {
        let start_page = (d.physical_start as usize) / PAGE_SIZE;
        let num_pages = d.number_of_pages as usize;
        let end_page = start_page.saturating_add(num_pages);
        let size_mb = (num_pages * PAGE_SIZE) / (1024 * 1024);

        // Log significant memory regions (>= 1MB) for debugging
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

        // Only mark usable memory regions as free
        if !is_usable_memory(d.typ) {
            continue;
        }

        for page in start_page..end_page {
            // Skip pages used by the bitmap itself
            if page >= bitmap_start_page && page < (bitmap_start_page + bitmap_num_pages) {
                continue;
            }
            // Skip page 0 to prevent null pointer allocations
            if page == 0 {
                continue;
            }

            set_page_free(page);
            free_pages += 1;
        }
    }

    FREE_PAGES.store(free_pages, Ordering::Relaxed);

    let mut current_run = 0usize;
    let mut max_run = 0usize;

    for page in 0..total_pages {
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

    let physical_ram_mb = (physical_ram_pages * PAGE_SIZE) / (1024 * 1024);
    log_info!("[pmm]", "Physical memory detected: {} MiB", physical_ram_mb);
    log_info!("[pmm]", "Tracked pages: {}", total_pages);
    log_info!("[pmm]", "Bitmap size: {} bytes", bitmap_size_bytes);
    log_info!("[pmm]", "Free pages: {}", free_pages);
    log_info!("[pmm]", "Largest contiguous block: {} pages", max_run);
}

#[allow(dead_code)]
pub fn enable_alloc_trace() {
    #[cfg(debug_assertions)]
    ALLOC_TRACE.store(true, Ordering::Relaxed);
}

pub fn alloc_page() -> Option<usize> {
    let free = FREE_PAGES.load(Ordering::Relaxed);
    if free == 0 {
        return None;
    }

    let total = TOTAL_PAGES.load(Ordering::Relaxed);

    unsafe {
        // Start from page 1 to avoid allocating the null page (page 0)
        // This helps catch null pointer dereferences
        for page in 1..total {
            if is_page_free(page) {
                set_page_allocated(page);
                FREE_PAGES.fetch_sub(1, Ordering::Relaxed);
                return Some(page * PAGE_SIZE);
            }
        }
    }

    None
}

pub fn free_page(addr: usize) {
    if addr % PAGE_SIZE != 0 {
        return;
    }

    let page = addr / PAGE_SIZE;
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }

    unsafe {
        if !is_page_free(page) {
            set_page_free(page);
            FREE_PAGES.fetch_add(1, Ordering::Relaxed);
        }
    }
}

unsafe fn is_page_free(page: usize) -> bool {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return false;
    }

    let byte = page / 8;
    let bit = page % 8;
    (*BITMAP.add(byte) & (1 << bit)) == 0
}

unsafe fn set_page_free(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }

    let byte = page / 8;
    let bit = page % 8;
    *BITMAP.add(byte) &= !(1 << bit);
}

unsafe fn set_page_allocated(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }

    let byte = page / 8;
    let bit = page % 8;
    *BITMAP.add(byte) |= 1 << bit;
}

pub fn get_stats() -> (usize, usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);
    (total, free)
}

pub fn alloc_pages(count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }

    if count == 1 {
        return alloc_page();
    }

    let free = FREE_PAGES.load(Ordering::Relaxed);
    if free < count {
        return None;
    }

    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let max_start = total.checked_sub(count)?;

    unsafe {
        // Start from page 1 to avoid allocating the null page (page 0)
        'outer: for start in 1..=max_start {
            for i in 0..count {
                if !is_page_free(start + i) {
                    continue 'outer;
                }
            }

            for i in 0..count {
                set_page_allocated(start + i);
            }

            FREE_PAGES.fetch_sub(count, Ordering::Relaxed);
            return Some(start * PAGE_SIZE);
        }
    }

    None
}

#[allow(dead_code)]
pub fn free_pages(addr: usize, count: usize) {
    for i in 0..count {
        free_page(addr + i * PAGE_SIZE);
    }
}

pub fn alloc_page_zeroed() -> Option<usize> {
    let addr = alloc_page()?;

    unsafe {
        let ptr = addr as *mut u8;
        core::ptr::write_bytes(ptr, 0, PAGE_SIZE);
    }

    Some(addr)
}

#[allow(dead_code)]
pub fn alloc_pages_zeroed(count: usize) -> Option<usize> {
    let count = count.max(1);
    let addr = alloc_pages(count)?;

    unsafe {
        core::ptr::write_bytes(
            addr as *mut u8,
            0,
            count * PAGE_SIZE,
        );
    }

    Some(addr)
}

pub fn is_page_aligned(addr: usize) -> bool {
    addr % PAGE_SIZE == 0
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

#[allow(dead_code)]
pub fn get_detailed_stats() -> MemoryStats {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);
    let used = total - free;

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

/// Get memory statistics in KB for userspace
/// Returns (total_kb, free_kb)
/// Note: total_kb is the actual physical RAM available, not the address space size
pub fn get_memory_stats() -> (u64, u64) {
    let total = PHYSICAL_RAM_PAGES.load(Ordering::Relaxed);
    let free = FREE_PAGES.load(Ordering::Relaxed);

    let total_kb = (total * PAGE_SIZE / 1024) as u64;
    let free_kb = (free * PAGE_SIZE / 1024) as u64;

    (total_kb, free_kb)
}

pub fn self_test() {
    log_info!("[pmm]", "Starting PMM self-test...");

    // Test 1: Single page allocation/free
    let page = alloc_page().expect("self_test: failed to allocate single page");
    free_page(page);
    log_debug!("[pmm]", "  Test 1 (single page) passed");

    // Test 2: Contiguous pages allocation/free
    let count = 1024; // 4 MiB
    let pages = alloc_pages(count).expect("self_test: failed to allocate 1024 contiguous pages");
    free_pages(pages, count);
    log_debug!("[pmm]", "  Test 2 (contiguous 4MB block) passed");

    // Test 3: Stress test (many small allocations)
    let mut allocated = [0usize; 100];
    for i in 0..100 {
        allocated[i] = alloc_page().expect("self_test: stress test allocation failed");
    }
    for i in 0..100 {
        free_page(allocated[i]);
    }
    log_debug!("[pmm]", "  Test 3 (stress test 100 pages) passed");

    log_info!("[pmm]", "PMM self-test completed successfully.");
}