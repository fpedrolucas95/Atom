# Design Document: Kernel Robustness Improvements

## Overview

This design addresses critical robustness gaps in the Atom kernel's memory management, error handling, resource accounting, and cleanup logic identified through comprehensive codebase analysis. The kernel has mature subsystems (~70% complete) but lacks hardening in several key areas that can lead to panics, resource leaks, and undefined behavior under stress.

The analysis revealed four critical issue categories:
1. **Memory Safety**: Page alignment validation gaps, PML4 protection enforcement issues, phys_to_virt_ptr early boot safety concerns
2. **Capability System**: Complex transfer_capability rollback logic, audit log overflow handling, missing revocation callbacks
3. **OOM Management**: System halts when no victim found (needs graceful degradation), no per-process memory limits
4. **Thread Cleanup**: Complex multi-phase cleanup with potential resource leaks across capabilities, address spaces, and IPC ports

This design consolidates improvements across six focus areas: memory safety validation, comprehensive error propagation, resource limits and accounting, OOM management, thread cleanup simplification, and diagnostics/observability. The approach follows defense-in-depth: validate early (alignment, bounds), fail gracefully (return errors instead of panic), track resources explicitly (accounting), and provide visibility (metrics, logging). All improvements integrate with existing subsystems without requiring architectural changes.

## Architecture

```mermaid
graph TB
    subgraph "Memory Safety Layer"
        A1[Page Alignment Validation]
        A2[PML4 Protection Enforcement]
        A3[phys_to_virt_ptr Early Boot Safety]
    end

    subgraph "Error Handling Layer"
        B1[Comprehensive Error Propagation]
        B2[Rollback Mechanisms]
        B3[Graceful Degradation]
    end

    subgraph "Resource Management Layer"
        C1[Per-Process Memory Limits]
        C2[Per-Thread Resource Limits]
        C3[Capability Table Bounds]
    end

    subgraph "OOM Management"
        D1[Memory Pressure Detection]
        D2[Graceful Degradation Strategies]
        D3[Process Memory Limits Integration]
    end

    subgraph "Thread Cleanup"
        E1[Simplified Multi-Phase Logic]
        E2[Resource Leak Detection]
        E3[Cleanup Validation]
    end

    subgraph "Diagnostics Layer"
        F1[Memory Fragmentation Metrics]
        F2[Capability Usage Statistics]
        F3[Thread Resource Accounting]
    end

    A1 --> B1
    A2 --> B1
    A3 --> B1
    B1 --> C1
    B2 --> C2
    B3 --> C3
    C1 --> D1
    C2 --> D2
    C3 --> D3
    D1 --> E1
    D2 --> E2
    D3 --> E3
    E1 --> F1
    E2 --> F2
    E3 --> F3


## Main Algorithm/Workflow

```mermaid
sequenceDiagram
    participant Syscall as Syscall Entry
    participant Validator as Validation Layer
    participant Allocator as Resource Allocator
    participant Accounting as Resource Accounting
    participant Cleanup as Cleanup Handler
    participant Diagnostics as Diagnostics

    Syscall->>Validator: Validate input (alignment, bounds, limits)
    alt Validation fails
        Validator-->>Syscall: Return error code
    else Validation succeeds
        Validator->>Allocator: Allocate resource
        alt Allocation fails
            Allocator->>Accounting: Check limits/pressure
            Accounting-->>Allocator: Limit exceeded or OOM
            Allocator-->>Syscall: Return ENOMEM
        else Allocation succeeds
            Allocator->>Accounting: Update counters
            Accounting->>Diagnostics: Record metrics
            Allocator-->>Syscall: Return success
        end
    end

    Note over Cleanup: On thread/process exit
    Cleanup->>Accounting: Enumerate owned resources
    Accounting->>Cleanup: Resource list
    Cleanup->>Allocator: Free each resource
    Allocator->>Accounting: Update counters
    Accounting->>Diagnostics: Record cleanup metrics
    Cleanup->>Diagnostics: Validate zero leaks
