//! Reusable, `mmap`-backed heap allocator for the filesystem daemon.
//!
//! This replaces the previous *irreversible* bump allocator. The bump
//! allocator never reclaimed freed memory (except for a LIFO fast-path), so a
//! long-lived `fsd` serving a flood of requests grew monotonically until the
//! 1 MiB static heap was exhausted and every subsequent allocation failed
//! forever — an irreversible denial of service.
//!
//! ## Design
//!
//! A **segregated free-list (size-class) allocator** carved from a single
//! anonymous region obtained from the kernel via `mmap`. Properties required by
//! PR5:
//!
//! * **Reuses freed memory** — each size class keeps an intrusive LIFO free
//!   list. `dealloc` pushes a block back; the next same-class `alloc` pops it.
//!   Repeated request/response cycles therefore reuse the same blocks instead
//!   of leaking monotonically.
//! * **`no_std` userspace compatible** — no external crates, only `core` and
//!   the `atom_syscall` mmap wrappers.
//! * **Backed by the kernel** — the pool and every oversized block come from
//!   `SYS_MMAP` (`mmap_anon`). Nothing is statically reserved in `.bss`.
//! * **W^X preserving** — every mapping is `PROT_READ | PROT_WRITE`, never
//!   `PROT_EXEC`. The heap can never become executable.
//! * **Controlled OOM** — on exhaustion `alloc` returns null. It never panics
//!   and never spins. The global `alloc_error` handler (in `main.rs`) turns a
//!   null into a clean process exit so `service_manager` can restart `fsd`.
//! * **Bounded** — the pool is a fixed [`POOL_SIZE`]; the per-class block sizes
//!   are fixed; oversized allocations are individually `mmap`/`munmap`-ed so
//!   they never permanently consume the pool.
//!
//! Allocations larger than the largest size class, or requiring an alignment
//! greater than [`POOL_ALIGN`], are served by a dedicated page-granular `mmap`
//! and returned to the kernel with `munmap` on `dealloc`. This keeps large
//! transient buffers (e.g. file images on write) from permanently fragmenting
//! the small-object pool, and guarantees they are fully reclaimed.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use atom_abi::{MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE, USER_PAGE_SIZE};

// ── Bounded heap limits ─────────────────────────────────────────────────────

/// Total size of the small-object pool obtained from the kernel via `mmap`.
/// Fixed and bounded — `fsd` can never grow its pool beyond this.
pub const POOL_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Maximum alignment the pool serves directly. Requests needing a stricter
/// alignment (e.g. page-aligned) fall through to a dedicated `mmap`.
const POOL_ALIGN: usize = 16;

/// Size-class payloads served from the pool. Anything larger goes to `mmap`.
const CLASS_SIZES: [usize; 9] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const NUM_CLASSES: usize = CLASS_SIZES.len();
const MAX_CLASS: usize = CLASS_SIZES[NUM_CLASSES - 1];

// ── Allocator state ─────────────────────────────────────────────────────────

/// One intrusive LIFO free list per size class. The head is the address of the
/// most-recently-freed block; each free block stores the next pointer in its
/// first 8 bytes (every class is ≥ 16 bytes, so this always fits).
struct Heap {
    /// Spin lock. `fsd` is single-threaded so this never actually contends, but
    /// `GlobalAlloc` must be `Sync`, and a never-contended lock keeps the
    /// implementation sound even if that assumption ever changes.
    lock: AtomicBool,
    /// Base of the pool region (lazily `mmap`-ed). 0 until initialised.
    base: AtomicUsize,
    /// Bump cursor for carving fresh blocks out of the pool.
    bump: AtomicUsize,
    /// Per-class free-list heads (0 = empty).
    free: [AtomicUsize; NUM_CLASSES],
    /// Diagnostics: live bytes carved into the pool (never decreases; tracks
    /// pool high-water carving, not live set).
    carved: AtomicUsize,
    /// Diagnostics: bytes currently mapped via the oversized `mmap` path.
    oversized: AtomicUsize,
}

unsafe impl Sync for Heap {}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicUsize = AtomicUsize::new(0);

