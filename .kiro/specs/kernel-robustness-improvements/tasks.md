# Implementation Plan: Kernel Robustness Improvements

## Overview

Este plano cobre duas camadas de trabalho:

**Camada 1 — Melhorias Incrementais (Fases 1–6):** Validação de memória, hardening de capabilities, OOM graceful degradation, cleanup de threads, resource limits e observabilidade. As fases 1–3 estão completas; as fases 4–5 estão em progresso; a Fase 6 (Syscall Hardening) está planejada.

**Camada 2 — Migração Arquitetural (Fases 0–9):** Plano de migração completo dirigido por invariantes, organizado em 9 epics que seguem a sequência de merge recomendada. Cada epic é independentemente mergeable e tem Definition of Done verificável.

## Tasks

- [x] 1. Phase 1: Memory Safety Validation Layer
  - Implement ValidationError enum and validation functions
  - Add PML4 protection registry in PMM
  - Add phys_to_virt_ptr safety checks
  - _Requirements: Req 1, Req 2_

  - [x] 1.1 Create ValidationError enum in kernel/src/mm/mod.rs
    - Define ValidationError with variants: Unaligned, OutOfBounds, ProtectedResource, NotInitialized, InvalidSize
    - Each variant should include relevant context (addresses, limits, resource IDs)
    - Implement Display trait for user-friendly error messages
    - _Requirements: Req 1, Req 17_

  - [x] 1.2 Implement page alignment validation functions in kernel/src/mm/mod.rs
    - Add validate_page_alignment(addr: usize) -> Result<(), ValidationError>
    - Add validate_page_range(start: usize, end: usize) -> Result<(), ValidationError>
    - Add validate_user_space_bounds(addr: usize, size: usize) -> Result<(), ValidationError>
    - Validate addresses are page-aligned (4096 bytes)
    - Validate user-space addresses are below KERNEL_BASE
    - _Requirements: Req 1.1, Req 1.5, Req 1.6_

  - [x] 1.3 Add PML4 protection registry in kernel/src/mm/pmm.rs
    - Add static PROTECTED_PML4S: Mutex<BTreeSet<usize>> registry
    - Implement register_active_pml4(pml4_phys: usize) function
    - Implement unregister_active_pml4(pml4_phys: usize) function
    - Implement is_pml4_protected(pml4_phys: usize) -> bool function
    - _Requirements: Req 2.1, Req 2.2, Req 2.4_

  - [x] 1.4 Modify pmm::free_page to check PML4 protection
    - Before freeing, call is_pml4_protected(addr)
    - If protected, return ValidationError::ProtectedResource and log attempt
    - Add diagnostic logging for protection violations
    - _Requirements: Req 2.3_

  - [x] 1.5 Add safe phys_to_virt_ptr wrapper in kernel/src/mm/vm.rs
    - Create phys_to_virt_ptr_safe(phys: usize) -> Result<usize, ValidationError>
    - Check HIGHER_HALF_READY flag before using higher-half offset
    - Return ValidationError::NotInitialized if called before init
    - Update existing phys_to_virt_ptr to use this internally
    - _Requirements: Req 1.4_

  - [x] 1.6 Integrate validation into vm::map_page_in_pml4
    - Call validate_page_alignment for virt and phys addresses
    - Call validate_user_space_bounds for user-space mappings
    - Return ValidationError instead of VmError for validation failures
    - _Requirements: Req 1.1, Req 1.5, Req 1.6_

  - [x] 1.7 Register/unregister PML4s in vm::init and process lifecycle
    - Call register_active_pml4 in vm::init for kernel PML4
    - Call register_active_pml4 when creating new process address spaces
    - Call unregister_active_pml4 in process cleanup
    - _Requirements: Req 2.4, Req 2.5_

- [x] 2. Checkpoint - Validate memory safety layer
  - Ensure all tests pass, ask the user if questions arise.

- [x] 3. Phase 2: Capability System Hardening
  - Refactor transfer_capability for atomic rollback
  - Implement bounded audit log with eviction
  - Add revocation callback registry
  - _Requirements: Req 3, Req 4, Req 5_

  - [x] 3.1 Refactor transfer_capability in kernel/src/cap.rs for atomic rollback
    - Extract rollback logic into separate rollback_transfer function
    - Validate target process capability table has space before starting
    - On any failure, call rollback_transfer to restore original state
    - Ensure capability exists in exactly one process table after completion
    - Add comprehensive error logging for each failure point
    - _Requirements: Req 3.1, Req 3.2, Req 3.3, Req 3.4_

  - [x] 3.2 Implement bounded audit log in kernel/src/cap.rs
    - Modify log_audit to check if log size >= MAX_AUDIT_LOG_ENTRIES
    - When full, evict oldest entry with pop_front()
    - Log warning when eviction occurs
    - Add get_audit_stats() function returning size and eviction count
    - _Requirements: Req 4.1, Req 4.2, Req 4.3, Req 4.4_

  - [x] 3.3 Add revocation callback registry in kernel/src/cap.rs
    - Add static REVOCATION_CALLBACKS: Mutex<BTreeMap<ResourceType, Vec<fn(CapHandle)>>>
    - Implement register_revocation_callback(resource_type, callback) function
    - Modify revoke_capability to invoke all registered callbacks
    - Log callback failures but continue with remaining callbacks
    - Invoke callbacks in registration order
    - _Requirements: Req 5.1, Req 5.2, Req 5.3, Req 5.4, Req 5.5_

  - [x] 3.4 Add audit log configuration in kernel/src/cap.rs
    - Make MAX_AUDIT_LOG_ENTRIES configurable at compile time
    - Add documentation for tuning audit log size
    - _Requirements: Req 4.5_

- [x] 4. Checkpoint - Validate capability hardening
  - Ensure all tests pass, ask the user if questions arise.

