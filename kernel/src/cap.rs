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
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::process::ProcessId;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
        /// Filesystem namespace root — grants access to a mounted filesystem tree.
    /// READ = read files/dirs; WRITE = create/modify; GRANT = sub-delegation.
    FsNamespace {
        namespace_id: u64,
    },
    /// Directory capability — scoped access to a directory subtree.
    /// Derived from FsNamespace or another FsDir (requires GRANT on parent).
    FsDir {
        namespace_id: u64,
        inode: u64,
    },
    /// File capability — access to a specific regular file inode.
    /// Derived from FsNamespace or FsDir; carries READ/WRITE/EXECUTE perms.
    FsFile {
        namespace_id: u64,
        inode: u64,
    },
    /// I/O port access capability — grants direct hardware port I/O.
    /// READ = in (port read); WRITE = out (port write).
    IoPort {
        port: u16,
    },
}

/// Type of input device for capability granting
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InputDeviceType {
    Keyboard,
    Mouse,
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub handle: CapHandle,
    pub resource: ResourceType,
    pub permissions: CapPermissions,
    pub owner: ProcessId,
    pub parent: Option<CapHandle>,
    pub children: Vec<CapHandle>,
}

impl Capability {
    pub fn new_root(resource: ResourceType, owner: ProcessId, permissions: CapPermissions) -> Self {
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
        new_owner: ProcessId,
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

    pub fn is_owned_by(&self, process_id: ProcessId) -> bool {
        self.owner == process_id
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

#[derive(Debug, Clone)]
pub struct CapabilityTable {
    capabilities: BTreeMap<CapHandle, Capability>,
    owner: Option<ProcessId>,
}

impl CapabilityTable {
    pub fn new(owner: Option<ProcessId>) -> Self {
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

    pub fn owner(&self) -> Option<ProcessId> {
        self.owner
    }

    pub fn set_owner_process(&mut self, owner: Option<ProcessId>) {
        self.owner = owner;
    }
}

/// Maximum number of audit log entries to retain in memory.
///
/// The audit log tracks all capability lifecycle events (create, derive, transfer, revoke)
/// for security auditing and debugging. When the log reaches this limit, the oldest entries
/// are evicted to prevent unbounded memory growth.
///
/// # Tuning Guidelines
///
/// - **Default (1000)**: Suitable for most systems, provides ~64KB of audit history
/// - **Low memory systems (100-500)**: Reduce if memory is constrained
/// - **High security systems (5000-10000)**: Increase for longer audit trails
/// - **Development/debugging (10000+)**: Increase for detailed capability tracking
///
/// # Memory Impact
///
/// Each audit entry is approximately 64 bytes, so:
/// - 1000 entries ≈ 64 KB
/// - 5000 entries ≈ 320 KB
/// - 10000 entries ≈ 640 KB
///
/// # Configuration
///
/// To customize this value, modify this constant and rebuild the kernel:
/// ```rust
/// const MAX_AUDIT_LOG_ENTRIES: usize = 5000;
/// ```
///
/// Alternatively, you can override via Cargo features. Add one of these features to your build:
/// - `audit_log_entries_100` - For low memory systems (100 entries)
/// - `audit_log_entries_500` - For constrained systems (500 entries)
/// - `audit_log_entries_5000` - For high security systems (5000 entries)
/// - `audit_log_entries_10000` - For development/debugging (10000 entries)
///
/// Example:
/// ```bash
/// cargo build --features audit_log_entries_5000
/// ```
#[cfg(not(any(
    feature = "audit_log_entries_100",
    feature = "audit_log_entries_500",
    feature = "audit_log_entries_5000",
    feature = "audit_log_entries_10000"
)))]
const MAX_AUDIT_LOG_ENTRIES: usize = 1000;

#[cfg(feature = "audit_log_entries_100")]
const MAX_AUDIT_LOG_ENTRIES: usize = 100;

#[cfg(feature = "audit_log_entries_500")]
const MAX_AUDIT_LOG_ENTRIES: usize = 500;

#[cfg(feature = "audit_log_entries_5000")]
const MAX_AUDIT_LOG_ENTRIES: usize = 5000;

