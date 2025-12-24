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
// - Fixed maximum physical memory limit (`MAX_PAGES`) for predictable bounds
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
// - Fixed upper bound on addressable physical memory
//
// Public interface:
// - `alloc_page` / `free_page` for single-page management
// - `alloc_pages` / `free_pages` for contiguous ranges
// - Zeroed variants for safe page table and heap initialization
// - Utility helpers for alignment and statistics reporting

use crate::boot::{MemoryMap, EFI_CONVENTIONAL_MEMORY};
#[allow(unused_imports)]
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use crate::{log_info};

const MAX_PAGES: usize = 256 * 1024;
static mut BITMAP: [u8; MAX_PAGES / 8] = [0xFF; MAX_PAGES / 8];  // 32 KiB bitmap
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(0);
static FREE_PAGES: AtomicUsize = AtomicUsize::new(0);
static NEXT_FREE_HINT: AtomicUsize = AtomicUsize::new(0);
static LARGEST_FREE_RUN: AtomicUsize = AtomicUsize::new(0);
pub const PAGE_SIZE: usize = 4096;

#[cfg(debug_assertions)]
#[allow(dead_code)]
static ALLOC_TRACE: AtomicBool = AtomicBool::new(false);

pub unsafe fn init(memory_map: &MemoryMap) {
    use core::sync::atomic::Ordering;

    core::ptr::write_bytes(
        core::ptr::addr_of_mut!(BITMAP).cast::<u8>(),
        0xFF,
        MAX_PAGES / 8,
    );

    let mut tracked_end_page: usize = 0;

    for d in memory_map.descriptors() {
        let start_page = (d.physical_start as usize) / PAGE_SIZE;
        let num_pages = d.number_of_pages as usize;
        let end_page = start_page.saturating_add(num_pages);

        if end_page > tracked_end_page {
            tracked_end_page = end_page;
        }
    }

    let total_pages = tracked_end_page.min(MAX_PAGES);

    TOTAL_PAGES.store(total_pages, Ordering::Relaxed);
    NEXT_FREE_HINT.store(0, Ordering::Relaxed);

    let mut free_pages: usize = 0;

    for d in memory_map.descriptors() {
        if d.typ != EFI_CONVENTIONAL_MEMORY {
            continue;
        }

        let start_page = (d.physical_start as usize) / PAGE_SIZE;
        let num_pages = d.number_of_pages as usize;
        let end_page = start_page.saturating_add(num_pages).min(total_pages);

        if start_page >= total_pages {
            continue;
        }

        for page in start_page..end_page {
            // Agora TOTAL_PAGES já está setado, então set_page_free funciona.
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

    log_info!(
        "[pmm]",
        "PMM initialized: tracked_pages={}, free_pages={}, largest_free_run={} pages",
        total_pages,
        free_pages,
        max_run
    );
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
        for page in 0..total {
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
    if page >= MAX_PAGES {
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
    (BITMAP[byte] & (1 << bit)) == 0
}

unsafe fn set_page_free(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }

    let byte = page / 8;
    let bit = page % 8;
    BITMAP[byte] &= !(1 << bit);
}

unsafe fn set_page_allocated(page: usize) {
    let total = TOTAL_PAGES.load(Ordering::Relaxed);
    if page >= total {
        return;
    }

    let byte = page / 8;
    let bit = page % 8;
    BITMAP[byte] |= 1 << bit;
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
        'outer: for start in 0..=max_start {
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