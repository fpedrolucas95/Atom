# Kernel Invariants (Canonical)

Status: `NORMATIVE`  
Canonical path: `docs/kernel_invariants.md`  
Last update: `2026-04-04`  
Scope: `shared/abi`, `kernel/src/mm`, `kernel/src/process.rs`, `kernel/src/cap.rs`, `kernel/src/syscall/mod.rs`, `kernel/src/mm/oom.rs`

## 1. Purpose and mandatory usage

This document is the single source of truth for architectural contracts in the kernel.

Mandatory rules:

1. Any change in MM, Process, Cap, Syscall, or OOM MUST be reviewed against this document.
2. Any PR touching these subsystems MUST list impacted invariant IDs (`INV-*`).
3. Inline comments are non-normative unless they reference an invariant ID from this file.
4. No contract may exist only in scattered comments. If a rule is architectural, it MUST live here.
5. If code and this document diverge, merge is blocked until both are aligned in the same change set.

## 2. Invariant index

| ID | Invariant | Owner subsystem | Consequence of violation |
|---|---|---|---|
| INV-ADDR-001 | Valid userspace VA is defined only by ABI typed validators | ABI + Syscall + MM | Pointer acceptance drift, undefined behavior |
| INV-MEM-001 | Memory ownership and accounting are process-centric | Process + MM | Partial cleanup, stale accounting, leaks |
| INV-OOM-001 | OOM terminal action is by process, never by isolated thread | OOM + Process + Thread | Zombie process states, nondeterministic reclaim |
| INV-LOCK-001 | Global lock order is explicit and mandatory | All structural subsystems | Deadlock and lock inversion |
| INV-CB-001 | Callbacks execute outside structural registry locks | Cap | Deadlock and lock contention amplification |
| INV-HOT-001 | Hot path does not consult global policy state | MM + Policy | Latency spikes and hidden coupling |
| INV-ERR-001 | Operational failures use structured errors per subsystem | MM + Process + Cap + Syscall + OOM | Kernel panic on recoverable failures |

## 3. Formal definition: valid userspace VA (INV-ADDR-001)

Canonical definition comes from `shared/abi/src/lib.rs` only.

Formal rules:

1. A userspace address `addr` is valid iff:
   - `is_canonical(addr) == true`
   - `USER_SPACE_MIN <= addr < USER_SPACE_MAX`
2. A userspace range `(base, len)` is valid iff:
   - `len > 0`
   - `base` is a valid userspace address
   - `base + len` does not overflow
   - `base + len <= USER_SPACE_MAX` (exclusive upper bound)
3. Syscall return addresses that represent user pointers MUST pass the same validation.

Normative API:

- `atom_abi::validate_user_addr`
- `atom_abi::validate_user_range`
- `atom_abi::validate_user_return_addr`

Prohibited:

- Any "userspace pointer validity" check based on `KERNEL_BASE` comparison.
- Duplicated userspace validation logic outside ABI validators.

## 4. Memory ownership by process (INV-MEM-001)

Process is the owner of:

1. Primary address space identity (PML4 ownership).
2. VMA namespace lifecycle.
3. Resident memory accounting source of truth.
4. Memory limit metadata and admission context.
5. Unified teardown for process termination paths.

Thread is:

- An execution unit, not memory ownership authority.
- Allowed to cache metadata, but cache is never the source of truth.

Required properties:

1. Accounting updates happen on map/materialize/unmap transitions.
2. Process teardown is idempotent.
3. Accounting snapshots used by OOM do not require deep lock recursion.

## 5. OOM semantics (INV-OOM-001)

OOM contract:

1. Pressure assessment is global.
2. Victim selection unit is process.
3. Terminal OOM action is:
   - Transition victim to `Dying(KillReason::Oom)`.
   - Block new growth/fault admission for this process.
   - Terminate all victim threads.
   - Run unified process resource teardown.
   - Transition process to `Dead`.

Prohibited:

- OOM killing a single thread and leaving process alive.
- OOM path that requires nested registry lock chains to compute victim data.

## 6. Global lock order (INV-LOCK-001)

### 6.1 Lock levels

1. Scheduler locks
2. ProcessRegistry lock
2b. CapabilityRegistry lock
3. Process object lock/state
3b. Capability object/state
4. AddressSpace/VMA locks
5. PMM/VMM locks
6. Callback execution context (no structural registry lock held)

### 6.2 Order rule

Locks MUST be acquired from lower level number to higher level number in a single flow (1 -> 2 -> 3 -> 4 -> 5).  
Branch `2b -> 3b` follows the same monotonic rule.  
No backtracking to a lower level is allowed while holding a higher-level lock.

### 6.3 Structural constraints

1. Callback registry is snapshot-only while locked.
2. No function may hold `PROCESS_REGISTRY` and call code that can reacquire process/VMA registry locks.
3. Deep scans use snapshot pattern:
   - Collect IDs/snapshot under lock.
   - Release lock.
   - Execute deeper queries on snapshot.

## 7. Callback execution rules (INV-CB-001)

1. Register and snapshot callbacks under `REVOCATION_CALLBACKS` lock.
2. Invoke callbacks only after releasing `REVOCATION_CALLBACKS`.
3. Callback execution is notification-only; it is not an integrity mechanism.
4. In `no_std`, panic in callback is a fatal kernel bug. No panic recovery is promised.
5. Callback code must avoid lock-order violations and reentrant structural deadlocks.