```

## Components and Interfaces

### Component 1: Memory Safety Validation Layer

**Purpose**: Validate all memory operations before execution to prevent undefined behavior

**Interface**:
```rust
// Page alignment validation
fn validate_page_alignment(addr: usize) -> Result<(), ValidationError>;
fn validate_page_range(start: usize, end: usize) -> Result<(), ValidationError>;

// PML4 protection enforcement
fn validate_pml4_access(pml4_phys: usize, operation: PML4Operation) -> Result<(), ValidationError>;
fn is_pml4_protected(pml4_phys: usize) -> bool;

// Early boot safety for phys_to_virt_ptr
fn phys_to_virt_ptr_safe(phys: usize) -> Result<usize, ValidationError>;
```

**Responsibilities**:
- Validate page alignment before all map/unmap operations
- Enforce PML4 protection (prevent freeing active page tables)
- Provide safe phys_to_virt_ptr that checks HIGHER_HALF_READY flag
- Return errors instead of panicking on invalid input

**Current Issues**:
- `vm::map_page_in_pml4` checks alignment but some callers don't validate before calling
- `pmm::free_page` doesn't check if page is an active PML4 (could corrupt page tables)
- `phys_to_virt_ptr` doesn't validate that HIGHER_HALF_READY is set before using higher-half offset

### Component 2: Capability System Hardening

**Purpose**: Improve capability transfer rollback and audit log management

**Interface**:
```rust
// Enhanced transfer with atomic rollback
fn transfer_capability_atomic(
    cap_handle: CapHandle,
    source: ThreadId,
    target: ThreadId
) -> Result<(), CapError>;

// Audit log with bounded size
fn log_audit_bounded(entry: AuditLogEntry) -> Result<(), AuditError>;
fn get_audit_stats() -> AuditStats;

// Revocation callbacks
fn register_revocation_callback(
    resource_type: ResourceType,
    callback: fn(CapHandle)
) -> Result<(), CapError>;
```

**Responsibilities**:
- Implement atomic capability transfer with complete rollback on failure
- Bound audit log size with configurable eviction policy
- Support revocation callbacks for resource cleanup
- Track capability transfer failures for diagnostics

**Current Issues**:
- `transfer_capability` has complex rollback logic that may leave inconsistent state on partial failure
- Audit log is unbounded VecDeque (MAX_AUDIT_LOG_ENTRIES=1000 but no overflow handling)
- No revocation callbacks - resources may leak when capabilities are revoked

### Component 3: OOM Management with Graceful Degradation

**Purpose**: Handle out-of-memory conditions without system halt

**Interface**:
```rust
// Enhanced OOM killer with fallback strategies
fn oom_kill_with_fallback() -> OomResult;
fn try_reclaim_memory(strategy: ReclaimStrategy) -> Result<usize, OomError>;

// Per-process memory limits
fn set_process_memory_limit(process_id: ProcessId, limit_pages: usize) -> Result<(), OomError>;
fn get_process_memory_usage(process_id: ProcessId) -> Option<MemoryUsage>;

// Memory pressure detection
fn check_memory_pressure_detailed() -> MemoryPressureInfo;
```

**Responsibilities**:
- Implement graceful degradation when no OOM victim found
- Support per-process memory limits with enforcement
- Provide multiple reclaim strategies (cache eviction, swap, etc.)
- Detailed memory pressure reporting for proactive management

**Current Issues**:
- `oom_kill()` calls `log_panic!` when no victim found - system halts instead of degrading gracefully
- No per-process memory limits - OOM killer only considers total system memory
- Only one reclaim strategy (kill largest process) - no cache eviction or other alternatives

### Component 4: Thread Cleanup Simplification

**Purpose**: Simplify multi-phase thread cleanup to prevent resource leaks

**Interface**:
```rust
// Unified cleanup coordinator
fn cleanup_thread_resources(thread_id: ThreadId) -> CleanupResult;

