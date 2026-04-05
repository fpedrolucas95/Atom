use super::*;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrd};

// ============================================================================
// Privileged Process Registry
// ============================================================================

static PRIVILEGED_PID: AtomicU64 = AtomicU64::new(0);

/// Register the first spawned userspace process as the privileged (init) process.
/// Must be called exactly once; subsequent calls are no-ops (logged as warnings).
pub(super) fn register_privileged_process(pid: crate::thread::ThreadId) {
    let result = PRIVILEGED_PID.compare_exchange(
        0,
        pid.raw(),
        AtomicOrd::SeqCst,
        AtomicOrd::SeqCst,
    );
    match result {
        Ok(_) => log_info!("syscall", "Privileged process registered: pid={}", pid),
        Err(existing) => log_warn!(
            "syscall",
            "register_privileged_process called again (existing={}, new={}) — ignored",
            existing,
            pid
        ),
    }
}

/// Returns true if the current thread is the registered privileged process (init).
#[inline]
pub(super) fn has_privilege() -> bool {
    let privileged = PRIVILEGED_PID.load(AtomicOrd::SeqCst);
    if privileged == 0 {
        return false;
    }
    crate::sched::current_thread()
        .map(|t| t.raw() == privileged)
        .unwrap_or(false)
}

// ============================================================================
// Syscall Policy Table
// ============================================================================

/// Discrete capability requirements used by the policy table.
#[derive(Debug, Clone, Copy)]
pub(super) enum CapRequirement {
    /// Caller must hold an InputDevice{Mouse} cap with READ.
    InputMouse,
    /// Caller must hold an InputDevice{Keyboard} cap with READ.
    InputKeyboard,
    /// Caller must hold any Framebuffer cap (any dimensions) with READ.
    AnyFramebuffer,
    /// Caller must hold any FsNamespace cap with READ.
    /// Used to gate kernel-internal storage syscalls (200-212 except 203)
    /// so that only fsd can reach the raw filesystem driver.
    AnyFsNamespace,
    /// Caller must be the registered privileged process (init bootstrap).
    /// Covers operations that produce sensitive system-wide information.
    PrivilegedOnly,
}

/// Per-syscall policy decision returned by `syscall_policy`.
#[derive(Debug)]
pub(super) enum SysPolicy {
    /// No capability gate — handler is solely responsible for safety.
    ExplicitlyUnrestricted,
    /// Gate: caller must satisfy the given CapRequirement.
    Requires(CapRequirement),
    /// Fail-closed decision for unknown/unclassified syscalls.
    ExplicitlyDenied(u64),
}

/// Evaluate `req` against the current thread.  Returns true if the
/// requirement is satisfied.
pub(super) fn check_cap_requirement(req: CapRequirement) -> bool {
    use crate::cap::{CapPermissions, ResourceType};
    use crate::cap::InputDeviceType;

    let caller = match crate::sched::current_thread() {
        Some(t) => t,
        None => return false,
    };

    match req {
        CapRequirement::InputMouse =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::InputDevice {
                    device_type: InputDeviceType::Mouse
                }),
            ),

        CapRequirement::InputKeyboard =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::InputDevice {
                    device_type: InputDeviceType::Keyboard
                }),
            ),

        CapRequirement::AnyFramebuffer =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::Framebuffer { .. }),
            ),

        CapRequirement::AnyFsNamespace =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::FsNamespace { .. }),
            ),

        CapRequirement::PrivilegedOnly => has_privilege(),
    }
}

