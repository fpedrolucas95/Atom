//! security_smoke — adversarial milestone smoke test (PR1–PR5)
//!
//! This is an *ordinary* application: it is launched by path via
//! `app_launcher` + `SYS_SPAWN_FROM_PATH`, so under the PR3 least-privilege
//! model it must receive ZERO sensitive capabilities. It then attempts a set of
//! sensitive operations and asserts that every one is denied or fails in a
//! controlled way.
//!
//! ## Machine-readable output
//!
//! Each category prints exactly one line on the kernel serial log:
//!
//! ```text
//! SECURITY_SMOKE PASS spawn_denied
//! SECURITY_SMOKE PASS reserved_port_denied
//! SECURITY_SMOKE PASS namesvc_spoof_denied
//! SECURITY_SMOKE PASS framebuffer_denied
//! SECURITY_SMOKE PASS input_denied
//! SECURITY_SMOKE PASS hardware_denied
//! SECURITY_SMOKE PASS rwx_mmap_denied
//! SECURITY_SMOKE PASS rwx_mprotect_denied
//! SECURITY_SMOKE PASS shared_exec_denied
//! SECURITY_SMOKE PASS fsd_limits
//! SECURITY_SMOKE PASS all
//! ```
//!
//! The QEMU runner (`scripts/ci/qemu-smoke.sh`) parses this output and FAILS
//! the build unless it finds `SECURITY_SMOKE PASS all`. A regression that
//! re-opens any of these holes turns the corresponding line into
//! `SECURITY_SMOKE FAIL <tag>` and suppresses `PASS all`, failing CI.
//!
//! Most checks are gated by the *policy table* in `kernel/src/syscall/policy.rs`
//! which runs BEFORE the handler, so `EPERM` is deterministic regardless of
//! argument validity. The raw device syscalls (PCI/DMA) are gated in-handler by
//! capability ownership; for those we only require a controlled *error* (the
//! app holds no Device/Irq cap), not a specific code.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::atom_abi::is_syscall_error;
use atom_syscall::debug::log;
use atom_syscall::error::EPERM;
use atom_syscall::raw::numbers::*;
use atom_syscall::raw::{syscall1, syscall2, syscall3, syscall4, syscall5};
use atom_syscall::thread::exit;

// ============================================================================
// Minimal global allocator. This app barely allocates, but the userspace target
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

/// An over-long path used to prove `fsd` rejects oversized paths (PR5 limits).
/// Length 2048 > fsd's MAX_PATH_LEN (1024), so the request must be rejected
/// with a controlled error rather than driving an allocation or a hang.
static mut BIG_PATH: [u8; 2048] = [b'A'; 2048];

fn scratch_ptr() -> u64 {
    core::ptr::addr_of_mut!(SCRATCH) as u64
}

/// Check that `ret == EPERM`. Returns 1 on failure (NOT denied).
fn expect_denied(name: &str, ret: u64) -> u32 {
    if ret == EPERM {
        0
    } else {
        log("security_smoke: NOT DENIED (FAIL) ->");
        log(name);
        1
    }
}

/// Check that `ret` is any syscall error (controlled failure). Returns 1 on
/// failure (the call unexpectedly succeeded).
fn expect_error(name: &str, ret: u64) -> u32 {
    if is_syscall_error(ret) {
        0
    } else {
        log("security_smoke: UNEXPECTED SUCCESS (FAIL) ->");
        log(name);
        1
    }
}

/// Check that `ret` is NOT an error (operation legitimately allowed).
fn expect_success(name: &str, ret: u64) -> u32 {
    if !is_syscall_error(ret) {
        0
    } else {
        log("security_smoke: UNEXPECTED ERROR (FAIL) ->");
        log(name);
        1
    }
}

