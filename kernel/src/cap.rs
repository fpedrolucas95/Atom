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

use crate::log_debug;
use crate::log_info;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::process::ProcessId;
use crate::thread::ThreadId;
use crate::log_warn;

const LOG_ORIGIN: &str = "cap";

pub mod revoke_plan;
use revoke_plan::build_revoke_plan_from_snapshot;

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
    PciEnumerate,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeError {
    NotFound,
    NotOwner,
    DiscoveryInconsistent,
    ProcessMutationFailed,
    ThreadMirrorMutationFailed,
    ParentLinkUpdateFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeStatus {
    Complete,
    Partial,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackError {
    Recoverable(&'static str),
    Fatal(&'static str),
}

pub type CallbackResult = Result<(), CallbackError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackExecutionResult {
    pub resource_type: ResourceType,
    pub handle: CapHandle,
    pub callback_index: usize,
    pub result: CallbackResult,
}

#[derive(Debug, Clone)]
pub struct RevokeReport {
    pub root: CapHandle,
    pub revoked: Vec<CapHandle>,
    pub missing: Vec<CapHandle>,
    pub failed: Vec<(CapHandle, RevokeError)>,
    pub callback_results: Vec<CallbackExecutionResult>,
    pub status: RevokeStatus,
}

impl RevokeReport {
    fn new(root: CapHandle) -> Self {
        Self {
            root,
            revoked: Vec::new(),
            missing: Vec::new(),
            failed: Vec::new(),
            callback_results: Vec::new(),
            status: RevokeStatus::Failed,
        }
    }

    fn finalize_status(&mut self) {
        self.status = if self.failed.is_empty() && self.missing.is_empty() {
            RevokeStatus::Complete
        } else if !self.revoked.is_empty() {
            RevokeStatus::Partial
        } else {
            RevokeStatus::Failed
        };
    }

    pub fn callbacks_executed(&self) -> usize {
        self.callback_results.len()
    }

    pub fn callback_failures(&self) -> usize {
        self.callback_results
            .iter()
            .filter(|entry| entry.result.is_err())
            .count()
    }
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
    ) -> Result<RevokeReport, RevokeError> {
        let snapshot = {
            let caps = self.global_caps.lock();
            let Some(root_capability) = caps.get(&handle) else {
                let mut report = RevokeReport::new(handle);
                report.missing.push(handle);
                report.finalize_status();
                return Ok(report);
            };
            if root_capability.owner != revoker {
                return Err(RevokeError::NotOwner);
            }
            caps.clone()
        };

        let plan = build_revoke_plan_from_snapshot(handle, &snapshot)?;
        let mut report = RevokeReport::new(handle);
        report.missing.extend(plan.missing.iter().copied());
        report.missing.sort();
        report.missing.dedup();
        let mut callback_queue = Vec::new();

        for current in plan.order.iter().copied() {
            let removed_capability = {
                let mut caps = self.global_caps.lock();
                caps.remove(&current)
            };

            let Some(removed_capability) = removed_capability else {
                report.missing.push(current);
                continue;
            };

            if crate::process::remove_process_capability(removed_capability.owner, current).is_none() {
                report.failed.push((current, RevokeError::ProcessMutationFailed));
            }

            if crate::thread::remove_process_capability_mirror(removed_capability.owner, current).is_none() {
                report.failed.push((current, RevokeError::ThreadMirrorMutationFailed));
            }

            if let Some(parent_handle) = removed_capability.parent {
                let parent_owner = snapshot
                    .get(&parent_handle)
                    .map(|parent| parent.owner);
                let Some(parent_owner) = parent_owner else {
                    report.failed.push((current, RevokeError::ParentLinkUpdateFailed));
                    continue;
                };

                if crate::process::remove_process_capability_child(parent_owner, parent_handle, current).is_err() {
                    report.failed.push((current, RevokeError::ParentLinkUpdateFailed));
                }

                if crate::thread::remove_process_capability_child_mirror(parent_owner, parent_handle, current).is_err() {
                    report.failed.push((current, RevokeError::ParentLinkUpdateFailed));
                }
            }

            self.log_audit(AuditLogEntry::new(
                AuditEventType::Revoke,
                revoker_thread,
                current,
            ));
            callback_queue.push((removed_capability.resource, current));
            report.revoked.push(current);
        }

        self.execute_revocation_callbacks(&callback_queue, &mut report);
        report.missing.sort();
        report.missing.dedup();
        report.finalize_status();
        Ok(report)
    }

    fn execute_revocation_callbacks(
        &self,
        callback_queue: &[(ResourceType, CapHandle)],
        report: &mut RevokeReport,
    ) {
        // Build invocation queue while holding callback registry lock.
        // Callback execution always happens after this scope.
        let callback_invocations = {
            let callbacks = REVOCATION_CALLBACKS.lock();
            let mut queue = Vec::new();

            for (resource_type, handle) in callback_queue.iter().copied() {
                let Some(registered_callbacks) = callbacks.get(&resource_type) else {
                    continue;
                };

                for (callback_index, callback) in registered_callbacks.iter().copied().enumerate() {
                    queue.push(QueuedCallbackInvocation {
                        resource_type,
                        handle,
                        callback_index,
                        callback,
                    });
                }
            }

            queue
        };

        for invocation in callback_invocations {
            let result = (invocation.callback)(invocation.handle);

            if let Err(err) = result {
                log_warn!(
                    LOG_ORIGIN,
                    "Revocation callback failure: resource={:?} handle={} callback={} err={:?}",
                    invocation.resource_type,
                    invocation.handle,
                    invocation.callback_index,
                    err
                );
            }

            report.callback_results.push(CallbackExecutionResult {
                resource_type: invocation.resource_type,
                handle: invocation.handle,
                callback_index: invocation.callback_index,
                result,
            });
        }
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

        let mut by_type = [0usize; 14];

        for cap in caps.values() {
            let idx = match cap.resource {
                ResourceType::Thread(_) => 0,
                ResourceType::MemoryRegion { .. } => 1,
                ResourceType::IpcPort { .. } => 2,
                ResourceType::Irq { .. } => 3,
                ResourceType::Device { .. } => 4,
                ResourceType::PciEnumerate => 5,
                ResourceType::DmaBuffer { .. } => 6,
                ResourceType::SharedMemoryRegion { .. } => 7,
                ResourceType::Framebuffer { .. } => 8,
                ResourceType::InputDevice { .. } => 9,
                ResourceType::FsNamespace { .. } => 10,
                ResourceType::FsDir { .. } => 11,
                ResourceType::FsFile { .. } => 12,
                ResourceType::IoPort { .. } => 13,
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
            pci_enumerate_caps: by_type[5],
            dma_caps: by_type[6],
            framebuffer_caps: by_type[8],
            input_caps: by_type[9],
            fs_namespace_caps: by_type[10],
            fs_dir_caps: by_type[11],
            fs_file_caps: by_type[12],
            io_port_caps: by_type[13],
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
    pub pci_enumerate_caps: usize,
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

type RevocationCallback = fn(CapHandle) -> CallbackResult;
type RevocationCallbackMap = BTreeMap<ResourceType, Vec<RevocationCallback>>;

#[derive(Debug, Clone, Copy)]
struct QueuedCallbackInvocation {
    resource_type: ResourceType,
    handle: CapHandle,
    callback_index: usize,
    callback: RevocationCallback,
}

static REVOCATION_CALLBACKS: Mutex<RevocationCallbackMap> = Mutex::new(BTreeMap::new());

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

pub fn revoke_capability(handle: CapHandle, revoker: ThreadId) -> Result<RevokeReport, RevokeError> {
    let revoker_process = required_process_id(revoker).map_err(|_| RevokeError::NotFound)?;
    CAPABILITY_MANAGER.revoke(handle, revoker_process, revoker)
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
/// Callback contract is explicit: return `Ok(())` on success and `Err(CallbackError)` on failure.
/// In `no_std`, callbacks must not panic; panic is a fatal kernel bug.
///
/// # Arguments
/// * `resource_type` - The type of resource to register the callback for
/// * `callback` - The function to call when a capability of this type is revoked
///
/// # Requirements
/// Implements Req 5.2: Allow registration of revocation callbacks per resource type
pub fn register_revocation_callback(
    resource_type: ResourceType,
    callback: fn(CapHandle) -> CallbackResult,
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

pub fn revoke_all_process_capabilities(process_id: crate::process::ProcessId) -> Vec<RevokeReport> {
    let mut reports = Vec::new();
    let Some(process) = crate::process::get_process(process_id) else {
        log_debug!(
            LOG_ORIGIN,
            "Capability cleanup requested for unknown process {} - skipping revoke_all_process_capabilities",
            process_id
        );
        return reports;
    };

    let owned_caps = process.capability_table.list();

    log_info!(
        LOG_ORIGIN,
        "Revoking {} capabilities for process {}",
        owned_caps.len(),
        process_id
    );

    for handle in owned_caps {
        match CAPABILITY_MANAGER.revoke(handle, process_id, process.primary_thread) {
            Ok(report) => reports.push(report),
            Err(err) => {
                let mut report = RevokeReport::new(handle);
                report.failed.push((handle, err));
                report.finalize_status();
                reports.push(report);
                log_warn!(
                    LOG_ORIGIN,
                    "Failed to revoke capability {} during process {} cleanup: {:?}",
                    handle,
                    process_id,
                    err
                );
            }
        }
    }

    reports
}

/// Revoke all capabilities owned by a thread
/// This should be called when a thread terminates to clean up all its capabilities
pub fn revoke_all_thread_capabilities(thread_id: ThreadId) -> Vec<RevokeReport> {
    let mut reports = Vec::new();
    if !begin_thread_capability_cleanup(thread_id) {
        log_debug!(
            LOG_ORIGIN,
            "Capability cleanup already ran for thread {} - skipping duplicate revoke_all_thread_capabilities",
            thread_id
        );
        return reports;
    }

    let owned_caps = crate::thread::list_thread_local_capabilities(thread_id);
    let revoker_process = crate::thread::get_thread_process_id(thread_id);

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
        if let Some(process_id) = revoker_process {
            match CAPABILITY_MANAGER.revoke(handle, process_id, thread_id) {
                Ok(report) => reports.push(report),
                Err(err) => {
                    let mut report = RevokeReport::new(handle);
                    report.failed.push((handle, err));
                    report.finalize_status();
                    reports.push(report);
                    log_warn!(
                        LOG_ORIGIN,
                        "Failed to revoke capability {} during thread {} cleanup: {:?}",
                        handle,
                        thread_id,
                        err
                    );
                }
            }
            continue;
        }

        let mut report = RevokeReport::new(handle);
        let removed_capability = {
            let mut caps = CAPABILITY_MANAGER.global_caps.lock();
            caps.remove(&handle)
        };
        let Some(removed_capability) = removed_capability else {
            report.missing.push(handle);
            report.finalize_status();
            reports.push(report);
            continue;
        };

        if crate::thread::remove_thread_capability(thread_id, handle).is_none() {
            report.failed.push((handle, RevokeError::ThreadMirrorMutationFailed));
        }

        CAPABILITY_MANAGER.log_audit(AuditLogEntry::new(
            AuditEventType::Revoke,
            thread_id,
            handle,
        ));
        CAPABILITY_MANAGER.execute_revocation_callbacks(&[(removed_capability.resource, handle)], &mut report);
        report.revoked.push(handle);
        report.finalize_status();
        reports.push(report);
    }

    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicBool, Ordering};
    use spin::Mutex;

    static TEST_SERIAL: Mutex<()> = Mutex::new(());
    static CALLBACK_TRACE: Mutex<Vec<(&'static str, u64)>> = Mutex::new(Vec::new());
    static CALLBACK_FIXTURE: Mutex<Option<TestFixture>> = Mutex::new(None);
    static CALLBACK_REENTRANT_REGISTERED: AtomicBool = AtomicBool::new(false);

    #[derive(Debug, Clone, Copy)]
    struct TestFixture {
        pid: ProcessId,
        tid: ThreadId,
        pml4: u64,
    }

    fn reset_callback_test_state() {
        CALLBACK_TRACE.lock().clear();
        *CALLBACK_FIXTURE.lock() = None;
        CALLBACK_REENTRANT_REGISTERED.store(false, Ordering::SeqCst);
    }

    fn callback_probe_subsystems(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("probe", handle.raw()));

        if let Some(fixture) = *CALLBACK_FIXTURE.lock() {
            let _ = crate::process::get_process(fixture.pid);
            let _ = crate::thread::list_thread_local_capabilities(fixture.tid);
            let _ = crate::ipc::get_stats();
            let _ = crate::mm::vma::get_process_stats(fixture.pid);
        }

        Ok(())
    }

    fn callback_first(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("first", handle.raw()));
        Ok(())
    }

    fn callback_second(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("second", handle.raw()));
        Ok(())
    }

    fn callback_error(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("error", handle.raw()));
        Err(CallbackError::Recoverable("test callback failure"))
    }

    fn callback_reentrant_register(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("reentrant", handle.raw()));
        if !CALLBACK_REENTRANT_REGISTERED.swap(true, Ordering::SeqCst) {
            register_revocation_callback(
                ResourceType::IpcPort { port_id: 95_009 },
                callback_reentrant_late,
            );
        }
        Ok(())
    }

    fn callback_reentrant_late(handle: CapHandle) -> CallbackResult {
        CALLBACK_TRACE.lock().push(("late", handle.raw()));
        Ok(())
    }

    fn setup_fixture(seed: u64) -> TestFixture {
        let fixture = TestFixture {
            pid: ProcessId::from_raw(seed),
            tid: ThreadId::from_raw(seed),
            pml4: 0xC000_0000 + (seed << 12),
        };
        reset_callback_test_state();
        cleanup_fixture(fixture);

        let thread = crate::thread::Thread {
            id: fixture.tid,
            process_id: Some(fixture.pid),
            state: crate::thread::ThreadState::Ready,
            context: crate::thread::CpuContext::zero(),
            kernel_stack: 0,
            kernel_stack_size: 0,
            address_space: fixture.pml4,
            priority: crate::thread::ThreadPriority::Normal,
            name: "cap-test",
            capability_table: create_capability_table(fixture.tid),
            is_userspace: true,
            user_stack: None,
        };
        crate::thread::add_thread(thread);

        fixture
    }

    fn cleanup_fixture(fixture: TestFixture) {
        let owned_handles = {
            let caps = CAPABILITY_MANAGER.global_caps.lock();
            caps.iter()
                .filter_map(|(handle, cap)| (cap.owner == fixture.pid).then_some(*handle))
                .collect::<Vec<_>>()
        };
        for handle in owned_handles {
            CAPABILITY_MANAGER.global_caps.lock().remove(&handle);
        }

        {
            let mut registry = crate::process::PROCESS_REGISTRY.lock();
            if let Some(process) = registry.get_mut(&fixture.pid) {
                process.cleaned = true;
                process.terminating = false;
                process.terminated = true;
            }
        }

        let _ = crate::thread::remove_thread(fixture.tid);
        crate::process::PROCESS_REGISTRY.lock().remove(&fixture.pid);
        crate::process::PML4_TO_PROCESS.lock().remove(&fixture.pml4);
        CLEANED_THREAD_CAPABILITIES.lock().remove(&fixture.tid);
        REVOCATION_CALLBACKS.lock().clear();
    }

    fn install_capability(fixture: TestFixture, capability: Capability) {
        CAPABILITY_MANAGER
            .register(capability.clone(), fixture.tid)
            .expect("capability registration must succeed in test fixture");
        crate::thread::add_thread_capability(fixture.tid, capability)
            .expect("process/thread capability mirror update must succeed");
    }

    fn install_simple_tree(
        fixture: TestFixture,
        resource_base: u64,
    ) -> (Capability, Capability, Capability) {
        let root_perms = CapPermissions::READ
            .union(CapPermissions::REVOKE)
            .union(CapPermissions::GRANT);
        let mut root = Capability::new_root(
            ResourceType::IpcPort {
                port_id: resource_base,
            },
            fixture.pid,
            root_perms,
        );
        let child_a = root
            .derive(fixture.pid, CapPermissions::READ)
            .expect("child a derivation must succeed");
        let child_b = root
            .derive(fixture.pid, CapPermissions::READ)
            .expect("child b derivation must succeed");

        install_capability(fixture, root.clone());
        install_capability(fixture, child_a.clone());
        install_capability(fixture, child_b.clone());
        (root, child_a, child_b)
    }

    fn install_deep_tree(fixture: TestFixture, resource_base: u64) -> Capability {
        let root_perms = CapPermissions::READ
            .union(CapPermissions::REVOKE)
            .union(CapPermissions::GRANT);

        let mut root = Capability::new_root(
            ResourceType::IpcPort {
                port_id: resource_base,
            },
            fixture.pid,
            root_perms,
        );

        let mut level1 = root
            .derive(fixture.pid, CapPermissions::READ)
            .expect("level1 derivation must succeed");
        let sibling = root
            .derive(fixture.pid, CapPermissions::READ)
            .expect("sibling derivation must succeed");
        let mut level2 = level1
            .derive(fixture.pid, CapPermissions::READ)
            .expect("level2 derivation must succeed");
        let mut level3 = level2
            .derive(fixture.pid, CapPermissions::READ)
            .expect("level3 derivation must succeed");
        let level4 = level3
            .derive(fixture.pid, CapPermissions::READ)
            .expect("level4 derivation must succeed");

        install_capability(fixture, root.clone());
        install_capability(fixture, level1.clone());
        install_capability(fixture, sibling);
        install_capability(fixture, level2.clone());
        install_capability(fixture, level3.clone());
        install_capability(fixture, level4);

        root
    }

    #[test]
    fn revoke_simple_tree_reports_complete_and_post_order() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_001);

        let (root, _, _) = install_simple_tree(fixture, 95_001);
        let plan = revoke_plan::build_revoke_plan(root.handle).expect("plan must build");
        let report =
            revoke_capability(root.handle, fixture.tid).expect("revoke must return a report");

        assert_eq!(report.status, RevokeStatus::Complete);
        assert_eq!(report.revoked, plan.order);
        assert!(report.missing.is_empty());
        assert!(report.failed.is_empty());

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_deep_tree_uses_stable_execution_order() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_002);

        let root = install_deep_tree(fixture, 95_002);
        let plan_a = revoke_plan::build_revoke_plan(root.handle).expect("first plan must build");
        let plan_b = revoke_plan::build_revoke_plan(root.handle).expect("second plan must build");
        assert_eq!(plan_a.order, plan_b.order);
        assert!(
            plan_a.order.len() >= 6,
            "deep tree must include at least six capabilities in post-order"
        );

        let report =
            revoke_capability(root.handle, fixture.tid).expect("revoke must return a report");
        assert_eq!(report.revoked, plan_a.order);
        assert!(report.failed.is_empty());

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_descendant_mutation_failure_is_reported() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_003);

        let (root, child_a, _) = install_simple_tree(fixture, 95_003);
        let removed = crate::process::remove_process_capability(fixture.pid, child_a.handle);
        assert!(
            removed.is_some(),
            "test setup must remove child from process table to force mutation failure"
        );

        let report =
            revoke_capability(root.handle, fixture.tid).expect("revoke must return a report");

        assert_eq!(report.status, RevokeStatus::Partial);
        assert!(report.revoked.contains(&child_a.handle));
        assert!(report.failed.iter().any(|(handle, err)| {
            *handle == child_a.handle && *err == RevokeError::ProcessMutationFailed
        }));

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_missing_root_is_classified_as_missing() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_004);

        let missing = CapHandle::from_raw(9_500_400);
        let report =
            revoke_capability(missing, fixture.tid).expect("missing root must still return report");

        assert_eq!(report.status, RevokeStatus::Failed);
        assert_eq!(report.missing, vec![missing]);
        assert!(report.revoked.is_empty());
        assert!(report.failed.is_empty());

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_is_idempotent_on_second_execution() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_005);

        let (root, _, _) = install_simple_tree(fixture, 95_005);
        let first =
            revoke_capability(root.handle, fixture.tid).expect("first revoke must return report");
        assert_eq!(first.status, RevokeStatus::Complete);

        let second = revoke_capability(root.handle, fixture.tid)
            .expect("second revoke must still return report");
        assert_eq!(second.status, RevokeStatus::Failed);
        assert!(second.revoked.is_empty());
        assert!(second.missing.contains(&root.handle));
        assert!(lookup_capability(root.handle).is_none());

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_callback_can_query_other_subsystems_without_deadlock() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_006);

        let (root, _, _) = install_simple_tree(fixture, 95_006);
        *CALLBACK_FIXTURE.lock() = Some(fixture);
        register_revocation_callback(
            ResourceType::IpcPort { port_id: 95_006 },
            callback_probe_subsystems,
        );

        let report = revoke_capability(root.handle, fixture.tid)
            .expect("revoke must complete when callback queries subsystems");

        assert_eq!(report.status, RevokeStatus::Complete);
        assert_eq!(report.callbacks_executed(), report.revoked.len());
        assert!(report.callback_results.iter().all(|entry| entry.result.is_ok()));

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_callback_error_is_recorded_without_aborting_revoke() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_007);

        let (root, _, _) = install_simple_tree(fixture, 95_007);
        register_revocation_callback(ResourceType::IpcPort { port_id: 95_007 }, callback_error);

        let report = revoke_capability(root.handle, fixture.tid)
            .expect("revoke must return report even with callback error");

        assert_eq!(report.status, RevokeStatus::Complete);
        assert!(report.failed.is_empty());
        assert_eq!(report.callback_failures(), report.revoked.len());
        assert!(report.callback_results.iter().all(|entry| entry.result.is_err()));

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_multiple_callbacks_execute_outside_lock_in_deterministic_order() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_008);

        let (root, _, _) = install_simple_tree(fixture, 95_008);
        register_revocation_callback(ResourceType::IpcPort { port_id: 95_008 }, callback_first);
        register_revocation_callback(ResourceType::IpcPort { port_id: 95_008 }, callback_second);

        let report = revoke_capability(root.handle, fixture.tid)
            .expect("revoke must return report for multiple callbacks");

        assert_eq!(report.status, RevokeStatus::Complete);
        assert_eq!(report.callbacks_executed(), report.revoked.len() * 2);
        assert!(report.callback_results.iter().all(|entry| entry.result.is_ok()));

        let expected_trace = report
            .revoked
            .iter()
            .flat_map(|handle| [("first", handle.raw()), ("second", handle.raw())])
            .collect::<Vec<_>>();
        assert_eq!(*CALLBACK_TRACE.lock(), expected_trace);

        let expected_indices = report
            .revoked
            .iter()
            .flat_map(|_| [0usize, 1usize])
            .collect::<Vec<_>>();
        let reported_indices = report
            .callback_results
            .iter()
            .map(|entry| entry.callback_index)
            .collect::<Vec<_>>();
        assert_eq!(reported_indices, expected_indices);

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_callback_reentrant_registration_does_not_deadlock_or_loop() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_009);

        let (first_root, _, _) = install_simple_tree(fixture, 95_009);
        register_revocation_callback(
            ResourceType::IpcPort { port_id: 95_009 },
            callback_reentrant_register,
        );

        let first_report = revoke_capability(first_root.handle, fixture.tid)
            .expect("first revoke should succeed with reentrant callback");
        assert_eq!(first_report.status, RevokeStatus::Complete);
        assert_eq!(first_report.callbacks_executed(), first_report.revoked.len());

        CALLBACK_TRACE.lock().clear();

        let (second_root, _, _) = install_simple_tree(fixture, 95_009);
        let second_report = revoke_capability(second_root.handle, fixture.tid)
            .expect("second revoke should include late callback");
        assert_eq!(second_report.status, RevokeStatus::Complete);
        assert_eq!(second_report.callbacks_executed(), second_report.revoked.len() * 2);

        let trace = CALLBACK_TRACE.lock().clone();
        assert!(trace.iter().any(|(name, _)| *name == "reentrant"));
        assert!(trace.iter().any(|(name, _)| *name == "late"));

        cleanup_fixture(fixture);
    }

    #[test]
    fn revoke_callback_partial_failure_is_aggregated() {
        let _serial = TEST_SERIAL.lock();
        let fixture = setup_fixture(95_010);

        let (root, _, _) = install_simple_tree(fixture, 95_010);
        register_revocation_callback(ResourceType::IpcPort { port_id: 95_010 }, callback_error);
        register_revocation_callback(ResourceType::IpcPort { port_id: 95_010 }, callback_first);

        let report = revoke_capability(root.handle, fixture.tid)
            .expect("revoke must succeed with partial callback failures");

        assert_eq!(report.status, RevokeStatus::Complete);
        assert_eq!(report.callbacks_executed(), report.revoked.len() * 2);
        assert_eq!(report.callback_failures(), report.revoked.len());
        assert_eq!(
            report.callback_results.len() - report.callback_failures(),
            report.revoked.len()
        );

        cleanup_fixture(fixture);
    }
}