- [x] 5. Phase 3: OOM Management Improvements
  - Replace log_panic! with graceful degradation
  - Implement per-process memory limits
  - Add detailed memory pressure detection
  - _Requirements: Req 6, Req 7, Req 8_

  - [x] 5.1 Refactor OomResult enum in kernel/src/mm/oom.rs
    - Add NoVictim variant with reason: NoVictimReason and fallback_action: FallbackAction
    - Add Reclaimed variant with strategy: ReclaimStrategy and pages_freed: usize
    - Define NoVictimReason enum: NoUserProcesses, AllProcessesBelowMinimum, SystemReserved
    - Define FallbackAction enum: DenyAllocation, KillOldestProcess, EnterEmergencyMode
    - _Requirements: Req 6.2_

  - [x] 5.2 Replace log_panic! in oom_kill with graceful degradation
    - When no victim found, return OomResult::NoVictim with reason and fallback
    - Log detailed memory pressure information
    - Never panic or halt the system
    - _Requirements: Req 6.1, Req 6.3, Req 6.4_

  - [x] 5.3 Implement try_reclaim_memory in kernel/src/mm/oom.rs
    - Add ReclaimStrategy enum: CacheEviction, CompactMemory, SwapOut
    - Implement try_reclaim_memory(strategy: ReclaimStrategy) -> Result<usize, OomError>
    - Return number of pages freed on success
    - _Requirements: Req 6.1, Req 6.5_

  - [x] 5.4 Add per-process memory limits in kernel/src/process.rs
    - Add memory_limit_pages: usize field to Process struct
    - Implement set_process_memory_limit(process_id, limit_pages) function
    - Implement get_process_memory_usage(process_id) -> Option<MemoryUsage> function
    - _Requirements: Req 7.1, Req 7.5_

  - [x] 5.5 Enforce per-process memory limits in allocation paths
    - Check process memory usage against limit before allocation
    - Deny allocation if hard limit exceeded
    - Log warning if soft limit exceeded but allow allocation
    - _Requirements: Req 7.2, Req 7.3_

  - [x] 5.6 Implement detailed memory pressure detection in kernel/src/mm/oom.rs
    - Create MemoryPressureInfo struct with level, free_pages, fragmentation_score, processes_over_limit
    - Implement check_memory_pressure_detailed() -> MemoryPressureInfo
    - Consider both absolute free pages and largest contiguous run
    - Track processes exceeding their memory limits
    - _Requirements: Req 8.1, Req 8.2, Req 8.3, Req 8.4, Req 8.5_

  - [x] 5.7 Integrate memory pressure into OOM killer
    - Call check_memory_pressure_detailed() before victim selection
    - Trigger reclamation strategies when pressure is Critical or Oom
    - Include pressure info in all OOM-related log messages
    - _Requirements: Req 8.2, Req 8.3_

- [x] 6. Checkpoint - Validate OOM improvements
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 7. Phase 4: Thread Cleanup Simplification
  - Create unified cleanup coordinator
  - Implement resource enumeration
  - Add leak detection and validation
  - _Requirements: Req 9, Req 10, Req 11_

  - [~] 7.1 Create CleanupResult struct in kernel/src/thread.rs
    - Define CleanupResult with fields: capabilities_revoked, address_spaces_destroyed, ipc_ports_closed, physical_pages_freed, leaks_detected, errors
    - Define LeakedResource enum: Capability, AddressSpace, IpcPort, PhysicalPage
    - Define CleanupError enum: ResourceNotFound, PermissionDenied, PartialCleanup
    - _Requirements: Req 9.6, Req 11.2, Req 11.5_

  - [~] 7.2 Create ThreadResources struct in kernel/src/thread.rs
    - Define ThreadResources with fields: capabilities, address_spaces, ipc_ports, kernel_stack, kernel_stack_pages
    - Implement total_count() method
    - Implement all_resources() iterator
    - _Requirements: Req 9.1_

  - [~] 7.3 Implement enumerate_thread_resources in kernel/src/thread.rs
    - Query capability system for owned capabilities
    - Query address space manager for owned address spaces
    - Query IPC system for owned ports
    - Query thread metadata for kernel stack info
    - Return ThreadResources struct
    - _Requirements: Req 9.1_

  - [~] 7.4 Implement cleanup_thread_resources in kernel/src/thread.rs
    - Call enumerate_thread_resources to get all owned resources
    - Revoke all capabilities (call revoke_capability for each)
    - Destroy all address spaces (call destroy_address_space for each)
    - Close all IPC ports (call close_port for each)
    - Free kernel stack pages
    - Collect errors and leaks into CleanupResult
    - _Requirements: Req 9.2, Req 9.3, Req 9.4, Req 9.5_

  - [~] 7.5 Implement validate_cleanup_complete in kernel/src/thread.rs
    - Call enumerate_thread_resources after cleanup
    - If any resources remain, return CleanupError with leak details
    - Log each leaked resource
    - _Requirements: Req 9.6, Req 11.1, Req 11.2_

  - [~] 7.6 Implement cleanup idempotency tracking
    - Add static CLEANED_THREADS: Mutex<BTreeSet<ThreadId>> registry
    - Check if thread already cleaned before starting cleanup
    - Mark thread as cleaned after successful cleanup
    - Return immediately if already cleaned
    - _Requirements: Req 10.1, Req 10.2, Req 10.3, Req 10.5_

  - [~] 7.7 Integrate cleanup_thread_resources into thread termination
    - Replace scattered cleanup calls with single cleanup_thread_resources call
    - Log CleanupResult details
    - Handle cleanup errors gracefully
    - _Requirements: Req 9.1, Req 9.6_

