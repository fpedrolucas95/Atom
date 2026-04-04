# Requirements Document: Kernel Robustness Improvements — Migração Arquitetural Dirigida por Invariantes

## Introduction

Este documento especifica os requisitos funcionais e não-funcionais para as melhorias de robustez do kernel Atom, organizados em duas camadas:

**Camada 1 — Melhorias Incrementais (Requisitos 1–20):** Validação de memória, hardening de capabilities, OOM graceful degradation, cleanup de threads, resource limits e observabilidade. Parcialmente implementados (Fases 1–3 completas).

**Camada 2 — Migração Arquitetural (Requisitos 21–36):** Invariantes arquiteturais que resolvem o problema central de fronteiras mal definidas entre MM, processo, OOM e capability lifecycle. Estes requisitos capturam os contratos explícitos que o kernel deve obedecer ao fim da migração.

## Glossary

- **PMM**: Physical Memory Manager
- **VMM**: Virtual Memory Manager
- **VMA**: Virtual Memory Area
- **PML4**: Page Map Level 4 — estrutura de paginação x86_64
- **OOM**: Out Of Memory
- **Capability**: Token infalsificável que concede acesso a recurso do kernel
- **ABI**: Application Binary Interface — contrato kernel/userspace em `shared/abi`
- **UserVAddr**: Tipo opaco para endereço de userspace validado pelo ABI
- **UserRange**: Tipo opaco para range de userspace validado pelo ABI
- **FaultResult**: Resultado tipado de resolução de page fault (substitui bool)
- **ProcessState**: Estado explícito do ciclo de vida de um processo
- **RevokeReport**: Resultado agregado de revogação de capability
- **Validation_Layer**: Componente que valida inputs antes de operações
- **Resource_Accounting**: Componente que rastreia uso de recursos por processo
- **Cleanup_Coordinator**: Componente que gerencia cleanup de recursos de thread
- **Memory_Pressure**: Métrica indicando proximidade de OOM

## Requirements

### Requirement 1: Memory Safety Validation

**User Story:** As a kernel developer, I want all memory operations validated before execution, so that invalid inputs are rejected early and cannot cause undefined behavior.

#### Acceptance Criteria

1. WHEN a memory operation is requested with an unaligned address, THEN THE Validation_Layer SHALL return an error indicating the address and required alignment
2. WHEN a memory operation is requested with an out-of-bounds address, THEN THE Validation_Layer SHALL return an error indicating the address and valid range
3. WHEN an attempt is made to free an active PML4 page, THEN THE Validation_Layer SHALL return an error indicating the protected resource
4. WHEN phys_to_virt_ptr is called before higher-half initialization, THEN THE Validation_Layer SHALL return an error indicating not initialized
5. THE Validation_Layer SHALL validate all addresses are page-aligned (4096 bytes) before map/unmap operations
6. THE Validation_Layer SHALL validate all user-space addresses are below KERNEL_BASE (0xFFFF_8000_0000_0000)
7. THE Validation_Layer SHALL validate all sizes are non-zero and within maximum bounds

### Requirement 2: PML4 Protection Enforcement

**User Story:** As a kernel developer, I want active page tables protected from premature freeing, so that the system cannot corrupt its own address space structures.

#### Acceptance Criteria

1. WHEN a page is freed, THE PMM SHALL check if the page is an active PML4 before freeing
2. WHEN a PML4 is active in any address space, THE PMM SHALL mark it as protected
3. WHEN a protected PML4 is requested for freeing, THE PMM SHALL return an error and log the attempt
4. THE PMM SHALL maintain a registry of active PML4 physical addresses
5. THE PMM SHALL automatically unprotect a PML4 when its address space is destroyed

### Requirement 3: Capability Transfer Atomicity

**User Story:** As a kernel developer, I want capability transfers to be atomic, so that partial failures cannot leave the system in an inconsistent state.

#### Acceptance Criteria

1. WHEN a capability transfer fails at any step, THEN THE Capability_System SHALL rollback all changes
2. WHEN a capability transfer rollback occurs, THEN THE Capability_System SHALL restore the capability to the source process
3. WHEN a capability transfer completes, THEN THE Capability_System SHALL ensure the capability exists in exactly one process table
4. THE Capability_System SHALL validate target process capability table has space before starting transfer
5. THE Capability_System SHALL log all transfer attempts and their outcomes to the audit log

