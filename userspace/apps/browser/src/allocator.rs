//! Global heap allocator for the browser.
//!
//! A lock-guarded bump allocator backed by lazily-`mmap`ed chunks. Keeping the
//! heap out of `.bss` matters on Atom: executable BSS is mapped eagerly, so a
//! multi-MiB static heap would force the browser to consume physical memory the
//! moment it launches — memory other apps need for their surfaces. Growing on
//! demand keeps idle resource use low (focus: low RAM footprint).

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atom_syscall::atom_abi::{MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, USER_PAGE_SIZE};
use atom_syscall::vm::mmap_anon;

/// Minimum bytes requested from the kernel per growth step.
const HEAP_CHUNK_SIZE: usize = 256 * 1024;

/// Hard cap on total heap bytes mapped from the kernel. The kernel uses
/// deferred physical allocation, so mmap() succeeds even when RAM is
/// exhausted — the fatal page fault only arrives on first access. By
/// refusing to mmap beyond this limit we ensure OOM is reported through
/// the alloc_error_handler (a clean log + exit) rather than a kernel
/// page fault that terminates the process without any diagnostic.
const MAX_HEAP_BYTES: usize = 64 * 1024 * 1024;

pub struct MmapBumpAllocator {
    lock: AtomicBool,
    next: AtomicUsize,
    end: AtomicUsize,
    total_mapped: AtomicUsize,
}

// Safety: all shared state is accessed under the spin `lock`.
unsafe impl Sync for MmapBumpAllocator {}

impl MmapBumpAllocator {
    pub const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            next: AtomicUsize::new(0),
            end: AtomicUsize::new(0),
            total_mapped: AtomicUsize::new(0),
        }
    }

    fn lock(&self) {
        while self
            .lock
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.lock.store(false, Ordering::Release);
    }

    fn align_up(value: usize, align: usize) -> Option<usize> {
        let mask = align.checked_sub(1)?;
        value.checked_add(mask).map(|v| v & !mask)
    }

    /// Request a fresh chunk large enough for `min_size` bytes at `align`.
    fn grow(&self, min_size: usize, align: usize) -> Option<(usize, usize)> {
        let needed = min_size.checked_add(align)?;
        let chunk = Self::align_up(needed.max(HEAP_CHUNK_SIZE), USER_PAGE_SIZE)?;
        if self.total_mapped.load(Ordering::Relaxed).saturating_add(chunk) > MAX_HEAP_BYTES {
            return None;
        }
        let addr = mmap_anon(chunk, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE).ok()?;
        self.total_mapped.fetch_add(chunk, Ordering::Relaxed);
        Some((addr as usize, chunk))
    }

    /// Try to carve `size` bytes at `align` from the live chunk `[next, end)`.
    fn try_bump(&self, size: usize, align: usize) -> Option<*mut u8> {
        let cur = self.next.load(Ordering::Relaxed);
        if cur == 0 {
            return None;
        }
        let end = self.end.load(Ordering::Relaxed);
        let aligned = Self::align_up(cur, align)?;
        let next = aligned.checked_add(size)?;
        if next <= end {
            self.next.store(next, Ordering::Relaxed);
            Some(aligned as *mut u8)
        } else {
            None
        }
    }
}

unsafe impl GlobalAlloc for MmapBumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(1);
        let align = layout.align().max(16);

        self.lock();
        if let Some(ptr) = self.try_bump(size, align) {
            self.unlock();
            return ptr;
        }

        // Current chunk exhausted: map a new one and bump from its base.
        let result = self
            .grow(size, align)
            .and_then(|(base, chunk)| {
                let aligned = Self::align_up(base, align)?;
                let next = aligned.checked_add(size)?;
                let new_end = base.checked_add(chunk)?;
                (next <= new_end).then(|| {
                    self.next.store(next, Ordering::Relaxed);
                    self.end.store(new_end, Ordering::Relaxed);
                    aligned as *mut u8
                })
            })
            .unwrap_or_default();
        self.unlock();
        result
    }

    // Bump allocators never reclaim individual allocations.
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}