- [~] 8. Checkpoint - Validate thread cleanup
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 9. Phase 5: Resource Limits and Accounting
  - Implement ProcessLimits and Limit structs
  - Add real-time resource accounting
  - Integrate with allocation paths
  - _Requirements: Req 12, Req 13_

  - [~] 9.1 Create Limit struct in kernel/src/process.rs
    - Define Limit with fields: current, maximum, soft_limit, hard_limit
    - Implement check_allocation() -> Result<(), LimitError> method
    - Log warning when soft limit exceeded
    - Return error when hard limit exceeded
    - _Requirements: Req 12.2, Req 12.3_

  - [~] 9.2 Create ProcessLimits struct in kernel/src/process.rs
    - Define ProcessLimits with fields: memory_pages, threads, capabilities, ipc_ports, address_spaces
    - Each field is a Limit struct
    - _Requirements: Req 12.1_

  - [~] 9.3 Add ProcessLimits to Process struct
    - Add limits: ProcessLimits field to Process struct
    - Initialize with default limits in process creation
    - _Requirements: Req 12.1_

  - [~] 9.4 Implement set_process_limits in kernel/src/process.rs
    - Add set_process_limits(process_id, limits: ProcessLimits) -> Result<(), LimitError>
    - Validate new limits are reasonable (soft <= hard, current <= hard)
    - Update process limits atomically
    - _Requirements: Req 12.1_

  - [~] 9.5 Implement check_process_limit in kernel/src/process.rs
    - Add check_process_limit(process_id, resource: ResourceType) -> Result<(), LimitError>
    - Look up process limits
    - Call limit.check_allocation() for the resource type
    - _Requirements: Req 12.2_

  - [~] 9.6 Implement resource accounting functions in kernel/src/process.rs
    - Add account_resource_allocation(process_id, resource: ResourceType) -> Result<(), AccountingError>
    - Add account_resource_deallocation(process_id, resource: ResourceType)
    - Add get_process_resource_usage(process_id) -> ResourceUsage
    - Update counters atomically
    - _Requirements: Req 13.1, Req 13.2, Req 13.5_

  - [~] 9.7 Integrate limit checks into allocation paths
    - Call check_process_limit before allocating memory pages
    - Call check_process_limit before creating threads
    - Call check_process_limit before creating capabilities
    - Call check_process_limit before creating IPC ports
    - Call check_process_limit before creating address spaces
    - _Requirements: Req 12.2, Req 12.4_

  - [~] 9.8 Integrate accounting into allocation/deallocation paths
    - Call account_resource_allocation after successful allocation
    - Call account_resource_deallocation after successful deallocation
    - Ensure accounting is atomic and consistent
    - _Requirements: Req 13.1, Req 13.2, Req 13.3, Req 13.4_

- [~] 10. Checkpoint - Validate resource limits
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 11. Phase 6: Diagnostics and Observability
  - Add memory fragmentation metrics
  - Implement capability usage statistics
  - Add thread resource breakdown APIs
  - _Requirements: Req 14, Req 15, Req 16_

  - [~] 11.1 Add fragmentation metrics to PMM in kernel/src/mm/pmm.rs
    - Track largest_free_run during allocation/deallocation
    - Compute fragmentation_score based on free page distribution
    - Add get_memory_fragmentation_stats() -> FragmentationStats function
    - Update metrics after each allocation and deallocation
    - _Requirements: Req 14.1, Req 14.2, Req 14.3, Req 14.4_

  - [~] 11.2 Integrate fragmentation into memory pressure calculation
    - Modify check_memory_pressure_detailed to include fragmentation_score
    - Consider largest_free_run in pressure level calculation
    - _Requirements: Req 14.5_

  - [~] 11.3 Add capability usage statistics in kernel/src/cap.rs
    - Implement get_capability_usage_by_type() -> BTreeMap<ResourceType, usize>
    - Implement get_capability_graph_depth() -> usize (longest derivation chain)
    - Track creation, derivation, transfer, and revocation counts
    - _Requirements: Req 15.1, Req 15.2, Req 15.4_

  - [~] 11.4 Add capability statistics API in kernel/src/cap.rs
    - Add get_capability_statistics() -> CapabilityStatistics function
    - Include counts by type, graph depth, operation counts
    - _Requirements: Req 15.3, Req 15.5_

  - [~] 11.5 Implement thread resource breakdown in kernel/src/thread.rs
    - Add get_thread_resource_breakdown(thread_id) -> ThreadResourceBreakdown function
    - Track capabilities, address_spaces, ipc_ports, memory_pages per thread
    - Update breakdown in real-time as resources are allocated/freed
    - _Requirements: Req 16.1, Req 16.2, Req 16.4_

  - [~] 11.6 Implement system resource summary in kernel/src/thread.rs
    - Add get_system_resource_summary() -> SystemResourceSummary function
    - Aggregate all thread resource breakdowns
    - Include capability statistics and memory fragmentation
    - _Requirements: Req 16.5_

  - [~] 11.7 Add diagnostic logging for all validation failures
    - Log ValidationError with address and reason
    - Log LimitError with process ID and resource type
    - Log OOM conditions with memory pressure and process usage
    - Log cleanup leaks with resource details
    - _Requirements: Req 18.1, Req 18.2, Req 18.3, Req 18.4_

- [~] 12. Final checkpoint - Integration validation
  - Ensure all tests pass, ask the user if questions arise.

---

## Fase 6 — Syscall Hardening (Remoção de Falhas Sistêmicas)

