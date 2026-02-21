// Kernel Preemptive Scheduler Stress Test
//
// Exercises the Atom preemptive scheduler under extreme load to surface
// latent corruption, starvation, and context-switch defects that only
// manifest under sustained concurrency.
//
// Design goals:
// - 128 kernel worker threads that continuously yield to the scheduler.
// - One High-priority monitor thread that samples liveness every second.
// - Stack canary validation performed each monitor interval.
// - Atomic-only instrumentation; no heap allocation after init.
// - Timer frequency boosted to 1 000 Hz for finer-grained preemption.
// - 5-minute (300 s) run by default, configurable via STRESS_TIMER_HZ /
//   TEST_DURATION_TICKS at compile time.
//
// Failure criteria (objective):
// - Stack canary corruption       → `CANARY_FAILURES > 0`  → FAIL
// - Thread stall (≥3 s silence)   → `STALL_FAILURES > 0`   → FAIL
// - Scheduler thread-count drift  → unexpected total count  → FAIL
// - Any kernel panic / exception  → system halt (observed externally)
//
// Activation:
// - Compile with `--features stress_test` (see kernel/Cargo.toml).
// - Invoked from `kmain` before `start_scheduling()`.
//
// Observable metrics (serial log at INFO level):
// - Per-second yield throughput per worker
// - Active thread count vs. expected
// - Cumulative failure count
// - Final PASS / FAIL verdict
//
// QEMU execution procedure:
// - See the repository README §"Stress Test" or the comment block at the
//   bottom of this file.
//
// Correctness and safety notes:
// - All shared state uses atomic operations; no spinlocks in the hot path.
// - The only `unsafe` blocks are direct canary memory reads (volatile,
//   bounded addresses computed from the allocator's return value).
// - Workers and monitor self-terminate cleanly via `terminate_entity`.
// - The idle thread reaps zombies, so no resource leak occurs post-test.
// - Timer frequency is restored to NORMAL_TIMER_HZ after the test.

#![allow(dead_code)]

use core::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::arch::read_cr3;
use crate::mm::{pmm, vm};
use crate::sched;
use crate::thread::{Thread, ThreadPriority, TerminationReason};
use crate::{log_debug, log_error, log_info, log_warn};

// ─────────────────────────────────────────────────────────────────────────────
// Public tuning knobs
// ─────────────────────────────────────────────────────────────────────────────

/// Number of concurrent worker threads to spawn (must be ≤ WORKER_COUNT).
pub const WORKER_COUNT: usize = 128;

/// Kernel stack size for each worker (pages).  2 pages = 8 KiB.
const WORKER_STACK_PAGES: usize = 2;

/// Kernel stack size for the monitor thread (pages).  4 pages = 16 KiB.
const MONITOR_STACK_PAGES: usize = 4;

/// APIC timer frequency during the stress test (Hz).
/// Higher value → more frequent preemption → more scheduler pressure.
const STRESS_TIMER_HZ: u32 = 1_000;

/// APIC timer frequency after the test completes.
const NORMAL_TIMER_HZ: u32 = 100;

/// Total duration of the stress test expressed in timer ticks.
/// At STRESS_TIMER_HZ = 1 000 Hz:  300 000 ticks = 300 s = 5 minutes.
const TEST_DURATION_TICKS: u64 = 300_000;

/// Monitor sampling interval in ticks (≈ 1 second at 1 000 Hz).
const MONITOR_INTERVAL_TICKS: u64 = 1_000;

/// Number of consecutive silent intervals before a worker is declared stalled.
const STALL_THRESHOLD: u64 = 3;

/// Stack canary value — must match `STACK_CANARY` in `thread.rs`.
const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;

const LOG_ORIGIN: &str = "stress";

// ─────────────────────────────────────────────────────────────────────────────
// Phase constants
// ─────────────────────────────────────────────────────────────────────────────

const PHASE_INIT: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_DONE: u8 = 2;

// ─────────────────────────────────────────────────────────────────────────────
// Global state — static arrays only, zero heap post-init
// ─────────────────────────────────────────────────────────────────────────────

static STRESS_PHASE: AtomicU8 = AtomicU8::new(PHASE_INIT);

/// Monotonically increasing yield counter per worker thread.
static YIELD_COUNTS: [AtomicU64; WORKER_COUNT] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; WORKER_COUNT]
};

/// Yield counts sampled in the previous monitor interval (for delta).
static PREV_YIELDS: [AtomicU64; WORKER_COUNT] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; WORKER_COUNT]
};