/// Return the policy for `syscall_num`.
///
/// Every syscall number that the kernel handles should appear here.
/// Unclassified syscalls are denied by default (fail-closed).
pub(super) fn syscall_policy(num: u64) -> SysPolicy {
    use SysPolicy::*;
    use CapRequirement::*;

    match num {
        // ── Thread management ──────────────────────────────────────────────
        // yield/exit/sleep carry no sensitive effect; thread_create's handler
        // already validates Thread(WRITE) capability.
        SYS_THREAD_YIELD | SYS_THREAD_EXIT | SYS_THREAD_SLEEP | SYS_THREAD_CREATE
            => ExplicitlyUnrestricted,

        // ── IPC ────────────────────────────────────────────────────────────
        // All IPC handlers perform parametric port-ownership checks; no
        // additional class-level gate needed here.
        SYS_IPC_CREATE_PORT | SYS_IPC_CLOSE_PORT
        | SYS_IPC_SEND | SYS_IPC_RECV
        | SYS_IPC_SEND_WITH_CAP
        | SYS_IPC_SEND_BATCH | SYS_IPC_RECV_BATCH
        | SYS_IPC_SEND_ASYNC | SYS_IPC_TRY_RECV
        | SYS_IPC_TRACE_READ | SYS_IPC_PORT_STATS
        | SYS_IPC_WAIT_ANY | SYS_IPC_CREATE_PORT_WITH_ID
            => ExplicitlyUnrestricted,

        // ── Capability management ──────────────────────────────────────────
        // Individual handlers enforce ownership / derivation rules.
        // (sys_cap_create is additionally guarded in its own body.)
        SYS_CAP_CREATE | SYS_CAP_CHECK | SYS_CAP_REVOKE | SYS_CAP_DERIVE
        | SYS_CAP_LIST | SYS_CAP_TRANSFER
        | SYS_CAP_QUERY_PARENT | SYS_CAP_QUERY_CHILDREN
            => ExplicitlyUnrestricted,

        // ── Shared memory / address space ──────────────────────────────────
        SYS_SHARED_REGION_CREATE | SYS_SHARED_REGION_MAP
        | SYS_SHARED_REGION_UNMAP | SYS_SHARED_REGION_DESTROY
        | SYS_ADDRSPACE_CREATE | SYS_ADDRSPACE_DESTROY
        | SYS_MAP_REGION | SYS_UNMAP_REGION | SYS_REMAP_REGION
        | SYS_REGISTER_FAULT_HANDLER
        | SYS_MMAP | SYS_MUNMAP | SYS_MPROTECT | SYS_BRK | SYS_FORK
            => ExplicitlyUnrestricted,

        // ── Input devices ─────────────────────────────────────────────────
        // Gated by InputDevice capability type.
        // Note: all processes currently receive these caps at spawn time
        // (over-provisioning — see P1-A TODO in spawn_process_internal).
        // Fixing over-provisioning will automatically tighten access here
        // without further changes to this table.
        SYS_MOUSE_POLL | SYS_MOUSE_GET_ID   => Requires(InputMouse),
        SYS_KEYBOARD_POLL                    => Requires(InputKeyboard),

        // ── Framebuffer / display ──────────────────────────────────────────
        // Gated by Framebuffer cap.  Same over-provisioning caveat applies.
        SYS_GET_FRAMEBUFFER | SYS_MAP_FRAMEBUFFER | SYS_SET_VIDEO_MODE
            => Requires(AnyFramebuffer),

        // Video info queries — non-sensitive, public.
        SYS_GET_VIDEO_MODES | SYS_GET_CURRENT_VIDEO_MODE | SYS_VIDEO_MODE_COUNT
            => ExplicitlyUnrestricted,

        // ── Infrastructure / Networking Phase 1 ───────────────────────────
        // Handlers perform parametric capability checks.
        SYS_PCI_GET_BAR | SYS_DEVICE_BIND_IRQ | SYS_IRQ_LISTEN
        | SYS_DMA_ALLOC | SYS_DMA_MAP | SYS_DMA_FREE | SYS_MAP_MMIO
        | SYS_IRQ_ACK
            => ExplicitlyUnrestricted,

        // ── IRQ management ────────────────────────────────────────────────
        // Handler uses ALLOWED_IRQS allowlist today; no additional class gate.
        SYS_REGISTER_IRQ_HANDLER | SYS_UNREGISTER_IRQ_HANDLER | SYS_GET_IRQ_COUNT
            => ExplicitlyUnrestricted,

        // ── Kernel FS backend (for fsd only) ──────────────────────────────
        // These syscalls bypass fsd and talk directly to the kernel FAT32
        // driver.  Gated by FsNamespace cap which is granted only to "fsd"
        // at spawn time.  This is the first hard enforcement of the rule
        // "only fsd calls kern_fs_*".
        SYS_KERN_FS_READ_FILE | SYS_KERN_FS_LIST_DIR | SYS_KERN_FS_STAT_PATH
        | SYS_KERN_FS_WRITE_FILE | SYS_KERN_FS_MKDIR | SYS_KERN_FS_RMDIR
        | SYS_KERN_FS_UNLINK | SYS_KERN_FS_RENAME | SYS_KERN_FS_SYNC
        | SYS_KERN_BLOCK_READ | SYS_KERN_BLOCK_WRITE | SYS_KERN_BLOCK_FLUSH
            => Requires(AnyFsNamespace),

        // ── POSIX FS (via fsd IPC) ─────────────────────────────────────────
        SYS_FS_OPEN | SYS_FS_CLOSE | SYS_FS_READ | SYS_FS_WRITE | SYS_FS_SEEK
        | SYS_FS_STAT | SYS_FS_FSTAT
        | SYS_FS_MKDIR | SYS_FS_RMDIR | SYS_FS_UNLINK | SYS_FS_RENAME
        | SYS_FS_READDIR | SYS_FS_TRUNCATE | SYS_FS_FSYNC
        | SYS_FS_MOUNT | SYS_FS_UMOUNT | SYS_FS_CHMOD
        | SYS_FS_DUP | SYS_FS_DUP2
        | SYS_FS_LINK | SYS_FS_SYMLINK | SYS_FS_READLINK
        | SYS_FS_UTIMES | SYS_FS_STATVFS
            => ExplicitlyUnrestricted,

        // ── Process / spawn ───────────────────────────────────────────────
        // TODO: restrict SYS_SPAWN_* to processes holding a future Spawn cap.
        //       For now, service_manager and app_launcher call these without
        //       a dedicated capability.
        SYS_SPAWN_PROCESS | SYS_SPAWN_FROM_PATH
            => ExplicitlyUnrestricted,

        // ── Sensitive system information ───────────────────────────────────
        // Kernel log exposes internal addresses, thread states, and error
        // details.  Restricted to the privileged (init) process.
        SYS_READ_KLOG => Requires(PrivilegedOnly),

        // Process enumeration is also privileged; non-init processes should
        // query the service_manager via IPC instead of the kernel directly.
        SYS_LIST_PROCESSES | SYS_GET_PROCESS_COUNT
            => Requires(PrivilegedOnly),

        // Non-sensitive system information — public.
        SYS_GET_TICKS | SYS_GET_MEMORY_INFO | SYS_GET_CPU_BRAND
            => ExplicitlyUnrestricted,

        // Debug log — no cap required (process names its own output).
        SYS_DEBUG_LOG => ExplicitlyUnrestricted,

        // ── Unclassified ──────────────────────────────────────────────────
        // Fail-closed by construction: unknown rows are denied.
        _ => ExplicitlyDenied(EPERM),
    }
}