### Requirement 4: Audit Log Bounded Size

**User Story:** As a kernel developer, I want the audit log to have bounded size, so that it cannot grow without limit and exhaust memory.

#### Acceptance Criteria

1. WHEN the audit log reaches MAX_AUDIT_LOG_ENTRIES, THEN THE Capability_System SHALL evict the oldest entry
2. THE Capability_System SHALL maintain audit log size at or below MAX_AUDIT_LOG_ENTRIES at all times
3. WHEN an audit entry is evicted, THE Capability_System SHALL log a warning indicating log overflow
4. THE Capability_System SHALL provide statistics on audit log size and eviction count
5. THE Capability_System SHALL allow configuration of MAX_AUDIT_LOG_ENTRIES at compile time

### Requirement 5: Capability Revocation Callbacks

**User Story:** As a kernel developer, I want revocation callbacks for capabilities, so that resources can be cleaned up when capabilities are revoked.

#### Acceptance Criteria

1. WHEN a capability is revoked, THEN THE Capability_System SHALL invoke all registered callbacks for that resource type
2. THE Capability_System SHALL allow registration of revocation callbacks per resource type
3. WHEN a revocation callback fails, THEN THE Capability_System SHALL log the failure and continue with remaining callbacks
4. THE Capability_System SHALL invoke callbacks in registration order
5. THE Capability_System SHALL pass the capability handle to each callback

### Requirement 6: OOM Graceful Degradation

**User Story:** As a system administrator, I want the system to degrade gracefully under OOM conditions, so that it does not halt when no victim process is found.

#### Acceptance Criteria

1. WHEN no OOM victim is found, THEN THE OOM_Manager SHALL attempt memory reclamation strategies
2. WHEN all reclamation strategies fail, THEN THE OOM_Manager SHALL return a structured result indicating the reason and fallback action
3. WHEN OOM occurs with no victim, THEN THE OOM_Manager SHALL log detailed memory pressure information
4. THE OOM_Manager SHALL never panic or halt the system due to OOM conditions
5. THE OOM_Manager SHALL provide fallback actions: DenyAllocation, KillOldestProcess, or EnterEmergencyMode

### Requirement 7: Per-Process Memory Limits

**User Story:** As a system administrator, I want per-process memory limits, so that a single process cannot exhaust all system memory.

#### Acceptance Criteria

1. WHEN a process is created, THEN THE Resource_Accounting SHALL initialize memory limits for that process
2. WHEN a process attempts to allocate memory beyond its hard limit, THEN THE Resource_Accounting SHALL deny the allocation
3. WHEN a process exceeds its soft limit, THEN THE Resource_Accounting SHALL log a warning and allow the allocation
4. THE Resource_Accounting SHALL track resident pages per process in real-time
5. THE Resource_Accounting SHALL provide an API to query and update process memory limits

### Requirement 8: Memory Pressure Detection

**User Story:** As a kernel developer, I want detailed memory pressure detection, so that the system can take proactive action before reaching OOM.

#### Acceptance Criteria

1. THE OOM_Manager SHALL compute memory pressure from free pages, fragmentation, and process limits
2. THE OOM_Manager SHALL classify pressure as None, Low, Critical, or Oom
3. WHEN memory pressure is Critical or Oom, THEN THE OOM_Manager SHALL trigger reclamation strategies
4. THE OOM_Manager SHALL consider both absolute free pages and largest contiguous run
5. THE OOM_Manager SHALL track processes exceeding their memory limits in pressure calculation

### Requirement 9: Unified Thread Cleanup

**User Story:** As a kernel developer, I want unified thread cleanup coordination, so that all resources are properly freed when a thread terminates.

#### Acceptance Criteria

1. WHEN a thread terminates, THEN THE Cleanup_Coordinator SHALL enumerate all resources owned by the thread
2. THE Cleanup_Coordinator SHALL revoke all capabilities owned by the thread
3. THE Cleanup_Coordinator SHALL destroy all address spaces owned by the thread
4. THE Cleanup_Coordinator SHALL close all IPC ports owned by the thread
5. THE Cleanup_Coordinator SHALL free all physical pages owned by the thread
6. WHEN cleanup completes, THEN THE Cleanup_Coordinator SHALL validate no resources remain and log any leaks

### Requirement 10: Cleanup Idempotency

**User Story:** As a kernel developer, I want thread cleanup to be idempotent, so that calling cleanup multiple times does not cause errors or double-frees.

