#![allow(dead_code)]

// Process registry and deterministic process lifetime management.
//
// The process record intentionally outlives logical termination. Termination
// closes IPC/caps/shared mappings and marks all threads exited, but the record
// remains in PROCESS_REGISTRY until the thread reaper removes the final zombie
// and calls try_finalize_process_after_reap(). This prevents use-after-removal
// of the process -> PML4 association during final stack/address-space cleanup.


use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::cap::{
    CapError,
    CapHandle,
    CapPermissions,
    Capability,
    CapabilityTable,
    ResourceType,
};
use crate::{log_debug, log_error, log_info, log_warn};
use crate::thread::ThreadId;

const LOG_ORIGIN: &str = "process";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(u64);

impl ProcessId {
    pub fn from_raw(value: u64) -> Self {
        Self(value)
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn from_thread(thread_id: ThreadId) -> Self {
        Self(thread_id.raw())
    }
}

impl From<ThreadId> for ProcessId {
    fn from(value: ThreadId) -> Self {
        Self::from_thread(value)
    }
}

impl core::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    OutOfMemory,
    KernelInvariantViolation,
    KilledByKernel { reason_code: u64 },
    NormalExit { exit_code: u64 },
}

impl TerminationReason {
    pub fn description(self) -> &'static str {
        match self {
            TerminationReason::OutOfMemory => "oom",
            TerminationReason::KernelInvariantViolation => "kernel_invariant_violation",
            TerminationReason::KilledByKernel { .. } => "killed_by_kernel",
            TerminationReason::NormalExit { .. } => "normal_exit",
        }
    }

    fn as_thread_reason(self) -> crate::thread::TerminationReason {
        match self {
            TerminationReason::OutOfMemory => crate::thread::TerminationReason::OutOfMemory,
            TerminationReason::KernelInvariantViolation => {
                crate::thread::TerminationReason::KilledByKernel {
                    reason_code: 0x0000_0000_0000_0A11,
                }
            }
            TerminationReason::KilledByKernel { reason_code } => {
                crate::thread::TerminationReason::KilledByKernel { reason_code }
            }
            TerminationReason::NormalExit { exit_code } => {
                crate::thread::TerminationReason::NormalExit { exit_code }
            }
        }
    }

    fn is_oom(self) -> bool {
        matches!(self, TerminationReason::OutOfMemory)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTerminationError {
    NotFound,
    AlreadyTerminated,
}

#[derive(Debug, Clone)]
pub struct Process {
    pub id: ProcessId,
    pub pml4_phys: u64,
    pub primary_thread: ThreadId,
    pub threads: Vec<ThreadId>,
    pub capability_table: CapabilityTable,
    pub cleaned: bool,
    /// Canonical resident private pages charged to this process.
    pub resident_private_pages: usize,
    /// Canonical resident shared pages charged to this process.
    /// Policy: shared mappings are charged per process mapping (RSS semantics).
    pub resident_shared_pages: usize,
    /// Canonical reserved virtual bytes for this process.
    pub reserved_bytes: usize,
    /// Memory limit in pages (0 = unlimited)
    pub memory_limit_pages: usize,
    /// Process has entered deterministic teardown and must reject new growth/fault operations.
    pub terminating: bool,
    /// Process was selected by OOM killer.
    pub oom_killed: bool,
    /// Process teardown finished and no external operations are allowed.
    pub terminated: bool,
}

pub static PROCESS_REGISTRY: Mutex<BTreeMap<ProcessId, Process>> = Mutex::new(BTreeMap::new());
pub static PML4_TO_PROCESS: Mutex<BTreeMap<u64, ProcessId>> = Mutex::new(BTreeMap::new());

fn debug_assert_capability_store_owner(process: &Process) {
    debug_assert_eq!(
        process.capability_table.owner(),
        Some(process.id),
        "Process {} capability table must be owned by the same process",
        process.id
    );
}

fn ensure_pml4_mapping(process_id: ProcessId, pml4_phys: u64) {
    let mut pml4_map = PML4_TO_PROCESS.lock();
    if let Some(existing) = pml4_map.get(&pml4_phys).copied() {
        debug_assert_eq!(
            existing,
            process_id,
            "PML4 0x{:X} already mapped to process {} while registering {}",
            pml4_phys,
            existing,
            process_id
        );
    } else {
        pml4_map.insert(pml4_phys, process_id);
    }
}

pub fn register_process(process_id: ProcessId, pml4_phys: u64, primary_thread: ThreadId) {
    debug_assert_ne!(pml4_phys, 0, "userspace process {} must have non-zero PML4", process_id);
    debug_assert_eq!(
        process_id.raw(),
        primary_thread.raw(),
        "ProcessId {} must match primary thread {} during initial registration",
        process_id,
        primary_thread
    );

    ensure_pml4_mapping(process_id, pml4_phys);

    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        debug_assert_eq!(process.pml4_phys, pml4_phys);
        debug_assert_eq!(process.primary_thread, primary_thread);
        if !process.threads.contains(&primary_thread) {
            process.threads.push(primary_thread);
        }
        return;
    }

    registry.insert(
        process_id,
        Process {
            id: process_id,
            pml4_phys,
            primary_thread,
            threads: vec![primary_thread],
            capability_table: crate::cap::create_process_capability_table(process_id),
            cleaned: false,
            resident_private_pages: 0,
            resident_shared_pages: 0,
            reserved_bytes: 0,
            memory_limit_pages: 0, // 0 = unlimited
            terminating: false,
            oom_killed: false,
            terminated: false,
        },
    );
}

