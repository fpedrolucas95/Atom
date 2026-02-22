//! hello_atxf — ATXF runtime-loader smoke-test application
//!
//! This is a minimal "Hello, World!" style application used to verify that
//! the ATXF runtime loader (via `app_launcher` + `SYS_SPAWN_FROM_PATH`) can
//! execute arbitrary ATXF binaries placed on the filesystem after boot.
//!
//! # What it does
//!
//! 1. Prints a greeting to the kernel serial log so that QEMU's serial output
//!    can be checked to confirm the app actually ran.
//! 2. Sleeps briefly to give the operator time to observe the log.
//! 3. Exits cleanly.
//!
//! # Smoke-test procedure (manual)
//!
//! After a successful build:
//!   1. `hello_atxf.atxf` is placed in `efi/drivers/` (boot image) and also
//!      optionally copied to `efi/apps/` for a runtime-loader test.
//!   2. Boot QEMU with `./build.sh --run`.
//!   3. In the file manager, navigate to `/apps/hello_atxf.atxf` and
//!      double-click it.
//!   4. Check QEMU's serial output for the greeting lines below.
//!
//! # Automated smoke-test
//!
//! The `build.sh` script includes an `--smoke-test` mode that launches QEMU
//! in snapshot mode, waits for the greeting line in the serial log, and exits
//! with code 0 on success.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::debug::log;
use atom_syscall::thread::{exit, sleep_ms};

// ============================================================================
// Minimal bump allocator — 32 KB is ample for this simple app
// ============================================================================

const HEAP_SIZE: usize = 32 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0u8; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let end = aligned + size;
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(cur, end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    loop {}
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    log("hello_atxf: PANIC");
    loop {}
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    // These lines are the observable proof that the app was loaded and ran.
    // The smoke-test script searches for "HELLO_ATXF_OK" in the serial log.
    log("hello_atxf: *** HELLO_ATXF_OK — runtime loader works! ***");
    log("hello_atxf: This application was loaded from the filesystem at runtime.");
    log("hello_atxf: It was not registered at boot — the ATXF loader found it by path.");

    // Give QEMU time for the serial output to flush before we exit.
    sleep_ms(500);

    log("hello_atxf: exiting cleanly");
    exit(0);
}