// Resource enumeration
fn enumerate_thread_resources(thread_id: ThreadId) -> ThreadResources;

// Cleanup validation
fn validate_cleanup_complete(thread_id: ThreadId) -> Result<(), CleanupError>;

// Leak detection
fn detect_resource_leaks(thread_id: ThreadId) -> Vec<LeakedResource>;
```

**Responsibilities**:
- Coordinate cleanup across capabilities, address spaces, IPC ports
- Enumerate all resources owned by a thread before cleanup
- Validate cleanup completion with leak detection
- Provide rollback for partial cleanup failures

**Current Issues**:
- Thread cleanup is scattered across multiple modules (cap.rs, addrspace.rs, ipc.rs)
- Each module has its own `CLEANED_THREAD_*` tracking set - complex coordination
- No unified resource enumeration before cleanup
- No validation that cleanup completed successfully

### Component 5: Resource Limits and Accounting

**Purpose**: Track and enforce resource limits per-process and per-thread

**Interface**:
```rust
// Per-process limits
struct ProcessLimits {
    max_memory_pages: usize,
    max_threads: usize,
    max_capabilities: usize,
    max_ipc_ports: usize,
}

fn set_process_limits(process_id: ProcessId, limits: ProcessLimits) -> Result<(), LimitError>;
fn check_process_limit(process_id: ProcessId, resource: ResourceType) -> Result<(), LimitError>;

// Resource accounting
fn account_resource_allocation(process_id: ProcessId, resource: ResourceType) -> Result<(), AccountingError>;
fn account_resource_deallocation(process_id: ProcessId, resource: ResourceType);
fn get_process_resource_usage(process_id: ProcessId) -> ResourceUsage;
```

**Responsibilities**:
- Define and enforce per-process resource limits
- Track resource allocation/deallocation in real-time
- Prevent resource exhaustion attacks
- Provide resource usage statistics for monitoring

**Current Issues**:
- No per-process memory limits (only global free page tracking)
- No per-thread resource limits
- Capability table is unbounded BTreeMap - no size limit
- No accounting of resources per-process (only global counters)

### Component 6: Diagnostics and Observability

**Purpose**: Provide visibility into kernel resource usage and health

**Interface**:
```rust
// Memory fragmentation metrics
fn get_memory_fragmentation_stats() -> FragmentationStats;
fn get_largest_free_run() -> usize;

// Capability usage statistics
fn get_capability_usage_by_type() -> BTreeMap<ResourceType, usize>;
fn get_capability_graph_depth() -> usize;

// Thread resource accounting
fn get_thread_resource_breakdown(thread_id: ThreadId) -> ThreadResourceBreakdown;
fn get_system_resource_summary() -> SystemResourceSummary;
```

**Responsibilities**:
- Track memory fragmentation and largest free runs
- Provide capability usage statistics by type
- Report per-thread resource accounting
- Generate system-wide resource summaries

**Current Issues**:
- PMM tracks LARGEST_FREE_RUN but no fragmentation metrics
- Capability stats only count by type - no graph depth or usage patterns
- No per-thread resource breakdown (only global counters)

## Data Models

### Model 1: ValidationError

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationError {
    Unaligned { addr: usize, required_alignment: usize },
    OutOfBounds { addr: usize, min: usize, max: usize },
    ProtectedResource { resource_id: u64 },
    NotInitialized,
    InvalidSize { size: usize, max_size: usize },
}
```

**Validation Rules**:
- All addresses must be page-aligned (4096 bytes)
- Addresses must be within valid ranges (user space < 0x0000_8000_0000_0000)
- Protected resources (active PML4s) cannot be freed
- Size must be non-zero and within maximum bounds

### Model 2: OomResult

