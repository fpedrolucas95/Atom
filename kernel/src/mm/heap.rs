// x86_64 Kernel Heap Allocator — Slab + Fallback
//
// Replaces the previous bump allocator with a slab-based design that
// actually reclaims memory on dealloc. This prevents the structural
// memory leak that the bump allocator suffered from.
//
// Architecture:
// - Small allocations (≤ 2048 bytes) go through size-class slab caches.
//   Each cache manages a linked list of free blocks of a fixed size.
// - Large allocations (> 2048 bytes) are served directly from the PMM
//   as full pages, with a small header tracking the allocation size.
//
// Slab size classes: 32, 64, 128, 256, 512, 1024, 2048 bytes.
// Each slab page is carved into blocks of the class size. When a slab
// is exhausted, a new page is allocated from the PMM and carved up.
//
// Thread safety: a single spinlock protects all slab state. This is
// acceptable for a uniprocessor kernel; SMP would need per-CPU caches.

use super::pmm;
use crate::mm::vm;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use spin::Mutex;
use crate::{log_info, log_warn};

/// Slab size classes — powers of two from 32 to 2048
const SIZE_CLASSES: [usize; 7] = [32, 64, 128, 256, 512, 1024, 2048];
const NUM_CLASSES: usize = SIZE_CLASSES.len();

/// Threshold: allocations larger than this go directly to PMM
const LARGE_THRESHOLD: usize = 2048;

/// Header prepended to large (page-based) allocations
const LARGE_HEADER_SIZE: usize = 16; // size + magic, aligned to 16

/// Magic number for large allocation validation
const LARGE_MAGIC: usize = 0xA110_CA7E;

/// A free block in a slab — just a pointer to the next free block
#[repr(C)]
struct FreeBlock {
    next: *mut FreeBlock,
}

/// A slab cache for one size class
struct SlabCache {
    /// Size of each allocation in this class
    block_size: usize,
    /// Linked list of free blocks
    free_list: *mut FreeBlock,
    /// Number of allocations served
    alloc_count: usize,
    /// Number of deallocations
    dealloc_count: usize,
}

impl SlabCache {
    const fn new(block_size: usize) -> Self {
        Self {
            block_size,
            free_list: null_mut(),
            alloc_count: 0,
            dealloc_count: 0,
        }
    }

    /// Allocate a block from this cache. If empty, request a new page.
    fn alloc(&mut self) -> *mut u8 {
        if self.free_list.is_null() {
            if !self.grow() {
                return null_mut();
            }
        }

        let block = self.free_list;
        unsafe {
            self.free_list = (*block).next;
        }
        self.alloc_count += 1;
        block as *mut u8
    }

    /// Return a block to this cache's free list
    fn dealloc(&mut self, ptr: *mut u8) {
        let block = ptr as *mut FreeBlock;
        unsafe {
            (*block).next = self.free_list;
        }
        self.free_list = block;
        self.dealloc_count += 1;
    }

    /// Allocate a new page from PMM and carve it into blocks
    fn grow(&mut self) -> bool {
        let phys = match pmm::alloc_page_zeroed() {
            Some(p) => p,
            None => return false,
        };

        let virt = vm::phys_to_virt_ptr(phys);
        let blocks_per_page = pmm::PAGE_SIZE / self.block_size;

        for i in 0..blocks_per_page {
            let block_addr = virt + i * self.block_size;
            let block = block_addr as *mut FreeBlock;
            unsafe {
                (*block).next = self.free_list;
            }
            self.free_list = block;
        }

        true
    }
}

// Safety: SlabAllocator contains raw pointers (FreeBlock*) that are only
// accessed while holding the ALLOCATOR_STATE spinlock. The pointers are
// to kernel heap memory and are never shared across threads unsynchronized.
unsafe impl Send for SlabCache {}

/// The allocator state, protected by a spinlock
struct SlabAllocator {
    caches: [SlabCache; NUM_CLASSES],
    large_alloc_count: usize,
    large_dealloc_count: usize,
    initialized: bool,
}

impl SlabAllocator {
    const fn new() -> Self {
        Self {
            caches: [
                SlabCache::new(32),
                SlabCache::new(64),
                SlabCache::new(128),
                SlabCache::new(256),
                SlabCache::new(512),
                SlabCache::new(1024),
                SlabCache::new(2048),
            ],
            large_alloc_count: 0,
            large_dealloc_count: 0,
            initialized: false,
        }
    }

    /// Find the appropriate size class for a given layout
    fn class_index(size: usize) -> Option<usize> {
        for (i, &class_size) in SIZE_CLASSES.iter().enumerate() {
            if size <= class_size {
                return Some(i);
            }
        }
        None
    }

