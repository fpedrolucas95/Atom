#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

// Simple bump allocator for userspace
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

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

use core::panic::PanicInfo;
use atom_syscall::process::spawn_process;
use atom_syscall::thread::{sleep_ms, exit};
use atom_syscall::debug::log;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("INIT: Starting microkernel bootstrap...");

    // 1. Start core services
    log("INIT: Spawning namesvc...");
    if let Err(_) = spawn_process("namesvc") {
        log("INIT: FATAL - Failed to spawn namesvc");
        exit(1);
    }
    sleep_ms(100); // Give it some time to initialize

    log("INIT: Spawning service_manager...");
    if let Err(_) = spawn_process("service_manager") {
        log("INIT: FATAL - Failed to spawn service_manager");
        exit(1);
    }
    sleep_ms(100);

    // 2. Start drivers
    log("INIT: Spawning display driver...");
    let _ = spawn_process("display");

    log("INIT: Spawning keyboard driver...");
    let _ = spawn_process("keyboard");

    log("INIT: Spawning mouse driver...");
    let _ = spawn_process("mouse");

    sleep_ms(200);

    // 3. Start UI Shell
    log("INIT: Spawning ui_shell...");
    if let Err(_) = spawn_process("ui_shell") {
        log("INIT: FATAL - Failed to spawn ui_shell");
        exit(1);
    }

    log("INIT: Bootstrap complete. Init process entering idle loop.");

    loop {
        sleep_ms(1000);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("INIT: PANIC!");
    exit(0xFF);
}
