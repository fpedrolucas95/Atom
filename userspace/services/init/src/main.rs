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
//! 4. Spawn `ui_shell` (compositor / window manager with unified input)
//! 5. Spawn `display` driver
//! 6. Spawn `terminal`
//! 7. Enter supervision loop
//!
//! Note: Keyboard and mouse input are handled directly by ui_shell via kernel
//! buffer polling. Separate keyboard/mouse driver processes are not spawned.
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
    log("init: spawning '");
    log(name);
    log("'...");
    match spawn_with_retry(name, 2) {
        Ok(pid) => {
            log("init: '");
            log(name);
            log("' spawned OK");
            Some(pid)
        }
        Err(_) => {
            log("init: '");
            log(name);
            log("' FAILED to spawn after retries");
            None
        }
    }
}

// ============================================================================
// Boot sequence
// ============================================================================

fn wait_for_namesvc() -> bool {
    log("init: waiting for namesvc on Port(1)...");

    // Timeout: 10000 iterations with yield (should be very fast)
    for _ in 0..10000u32 {
        // Try sending a raw async message to Port(1)
        // This will only succeed once Port(1) is created by namesvc
        if atom_syscall::ipc::send_async(1, &[]).is_ok() {
            log("init: namesvc Port(1) is ready");
            return true;
        }
        atom_syscall::thread::yield_now();
    }

    log("init: WARNING - timed out waiting for namesvc Port(1)");
    false
}

/// Wait for a service to register, with a timeout to prevent blocking forever.
/// Returns true if the service was found, false if timed out.
fn wait_for_service(name: &str) -> bool {
    log("init: waiting for service '");
    log(name);
    log("'...");

    // Each lookup_service call polls namesvc for up to ~200ms internally.
    // Retry 100 times = up to ~20 seconds total before giving up.
    // Use yield_now between retries (not sleep_ms) to avoid any dependence
    // on the system clock tick rate being calibrated correctly at boot.
    const MAX_RETRIES: u32 = 100;

    for _ in 0..MAX_RETRIES {
        if libipc::protocol::lookup_service(name).is_ok() {
            log("init: service '");
            log(name);
            log("' is registered and ready");
            return true;
        }
        // Yield a few thousand times between retries — keeps init responsive
        // without relying on sleep_ms tick calibration.
        for _ in 0..5000u32 {
            atom_syscall::thread::yield_now();
        }
    }

    log("init: WARNING - timed out waiting for service '");
    log(name);
    log("', continuing boot sequence anyway");
    false
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
    let namesvc_ready = wait_for_namesvc();
    if !namesvc_ready {
        log("[Phase 1] WARNING: namesvc not ready, continuing anyway");
    }

    log("[Phase 1] Spawning service_manager...");
    let _svcmgr_pid = spawn_service("service_manager");

    // Spawn the app_launcher service so that any component that wants to
    // execute an ATXF binary by path (e.g. the file manager on double-click)
    // can do so via IPC without needing spawn capabilities themselves.
    // app_launcher uses the kernel FAT32 driver directly (SYS_SPAWN_FROM_PATH)
    // and therefore does NOT depend on fsd; it can be started early.
    log("[Phase 1] Spawning app_launcher...");
    let _app_launcher_pid = spawn_service("app_launcher");

    log("[Phase 1] Core services ready");

    // -----------------------------------------------------------------------
    // Phase 1.5: Filesystem service (CRITICAL)
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 1.5] Spawning filesystem daemon (fsd)...");
    
    let _fsd_pid = match spawn_service("fsd") {
        Some(pid) => {
            log("[Phase 1.5] FSD spawned with PID ");
            // Note: Can't directly log u64, would need custom formatter
            log("(check service_manager for details)");
            pid
        }
        None => {
            log("[Phase 1.5] CRITICAL ERROR: Failed to spawn fsd after retries");
            log("[Phase 1.5] System cannot boot without filesystem service");
            panic!("fsd spawn failed");
        }
    };

    // Wait for fsd to register with service_manager and become ready
    // Timeout: 5 seconds (handled by wait_for_service)
    let fsd_ready = wait_for_service("fsd");
    if !fsd_ready {
        log("[Phase 1.5] CRITICAL ERROR: fsd did not register within timeout");
        log("[Phase 1.5] Filesystem operations will fail");
        log("[Phase 1.5] Boot cannot continue without filesystem service");
        panic!("fsd readiness timeout");
    }

    log("[Phase 1.5] Filesystem service ready and operational");

    // -----------------------------------------------------------------------
    // Phase 2: Network services
    // Network has no dependency on the UI compositor, so it starts here.
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 2] Spawning networking services...");
    let _nic_pid = spawn_service("nic_driver");
    let _netd_pid = spawn_service("netd");
    // Give the NIC driver time to detect hardware and notify netd.
    atom_syscall::thread::sleep_ms(200);
    log("[Phase 2] Network services spawned");

    // -----------------------------------------------------------------------
    // Phase 3: UI shell (compositor)
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 3] Spawning UI shell (compositor)...");

    let _ui_pid = spawn_service("ui_shell");

    // Wait for compositor to be registered before display/input drivers.
    // Use a non-blocking check so we never stall boot indefinitely.
    let compositor_ready = wait_for_service("compositor");
    if !compositor_ready {
        log("[Phase 3] WARNING: Compositor not ready, drivers will retry lookup themselves");
    }

    log("[Phase 3] UI shell phase complete");

    // -----------------------------------------------------------------------
    // Phase 4: Display driver
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 4] Spawning display driver...");
    let _display_pid = spawn_service("display");
    // Give drivers time to initialize
    atom_syscall::thread::sleep_ms(50);

    log("[Phase 4] Drivers spawned");

    // -----------------------------------------------------------------------
    // Phase 5: Applications
    // -----------------------------------------------------------------------
    log("");
    log("[Phase 5] No applications configured for auto-start");

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