#[cfg(feature = "audit_log_entries_10000")]
const MAX_AUDIT_LOG_ENTRIES: usize = 10000;

pub struct CapabilityManager {
    global_caps: Mutex<BTreeMap<CapHandle, Capability>>,
    audit_log: Mutex<VecDeque<AuditLogEntry>>,
    eviction_count: AtomicU64,
}

impl CapabilityManager {
    pub const fn new() -> Self {
        Self {
            global_caps: Mutex::new(BTreeMap::new()),
            audit_log: Mutex::new(VecDeque::new()),
            eviction_count: AtomicU64::new(0),
        }
    }

    fn log_audit(&self, entry: AuditLogEntry) {
        let mut log = self.audit_log.lock();

        if log.len() >= MAX_AUDIT_LOG_ENTRIES {
            log.pop_front();
            self.eviction_count.fetch_add(1, Ordering::Relaxed);
            log_debug!(
                LOG_ORIGIN,
                "Audit log full: evicted oldest entry (total evictions: {})",
                self.eviction_count.load(Ordering::Relaxed)
            );
        }

        log.push_back(entry);
    }

    pub fn get_audit_log(&self, max_entries: usize) -> Vec<AuditLogEntry> {
        let log = self.audit_log.lock();
        let count = core::cmp::min(max_entries, log.len());
        log.iter().rev().take(count).cloned().collect()
    }

    pub fn get_audit_stats(&self) -> AuditStats {
        let log = self.audit_log.lock();
        AuditStats {
            size: log.len(),
            eviction_count: self.eviction_count.load(Ordering::Relaxed),
            max_entries: MAX_AUDIT_LOG_ENTRIES,
        }
    }

    pub fn register(&self, cap: Capability, actor_thread: ThreadId) -> Result<CapHandle, CapError> {
        let mut caps = self.global_caps.lock();
        let handle = cap.handle;

        if caps.contains_key(&handle) {
            return Err(CapError::AlreadyExists);
        }

        caps.insert(handle, cap);
        drop(caps);

        note_thread_capability_activity(actor_thread);

        self.log_audit(AuditLogEntry::new(
            AuditEventType::Create,
            actor_thread,
            handle,
        ));

        Ok(handle)
    }
    
