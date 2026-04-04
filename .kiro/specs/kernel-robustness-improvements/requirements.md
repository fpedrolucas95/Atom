# Requirements Document: Kernel Robustness Improvements

## Introduction

This document specifies the functional and non-functional requirements for improving the robustness of the Atom kernel's memory management, error handling, resource accounting, and cleanup logic. The improvements address critical gaps that can lead to system panics, resource leaks, and undefined behavior under stress conditions.

The requirements are derived from comprehensive codebase analysis that identified four critical issue categories: memory safety validation gaps, capability system complexity, OOM management limitations, and thread cleanup coordination challenges. The solution consolidates improvements across six focus areas while maintaining compatibility with existing kernel subsystems.

## Glossary

- **PMM**: Physical Memory Manager - manages allocation of physical memory pages
- **VMM**: Virtual Memory Manager - manages virtual address spaces and page tables
- **VMA**: Virtual Memory Area - tracks virtual memory regions per address space
- **PML4**: Page Map Level 4 - top-level page table structure in x86_64 paging
- **OOM**: Out Of Memory - condition when physical memory is exhausted
- **Capability**: Unforgeable token granting access to a kernel resource
- **Thread**: Execution context with its own stack and register state
- **Process**: Collection of threads sharing an address space and resources
- **Validation_Layer**: Component that validates inputs before operations
- **Resource_Accounting**: Component that tracks resource usage per process
- **Cleanup_Coordinator**: Component that manages thread resource cleanup
- **Memory_Pressure**: Metric indicating how close the system is to OOM

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
4. THE OOM_Manager SHALL complete victim selection in O(n) time where n is the number of threads
5. THE kernel SHALL cache frequently computed values (e.g., memory pressure) to avoid repeated computation