- [x] 13. Phase 6 — Syscall Hardening
  - Auditar e eliminar assert! operacionais do syscall path
  - Introduzir taxonomia de erros SyscallError e SyscallContextError
  - Implementar contenção local de falhas de contexto
  - Integrar erros por subsistema na camada de normalização
  - _Requirements: Req 29, Req 37, Req 38, Req 39, Req 40, Req 41_

  - [x] 13.1 Auditar todos os assert! no syscall path e classificar
    - Mapear todos os `assert!`, `assert_eq!`, `debug_assert!` em: `syscall/mod.rs`, `process.rs`, `mm/pmm.rs`, `ipc.rs`
    - Classificar cada ocorrência como: (A) invariante de compilador/estrutural — manter, (B) condição operacional de runtime — substituir
    - Documentar resultado da auditoria como comentários inline antes de substituir
    - Confirmar que `const _: () = assert!(...)` em `idt.rs` e size checks em `interrupts/handlers.rs` são Classe A
    - _Requirements: Req 39.6, Req 39.7_

  - [x] 13.2 Definir SyscallError e SyscallContextError em kernel/src/syscall/mod.rs
    - Definir `enum SyscallError { AddressSpaceDrift, ProcessMetadataCorrupted, ThreadProcessMismatch, InvalidUserReturnAddress, AdmissionDenied, CapabilityRevokePartial, InvalidPointer, PermissionDenied, InternalInconsistency(&'static str) }`
    - Definir `enum SyscallContextError { ProcessContextMismatch, InvalidUserReturnAddress, MissingAddressSpace, ThreadMetadataDrift }`
    - Implementar `SyscallError::to_errno() -> u64` mapeando cada variante para errno POSIX-like
    - _Requirements: Req 37.1, Req 37.2, Req 37.6, Req 29.4_

  - [x] 13.3 Substituir assert! em ProtectedPml4Registry::insert (pmm.rs ~137)
    - Substituir `assert!(self.len < self.entries.len(), "protected PML4 registry exhausted")` por retorno de erro estruturado
    - Adicionar variante `RegistryExhausted` ao `ValidationError` existente (ou tipo equivalente)
    - Propagar erro ao caller com `error!` log incluindo capacidade atual
    - _Requirements: Req 39.1, Req 40.1_

  - [x] 13.4 Substituir assert! em verify_process_accounting (process.rs)
    - Localizar `assert!` em `verify_process_accounting` que é chamado em caminhos operacionais
    - Substituir por `Err(SyscallError::ProcessMetadataCorrupted)` com log estruturado
    - Garantir que o caller trata o erro e não propaga como panic
    - _Requirements: Req 39.2, Req 40.1_

  - [x] 13.5 Revisar e tratar debug_assert! em process.rs (thread registration)
    - Revisar `debug_assert!` nas linhas ~240, 264, 303, 521, 539 de `process.rs`
    - Para cada um: avaliar se a condição pode ocorrer em release por estado de runtime
    - Se sim: substituir por `error!` log + retorno de `SyscallContextError::ThreadMetadataDrift`
    - Se não (nunca ativo em release): adicionar comentário `// debug-only: [descrição da invariante]`
    - _Requirements: Req 39.4, Req 39.7_

  - [x] 13.6 Revisar e tratar debug_assert! em ipc.rs (validações ~479, 499)
    - Revisar `debug_assert!` nas linhas ~479 e 499 de `ipc.rs`
    - Aplicar mesma classificação da task 13.5
    - Se operacional: substituir por `Err(SyscallError::InternalInconsistency("ipc validation"))` + log
    - _Requirements: Req 39.5_

  - [x] 13.7 Implementar validate_syscall_context em kernel/src/syscall/mod.rs
    - Implementar `fn validate_syscall_context(thread_id: ThreadId, expected_pml4: u64) -> Result<SyscallContext, SyscallContextError>`
    - Verificar: thread existe, tem processo associado, PML4 do processo bate com `expected_pml4`, processo não está em estado `Dying`/`Dead`
    - Definir `struct SyscallContext { thread_id, process_id, pml4_phys }`
    - _Requirements: Req 38.1, Req 38.3_

  - [x] 13.8 Implementar contain_context_failure em kernel/src/syscall/mod.rs
    - Implementar `fn contain_context_failure(error: SyscallContextError, thread_id: ThreadId, process_id: Option<ProcessId>)`
    - Emitir `error!` log estruturado com: variante, thread_id, process_id, contexto
    - Chamar `transition_to_dying(pid, KillReason::FatalFault)` quando process_id disponível
    - Chamar `terminate_thread` quando apenas thread_id disponível
    - Garantir que kernel continua após contenção
    - _Requirements: Req 38.1, Req 38.2, Req 38.3, Req 38.4, Req 38.5, Req 38.6, Req 38.7_

  - [x] 13.9 Substituir log_panic! operacionais no syscall path por SyscallContextError
    - Identificar todos os `log_panic!` em `syscall/mod.rs` que tratam: contexto ausente, return address inválido, PML4 mismatch
    - Substituir por: `contain_context_failure(SyscallContextError::*, ...)` + retorno de `EINVAL`
    - Verificar que nenhum `log_panic!` operacional permanece no dispatcher
    - _Requirements: Req 29.2, Req 29.3, Req 38.6_

  - [x] 13.10 Integrar mapeamento de erros por subsistema na camada de normalização
    - No dispatcher de syscall, adicionar conversão de erros de MM → `SyscallError::AdmissionDenied` / `InvalidPointer`
    - Adicionar conversão de erros de Process → `SyscallError::ThreadProcessMismatch` / `ProcessMetadataCorrupted`
    - Adicionar conversão de erros de Cap → `SyscallError::CapabilityRevokePartial` / `PermissionDenied`
    - Garantir que nenhum erro de subsistema chega ao userspace sem passar pela normalização
    - _Requirements: Req 41.1, Req 41.2, Req 41.3, Req 41.4, Req 41.5, Req 41.6_

  - [x] 13.11 Adicionar testes de propriedade para o syscall hardening
    - Escrever property test: para qualquer `SyscallContextError`, kernel não entra em panic
    - Escrever property test: para qualquer `SyscallError`, `to_errno()` retorna valor válido (não zero, não overflow)
    - Escrever property test: após contenção local, processo está em `Dying` ou `Dead`, nunca `Running`
    - Escrever testes de exemplo: thread/process mismatch, PML4 mismatch, registry esgotado, ponteiro inválido
    - _Requirements: Req 37.3, Req 38.1, Req 38.5, Req 40.6_

  - [x] 13.12 Documentar assert! restantes como invariantes estruturais
    - Para cada `assert!` que permanece (Classe A), adicionar comentário: `// INVARIANT: [descrição] — structural, not operational`
    - Verificar que nenhum `assert!` sem comentário de invariante existe fora de `#[cfg(test)]` e `const` contexts
    - _Requirements: Req 39.6, Req 39.7_