pub(super) fn authorize_syscall_class(syscall_num: u64, log_origin: &'static str) -> Option<u64> {
    match syscall_policy(syscall_num) {
        SysPolicy::ExplicitlyUnrestricted => None,
        SysPolicy::Requires(req) => {
            if check_cap_requirement(req) {
                None
            } else {
                log_warn!(
                    log_origin,
                    "Policy gate DENIED: syscall={} tid={:?} requirement={:?}",
                    syscall_num,
                    crate::sched::current_thread(),
                    req
                );
                Some(EPERM)
            }
        }
        SysPolicy::ExplicitlyDenied(errno) => Some(errno),
    }
}

// ============================================================================
// Inline policy check extractions
// ============================================================================

/// Validates that the calling thread holds an IoPort capability for `port` with
/// the specified permission. Returns `Ok(())` on success or `Err(EPERM)` on
/// failure, logging a warning with context in the denial case.
pub(super) fn validate_io_port_access(
    port: u16,
    required_perm: crate::cap::CapPermissions,
) -> Result<(), u64> {
    let caller = crate::sched::current_thread().ok_or(EPERM)?;

    // Check 1: Explicit IoPort capability
    let has_io_cap = crate::thread::validate_thread_capability_by_type(
        caller,
        required_perm,
        |resource| matches!(resource, crate::cap::ResourceType::IoPort { port: p } if *p == port),
    );
    if has_io_cap {
        return Ok(());
    }

    // Check 2: Device capability that contains this port (for VirtIO Legacy I/O BARs)
    let has_device_cap = crate::thread::validate_thread_capability_by_type(
        caller,
        required_perm,
        |resource| {
            if let crate::cap::ResourceType::Device { bdf } = resource {
                let bus = (bdf >> 8) as u8;
                let dev = ((bdf >> 3) & 0x1f) as u8;
                let func = (bdf & 0x07) as u8;
                // Check if any BAR of this device is an I/O BAR and contains the requested port
                for i in 0..6 {
                    if let Some(bar) = crate::drivers::pci::get_bar_info(bus, dev, func, i) {
                        if !bar.is_mmio && port >= bar.base as u16 && (port as u64) < bar.base + bar.size {
                            return true;
                        }
                    }
                }
            }
            false
        }
    );

    if !has_device_cap {
        log_warn!(
            "syscall",
            "io_port access denied: port=0x{:X} perm={:?} caller={}",
            port,
            required_perm,
            caller
        );
        return Err(EPERM);
    }
    Ok(())
}

