# Implementation Plan: Kernel Robustness Improvements

## Overview

This implementation plan addresses critical robustness gaps in the Atom kernel's memory management, error handling, resource accounting, and cleanup logic. The improvements are organized into six phases that build incrementally, with each phase validating core functionality through code before proceeding.

The implementation follows a defense-in-depth approach: validate early (alignment, bounds), fail gracefully (return errors instead of panic), track resources explicitly (accounting), and provide visibility (metrics, logging). All improvements integrate with existing subsystems without requiring architectural changes.

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

## Notes

- Each phase builds on the previous phase and validates functionality incrementally
- All tasks reference specific requirements for traceability
- Checkpoints ensure incremental validation and allow for user feedback
- Implementation uses Rust and integrates with existing kernel subsystems
- No architectural changes required - all improvements are additive
- Focus on defense-in-depth: validate early, fail gracefully, track explicitly, provide visibility
