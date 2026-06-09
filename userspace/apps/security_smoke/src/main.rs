//! security_smoke — adversarial least-privilege smoke test (PR3)
//!
//! This is an *ordinary* application: it is launched by path via
//! `app_launcher` + `SYS_SPAWN_FROM_PATH`, so under the PR3 least-privilege
//! model it must receive ZERO sensitive capabilities. It then attempts a set of
//! sensitive syscalls and asserts that every one is denied with `EPERM`.
//!
//! The syscalls checked here are all gated by the *policy table* in
//! `kernel/src/syscall/policy.rs`, which runs BEFORE the handler — so the
//! `EPERM` is deterministic regardless of argument validity. That makes this a
//! reliable runtime proof that a common process cannot:
//!   * obtain or map the framebuffer (FramebufferMap),
//!   * change the video mode (DisplayModeSet),
//!   * read keyboard/mouse input (InputDevice),
//!   * read the kernel log (ReadKernelLog),
//!   * reach the raw filesystem backend (FsNamespace),
//!   * bind a reserved IPC port (ReservedPort).
//!
//! Raw PCI/MMIO/IRQ/DMA are additionally gated in-handler by Device/Irq
//! capability ownership (an app holds none), and are covered by the kernel
//! manifest unit tests; they are intentionally not asserted here because their
//! handler-level error code depends on argument order.
//!
//! Result is reported on the kernel serial log:
//!   "security_smoke: PASS" — every sensitive syscall was denied.
//!   "security_smoke: FAIL ..." — at least one was NOT denied (a regression).
//!
//! Run manually after `./build.sh --run` by launching
//! `/apps/user/security_smoke.atxf` (e.g. from the file manager) and checking
//! the serial log.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::debug::log;
use atom_syscall::error::EPERM;
use atom_syscall::raw::numbers::*;
use atom_syscall::raw::{syscall1, syscall2, syscall4};
use atom_syscall::thread::exit;

// ============================================================================
// Minimal global allocator. This app never allocates, but the userspace target
// links `alloc` (via atom_syscall), so a global allocator must be present.
// ============================================================================

const HEAP_SIZE: usize = 4 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(8);
        let cur = self.next.load(Ordering::Relaxed);
        let aligned = (cur + align - 1) & !(align - 1);
        let end = aligned + layout.size();
        if end > HEAP_SIZE {
            return core::ptr::null_mut();
        }
        self.next.store(end, Ordering::Relaxed);
        (self.heap.get() as *mut u8).add(aligned)
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: UnsafeCell::new([0u8; HEAP_SIZE]),
    next: AtomicUsize::new(0),
};

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    log("security_smoke: alloc error");
    loop {
        core::hint::spin_loop();
    }
}

/// A scratch buffer so syscalls that take a user pointer get a valid (but
/// irrelevant) address; the policy gate denies them before the pointer is used.
static mut SCRATCH: [u8; 64] = [0u8; 64];

fn scratch_ptr() -> u64 {
    core::ptr::addr_of_mut!(SCRATCH) as u64
}

/// Check that `ret == EPERM`; log the outcome and return 1 on failure.
fn expect_denied(name: &str, ret: u64) -> u32 {
    if ret == EPERM {
        log("security_smoke: DENIED (ok) ->");
        log(name);
        0
    } else {
        log("security_smoke: NOT DENIED (FAIL) ->");
        log(name);
        1
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    log("security_smoke: start (ordinary app — every sensitive syscall must be EPERM)");

    let p = scratch_ptr();
    let mut fails: u32 = 0;

    // Framebuffer map authority.
    fails += expect_denied("SYS_GET_FRAMEBUFFER", unsafe { syscall1(SYS_GET_FRAMEBUFFER, p) });
    fails += expect_denied("SYS_MAP_FRAMEBUFFER", unsafe { syscall1(SYS_MAP_FRAMEBUFFER, p) });

    // Video-mode change authority (queries are public and intentionally not here).
    fails += expect_denied("SYS_SET_VIDEO_MODE", unsafe { syscall2(SYS_SET_VIDEO_MODE, 0, 0) });

    // Input authority.
    fails += expect_denied("SYS_KEYBOARD_POLL", unsafe { syscall1(SYS_KEYBOARD_POLL, p) });
    fails += expect_denied("SYS_MOUSE_POLL", unsafe { syscall1(SYS_MOUSE_POLL, p) });

    // Kernel log.
    fails += expect_denied("SYS_READ_KLOG", unsafe { syscall2(SYS_READ_KLOG, p, 16) });

    // Raw filesystem backend (fsd-only).
    fails += expect_denied(
        "SYS_KERN_FS_READ_FILE",
        unsafe { syscall4(SYS_KERN_FS_READ_FILE, p, 1, p, 1) },
    );
    fails += expect_denied(
        "SYS_KERN_FS_WRITE_FILE",
        unsafe { syscall4(SYS_KERN_FS_WRITE_FILE, p, 1, p, 1) },
    );

    // Reserved bootstrap ports — an app must not be able to squat Port(1/2/3).
    fails += expect_denied("SYS_IPC_CREATE_PORT_WITH_ID(1)", unsafe {
        syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 1)
    });
    fails += expect_denied("SYS_IPC_CREATE_PORT_WITH_ID(3)", unsafe {
        syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 3)
    });

    if fails == 0 {
        log("security_smoke: PASS — all sensitive syscalls denied for ordinary app");
        exit(0);
    } else {
        log("security_smoke: FAIL — a sensitive syscall was NOT denied (least-privilege regression)");
        exit(1);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("security_smoke: PANIC");
    loop {
        core::hint::spin_loop();
    }
}