```rust
#[derive(Debug, Clone)]
enum OomResult {
    Killed { 
        tid: ThreadId, 
        name: &'static str, 
        pages_freed: usize 
    },
    NoVictim { 
        reason: NoVictimReason,
        fallback_action: FallbackAction,
    },
    Reclaimed { 
        strategy: ReclaimStrategy, 
        pages_freed: usize 
    },
}

#[derive(Debug, Clone, Copy)]
enum NoVictimReason {
    NoUserProcesses,
    AllProcessesBelowMinimum,
    SystemReserved,
}

#[derive(Debug, Clone, Copy)]
enum FallbackAction {
    DenyAllocation,
    KillOldestProcess,
    EnterEmergencyMode,
}
```

**Validation Rules**:
- OOM killer must never kill kernel threads
- Must attempt reclaim strategies before declaring NoVictim
- Must log detailed reason when no victim found
- Must specify fallback action instead of panicking

### Model 3: CleanupResult

```rust
#[derive(Debug, Clone)]
struct CleanupResult {
    capabilities_revoked: usize,
    address_spaces_destroyed: usize,
    ipc_ports_closed: usize,
    physical_pages_freed: usize,
    leaks_detected: Vec<LeakedResource>,
    errors: Vec<CleanupError>,
}

#[derive(Debug, Clone)]
enum LeakedResource {
    Capability(CapHandle),
    AddressSpace(AddressSpaceId),
    IpcPort(PortId),
    PhysicalPage(usize),
}

#[derive(Debug, Clone)]
enum CleanupError {
    ResourceNotFound { resource_type: &'static str, id: u64 },
    PermissionDenied { resource_type: &'static str, id: u64 },
    PartialCleanup { completed: usize, failed: usize },
}
```

**Validation Rules**:
- All resources must be enumerated before cleanup
- Cleanup must be idempotent (safe to call multiple times)
- Leaks must be logged and tracked for debugging
- Errors must not prevent cleanup of remaining resources

### Model 4: ResourceLimits

```rust
#[derive(Debug, Clone, Copy)]
struct ResourceLimits {
    memory_pages: Limit,
    threads: Limit,
    capabilities: Limit,
    ipc_ports: Limit,
    address_spaces: Limit,
}

#[derive(Debug, Clone, Copy)]
struct Limit {
    current: usize,
    maximum: usize,
    soft_limit: usize,
    hard_limit: usize,
}

impl Limit {
    fn check_allocation(&self) -> Result<(), LimitError> {
        if self.current >= self.hard_limit {
            Err(LimitError::HardLimitExceeded)
        } else if self.current >= self.soft_limit {
            log_warn!("Soft limit exceeded: {}/{}", self.current, self.soft_limit);
            Ok(())
        } else {
            Ok(())
        }
    }
}
```

**Validation Rules**:
- Soft limit triggers warnings but allows allocation
- Hard limit prevents allocation and returns error
- Current usage must never exceed hard limit
- Limits must be enforced atomically

### Model 5: MemoryPressureInfo

```rust
#[derive(Debug, Clone, Copy)]
struct MemoryPressureInfo {
    level: MemoryPressure,
    free_pages: usize,
    total_pages: usize,
    free_percent: usize,
    largest_free_run: usize,
    fragmentation_score: f32,
    processes_over_limit: usize,
}

impl MemoryPressureInfo {
    fn should_trigger_oom(&self) -> bool {
        matches!(self.level, MemoryPressure::Oom) 
            || (self.free_pages < 256 && self.largest_free_run < 64)
    }
    
    fn should_reclaim(&self) -> bool {
        matches!(self.level, MemoryPressure::Critical | MemoryPressure::Oom)
    }
}
```

**Validation Rules**:
- Pressure level must be computed from multiple metrics (free pages, fragmentation, limits)
- Must consider both absolute free pages and largest contiguous run
- Must track processes exceeding their memory limits

## Algorithmic Pseudocode

### Main Processing Algorithm: Resource Allocation with Validation