pub(super) fn validate_thread_create_capability(
    caller: crate::thread::ThreadId,
    log_origin: &'static str,
) -> Result<(), u64> {
    let has_permission = crate::thread::validate_thread_capability_by_type(
        caller,
        crate::cap::CapPermissions::WRITE,
        |resource| matches!(resource, crate::cap::ResourceType::Thread(_)),
    );

    if !has_permission {
        log_warn!(
            log_origin,
            "thread_create denied: missing Thread capability with WRITE permission (caller={})",
            caller
        );
        return Err(EPERM);
    }

    log_debug!(
        log_origin,
        "thread_create capability validated (caller={})",
        caller
    );

    Ok(())
}

pub(super) fn validate_ipc_send_with_cap_permissions(
    sender: crate::thread::ThreadId,
    port_id: crate::ipc::PortId,
    port_id_raw: u64,
    cap_handle: crate::cap::CapHandle,
    cap_handle_raw: u64,
) -> Result<(), u64> {
    let has_port_permission = crate::thread::validate_thread_capability_by_type(
        sender,
        crate::cap::CapPermissions::WRITE,
        |resource| {
            matches!(
                resource,
                crate::cap::ResourceType::IpcPort { port_id: id }
                    if *id == port_id.raw()
            )
        },
    );

    if !has_port_permission {
        log_warn!(
            "syscall",
            "ipc_send_with_cap: denied (missing IPCPortCap::WRITE, sender={:?}, port={})",
            sender,
            port_id_raw
        );
        return Err(EPERM);
    }

    if !crate::thread::thread_has_capability(sender, cap_handle) {
        log_warn!(
            "syscall",
            "ipc_send_with_cap: denied (sender does not own capability cap={:#x})",
            cap_handle_raw
        );
        return Err(EPERM);
    }

    let has_grant_permission = crate::thread::validate_thread_capability_by_type(
        sender,
        crate::cap::CapPermissions::GRANT,
        |_resource| true,
    );

    if !has_grant_permission {
        log_warn!(
            "syscall",
            "ipc_send_with_cap: denied (missing GRANT permission)"
        );
        return Err(EPERM);
    }

    Ok(())
}

pub(super) fn resolve_cap_create_resource(
    resource_type: u64,
    resource_id: u64,
    caller: crate::thread::ThreadId,
) -> Result<crate::cap::ResourceType, u64> {
    match resource_type {
        0 => {
            let tid = crate::thread::ThreadId::from_raw(resource_id);
            if tid != caller && !has_privilege() {
                log_warn!(
                    "syscall",
                    "cap_create: DENIED — unprivileged attempt to forge Thread cap \
                     for tid={} by caller={}",
                    resource_id,
                    caller
                );
                return Err(EPERM);
            }
            Ok(crate::cap::ResourceType::Thread(tid))
        }
        2 => {
            Ok(crate::cap::ResourceType::IpcPort { port_id: resource_id })
        }
        3 => {
            if !has_privilege() {
                log_warn!(
                    "syscall",
                    "cap_create: DENIED — unprivileged attempt to create Irq cap \
                     irq={} by caller={}",
                    resource_id,
                    caller
                );
                return Err(EPERM);
            }
            if resource_id > 255 {
                log_warn!(
                    "syscall",
                    "cap_create: invalid IRQ number {}",
                    resource_id
                );
                return Err(EINVAL);
            }
            Ok(crate::cap::ResourceType::Irq {
                irq_num: resource_id as u8,
            })
        }
        _ => {
            log_warn!(
                "syscall",
                "cap_create: unsupported resource type {}",
                resource_type
            );
            Err(ENOSYS)
        }
    }
}

