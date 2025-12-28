// Capability System
//
// Implements capability-based access control for the Atom kernel.
// This module defines the core security model used across the kernel,
// where all sensitive operations are authorized via explicit capabilities
// instead of implicit privileges.
//
// Key responsibilities:
// - Define capability handles as unforgeable, opaque identifiers
// - Represent permissions and protected resource types
// - Manage capability creation, derivation, transfer, and revocation
// - Maintain a global capability registry with audit logging
// - Enforce ownership and permission checks across threads
//
// Design principles:
// - Capabilities are data, not pointers: handles index kernel-managed state
// - Least privilege: derived capabilities can only reduce permissions
// - Explicit ownership: every capability has a single owning thread
// - Revocation is transitive: revoking a parent invalidates all descendants
// - Auditable security: all capability lifecycle events are logged
//
// Core abstractions:
// - `CapHandle`: globally unique, monotonically allocated capability IDs
// - `CapPermissions`: composable bitflags (READ, WRITE, GRANT, etc.)
// - `ResourceType`: enumerates all kernel-managed resource classes
// - `Capability`: binds a handle to a resource, owner, permissions, and lineage
// - `CapabilityTable`: per-thread capability view
// - `CapabilityManager`: global authority and audit log
//
// Correctness and safety notes:
// - Global capability state is protected by spinlocks
// - Permission checks are explicit and centralized
// - Thread-local and global views are kept consistent on transfer/revoke
// - Audit log is size-bounded to prevent unbounded memory growth
//
// Security model:
// - All syscalls are expected to validate capabilities defined here
// - Delegation supports both transfer of ownership and permission reduction
// - The capability graph forms a directed tree/forest per resource
//
// This module is the foundation of Atom’s security architecture and is
// intentionally strict, explicit, and highly auditable.

#![allow(dead_code)]

use crate::log_info;
use crate::log_debug;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::thread::ThreadId;

const LOG_ORIGIN: &str = "cap";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapHandle(u64);

impl CapHandle {
    fn new() -> Self {
        static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);
        CapHandle(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn from_raw(value: u64) -> Self {
        CapHandle(value)
    }
}

impl core::fmt::Display for CapHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cap:{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapPermissions {
    bits: u32,
}

impl CapPermissions {
    pub const NONE: Self = Self { bits: 0 };
    pub const READ: Self = Self { bits: 1 << 0 };
    pub const WRITE: Self = Self { bits: 1 << 1 };
    pub const EXECUTE: Self = Self { bits: 1 << 2 };
    pub const GRANT: Self = Self { bits: 1 << 3 };
    pub const REVOKE: Self = Self { bits: 1 << 4 };
    pub const ALL: Self = Self { bits: 0xFFFFFFFF };

    pub const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    pub const fn bits(&self) -> u32 {
        self.bits
    }

    pub const fn contains(&self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }

    pub const fn union(&self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    pub const fn intersection(&self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    pub const fn is_subset_of(&self, other: Self) -> bool {
        (self.bits & !other.bits) == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEventType {
    Create,
    Derive,
    Transfer,
    Revoke,
}

#[derive(Debug, Clone)]
pub struct AuditLogEntry {
    pub timestamp: u64,
    pub event_type: AuditEventType,
    pub thread_id: ThreadId,
    pub cap_handle: CapHandle,
    pub parent_handle: Option<CapHandle>,
    pub target_thread: Option<ThreadId>,
}

impl AuditLogEntry {
    fn new(
        event_type: AuditEventType,
        thread_id: ThreadId,
        cap_handle: CapHandle,
    ) -> Self {
        Self {
            timestamp: crate::interrupts::get_ticks(),
            event_type,
            thread_id,
            cap_handle,
            parent_handle: None,
            target_thread: None,
        }
    }

    fn new_derive(
        thread_id: ThreadId,
        child_handle: CapHandle,
        parent_handle: CapHandle,
    ) -> Self {
        let mut entry = Self::new(AuditEventType::Derive, thread_id, child_handle);
        entry.parent_handle = Some(parent_handle);
        entry
    }

    fn new_transfer(
        thread_id: ThreadId,
        cap_handle: CapHandle,
        target_thread: ThreadId,
    ) -> Self {
        let mut entry = Self::new(AuditEventType::Transfer, thread_id, cap_handle);
        entry.target_thread = Some(target_thread);
        entry
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Thread(ThreadId),
    MemoryRegion {
        virt_addr: u64,
        phys_addr: u64,
        size: usize,
    },
    IpcPort {
        port_id: u64,
    },
    Irq {
        irq_num: u8,
    },
    Device {
        bdf: u16,
    },
    DmaBuffer {
        phys_addr: u64,
        size: usize,
    },
    SharedMemoryRegion {
        region_id: u64,
    },
    /// Framebuffer access capability - grants access to the display framebuffer
    Framebuffer {
        address: u64,
        width: u32,
        height: u32,
        stride: u32,
        bytes_per_pixel: u8,
    },
    /// Input device capability - grants access to keyboard/mouse input
    InputDevice {
        device_type: InputDeviceType,
    },
}

/// Type of input device for capability granting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDeviceType {
    Keyboard,
    Mouse,
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub handle: CapHandle,
    pub resource: ResourceType,
    pub permissions: CapPermissions,
    pub owner: ThreadId,
    pub parent: Option<CapHandle>,
    pub children: Vec<CapHandle>,
}

impl Capability {
    pub fn new_root(resource: ResourceType, owner: ThreadId, permissions: CapPermissions) -> Self {
        Self {
            handle: CapHandle::new(),
            resource,
            permissions,
            owner,
            parent: None,
            children: Vec::new(),
        }
    }

    pub fn derive(
        &mut self,
        new_owner: ThreadId,
        reduced_permissions: CapPermissions,
    ) -> Result<Self, CapError> {
        if !reduced_permissions.is_subset_of(self.permissions) {
            return Err(CapError::PermissionDenied);
        }

        let child_handle = CapHandle::new();

        self.children.push(child_handle);

        Ok(Self {
            handle: child_handle,
            resource: self.resource,
            permissions: reduced_permissions,
            owner: new_owner,
            parent: Some(self.handle),
            children: Vec::new(),
        })
    }

    pub fn has_permission(&self, perm: CapPermissions) -> bool {
        self.permissions.contains(perm)
    }

    pub fn is_owned_by(&self, thread_id: ThreadId) -> bool {
        self.owner == thread_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    NotFound,
    PermissionDenied,
    InvalidHandle,
    AlreadyExists,
    WrongResourceType,
    NotOwner,
}

#[derive(Debug)]
pub struct CapabilityTable {
    capabilities: BTreeMap<CapHandle, Capability>,
    owner: ThreadId,
}

impl CapabilityTable {
    pub fn new(owner: ThreadId) -> Self {
        Self {
            capabilities: BTreeMap::new(),
            owner,
        }
    }

    pub fn insert(&mut self, cap: Capability) -> Result<CapHandle, CapError> {
        let handle = cap.handle;

        if self.capabilities.contains_key(&handle) {
            return Err(CapError::AlreadyExists);
        }

        self.capabilities.insert(handle, cap);
        Ok(handle)
    }

    pub fn get(&self, handle: CapHandle) -> Option<&Capability> {
        self.capabilities.get(&handle)
    }

    pub fn get_mut(&mut self, handle: CapHandle) -> Option<&mut Capability> {
        self.capabilities.get_mut(&handle)
    }

    pub fn remove(&mut self, handle: CapHandle) -> Option<Capability> {
        self.capabilities.remove(&handle)
    }

    pub fn contains(&self, handle: CapHandle) -> bool {
        self.capabilities.contains_key(&handle)
    }
    
    pub fn validate(
        &self,
        handle: CapHandle,
        required_permission: CapPermissions,
    ) -> Result<&Capability, CapError> {
        let cap = self.get(handle).ok_or(CapError::NotFound)?;

        if !cap.has_permission(required_permission) {
            return Err(CapError::PermissionDenied);
        }

        Ok(cap)
    }

    pub fn list(&self) -> Vec<CapHandle> {
        self.capabilities.keys().copied().collect()
    }

    pub fn count(&self) -> usize {
        self.capabilities.len()
    }

    pub fn owner(&self) -> ThreadId {
        self.owner
    }
}

pub struct CapabilityManager {
    global_caps: Mutex<BTreeMap<CapHandle, Capability>>,
    audit_log: Mutex<VecDeque<AuditLogEntry>>,
}

const MAX_AUDIT_LOG_ENTRIES: usize = 1000;

impl CapabilityManager {
    pub const fn new() -> Self {
        Self {
            global_caps: Mutex::new(BTreeMap::new()),
            audit_log: Mutex::new(VecDeque::new()),
        }
    }

    fn log_audit(&self, entry: AuditLogEntry) {
        let mut log = self.audit_log.lock();

        if log.len() >= MAX_AUDIT_LOG_ENTRIES {
            log.pop_front();
        }

        log.push_back(entry);
    }

    pub fn get_audit_log(&self, max_entries: usize) -> Vec<AuditLogEntry> {
        let log = self.audit_log.lock();
        let count = core::cmp::min(max_entries, log.len());
        log.iter().rev().take(count).cloned().collect()
    }

    pub fn register(&self, cap: Capability) -> Result<CapHandle, CapError> {
        let mut caps = self.global_caps.lock();
        let handle = cap.handle;
        let owner = cap.owner;

        if caps.contains_key(&handle) {
            return Err(CapError::AlreadyExists);
        }

        caps.insert(handle, cap);
        drop(caps);

        self.log_audit(AuditLogEntry::new(
            AuditEventType::Create,
            owner,
            handle,
        ));

        Ok(handle)
    }
    
    pub fn revoke(&self, handle: CapHandle, revoker: ThreadId) -> Result<Vec<CapHandle>, CapError> {
        let mut caps = self.global_caps.lock();
        let mut revoked = Vec::new();
        let cap = caps.get(&handle).ok_or(CapError::NotFound)?;
        let owner = cap.owner;
        let children = cap.children.clone();

        caps.remove(&handle);
        revoked.push(handle);
        drop(caps);

        crate::thread::remove_thread_capability(owner, handle);

        self.log_audit(AuditLogEntry::new(
            AuditEventType::Revoke,
            revoker,
            handle,
        ));

        for child_handle in children {
            if let Ok(mut child_revoked) = self.revoke(child_handle, revoker) {
                revoked.append(&mut child_revoked);
            }
        }

        Ok(revoked)
    }
    
    pub fn query_parent(&self, handle: CapHandle) -> Result<Option<CapHandle>, CapError> {
        let caps = self.global_caps.lock();
        let cap = caps.get(&handle).ok_or(CapError::NotFound)?;
        Ok(cap.parent)
    }
    
    pub fn query_children(&self, handle: CapHandle) -> Result<Vec<CapHandle>, CapError> {
        let caps = self.global_caps.lock();
        let cap = caps.get(&handle).ok_or(CapError::NotFound)?;
        Ok(cap.children.clone())
    }

    pub fn lookup(&self, handle: CapHandle) -> Option<Capability> {
        let caps = self.global_caps.lock();
        caps.get(&handle).cloned()
    }

    pub fn stats(&self) -> CapabilityStats {
        let caps = self.global_caps.lock();
        let total = caps.len();

        let mut by_type = [0usize; 9];

        for cap in caps.values() {
            let idx = match cap.resource {
                ResourceType::Thread(_) => 0,
                ResourceType::MemoryRegion { .. } => 1,
                ResourceType::IpcPort { .. } => 2,
                ResourceType::Irq { .. } => 3,
                ResourceType::Device { .. } => 4,
                ResourceType::DmaBuffer { .. } => 5,
                ResourceType::SharedMemoryRegion { .. } => 6,
                ResourceType::Framebuffer { .. } => 7,
                ResourceType::InputDevice { .. } => 8,
            };
            by_type[idx] += 1;
        }

        CapabilityStats {
            total,
            thread_caps: by_type[0],
            memory_caps: by_type[1],
            ipc_caps: by_type[2],
            irq_caps: by_type[3],
            device_caps: by_type[4],
            dma_caps: by_type[5],
            framebuffer_caps: by_type[7],
            input_caps: by_type[8],
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CapabilityStats {
    pub total: usize,
    pub thread_caps: usize,
    pub memory_caps: usize,
    pub ipc_caps: usize,
    pub irq_caps: usize,
    pub device_caps: usize,
    pub dma_caps: usize,
    pub framebuffer_caps: usize,
    pub input_caps: usize,
}

static CAPABILITY_MANAGER: CapabilityManager = CapabilityManager::new();

pub fn init() {
    log_info!(
        LOG_ORIGIN,
        "Capability subsystem initialized (Phase 3.4 complete)"
    );
    log_info!(
        LOG_ORIGIN,
        "Enforcement active: thread creation + IPC operations require validated capabilities"
    );
    log_info!(
        LOG_ORIGIN,
        "Delegation enabled (grant/move) with permission filtering and revoke propagation"
    );
    log_info!(
        LOG_ORIGIN,
        "Audit logging enabled: tracking all cap operations (create/derive/transfer/revoke)"
    );
    log_debug!(
        LOG_ORIGIN,
        "Query APIs available: query_parent, query_children for derivation tree inspection"
    );
}

pub fn create_capability_table(owner: ThreadId) -> CapabilityTable {
    CapabilityTable::new(owner)
}

pub fn create_root_capability(
    resource: ResourceType,
    owner: ThreadId,
    permissions: CapPermissions,
) -> Result<Capability, CapError> {
    let cap = Capability::new_root(resource, owner, permissions);
    CAPABILITY_MANAGER.register(cap.clone())?;
    Ok(cap)
}

pub fn revoke_capability(handle: CapHandle, revoker: ThreadId) -> Result<Vec<CapHandle>, CapError> {
    CAPABILITY_MANAGER.revoke(handle, revoker)
}

pub fn query_parent(handle: CapHandle) -> Result<Option<CapHandle>, CapError> {
    CAPABILITY_MANAGER.query_parent(handle)
}

pub fn query_children(handle: CapHandle) -> Result<Vec<CapHandle>, CapError> {
    CAPABILITY_MANAGER.query_children(handle)
}

pub fn get_audit_log(max_entries: usize) -> Vec<AuditLogEntry> {
    CAPABILITY_MANAGER.get_audit_log(max_entries)
}

pub fn get_capability_stats() -> CapabilityStats {
    CAPABILITY_MANAGER.stats()
}

pub fn transfer_capability(
    cap_handle: CapHandle,
    source_thread: ThreadId,
    target_thread: ThreadId,
) -> Result<(), CapError> {
    let cap = crate::thread::remove_thread_capability(source_thread, cap_handle)
        .ok_or(CapError::NotFound)?;

    if !cap.is_owned_by(source_thread) {
        let _ = crate::thread::add_thread_capability(source_thread, cap);
        return Err(CapError::NotOwner);
    }

    if !cap.has_permission(CapPermissions::GRANT) {
        let _ = crate::thread::add_thread_capability(source_thread, cap);
        return Err(CapError::PermissionDenied);
    }

    let mut transferred_cap = cap;
    transferred_cap.owner = target_thread;

    crate::thread::add_thread_capability(target_thread, transferred_cap)
        .map_err(|_| CapError::AlreadyExists)?;

    let mut caps = CAPABILITY_MANAGER.global_caps.lock();
    if let Some(global_cap) = caps.get_mut(&cap_handle) {
        global_cap.owner = target_thread;
    }
    drop(caps);

    CAPABILITY_MANAGER.log_audit(AuditLogEntry::new_transfer(
        source_thread,
        cap_handle,
        target_thread,
    ));

    Ok(())
}

pub fn derive_capability(
    parent_handle: CapHandle,
    owner_thread: ThreadId,
    new_owner: ThreadId,
    reduced_perms: CapPermissions,
) -> Result<CapHandle, CapError> {
    if !crate::thread::thread_has_capability(owner_thread, parent_handle) {
        return Err(CapError::NotFound);
    }

    let mut caps = CAPABILITY_MANAGER.global_caps.lock();
    let parent = caps.get_mut(&parent_handle).ok_or(CapError::NotFound)?;

    if !parent.is_owned_by(owner_thread) {
        return Err(CapError::NotOwner);
    }

    if !parent.has_permission(CapPermissions::GRANT) {
        return Err(CapError::PermissionDenied);
    }

    let child = parent.derive(new_owner, reduced_perms)?;
    let child_handle = child.handle;

    caps.insert(child_handle, child.clone());
    drop(caps);

    crate::thread::add_thread_capability(new_owner, child)?;

    CAPABILITY_MANAGER.log_audit(AuditLogEntry::new_derive(
        owner_thread,
        child_handle,
        parent_handle,
    ));

    Ok(child_handle)
}

pub fn lookup_capability(handle: CapHandle) -> Option<Capability> {
    CAPABILITY_MANAGER.lookup(handle)
}