```pascal
ALGORITHM allocate_resource_with_validation(process_id, resource_type, size)
INPUT: process_id of type ProcessId, resource_type, size in bytes
OUTPUT: result of type Result<ResourceHandle, AllocationError>

BEGIN
  // Step 1: Validate input parameters
  ASSERT process_id IS valid AND size > 0
  
  IF NOT is_page_aligned(size) THEN
    RETURN Error(ValidationError::Unaligned)
  END IF
  
  // Step 2: Check resource limits
  limits ← get_process_limits(process_id)
  current_usage ← get_resource_usage(process_id, resource_type)
  
  IF current_usage >= limits.hard_limit THEN
    RETURN Error(LimitError::HardLimitExceeded)
  END IF
  
  IF current_usage >= limits.soft_limit THEN
    log_warning("Soft limit exceeded", process_id, resource_type)
  END IF
  
  // Step 3: Check memory pressure
  pressure ← check_memory_pressure_detailed()
  
  IF pressure.should_trigger_oom() THEN
    reclaim_result ← try_reclaim_memory(ReclaimStrategy::Aggressive)
    
    IF reclaim_result IS Error THEN
      RETURN Error(AllocationError::OutOfMemory)
    END IF
  END IF
  
  // Step 4: Attempt allocation
  handle ← allocate_resource_internal(resource_type, size)
  
  IF handle IS Error THEN
    // Allocation failed - try OOM recovery
    oom_result ← oom_kill_with_fallback()
    
    MATCH oom_result WITH
      | Killed(tid, name, pages) →
          log_info("OOM killed process", tid, pages)
          handle ← allocate_resource_internal(resource_type, size)
      | Reclaimed(strategy, pages) →
          log_info("Reclaimed memory", strategy, pages)
          handle ← allocate_resource_internal(resource_type, size)
      | NoVictim(reason, fallback) →
          log_error("OOM with no victim", reason)
          MATCH fallback WITH
            | DenyAllocation → RETURN Error(AllocationError::OutOfMemory)
            | EnterEmergencyMode → enter_emergency_mode()
          END MATCH
    END MATCH
  END IF
  
  // Step 5: Update accounting
  account_resource_allocation(process_id, resource_type, size)
  update_diagnostics(resource_type, size)
  
  RETURN Ok(handle)
END
```

**Preconditions**:
- process_id must be valid and registered
- size must be positive and page-aligned
- Resource limits must be initialized for process

**Postconditions**:
- If successful: resource is allocated and accounted
- If failed: no resources leaked, accounting unchanged
- All errors are logged with context

**Loop Invariants**: N/A (no loops in main path)

### Validation Algorithm: Page Alignment and Bounds

```pascal
ALGORITHM validate_memory_operation(addr, size, operation)
INPUT: addr of type usize, size of type usize, operation of type MemoryOperation
OUTPUT: result of type Result<(), ValidationError>

BEGIN
  // Check alignment
  IF addr % PAGE_SIZE ≠ 0 THEN
    RETURN Error(ValidationError::Unaligned { addr, required_alignment: PAGE_SIZE })
  END IF
  
  IF size % PAGE_SIZE ≠ 0 THEN
    size ← align_up(size, PAGE_SIZE)
  END IF
  
  // Check bounds
  end_addr ← addr + size
  
  IF end_addr < addr THEN
    RETURN Error(ValidationError::OutOfBounds { addr, min: 0, max: USIZE_MAX })
  END IF
  
  // Check user space bounds
  IF operation IS UserSpace THEN
    IF addr >= KERNEL_BASE OR end_addr > USER_CANONICAL_MAX THEN
      RETURN Error(ValidationError::OutOfBounds { addr, min: 0, max: USER_CANONICAL_MAX })
    END IF
  END IF
  
  // Check protected resources
  IF operation IS Free THEN
    IF is_active_pml4(addr) THEN
      RETURN Error(ValidationError::ProtectedResource { resource_id: addr })
    END IF
  END IF
  
  RETURN Ok(())
END
```

**Preconditions**:
- addr and size are provided (may be unaligned)
- operation type is specified

**Postconditions**:
- Returns Ok only if all validation checks pass
- Returns specific error for each validation failure
- No side effects on input parameters

**Loop Invariants**: N/A (no loops)