## 8. Hot path rule: no global policy lookup (INV-HOT-001)

For page-fault hot paths:

1. Classification and materialization MUST not query global OOM/policy state.
2. Hot path MUST not perform deep process registry lookups.
3. Admission/policy decisions happen in a dedicated admission layer, fed by explicit context.

Allowed in hot path:

- Fault context decode
- VMA metadata access
- PTE operations
- Physical allocation and mapping
- Local structured error propagation

## 9. Structured error contracts by subsystem (INV-ERR-001)

| Subsystem | Required contract | Prohibited behavior |
|---|---|---|
| MM/VMA | Typed fault result/error; no boolean collapse for fault cause | Silent `true/false` loss of cause |
| Process | Explicit state transition errors for lifecycle operations | Panic for recoverable operational mismatch |
| Cap | Revocation report with explicit failure surfacing | Silent child revoke failure |
| Syscall | Internal context errors map to structured syscall outcomes | `assert!`/`panic!` for userspace operational errors |
| OOM | Typed `OomResult` for kill/no-victim/reclaim | Panic or halt on no-victim path |

Rule:

- `panic!`/`assert!` are reserved for structural corruption invariants only.
- Operational failures must be reported through subsystem error contracts.

## 10. MM section

Scope owner: `kernel/src/mm/*`

Responsibilities:

1. VMA lookup, page admission input, physical page materialization, mapping.
2. Preserve lock order and keep hot path free from policy coupling.

Must not:

1. Define userspace VA validity independently from ABI.
2. Rebuild process policy state during fault materialization.

Primary invariants:

- `INV-ADDR-001`, `INV-HOT-001`, `INV-LOCK-001`, `INV-ERR-001`

## 11. Process section

Scope owner: `kernel/src/process.rs`

Responsibilities:

1. Process ownership model (address space, accounting authority, lifecycle state).
2. Unified and idempotent process teardown.
3. Snapshot interfaces for OOM and policy consumers.

Must not:

1. Leak process-internal state access patterns across subsystem boundaries.
2. Require deep nested locks for public memory/accounting queries.

Primary invariants:

- `INV-MEM-001`, `INV-OOM-001`, `INV-LOCK-001`, `INV-ERR-001`

## 12. Cap section

Scope owner: `kernel/src/cap.rs`

Responsibilities:

1. Capability graph integrity and revoke semantics.
2. Callback dispatch isolation.
3. Revoke executes as `discovery -> execution -> callback` with explicit report output.

Must not:

1. Hide revoke failures in transitive operations.
2. Execute callbacks under structural callback registry lock.
3. Mutate graph structure during revoke discovery.

Primary invariants:

- `INV-CB-001`, `INV-LOCK-001`, `INV-ERR-001`

### 12.1 Revoke model (normative)

Selected semantic model: **Model B — Partial Explicit**.

Normative rules:

1. Revoke discovery builds a closed immutable `RevokePlan` before mutation.
2. Revoke execution consumes the plan in deterministic DFS post-order.
3. Execution continues on per-node failure and records every failure in `RevokeReport.failed`.
4. Missing nodes are surfaced in `RevokeReport.missing`; no silent discard is allowed.
5. Callback execution is a distinct phase after mutation and outside structural locks.
6. Revoke result visibility is mandatory: each operation returns a `RevokeReport` with status `Complete`, `Partial`, or `Failed`.

## 13. Syscall section

Scope owner: `kernel/src/syscall/mod.rs`

Responsibilities:

1. ABI-compliant pointer validation and return-address contracts.
2. Mapping internal context failures to structured syscall outcomes.

Must not:

1. Redefine userspace address validity outside ABI.
2. Panic on recoverable userspace/context operational errors.

Primary invariants:

- `INV-ADDR-001`, `INV-ERR-001`, `INV-LOCK-001`

## 14. OOM section

Scope owner: `kernel/src/mm/oom.rs`

Responsibilities:

1. Pressure assessment and victim selection.
2. Process-level kill initiation and teardown orchestration.

Must not:

1. Operate with thread-level terminal semantics.
2. Hold structural locks while performing deep memory usage queries.

Primary invariants:

- `INV-OOM-001`, `INV-MEM-001`, `INV-LOCK-001`, `INV-ERR-001`

## 15. Definition of Done

This invariant baseline is considered done only when all items are true:

1. `docs/kernel_invariants.md` exists and is maintained as canonical reference.
2. MM, Process, Cap, Syscall, and OOM each have explicit owner sections in this file.
3. Contributors treat this document as mandatory review input for subsystem changes.
4. Architectural contracts are defined here, not only in scattered comments.
5. Any code comment asserting architectural behavior references an `INV-*` ID.
6. PRs touching the scoped subsystems list impacted invariant IDs and alignment notes.

## 16. Result

Expected outcome:

1. One shared architectural truth replaces local truths.
2. Cross-subsystem changes become reviewable against explicit invariants.
3. Regression risk from implicit contracts and lock-order drift is reduced.
4. Kernel evolution shifts from patch-by-patch fixes to contract-driven engineering.