- [ ] 14. Checkpoint — Validar Fase 6 Syscall Hardening
  - Verificar que nenhum `assert!` operacional permanece no syscall path
  - Verificar que todos os erros de contexto disparam contenção local, não panic
  - Verificar que `SyscallError::to_errno()` cobre todas as variantes
  - Executar testes de propriedade da task 13.11
  - _Requirements: Req 37, Req 38, Req 39, Req 40, Req 41_


---

## Architectural Migration Tasks (Invariant-Driven)

As tasks a seguir implementam o plano de migração arquitetural em 9 epics, seguindo a sequência de merge recomendada. Cada epic é independentemente mergeable.

### Fase 0 — Congelamento de Invariantes (Pré-requisito)

- [ ] A0. Criar documento de invariantes do kernel
  - _Requirements: Req 31_

  - [ ] A0.1 Criar KERNEL_INVARIANTS.md em kernel/
    - Documentar invariantes de memória: toda VA de userspace válida pertence ao contrato ABI; fault resolver não consulta quota/política global
    - Documentar invariantes de processo: accounting de memória é por processo; ação OOM terminal é por processo
    - Documentar invariantes de capabilities: revogação tem semântica explícita; callback é efeito colateral observável, não mecanismo de integridade
    - Documentar invariantes de concorrência: nenhum callback executa sob lock de registro; nenhum código segura registry lock e entra em função que pode recursar em outro registry relacionado
    - _Requirements: Req 31.1, Req 31.2_

  - [ ] A0.2 Documentar lock order global em KERNEL_INVARIANTS.md
    - Definir hierarquia: Scheduler(1) > ProcessRegistry(2) > Process(3) > AddressSpace/VMA(4) > PMM/VMM(5)
    - Definir: CapabilityRegistry(2b) > CapabilityObject(3b)
    - Definir: CallbackRegistry como snapshot-only (nunca segura durante invocação)
    - Para cada lock: documentar nível, quem pode adquirir, quais funções são proibidas sob esse lock
    - _Requirements: Req 28.1, Req 28.2_

  - [ ] A0.3 Documentar modelo de ownership de memória
    - MM: VMA lookup, page admission input, physical materialization, mapping
    - Process: ownership do address space, accounting agregado, lifecycle state, quota metadata
    - OOM: pressão global, seleção de vítima, acionar kill por processo
    - Capabilities: grafo de derivação, revoke state machine, callbacks como notificação
    - _Requirements: Req 30, Req 31.4_

### Epic 1 — Address Contract Unification

- [ ] B1. Unificar contrato de endereços de userspace no ABI
  - Definition of Done: Não existe mais nenhum caminho que valide userspace por KERNEL_BASE; syscalls usam só a API tipada; ABI e kernel compartilham o mesmo contrato sem duplicação lógica
  - _Requirements: Req 21, Req 32_

  - [ ] B1.1 Criar tipos UserVAddr e UserRange em shared/abi/src/lib.rs
    - Definir UserVAddr como newtype opaco sobre usize — só construível via validate_user_addr()
    - Definir UserRange como struct { base: UserVAddr, len: usize } — só construível via validate_user_range()
    - Garantir que ambos os tipos são #[repr(transparent)] e Copy
    - _Requirements: Req 21.3, Req 32.3_

  - [ ] B1.2 Adicionar UserAddressError e funções de validação em shared/abi/src/lib.rs
    - Definir enum UserAddressError { NonCanonical, BelowUserMin, AboveUserMax, Overflow, EmptyRange }
    - Implementar validate_user_addr(addr: usize) -> Result<UserVAddr, UserAddressError>
    - Implementar validate_user_range(base: usize, len: usize) -> Result<UserRange, UserAddressError>
    - Implementar validate_user_return_addr(addr: usize) -> Result<UserVAddr, UserAddressError>
    - _Requirements: Req 21.5, Req 21.6_

  - [ ] B1.3 Remover validate_user_space_bounds() baseada em KERNEL_BASE de kernel/src/mm/mod.rs
    - Substituir por chamada a atom_abi::validate_user_addr / validate_user_range
    - Atualizar todos os call sites que usavam validate_user_space_bounds
    - _Requirements: Req 21.2, Req 32.2_

  - [ ] B1.4 Migrar syscall layer para API tipada
    - Substituir validações de ponteiro userspace em kernel/src/syscall/mod.rs por validate_user_addr/validate_user_range
    - Garantir que handlers de syscall recebem UserVAddr/UserRange, não usize cru
    - _Requirements: Req 21.3, Req 21.4_

  - [ ] B1.5 Eliminar funções duplicadas de validação de userspace
    - Buscar e remover qualquer comparação com KERNEL_BASE para validação de ponteiro userspace
    - Verificar que não existe lógica de validação de VA duplicada entre ABI e kernel MM
    - _Requirements: Req 21.7, Req 32.5_

### Epic 2 — Fault Path Rework

