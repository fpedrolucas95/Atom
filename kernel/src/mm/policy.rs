// User-Space Memory Policy Integration
//
// Bridges kernel memory faults to user-space policy managers. The goal is to
// keep the kernel focused on enforcement while letting user services define
// strategies such as swapping or lazy file-backed mappings.
//
// Responsibilities:
// - Track an optional page-fault policy endpoint registered by user space
// - Emit structured IPC notifications on page faults
// - Validate that only the owning thread can register policy hooks
//
// Design notes:
// - Notifications are best-effort; the kernel will continue to fail-stop on
//   unrecoverable faults but surfaces enough context for external decisions.
// - Payloads are compact, fixed-width fields (address, error code, RIP, TID)
//   to keep IPC parsing simple for user-space services.
// - Ownership validation relies on IPC port metadata to prevent hijacking of
//   fault streams by other threads.

use alloc::vec::Vec;
use spin::Mutex;

use crate::ipc::{self, Message, PortId};
use crate::thread::ThreadId;
use crate::{log_debug, log_info, log_warn};

const LOG_ORIGIN: &str = "mem-policy";
const MSG_TYPE_PAGE_FAULT: u32 = 0xF001;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPolicyError {
    InvalidPort,
    PermissionDenied,
    NotRegistered,
    SendFailed,
}

struct PolicyState {
    page_fault_port: Option<PortId>,
}

impl PolicyState {
    const fn new() -> Self {
        Self { page_fault_port: None }
    }
}

pub struct MemoryPolicyManager {
    state: Mutex<PolicyState>,
}

impl MemoryPolicyManager {
    pub const fn new() -> Self {
        Self {
            state: Mutex::new(PolicyState::new()),
        }
    }

    pub fn register_page_fault_handler(
        &self,
        port_id: PortId,
        caller: ThreadId,
    ) -> Result<(), MemoryPolicyError> {
        let owner = ipc::get_port_owner(port_id).ok_or(MemoryPolicyError::InvalidPort)?;

        if owner != caller {
            log_warn!(
                LOG_ORIGIN,
                "Page fault handler registration rejected: port {:?} not owned by {}",
                port_id,
                caller
            );
            return Err(MemoryPolicyError::PermissionDenied);
        }

        let mut state = self.state.lock();
        state.page_fault_port = Some(port_id);

        log_info!(
            LOG_ORIGIN,
            "Registered user-space page fault handler on port {:?} (owner: {})",
            port_id,
            caller
        );

        Ok(())
    }

    pub fn notify_page_fault(
        &self,
        tid: ThreadId,
        fault_addr: u64,
        error_code: u64,
        instruction_pointer: u64,
    ) -> Result<(), MemoryPolicyError> {
        let port = {
            let state = self.state.lock();
            state.page_fault_port.ok_or(MemoryPolicyError::NotRegistered)?
        };

        let mut payload = Vec::with_capacity(32);
        payload.extend_from_slice(&fault_addr.to_le_bytes());
        payload.extend_from_slice(&error_code.to_le_bytes());
        payload.extend_from_slice(&instruction_pointer.to_le_bytes());
        payload.extend_from_slice(&tid.raw().to_le_bytes());

        let message = Message::new(tid, MSG_TYPE_PAGE_FAULT, payload);

        log_debug!(
            LOG_ORIGIN,
            "Dispatching page fault notification: port={:?} addr=0x{:X} err=0x{:X} rip=0x{:X} tid={}",
            port,
            fault_addr,
            error_code,
            instruction_pointer,
            tid
        );

        ipc::send_message_async(port, message).map_err(|_| MemoryPolicyError::SendFailed)
    }
}

static POLICY_MANAGER: MemoryPolicyManager = MemoryPolicyManager::new();

pub fn init() {
    log_info!(
        LOG_ORIGIN,
        "User-space memory policy hooks active: page fault notifications available"
    );
}

pub fn register_page_fault_handler(
    port_id: PortId,
    caller: ThreadId,
) -> Result<(), MemoryPolicyError> {
    POLICY_MANAGER.register_page_fault_handler(port_id, caller)
}

pub fn notify_page_fault(
    tid: ThreadId,
    fault_addr: u64,
    error_code: u64,
    instruction_pointer: u64,
) -> Result<(), MemoryPolicyError> {
    POLICY_MANAGER.notify_page_fault(tid, fault_addr, error_code, instruction_pointer)
}

#[allow(dead_code)]
pub fn page_fault_message_type() -> u32 {
    MSG_TYPE_PAGE_FAULT
}