pub fn attach_thread_to_process(process_id: ProcessId, thread_id: ThreadId, pml4_phys: u64) {
    debug_assert_ne!(pml4_phys, 0, "userspace process {} must have non-zero PML4", process_id);

    ensure_pml4_mapping(process_id, pml4_phys);

    let mut registry = PROCESS_REGISTRY.lock();
    let process = if let Some(process) = registry.get_mut(&process_id) {
        process
    } else {
        log_warn!(
            LOG_ORIGIN,
            "Attaching thread {} to previously unregistered process {} (pml4=0x{:X})",
            thread_id,
            process_id,
            pml4_phys
        );
        registry.entry(process_id).or_insert(Process {
            id: process_id,
            pml4_phys,
            primary_thread: ThreadId::from_raw(process_id.raw()),
            threads: vec![thread_id],
            capability_table: crate::cap::create_process_capability_table(process_id),
            cleaned: false,
            resident_private_pages: 0,
            resident_shared_pages: 0,
            reserved_bytes: 0,
            memory_limit_pages: 0, // 0 = unlimited
            terminating: false,
            oom_killed: false,
            terminated: false,
        })
    };

    debug_assert_eq!(
        process.pml4_phys,
        pml4_phys,
        "Thread {} tried to join process {} with mismatched PML4 0x{:X} != 0x{:X}",
        thread_id,
        process_id,
        pml4_phys,
        process.pml4_phys
    );

    if !process.threads.contains(&thread_id) {
        process.threads.push(thread_id);
    }
}

pub fn debug_assert_thread_process_alignment(
    process_id: ProcessId,
    thread_id: ThreadId,
    pml4_phys: u64,
) {
    let registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get(&process_id) else {
        // Operational: thread references a process that no longer exists (may have been cleaned up).
        log_warn!(
            LOG_ORIGIN,
            "Thread {} references missing process {} during alignment check",
            thread_id,
            process_id
        );
        return;
    };

    debug_assert_eq!(
        process.id,
        process_id,
        "Process registry entry corrupted for process {}",
        process_id
    );
    debug_assert_eq!(
        process.pml4_phys,
        pml4_phys,
        "Thread {} cache PML4 0x{:X} does not match process {} PML4 0x{:X}",
        thread_id,
        pml4_phys,
        process_id,
        process.pml4_phys
    );
    // AUDIT: Class B — operational condition, will be replaced with structured error
    if !process.threads.contains(&thread_id) {
        log_warn!(
            LOG_ORIGIN,
            "Thread {} is missing from process {} membership list",
            thread_id,
            process_id
        );
    }

    if process_id.raw() == thread_id.raw() {
        debug_assert_eq!(
            process.primary_thread,
            thread_id,
            "Process {} primary thread must remain {}",
            process_id,
            thread_id
        );
    }
}

