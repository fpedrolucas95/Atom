# Agent Memory — Atom

## Browser engine (userspace/apps/browser)

- Pipeline: `tokenizer.rs` (HTML5 tokenizer) → `domtree.rs` (tree construction,
  arena DOM) → `css.rs` + `style.rs` (selector matching, cascade, inheritance)
  → `html.rs` (flattener: styled DOM → `dom.rs` flat blocks) → `render.rs`.
- `entities.rs` holds named/numeric character references (ASCII transliteration
  — the bitmap font is ASCII-only).
- Host-side regression tests: `cd tools/browser_tests && cargo test`
  (stub libgui/libimage; overrides the repo's UEFI cargo target).
- DOM depth is capped (`domtree::MAX_DEPTH`) because the flattener recurses on
  tree depth and user stacks are 512 KiB; keep recursion bounded.

## Build and run

- Linux/macOS build+run: `./build.sh --run`
- SMP run: `./build.sh --run --smp=2` / `--smp=4`
- Windows SMP run: `./build.ps1 --run --Smp 2`

## SMP implementation landmarks

- SMP topology/boot/AP startup: `kernel/src/smp.rs` (`bringup_aps`, `smp_ap_entry`)
- AP trampoline binary source: `kernel/src/ap_trampoline.asm`
- Scheduler SMP core + affinity/work stealing: `kernel/src/sched.rs`
- Per-CPU GDT/TSS support: `kernel/src/arch/gdt.rs`
- Per-CPU IDT load path: `kernel/src/interrupts/idt.rs`
- Reschedule IPI plumbing: `kernel/src/interrupts/apic.rs`, `kernel/src/interrupts/handlers.asm`, `kernel/src/interrupts/handlers.rs`
- Syscall per-CPU kernel stack via GS: `kernel/src/syscall/handler.asm` + `smp::CPU_LOCAL_ASM_STATE`

## ABI additions

- `SYS_GET_CPU_ID = 90`
- `SYS_GET_CPU_COUNT = 91`
- `SYS_SET_THREAD_AFFINITY = 92`
- `SYS_GET_THREAD_AFFINITY = 93`

## Validation hints

Look for logs:

- MADT CPU detection + APIC mapping
- AP online confirmation
- `smp_smoke` thread logs with different `cpu=` values (verbose boot mode)
- scheduler debug logs: remote enqueue, work steal, reschedule IPI

## SMP safety notes

- `interrupts::handlers::TICKS` is atomic (`AtomicU64`); do not revert to `static mut` because timer IRQs run on all CPUs.
- Scheduler wakeup path (`wake_sleeping_threads`) avoids heap allocation to stay safe in IRQ context.
- `sched` enforces running-thread ownership per thread using `ownership` map with `NO_CPU_OWNER` sentinel; avoid remove/insert churn in hot paths.
- Affinity changes trigger immediate requeue for Ready threads and reschedule IPI if a Running thread is on a disallowed CPU.