### Thread Cleanup Algorithm: Unified Resource Cleanup

```pascal
ALGORITHM cleanup_thread_resources(thread_id)
INPUT: thread_id of type ThreadId
OUTPUT: result of type CleanupResult

BEGIN
  result ← CleanupResult::new()
  
  // Step 1: Enumerate all resources owned by thread
  resources ← enumerate_thread_resources(thread_id)
  
  log_info("Cleaning up thread", thread_id, resources.total_count())
  
  // Step 2: Revoke all capabilities
  FOR each cap_handle IN resources.capabilities DO
    MATCH revoke_capability(cap_handle, thread_id) WITH
      | Ok(revoked_handles) →
          result.capabilities_revoked ← result.capabilities_revoked + revoked_handles.length
      | Error(err) →
          result.errors.push(CleanupError::from(err))
          result.leaks_detected.push(LeakedResource::Capability(cap_handle))
    END MATCH
  END FOR
  
  // Step 3: Destroy all address spaces
  FOR each addr_space_id IN resources.address_spaces DO
    MATCH destroy_address_space(addr_space_id, thread_id) WITH
      | Ok(()) →
          result.address_spaces_destroyed ← result.address_spaces_destroyed + 1
      | Error(err) →
          result.errors.push(CleanupError::from(err))
          result.leaks_detected.push(LeakedResource::AddressSpace(addr_space_id))
    END MATCH
  END FOR
  
  // Step 4: Close all IPC ports
  FOR each port_id IN resources.ipc_ports DO
    MATCH close_port(port_id, thread_id) WITH
      | Ok(()) →
          result.ipc_ports_closed ← result.ipc_ports_closed + 1
      | Error(err) →
          result.errors.push(CleanupError::from(err))
          result.leaks_detected.push(LeakedResource::IpcPort(port_id))
    END MATCH
  END FOR
  
  // Step 5: Free physical pages
  IF resources.kernel_stack ≠ 0 THEN
    free_pages(resources.kernel_stack, resources.kernel_stack_pages)
    result.physical_pages_freed ← result.physical_pages_freed + resources.kernel_stack_pages
  END IF
  
  // Step 6: Validate cleanup completion
  remaining ← enumerate_thread_resources(thread_id)
  
  IF remaining.total_count() > 0 THEN
    log_error("Incomplete cleanup", thread_id, remaining.total_count())
    FOR each leaked_resource IN remaining.all_resources() DO
      result.leaks_detected.push(leaked_resource)
    END FOR
  END IF
  
  RETURN result
END
```

**Preconditions**:
- thread_id must be valid (may be already terminated)
- Thread must not be currently running

**Postconditions**:
- All owned resources are freed or logged as leaked
- Cleanup result contains complete accounting
- Cleanup is idempotent (safe to call multiple times)

**Loop Invariants**:
- All previously processed resources are freed or logged
- Cleanup state remains consistent throughout iteration
- Errors do not prevent cleanup of remaining resources

### OOM Recovery Algorithm: Graceful Degradation