fn finalize_removed_process(process_id: ProcessId, pml4_phys: u64) {
    let mut pml4_map = PML4_TO_PROCESS.lock();
    if pml4_map.get(&pml4_phys).copied() == Some(process_id) {
        pml4_map.remove(&pml4_phys);
    }
    drop(pml4_map);

    crate::shared_mem::forget_process_shared_memory_cleanup(process_id);
}

pub fn detach_thread_from_process(process_id: ProcessId, thread_id: ThreadId) {
    let mut registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get_mut(&process_id) else {
        return;
    };

    process.threads.retain(|existing| *existing != thread_id);

    if process.threads.is_empty() && !process.cleaned {
        // Operational: process reached zero threads before the owner path
        // explicitly claimed final cleanup. Keep the process record alive until
        // the reaper calls `try_finalize_process_after_reap`; other subsystems
        // may still need the process -> PML4 association while the zombie thread
        // is being physically removed.
        log_warn!(
            LOG_ORIGIN,
            "Process {} reached zero threads before final cleanup was claimed",
            process_id
        );
    }
}

pub fn process_id_for_pml4(pml4_phys: u64) -> Option<ProcessId> {
    PML4_TO_PROCESS.lock().get(&pml4_phys).copied()
}

pub fn get_process(process_id: ProcessId) -> Option<Process> {
    PROCESS_REGISTRY.lock().get(&process_id).cloned()
}

pub fn get_process_pml4(process_id: ProcessId) -> Option<u64> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id)?;
    Some(process.pml4_phys)
}

pub fn is_process_terminating(process_id: ProcessId) -> bool {
    PROCESS_REGISTRY
        .lock()
        .get(&process_id)
        .map(|process| process.terminating)
        .unwrap_or(false)
}

pub fn is_process_terminated(process_id: ProcessId) -> bool {
    PROCESS_REGISTRY
        .lock()
        .get(&process_id)
        .map(|process| process.terminated)
        .unwrap_or(true)
}

/// Returns true when the process is allowed to create/grow mappings and
/// resolve faults. Missing process state is treated as blocked.
pub fn process_allows_memory_operations(process_id: ProcessId) -> bool {
    PROCESS_REGISTRY
        .lock()
        .get(&process_id)
        .map(|process| !process.terminating && !process.terminated)
        .unwrap_or(false)
}

pub fn pml4_allows_memory_operations(pml4_phys: u64) -> bool {
    let Some(process_id) = process_id_for_pml4(pml4_phys) else {
        return false;
    };
    process_allows_memory_operations(process_id)
}

fn mark_process_terminated(process_id: ProcessId) {
    let mut registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get_mut(&process_id) else {
        return;
    };

    process.terminated = true;
    process.terminating = false;
    process.cleaned = true;
    process.resident_private_pages = 0;
    process.resident_shared_pages = 0;
    process.reserved_bytes = 0;

    // Do not remove the registry entry here. The thread reaper may still need
    // process metadata and the PML4 association while it releases the zombie
    // thread's final physical resources.
}

/// Finalize a process after its last zombie thread was physically reaped.
///
/// This is the only normal path that removes a process record from
/// `PROCESS_REGISTRY`. It must be called by the thread reaper after:
///
/// 1. the zombie thread has been removed from the thread list;
/// 2. `detach_thread_from_process(process_id, tid)` has updated membership;
/// 3. the reaper no longer needs the process metadata for stack/address-space
///    cleanup.
///
/// The two-phase lifetime is intentional:
/// - `terminate_process` performs logical teardown and blocks new operations;
/// - `thread::reap_zombies` performs physical thread teardown;
/// - this function removes the process registry/PML4 mapping once both sides
///   agree there are no remaining threads.
pub fn try_finalize_process_after_reap(process_id: ProcessId) -> bool {
    let removed_pml4 = {
        let mut registry = PROCESS_REGISTRY.lock();

        let Some(process) = registry.get(&process_id) else {
            return false;
        };

        if !(process.cleaned && process.terminated && process.threads.is_empty()) {
            return false;
        }

        let pml4_phys = process.pml4_phys;
        registry.remove(&process_id);
        pml4_phys
    };

    finalize_removed_process(process_id, removed_pml4);

    log_debug!(
        LOG_ORIGIN,
        "[finalize] pid={} removed after zombie reap",
        process_id
    );

    true
}