#### Acceptance Criteria

1. WHEN cleanup is called multiple times for the same thread, THEN THE Cleanup_Coordinator SHALL produce the same result
2. THE Cleanup_Coordinator SHALL track which threads have been cleaned up
3. WHEN cleanup is called for an already-cleaned thread, THEN THE Cleanup_Coordinator SHALL log a warning and return immediately
4. THE Cleanup_Coordinator SHALL not attempt to free already-freed resources
5. THE Cleanup_Coordinator SHALL maintain cleanup state across multiple invocations

### Requirement 11: Resource Leak Detection

**User Story:** As a kernel developer, I want automatic resource leak detection, so that leaked resources are identified and logged for debugging.

#### Acceptance Criteria

1. WHEN thread cleanup completes, THEN THE Cleanup_Coordinator SHALL enumerate remaining resources
2. WHEN resources remain after cleanup, THEN THE Cleanup_Coordinator SHALL log each leaked resource with type and ID
3. THE Cleanup_Coordinator SHALL maintain counters for leaked resources by type
4. THE Cleanup_Coordinator SHALL provide an API to query leak statistics
5. THE Cleanup_Coordinator SHALL include leak information in the cleanup result

### Requirement 12: Per-Process Resource Limits

**User Story:** As a system administrator, I want per-process limits for all resource types, so that processes cannot exhaust system resources.

#### Acceptance Criteria

1. THE Resource_Accounting SHALL enforce limits for memory pages, threads, capabilities, IPC ports, and address spaces
2. WHEN a process attempts to allocate a resource beyond its hard limit, THEN THE Resource_Accounting SHALL deny the allocation
3. WHEN a process exceeds a soft limit, THEN THE Resource_Accounting SHALL log a warning and allow the allocation
4. THE Resource_Accounting SHALL track current usage for each resource type per process
5. THE Resource_Accounting SHALL provide an API to query and update resource limits

### Requirement 13: Real-Time Resource Accounting

**User Story:** As a kernel developer, I want real-time resource accounting, so that resource usage is always accurate and up-to-date.

#### Acceptance Criteria

1. WHEN a resource is allocated, THEN THE Resource_Accounting SHALL increment the usage counter atomically
2. WHEN a resource is freed, THEN THE Resource_Accounting SHALL decrement the usage counter atomically
3. THE Resource_Accounting SHALL maintain separate counters for each resource type per process
4. THE Resource_Accounting SHALL ensure current usage never exceeds hard limits
5. THE Resource_Accounting SHALL provide an API to query current resource usage

### Requirement 14: Memory Fragmentation Metrics

**User Story:** As a system administrator, I want memory fragmentation metrics, so that I can monitor system health and predict allocation failures.

#### Acceptance Criteria

1. THE PMM SHALL track the largest contiguous free run of pages
2. THE PMM SHALL compute a fragmentation score based on free page distribution
3. THE PMM SHALL provide an API to query fragmentation statistics
4. THE PMM SHALL update fragmentation metrics after each allocation and deallocation
5. THE PMM SHALL include fragmentation metrics in memory pressure calculation

### Requirement 15: Capability Usage Statistics

**User Story:** As a system administrator, I want capability usage statistics, so that I can monitor capability usage patterns and detect anomalies.

#### Acceptance Criteria

1. THE Capability_System SHALL track capability count by resource type
2. THE Capability_System SHALL compute capability graph depth (longest derivation chain)
3. THE Capability_System SHALL provide an API to query capability statistics
4. THE Capability_System SHALL track capability creation, derivation, transfer, and revocation counts
5. THE Capability_System SHALL include capability statistics in system resource summary

### Requirement 16: Thread Resource Breakdown

**User Story:** As a kernel developer, I want per-thread resource breakdowns, so that I can identify which threads are consuming the most resources.

#### Acceptance Criteria

1. THE Resource_Accounting SHALL track capabilities, address spaces, IPC ports, and memory pages per thread
2. THE Resource_Accounting SHALL provide an API to query resource breakdown for a specific thread
3. THE Resource_Accounting SHALL include resource breakdown in cleanup results
4. THE Resource_Accounting SHALL update resource breakdown in real-time as resources are allocated and freed
5. THE Resource_Accounting SHALL provide a system-wide resource summary aggregating all threads

### Requirement 17: Error Propagation

