use super::*;

// ============================================================================
// Trust root
// ============================================================================
//
// There is intentionally no "privileged process" concept here. Authority is
// not derived from boot order, PID, or process name. Every sensitive syscall
// is gated by a specific capability (see `CapRequirement`), and the authority
// to create processes or read the kernel log comes exclusively from the
// kernel-side `SystemServiceManifest` (`crate::system_manifest`).

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
    /// Caller must hold a Framebuffer (map) cap with READ. Gates obtaining the
    /// framebuffer address and mapping it. Distinct from mode-set authority.
    FramebufferMap,
    /// Caller must hold a `DisplayModeSet` cap with EXECUTE. Gates changing the
    /// active video mode. Video-mode *queries* are public.
    DisplayModeSet,
    /// Caller must hold any FsNamespace cap with READ.
    /// Used to gate kernel-internal storage syscalls (200-212 except 203)
    /// so that only fsd can reach the raw filesystem driver.
    AnyFsNamespace,
    /// Caller must hold a `SpawnSystemService` cap with EXECUTE.
    /// Necessary (not sufficient) for `SYS_SPAWN_PROCESS`; the handler also
    /// enforces the manifest declaration and `allowed_children` rules.
    SpawnSystemService,
    /// Caller must hold a `SpawnFromPath` cap with EXECUTE.
    /// Necessary (not sufficient) for `SYS_SPAWN_FROM_PATH`; the handler also
    /// enforces path/extension/system-directory rules.
    SpawnFromPath,
    /// Caller must hold a `ReadKernelLog` cap with READ.
    /// Gates `SYS_READ_KLOG`, which exposes sensitive system-wide information.
    ReadKernelLog,
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

        CapRequirement::FramebufferMap =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::Framebuffer { .. }),
            ),

        CapRequirement::DisplayModeSet =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::EXECUTE,
                |r| matches!(r, ResourceType::DisplayModeSet),
            ),

        CapRequirement::AnyFsNamespace =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::FsNamespace { .. }),
            ),

        CapRequirement::SpawnSystemService =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::EXECUTE,
                |r| matches!(r, ResourceType::SpawnSystemService),
            ),

        CapRequirement::SpawnFromPath =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::EXECUTE,
                |r| matches!(r, ResourceType::SpawnFromPath),
            ),

        CapRequirement::ReadKernelLog =>
            crate::thread::validate_thread_capability_by_type(
                caller,
                CapPermissions::READ,
                |r| matches!(r, ResourceType::ReadKernelLog),
            ),
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

        // ── Authenticated IPC support (PR2) ───────────────────────────────
        // recv_envelope only reveals who sent to the caller's OWN port, so it
        // is no more sensitive than recv. The query syscalls expose read-only
        // facts (manifest name allowlist, port owner, process liveness) that
        // namesvc needs to authorise registration; none grant authority.
        // SYS_IPC_CREATE_PORT_WITH_ID additionally enforces the ReservedPort
        // capability inside its handler for ids 1..=255.
        SYS_IPC_RECV_ENVELOPE | SYS_SERVICE_NAME_ALLOWED
        | SYS_IPC_PORT_OWNER | SYS_PROCESS_ALIVE
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
        // Gated by InputDevice capability type. These caps are granted only to
        // the compositor (ui_shell) through the SystemServiceManifest; ordinary
        // apps hold none and are denied EPERM here. There are no ambient grants.
        SYS_MOUSE_POLL | SYS_MOUSE_GET_ID   => Requires(InputMouse),
        SYS_KEYBOARD_POLL                    => Requires(InputKeyboard),

        // ── Framebuffer / display ──────────────────────────────────────────
        // Gated by Framebuffer cap, granted only to the compositor (ui_shell)
        // via the manifest's FramebufferMap environment grant.
        SYS_GET_FRAMEBUFFER | SYS_MAP_FRAMEBUFFER
            => Requires(FramebufferMap),
        SYS_SET_VIDEO_MODE
            => Requires(DisplayModeSet),

        // Video info queries — non-sensitive, public.
        SYS_GET_VIDEO_MODES | SYS_GET_CURRENT_VIDEO_MODE | SYS_VIDEO_MODE_COUNT
            => ExplicitlyUnrestricted,

        // ── Infrastructure / Networking Phase 1 ───────────────────────────
        // Handlers perform parametric capability checks.
        SYS_PCI_GET_BAR | SYS_DEVICE_BIND_IRQ | SYS_IRQ_LISTEN
        | SYS_DMA_ALLOC | SYS_DMA_MAP | SYS_DMA_FREE | SYS_MAP_MMIO
        | SYS_IRQ_ACK | SYS_PCI_QUERY_DEVICE
            => ExplicitlyUnrestricted,

        // ── Raw I/O ports ─────────────────────────────────────────────────
        // Gated in-handler by `validate_io_port_access`, which requires an
        // explicit IoPort (or containing Device) capability with the matching
        // permission. An ordinary process holds none and is denied EPERM by the
        // handler. Classified here explicitly so these syscalls do not rely on
        // the fail-closed wildcard (which would make the handler unreachable).
        SYS_IO_PORT_READ | SYS_IO_PORT_WRITE
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
        // Deny-by-default: spawn is a kernel-authorized operation. The cap is
        // necessary but not sufficient — the handlers additionally enforce the
        // SystemServiceManifest (declaration + allowed_children) for
        // SYS_SPAWN_PROCESS and path/system-directory rules for
        // SYS_SPAWN_FROM_PATH.
        SYS_SPAWN_PROCESS   => Requires(SpawnSystemService),
        SYS_SPAWN_FROM_PATH => Requires(SpawnFromPath),

        // ── Sensitive system information ───────────────────────────────────
        // Kernel log exposes internal addresses, thread states, and error
        // details.  Gated by an explicit ReadKernelLog capability — no generic
        // "privileged process" concept.
        SYS_READ_KLOG => Requires(ReadKernelLog),

        // Process enumeration — used by service_manager for health monitoring.
        SYS_LIST_PROCESSES | SYS_GET_PROCESS_COUNT
            => ExplicitlyUnrestricted,

        // Non-sensitive system information — public.
        SYS_GET_TICKS | SYS_GET_MEMORY_INFO | SYS_GET_CPU_BRAND
        | SYS_GET_CPU_ID | SYS_GET_CPU_COUNT
        | SYS_SET_THREAD_AFFINITY | SYS_GET_THREAD_AFFINITY
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

    // The capability being delegated must itself be owned by the sender AND
    // carry GRANT. Holding GRANT on some *other* capability must never authorise
    // delegating this one — that would defeat per-capability least privilege and
    // let a sender escalate a non-delegable handle. `validate_thread_capability`
    // checks both ownership (handle present in the caller's table) and the
    // GRANT permission on that exact handle in one step.
    match crate::thread::validate_thread_capability(
        sender,
        cap_handle,
        crate::cap::CapPermissions::GRANT,
    ) {
        Ok(()) => {}
        Err(crate::cap::CapError::PermissionDenied) => {
            log_warn!(
                "syscall",
                "ipc_send_with_cap: denied (capability cap={:#x} lacks GRANT permission)",
                cap_handle_raw
            );
            return Err(EPERM);
        }
        Err(_) => {
            log_warn!(
                "syscall",
                "ipc_send_with_cap: denied (sender does not own capability cap={:#x})",
                cap_handle_raw
            );
            return Err(EPERM);
        }
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
            // A thread may only mint a Thread capability for itself. Forging a
            // Thread cap for another thread is denied by default — there is no
            // privileged process that can bypass this.
            if tid != caller {
                log_warn!(
                    "syscall",
                    "cap_create: DENIED — attempt to forge Thread cap \
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
            // IRQ capabilities are not mintable from userspace. They are issued
            // by the kernel to drivers through dedicated paths; allowing
            // userspace to forge them would grant arbitrary hardware authority.
            // Denied by default now that the privileged-process escape hatch is
            // gone.
            log_warn!(
                "syscall",
                "cap_create: DENIED — userspace cannot forge Irq cap \
                 irq={} (caller={})",
                resource_id,
                caller
            );
            Err(EPERM)
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

/// Runtime mirror of the `#[cfg(test)]` policy invariants below: the kernel
/// library builds with `test = false`, so these security-critical assertions
/// also execute as boot self-tests under the QEMU CI gate (a panic here trips
/// the smoke gate's forbidden markers).
pub(super) fn run_security_policy_selftests() {
    use super::{
        SYS_IPC_CREATE_PORT_WITH_ID, SYS_IPC_PORT_OWNER, SYS_IPC_RECV_ENVELOPE,
        SYS_PROCESS_ALIVE, SYS_READ_KLOG, SYS_SERVICE_NAME_ALLOWED, SYS_SPAWN_FROM_PATH,
        SYS_SPAWN_PROCESS,
    };

    // Authenticated-IPC support syscalls stay classified (never fail-closed).
    for num in [
        SYS_IPC_RECV_ENVELOPE,
        SYS_SERVICE_NAME_ALLOWED,
        SYS_IPC_PORT_OWNER,
        SYS_PROCESS_ALIVE,
        SYS_IPC_CREATE_PORT_WITH_ID,
    ] {
        assert!(
            matches!(syscall_policy(num), SysPolicy::ExplicitlyUnrestricted),
            "syscall {} must be classified as unrestricted at the table",
            num
        );
    }

    // Spawn must be capability-gated, never ambient.
    assert!(matches!(
        syscall_policy(SYS_SPAWN_PROCESS),
        SysPolicy::Requires(CapRequirement::SpawnSystemService)
    ));
    assert!(matches!(
        syscall_policy(SYS_SPAWN_FROM_PATH),
        SysPolicy::Requires(CapRequirement::SpawnFromPath)
    ));

    // Kernel log requires the explicit cap; no generic privileged gate.
    assert!(matches!(
        syscall_policy(SYS_READ_KLOG),
        SysPolicy::Requires(CapRequirement::ReadKernelLog)
    ));

    // Unknown syscalls stay fail-closed.
    assert!(matches!(
        syscall_policy(u64::MAX),
        SysPolicy::ExplicitlyDenied(_)
    ));

    // SYS_SPAWN_FROM_PATH must never reach system-service images.
    assert!(super::is_system_service_path("/drivers/fsd.atxf"));
    assert!(super::is_system_service_path("/bin/init.atxf"));
    assert!(!super::is_system_service_path("/apps/user/fileman.atxf"));
    assert!(!super::is_system_service_path("/apps/system/terminal.atxf"));
}

#[cfg(test)]
mod tests {
    use super::{syscall_policy, CapRequirement, SysPolicy};
    use super::super::{SYS_READ_KLOG, SYS_SPAWN_FROM_PATH, SYS_SPAWN_PROCESS};
    use super::super::{
        SYS_IPC_CREATE_PORT_WITH_ID, SYS_IPC_PORT_OWNER, SYS_IPC_RECV_ENVELOPE,
        SYS_PROCESS_ALIVE, SYS_SERVICE_NAME_ALLOWED,
    };

    /// The authenticated-IPC support syscalls must be classified (never
    /// fail-closed/denied). Reserved-port enforcement happens inside the
    /// create_port_with_id handler, so it stays "unrestricted" at the table.
    #[test]
    fn authenticated_ipc_syscalls_are_classified() {
        for num in [
            SYS_IPC_RECV_ENVELOPE,
            SYS_SERVICE_NAME_ALLOWED,
            SYS_IPC_PORT_OWNER,
            SYS_PROCESS_ALIVE,
            SYS_IPC_CREATE_PORT_WITH_ID,
        ] {
            assert!(
                matches!(syscall_policy(num), SysPolicy::ExplicitlyUnrestricted),
                "syscall {} must be classified as unrestricted at the table",
                num
            );
        }
    }

    /// Spawn syscalls must NOT be unrestricted: they require a specific cap.
    #[test]
    fn spawn_syscalls_are_capability_gated() {
        assert!(matches!(
            syscall_policy(SYS_SPAWN_PROCESS),
            SysPolicy::Requires(CapRequirement::SpawnSystemService)
        ));
        assert!(matches!(
            syscall_policy(SYS_SPAWN_FROM_PATH),
            SysPolicy::Requires(CapRequirement::SpawnFromPath)
        ));
    }

    /// Reading the kernel log requires the explicit ReadKernelLog cap; there is
    /// no generic "privileged process" gate any more.
    #[test]
    fn klog_requires_read_kernel_log_cap() {
        assert!(matches!(
            syscall_policy(SYS_READ_KLOG),
            SysPolicy::Requires(CapRequirement::ReadKernelLog)
        ));
    }

    /// Unknown syscalls remain fail-closed.
    #[test]
    fn unknown_syscall_is_denied() {
        assert!(matches!(
            syscall_policy(u64::MAX),
            SysPolicy::ExplicitlyDenied(_)
        ));
    }

    /// SYS_SPAWN_FROM_PATH must never be a back door to system-service images.
    #[test]
    fn system_service_paths_are_rejected_for_path_spawn() {
        assert!(super::super::is_system_service_path("/drivers/fsd.atxf"));
        assert!(super::super::is_system_service_path("/bin/init.atxf"));
        assert!(super::super::is_system_service_path("/sbin/netd.atxf"));
        assert!(!super::super::is_system_service_path("/apps/user/fileman.atxf"));
        assert!(!super::super::is_system_service_path("/apps/system/terminal.atxf"));
    }
}