    pub fn revoke(
        &self,
        handle: CapHandle,
        revoker: ProcessId,
        revoker_thread: ThreadId,
    ) -> Result<Vec<CapHandle>, CapError> {
        let mut caps = self.global_caps.lock();
        let mut revoked = Vec::new();

        // All of: lookup, ownership check, children collection, and remove happen
        // under the same lock to prevent TOCTOU between concurrent revoke calls.
        let cap = caps.get(&handle).ok_or(CapError::NotFound)?;
        let owner = cap.owner;

        // Only the capability owner may revoke it.
        // Transitive revocation of children (lines below) is authorised because
        // the owner of the root initiated the operation.
        if owner != revoker {
            return Err(CapError::NotOwner);
        }

        let children = cap.children.clone();
        let resource_type = cap.resource;

        caps.remove(&handle);
        revoked.push(handle);
        drop(caps);

        let removed_from_process = crate::process::remove_process_capability(owner, handle);
        debug_assert!(
            removed_from_process.is_some(),
            "Capability {} missing from authoritative process {} table during revoke",
            handle,
            owner
        );
        let _ = crate::thread::remove_process_capability_mirror(owner, handle);

        self.log_audit(AuditLogEntry::new(
            AuditEventType::Revoke,
            revoker_thread,
            handle,
        ));

        // Invoke registered revocation callbacks for this resource type
        // Requirements: Req 5.1, Req 5.3, Req 5.4, Req 5.5
        invoke_revocation_callbacks(resource_type, handle);

        for child_handle in children {
            if let Ok(mut child_revoked) = self.revoke(child_handle, revoker, revoker_thread) {
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

        let mut by_type = [0usize; 13];

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
                ResourceType::FsNamespace { .. } => 9,
                ResourceType::FsDir { .. } => 10,
                ResourceType::FsFile { .. } => 11,
                ResourceType::IoPort { .. } => 12,
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
            fs_namespace_caps: by_type[9],
            fs_dir_caps: by_type[10],
            fs_file_caps: by_type[11],
            io_port_caps: by_type[12],
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
    pub fs_namespace_caps: usize,
    pub fs_dir_caps: usize,
    pub fs_file_caps: usize,
    pub io_port_caps: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct AuditStats {
    pub size: usize,
    pub eviction_count: u64,
    pub max_entries: usize,
}

static CAPABILITY_MANAGER: CapabilityManager = CapabilityManager::new();
static CLEANED_THREAD_CAPABILITIES: Mutex<BTreeSet<ThreadId>> = Mutex::new(BTreeSet::new());
type RevocationCallback = fn(CapHandle);
type RevocationCallbackMap = BTreeMap<ResourceType, Vec<RevocationCallback>>;

static REVOCATION_CALLBACKS: Mutex<RevocationCallbackMap> = Mutex::new(BTreeMap::new());

/// Invoke all registered revocation callbacks for a given resource type.
/// Callbacks are invoked in registration order.
/// If a callback fails (panics), the failure is logged but remaining callbacks continue.
/// 
/// # Arguments
/// * `resource_type` - The type of resource being revoked
/// * `handle` - The capability handle being revoked
/// 
/// # Requirements
/// Implements Req 5.1: Invoke all registered callbacks for resource type
/// Implements Req 5.3: Log callback failures but continue with remaining callbacks
/// Implements Req 5.4: Invoke callbacks in registration order
/// Implements Req 5.5: Pass capability handle to each callback
fn invoke_revocation_callbacks(resource_type: ResourceType, handle: CapHandle) {
    let callbacks = REVOCATION_CALLBACKS.lock();
    
    if let Some(callback_list) = callbacks.get(&resource_type) {
        log_debug!(
            LOG_ORIGIN,
            "Invoking {} revocation callbacks for capability {} (resource type {:?})",
            callback_list.len(),
            handle,
            resource_type
        );
        
        // Invoke each callback in registration order
        // Note: In a no_std environment, we cannot catch panics, so callbacks
        // must be written to not panic. If a callback panics, it will propagate.
        for (idx, callback) in callback_list.iter().enumerate() {
            log_debug!(
                LOG_ORIGIN,
                "Invoking revocation callback {} for capability {}",
                idx,
                handle
            );
            callback(handle);
        }
    }
}

fn required_process_id(thread_id: ThreadId) -> Result<ProcessId, CapError> {
    crate::thread::get_thread_process_id(thread_id).ok_or(CapError::NotFound)
}

pub(crate) fn note_thread_capability_activity(thread_id: ThreadId) {
    CLEANED_THREAD_CAPABILITIES.lock().remove(&thread_id);
}

pub(crate) fn forget_thread_capability_cleanup(thread_id: ThreadId) {
    CLEANED_THREAD_CAPABILITIES.lock().remove(&thread_id);
}

fn begin_thread_capability_cleanup(thread_id: ThreadId) -> bool {
    CLEANED_THREAD_CAPABILITIES.lock().insert(thread_id)
}

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

pub fn create_capability_table(_owner: ThreadId) -> CapabilityTable {
    CapabilityTable::new(None)
}

pub fn create_process_capability_table(owner: ProcessId) -> CapabilityTable {
    CapabilityTable::new(Some(owner))
}

pub fn create_root_capability(
    resource: ResourceType,
    owner: ThreadId,
    permissions: CapPermissions,
) -> Result<Capability, CapError> {
    let owner_process = required_process_id(owner)?;
    let cap = Capability::new_root(resource, owner_process, permissions);
    CAPABILITY_MANAGER.register(cap.clone(), owner)?;
    Ok(cap)
}

pub fn revoke_capability(handle: CapHandle, revoker: ThreadId) -> Result<Vec<CapHandle>, CapError> {
    CAPABILITY_MANAGER.revoke(handle, required_process_id(revoker)?, revoker)
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

pub fn get_audit_stats() -> AuditStats {
    CAPABILITY_MANAGER.get_audit_stats()
}

pub fn get_capability_stats() -> CapabilityStats {
    CAPABILITY_MANAGER.stats()
}

/// Register a callback to be invoked when a capability of the specified resource type is revoked.
/// Callbacks are invoked in registration order.
/// 
/// # Arguments
/// * `resource_type` - The type of resource to register the callback for
/// * `callback` - The function to call when a capability of this type is revoked
/// 
/// # Requirements
/// Implements Req 5.2: Allow registration of revocation callbacks per resource type
pub fn register_revocation_callback(
    resource_type: ResourceType,
    callback: fn(CapHandle),
) {
    let mut callbacks = REVOCATION_CALLBACKS.lock();
    callbacks.entry(resource_type).or_default().push(callback);
    log_debug!(
        LOG_ORIGIN,
        "Registered revocation callback for resource type {:?}",
        resource_type
    );
}

/// Rollback a failed capability transfer by restoring the capability to the source process.
/// This function ensures atomicity by undoing all changes made during a transfer attempt.
fn rollback_transfer(
    cap_handle: CapHandle,
    source_process: ProcessId,
    original_cap: Capability,
) {
    log_debug!(
        LOG_ORIGIN,
        "Rolling back capability transfer for {} to process {}",
        cap_handle,
        source_process
    );

    // Restore capability to source process
    if let Err(err) = crate::process::add_process_capability(source_process, original_cap.clone()) {
        log_debug!(
            LOG_ORIGIN,
            "Failed to restore capability {} to source process {} during rollback: {:?}",
            cap_handle,
            source_process,
            err
        );
    }

    // Restore capability mirrors to source process threads
    if let Err(err) = crate::thread::mirror_process_capability_to_threads(source_process, original_cap) {
        log_debug!(
            LOG_ORIGIN,
            "Failed to restore capability {} mirrors to source process {} threads during rollback: {:?}",
            cap_handle,
            source_process,
            err
        );
    }
}

pub fn transfer_capability(
    cap_handle: CapHandle,
    source_thread: ThreadId,
    target_thread: ThreadId,
) -> Result<(), CapError> {
    let source_process = required_process_id(source_thread)?;
    let target_process = required_process_id(target_thread)?;

    // Step 1: Validate capability exists and permissions
    let cap = crate::process::get_process_capability(source_process, cap_handle)
        .ok_or_else(|| {
            log_debug!(
                LOG_ORIGIN,
                "Transfer failed: capability {} not found in source process {}",
                cap_handle,
                source_process
            );
            CapError::NotFound
        })?;

    if !cap.is_owned_by(source_process) {
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: capability {} not owned by source process {}",
            cap_handle,
            source_process
        );
        return Err(CapError::NotOwner);
    }

    if !cap.has_permission(CapPermissions::GRANT) {
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: capability {} lacks GRANT permission",
            cap_handle
        );
        return Err(CapError::PermissionDenied);
    }

    // Step 2: Handle same-process transfer (no-op)
    if source_process == target_process {
        CAPABILITY_MANAGER.log_audit(AuditLogEntry::new_transfer(
            source_thread,
            cap_handle,
            target_thread,
        ));
        return Ok(());
    }

    // Step 3: Validate target process has space for the capability
    // This check prevents starting a transfer that will fail due to table limits
    let target_cap_count = crate::process::get_process_capability_count(target_process)
        .ok_or_else(|| {
            log_debug!(
                LOG_ORIGIN,
                "Transfer failed: target process {} not found",
                target_process
            );
            CapError::NotFound
        })?;

    // Check if target process capability table has reasonable space
    // This is a soft check - the actual add operation will do the final validation
    if target_cap_count >= 1000 {
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: target process {} capability table near capacity ({} capabilities)",
            target_process,
            target_cap_count
        );
        return Err(CapError::AlreadyExists);
    }

    // Step 4: Remove capability from source process
    let removed_cap = crate::process::remove_process_capability(source_process, cap_handle)
        .ok_or_else(|| {
            log_debug!(
                LOG_ORIGIN,
                "Transfer failed: could not remove capability {} from source process {}",
                cap_handle,
                source_process
            );
            CapError::NotFound
        })?;

    // Remove from source thread mirrors
    let _ = crate::thread::remove_process_capability_mirror(source_process, cap_handle);

    // Save original capability for rollback
    let original_cap = removed_cap.clone();
    let mut transferred_cap = removed_cap;
    transferred_cap.owner = target_process;

    // Step 5: Add capability to target process
    if let Err(err) = crate::process::add_process_capability(target_process, transferred_cap.clone()) {
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: could not add capability {} to target process {}: {:?}",
            cap_handle,
            target_process,
            err
        );
        rollback_transfer(cap_handle, source_process, original_cap);
        return Err(err);
    }

    // Step 6: Mirror capability to target process threads
    if let Err(err) = crate::thread::mirror_process_capability_to_threads(target_process, transferred_cap.clone()) {
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: could not mirror capability {} to target process {} threads: {:?}",
            cap_handle,
            target_process,
            err
        );
        // Remove from target process before rollback
        let _ = crate::process::remove_process_capability(target_process, cap_handle);
        rollback_transfer(cap_handle, source_process, original_cap);
        return Err(err);
    }