**User Story:** As a kernel developer, I want comprehensive error propagation, so that errors are returned to callers instead of causing panics.

#### Acceptance Criteria

1. WHEN a validation check fails, THEN THE Validation_Layer SHALL return a specific error variant
2. WHEN a resource limit is exceeded, THEN THE Resource_Accounting SHALL return a limit error
3. WHEN an OOM condition occurs, THEN THE OOM_Manager SHALL return a structured OOM result
4. WHEN cleanup fails, THEN THE Cleanup_Coordinator SHALL return a cleanup result with errors
5. THE kernel SHALL never panic due to validation failures, limit violations, or OOM conditions

### Requirement 18: Diagnostic Logging

**User Story:** As a kernel developer, I want comprehensive diagnostic logging, so that I can debug issues and understand system behavior.

#### Acceptance Criteria

1. WHEN a validation check fails, THEN THE Validation_Layer SHALL log the failure with address and reason
2. WHEN a resource limit is exceeded, THEN THE Resource_Accounting SHALL log the violation with process ID and resource type
3. WHEN an OOM condition occurs, THEN THE OOM_Manager SHALL log memory pressure and all process memory usage
4. WHEN cleanup detects leaks, THEN THE Cleanup_Coordinator SHALL log each leaked resource
5. THE kernel SHALL log all security-relevant operations to the audit trail

### Requirement 19: Rollback Mechanisms

**User Story:** As a kernel developer, I want automatic rollback on partial failures, so that the system remains in a consistent state.

#### Acceptance Criteria

1. WHEN a multi-step operation fails, THEN THE kernel SHALL rollback all completed steps
2. WHEN a capability transfer fails, THEN THE Capability_System SHALL restore the original state
3. WHEN a memory mapping fails partway through, THEN THE VMM SHALL unmap all successfully mapped pages
4. THE kernel SHALL log all rollback operations with the reason for rollback
5. THE kernel SHALL ensure rollback operations are idempotent and cannot fail

### Requirement 20: Performance Overhead Limits

**User Story:** As a kernel developer, I want robustness improvements to have minimal performance overhead, so that system performance is not significantly impacted.

#### Acceptance Criteria

1. THE Validation_Layer SHALL add no more than 20 CPU cycles per memory operation
2. THE Resource_Accounting SHALL add no more than 10 CPU cycles per resource allocation
3. THE Cleanup_Coordinator SHALL complete cleanup in O(r) time where r is the number of resources
4. THE OOM_Manager SHALL complete victim selection in O(n) time where n is the number of processes
5. THE kernel SHALL cache frequently computed values (e.g., memory pressure) to avoid repeated computation

---

## Architectural Migration Requirements (Invariant-Driven)

Os requisitos a seguir capturam os invariantes arquiteturais que devem ser verdadeiros ao fim da migração. Cada requisito corresponde a um invariante do sistema que elimina uma fronteira mal definida entre subsistemas.

### Requirement 21: Fonte Única de Verdade para Endereços de Userspace

**User Story:** As a kernel developer, I want a single authoritative definition of valid userspace addresses, so that validation logic cannot diverge between subsystems.

#### Acceptance Criteria

1. THE ABI module (`shared/abi`) SHALL be the sole authority for userspace address validation
2. THE kernel SHALL NOT contain any validation of userspace addresses based on `KERNEL_BASE` local constant after migration
3. THE syscall layer SHALL only accept `UserVAddr` and `UserRange` typed values for userspace pointers, never raw `usize`
4. THE MM subsystem SHALL NOT accept raw `usize` for userspace pointers without prior ABI validation
5. WHEN `validate_user_addr(addr)` is called, THEN it SHALL return `UserAddressError::NonCanonical` for non-canonical addresses, `UserAddressError::BelowUserMin` for null/low addresses, `UserAddressError::AboveUserMax` for kernel-space addresses
6. THE ABI SHALL export `validate_user_addr`, `validate_user_range`, and `validate_user_return_addr` as the canonical validation API
7. AFTER migration, there SHALL be no code path that validates userspace pointers via `KERNEL_BASE` comparison

### Requirement 22: Resultado Tipado de Page Fault

**User Story:** As a kernel developer, I want page fault resolution to return typed errors, so that callers can make decisions based on the specific failure reason without heuristics.

#### Acceptance Criteria