/// Canonical deterministic process termination path.
///
/// Teardown order (INV-OOM-001, INV-MEM-001):
/// [1] mark process terminating
/// [2] block new memory operations
/// [3] remove all process threads from scheduler eligibility
/// [4] cleanup IPC ports
/// [5] cleanup shared memory mappings/ownership
/// [6] revoke process-owned capabilities + close FDs
/// [7] terminate all process threads (address-space teardown happens here)
/// [8] zero process accounting
/// [9] mark process terminated
pub fn terminate_process(
    process_id: ProcessId,
    reason: TerminationReason,
) -> core::result::Result<(), ProcessTerminationError> {
    let (thread_ids, resident_pages, limit_pages, pml4_phys) = {
        let mut registry = PROCESS_REGISTRY.lock();
        let Some(process) = registry.get_mut(&process_id) else {
            return Err(ProcessTerminationError::NotFound);
        };

        if process.terminated {
            return Err(ProcessTerminationError::AlreadyTerminated);
        }

        process.terminating = true;
        process.cleaned = true;
        if reason.is_oom() {
            process.oom_killed = true;
        }

        (
            process.threads.clone(),
            total_resident_pages(process),
            process.memory_limit_pages,
            process.pml4_phys,
        )
    };

    log_info!(
        LOG_ORIGIN,
        "[terminate] step=1 pid={} reason={} threads={} resident_pages={} limit_pages={} pml4=0x{:X}",
        process_id,
        reason.description(),
        thread_ids.len(),
        resident_pages,
        limit_pages,
        pml4_phys
    );

    log_debug!(LOG_ORIGIN, "[terminate] step=2 pid={} block_new_ops=true", process_id);

    for thread_id in &thread_ids {
        let _ = crate::thread::set_thread_state(*thread_id, crate::thread::ThreadState::Exited);
        crate::sched::deschedule_thread(*thread_id);
    }
    log_debug!(
        LOG_ORIGIN,
        "[terminate] step=3 pid={} scheduler_threads_descheduled={}",
        process_id,
        thread_ids.len()
    );

    for thread_id in &thread_ids {
        crate::ipc::close_all_thread_ports(*thread_id);
    }
    log_debug!(
        LOG_ORIGIN,
        "[terminate] step=4 pid={} ipc_cleanup_threads={}",
        process_id,
        thread_ids.len()
    );

    crate::shared_mem::cleanup_process_shared_memory(process_id);
    log_debug!(LOG_ORIGIN, "[terminate] step=5 pid={} shared_mem_cleanup=done", process_id);

    crate::cap::revoke_all_process_capabilities(process_id);
    crate::syscall::close_fds_for_process(process_id);
    log_debug!(LOG_ORIGIN, "[terminate] step=6 pid={} caps_fd_cleanup=done", process_id);

    let thread_reason = reason.as_thread_reason();
    let mut terminated_threads = 0usize;
    for thread_id in &thread_ids {
        if crate::thread::get_thread_state(*thread_id).is_none() {
            continue;
        }
        crate::thread::terminate_entity(*thread_id, thread_reason);
        terminated_threads = terminated_threads.saturating_add(1);
    }
    log_debug!(
        LOG_ORIGIN,
        "[terminate] step=7 pid={} thread_teardown_count={}",
        process_id,
        terminated_threads
    );

    reset_process_memory_accounting(process_id);
    log_debug!(LOG_ORIGIN, "[terminate] step=8 pid={} accounting=zeroed", process_id);

    mark_process_terminated(process_id);
    log_info!(LOG_ORIGIN, "[terminate] step=9 pid={} state=terminated", process_id);

    Ok(())
}