    fn alloc(&mut self, layout: Layout) -> *mut u8 {
        if !self.initialized {
            return null_mut();
        }

        let size = layout.size().max(layout.align());

        // Try slab allocation for small sizes
        if size <= LARGE_THRESHOLD {
            if let Some(idx) = Self::class_index(size) {
                // Ensure alignment — slab blocks are naturally aligned to their
                // size (power of two), so alignment ≤ block_size is guaranteed.
                if layout.align() <= SIZE_CLASSES[idx] {
                    return self.caches[idx].alloc();
                }
            }
        }

        // Large allocation: allocate full pages from PMM
        self.alloc_large(layout)
    }

    fn dealloc(&mut self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() || !self.initialized {
            return;
        }

        let size = layout.size().max(layout.align());

        // Check if this was a slab allocation
        if size <= LARGE_THRESHOLD {
            if let Some(idx) = Self::class_index(size) {
                if layout.align() <= SIZE_CLASSES[idx] {
                    self.caches[idx].dealloc(ptr);
                    return;
                }
            }
        }

        // Large allocation: free the pages
        self.dealloc_large(ptr);
    }

    fn alloc_large(&mut self, layout: Layout) -> *mut u8 {
        let total_size = layout.size() + LARGE_HEADER_SIZE;
        let pages = (total_size + pmm::PAGE_SIZE - 1) / pmm::PAGE_SIZE;

        let phys = match pmm::alloc_pages_zeroed(pages) {
            Some(p) => p,
            None => return null_mut(),
        };

        let virt = vm::phys_to_virt_ptr(phys);

        // Write header: [size_in_pages | magic]
        unsafe {
            let header = virt as *mut usize;
            *header = pages;
            *header.add(1) = LARGE_MAGIC;
        }

        self.large_alloc_count += 1;

        // Return pointer past the header
        (virt + LARGE_HEADER_SIZE) as *mut u8
    }

    fn dealloc_large(&mut self, ptr: *mut u8) {
        let header_ptr = (ptr as usize - LARGE_HEADER_SIZE) as *const usize;

        unsafe {
            let pages = *header_ptr;
            let magic = *header_ptr.add(1);

            if magic != LARGE_MAGIC {
                // Corruption detected — don't free to avoid damage
                return;
            }

            // Convert virtual back to physical
            let virt_base = ptr as usize - LARGE_HEADER_SIZE;
            // The virt address is HIGHER_HALF_BASE + phys, so:
            let phys = virt_base - vm::HIGHER_HALF_BASE;

            pmm::free_pages(phys, pages);
            self.large_dealloc_count += 1;
        }
    }
}

static ALLOCATOR_STATE: Mutex<SlabAllocator> = Mutex::new(SlabAllocator::new());

pub struct KernelAllocator;

pub fn init() {
    // Pre-populate all slab caches with at least one page each
    let mut state = ALLOCATOR_STATE.lock();
    state.initialized = true;

    for cache in state.caches.iter_mut() {
        if !cache.grow() {
            log_warn!("heap", "Failed to pre-populate slab cache for size {}", cache.block_size);
        }
    }

    log_info!("heap", "Slab allocator initialized — {} size classes (32-2048), large allocs via PMM", NUM_CLASSES);
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut state = ALLOCATOR_STATE.lock();
        state.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut state = ALLOCATOR_STATE.lock();
        state.dealloc(ptr, layout);
    }
}

#[allow(dead_code)]
pub fn get_stats() -> (usize, usize) {
    let state = ALLOCATOR_STATE.lock();
    let mut total_allocs = 0usize;
    let mut total_deallocs = 0usize;

    for cache in state.caches.iter() {
        total_allocs += cache.alloc_count;
        total_deallocs += cache.dealloc_count;
    }

    total_allocs += state.large_alloc_count;
    total_deallocs += state.large_dealloc_count;

    // Return (total_allocations, active_allocations) for compatibility
    (total_allocs, total_allocs.saturating_sub(total_deallocs))
}

/// Get detailed slab statistics
#[allow(dead_code)]
pub fn get_detailed_stats() -> SlabStats {
    let state = ALLOCATOR_STATE.lock();
    let mut classes = [SlabClassStats { block_size: 0, alloc_count: 0, dealloc_count: 0, active: 0 }; NUM_CLASSES];

    for (i, cache) in state.caches.iter().enumerate() {
        classes[i] = SlabClassStats {
            block_size: cache.block_size,
            alloc_count: cache.alloc_count,
            dealloc_count: cache.dealloc_count,
            active: cache.alloc_count.saturating_sub(cache.dealloc_count),
        };
    }

    SlabStats {
        classes,
        large_alloc_count: state.large_alloc_count,
        large_dealloc_count: state.large_dealloc_count,
    }
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct SlabClassStats {
    pub block_size: usize,
    pub alloc_count: usize,
    pub dealloc_count: usize,
    pub active: usize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SlabStats {
    pub classes: [SlabClassStats; NUM_CLASSES],
    pub large_alloc_count: usize,
    pub large_dealloc_count: usize,
}