1. THE fault resolution path SHALL return `FaultResult = Result<FaultResolved, FaultError>` instead of `bool`
2. THE `FaultError` enum SHALL include variants: `NoVma`, `AccessViolation`, `WriteToReadonly`, `ExecViolation`, `AddressNotUser`, `QuotaExceeded`, `PhysicalAllocFailed`, `MapFailed`, `ProcessContextMissing`, `InvariantBroken`
3. WHEN a fault cannot be resolved, THE fault handler SHALL return the specific `FaultError` variant matching the cause
4. THE caller of fault resolution SHALL NOT perform textual heuristics to determine the failure reason
5. ALL log messages for fault failures SHALL include the typed `FaultError` variant

### Requirement 23: Separação de Mecanismo e Policy no Fault Path

**User Story:** As a kernel developer, I want fault resolution separated into classifier, admission, and materializer layers, so that each layer has a single responsibility.

#### Acceptance Criteria

1. THE fault path SHALL be organized into three distinct layers: `classify_fault`, `admit_memory_growth`, `materialize_fault`
2. THE `classify_fault` function SHALL only determine VMA membership and access permission — it SHALL NOT consult quota, OOM registry, or policy manager
3. THE `admit_memory_growth` function SHALL only determine if the process can grow — it SHALL NOT allocate physical memory
4. THE `materialize_fault` function SHALL only perform physical allocation, zero-fill, mapping, and accounting — it SHALL NOT look up process identity from PML4 or consult global OOM state
5. THE `materialize_fault` function SHALL receive a `ProcessMemoryContext` as explicit parameter, not derive it from global state
6. AFTER migration, `resolve_anon_fault` SHALL NOT call `get_process_memory_usage()` or `process_id_for_pml4()` in the hot path

### Requirement 24: OOM Opera por Processo

**User Story:** As a kernel developer, I want OOM to kill entire processes, not individual threads, so that the system does not enter zombie process states.

#### Acceptance Criteria

1. THE OOM killer SHALL select a victim process, not a victim thread
2. WHEN an OOM victim is selected, THE OOM_Manager SHALL transition the process to `ProcessState::Dying(KillReason::Oom)`
3. WHEN a process enters `Dying` state, THE fault materializer SHALL reject new fault materializations for that process
4. WHEN a process enters `Dying` state, THE OOM_Manager SHALL terminate ALL threads of that process
5. WHEN a process enters `Dying` state, THE OOM_Manager SHALL trigger unified teardown: address space, capabilities, IPC ports
6. AFTER OOM kill completes, THE process SHALL be in `Dead` state with all resources released
7. THE `count_processes_over_limit` function SHALL use snapshot-based analysis and SHALL NOT hold registry lock during deep queries

### Requirement 25: ProcessState Explícito

**User Story:** As a kernel developer, I want explicit process lifecycle states, so that subsystems can make correct decisions without heuristics.

#### Acceptance Criteria

1. THE `Process` struct SHALL include a `state: ProcessState` field
2. THE `ProcessState` enum SHALL include variants: `Running`, `Exiting(ExitReason)`, `Dying(KillReason)`, `Dead`
3. THE `KillReason` enum SHALL include variants: `Oom`, `FatalFault`, `CapabilityViolation`
4. WHEN a process is in `Dying` state, THE fault materializer SHALL return `FaultError::ProcessContextMissing`
5. THE `transition_to_dying` function SHALL be atomic and SHALL prevent concurrent transitions
6. THE process state SHALL be observable via `get_process_state(process_id) -> Option<ProcessState>`

### Requirement 26: Capability Revoke Transacional

**User Story:** As a kernel developer, I want capability revocation to be transactional, so that partial revocation is never silently ignored.

#### Acceptance Criteria

1. THE capability revocation SHALL use a two-phase protocol: mark as `Revoking`, then commit removal
2. WHEN a capability is in `Revoking` state, THE capability system SHALL reject new uses of that capability
3. THE revocation SHALL return `RevokeReport` containing `revoked: Vec<CapHandle>` and `failed: Vec<(CapHandle, RevokeError)>`
4. WHEN a child capability fails to revoke, THE failure SHALL be recorded in `RevokeReport.failed` — it SHALL NOT be silently discarded
5. THE `RevokeError` enum SHALL include variants: `ChildRevokeFailed(CapHandle)`, `Busy(CapHandle)`, `InvariantBroken`
6. AFTER migration, there SHALL be no occurrence of `let _ = self.revoke(...)` or equivalent error-discarding patterns in the codebase
7. THE state of the capability tree SHALL be observable and consistent after any revocation attempt