pub fn claim_process_cleanup(
    process_id: ProcessId,
    exiting_thread: ThreadId,
    pml4_phys: u64,
) -> bool {
    let mut registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get_mut(&process_id) else {
        // Operational: process may have been removed before cleanup claim.
        log_warn!(
            LOG_ORIGIN,
            "Missing process {} while claiming final cleanup for thread {}",
            process_id,
            exiting_thread
        );
        return false;
    };

    debug_assert_eq!(
        process.pml4_phys,
        pml4_phys,
        "Process {} PML4 0x{:X} must match exiting thread {} PML4 0x{:X}",
        process_id,
        process.pml4_phys,
        exiting_thread,
        pml4_phys
    );
    // AUDIT: Class B — operational condition, will be replaced with structured error
    if !process.threads.contains(&exiting_thread) {
        log_warn!(
            LOG_ORIGIN,
            "Process {} is missing exiting thread {} during cleanup claim",
            process_id,
            exiting_thread
        );
    }

    if process.cleaned {
        return false;
    }

    let has_live_sibling = process.threads.iter().copied().any(|other_thread| {
        other_thread != exiting_thread
            && matches!(
                crate::thread::get_thread_state(other_thread),
                Some(crate::thread::ThreadState::Running)
                    | Some(crate::thread::ThreadState::Ready)
                    | Some(crate::thread::ThreadState::Blocked)
                    | Some(crate::thread::ThreadState::WaitingIpc)
            )
    });

    if has_live_sibling {
        return false;
    }

    process.cleaned = true;
    true
}

pub fn is_process_cleaned(process_id: ProcessId) -> bool {
    PROCESS_REGISTRY
        .lock()
        .get(&process_id)
        .map(|process| process.cleaned)
        .unwrap_or(false)
}

pub fn add_process_capability(
    process_id: ProcessId,
    capability: Capability,
) -> Result<CapHandle, CapError> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id).ok_or(CapError::NotFound)?;
    debug_assert_capability_store_owner(process);
    debug_assert_eq!(
        capability.owner,
        process_id,
        "Capability {} owner {} does not match target process {}",
        capability.handle,
        capability.owner,
        process_id
    );
    process.capability_table.insert(capability)
}

pub fn remove_process_capability(
    process_id: ProcessId,
    cap_handle: CapHandle,
) -> Option<Capability> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id)?;
    debug_assert_capability_store_owner(process);
    process.capability_table.remove(cap_handle)
}

pub fn get_process_capability(
    process_id: ProcessId,
    cap_handle: CapHandle,
) -> Option<Capability> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id)?;
    debug_assert_capability_store_owner(process);
    process.capability_table.get(cap_handle).cloned()
}

/// Get the number of capabilities in a process's capability table.
/// Returns None if the process does not exist.
pub fn get_process_capability_count(process_id: ProcessId) -> Option<usize> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id)?;
    Some(process.capability_table.count())
}

pub fn process_has_capability(process_id: ProcessId, cap_handle: CapHandle) -> bool {
    let registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get(&process_id) else {
        return false;
    };

    debug_assert_capability_store_owner(process);

    process.capability_table.contains(cap_handle)
}

pub fn append_process_capability_child(
    process_id: ProcessId,
    parent_handle: CapHandle,
    child_handle: CapHandle,
) -> Result<(), CapError> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id).ok_or(CapError::NotFound)?;
    debug_assert_capability_store_owner(process);
    let parent = process
        .capability_table
        .get_mut(parent_handle)
        .ok_or(CapError::NotFound)?;

    if !parent.children.contains(&child_handle) {
        parent.children.push(child_handle);
    }

    Ok(())
}

pub fn remove_process_capability_child(
    process_id: ProcessId,
    parent_handle: CapHandle,
    child_handle: CapHandle,
) -> Result<(), CapError> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id).ok_or(CapError::NotFound)?;
    debug_assert_capability_store_owner(process);
    let parent = process
        .capability_table
        .get_mut(parent_handle)
        .ok_or(CapError::NotFound)?;

    parent.children.retain(|existing| *existing != child_handle);
    Ok(())
}