/// Emit the machine-readable result line for a category and fold its failures
/// into the running total.
fn report(pass_line: &str, fail_line: &str, fails: u32, total: &mut u32) {
    if fails == 0 {
        log(pass_line);
    } else {
        log(fail_line);
    }
    *total += fails;
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    log("security_smoke: start (ordinary app — every sensitive op must be denied)");

    let p = scratch_ptr();
    let mut total: u32 = 0;

    // ── PR1: spawn / kernel-log authority ──────────────────────────────────
    {
        let mut f = 0;
        f += expect_denied("SYS_SPAWN_PROCESS", unsafe {
            syscall2(SYS_SPAWN_PROCESS, p, 4)
        });
        f += expect_denied("SYS_SPAWN_FROM_PATH", unsafe {
            syscall2(SYS_SPAWN_FROM_PATH, p, 4)
        });
        f += expect_denied("SYS_READ_KLOG", unsafe { syscall2(SYS_READ_KLOG, p, 16) });
        report(
            "SECURITY_SMOKE PASS spawn_denied",
            "SECURITY_SMOKE FAIL spawn_denied",
            f,
            &mut total,
        );
    }

    // ── PR2: reserved bootstrap ports ──────────────────────────────────────
    {
        let mut f = 0;
        f += expect_denied("create_port(1)", unsafe {
            syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 1)
        });
        f += expect_denied("create_port(2)", unsafe {
            syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 2)
        });
        f += expect_denied("create_port(3)", unsafe {
            syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 3)
        });
        report(
            "SECURITY_SMOKE PASS reserved_port_denied",
            "SECURITY_SMOKE FAIL reserved_port_denied",
            f,
            &mut total,
        );
    }

    // ── PR2: namesvc spoofing ──────────────────────────────────────────────
    // An ordinary app cannot squat the well-known namesvc port (2), so it
    // cannot impersonate the name service and forge registrations. (Forged
    // *registration* messages are independently rejected by namesvc because the
    // app holds no ServiceIdentity; that path is covered by namesvc unit tests
    // and cannot be exercised here without risking a blocking IPC round-trip.)
    {
        let mut f = 0;
        f += expect_denied("namesvc_port_squat(2)", unsafe {
            syscall1(SYS_IPC_CREATE_PORT_WITH_ID, 2)
        });
        report(
            "SECURITY_SMOKE PASS namesvc_spoof_denied",
            "SECURITY_SMOKE FAIL namesvc_spoof_denied",
            f,
            &mut total,
        );
    }

    // ── PR3: framebuffer / video-mode authority ────────────────────────────
    {
        let mut f = 0;
        f += expect_denied("SYS_GET_FRAMEBUFFER", unsafe {
            syscall1(SYS_GET_FRAMEBUFFER, p)
        });
        f += expect_denied("SYS_MAP_FRAMEBUFFER", unsafe {
            syscall1(SYS_MAP_FRAMEBUFFER, p)
        });
        f += expect_denied("SYS_SET_VIDEO_MODE", unsafe {
            syscall2(SYS_SET_VIDEO_MODE, 0, 0)
        });
        report(
            "SECURITY_SMOKE PASS framebuffer_denied",
            "SECURITY_SMOKE FAIL framebuffer_denied",
            f,
            &mut total,
        );
    }

    // ── PR3: input authority ───────────────────────────────────────────────
    {
        let mut f = 0;
        f += expect_denied("SYS_KEYBOARD_POLL", unsafe {
            syscall1(SYS_KEYBOARD_POLL, p)
        });
        f += expect_denied("SYS_MOUSE_POLL", unsafe { syscall1(SYS_MOUSE_POLL, p) });
        report(
            "SECURITY_SMOKE PASS input_denied",
            "SECURITY_SMOKE FAIL input_denied",
            f,
            &mut total,
        );
    }

    // ── PR3: hardware / raw FS backend authority ───────────────────────────
    {
        let mut f = 0;
        // FS kernel backend: policy-gated, deterministic EPERM.
        f += expect_denied("SYS_KERN_FS_READ_FILE", unsafe {
            syscall4(SYS_KERN_FS_READ_FILE, p, 1, p, 1)
        });
        f += expect_denied("SYS_KERN_FS_WRITE_FILE", unsafe {
            syscall4(SYS_KERN_FS_WRITE_FILE, p, 1, p, 1)
        });
        // Raw PCI/DMA: handler-gated by Device/Irq cap ownership (app holds
        // none) — require a controlled error, not a specific code.
        f += expect_error("SYS_PCI_QUERY_DEVICE", unsafe {
            syscall2(SYS_PCI_QUERY_DEVICE, 0, p)
        });
        f += expect_error("SYS_DMA_ALLOC", unsafe { syscall2(SYS_DMA_ALLOC, p, p) });
        report(
            "SECURITY_SMOKE PASS hardware_denied",
            "SECURITY_SMOKE FAIL hardware_denied",
            f,
            &mut total,
        );
    }

    const PAGE: u64 = 4096;
    const MMAP_FLAGS: u64 =
        atom_syscall::atom_abi::MAP_ANONYMOUS | atom_syscall::atom_abi::MAP_PRIVATE;
    const R: u64 = atom_syscall::atom_abi::PROT_READ;
    const W: u64 = atom_syscall::atom_abi::PROT_WRITE;
    const X: u64 = atom_syscall::atom_abi::PROT_EXEC;

    // ── PR4: W^X on mmap ───────────────────────────────────────────────────
    {
        let mut f = 0;
        f += expect_denied("mmap(RWX)", unsafe {
            syscall4(SYS_MMAP, 0, PAGE, R | W | X, MMAP_FLAGS)
        });
        report(
            "SECURITY_SMOKE PASS rwx_mmap_denied",
            "SECURITY_SMOKE FAIL rwx_mmap_denied",
            f,
            &mut total,
        );
    }

    // ── PR4: W^X on mprotect ───────────────────────────────────────────────
    {
        let mut f = 0;
        let rw = unsafe { syscall4(SYS_MMAP, 0, PAGE, R | W, MMAP_FLAGS) };
        f += expect_success("mmap(RW)", rw);
        if !is_syscall_error(rw) {
            f += expect_denied("mprotect(RW -> RWX)", unsafe {
                syscall3(SYS_MPROTECT, rw, PAGE, R | W | X)
            });
            // Clean up; failure here does not affect the W^X assertion.
            let _ = unsafe { syscall3(SYS_MPROTECT, rw, PAGE, R) };
            let _ = unsafe { syscall2(SYS_MUNMAP, rw, PAGE) };
        }
        report(
            "SECURITY_SMOKE PASS rwx_mprotect_denied",
            "SECURITY_SMOKE FAIL rwx_mprotect_denied",
            f,
            &mut total,
        );
    }

    // ── PR4: executable shared memory / map_region W+X ─────────────────────
    {
        let mut f = 0;
        let shared = unsafe { syscall1(SYS_SHARED_REGION_CREATE, PAGE) };
        f += expect_success("shared_region_create", shared);
        if !is_syscall_error(shared) {
            f += expect_denied("shared_region_map(EXEC)", unsafe {
                syscall3(SYS_SHARED_REGION_MAP, shared, 0, 0x5)
            });
            let _ = unsafe { syscall1(SYS_SHARED_REGION_DESTROY, shared) };
        }
        f += expect_denied("map_region(WRITE|EXEC)", unsafe {
            syscall5(SYS_MAP_REGION, 0, 0x4000, 0x4000, PAGE, 0x2)
        });
        report(
            "SECURITY_SMOKE PASS shared_exec_denied",
            "SECURITY_SMOKE FAIL shared_exec_denied",
            f,
            &mut total,
        );
    }

    // ── PR5: fsd per-request limits ────────────────────────────────────────
    // Opening a path far longer than fsd's MAX_PATH_LEN must fail in a
    // controlled way (error reply, no hang, no unbounded allocation). The call
    // returns promptly, which also proves a client does not block indefinitely
    // on a rejected request.
    {
        let mut f = 0;
        // Make it look path-like; content is irrelevant past the length check.
        unsafe {
            (*core::ptr::addr_of_mut!(BIG_PATH))[0] = b'/';
        }
        let big = core::ptr::addr_of!(BIG_PATH) as u64;
        let big_len = 2048u64;
        f += expect_error("fs_open(oversized_path)", unsafe {
            syscall4(SYS_FS_OPEN, big, big_len, 0, 0)
        });
        report(
            "SECURITY_SMOKE PASS fsd_limits",
            "SECURITY_SMOKE FAIL fsd_limits",
            f,
            &mut total,
        );
    }

    // ── Final aggregate verdict (the runner greps for this) ────────────────
    if total == 0 {
        log("SECURITY_SMOKE PASS all");
        exit(0);
    } else {
        log("SECURITY_SMOKE FAIL all");
        exit(1);
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    log("security_smoke: PANIC");
    log("SECURITY_SMOKE FAIL all");
    loop {
        core::hint::spin_loop();
    }
}
