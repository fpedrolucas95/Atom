//! Init Process (PID 1)
//!
//! The init process is the first userspace process spawned by the kernel.
//! It orchestrates the entire boot sequence by spawning system services
//! and drivers in the correct order.
//!
//! ## Boot sequence
//!
//! 1. Spawn `namesvc` (service discovery)
//! 2. Spawn `service_manager` (lifecycle management)
//! 3. Wait for services to be ready
//! 4. Spawn `ui_shell` (compositor / window manager)
//! 5. Spawn userspace drivers: keyboard, mouse, display
//! 6. Spawn `terminal`
//! 7. Enter supervision loop
//!
//! ## Design
//!
//! Init does NOT do hardware access, UI rendering, or IPC routing.
//! It only spawns processes and monitors them. All policy decisions
//! (restart, dependencies, health) are delegated to service_manager.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

// Simple bump allocator for userspace
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 64 * 1024; // 64 KB heap

    #[repr(align(4096))]
    struct Heap {
        data: UnsafeCell<[u8; HEAP_SIZE]>,
        next: UnsafeCell<usize>,
    }

    unsafe impl Sync for Heap {}

    static HEAP: Heap = Heap {
        data: UnsafeCell::new([0; HEAP_SIZE]),
        next: UnsafeCell::new(0),
    };

    pub struct BumpAllocator;

    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let next = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;

            let align = layout.align();
            let size = layout.size();

            let current = *next;
            let aligned = (current + align - 1) & !(align - 1);

            if aligned + size > HEAP_SIZE {
                return null_mut();
            }

            *next = aligned + size;
            heap_start.add(aligned)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator doesn't free
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Logging helper
// ============================================================================

fn log(msg: &str) {
    atom_syscall::debug::log(msg);
}

// ============================================================================
// Process spawning with retry
// ============================================================================

/// Spawn a process by name, retrying a few times on failure
fn spawn_with_retry(name: &str, max_retries: u32) -> Result<u64, ()> {
    for attempt in 0..=max_retries {
        match atom_syscall::process::spawn_process(name) {
            Ok(pid) => return Ok(pid),
            Err(_) => {
                if attempt < max_retries {
                    // Wait before retrying
                    atom_syscall::thread::sleep_ms(50);
                }
            }
        }
    }
    Err(())
}

/// Spawn a process, log success or failure
fn spawn_service(name: &str) -> Option<u64> {
    log(name);
    match spawn_with_retry(name, 2) {
        Ok(pid) => {
            log("  -> spawned OK");
            Some(pid)
        }
        Err(_) => {
            log("  -> FAILED to spawn");
            None
        }
    }
}

// ============================================================================
// Boot sequence
// ============================================================================

fn wait_for_namesvc() {
    log("init: waiting for namesvc on Port(1)...");
    loop {
        // Try sending a raw async message to Port(1)
        // This will only succeed once Port(1) is created by namesvc
        if atom_syscall::ipc::send_async(1, &[]).is_ok() {
            log("init: namesvc Port(1) is ready");
            break;
        }
        atom_syscall::thread::yield_now();
    }
}

fn wait_for_service(name: &str) {
    log("init: waiting for service registration: ");
    log(name);
    loop {
        if libipc::protocol::lookup_service(name).is_ok() {
            log("  -> service is registered and ready");
            break;
        }
        atom_syscall::thread::sleep_ms(10);
    }
}

fn boot_sequence() {
    log("===========================================");
    log("Atom Init Process (PID 1)");
    log("===========================================");

    // -----------------------------------------------------------------------
    // Phase 1: Core system services
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 1] Spawning namesvc...");
    let _namesvc_pid = spawn_service("namesvc");

    // CRITICAL: Wait for namesvc to initialize Port(1) before anything else
    // This prevents other services from "stealing" Port(1).
    wait_for_namesvc();

    log("[Phase 1] Spawning service_manager...");
    let _svcmgr_pid = spawn_service("service_manager");

    log("[Phase 1] Core services ready");

    // -----------------------------------------------------------------------
    // Phase 2: UI shell (compositor)
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 2] Spawning UI shell (compositor)...");

    let _ui_pid = spawn_service("ui_shell");

    // CRITICAL: Wait for compositor to be registered before drivers
    // Input drivers depend on the compositor's IPC port.
    wait_for_service("compositor");

    log("[Phase 2] UI shell ready");

    // -----------------------------------------------------------------------
    // Phase 3: Userspace drivers
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 3] Spawning userspace drivers...");

    let _kbd_pid = spawn_service("keyboard");
    let _mouse_pid = spawn_service("mouse");
    let _display_pid = spawn_service("display");

    // Give drivers time to initialize
    atom_syscall::thread::sleep_ms(50);

    log("[Phase 3] Drivers ready");

    // -----------------------------------------------------------------------
    // Phase 4: Applications
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 4] Spawning applications...");

    let _terminal_pid = spawn_service("terminal");

    log("[Phase 4] Applications ready");

    // -----------------------------------------------------------------------
    // Done
    // -----------------------------------------------------------------------
    log("");
    log("===========================================");
    log("Init: Boot sequence complete");
    log("===========================================");
}

// ============================================================================
// Supervision loop
// ============================================================================

/// After boot, init enters a supervision loop.
/// In a full implementation, this would monitor child processes
/// and coordinate with service_manager for restarts.
fn supervision_loop() -> ! {
    loop {
        // Sleep and periodically check system health
        // The service_manager handles actual supervision;
        // init just stays alive as PID 1
        atom_syscall::thread::sleep_ms(5000);
    }
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    boot_sequence();
    supervision_loop();
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("init: PANIC! System cannot continue.");
    loop {
        atom_syscall::thread::sleep_ms(1000);
    }
}