pub fn validate_process_capability(
    process_id: ProcessId,
    cap_handle: CapHandle,
    required_permission: CapPermissions,
) -> Result<(), CapError> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id).ok_or(CapError::NotFound)?;
    debug_assert_capability_store_owner(process);
    process
        .capability_table
        .validate(cap_handle, required_permission)
        .map(|_| ())
}

pub fn validate_process_capability_by_type<F>(
    process_id: ProcessId,
    required_permission: CapPermissions,
    resource_filter: F,
) -> bool
where
    F: Fn(&ResourceType) -> bool,
{
    let registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get(&process_id) else {
        return false;
    };

    debug_assert_capability_store_owner(process);

    for handle in process.capability_table.list() {
        if let Some(cap) = process.capability_table.get(handle) {
            if resource_filter(&cap.resource) && cap.has_permission(required_permission) {
                return true;
            }
        }
    }

    false
}

/// Memory usage information for a process
#[derive(Debug, Clone, Copy)]
pub struct MemoryUsage {
    /// Current total resident pages (private + shared)
    pub resident_pages: usize,
    /// Current resident private pages
    pub resident_private_pages: usize,
    /// Current resident shared pages
    pub resident_shared_pages: usize,
    /// Memory limit in pages (0 = unlimited)
    pub limit_pages: usize,
    /// Reserved virtual memory in bytes
    pub reserved_bytes: usize,
}

/// Immutable per-process snapshot used by OOM/policy code.
///
/// Collected under `PROCESS_REGISTRY` and then consumed without holding
/// structural locks (INV-LOCK-001 deep-scan snapshot rule).
#[derive(Debug, Clone, Copy)]
pub struct ProcessMemorySnapshot {
    pub process_id: ProcessId,
    pub resident_pages: usize,
    pub resident_private_pages: usize,
    pub resident_shared_pages: usize,
    pub limit_pages: usize,
    pub reserved_bytes: usize,
    pub terminating: bool,
    pub terminated: bool,
    pub oom_killed: bool,
}

#[inline]
fn total_resident_pages(process: &Process) -> usize {
    process
        .resident_private_pages
        .saturating_add(process.resident_shared_pages)
}

/// Collect an immutable process-memory snapshot for OOM and policy consumers.
pub fn collect_process_memory_snapshot() -> Vec<ProcessMemorySnapshot> {
    let registry = PROCESS_REGISTRY.lock();
    registry
        .values()
        .map(|process| ProcessMemorySnapshot {
            process_id: process.id,
            resident_pages: total_resident_pages(process),
            resident_private_pages: process.resident_private_pages,
            resident_shared_pages: process.resident_shared_pages,
            limit_pages: process.memory_limit_pages,
            reserved_bytes: process.reserved_bytes,
            terminating: process.terminating,
            terminated: process.terminated,
            oom_killed: process.oom_killed,
        })
        .collect()
}

/// Add resident pages to a process accounting record.
pub fn account_process_resident_pages_add(
    process_id: ProcessId,
    private_pages: usize,
    shared_pages: usize,
) {
    if private_pages == 0 && shared_pages == 0 {
        return;
    }

    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        process.resident_private_pages = process.resident_private_pages.saturating_add(private_pages);
        process.resident_shared_pages = process.resident_shared_pages.saturating_add(shared_pages);
    }
}

/// Subtract resident pages from a process accounting record.
pub fn account_process_resident_pages_sub(
    process_id: ProcessId,
    private_pages: usize,
    shared_pages: usize,
) {
    if private_pages == 0 && shared_pages == 0 {
        return;
    }

    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        process.resident_private_pages = process.resident_private_pages.saturating_sub(private_pages);
        process.resident_shared_pages = process.resident_shared_pages.saturating_sub(shared_pages);
    }
}

/// Add reserved virtual bytes to process accounting.
pub fn account_process_reserved_bytes_add(process_id: ProcessId, bytes: usize) {
    if bytes == 0 {
        return;
    }

    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        process.reserved_bytes = process.reserved_bytes.saturating_add(bytes);
    }
}

/// Subtract reserved virtual bytes from process accounting.
pub fn account_process_reserved_bytes_sub(process_id: ProcessId, bytes: usize) {
    if bytes == 0 {
        return;
    }

    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        process.reserved_bytes = process.reserved_bytes.saturating_sub(bytes);
    }
}