### Requirement 27: Callbacks Fora de Lock Estrutural

**User Story:** As a kernel developer, I want revocation callbacks to execute outside structural locks, so that callbacks cannot cause deadlocks.

#### Acceptance Criteria

1. THE `invoke_revocation_callbacks` function SHALL snapshot the callback list before invocation
2. THE `REVOCATION_CALLBACKS` lock SHALL be released before any callback is invoked
3. WHEN callbacks are invoked, THE `REVOCATION_CALLBACKS` lock SHALL NOT be held
4. THE documentation for revocation callbacks SHALL state explicitly: "panic in a callback is a fatal kernel bug"
5. THE kernel SHALL NOT promise panic recovery for callbacks in no_std environments
6. AFTER migration, there SHALL be no code path where a callback executes while `REVOCATION_CALLBACKS` is locked

### Requirement 28: Hierarquia Global de Locks Documentada

**User Story:** As a kernel developer, I want a documented and enforced lock acquisition order, so that deadlocks from accidental lock inversion are structurally impossible.

#### Acceptance Criteria

1. THE kernel SHALL maintain a `KERNEL_INVARIANTS.md` document defining the global lock order
2. THE lock hierarchy SHALL define levels: Scheduler(1) > ProcessRegistry(2) > Process(3) > AddressSpace/VMA(4) > PMM/VMM(5), CapabilityRegistry(2b) > CapabilityObject(3b), CallbackRegistry(snapshot-only)
3. WHEN a function holds a lock at level N, it SHALL NOT call any function that acquires a lock at level ≤ N
4. WHEN a global scan is needed, THE scan SHALL use snapshot iteration and SHALL NOT hold the registry lock during deep analysis
5. CRITICAL functions SHALL include lock contract comments documenting which locks they acquire and which they must not be called under
6. THE `oom.rs`, `vma.rs`, and `cap.rs` modules SHALL follow the formal lock order

### Requirement 29: Sem assert!/panic! em Caminhos Operacionais

**User Story:** As a kernel developer, I want operational errors to produce structured results instead of kernel panics, so that a single process failure cannot crash the entire system.

#### Acceptance Criteria

1. THE syscall hot path SHALL NOT contain `assert_eq!` or `assert!` macros for operational conditions
2. THE `log_panic!` macro SHALL NOT be used for operational errors related to userspace or process state
3. WHEN a syscall context error occurs (missing process, invalid return address, PML4 mismatch), THE kernel SHALL return a `SyscallContextError` and trigger local process teardown
4. THE `SyscallContextError` enum SHALL include variants: `ProcessContextMismatch`, `InvalidUserReturnAddress`, `MissingAddressSpace`, `ThreadMetadataDrift`
5. AFTER migration, `assert!`/`panic!` SHALL be reserved for structural kernel invariant violations only (e.g., corrupted central tables)
6. WHEN an operational error triggers process teardown, THE kernel SHALL continue running for other processes

### Requirement 30: Interfaces de Ownership Claras entre Subsistemas

**User Story:** As a kernel developer, I want clear ownership boundaries between MM, Process, OOM, Capabilities, and IPC, so that each subsystem has a single responsibility.

#### Acceptance Criteria

1. THE MM subsystem SHALL own: VMA lookup, page admission input, physical materialization, mapping
2. THE Process subsystem SHALL own: address space ownership, aggregated accounting, lifecycle state, quota metadata
3. THE OOM subsystem SHALL own: global pressure assessment, victim selection, process kill initiation
4. THE Capability subsystem SHALL own: derivation graph, revoke state machine; callbacks SHALL be notifications only, not integrity mechanisms
5. THE IPC/Callback subsystem SHALL react to external events without holding structural locks and without influencing revocation atomicity
6. AFTER migration, no subsystem SHALL reach into another subsystem's internal state without going through the defined interface

### Requirement 31: Documento de Invariantes do Kernel

**User Story:** As a kernel developer, I want a formal invariants document, so that all contributors understand the architectural contracts before modifying critical subsystems.

#### Acceptance Criteria