pub(super) fn validate_cap_query_ownership(
    caller: crate::thread::ThreadId,
    handle: crate::cap::CapHandle,
    handle_raw: u64,
    operation: &'static str,
) -> Result<(), u64> {
    if !crate::thread::thread_has_capability(caller, handle) {
        log_warn!(
            "syscall",
            "{}: denied (caller does not own capability handle={:#x})",
            operation,
            handle_raw
        );
        return Err(EPERM);
    }

    Ok(())
}

const ALLOWED_IRQS: [u8; 2] = [1, 12];

pub(super) fn validate_irq_registration(irq: u8) -> Result<(), u64> {
    if !ALLOWED_IRQS.contains(&irq) {
        log_warn!(
            "syscall",
            "Attempt to register handler for disallowed IRQ {}",
            irq
        );
        return Err(EPERM);
    }

    Ok(())
}

#[inline]
pub(super) fn validate_irq_owner(
    owner: crate::thread::ThreadId,
    caller: crate::thread::ThreadId,
) -> Result<(), u64> {
    if owner != caller {
        return Err(EPERM);
    }

    Ok(())
}

pub(super) fn require_shared_region_caller_process(
    caller: crate::thread::ThreadId,
    operation: &'static str,
) -> Result<crate::process::ProcessId, u64> {
    match crate::thread::get_thread_process_id(caller) {
        Some(process_id) => Ok(process_id),
        None => {
            log_error!(
                "syscall",
                "{}: thread {} has no process_id",
                operation,
                caller
            );
            Err(EPERM)
        }
    }
}

pub(super) fn enforce_vma_memory_operation_allowed(
    process_id: crate::process::ProcessId,
    tid: crate::thread::ThreadId,
) -> Result<(), u64> {
    if !crate::process::process_allows_memory_operations(process_id) {
        log_warn!(
            "syscall",
            "[VMA_BLOCKED] pid={} tid={} reason=process_terminating",
            process_id,
            tid
        );
        return Err(EPERM);
    }

    Ok(())
}

pub(super) fn current_fd_owner_context() -> Result<super::FdOwnerContext, super::FdContextError> {
    let tid = crate::sched::current_thread().ok_or(super::FdContextError::NoCurrentThread)?;
    let process_id = crate::thread::get_thread_process_id(tid).ok_or(super::FdContextError::NoCurrentProcess)?;
    let address_space_pml4 =
        crate::thread::get_thread_address_space(tid).ok_or(super::FdContextError::CorruptedState)?;
    if address_space_pml4 == 0 {
        return Err(super::FdContextError::CorruptedState);
    }

    let process = crate::process::get_process(process_id).ok_or(super::FdContextError::CorruptedState)?;
    if process.pml4_phys == 0 {
        return Err(super::FdContextError::CorruptedState);
    }
    if process.pml4_phys != address_space_pml4 {
        return Err(super::FdContextError::CorruptedState);
    }

    Ok(super::FdOwnerContext {
        process_id,
        address_space_pml4,
    })
}

pub(super) fn fd_owner_process(
    fd: &super::KernelFd,
) -> Result<crate::process::ProcessId, super::FdContextError> {
    fd.owner_process.ok_or(super::FdContextError::InvalidOwner)
}

pub(super) fn validate_fd_process_alignment(
    fd: &super::KernelFd,
) -> Result<(), super::FdContextError> {
    if !fd.in_use {
        return Ok(());
    }

    let process_id = fd_owner_process(fd)?;
    let process = crate::process::get_process(process_id).ok_or(super::FdContextError::CorruptedState)?;
    if process.pml4_phys == 0 {
        return Err(super::FdContextError::CorruptedState);
    }
    Ok(())
}

pub(super) fn validate_fd_ownership(
    idx: usize,
    table: &[super::KernelFd; super::MAX_KERNEL_FDS],
) -> Result<(), super::FdPathError> {
    let caller = current_fd_owner_context()?;
    validate_fd_process_alignment(&table[idx])?;

    if fd_owner_process(&table[idx])? != caller.process_id {
        return Err(super::FdContextError::OwnershipMismatch.into());
    }

    Ok(())
}

pub(super) fn validate_fd_ownership_with_owner(
    idx: usize,
    table: &[super::KernelFd; super::MAX_KERNEL_FDS],
    caller: super::FdOwnerContext,
) -> Result<(), super::FdPathError> {
    validate_fd_process_alignment(&table[idx])?;

    if fd_owner_process(&table[idx])? != caller.process_id {
        return Err(super::FdContextError::OwnershipMismatch.into());
    }

    Ok(())
}
