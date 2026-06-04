// Display Driver - Userspace Framebuffer Manager
//
// This is a userspace driver that manages the system framebuffer and provides
// a compositing service for other applications. It runs entirely in Ring 3
// and communicates with the kernel via syscalls.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::graphics::Framebuffer;
use atom_syscall::ipc::create_port;
use atom_syscall::thread::exit;
use atom_syscall::debug::log;

use libipc::protocol::register_service;

// ============================================================================
// Simple Bump Allocator for Userspace
// ============================================================================

const HEAP_SIZE: usize = 64 * 1024; // 64 KB heap

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self { heap: UnsafeCell::new([0; HEAP_SIZE]), next: AtomicUsize::new(0) }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + layout.size();
            if new_next > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(current, new_next, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! { loop {} }

// ============================================================================
// Main Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("Display Driver: Starting...");

    // Keep port in scope so the idle loop can block on it instead of spinning.
    let port = create_port().ok();
    if let Some(p) = port {
        let _ = register_service("display", p);
    }

    // Acquire framebuffer from kernel
    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("Display Driver: Failed to acquire framebuffer");
            exit(1);
        }
    };

    let _fb = fb;
    log("Display Driver: Framebuffer acquired");

    log("Display Driver: Ready");

    // Block on the IPC port (infinite wait) so this thread does not spin.
    // Any incoming message (e.g. from a future compositor) will wake it up.
    // If port creation failed fall back to an indefinite sleep.
    loop {
        if let Some(p) = port {
            atom_syscall::ipc::wait_any(&[p], u64::MAX).ok();
        } else {
            atom_syscall::thread::sleep_ms(u64::MAX);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Display Driver: PANIC!");
    exit(0xFF);
}
