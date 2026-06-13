//! Reusable mmap-backed allocator for the long-lived network daemon.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atom_syscall::atom_abi::{MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, USER_PAGE_SIZE};

const POOL_SIZE: usize = 4 * 1024 * 1024;
const POOL_ALIGN: usize = 16;
const CLASS_SIZES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_CLASSES: usize = CLASS_SIZES.len();
const MAX_CLASS: usize = CLASS_SIZES[NUM_CLASSES - 1];

struct Heap {
    lock: AtomicBool,
    base: AtomicUsize,
    bump: AtomicUsize,
    free: [AtomicUsize; NUM_CLASSES],
}

unsafe impl Sync for Heap {}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicUsize = AtomicUsize::new(0);

static HEAP: Heap = Heap {
    lock: AtomicBool::new(false),
    base: AtomicUsize::new(0),
    bump: AtomicUsize::new(0),
    free: [ZERO; NUM_CLASSES],
};

struct LockGuard;

impl Drop for LockGuard {
    fn drop(&mut self) {
        HEAP.lock.store(false, Ordering::Release);
    }
}

fn lock() -> LockGuard {
    while HEAP
        .lock
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    LockGuard
}

#[inline]
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[inline]
fn class_index(size: usize) -> Option<usize> {
    CLASS_SIZES.iter().position(|&class| class >= size.max(1))
}

fn mmap_rw(len: usize) -> *mut u8 {
    let len = align_up(len.max(1), USER_PAGE_SIZE);
    atom_syscall::vm::mmap_anon(len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE)
        .map(|addr| addr as *mut u8)
        .unwrap_or(null_mut())
}

impl Heap {
    fn ensure_pool(&self) -> usize {
        let existing = self.base.load(Ordering::Relaxed);
        if existing != 0 {
            return existing;
        }
        let ptr = mmap_rw(POOL_SIZE);
        if ptr.is_null() {
            return 0;
        }
        let base = ptr as usize;
        self.base.store(base, Ordering::Relaxed);
        self.bump.store(base, Ordering::Relaxed);
        base
    }

    fn pop_free(&self, class: usize) -> usize {
        let head = self.free[class].load(Ordering::Relaxed);
        if head == 0 {
            return 0;
        }
        let next = unsafe { *(head as *const usize) };
        self.free[class].store(next, Ordering::Relaxed);
        head
    }

    fn push_free(&self, class: usize, block: usize) {
        let head = self.free[class].load(Ordering::Relaxed);
        unsafe { *(block as *mut usize) = head };
        self.free[class].store(block, Ordering::Relaxed);
    }

    fn carve(&self, class: usize) -> usize {
        let base = self.ensure_pool();
        if base == 0 {
            return 0;
        }
        let start = align_up(self.bump.load(Ordering::Relaxed), POOL_ALIGN);
        let Some(end) = start.checked_add(CLASS_SIZES[class]) else {
            return 0;
        };
        if end > base + POOL_SIZE {
            return 0;
        }
        self.bump.store(end, Ordering::Relaxed);
        start
    }
}

pub struct ReusableAllocator;

unsafe impl GlobalAlloc for ReusableAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();
        if size == 0 {
            return align.max(1) as *mut u8;
        }
        if align > POOL_ALIGN || size > MAX_CLASS {
            return mmap_rw(size);
        }

        let Some(class) = class_index(size) else {
            return null_mut();
        };
        let _guard = lock();
        let reused = HEAP.pop_free(class);
        let block = if reused != 0 {
            reused
        } else {
            HEAP.carve(class)
        };
        block as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        let align = layout.align();
        if ptr.is_null() || size == 0 {
            return;
        }
        if align > POOL_ALIGN || size > MAX_CLASS {
            let _ = atom_syscall::vm::munmap(ptr as u64, align_up(size, USER_PAGE_SIZE));
            return;
        }

        let Some(class) = class_index(size) else {
            return;
        };
        let _guard = lock();
        HEAP.push_free(class, ptr as usize);
    }
}

#[global_allocator]
pub static ALLOCATOR: ReusableAllocator = ReusableAllocator;