1. THE kernel SHALL include a `KERNEL_INVARIANTS.md` document at the repository root or kernel directory
2. THE document SHALL define: memory addressing model, global lock order, memory ownership semantics, revoke semantics, OOM semantics
3. THE document SHALL be updated before any code change that affects the defined invariants
4. THE document SHALL include: for each invariant, the invariant statement, the subsystem responsible, and the consequence of violation
5. THE document SHALL be the reference for code review of changes to `mm/`, `cap.rs`, `process.rs`, `oom.rs`

### Requirement 32: Unificação do Contrato de Endereços de Userspace

**User Story:** As a kernel developer, I want the ABI to be the single source of truth for userspace address validation, eliminating the local KERNEL_BASE-based check.

#### Acceptance Criteria

1. THE functions `is_valid_user_address`, `is_valid_user_range`, `is_valid_user_return_address` SHALL be defined only in `shared/abi`
2. THE `validate_user_space_bounds()` function in `kernel/src/mm/mod.rs` SHALL be replaced by ABI-based validation
3. THE `UserVAddr` and `UserRange` types SHALL be new opaque types that can only be constructed via ABI validation functions
4. THE syscall layer SHALL use only the typed API for userspace pointer validation
5. AFTER migration, there SHALL be no duplicate validation logic between ABI and kernel MM

### Requirement 33: Accounting de Memória por Processo Unificado

**User Story:** As a kernel developer, I want a single source of truth for per-process memory accounting, so that OOM decisions are based on accurate data without deep lock chains.

#### Acceptance Criteria

1. THE per-process memory accounting SHALL have a single authoritative source updated at map/unmap/materialization events
2. THE OOM subsystem SHALL query aggregated accounting snapshots, not reconstruct memory usage via deep registry scans
3. THE `get_process_memory_snapshot()` function SHALL return a lightweight snapshot without holding nested locks
4. THE accounting SHALL be updated atomically at each map and unmap event
5. AFTER migration, the OOM killer SHALL NOT call `get_process_memory_usage()` while holding any registry lock

### Requirement 34: Teardown Unificado de Processo

**User Story:** As a kernel developer, I want a unified process teardown path, so that all resources are deterministically released when a process dies for any reason.

#### Acceptance Criteria

1. THE kernel SHALL have a single `cleanup_process_resources(process_id)` function used for all process termination paths (normal exit, OOM kill, fatal fault)
2. WHEN `cleanup_process_resources` is called, it SHALL release: address space, all capabilities, all IPC ports, all shared memory regions
3. THE teardown SHALL be idempotent — calling it multiple times SHALL produce the same result
4. AFTER teardown completes, THE process SHALL be in `Dead` state
5. THE teardown function SHALL log a structured summary of all resources released

### Requirement 35: Sequência de Merge Controlada

**User Story:** As a kernel developer, I want the architectural migration to follow a defined merge sequence, so that each step is independently verifiable and does not break existing functionality.

#### Acceptance Criteria

1. THE migration SHALL follow this sequence: (1) Address contract unification, (2) FaultResult typed, (3) Fault path split, (4) Process memory accounting unified, (5) OOM by process, (6) Lock hierarchy cleanup, (7) Cap revoke redesign, (8) Callback isolation, (9) Operational error handling cleanup
2. EACH step SHALL be independently mergeable without breaking existing tests
3. EACH step SHALL have a Definition of Done that can be verified by code inspection
4. THE migration SHALL NOT proceed to step N+1 until step N passes all existing tests
5. AFTER all 9 steps, the system SHALL satisfy all 8 architectural invariants defined in the design document

### Requirement 36: Critérios de Pronto do Sistema

**User Story:** As a kernel developer, I want unambiguous system-level acceptance criteria for the migration, so that completion can be verified objectively.

#### Acceptance Criteria

1. AFTER migration: "Userspace pointer válido é definido onde?" SHALL have exactly one answer: ABI (`shared/abi`)
2. AFTER migration: "Quem decide quota/política de memória?" SHALL have exactly one answer: camada de admission/policy, não fault materializer
3. AFTER migration: "OOM mata quem?" SHALL have exactly one answer: processo (não thread individual)
4. AFTER migration: "Revogação parcial pode passar silenciosa?" SHALL have exactly one answer: não
5. AFTER migration: "Callback roda sob lock?" SHALL have exactly one answer: não
6. AFTER migration: "Erro operacional panica kernel?" SHALL have exactly one answer: não, salvo corrupção estrutural real
7. AFTER migration: "Existe lock order documentado e seguido?" SHALL have exactly one answer: sim, em `KERNEL_INVARIANTS.md`