static HEAP: Heap = Heap {
    lock: AtomicBool::new(false),
    base: AtomicUsize::new(0),
    bump: AtomicUsize::new(0),
    free: [ZERO; NUM_CLASSES],
    carved: AtomicUsize::new(0),
    oversized: AtomicUsize::new(0),
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

/// Smallest size-class index whose payload fits `size`, or `None` if `size`
/// exceeds the largest class.
#[inline]
fn class_index(size: usize) -> Option<usize> {
    CLASS_SIZES.iter().position(|&c| c >= size.max(1))
}

/// Map a fresh anonymous, read/write (never executable) region from the kernel.
fn mmap_rw(len: usize) -> *mut u8 {
    let len = align_up(len.max(1), USER_PAGE_SIZE);
    match atom_syscall::vm::mmap_anon(len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE) {
        Ok(addr) => addr as *mut u8,
        Err(_) => null_mut(),
    }
}

impl Heap {
    /// Ensure the pool is mapped. Must be called with the lock held. Returns the
    /// pool base, or 0 on failure.
    fn ensure_pool(&self) -> usize {
        let base = self.base.load(Ordering::Relaxed);
        if base != 0 {
            return base;
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

    /// Pop a block from the free list for `class`, or 0 if empty. Lock held.
    fn pop_free(&self, class: usize) -> usize {
        let head = self.free[class].load(Ordering::Relaxed);
        if head == 0 {
            return 0;
        }
        // SAFETY: a free block stores its successor in its first 8 bytes.
        let next = unsafe { *(head as *const usize) };
        self.free[class].store(next, Ordering::Relaxed);
        head
    }

    /// Push `block` onto the free list for `class`. Lock held.
    fn push_free(&self, class: usize, block: usize) {
        let head = self.free[class].load(Ordering::Relaxed);
        // SAFETY: `block` is at least 16 bytes, room for the next pointer.
        unsafe { *(block as *mut usize) = head };
        self.free[class].store(block, Ordering::Relaxed);
    }

    /// Carve a fresh `class` block from the bump region, or 0 on exhaustion.
    /// Lock held.
    fn carve(&self, class: usize) -> usize {
        let base = self.ensure_pool();
        if base == 0 {
            return 0;
        }
        let cs = CLASS_SIZES[class];
        let cur = self.bump.load(Ordering::Relaxed);
        let start = align_up(cur, POOL_ALIGN);
        let end = match start.checked_add(cs) {
            Some(e) => e,
            None => return 0,
        };
        if end > base + POOL_SIZE {
            return 0; // pool exhausted — controlled OOM
        }
        self.bump.store(end, Ordering::Relaxed);
        self.carved.fetch_add(cs, Ordering::Relaxed);
        start
    }
}

pub struct ReusableAllocator;

unsafe impl GlobalAlloc for ReusableAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align();

        // Zero-sized allocations: hand back a non-null, suitably aligned dangling
        // pointer. `dealloc` ignores these (size == 0).
        if size == 0 {
            return align.max(1) as *mut u8;
        }

        // Oversized or over-aligned requests get their own mapping so they are
        // fully reclaimable and never fragment the small-object pool.
        if align > POOL_ALIGN || size > MAX_CLASS {
            let ptr = mmap_rw(size);
            if !ptr.is_null() {
                HEAP.oversized
                    .fetch_add(align_up(size, USER_PAGE_SIZE), Ordering::Relaxed);
            }
            return ptr;
        }

        let class = match class_index(size) {
            Some(c) => c,
            None => return null_mut(),
        };

        let _g = lock();
        let block = {
            let reused = HEAP.pop_free(class);
            if reused != 0 {
                reused
            } else {
                HEAP.carve(class)
            }
        };
        if block == 0 {
            return null_mut(); // controlled OOM
        }
        block as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size();
        let align = layout.align();

        if size == 0 || ptr.is_null() {
            return;
        }

        if align > POOL_ALIGN || size > MAX_CLASS {
            let len = align_up(size, USER_PAGE_SIZE);
            let _ = atom_syscall::vm::munmap(ptr as u64, len);
            HEAP.oversized.fetch_sub(len, Ordering::Relaxed);
            return;
        }

        let class = match class_index(size) {
            Some(c) => c,
            None => return,
        };

        let _g = lock();
        HEAP.push_free(class, ptr as usize);
    }
}

#[global_allocator]
pub static ALLOCATOR: ReusableAllocator = ReusableAllocator;

/// Snapshot of pool usage for diagnostics / smoke tests.
pub struct HeapStats {
    pub pool_size: usize,
    pub carved: usize,
    pub oversized: usize,
}

/// Return a coarse snapshot of heap usage. Cheap and lock-free.
pub fn stats() -> HeapStats {
    HeapStats {
        pool_size: POOL_SIZE,
        carved: HEAP.carved.load(Ordering::Relaxed),
        oversized: HEAP.oversized.load(Ordering::Relaxed),
    }
}
