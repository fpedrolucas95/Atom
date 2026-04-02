#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::cap::{CapError, CapHandle, CapPermissions, Capability, CapabilityTable, ResourceType};
use crate::log_warn;
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

#[derive(Debug, Clone)]
pub struct Process {
    pub id: ProcessId,
    pub pml4_phys: u64,
    pub primary_thread: ThreadId,
    pub threads: Vec<ThreadId>,
    pub capability_table: CapabilityTable,
    pub cleaned: bool,
}

pub static PROCESS_REGISTRY: Mutex<BTreeMap<ProcessId, Process>> = Mutex::new(BTreeMap::new());
pub static PML4_TO_PROCESS: Mutex<BTreeMap<u64, ProcessId>> = Mutex::new(BTreeMap::new());

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
            capability_table: crate::cap::create_capability_table(primary_thread),
            cleaned: false,
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
            capability_table: crate::cap::create_capability_table(ThreadId::from_raw(process_id.raw())),
            cleaned: false,
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
        debug_assert!(
            false,
            "Thread {} references missing process {} during registration",
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
    debug_assert!(
        process.threads.contains(&thread_id),
        "Thread {} is missing from process {} membership list",
        thread_id,
        process_id
    );

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

pub fn detach_thread_from_process(process_id: ProcessId, thread_id: ThreadId) {
    let mut removed_pml4 = None;

    {
        let mut registry = PROCESS_REGISTRY.lock();
        let Some(process) = registry.get_mut(&process_id) else {
            return;
        };

        process.threads.retain(|existing| *existing != thread_id);
        if process.threads.is_empty() {
            debug_assert!(
                process.cleaned,
                "Process {} reached zero threads before final cleanup was claimed",
                process_id
            );
        }

        if process.cleaned && process.threads.is_empty() {
            removed_pml4 = Some(process.pml4_phys);
            registry.remove(&process_id);
        }
    }

    if let Some(pml4_phys) = removed_pml4 {
        let mut pml4_map = PML4_TO_PROCESS.lock();
        if pml4_map.get(&pml4_phys).copied() == Some(process_id) {
            pml4_map.remove(&pml4_phys);
        }
    }
}

pub fn process_id_for_pml4(pml4_phys: u64) -> Option<ProcessId> {
    PML4_TO_PROCESS.lock().get(&pml4_phys).copied()
}

pub fn get_process(process_id: ProcessId) -> Option<Process> {
    PROCESS_REGISTRY.lock().get(&process_id).cloned()
}

pub fn claim_process_cleanup(
    process_id: ProcessId,
    exiting_thread: ThreadId,
    pml4_phys: u64,
) -> bool {
    let mut registry = PROCESS_REGISTRY.lock();
    let Some(process) = registry.get_mut(&process_id) else {
        debug_assert!(
            false,
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
    debug_assert!(
        process.threads.contains(&exiting_thread),
        "Process {} is missing exiting thread {} during cleanup claim",
        process_id,
        exiting_thread
    );

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
    process.capability_table.insert(capability)
}

pub fn remove_process_capability(
    process_id: ProcessId,
    cap_handle: CapHandle,
) -> Option<Capability> {
    let mut registry = PROCESS_REGISTRY.lock();
    let process = registry.get_mut(&process_id)?;
    process.capability_table.remove(cap_handle)
}

pub fn validate_process_capability(
    process_id: ProcessId,
    cap_handle: CapHandle,
    required_permission: CapPermissions,
) -> Result<(), CapError> {
    let registry = PROCESS_REGISTRY.lock();
    let process = registry.get(&process_id).ok_or(CapError::NotFound)?;
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

    for handle in process.capability_table.list() {
        if let Some(cap) = process.capability_table.get(handle) {
            if resource_filter(&cap.resource) && cap.has_permission(required_permission) {
                return true;
            }
        }
    }

    false
}