```pascal
ALGORITHM oom_kill_with_fallback()
INPUT: none
OUTPUT: result of type OomResult

BEGIN
  pressure ← check_memory_pressure_detailed()
  
  log_warn("OOM condition", pressure.free_pages, pressure.total_pages)
  
  // Step 1: Try to find a victim process
  threads ← get_all_thread_info()
  best_victim ← None
  
  FOR each (tid, name, process_id, is_userspace) IN threads DO
    IF NOT is_userspace THEN
      CONTINUE  // Skip kernel threads
    END IF
    
    resident ← get_process_memory_usage(process_id).resident_pages
    
    IF resident < MIN_VICTIM_SIZE THEN
      CONTINUE  // Skip processes below minimum
    END IF
    
    MATCH best_victim WITH
      | None → best_victim ← Some((tid, name, resident))
      | Some((_, _, best_resident)) →
          IF resident > best_resident THEN
            best_victim ← Some((tid, name, resident))
          END IF
    END MATCH
  END FOR
  
  // Step 2: Kill victim if found
  MATCH best_victim WITH
    | Some((tid, name, resident)) →
        log_warn("Killing process", tid, name, resident)
        terminate_entity(tid, TerminationReason::OutOfMemory)
        RETURN OomResult::Killed { tid, name, pages_freed: resident }
    | None →
        // No victim found - try fallback strategies
        log_error("No OOM victim found", pressure)
  END MATCH
  
  // Step 3: Try reclaim strategies
  FOR each strategy IN [CacheEviction, CompactMemory, SwapOut] DO
    MATCH try_reclaim_memory(strategy) WITH
      | Ok(pages_freed) →
          IF pages_freed > 0 THEN
            log_info("Reclaimed memory", strategy, pages_freed)
            RETURN OomResult::Reclaimed { strategy, pages_freed }
          END IF
      | Error(err) →
          log_debug("Reclaim failed", strategy, err)
    END MATCH
  END FOR
  
  // Step 4: Determine fallback action
  fallback ← IF pressure.free_pages < CRITICAL_THRESHOLD THEN
    FallbackAction::EnterEmergencyMode
  ELSE
    FallbackAction::DenyAllocation
  END IF
  
  RETURN OomResult::NoVictim {
    reason: NoVictimReason::AllProcessesBelowMinimum,
    fallback_action: fallback
  }
END
```

**Preconditions**:
- Memory pressure is at OOM level
- Thread list is accessible and consistent

**Postconditions**:
- Either a victim is killed, memory is reclaimed, or fallback action is specified
- No kernel threads are killed
- All actions are logged for audit

**Loop Invariants**:
- best_victim always points to the largest eligible process seen so far
- All previously checked threads remain valid
- Kernel threads are never selected as victims

## Error Handling

### Error Scenario 1: Page Alignment Validation Failure

**Condition**: User provides unaligned address to map_page syscall
**Response**: Return EINVAL error code to userspace
**Recovery**: User must retry with page-aligned address
**Logging**: Log validation failure with address and required alignment

### Error Scenario 2: Resource Limit Exceeded

**Condition**: Process attempts to allocate resource beyond hard limit
**Response**: Return ELIMIT error code, deny allocation
**Recovery**: Process must free existing resources or request limit increase
**Logging**: Log limit violation with process ID, resource type, current/max usage

### Error Scenario 3: OOM with No Victim

**Condition**: System out of memory but no killable processes found
**Response**: Execute fallback action (deny allocation or enter emergency mode)
**Recovery**: System enters degraded mode, logs detailed state, waits for manual intervention
**Logging**: Log OOM condition, memory pressure, all processes and their memory usage

### Error Scenario 4: Capability Transfer Rollback

**Condition**: Capability transfer fails mid-operation (e.g., target process table full)
**Response**: Rollback all changes, restore capability to source process
**Recovery**: Capability remains with source, transfer can be retried
**Logging**: Log transfer failure, rollback actions, final state

### Error Scenario 5: Thread Cleanup Incomplete

**Condition**: Thread cleanup detects leaked resources after cleanup
**Response**: Log all leaked resources, update leak counters
**Recovery**: Leaked resources remain allocated, manual cleanup may be needed
**Logging**: Log each leaked resource with type, ID, and thread context

## Testing Strategy

### Unit Testing Approach

Test each component in isolation with mocked dependencies:

1. **Memory Safety Validation**
   - Test page alignment validation with aligned/unaligned addresses
   - Test PML4 protection with active/inactive page tables
   - Test phys_to_virt_ptr safety with HIGHER_HALF_READY flag states

2. **Capability System**
   - Test atomic transfer with success/failure scenarios
   - Test audit log bounded size with overflow conditions
   - Test revocation callbacks with various resource types

3. **OOM Management**
   - Test OOM killer with various victim selection scenarios
   - Test graceful degradation with no victim found
   - Test per-process memory limits enforcement

