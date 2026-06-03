# SMP Architecture (x86_64)

This document describes the current multicore design implemented in Atom.

## Scope

Implemented and enabled by default:

- ACPI MADT CPU discovery (`APIC IDs`)
- BSP/AP split with AP startup via INIT/SIPI + trampoline
- Per-CPU bootstrap and runtime state
- Per-CPU scheduler state (idle/current/run queue/ticks)
- Cross-core wakeups and reschedule IPIs
- Work stealing + affinity masks
- Per-CPU syscall stack state via `GS_BASE` (`swapgs` path)

## Boot and AP Bring-up

Entry path:

1. BSP initializes memory, GDT/IDT, LAPIC, scheduler bootstrap.
2. BSP parses MADT and builds `cpu_id <-> apic_id` mapping.
3. BSP copies AP trampoline to low memory (`0x8000`) and programs startup mailbox.
4. BSP sends INIT + SIPI to each APIC ID.
5. AP enters trampoline, switches to long mode entry, and calls `smp_ap_entry`.
6. AP performs per-CPU init: GDT/TSS, IDT load, LAPIC local init, timer init, per-CPU idle thread, marks Online.

If AP startup fails, BSP logs timeout/failure and continues safely with online CPUs.

## Per-CPU State

`kernel/src/smp.rs` owns CPU topology/state and syscall GS-local state.

Per CPU runtime includes:

- lifecycle state: Booting/Online/Idle/Offline/Panic
- APIC ID mapping
- online bitmap/counters
- GS-local syscall state (`current_kstack`, `temp_user_rsp`)

Scheduler-local per CPU (`kernel/src/sched.rs`):

- local ready queues by priority
- current thread id
- idle thread id
- local tick/context-switch/steal counters
- reschedule-pending flag

## Scheduler Model

The scheduler is now per-CPU with fixed priorities + round-robin per priority class.

Key properties:

- enqueue chooses target CPU by least-load + affinity filter
- each CPU pops from local queue first
- idle CPU attempts work stealing from other CPUs
- remote wakeup can trigger LAPIC reschedule IPI
- CPU affinity is mask-based (`u64`, bit N = CPU N)

State invariants enforced by transitions and queue ownership bookkeeping:

- `Ready` => exactly one run queue
- `Running(cpu)` => not in any run queue
- `Blocked/Sleeping/Exited` => not in run queue

## Interrupts, Context Switch, Syscall Path

- Timer interrupt is local per CPU (LAPIC timer).
- Reschedule IPI vector (`0x2D`) drives immediate local scheduling.
- Context-switch updates both TSS.RSP0 and per-CPU GS syscall stack pointer.
- Syscall ASM uses `swapgs` and `gs:[offset]` instead of global stack variables.

## IPC and Cross-core Wakeups

IPC wakeups route through scheduler enqueue APIs and are SMP-safe:

- unblock may target remote CPU
- remote enqueue may send reschedule IPI when preemption is beneficial
- priority donation logic still updates effective priority and scheduler placement

## Locking Rules (current)

- Never sleep/block while holding spinlocks.
- Scheduler critical data is protected by per-CPU queue locks + short global maps (`affinity`, `ownership`, priorities).
- Thread/process/capability registries remain globally locked but are used in short critical sections.
- Cross-subsystem ordering should avoid nested lock cycles; scheduler avoids holding a run-queue lock while issuing heavy operations.

## Syscall/ABI additions

Added syscall numbers:

- `SYS_GET_CPU_ID = 90`
- `SYS_GET_CPU_COUNT = 91`
- `SYS_SET_THREAD_AFFINITY = 92`
- `SYS_GET_THREAD_AFFINITY = 93`

Implemented in kernel and userspace wrappers (`userspace/libs/syscall`).

## Running and Smoke Validation

Linux/macOS:

```bash
./build.sh --run --smp=2
./build.sh --run --smp=4
```

Windows:

```powershell
.\build.ps1 --run --Smp 2
.\build.ps1 --run --Smp 4
```

Expected serial evidence:

- detected CPUs and APIC IDs
- AP online logs
- per-CPU idle/scheduler startup
- `smp_smoke` kernel threads reporting execution on different CPU IDs (verbose boot mode)
- scheduler debug logs for remote enqueue, steals, reschedule IPIs

## Known Remaining Work

- deeper lock-order formalization and full-kernel lock audit
- long-duration stress/perf baselines (8+ CPUs, sustained mixed IPC + FS + net)
- broader real-hardware validation beyond QEMU