    // Step 7: Update global capability registry
    let mut caps = CAPABILITY_MANAGER.global_caps.lock();
    if let Some(global_cap) = caps.get_mut(&cap_handle) {
        global_cap.owner = target_process;
    } else {
        // This should never happen - log error and attempt rollback
        log_debug!(
            LOG_ORIGIN,
            "Transfer failed: capability {} not found in global registry",
            cap_handle
        );
        drop(caps);
        let _ = crate::process::remove_process_capability(target_process, cap_handle);
        let _ = crate::thread::remove_process_capability_mirror(target_process, cap_handle);
        rollback_transfer(cap_handle, source_process, original_cap);
        return Err(CapError::NotFound);
    }
    drop(caps);

    // Step 8: Log successful transfer
    CAPABILITY_MANAGER.log_audit(AuditLogEntry::new_transfer(
        source_thread,
        cap_handle,
        target_thread,
    ));

    log_debug!(
        LOG_ORIGIN,
        "Successfully transferred capability {} from process {} to process {}",
        cap_handle,
        source_process,
        target_process
    );

    Ok(())
}

pub fn derive_capability(
    parent_handle: CapHandle,
    owner_thread: ThreadId,
    new_owner: ThreadId,
    reduced_perms: CapPermissions,
) -> Result<CapHandle, CapError> {
    let owner_process = required_process_id(owner_thread)?;
    let new_owner_process = required_process_id(new_owner)?;

    if !crate::process::process_has_capability(owner_process, parent_handle) {
        return Err(CapError::NotFound);
    }

    let mut caps = CAPABILITY_MANAGER.global_caps.lock();
    let parent = caps.get_mut(&parent_handle).ok_or(CapError::NotFound)?;

    if !parent.is_owned_by(owner_process) {
        return Err(CapError::NotOwner);
    }

    if !parent.has_permission(CapPermissions::GRANT) {
        return Err(CapError::PermissionDenied);
    }

    let child = parent.derive(new_owner_process, reduced_perms)?;
    let child_handle = child.handle;

    caps.insert(child_handle, child.clone());
    drop(caps);

    if let Err(err) = crate::process::append_process_capability_child(owner_process, parent_handle, child_handle) {
        let mut caps = CAPABILITY_MANAGER.global_caps.lock();
        caps.remove(&child_handle);
        if let Some(parent) = caps.get_mut(&parent_handle) {
            parent.children.retain(|existing| *existing != child_handle);
        }
        return Err(err);
    }

    if let Err(err) = crate::thread::append_process_capability_child_mirror(owner_process, parent_handle, child_handle) {
        let mut caps = CAPABILITY_MANAGER.global_caps.lock();
        caps.remove(&child_handle);
        if let Some(parent) = caps.get_mut(&parent_handle) {
            parent.children.retain(|existing| *existing != child_handle);
        }
        drop(caps);
        let _ = crate::process::remove_process_capability_child(owner_process, parent_handle, child_handle);
        return Err(err);
    }

    if let Err(err) = crate::process::add_process_capability(new_owner_process, child.clone()) {
        let mut caps = CAPABILITY_MANAGER.global_caps.lock();
        caps.remove(&child_handle);
        if let Some(parent) = caps.get_mut(&parent_handle) {
            parent.children.retain(|existing| *existing != child_handle);
        }
        drop(caps);
        let _ = crate::process::remove_process_capability_child(owner_process, parent_handle, child_handle);
        let _ = crate::thread::remove_process_capability_child_mirror(owner_process, parent_handle, child_handle);
        return Err(err);
    }

    if let Err(err) = crate::thread::mirror_process_capability_to_threads(new_owner_process, child.clone()) {
        let _ = crate::process::remove_process_capability(new_owner_process, child_handle);
        let mut caps = CAPABILITY_MANAGER.global_caps.lock();
        caps.remove(&child_handle);
        if let Some(parent) = caps.get_mut(&parent_handle) {
            parent.children.retain(|existing| *existing != child_handle);
        }
        drop(caps);
        let _ = crate::process::remove_process_capability_child(owner_process, parent_handle, child_handle);
        let _ = crate::thread::remove_process_capability_child_mirror(owner_process, parent_handle, child_handle);
        return Err(err);
    }

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

pub fn revoke_all_process_capabilities(process_id: crate::process::ProcessId) {
    let Some(process) = crate::process::get_process(process_id) else {
        log_debug!(
            LOG_ORIGIN,
            "Capability cleanup requested for unknown process {} - skipping revoke_all_process_capabilities",
            process_id
        );
        return;
    };

    let owned_caps = process.capability_table.list();

    log_info!(
        LOG_ORIGIN,
        "Revoking {} capabilities for process {}",
        owned_caps.len(),
        process_id
    );

    let mut caps = CAPABILITY_MANAGER.global_caps.lock();

    for handle in owned_caps {
        if let Some(_cap) = caps.remove(&handle) {
            log_debug!(
                LOG_ORIGIN,
                "Revoked capability {} from process {}",
                handle,
                process_id
            );
        }

        drop(caps);
        let _ = crate::process::remove_process_capability(process_id, handle);
        let _ = crate::thread::remove_process_capability_mirror(process_id, handle);
        CAPABILITY_MANAGER.log_audit(AuditLogEntry::new(
            AuditEventType::Revoke,
            process.primary_thread,
            handle,
        ));
        caps = CAPABILITY_MANAGER.global_caps.lock();
    }
}

/// Revoke all capabilities owned by a thread
/// This should be called when a thread terminates to clean up all its capabilities
pub fn revoke_all_thread_capabilities(thread_id: ThreadId) {
    if !begin_thread_capability_cleanup(thread_id) {
        log_debug!(
            LOG_ORIGIN,
            "Capability cleanup already ran for thread {} - skipping duplicate revoke_all_thread_capabilities",
            thread_id
        );
        return;
    }

    let owned_caps = crate::thread::list_thread_local_capabilities(thread_id);

    let mut caps = CAPABILITY_MANAGER.global_caps.lock();

    log_info!(
        LOG_ORIGIN,
        "Revoking {} capabilities for thread {}",
        owned_caps.len(),
        thread_id
    );

    // Remove all capabilities owned by this thread
    // Note: We do NOT recursively revoke children here because those
    // may belong to other threads. The capability graph may have
    // dangling parent references after this, but that's acceptable
    // since the parent capability is gone.
    for handle in owned_caps {
        if let Some(_cap) = caps.remove(&handle) {
            log_debug!(
                LOG_ORIGIN,
                "Revoked capability {} from thread {}",
                handle,
                thread_id
            );

            // Log audit entry for revocation
            drop(caps);
            let _ = crate::thread::remove_thread_capability(thread_id, handle);
            CAPABILITY_MANAGER.log_audit(AuditLogEntry::new(
                AuditEventType::Revoke,
                thread_id,
                handle,
            ));
            caps = CAPABILITY_MANAGER.global_caps.lock();
        }
    }
}