4. **Thread Cleanup**
   - Test unified cleanup with various resource combinations
   - Test leak detection with intentionally leaked resources
   - Test cleanup idempotency (multiple calls)

5. **Resource Limits**
   - Test soft/hard limit enforcement
   - Test limit checking with concurrent allocations
   - Test resource accounting accuracy

### Property-Based Testing Approach

Use property-based testing to verify invariants across random inputs:

**Property Test Library**: QuickCheck (Rust)

**Properties to Test**:

1. **Memory Safety**: For all valid addresses, validation succeeds; for all invalid addresses, validation fails
2. **Resource Accounting**: allocated_count - freed_count = current_usage (always)
3. **Cleanup Idempotency**: cleanup(thread_id) called N times produces same result as calling once
4. **Limit Enforcement**: current_usage ≤ hard_limit (always)
5. **OOM Recovery**: After OOM recovery, either memory is freed or allocation is denied (never panic)

### Integration Testing Approach

Test interactions between components in realistic scenarios:

1. **End-to-End Allocation**: Syscall → Validation → Limit Check → Allocation → Accounting
2. **OOM Recovery Flow**: Allocation failure → Memory pressure check → OOM kill → Retry allocation
3. **Thread Lifecycle**: Thread creation → Resource allocation → Thread exit → Cleanup → Validation
4. **Capability Lifecycle**: Create → Derive → Transfer → Revoke → Cleanup

## Performance Considerations

### Memory Overhead

- Validation layer adds minimal overhead (alignment checks are bitwise operations)
- Resource accounting requires per-process counters (8 bytes × 5 resources = 40 bytes per process)
- Audit log bounded at 1000 entries (1000 × ~64 bytes = 64 KB)
- Cleanup tracking sets use BTreeSet (O(log n) operations)

### CPU Overhead

- Validation checks add ~10-20 CPU cycles per allocation
- Resource limit checks add ~5-10 CPU cycles per allocation
- OOM killer victim selection is O(n) in number of threads (acceptable for <1000 threads)
- Thread cleanup is O(r) in number of resources owned by thread

### Optimization Strategies

1. **Fast Path Validation**: Cache validation results for frequently used addresses
2. **Batch Accounting**: Update resource counters in batches to reduce lock contention
3. **Lazy Cleanup**: Defer non-critical cleanup to background thread
4. **Pressure Caching**: Cache memory pressure calculation for 100ms to avoid repeated computation

## Security Considerations

### Threat Model

1. **Resource Exhaustion Attack**: Malicious process attempts to exhaust system resources
   - Mitigation: Per-process resource limits with hard enforcement
   
2. **Capability Forgery**: Attacker attempts to create or modify capabilities
   - Mitigation: Capabilities are opaque handles, all operations validated against global registry
   
3. **Memory Corruption**: Attacker attempts to corrupt kernel memory via invalid addresses
   - Mitigation: All addresses validated before use, PML4 protection prevents page table corruption
   
4. **Information Disclosure**: Attacker attempts to read kernel memory via crafted addresses
   - Mitigation: User space bounds checking prevents access to kernel addresses

### Security Properties

1. **Isolation**: Each process has independent resource limits and accounting
2. **Least Privilege**: Capabilities enforce minimum required permissions
3. **Fail-Safe**: All validation failures return errors instead of panicking
4. **Auditability**: All security-relevant operations logged to audit trail

## Dependencies

### Internal Kernel Dependencies

- **PMM (Physical Memory Manager)**: Page allocation, free page tracking
- **VMM (Virtual Memory Manager)**: Page table management, address translation
- **VMA (Virtual Memory Areas)**: Per-process memory region tracking
- **Capability System**: Capability validation and lifecycle management
- **Thread Management**: Thread lifecycle, context switching
- **Process Management**: Process lifecycle, resource ownership
- **IPC System**: Port management, message passing

### External Dependencies

None - all improvements are internal to the kernel.

### Build Dependencies

- Rust toolchain (nightly for inline assembly)
- x86_64 target support
- No additional crates required (uses existing alloc, core, spin)
