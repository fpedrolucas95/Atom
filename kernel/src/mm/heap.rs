// x86_64 Kernel Heap Allocator
//
// This module provides a simple, contiguous kernel heap for dynamic memory
// allocation. It wraps physical page allocations from the PMM and exposes
// a `GlobalAlloc` interface for Rust code. It includes basic alignment,
// statistics tracking, and handles failures gracefully during initialization.

use super::pmm::{alloc_pages, PAGE_SIZE};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::{log_info, log_panic, log_warn};
use crate::arch::halt;

const HEAP_SIZE: usize = 4 * 1024 * 1024;

static HEAP_START: AtomicUsize = AtomicUsize::new(0);
static HEAP_POS: AtomicUsize = AtomicUsize::new(0);
static HEAP_END: AtomicUsize = AtomicUsize::new(0);

pub struct KernelAllocator;

pub fn init() {
    let num_pages = HEAP_SIZE / PAGE_SIZE;
    let (heap_base, actual_pages) = match alloc_pages(num_pages) {
        Some(base) => (base, num_pages),
        None => {
            log_warn!("heap", "Failed to allocate {} contiguous pages, trying 1 MiB", num_pages);
            match alloc_pages(num_pages / 4) {
                Some(base) => (base, num_pages / 4),
                None => {
                    log_panic!("heap", "FATAL: Cannot allocate kernel heap!");
                    loop { halt(); }
                }
            }
        }
    };

    let actual_size = actual_pages * PAGE_SIZE;
    HEAP_START.store(heap_base, Ordering::Relaxed);
    HEAP_POS.store(heap_base, Ordering::Relaxed);
    HEAP_END.store(heap_base + actual_size, Ordering::Relaxed);

    log_info!("heap", "Initialized with {} bytes at 0x{:X}", actual_size, heap_base);
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let heap_start = HEAP_START.load(Ordering::Relaxed);

        if heap_start == 0 {
            return null_mut();
        }

        let size = layout.size();
        let align = layout.align();

        let current = HEAP_POS.load(Ordering::Relaxed);
        let aligned = align_up(current, align);

        let new_pos = aligned + size;
        let heap_end = HEAP_END.load(Ordering::Relaxed); 

        if new_pos > heap_end {
            return null_mut();
        }

        HEAP_POS.store(new_pos, Ordering::Relaxed);

        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

fn align_up(val: usize, align: usize) -> usize {
    (val + align - 1) & !(align - 1)
}

#[allow(dead_code)]
pub fn get_stats() -> (usize, usize) {
    let start = HEAP_START.load(Ordering::Relaxed);
    let end = HEAP_END.load(Ordering::Relaxed);
    let pos = HEAP_POS.load(Ordering::Relaxed);

    if start == 0 {
        return (0, 0);
    }

    let total = end - start;
    let used = pos - start;
    (total, used)
}