- [ ] B2. Tipar resultado de page fault e separar camadas
  - Definition of Done: Resolver de fault não contém policy global nem consultas profundas de registry; resultado é sempre FaultResult tipado
  - _Requirements: Req 22, Req 23_

  - [ ] B2.1 Definir FaultError enum em kernel/src/mm/vma.rs
    - Definir enum FaultError { NoVma, AccessViolation, WriteToReadonly, ExecViolation, AddressNotUser, QuotaExceeded, PhysicalAllocFailed, MapFailed, ProcessContextMissing, InvariantBroken(&'static str) }
    - Definir type FaultResult = Result<FaultResolved, FaultError>
    - Definir struct FaultResolved (unit struct)
    - _Requirements: Req 22.1, Req 22.2_

  - [ ] B2.2 Substituir retorno bool por FaultResult em handle_page_fault
    - Alterar assinatura de handle_page_fault para retornar FaultResult
    - Substituir todos os return false por return Err(FaultError::*) com variante apropriada
    - Substituir return true por return Ok(FaultResolved)
    - _Requirements: Req 22.1, Req 22.3_

  - [ ] B2.3 Criar camada classify_fault em kernel/src/mm/vma.rs
    - Implementar fn classify_fault(pml4_phys: usize, fault_addr: usize, error_code: u64) -> Result<FaultClassified, FaultError>
    - Definir struct FaultClassified { vma: Vma, page_addr: usize, access_type: FaultAccessType }
    - classify_fault não deve consultar quota, OOM registry, ou process identity
    - _Requirements: Req 23.1, Req 23.2_

  - [ ] B2.4 Criar camada admit_memory_growth em kernel/src/mm/policy.rs
    - Implementar fn admit_memory_growth(ctx: &ProcessMemoryContext, pages_needed: usize) -> Result<(), FaultError>
    - Definir struct ProcessMemoryContext { process_id: ProcessId, resident_pages: usize, limit_pages: usize }
    - admit_memory_growth não deve alocar memória física
    - _Requirements: Req 23.1, Req 23.3_

  - [ ] B2.5 Criar camada materialize_fault em kernel/src/mm/vma.rs
    - Implementar fn materialize_fault(pml4_phys: usize, vma: &Vma, page_addr: usize) -> Result<FaultResolved, FaultError>
    - materialize_fault não deve chamar get_process_memory_usage() nem process_id_for_pml4()
    - materialize_fault recebe ProcessMemoryContext como parâmetro explícito
    - _Requirements: Req 23.1, Req 23.4, Req 23.5_

  - [ ] B2.6 Remover consulta de quota/OOM de resolve_anon_fault
    - Remover chamada a get_process_memory_usage() do hot path de resolve_anon_fault
    - Remover process_id_for_pml4() do hot path de resolve_anon_fault
    - Garantir que resolver depende só de VMA + contexto explícito
    - _Requirements: Req 23.6_

  - [ ] B2.7 Atualizar caller de handle_page_fault para inspecionar FaultResult
    - Atualizar kernel/src/interrupts/handlers.rs para tratar FaultResult tipado
    - Garantir que caller não faz heurística textual para decidir ação
    - Incluir FaultError variant em mensagens de log
    - _Requirements: Req 22.4, Req 22.5_

### Epic 3 — Process-Centric Memory Accounting

- [ ] B3. Unificar accounting de memória por processo
  - Definition of Done: OOM consulta accounting agregado, não reconstrói memória do processo sob pressão
  - _Requirements: Req 33_

  - [ ] B3.1 Definir fonte única de accounting por processo em kernel/src/process.rs
    - Adicionar campo resident_pages: AtomicUsize ao Process struct
    - Garantir que este campo é a fonte autoritativa de resident pages por processo
    - _Requirements: Req 33.1_

  - [ ] B3.2 Atualizar accounting em eventos de map/unmap/materialization
    - Chamar process.resident_pages.fetch_add(1) em cada materialização bem-sucedida
    - Chamar process.resident_pages.fetch_sub(1) em cada unmap
    - Garantir que updates são atômicos
    - _Requirements: Req 33.4_

  - [ ] B3.3 Implementar get_process_memory_snapshot() sem lock recursion
    - Implementar fn get_process_memory_snapshot() -> Vec<ProcessMemorySnapshot>
    - Definir struct ProcessMemorySnapshot { process_id, resident_pages, limit_pages, state }
    - Snapshot deve ser obtido sem segurar registry lock durante consultas profundas
    - _Requirements: Req 33.2, Req 33.3_

  - [ ] B3.4 Refatorar count_processes_over_limit para usar snapshot
    - Substituir implementação atual (que segura registry lock durante consultas profundas) por snapshot + análise
    - Garantir que count_processes_over_limit nunca segura registry lock durante get_process_memory_usage
    - _Requirements: Req 33.5_

### Epic 4 — OOM Redesign

- [ ] B4. Redesenhar OOM para modelo 100% por processo
  - Definition of Done: Após OOM kill, o processo inteiro sai do sistema de forma determinística; count_processes_over_limit não segura lock aninhado
  - _Requirements: Req 24, Req 25_

  - [ ] B4.1 Adicionar ProcessState ao Process struct em kernel/src/process.rs
    - Definir enum ProcessState { Running, Exiting(ExitReason), Dying(KillReason), Dead }
    - Definir enum KillReason { Oom, FatalFault, CapabilityViolation }
    - Definir enum ExitReason { Normal(i32), Signal(u32) }
    - Adicionar state: ProcessState ao Process struct
    - _Requirements: Req 25.1, Req 25.2, Req 25.3_

  - [ ] B4.2 Implementar transition_to_dying em kernel/src/process.rs
    - Implementar fn transition_to_dying(process_id: ProcessId, reason: KillReason) -> Result<(), ProcessError>
    - Transição deve ser atômica — prevenir transições concorrentes
    - Implementar fn is_process_dying(process_id: ProcessId) -> bool
    - Implementar fn get_process_state(process_id: ProcessId) -> Option<ProcessState>
    - _Requirements: Req 25.5, Req 25.6_

  - [ ] B4.3 Bloquear novos faults para processo em estado Dying
    - Em classify_fault ou admit_memory_growth, verificar is_process_dying(process_id)
    - Se processo está Dying, retornar FaultError::ProcessContextMissing imediatamente
    - _Requirements: Req 24.3, Req 25.4_

  - [ ] B4.4 Mudar seleção de vítima OOM para processo em kernel/src/mm/oom.rs
    - Substituir seleção por thread por seleção por processo usando get_process_memory_snapshot()
    - Usar ProcessMemorySnapshot para comparar resident_pages por processo
    - _Requirements: Req 24.1_

  - [ ] B4.5 Implementar oom_kill_process em kernel/src/mm/oom.rs
    - Chamar transition_to_dying(victim.process_id, KillReason::Oom)
    - Encerrar todas as threads do processo via get_process_threads + terminate_thread
    - Chamar cleanup_process_resources(victim.process_id) para teardown unificado
    - Retornar OomResult::Killed { process_id, pages_freed }
    - _Requirements: Req 24.2, Req 24.4, Req 24.5, Req 24.6_

  - [ ] B4.6 Implementar cleanup_process_resources em kernel/src/process.rs
    - Implementar fn cleanup_process_resources(process_id: ProcessId) -> CleanupResult
    - Liberar: address space, todas as capabilities, todas as IPC ports, shared memory
    - Teardown deve ser idempotente
    - Após teardown, processo deve estar em estado Dead
    - _Requirements: Req 34.1, Req 34.2, Req 34.3, Req 34.4_

### Epic 5 — Lock Discipline

- [ ] B5. Instituir hierarquia global de locks
  - Definition of Done: Nenhuma função crítica entra em registry correlato segurando lock incompatível; toda função sensível tem comentário de lock contract
  - _Requirements: Req 28_

  - [ ] B5.1 Inventariar todos os locks estruturais do kernel
    - Listar todos os Mutex/RwLock em: process.rs, vma.rs, cap.rs, oom.rs, ipc.rs, thread.rs
    - Para cada lock: identificar nível na hierarquia, quem o adquire, quais funções são chamadas sob ele
    - _Requirements: Req 28.1_

  - [ ] B5.2 Adicionar comentários de lock contract em funções críticas
    - Adicionar comentário "// LOCK CONTRACT: holds X, must not acquire Y" em funções que adquirem locks estruturais
    - Priorizar: count_processes_over_limit, handle_page_fault, revoke_capability, oom_kill
    - _Requirements: Req 28.5_

  - [ ] B5.3 Corrigir count_processes_over_limit para não segurar registry lock durante consultas profundas
    - Verificar que a implementação usa snapshot (B3.4) e não segura PROCESS_REGISTRY durante get_process_memory_usage
    - _Requirements: Req 28.4_

  - [ ] B5.4 Trocar scans sob lock por snapshot iteration em oom.rs e vma.rs
    - Identificar todos os lugares onde um lock é segurado durante iteração profunda
    - Substituir por: coletar keys/snapshot sob lock, liberar lock, processar snapshot
    - _Requirements: Req 28.4_

  - [ ] B5.5 Verificar conformidade de oom.rs, vma.rs, cap.rs com lock order formal
    - Revisar cada módulo contra a hierarquia documentada em KERNEL_INVARIANTS.md
    - Corrigir qualquer violação encontrada
    - _Requirements: Req 28.6_

### Epic 6 — Capability Revocation Rebuild

- [ ] B6. Reescrever revogação de capability com semântica explícita
  - Definition of Done: Não existe árvore parcialmente revogada sem surfacing explícito; let _ = self.revoke(...) não existe no código
  - _Requirements: Req 26_

  - [ ] B6.1 Definir semântica two-phase e tipos em kernel/src/cap.rs
    - Definir enum RevokeError { ChildRevokeFailed(CapHandle), Busy(CapHandle), InvariantBroken(&'static str) }
    - Definir struct RevokeReport { revoked: Vec<CapHandle>, failed: Vec<(CapHandle, RevokeError)> }
    - Adicionar estado Revoking ao capability (campo ou enum separado)
    - _Requirements: Req 26.1, Req 26.5_

  - [ ] B6.2 Implementar mark_cap_revoking em kernel/src/cap.rs
    - Implementar fn mark_cap_revoking(handle: CapHandle) -> Result<Vec<CapHandle>, RevokeError>
    - Marcar capability como Revoking, retornar lista de filhos
    - Impedir uso novo de capability em estado Revoking
    - _Requirements: Req 26.1, Req 26.2_

  - [ ] B6.3 Implementar revoke_capability_two_phase em kernel/src/cap.rs
    - Fase 1: marcar handle e todos os filhos como Revoking
    - Fase 2: commit — remover da árvore, registrar em RevokeReport
    - Propagar falhas de descendentes em RevokeReport.failed
    - _Requirements: Req 26.3, Req 26.4_

  - [ ] B6.4 Eliminar descarte de erro em revoke recursivo
    - Buscar e eliminar todos os let _ = self.revoke(...) ou equivalentes
    - Substituir por propagação explícita em RevokeReport
    - _Requirements: Req 26.6_

  - [ ] B6.5 Verificar que estado final da árvore é observável e consistente
    - Após qualquer revogação (completa ou parcial), o estado da árvore deve ser consultável
    - Adicionar teste que verifica consistência após revogação parcial
    - _Requirements: Req 26.7_

### Epic 7 — Callback Isolation

- [ ] B7. Tirar callbacks de revogação de dentro do lock
  - Definition of Done: Nenhum callback roda sob REVOCATION_CALLBACKS lock; documentação casa com comportamento real
  - _Requirements: Req 27_

  - [ ] B7.1 Refatorar invoke_revocation_callbacks para snapshot antes da invocação
    - Adquirir REVOCATION_CALLBACKS lock, clonar callbacks relevantes, liberar lock
    - Invocar callbacks fora do lock global
    - _Requirements: Req 27.1, Req 27.2, Req 27.3_

  - [ ] B7.2 Atualizar documentação de callbacks para refletir comportamento real
    - Remover qualquer promessa de captura/continuidade de panic em callback
    - Documentar explicitamente: "panic em callback é bug fatal do kernel em ambiente no_std"
    - _Requirements: Req 27.4, Req 27.5_

  - [ ] B7.3 Definir política de callbacks: trusted, panic fatal
    - Adicionar comentário em register_revocation_callback: callbacks devem ser escritos para não panicking
    - Documentar que não há mecanismo de isolamento de panic em no_std
    - _Requirements: Req 27.4_

  - [ ] B7.4 Verificar que nenhum callback executa sob lock estrutural
    - Revisar todos os call sites de invoke_revocation_callbacks
    - Garantir que em nenhum caso o lock é segurado durante a invocação
    - _Requirements: Req 27.6_

### Epic 8 — Operational Error Handling Cleanup

- [ ] B8. Remover assert!/panic! de caminhos operacionais
  - Definition of Done: Erro de processo/userspace não derruba kernel inteiro por padrão; assert! reservado para corrupção estrutural real
  - _Requirements: Req 29_

  - [ ] B8.1 Remover assert_eq! de syscall hot path em kernel/src/syscall/mod.rs
    - Identificar todos os assert!/assert_eq! no dispatcher e handlers de syscall
    - Substituir por retorno de erro estruturado (EINVAL, EPERM, etc.)
    - _Requirements: Req 29.1_

  - [ ] B8.2 Definir SyscallContextError em kernel/src/syscall/mod.rs
    - Definir enum SyscallContextError { ProcessContextMismatch, InvalidUserReturnAddress, MissingAddressSpace, ThreadMetadataDrift }
    - _Requirements: Req 29.4_

  - [ ] B8.3 Substituir log_panic! operacional por erro estruturado
    - Identificar todos os log_panic! que tratam erros operacionais (contexto ausente, return address inválido, PML4 mismatch recuperável)
    - Substituir por SyscallContextError + teardown local do processo
    - _Requirements: Req 29.2, Req 29.3_

  - [ ] B8.4 Implementar teardown local de processo para falhas de contexto
    - Quando SyscallContextError ocorre, chamar transition_to_dying(process_id, KillReason::FatalFault)
    - Garantir que kernel continua rodando para outros processos
    - _Requirements: Req 29.3, Req 29.6_

  - [ ] B8.5 Reservar panic para corrupção estrutural real
    - Revisar todos os panic!/assert! restantes
    - Documentar cada um com comentário: "// INVARIANT: [descrição da invariante fatal]"
    - _Requirements: Req 29.5_

### Epic 9 — Subsystem Interface Cleanup

- [ ] B9. Criar interfaces de ownership claras entre subsistemas
  - Definition of Done: Cada subsistema tem responsabilidade única documentada; nenhum subsistema acessa estado interno de outro sem interface definida
  - _Requirements: Req 30, Req 35, Req 36_

  - [ ] B9.1 Verificar que MM não acessa estado de processo diretamente
    - Revisar kernel/src/mm/ para chamadas diretas a PROCESS_REGISTRY
    - Substituir por chamadas a interfaces de processo (get_process_state, get_process_memory_context)
    - _Requirements: Req 30.1_

  - [ ] B9.2 Verificar que OOM usa apenas interfaces de processo e MM
    - Revisar kernel/src/mm/oom.rs para acesso direto a estruturas internas
    - Garantir que OOM usa: get_process_memory_snapshot(), transition_to_dying(), cleanup_process_resources()
    - _Requirements: Req 30.3_

  - [ ] B9.3 Verificar que callbacks de capability são notificações, não mecanismos de integridade
    - Revisar todos os usos de register_revocation_callback
    - Garantir que nenhum callback é usado para garantir atomicidade de revogação
    - _Requirements: Req 30.4_

  - [ ] B9.4 Executar checklist de critérios de pronto do sistema
    - Verificar: "Userspace pointer válido é definido onde?" → única resposta: ABI
    - Verificar: "Quem decide quota/política de memória?" → única resposta: camada de admission
    - Verificar: "OOM mata quem?" → única resposta: processo
    - Verificar: "Revogação parcial pode passar silenciosa?" → única resposta: não
    - Verificar: "Callback roda sob lock?" → única resposta: não
    - Verificar: "Erro operacional panica kernel?" → única resposta: não, salvo corrupção estrutural
    - Verificar: "Existe lock order documentado e seguido?" → única resposta: sim, em KERNEL_INVARIANTS.md
    - _Requirements: Req 36_

  - [ ] B9.5 Final checkpoint — todos os testes passam com migração completa
    - Executar suite completa de testes
    - Verificar que nenhuma regressão foi introduzida
    - _Requirements: Req 35.4_

## Notes

- Camada 1 (Fases 1–6): melhorias incrementais, abordagem defense-in-depth, sem mudanças arquiteturais
- Camada 2 (Fases 0–9): migração arquitetural dirigida por invariantes, sequência de merge controlada
- Cada epic da Camada 2 é independentemente mergeable e tem Definition of Done verificável por inspeção de código
- A Fase 0 (KERNEL_INVARIANTS.md) é pré-requisito para todos os epics da Camada 2
- Os epics B1–B9 seguem a sequência de merge recomendada: address contract → fault result → fault split → accounting → OOM → locks → cap revoke → callbacks → cleanup