/// Reset all canonical memory accounting fields for a process.
pub fn reset_process_memory_accounting(process_id: ProcessId) {
    let mut registry = PROCESS_REGISTRY.lock();
    if let Some(process) = registry.get_mut(&process_id) {
        process.resident_private_pages = 0;
        process.resident_shared_pages = 0;
        process.reserved_bytes = 0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessAccountingState {
    pub resident_private_pages: usize,
    pub resident_shared_pages: usize,
    pub reserved_bytes: usize,
}

impl ProcessAccountingState {
    pub fn resident_pages(self) -> usize {
        self.resident_private_pages
            .saturating_add(self.resident_shared_pages)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountingDriftError {
    pub process_id: ProcessId,
    pub canonical: ProcessAccountingState,
    pub observed: ProcessAccountingState,
    pub page_table_resident_pages: usize,
    pub classified_resident_pages: usize,
    pub vma_map_present: bool,
    pub vma_owner_mismatch_pages: usize,
    pub vma_owner_missing_pages: usize,
    pub observed_shared_mem_pages: usize,
}

fn log_accounting_drift(context: &'static str, drift: &AccountingDriftError) {
    log_warn!(
        LOG_ORIGIN,
        "[ACCOUNTING_DRIFT] context={} pid={} canonical_private={} observed_private={} canonical_shared={} observed_shared={} canonical_reserved={} observed_reserved={} canonical_total={} observed_total={} page_table_total={} classified_total={} shared_mem_pages={} vma_map_present={} vma_owner_mismatch_pages={} vma_owner_missing_pages={}",
        context,
        drift.process_id,
        drift.canonical.resident_private_pages,
        drift.observed.resident_private_pages,
        drift.canonical.resident_shared_pages,
        drift.observed.resident_shared_pages,
        drift.canonical.reserved_bytes,
        drift.observed.reserved_bytes,
        drift.canonical.resident_pages(),
        drift.observed.resident_pages(),
        drift.page_table_resident_pages,
        drift.classified_resident_pages,
        drift.observed_shared_mem_pages,
        drift.vma_map_present,
        drift.vma_owner_mismatch_pages,
        drift.vma_owner_missing_pages
    );
}

/// Recalculate real process memory usage and compare against canonical accounting.
pub fn verify_process_accounting(process_id: ProcessId) -> Result<(), AccountingDriftError> {
    let canonical = {
        let registry = PROCESS_REGISTRY.lock();
        let process = registry.get(&process_id).ok_or(AccountingDriftError {
            process_id,
            canonical: ProcessAccountingState::default(),
            observed: ProcessAccountingState::default(),
            page_table_resident_pages: 0,
            classified_resident_pages: 0,
            vma_map_present: false,
            vma_owner_mismatch_pages: 0,
            vma_owner_missing_pages: 0,
            observed_shared_mem_pages: 0,
        })?;
        ProcessAccountingState {
            resident_private_pages: process.resident_private_pages,
            resident_shared_pages: process.resident_shared_pages,
            reserved_bytes: process.reserved_bytes,
        }
    };

    let vma_observed = crate::mm::vma::recalculate_process_vma_accounting(process_id);
    let shared_pages = crate::shared_mem::count_process_mapped_shared_pages(process_id);
    let observed = ProcessAccountingState {
        resident_private_pages: vma_observed.resident_private_pages,
        resident_shared_pages: vma_observed
            .resident_shared_pages
            .saturating_add(shared_pages),
        reserved_bytes: vma_observed.reserved_bytes,
    };
    let classified_resident_pages = observed.resident_pages();
    let page_table_resident_pages = get_process_pml4(process_id)
        .map(|pml4| crate::thread::count_user_space_pages(pml4 as usize))
        .unwrap_or(0);

    if canonical == observed && page_table_resident_pages == classified_resident_pages {
        return Ok(());
    }

    Err(AccountingDriftError {
        process_id,
        canonical,
        observed,
        page_table_resident_pages,
        classified_resident_pages,
        vma_map_present: vma_observed.map_present,
        vma_owner_mismatch_pages: vma_observed.owner_mismatch_pages,
        vma_owner_missing_pages: vma_observed.owner_missing_pages,
        observed_shared_mem_pages: shared_pages,
    })
}

/// Verify accounting and fail-fast in debug builds on drift.
pub fn verify_process_accounting_fail_fast(process_id: ProcessId, context: &'static str) -> bool {
    match verify_process_accounting(process_id) {
        Ok(()) => true,
        Err(drift) => {
            log_accounting_drift(context, &drift);
            // Operational accounting drift — log structured error, do not panic.
            // Callers must handle false return and take appropriate action.
            log_error!(
                LOG_ORIGIN,
                "[ACCOUNTING_DRIFT] context={} pid={} — process accounting drift detected, returning false",
                context,
                process_id
            );
            false
        }
    }
}

/// Debug-only sampling helper to verify a subset of processes.
pub fn verify_process_accounting_sample(limit: usize, context: &'static str) {
    if limit == 0 {
        return;
    }

    let snapshot = collect_process_memory_snapshot();
    for process in snapshot.iter().take(limit) {
        let _ = verify_process_accounting_fail_fast(process.process_id, context);
    }
}

/// Set the memory limit for a process in pages.
///
/// # Arguments
/// * `process_id` - The process to set the limit for
/// * `limit_pages` - The memory limit in pages (0 = unlimited)
///
/// # Returns
/// * `Ok(())` - Limit was set successfully
/// * `Err(())` - Process not found
///
/// # Requirements
/// Implements Req 7.1, Req 7.5
pub fn set_process_memory_limit(process_id: ProcessId, limit_pages: usize) -> Result<(), ()> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id).ok_or(())?;
    
    process.memory_limit_pages = limit_pages;
    
    log_info!(
        LOG_ORIGIN,
        "Set memory limit for process {} to {} pages ({} KB)",
        process_id,
        limit_pages,
        limit_pages * 4 // 4KB pages
    );
    
    Ok(())
}

/// Get the current memory usage for a process.
///
/// This is an O(1) read from canonical per-process accounting fields.
///
/// # Arguments
/// * `process_id` - The process to query
///
/// # Returns
/// * `Some(MemoryUsage)` - Memory usage information
/// * `None` - Process not found
///
/// # Requirements
/// Implements Req 7.1, Req 7.5
pub fn get_process_memory_usage(process_id: ProcessId) -> Option<MemoryUsage> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id)?;
    let resident_pages = total_resident_pages(process);

    Some(MemoryUsage {
        resident_pages,
        resident_private_pages: process.resident_private_pages,
        resident_shared_pages: process.resident_shared_pages,
        limit_pages: process.memory_limit_pages,
        reserved_bytes: process.reserved_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup_process_for_test(process_id: ProcessId, pml4_phys: u64) {
        PROCESS_REGISTRY.lock().remove(&process_id);
        PML4_TO_PROCESS.lock().remove(&pml4_phys);
    }

    #[test]
    fn verify_process_accounting_passes_when_zeroed() {
        let process_id = ProcessId::from_raw(90_001);
        let thread_id = ThreadId::from_raw(90_001);
        let pml4_phys = 0x9_0001_000;

        register_process(process_id, pml4_phys, thread_id);
        reset_process_memory_accounting(process_id);

        assert!(
            verify_process_accounting(process_id).is_ok(),
            "zeroed process without mappings should verify cleanly"
        );

        cleanup_process_for_test(process_id, pml4_phys);
    }

    #[test]
    fn verify_process_accounting_detects_drift() {
        let process_id = ProcessId::from_raw(90_002);
        let thread_id = ThreadId::from_raw(90_002);
        let pml4_phys = 0x9_0002_000;

        register_process(process_id, pml4_phys, thread_id);
        account_process_resident_pages_add(process_id, 3, 0);

        let drift = verify_process_accounting(process_id)
            .expect_err("drift must be detected when canonical counters diverge");

        assert_eq!(drift.process_id, process_id);
        assert_eq!(drift.canonical.resident_private_pages, 3);

        cleanup_process_for_test(process_id, pml4_phys);
    }
}