/// Consecutive silent-interval count per worker (reset on progress).
static STALL_STREAK: [AtomicU64; WORKER_COUNT] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; WORKER_COUNT]
};

/// Physical-base virtual address of each worker's stack bottom.
/// Stored during `run()` so the monitor can validate canaries directly.
static WORKER_STACK_BOTTOMS: [AtomicU64; WORKER_COUNT] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; WORKER_COUNT]
};

/// Self-assigned index claimed by each worker at startup.
static NEXT_WORKER_ID: AtomicUsize = AtomicUsize::new(0);

// ─────────────────────────────────────────────────────────────────────────────
// Aggregate failure counters (read at test end for the verdict)
// ─────────────────────────────────────────────────────────────────────────────

/// Total fault events (sum of all sub-counters).
static TOTAL_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Stack canary violations detected by the monitor.
static CANARY_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Worker stall detections (silent for ≥ STALL_THRESHOLD intervals).
static STALL_FAILURES: AtomicUsize = AtomicUsize::new(0);

/// Thread-count drift events (actual ≠ expected).
static COUNT_FAILURES: AtomicUsize = AtomicUsize::new(0);

// ─────────────────────────────────────────────────────────────────────────────
// Worker thread
// ─────────────────────────────────────────────────────────────────────────────

/// Entry point for every stress-worker thread.
///
/// Each instance:
/// 1. Atomically claims a stable slot index.
/// 2. Spins (yielding) until the monitor transitions to PHASE_RUNNING.
/// 3. Loops: increments its yield counter, then voluntarily yields to the
///    scheduler, until PHASE_DONE is observed.
/// 4. Self-terminates cleanly via `terminate_entity` + `yield_current`.
extern "C" fn stress_worker_entry() -> ! {
    // Claim a stable index before the first yield.
    let my_index = NEXT_WORKER_ID.fetch_add(1, Ordering::Relaxed);

    // Wait until the monitor is ready to begin the timed run.
    // We yield rather than spin-without-yield so the monitor thread
    // (High priority) gets CPU time to set PHASE_RUNNING.
    while STRESS_PHASE.load(Ordering::Acquire) == PHASE_INIT {
        sched::yield_current();
    }

    // Main stress loop: yield as fast as possible.
    loop {
        if STRESS_PHASE.load(Ordering::Relaxed) == PHASE_DONE {
            break;
        }

        if my_index < WORKER_COUNT {
            YIELD_COUNTS[my_index].fetch_add(1, Ordering::Relaxed);
        }

        sched::yield_current();
    }

    // Clean self-termination.
    if let Some(tid) = sched::current_thread() {
        crate::thread::terminate_entity(
            tid,
            TerminationReason::NormalExit { exit_code: 0 },
        );
    }

    // This yield context-switches away permanently (our state is Exited).
    sched::yield_current();

    // Unreachable — keep the type checker satisfied.
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Monitor thread
// ─────────────────────────────────────────────────────────────────────────────

/// Entry point for the single High-priority monitor thread.
///
/// Responsibilities:
/// - Signal PHASE_RUNNING to unblock workers.
/// - Sample per-worker yield counts every MONITOR_INTERVAL_TICKS ticks.
/// - Validate stack canaries for all workers on each interval.
/// - Detect thread-count drift.
/// - Emit per-second metrics (INFO when enabled, DEBUG-level detail).
/// - Signal PHASE_DONE after TEST_DURATION_TICKS and emit a final verdict.
extern "C" fn stress_monitor_entry() -> ! {
    // Record the start tick before announcing anything.
    let start_tick = crate::interrupts::get_ticks();
    let end_tick = start_tick.wrapping_add(TEST_DURATION_TICKS);

    // Expected total threads during the test:
    // idle (1) + monitor (1) + workers (WORKER_COUNT).
    // In practice other kernel threads (init, etc.) may be present;
    // we use ≥ WORKER_COUNT + 2 as the lower-bound sanity check.
    let min_expected_threads = WORKER_COUNT + 2;

    log_info!(
        LOG_ORIGIN,
        "=== STRESS TEST BEGIN: workers={} timer={}Hz duration={}s ===",
        WORKER_COUNT,
        STRESS_TIMER_HZ,
        TEST_DURATION_TICKS / MONITOR_INTERVAL_TICKS
    );

    // Unblock all workers.
    STRESS_PHASE.store(PHASE_RUNNING, Ordering::SeqCst);

    let mut next_sample = start_tick.wrapping_add(MONITOR_INTERVAL_TICKS);
    let mut elapsed_intervals: u64 = 0;

    loop {
        let now = crate::interrupts::get_ticks();

        // Yield until next sample window.
        if now < next_sample {
            sched::yield_current();
            continue;
        }

        elapsed_intervals += 1;
        let elapsed_s = elapsed_intervals * MONITOR_INTERVAL_TICKS / MONITOR_INTERVAL_TICKS;
        next_sample = now.wrapping_add(MONITOR_INTERVAL_TICKS);

        // ── Liveness sampling ──────────────────────────────────────────────

        let mut interval_yield_total: u64 = 0;
        let mut stalled_count: usize = 0;

        for i in 0..WORKER_COUNT {
            let current = YIELD_COUNTS[i].load(Ordering::Relaxed);
            let previous = PREV_YIELDS[i].load(Ordering::Relaxed);
            let delta = current.wrapping_sub(previous);

            if delta == 0 {
                let streak = STALL_STREAK[i].fetch_add(1, Ordering::Relaxed) + 1;
                if streak >= STALL_THRESHOLD {
                    stalled_count += 1;
                    STALL_FAILURES.fetch_add(1, Ordering::Relaxed);
                    TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
                    log_error!(
                        LOG_ORIGIN,
                        "[STALL] worker={} consecutive_silent_intervals={} total_yields={}",
                        i,
                        streak,
                        current
                    );
                }
            } else {
                STALL_STREAK[i].store(0, Ordering::Relaxed);
                interval_yield_total = interval_yield_total.wrapping_add(delta);
            }

            PREV_YIELDS[i].store(current, Ordering::Relaxed);
        }

        // ── Stack canary validation ────────────────────────────────────────
        // Read each worker's stack bottom directly (volatile read).
        // The address was stored in run() immediately after alloc_pages().

        for i in 0..WORKER_COUNT {
            let bottom = WORKER_STACK_BOTTOMS[i].load(Ordering::Relaxed);
            if bottom == 0 {
                continue;
            }
            let actual = unsafe { core::ptr::read_volatile(bottom as *const u64) };
            if actual != STACK_CANARY {
                CANARY_FAILURES.fetch_add(1, Ordering::Relaxed);
                TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
                log_error!(
                    LOG_ORIGIN,
                    "[CANARY_CORRUPT] worker={} addr={:#018X} expected={:#018X} actual={:#018X}",
                    i,
                    bottom,
                    STACK_CANARY,
                    actual
                );
            }
        }

        // ── Thread-count sanity ────────────────────────────────────────────

        let thread_stats = crate::thread::get_thread_stats();
        if thread_stats.total < min_expected_threads {
            COUNT_FAILURES.fetch_add(1, Ordering::Relaxed);
            TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
            log_error!(
                LOG_ORIGIN,
                "[THREAD_COUNT] total={} min_expected={} (ready={} running={} exited={})",
                thread_stats.total,
                min_expected_threads,
                thread_stats.ready,
                thread_stats.running,
                thread_stats.exited
            );
        }

        // ── Emit per-interval metrics ──────────────────────────────────────
        // Use log_debug! for high-frequency detail (only when Debug level is set).
        // Use log_info! for the one-line summary (always emitted).

        #[cfg(debug_assertions)]
        log_debug!(
            LOG_ORIGIN,
            "[t={:>4}s] per_worker_yields/s={} stalls={} canary_fails={} count_fails={} threads(total={} ready={} run={} exit={})",
            elapsed_s,
            if WORKER_COUNT > 0 { interval_yield_total / WORKER_COUNT as u64 } else { 0 },
            stalled_count,
            CANARY_FAILURES.load(Ordering::Relaxed),
            COUNT_FAILURES.load(Ordering::Relaxed),
            thread_stats.total,
            thread_stats.ready,
            thread_stats.running,
            thread_stats.exited
        );

        log_info!(
            LOG_ORIGIN,
            "[t={:>4}s/{:>4}s] yields/s={} stalls={} failures={} threads={}",
            elapsed_s,
            TEST_DURATION_TICKS / MONITOR_INTERVAL_TICKS,
            interval_yield_total,
            stalled_count,
            TOTAL_FAILURES.load(Ordering::Relaxed),
            thread_stats.total
        );

        // ── Duration check ─────────────────────────────────────────────────

        if now >= end_tick {
            break;
        }
    }

    // Signal workers to terminate.
    STRESS_PHASE.store(PHASE_DONE, Ordering::SeqCst);

    // Brief pause for workers to observe PHASE_DONE and begin exiting.
    // We yield WORKER_COUNT times to give every worker one scheduling
    // opportunity.
    for _ in 0..WORKER_COUNT {
        sched::yield_current();
    }

    // Restore the APIC timer to the kernel's original frequency.
    crate::interrupts::init_timer(NORMAL_TIMER_HZ);

    // ── Final verdict ──────────────────────────────────────────────────────

    let total_f = TOTAL_FAILURES.load(Ordering::Relaxed);
    let canary_f = CANARY_FAILURES.load(Ordering::Relaxed);
    let stall_f = STALL_FAILURES.load(Ordering::Relaxed);
    let count_f = COUNT_FAILURES.load(Ordering::Relaxed);

    log_info!(LOG_ORIGIN, "=== STRESS TEST COMPLETE ===");
    log_info!(LOG_ORIGIN, "  Duration        : {}s", TEST_DURATION_TICKS / MONITOR_INTERVAL_TICKS);
    log_info!(LOG_ORIGIN, "  Workers         : {}", WORKER_COUNT);
    log_info!(LOG_ORIGIN, "  Timer           : {} Hz", STRESS_TIMER_HZ);
    log_info!(LOG_ORIGIN, "  Total failures  : {}", total_f);
    log_info!(LOG_ORIGIN, "  Canary failures : {}", canary_f);
    log_info!(LOG_ORIGIN, "  Stall failures  : {}", stall_f);
    log_info!(LOG_ORIGIN, "  Count failures  : {}", count_f);

    if total_f == 0 {
        log_info!(LOG_ORIGIN, "  RESULT: PASS — no latent corruption detected after {}s", TEST_DURATION_TICKS / MONITOR_INTERVAL_TICKS);
    } else {
        log_error!(
            LOG_ORIGIN,
            "  RESULT: FAIL — {} fault(s) detected (canary={} stall={} count={})",
            total_f, canary_f, stall_f, count_f
        );
    }

    log_info!(LOG_ORIGIN, "============================");

    // Resource counters for cross-check.
    crate::thread::RESOURCE_COUNTERS.log_stats();

    // Self-terminate and yield away permanently.
    if let Some(tid) = sched::current_thread() {
        crate::thread::terminate_entity(
            tid,
            TerminationReason::NormalExit { exit_code: 0 },
        );
    }
    sched::yield_current();

    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Initialise and register all stress-test threads with the scheduler.
///
/// Must be called **after** all kernel subsystems are initialised and
/// **before** `start_scheduling()`.  The threads are immediately placed in
/// the Ready state but block on `STRESS_PHASE` until the monitor releases them.
///
/// Physical memory cost:
///   WORKER_COUNT × WORKER_STACK_PAGES + MONITOR_STACK_PAGES pages.
///   At 128 workers × 2 pages + 4 pages = 260 pages ≈ 1 MiB.
pub fn run() {
    log_info!(
        LOG_ORIGIN,
        "Initialising stress test: workers={} stack_per_worker={}KiB duration={}s timer={}Hz",
        WORKER_COUNT,
        WORKER_STACK_PAGES * pmm::PAGE_SIZE / 1024,
        TEST_DURATION_TICKS / MONITOR_INTERVAL_TICKS,
        STRESS_TIMER_HZ
    );

    // Boost the APIC timer frequency for finer-grained preemption.
    crate::interrupts::init_timer(STRESS_TIMER_HZ);
    log_info!(LOG_ORIGIN, "APIC timer boosted to {} Hz", STRESS_TIMER_HZ);

    let cr3 = read_cr3();
    let mut spawned: usize = 0;

    // ── Spawn worker threads ───────────────────────────────────────────────

    for i in 0..WORKER_COUNT {
        let stack_phys = match pmm::alloc_pages(WORKER_STACK_PAGES) {
            Some(p) => p,
            None => {
                log_warn!(
                    LOG_ORIGIN,
                    "OOM: cannot allocate stack for worker {} — spawned {} of {}",
                    i, spawned, WORKER_COUNT
                );
                TOTAL_FAILURES.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        let stack_size = WORKER_STACK_PAGES * pmm::PAGE_SIZE;
        let stack_bottom_virt = vm::HIGHER_HALF_BASE + stack_phys;
        let stack_top_virt = stack_bottom_virt + stack_size;

        // Record bottom address so the monitor can validate the canary later.
        WORKER_STACK_BOTTOMS[i].store(stack_bottom_virt as u64, Ordering::Relaxed);

        let thread = Thread::new(
            stress_worker_entry as *const () as u64,
            stack_top_virt as u64,
            stack_size,
            cr3,
            ThreadPriority::Normal,
            "stress-w",
        );

        sched::add_thread(thread);
        spawned += 1;

        log_debug!(
            LOG_ORIGIN,
            "worker[{:03}] spawned stack_bottom={:#018X} stack_top={:#018X}",
            i, stack_bottom_virt, stack_top_virt
        );
    }

    // ── Spawn monitor thread ───────────────────────────────────────────────

    let mon_phys = pmm::alloc_pages(MONITOR_STACK_PAGES)
        .expect("stress_test: OOM allocating monitor stack");

    let mon_stack_size = MONITOR_STACK_PAGES * pmm::PAGE_SIZE;
    let mon_stack_bottom = vm::HIGHER_HALF_BASE + mon_phys;
    let mon_stack_top = mon_stack_bottom + mon_stack_size;

    let monitor = Thread::new(
        stress_monitor_entry as *const () as u64,
        mon_stack_top as u64,
        mon_stack_size,
        cr3,
        ThreadPriority::High,  // High priority → runs before workers
        "stress-m",
    );

    sched::add_thread(monitor);

    log_info!(
        LOG_ORIGIN,
        "Threads registered: {} workers + 1 monitor (total spawned={})",
        spawned, spawned + 1
    );
    log_info!(
        LOG_ORIGIN,
        "Stress test armed — scheduler will start the run."
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// QEMU Execution Procedure
// ─────────────────────────────────────────────────────────────────────────────
//
// 1. Build with the stress_test feature enabled:
//
//      cargo build -p atom-kernel --release --features stress_test
//
//    Or invoke the build script with the extra flag:
//
//      RUSTFLAGS="" cargo build -p atom-kernel --release \
//          --features stress_test 2>&1 | tee build/cargo_stress.log
//
//    Then assemble + link as usual:
//
//      nasm -f win64 arch/x86_64/boot.asm -o build/boot.obj
//      nasm -f win64 -i kernel/src/interrupts/ \
//          kernel/src/interrupts/handlers.asm -o build/handlers.obj
//      nasm -f win64 kernel/src/interrupts/switch.asm -o build/switch.obj
//      nasm -f win64 kernel/src/syscall/handler.asm -o build/syscall_handler.obj
//      rust-lld -flavor link \
//          build/boot.obj build/handlers.obj build/switch.obj \
//          build/syscall_handler.obj \
//          target/x86_64-unknown-uefi/release/libatom.a \
//          /OUT:build/Atom.efi /SUBSYSTEM:EFI_APPLICATION \
//          /ENTRY:efi_entry /NODEFAULTLIB
//      cp build/Atom.efi efi/EFI/BOOT/BOOTX64.EFI
//
// 2. Launch QEMU with serial logging to a file for post-mortem analysis:
//
//      qemu-system-x86_64                         \
//        -machine q35                              \
//        -cpu qemu64                               \
//        -m 512M                                   \
//        -bios ovmf/OVMF.fd                        \
//        -drive format=raw,file=fat:rw:efi         \
//        -device VGA                               \
//        -serial file:stress_serial.log            \
//        -debugcon file:stress_debugcon.log        \
//        -global isa-debugcon.iobase=0xE9          \
//        -no-reboot                                \
//        -no-shutdown
//
//    The `-no-reboot -no-shutdown` flags keep QEMU alive on a kernel panic
//    so that the full log is preserved.
//
// 3. Monitor progress in real time:
//
//      tail -f stress_serial.log | grep -E 'stress|FAIL|CANARY|STALL|PASS'
//
// 4. Expected output (passing run):
//
//      [INFO] [stress] === STRESS TEST BEGIN: workers=128 timer=1000Hz duration=300s ===
//      [INFO] [stress] [t=   1s/ 300s] yields/s=<N> stalls=0 failures=0 threads=<T>
//      [INFO] [stress] [t=   2s/ 300s] ...
//      ...
//      [INFO] [stress] === STRESS TEST COMPLETE ===
//      [INFO] [stress]   RESULT: PASS — no latent corruption detected after 300s
//
// 5. Objective failure criteria:
//
//    | Condition                          | Indicator                       |
//    |------------------------------------|----------------------------------|
//    | Stack overflow / smash             | [CANARY_CORRUPT] in log         |
//    | Thread starvation                  | [STALL] in log                  |
//    | Scheduler thread-count invariant   | [THREAD_COUNT] in log           |
//    | Kernel panic / exception           | PANIC / exception vector in log |
//    | Deadlock (system freezes)          | No log output for > 10 s        |
//
//    Any of the above constitutes a hard FAIL.  The final verdict line
//    emits either "RESULT: PASS" or "RESULT: FAIL" for unambiguous parsing.
