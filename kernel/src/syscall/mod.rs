// kernel/src/syscall/mod.rs
//
// System Call Subsystem
//
// Implements the x86_64 syscall entry, dispatch, and high-level syscall logic
// for the kernel. This module is the primary boundary between user space and
// kernel space, enforcing privilege separation and capability-based security.
//
// Key responsibilities:
// - Configure the CPU syscall mechanism using MSRs (STAR, LSTAR, SFMASK, EFER)
// - Define the global syscall ABI and numeric syscall identifiers
// - Dispatch syscalls from user space to Rust kernel handlers
// - Translate kernel/domain errors into stable user-visible error codes
//
// Architecture and entry setup:
// - Uses the `SYSCALL/SYSRET` fast path (x86_64)
// - `MSR_STAR` defines user ↔ kernel code segment transitions
// - `MSR_LSTAR` points to the assembly-level syscall entry stub
// - `MSR_SFMASK` masks IF/TF on entry to prevent user-controlled flags
// - Enables syscall support by setting EFER.SCE
//
// Dispatch model:
// - All syscalls funnel through `rust_syscall_dispatcher`
// - Syscall number and up to 6 arguments are passed in registers
// - A single `match` statement provides explicit, auditable routing
// - Unknown syscalls return `ENOSYS`
// - Extensive serial logging aids early debugging and tracing
//
// Design principles:
// - Capability-oriented security: most syscalls validate ownership and
//   permissions via thread-bound capabilities
// - Explicit error handling with POSIX-like error codes
// - Clear separation between syscall glue and subsystem logic
// - Fail-safe defaults: invalid input typically yields `EINVAL` or `EPERM`
//
// Subsystem coverage:
// - Thread management (yield, exit, sleep, create)
// - IPC (ports, send/recv, async, batching, tracing, stats)
// - Capability lifecycle (create, check, revoke, derive, transfer, query)
// - Shared memory regions (create/map/unmap/destroy)
// - Address space management and virtual memory region mapping
//
// Capability semantics:
// - Capabilities are validated per-thread at syscall time
// - WRITE/READ/GRANT permissions are enforced where applicable
// - Delegation via IPC supports both MOVE and GRANT-with-reduction
// - Many checks are marked MVP-friendly, allowing gradual hardening
//
// Correctness and safety notes:
// - User pointers are copied explicitly into kernel-owned buffers
// - Blocking syscalls interact carefully with the scheduler and timer ticks
// - Misconfiguration of syscall MSRs can cause fatal faults, making `init()`
//   strictly early-boot only
// - This module assumes interrupts and GDT are already initialized
//
// Future considerations:
// - Stricter validation of user pointers and memory regions
// - Reduction of logging in production builds
// - Per-process syscall filtering or sandboxing

#![allow(dead_code)]

mod policy;

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::gdt::{KERNEL_CODE_SELECTOR, USER_CODE_SELECTOR};
use crate::{log_debug, log_info, log_warn, log_error, log_panic};

const MSR_STAR: u32 = 0xC000_0081;
const MSR_LSTAR: u32 = 0xC000_0082;
const MSR_SFMASK: u32 = 0xC000_0084;

pub const SYS_THREAD_YIELD: u64 = 0;
pub const SYS_THREAD_EXIT: u64 = 1;
pub const SYS_THREAD_SLEEP: u64 = 2;
pub const SYS_THREAD_CREATE: u64 = 3;
pub const SYS_IPC_CREATE_PORT: u64 = 4;
pub const SYS_IPC_CLOSE_PORT: u64 = 5;
pub const SYS_IPC_SEND: u64 = 6;
pub const SYS_IPC_RECV: u64 = 7;
pub const SYS_CAP_CREATE: u64 = 8;
pub const SYS_CAP_CHECK: u64 = 9;
pub const SYS_CAP_REVOKE: u64 = 10;
pub const SYS_CAP_DERIVE: u64 = 11;
pub const SYS_CAP_LIST: u64 = 12;
pub const SYS_CAP_TRANSFER: u64 = 13;
pub const SYS_IPC_SEND_WITH_CAP: u64 = 14;
pub const SYS_CAP_QUERY_PARENT: u64 = 15;
pub const SYS_CAP_QUERY_CHILDREN: u64 = 16;
pub const SYS_SHARED_REGION_CREATE: u64 = 17;
pub const SYS_SHARED_REGION_MAP: u64 = 18;
pub const SYS_SHARED_REGION_UNMAP: u64 = 19;
pub const SYS_SHARED_REGION_DESTROY: u64 = 20;
pub const SYS_IPC_SEND_BATCH: u64 = 21;
pub const SYS_IPC_RECV_BATCH: u64 = 22;
pub const SYS_IPC_SEND_ASYNC: u64 = 23;
pub const SYS_IPC_TRY_RECV: u64 = 24;
pub const SYS_IPC_TRACE_READ: u64 = 25;
pub const SYS_IPC_PORT_STATS: u64 = 26; 
pub const SYS_ADDRSPACE_CREATE: u64 = 27;
pub const SYS_ADDRSPACE_DESTROY: u64 = 28; 
pub const SYS_MAP_REGION: u64 = 29;
pub const SYS_UNMAP_REGION: u64 = 30;
pub const SYS_REMAP_REGION: u64 = 31;
pub const SYS_REGISTER_FAULT_HANDLER: u64 = 32;
pub const SYS_MOUSE_POLL: u64 = 33;
pub const SYS_IO_PORT_READ: u64 = 34;
pub const SYS_IO_PORT_WRITE: u64 = 35;
pub const SYS_KEYBOARD_POLL: u64 = 36;
pub const SYS_GET_FRAMEBUFFER: u64 = 37;
pub const SYS_GET_TICKS: u64 = 38;
pub const SYS_DEBUG_LOG: u64 = 39;
pub const SYS_REGISTER_IRQ_HANDLER: u64 = 40;
pub const SYS_MAP_FRAMEBUFFER: u64 = 41;
pub const SYS_UNREGISTER_IRQ_HANDLER: u64 = 42;
pub const SYS_IPC_WAIT_ANY: u64 = 43;  // Wait on multiple ports for any event
pub const SYS_GET_IRQ_COUNT: u64 = 44; // Get IRQ occurrence count for a registered handler
pub const SYS_SPAWN_PROCESS: u64 = 45; // Spawn a new process from a registered driver
pub const SYS_GET_MEMORY_INFO: u64 = 46; // Get system memory information
pub const SYS_LIST_PROCESSES: u64 = 47; // List all processes/threads
pub const SYS_GET_PROCESS_COUNT: u64 = 48; // Get total number of processes
pub const SYS_READ_KLOG: u64 = 49; // Read kernel log buffer
pub const SYS_MOUSE_GET_ID: u64 = 50; // Get detected PS/2 mouseID (0, 3, or 4)
pub const SYS_IPC_CREATE_PORT_WITH_ID: u64 = 51; // Create IPC port with specific reserved ID
pub const SYS_GET_CPU_BRAND: u64 = 52;

// ---------------------------------------------------------------------------
// Infrastructure / Networking Phase 1 syscalls
// ---------------------------------------------------------------------------
pub const SYS_PCI_GET_BAR:            u64 = 81;
pub const SYS_DEVICE_BIND_IRQ:        u64 = 82;
pub const SYS_IRQ_LISTEN:             u64 = 83;
pub const SYS_DMA_ALLOC:              u64 = 84;
pub const SYS_DMA_MAP:                u64 = 85;
pub const SYS_DMA_FREE:               u64 = 86;
pub const SYS_MAP_MMIO:               u64 = 87;
pub const SYS_IRQ_ACK:                u64 = 88;

// ---------------------------------------------------------------------------
// Video mode management syscalls (BGA/VBE_DISPI)
// ---------------------------------------------------------------------------

/// Set video mode: (width: u16, height: u16, bpp: u8) -> 0 | errno
///
/// width and height are packed into arg0: arg0 = (height << 16) | width
/// bpp is in arg1. Returns ESUCCESS on success.
/// Returns ENOTSUP if no backend supports mode changes.
/// Returns EINVAL if the mode parameters are invalid.
pub const SYS_SET_VIDEO_MODE: u64 = 77;

/// Get available video modes into a userspace buffer.
/// arg0 = pointer to array of VideoModeEntry structs (20 bytes each)
/// arg1 = max number of entries the buffer can hold
/// Returns number of modes written, or errno.
///
/// VideoModeEntry layout (20 bytes, all u32):
///   [0] width
///   [1] height
///   [2] bpp
///   [3] refresh_rate
///   [4] flags (0 = LFB supported)
pub const SYS_GET_VIDEO_MODES: u64 = 78;

/// Get current video mode info into a userspace buffer.
/// arg0 = pointer to KernelVideoInfo struct (see bga.rs)
/// Layout (32 bytes):
///   [0..8]  lfb_phys (u64)
///   [8..12] width (u32)
///   [12..16] height (u32)
///   [16..20] pitch (u32)
///   [20..24] bytes_per_pixel (u32)
///   [24..28] bga_available (u32)
///   [28..32] mode_active (u32)
/// Returns ESUCCESS on success.
pub const SYS_GET_CURRENT_VIDEO_MODE: u64 = 79;

/// Get total count of available video modes.
/// Returns the count (positive), or 0 if unavailable.
pub const SYS_VIDEO_MODE_COUNT: u64 = 80;

// ---------------------------------------------------------------------------
// Virtual memory management syscalls
// ---------------------------------------------------------------------------

/// mmap(addr_hint, length, prot, flags, 0, 0) -> mapped_addr | errno
pub const SYS_MMAP: u64 = 100;
/// munmap(addr, length) -> 0 | errno
pub const SYS_MUNMAP: u64 = 101;
/// mprotect(addr, length, prot) -> 0 | errno
pub const SYS_MPROTECT: u64 = 102;
/// brk(new_brk) -> current_brk | errno
pub const SYS_BRK: u64 = 103;
/// fork() -> child_pid in parent, 0 in child, errno on failure
pub const SYS_FORK: u64 = 104;

// ---------------------------------------------------------------------------
// Kernel FS backend syscalls — used exclusively by fsd to access the
// kernel's FAT32 driver.  These are *not* general-purpose filesystem
// syscalls; applications use SYS_FS_OPEN etc. which route through fsd.
// ---------------------------------------------------------------------------

/// Read file contents: (path_ptr, path_len, buf_ptr, buf_len) -> bytes_read
pub const SYS_KERN_FS_READ_FILE: u64 = 200;
/// List directory:     (path_ptr, path_len, buf_ptr, buf_len) -> bytes_used
pub const SYS_KERN_FS_LIST_DIR: u64 = 201;
/// Stat a path:        (path_ptr, path_len, stat_ptr) -> 0 | errno
pub const SYS_KERN_FS_STAT_PATH: u64 = 202;
/// Write full file contents: (path_ptr, path_len, buf_ptr, buf_len) -> 0 | errno
pub const SYS_KERN_FS_WRITE_FILE: u64 = 204;
/// mkdir(path_ptr, path_len) -> 0 | errno
pub const SYS_KERN_FS_MKDIR: u64 = 205;
/// rmdir(path_ptr, path_len) -> 0 | errno
pub const SYS_KERN_FS_RMDIR: u64 = 206;
/// unlink(path_ptr, path_len) -> 0 | errno
pub const SYS_KERN_FS_UNLINK: u64 = 207;
/// rename(old_ptr, old_len, new_ptr, new_len) -> 0 | errno
pub const SYS_KERN_FS_RENAME: u64 = 208;
/// sync filesystem data+metadata to disk cache barrier -> 0 | errno
pub const SYS_KERN_FS_SYNC: u64 = 209;
/// raw block read: (lba, sector_count, buf_ptr, buf_len) -> bytes_read | errno
pub const SYS_KERN_BLOCK_READ: u64 = 210;
/// raw block write: (lba, sector_count, buf_ptr, buf_len) -> 0 | errno
pub const SYS_KERN_BLOCK_WRITE: u64 = 211;
/// raw block flush cache: () -> 0 | errno
pub const SYS_KERN_BLOCK_FLUSH: u64 = 212;

// ---------------------------------------------------------------------------
// App launcher syscall — spawn an ATXF executable by filesystem path.
// Intended for use by the privileged app_launcher service only; the
// file manager and other unprivileged processes must go through the
// app_launcher IPC service rather than calling this syscall directly.
// ---------------------------------------------------------------------------

/// Spawn a process from an ATXF file on the filesystem.
/// (path_ptr, path_len) -> new_pid | errno
///
/// The path must end with ".atxf" and the file must contain a valid ATXF
/// image.  The process is spawned with the same default capability set as
/// processes created via SYS_SPAWN_PROCESS.
pub const SYS_SPAWN_FROM_PATH: u64 = 203;

/// open(path_ptr, path_len, flags, mode) -> fd (u64 handle)
pub const SYS_FS_OPEN: u64 = 53;
/// close(fd)
pub const SYS_FS_CLOSE: u64 = 54;
/// read(fd, buf_ptr, len) -> bytes_read
pub const SYS_FS_READ: u64 = 55;
/// write(fd, buf_ptr, len) -> bytes_written
pub const SYS_FS_WRITE: u64 = 56;
/// lseek(fd, offset, whence) -> new_offset
pub const SYS_FS_SEEK: u64 = 57;
/// stat(path_ptr, path_len, stat_ptr) -> 0 | errno
pub const SYS_FS_STAT: u64 = 58;
/// fstat(fd, stat_ptr) -> 0 | errno
pub const SYS_FS_FSTAT: u64 = 59;
/// mkdir(path_ptr, path_len, mode) -> 0 | errno
pub const SYS_FS_MKDIR: u64 = 60;
/// rmdir(path_ptr, path_len) -> 0 | errno
pub const SYS_FS_RMDIR: u64 = 61;
/// unlink(path_ptr, path_len) -> 0 | errno
pub const SYS_FS_UNLINK: u64 = 62;
/// rename(old_ptr, old_len, new_ptr, new_len) -> 0 | errno
pub const SYS_FS_RENAME: u64 = 63;
/// readdir(fd, dent_ptr, buf_len) -> entries_read | errno
pub const SYS_FS_READDIR: u64 = 64;
/// truncate(fd, new_size) -> 0 | errno
pub const SYS_FS_TRUNCATE: u64 = 65;
/// fsync(fd) -> 0 | errno
pub const SYS_FS_FSYNC: u64 = 66;
/// mount(dev_path_ptr, dev_path_len, mnt_ptr, mnt_len, fstype_ptr, fstype_len, flags) -> 0 | errno
pub const SYS_FS_MOUNT: u64 = 67;
/// umount(path_ptr, path_len) -> 0 | errno
pub const SYS_FS_UMOUNT: u64 = 68;
/// chmod(path_ptr, path_len, mode) -> 0 | errno
pub const SYS_FS_CHMOD: u64 = 69;
/// dup(fd) -> new_fd | errno
pub const SYS_FS_DUP: u64 = 70;
/// dup2(oldfd, newfd) -> newfd | errno
pub const SYS_FS_DUP2: u64 = 71;
/// link(old_ptr, old_len, new_ptr, new_len) -> 0 | errno (hard link)
pub const SYS_FS_LINK: u64 = 72;
/// symlink(target_ptr, target_len, link_ptr, link_len) -> 0 | errno
pub const SYS_FS_SYMLINK: u64 = 73;
/// readlink(path_ptr, path_len, buf_ptr, buf_len) -> bytes_read | errno
pub const SYS_FS_READLINK: u64 = 74;
/// utimes(path_ptr, path_len, atime_ns, mtime_ns) -> 0 | errno
pub const SYS_FS_UTIMES: u64 = 75;
/// statvfs(path_ptr, path_len, stat_ptr) -> 0 | errno
pub const SYS_FS_STATVFS: u64 = 76;

// Error codes — re-exported from the shared ABI crate (single source of truth).
pub use atom_abi::{
    ESUCCESS, EINVAL, ENOSYS, ENOMEM, EPERM, EBUSY,
    EMSGSIZE, ETIMEDOUT, EWOULDBLOCK, EDEADLK, ENOTFOUND,
    // Filesystem error codes
    ENOENT, EISDIR, ENOTDIR, EBADF, ENAMETOOLONG, EIO,
    EMFILE, EEXIST, EACCES, EXDEV, ENOTEMPTY,
    ENOTSUP,
    ENODEV,
    // FS limits
    FS_MAX_PATH_LEN,
};

// ============================================================================
// Syscall Error Taxonomy (Phase 6 — Syscall Hardening)
// ============================================================================
//
// Two error enums cover all operational failures in the syscall path:
//
// SyscallError — operational errors from handlers; each maps to a POSIX errno.
//   Never causes a kernel panic. Returned to userspace as an errno value.
//
// SyscallContextError — metadata drift errors detected before dispatch.
//   When these occur, the process is isolated via contain_context_failure()
//   and EINVAL is returned to userspace.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelOperationalError {
    ContextCorruption,
    OwnershipCorruption,
    AddressSpaceCorruption,
    InternalInvariantViolation,
}

impl KernelOperationalError {
    fn reason_code(self) -> u64 {
        match self {
            KernelOperationalError::ContextCorruption => 0x101,
            KernelOperationalError::OwnershipCorruption => 0x102,
            KernelOperationalError::AddressSpaceCorruption => 0x103,
            KernelOperationalError::InternalInvariantViolation => 0x104,
        }
    }
}

fn best_effort_current_process() -> Option<crate::process::ProcessId> {
    crate::sched::current_thread().and_then(crate::thread::get_thread_process_id)
}

fn contain_operational_violation(
    origin: &'static str,
    operational_error: KernelOperationalError,
    pid: Option<crate::process::ProcessId>,
    tid: Option<crate::thread::ThreadId>,
) {
    fn schedule_away_if_current_exited() {
        let Some(current) = crate::sched::current_thread() else {
            return;
        };
        if !matches!(
            crate::thread::get_thread_state(current),
            Some(crate::thread::ThreadState::Exited)
        ) {
            return;
        }

        let (prev, next) = crate::sched::on_timer_tick();
        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                crate::sched::perform_context_switch(prev_id, next_id);
            }
        }
    }

    log_error!(
        "syscall",
        "[OPERATIONAL_CONTAINMENT] origin={} error={:?} pid={:?} tid={:?}",
        origin,
        operational_error,
        pid,
        tid
    );

    if let Some(process_id) = pid {
        match crate::process::terminate_process(
            process_id,
            crate::process::TerminationReason::KernelInvariantViolation,
        ) {
            Ok(()) => {
                schedule_away_if_current_exited();
                return;
            }
            Err(err) => {
                log_error!(
                    "syscall",
                    "[OPERATIONAL_CONTAINMENT] origin={} pid={} terminate_process failed: {:?}",
                    origin,
                    process_id,
                    err
                );
            }
        }
    }

    if let Some(thread_id) = tid.or_else(crate::sched::current_thread) {
        crate::thread::terminate_entity(
            thread_id,
            crate::thread::TerminationReason::KilledByKernel {
                reason_code: operational_error.reason_code(),
            },
        );
        schedule_away_if_current_exited();
    }
}

/// Operational errors from the syscall path.
/// Each variant maps to a POSIX-like errno via `to_errno()`.
/// These errors are NEVER fatal to the kernel — they are contained locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallError {
    /// Address space of the process diverged from the expected PML4.
    AddressSpaceDrift,
    /// Process metadata is corrupted or internally inconsistent.
    ProcessMetadataCorrupted,
    /// Thread references a different process than expected.
    ThreadProcessMismatch,
    /// Userspace return address is invalid or points into kernel space.
    InvalidUserReturnAddress,
    /// Operation denied by admission policy (quota, OOM pressure).
    AdmissionDenied,
    /// Capability revocation was partial — some children failed to revoke.
    CapabilityRevokePartial,
    /// Userspace pointer is invalid or outside permitted bounds.
    InvalidPointer,
    /// Insufficient capability permissions for the requested operation.
    PermissionDenied,
    /// Internal inconsistency detected — local containment applied.
    InternalInconsistency(&'static str),
}

impl SyscallError {
    /// Convert to a POSIX-like errno value for return to userspace.
    /// Returns a non-zero value for every variant (zero = success).
    pub fn to_errno(self) -> u64 {
        match self {
            SyscallError::AddressSpaceDrift          => EINVAL,
            SyscallError::ProcessMetadataCorrupted   => EINVAL,
            SyscallError::ThreadProcessMismatch      => EINVAL,
            SyscallError::InvalidUserReturnAddress   => EINVAL,
            SyscallError::AdmissionDenied            => ENOMEM,
            SyscallError::CapabilityRevokePartial    => EBUSY,
            SyscallError::InvalidPointer             => EINVAL,
            SyscallError::PermissionDenied           => EPERM,
            SyscallError::InternalInconsistency(_)   => EINVAL,
        }
    }
}

/// Context errors detected before syscall dispatch.
/// When these occur, the process is isolated and EINVAL is returned.
/// These indicate kernel metadata drift, not userspace bugs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyscallContextError {
    /// Thread's PML4 does not match the registered process PML4.
    ProcessContextMismatch,
    /// Userspace return address is invalid or points into kernel space.
    InvalidUserReturnAddress,
    /// Address space is absent or not mapped for the process.
    MissingAddressSpace,
    /// Thread/process metadata has diverged (thread_id, process_id inconsistent).
    ThreadMetadataDrift,
}

/// Validated syscall execution context.
/// Produced by `validate_syscall_context` when all checks pass.
#[derive(Debug, Clone, Copy)]
pub struct SyscallContext {
    pub thread_id: crate::thread::ThreadId,
    pub process_id: crate::process::ProcessId,
    pub pml4_phys: u64,
}

/// Validate the syscall execution context before dispatching.
///
/// Checks:
/// 1. Thread exists and has an associated process
/// 2. Process PML4 matches `expected_pml4`
/// 3. Process is not in a terminal state (terminating or terminated)
///
/// Returns `Ok(SyscallContext)` if all checks pass.
/// Returns `Err(SyscallContextError)` if any check fails.
pub fn validate_syscall_context(
    thread_id: crate::thread::ThreadId,
    expected_pml4: u64,
) -> Result<SyscallContext, SyscallContextError> {
    // Check 1: thread has an associated process
    let process_id = crate::thread::get_thread_process_id(thread_id)
        .ok_or(SyscallContextError::ThreadMetadataDrift)?;

    // Check 2: process PML4 matches expected
    let process_pml4 = crate::process::get_process_pml4(process_id)
        .ok_or(SyscallContextError::MissingAddressSpace)?;

    if process_pml4 != expected_pml4 {
        return Err(SyscallContextError::ProcessContextMismatch);
    }

    // Check 3: process is not in a terminal state
    if crate::process::is_process_terminating(process_id)
        || crate::process::is_process_terminated(process_id)
    {
        return Err(SyscallContextError::ThreadMetadataDrift);
    }

    Ok(SyscallContext {
        thread_id,
        process_id,
        pml4_phys: expected_pml4,
    })
}

/// Map a `SyscallContextError` to a numeric reason code for use in termination.
fn context_error_code(error: SyscallContextError) -> u64 {
    match error {
        SyscallContextError::ProcessContextMismatch   => 1,
        SyscallContextError::InvalidUserReturnAddress => 2,
        SyscallContextError::MissingAddressSpace      => 3,
        SyscallContextError::ThreadMetadataDrift      => 4,
    }
}

fn context_error_kind(error: SyscallContextError) -> KernelOperationalError {
    match error {
        SyscallContextError::ProcessContextMismatch | SyscallContextError::MissingAddressSpace => {
            KernelOperationalError::AddressSpaceCorruption
        }
        SyscallContextError::InvalidUserReturnAddress | SyscallContextError::ThreadMetadataDrift => {
            KernelOperationalError::ContextCorruption
        }
    }
}

/// Contain a syscall context failure by isolating the affected process/thread.
///
/// This function is called when `validate_syscall_context` returns an error,
/// or when a context error is detected during syscall dispatch. It:
/// 1. Emits a structured error log with all available context
/// 2. Transitions the process to a dying state (if process_id is known)
/// 3. Terminates the thread (if only thread_id is known)
/// 4. Returns normally — the kernel continues running for other processes
///
/// This implements local containment: a single process's metadata drift
/// cannot crash the entire kernel.
pub fn contain_context_failure(
    error: SyscallContextError,
    thread_id: crate::thread::ThreadId,
    process_id: Option<crate::process::ProcessId>,
) {
    log_error!(
        "syscall",
        "[CONTEXT_FAILURE] error={:?} tid={} pid={:?} — isolating process/thread",
        error,
        thread_id,
        process_id
    );

    contain_operational_violation(
        "syscall_context",
        context_error_kind(error),
        process_id,
        Some(thread_id),
    );
    // Kernel continues — containment is local to this process/thread.
}

extern "C" {
    fn syscall_entry();
}

pub fn init() {
    const LOG_ORIGIN: &str = "syscall";

    unsafe {
        let star_value =
            ((USER_CODE_SELECTOR as u64 & !3) << 48) |
            ((KERNEL_CODE_SELECTOR as u64) << 32);
        wrmsr(MSR_STAR, star_value);

        let entry_addr = syscall_entry as *const () as u64;
        wrmsr(MSR_LSTAR, entry_addr);

        let sfmask = (1 << 8) | (1 << 9) | (1 << 10);
        wrmsr(MSR_SFMASK, sfmask);

        let efer_msr = 0xC000_0080;
        let mut efer = rdmsr(efer_msr);
        efer |= 1;
        wrmsr(efer_msr, efer);
    }

    log_info!(
        LOG_ORIGIN,
        "Syscall subsystem initialized"
    );

    log_debug!(
        LOG_ORIGIN,
        "STAR configured: user_cs=0x{:02X}, kernel_cs=0x{:02X}",
        USER_CODE_SELECTOR & !3,
        KERNEL_CODE_SELECTOR
    );

    log_debug!(
        LOG_ORIGIN,
        "LSTAR entry point: {:#X}",
        syscall_entry as *const () as u64
    );
}

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") low,
        in("edx") high,
        options(nostack, preserves_flags)
    );
}

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") low,
        out("edx") high,
        options(nostack, preserves_flags)
    );
    ((high as u64) << 32) | (low as u64)
}

/// Normalize a raw errno value to a SyscallError variant for observability.
/// Returns None for non-error results or unclassified errors.
/// Does NOT change the errno — only provides a structured classification.
fn normalize_syscall_result(errno: u64) -> Option<SyscallError> {
    match errno {
        e if e == ENOMEM => Some(SyscallError::AdmissionDenied),
        e if e == EPERM  => Some(SyscallError::PermissionDenied),
        e if e == EBUSY  => Some(SyscallError::CapabilityRevokePartial),
        e if e == EINVAL => Some(SyscallError::InvalidPointer),
        _                => None,
    }
}

#[repr(C)]
struct SyscallSavedFrame {
    user_r9: u64,
    user_r8: u64,
    user_r10: u64,
    user_rdx: u64,
    user_rsi: u64,
    user_rdi: u64,
    user_r15: u64,
    user_r14: u64,
    user_r13: u64,
    user_r12: u64,
    user_rbp: u64,
    user_rbx: u64,
    user_rip: u64,
    user_cs: u64,
    user_rflags: u64,
    user_rsp: u64,
    user_ss: u64,
}

#[no_mangle]
extern "win64" fn rust_syscall_dispatcher(
    syscall_num: u64,
    frame_ptr: *const SyscallSavedFrame,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    if frame_ptr.is_null() {
        // INVARIANT: frame_ptr is never null — structural, not operational.
        // A null frame_ptr indicates a bug in the assembly syscall bridge, not a userspace error.
        log_panic!(LOG_ORIGIN, "SYSCALL bridge bug: null frame_ptr");
    }

    let frame = unsafe { &*frame_ptr };
    let arg0 = frame.user_rdi;
    let arg1 = frame.user_rsi;
    let arg2 = frame.user_rdx;
    let arg3 = frame.user_r10;
    let arg4 = frame.user_r8;
    let arg5 = frame.user_r9;

    #[cfg(feature = "syscall-frame-debug")]
    let frame_before = (
        frame.user_rip,
        frame.user_rsp,
        frame.user_rbx,
        frame.user_rbp,
        frame.user_r12,
        frame.user_r13,
        frame.user_r14,
        frame.user_r15,
    );

    // ----------------------------------------------------------------
    // Syscall entry instrumentation.
    //
    // user_rip == the user RIP at the time of SYSCALL (saved by
    //              handler.asm as `push rcx` in the IRET frame, then
    //              read from the saved syscall frame). IRETQ will
    //              restore execution there on return.
    // ----------------------------------------------------------------
    let entry_cr3: u64 = unsafe {
        let cr3: u64;
        core::arch::asm!("mov {0}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        cr3
    };

    // Guard: a zero return RIP means IRETQ will resume Doom at 0x0 —
    // the classic null-deref vector.  Log loudly so it can be diagnosed.
    if frame.user_rip == 0 {
        log_warn!(
            "syscall",
            "SYSCALL-ENTRY ANOMALY: user_rip=0x0 \
             (tid={:?} syscall={} user_rsp={:#X} cr3={:#X}) \
             — null user RIP! Thread stack may be corrupted.",
            crate::sched::current_thread(),
            syscall_num,
            frame.user_rsp,
            entry_cr3,
        );
    } else if !atom_abi::is_canonical(frame.user_rip) {
        log_warn!(
            LOG_ORIGIN,
            "SYSCALL-ENTRY ANOMALY: non-canonical user_rip={:#X} \
             (tid={:?} syscall={})",
            frame.user_rip,
            crate::sched::current_thread(),
            syscall_num,
        );
    }

    log_debug!(
        LOG_ORIGIN,
        "[ABI] version={} syscall={} tid={:?}",
        atom_abi::ABI_VERSION,
        syscall_num,
        crate::sched::current_thread()
    );

    log_debug!(
        LOG_ORIGIN,
        "Syscall entry (TID={:?}): num={} user_rip={:#X} user_rsp={:#X} cr3={:#X} \
         args=({:#X}, {:#X}, {:#X}, {:#X}, {:#X}, {:#X})",
        crate::sched::current_thread(),
        syscall_num,
        frame.user_rip,
        frame.user_rsp,
        entry_cr3,
        arg0, arg1, arg2, arg3, arg4, arg5
    );

    if let Some(early_return) = policy::authorize_syscall_class(syscall_num, LOG_ORIGIN) {
        return early_return;
    }

    let result = match syscall_num {
        SYS_THREAD_YIELD => sys_thread_yield(),
        SYS_THREAD_EXIT => sys_thread_exit(arg0),
        SYS_THREAD_SLEEP => sys_thread_sleep(arg0),
        SYS_THREAD_CREATE => sys_thread_create(arg0, arg1, arg2),
        SYS_IPC_CREATE_PORT => sys_ipc_create_port(),
        SYS_IPC_CLOSE_PORT => sys_ipc_close_port(arg0),
        SYS_IPC_SEND => sys_ipc_send(arg0, arg1, arg2, arg3),
        SYS_IPC_RECV => sys_ipc_recv(arg0, arg1, arg2, arg3),
        SYS_CAP_CREATE => sys_cap_create(arg0, arg1, arg2),
        SYS_CAP_CHECK => sys_cap_check(arg0, arg1),
        SYS_CAP_REVOKE => sys_cap_revoke(arg0),
        SYS_CAP_DERIVE => sys_cap_derive(arg0, arg1, arg2),
        SYS_CAP_LIST => sys_cap_list(arg0, arg1),
        SYS_CAP_TRANSFER => sys_cap_transfer(arg0, arg1),
        SYS_IPC_SEND_WITH_CAP => sys_ipc_send_with_cap(arg0, arg1, arg2, arg3, arg4),
        SYS_CAP_QUERY_PARENT => sys_cap_query_parent(arg0),
        SYS_CAP_QUERY_CHILDREN => sys_cap_query_children(arg0, arg1, arg2),
        SYS_SHARED_REGION_CREATE => sys_shared_region_create(arg0),
        SYS_SHARED_REGION_MAP => sys_shared_region_map(arg0, arg1, arg2),
        SYS_SHARED_REGION_UNMAP => sys_shared_region_unmap(arg0),
        SYS_SHARED_REGION_DESTROY => sys_shared_region_destroy(arg0),
        SYS_IPC_SEND_BATCH => sys_ipc_send_batch(arg0, arg1, arg2),
        SYS_IPC_RECV_BATCH => sys_ipc_recv_batch(arg0, arg1, arg2),
        SYS_IPC_SEND_ASYNC => sys_ipc_send_async(arg0, arg1, arg2, arg3),
        SYS_IPC_TRY_RECV => sys_ipc_try_recv(arg0, arg1, arg2),
        SYS_IPC_TRACE_READ => sys_ipc_trace_read(arg0, arg1),
        SYS_IPC_PORT_STATS => sys_ipc_port_stats(arg0, arg1),
        SYS_ADDRSPACE_CREATE => sys_addrspace_create(),
        SYS_ADDRSPACE_DESTROY => sys_addrspace_destroy(arg0),
        SYS_MAP_REGION => sys_map_region(arg0, arg1, arg2, arg3, arg4),
        SYS_UNMAP_REGION => sys_unmap_region(arg0, arg1, arg2),
        SYS_REMAP_REGION => sys_remap_region(arg0, arg1, arg2, arg3),
        SYS_REGISTER_FAULT_HANDLER => sys_register_fault_handler(arg0),
        SYS_MOUSE_POLL => sys_mouse_poll(arg0),
        SYS_IO_PORT_READ => sys_io_port_read(arg0 as u16, arg1 as u8),
        SYS_IO_PORT_WRITE => sys_io_port_write(arg0 as u16, arg1 as u8),
        SYS_KEYBOARD_POLL => sys_keyboard_poll(arg0),
        SYS_GET_FRAMEBUFFER => sys_get_framebuffer(arg0),
        SYS_GET_TICKS => sys_get_ticks(),
        SYS_DEBUG_LOG => sys_debug_log(arg0, arg1 as usize),
        SYS_REGISTER_IRQ_HANDLER => sys_register_irq_handler(arg0 as u8, arg1),
        SYS_MAP_FRAMEBUFFER => sys_map_framebuffer_to_user(arg0),
        SYS_UNREGISTER_IRQ_HANDLER => sys_unregister_irq_handler(arg0 as u8),
        SYS_IPC_WAIT_ANY => sys_ipc_wait_any(arg0, arg1, arg2),
        SYS_GET_IRQ_COUNT => sys_get_irq_count(arg0 as u8),
        SYS_SPAWN_PROCESS => sys_spawn_process(arg0, arg1 as usize),
        SYS_GET_MEMORY_INFO => sys_get_memory_info(arg0),
        SYS_LIST_PROCESSES => sys_list_processes(arg0, arg1 as usize),
        SYS_GET_PROCESS_COUNT => sys_get_process_count(),
        SYS_READ_KLOG => sys_read_klog(arg0, arg1 as usize),
        SYS_MOUSE_GET_ID => sys_mouse_get_id(),
        SYS_IPC_CREATE_PORT_WITH_ID => sys_ipc_create_port_with_id(arg0),
        SYS_GET_CPU_BRAND => sys_get_cpu_brand(arg0, arg1 as usize),

        // Virtual memory management syscalls
        SYS_MMAP => sys_mmap(arg0, arg1, arg2, arg3),
        SYS_MUNMAP => sys_munmap(arg0, arg1),
        SYS_MPROTECT => sys_mprotect(arg0, arg1, arg2),
        SYS_BRK => sys_brk(arg0),
        SYS_FORK => sys_fork(frame),

        // Kernel FS backend syscalls (for fsd only)
        SYS_KERN_FS_READ_FILE  => sys_kern_fs_read_file(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_LIST_DIR   => sys_kern_fs_list_dir(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_STAT_PATH  => sys_kern_fs_stat_path(arg0, arg1 as usize, arg2),
        SYS_KERN_FS_WRITE_FILE => sys_kern_fs_write_file(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_MKDIR      => sys_kern_fs_mkdir(arg0, arg1 as usize),
        SYS_KERN_FS_RMDIR      => sys_kern_fs_rmdir(arg0, arg1 as usize),
        SYS_KERN_FS_UNLINK     => sys_kern_fs_unlink(arg0, arg1 as usize),
        SYS_KERN_FS_RENAME     => sys_kern_fs_rename(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_SYNC       => sys_kern_fs_sync(),
        SYS_KERN_BLOCK_READ    => sys_kern_block_read(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_BLOCK_WRITE   => sys_kern_block_write(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_BLOCK_FLUSH   => sys_kern_block_flush(),


        // Filesystem syscalls — forwarded to fsd via IPC
        SYS_FS_OPEN     => fsd_sys_fs_open(arg0, arg1 as usize, arg2 as u32, arg3 as u32),
        SYS_FS_CLOSE    => fsd_sys_fs_close(arg0),
        SYS_FS_READ     => fsd_sys_fs_read(arg0, arg1, arg2 as usize),
        SYS_FS_WRITE    => fsd_sys_fs_write(arg0, arg1, arg2 as usize),
        SYS_FS_SEEK     => fsd_sys_fs_seek(arg0, arg1 as i64, arg2 as u32),
        SYS_FS_STAT     => fsd_sys_fs_stat(arg0, arg1 as usize, arg2),
        SYS_FS_FSTAT    => fsd_sys_fs_fstat(arg0, arg1),
        SYS_FS_MKDIR    => fsd_sys_fs_mkdir(arg0, arg1 as usize, arg2 as u32),
        SYS_FS_RMDIR    => fsd_sys_fs_rmdir(arg0, arg1 as usize),
        SYS_FS_UNLINK   => fsd_sys_fs_unlink(arg0, arg1 as usize),
        SYS_FS_RENAME   => fsd_sys_fs_rename(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_FS_READDIR  => fsd_sys_fs_readdir(arg0, arg1, arg2 as usize),
        SYS_FS_TRUNCATE => fsd_sys_fs_truncate(arg0, arg1),
        SYS_FS_FSYNC    => fsd_sys_fs_fsync(arg0),
        SYS_FS_MOUNT    => sys_fs_mount(arg0, arg1 as usize, arg2, arg3 as usize, arg4, arg5 as usize),
        SYS_FS_UMOUNT   => sys_fs_umount(arg0, arg1 as usize),
        SYS_FS_CHMOD    => sys_fs_chmod(arg0, arg1 as usize, arg2 as u32),
        SYS_FS_DUP      => sys_fs_dup(arg0),
        SYS_FS_DUP2     => sys_fs_dup2(arg0, arg1),
        SYS_FS_LINK     => sys_fs_link(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_FS_SYMLINK  => sys_fs_symlink(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_FS_READLINK => sys_fs_readlink(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_FS_UTIMES   => sys_fs_utimes(arg0, arg1 as usize, arg2 as i64, arg3 as i64),
        SYS_FS_STATVFS  => sys_fs_statvfs(arg0, arg1 as usize, arg2),

        // App launcher — spawn ATXF by path
        SYS_SPAWN_FROM_PATH => sys_spawn_from_path(arg0, arg1 as usize),

        // Infrastructure / Networking Phase 1
        SYS_PCI_GET_BAR     => sys_pci_get_bar(arg0, arg1 as u8, arg2),
        SYS_DEVICE_BIND_IRQ => sys_device_bind_irq(arg0, arg1),
        SYS_IRQ_LISTEN      => sys_irq_listen(arg0, arg1),
        SYS_DMA_ALLOC       => sys_dma_alloc(arg0, arg1),
        SYS_DMA_MAP         => sys_dma_map(arg0, arg1),
        SYS_DMA_FREE        => sys_dma_free(arg0),
        SYS_MAP_MMIO        => sys_map_mmio(arg0, arg1),
        SYS_IRQ_ACK         => sys_irq_ack(arg0),

        // Video mode management (BGA/VBE_DISPI)
        SYS_SET_VIDEO_MODE         => sys_set_video_mode(arg0, arg1),
        SYS_GET_VIDEO_MODES        => sys_get_video_modes(arg0, arg1 as usize),
        SYS_GET_CURRENT_VIDEO_MODE => sys_get_current_video_mode(arg0),
        SYS_VIDEO_MODE_COUNT       => sys_video_mode_count(),

        _ => {
            log_warn!(
                "syscall",
                "Unknown syscall number: {}",
                syscall_num
            );
            ENOSYS
        }
    };

    // ----------------------------------------------------------------
    // Subsystem error normalization (Phase 6 — Syscall Hardening)
    //
    // Classify the result into a SyscallError variant for structured
    // observability. This does NOT change the errno returned to userspace —
    // it only adds a classification log when the result is an error.
    //
    // Mapping:
    //   ENOMEM → SyscallError::AdmissionDenied (MM quota/OOM)
    //   EPERM  → SyscallError::PermissionDenied (capability check)
    //   EBUSY  → SyscallError::CapabilityRevokePartial (partial revoke)
    //   EINVAL → SyscallError::InvalidPointer (invalid userspace pointer)
    //   other  → no classification (pass through)
    // ----------------------------------------------------------------
    if result != ESUCCESS && result != ENOSYS && result != EWOULDBLOCK
        && result != ETIMEDOUT && result != ENOTFOUND
    {
        let classified = normalize_syscall_result(result);
        if let Some(err) = classified {
            log_debug!(
                LOG_ORIGIN,
                "[SYSCALL_ERR] syscall={} tid={:?} errno={:#X} classified={:?}",
                syscall_num,
                crate::sched::current_thread(),
                result,
                err
            );
        }
    }

    #[cfg(feature = "syscall-frame-debug")]
    {
        let frame_after = unsafe { &*frame_ptr };
        let after = (
            frame_after.user_rip,
            frame_after.user_rsp,
            frame_after.user_rbx,
            frame_after.user_rbp,
            frame_after.user_r12,
            frame_after.user_r13,
            frame_after.user_r14,
            frame_after.user_r15,
        );

        if frame_before != after {
            log_warn!(
                LOG_ORIGIN,
                "SYSCALL-FRAME CORRUPTION: before={:?} after={:?} syscall={} tid={:?}",
                frame_before,
                after,
                syscall_num,
                crate::sched::current_thread(),
            );
        }
    }

    // ----------------------------------------------------------------
    // Syscall exit instrumentation.
    // Verify the IRET return RIP has not been clobbered during this call.
    // ----------------------------------------------------------------
    if frame.user_rip == 0 {
        log_warn!(
            LOG_ORIGIN,
            "SYSCALL-EXIT ANOMALY: return user_rip=0x0 \
             (tid={:?} syscall={} result={:#X}) \
             — IRETQ will resume at null address!",
            crate::sched::current_thread(),
            syscall_num,
            result,
        );
    }

    log_debug!(
        LOG_ORIGIN,
        "Syscall exit  (TID={:?}): num={} return_rip={:#X} return_rsp={:#X} result={:#X}",
        crate::sched::current_thread(),
        syscall_num,
        frame.user_rip,
        frame.user_rsp,
        result,
    );

    result
}

fn sys_mouse_poll(out_ptr: u64) -> u64 {
    let out_ptr = match validate_user_addr(out_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(out_ptr.as_u64(), 1).is_err() {
        return EINVAL;
    }

    // Return next raw mouse byte for userspace driver to process
    if let Some(byte) = crate::input::poll_mouse_byte() {
        unsafe {
            *out_ptr.as_mut_ptr::<u8>() = byte;
        }
        return ESUCCESS;
    }
    EWOULDBLOCK
}

/// Return the detected PS/2 mouseID (0, 3, or 4).
/// Userspace uses this to determine packet size and feature support.
fn sys_mouse_get_id() -> u64 {
    crate::input::get_mouse_id() as u64
}

/// Read a byte from an IO port (privileged operation for drivers)
fn sys_io_port_read(port: u16, _size: u8) -> u64 {
    if let Err(e) = policy::validate_io_port_access(port, crate::cap::CapPermissions::READ) {
        return e;
    }

    let value: u8 = unsafe {
        let mut val: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
        val
    };

    value as u64
}

/// Write a byte to an IO port (privileged operation for drivers)
fn sys_io_port_write(port: u16, value: u8) -> u64 {
    if let Err(e) = policy::validate_io_port_access(port, crate::cap::CapPermissions::WRITE) {
        return e;
    }

    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") value,
            options(nomem, nostack, preserves_flags)
        );
    }

    ESUCCESS
}

/// Poll keyboard buffer for input (raw scancode)
fn sys_keyboard_poll(out_ptr: u64) -> u64 {
    let out_ptr = match validate_user_addr(out_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(out_ptr.as_u64(), 1).is_err() {
        return EINVAL;
    }

    if let Some(scancode) = crate::input::poll_keyboard_byte() {
        unsafe {
            *out_ptr.as_mut_ptr::<u8>() = scancode;
        }
        return ESUCCESS;
    }
    EWOULDBLOCK
}

/// Get framebuffer information for userspace graphics
fn sys_get_framebuffer(info_ptr: u64) -> u64 {
    let info_ptr = match validate_user_addr(info_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(info_ptr.as_u64(), 5 * core::mem::size_of::<u64>()).is_err() {
        return EINVAL;
    }
    
    if let Some((width, height)) = crate::graphics::get_dimensions() {
        if let Some(addr) = crate::graphics::get_framebuffer_address() {
            let info_ptr = info_ptr.as_mut_ptr::<u64>();
            unsafe {
                // Write: [address, width, height, stride, bytes_per_pixel]
                *info_ptr = addr as u64;
                *info_ptr.add(1) = width as u64;
                *info_ptr.add(2) = height as u64;
                *info_ptr.add(3) = crate::graphics::get_stride() as u64;
                *info_ptr.add(4) = crate::graphics::get_bytes_per_pixel() as u64;
            }
            return ESUCCESS;
        }
    }
    EINVAL
}

/// Get current system ticks
fn sys_get_ticks() -> u64 {
    crate::interrupts::get_ticks()
}

/// Debug log from userspace
fn sys_debug_log(msg_ptr: u64, len: usize) -> u64 {
    if len == 0 {
        return ESUCCESS;
    }

    let msg_ptr = match validate_user_ptr::<u8>(msg_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if len > 256 {
        return EINVAL;
    }
    let msg_range = match validate_user_byte_range(msg_ptr, len) {
        Ok(range) => range,
        Err(e) => return e,
    };

    // SAFETY: msg_range is ABI-validated and guaranteed canonical.
    let msg = unsafe { core::slice::from_raw_parts(msg_range.base().as_ptr::<u8>(), msg_range.len()) };

    if let Ok(s) = core::str::from_utf8(msg) {
        log_info!("userspace", "{}", s);
    }

    ESUCCESS
}

fn sys_thread_yield() -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "thread_yield()"
    );

    let (prev, next) = crate::sched::on_timer_tick();
    if let (Some(prev_id), Some(next_id)) = (prev, next) {
        if prev_id != next_id {
            crate::sched::perform_context_switch(prev_id, next_id);
        }
    }
    ESUCCESS
}

fn sys_thread_exit(exit_code: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_info!(
        LOG_ORIGIN,
        "thread_exit(code=0x{:X})",
        exit_code
    );

    if let Some(tid) = crate::sched::current_thread() {
        // Terminate entity with comprehensive cleanup
        // This is the unified termination path for normal exits
        crate::thread::terminate_entity(
            tid,
            crate::thread::TerminationReason::NormalExit { exit_code }
        );

        // Schedule next thread - this should never return since our thread is gone
        let (prev, next) = crate::sched::on_timer_tick();

        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                crate::sched::perform_context_switch(prev_id, next_id);
            }
        }

        // Operational: no threads available to switch to after thread_exit.
        // Log the condition and return — the scheduler will handle the idle state.
        log_error!(
            LOG_ORIGIN,
            "[THREAD_EXIT] tid={} — no threads to switch to after exit, returning to idle",
            tid
        );
    }

    ESUCCESS
}

fn sys_thread_sleep(milliseconds: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "thread_sleep(ms={})",
        milliseconds
    );

    if milliseconds == 0 {
        return sys_thread_yield();
    }

    let tid = match crate::sched::current_thread() {
        Some(t) => t,
        None => return EINVAL,
    };

    // Calculate wake-up tick (assuming 100Hz timer = 10ms per tick)
    let ticks_to_sleep = milliseconds.div_ceil(10); // Round up
    let wake_tick = crate::interrupts::get_ticks() + ticks_to_sleep;

    // Register in the sleep queue.  This sets the thread to Blocked and
    // records the wake tick.  The timer interrupt handler calls
    // sched::wake_sleeping_threads() every tick and will move this thread
    // back to Ready once wake_tick is reached.
    crate::sched::sleep_thread(tid, wake_tick);

    // Yield the CPU so another thread can run.  When the timer wakes us
    // the scheduler will eventually context-switch back here.
    crate::sched::drive_cooperative_tick();

    ESUCCESS
}

fn sys_thread_create(entry_point: u64, stack_ptr: u64, flags: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "thread_create(entry={:#X}, stack={:#X}, flags={:#X})",
        entry_point,
        stack_ptr,
        flags
    );

    if entry_point == 0 || stack_ptr == 0 {
        log_warn!(
            LOG_ORIGIN,
            "thread_create rejected: invalid arguments (entry={:#X}, stack={:#X})",
            entry_point,
            stack_ptr
        );
        return EINVAL;
    }

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "thread_create rejected: no current thread"
            );
            return EINVAL;
        }
    };

    if let Err(e) = policy::validate_thread_create_capability(caller, LOG_ORIGIN) {
        return e;
    }

    const KERNEL_STACK_SIZE: usize = 64 * 1024;  // 64KB to handle deep call stacks with logging/IPC
    let kernel_stack_phys = match crate::mm::pmm::alloc_pages(KERNEL_STACK_SIZE / 4096) {
        Some(addr) => addr,
        None => {
            log_error!(
                LOG_ORIGIN,
                "thread_create failed: kernel stack allocation failed"
            );
            return ENOMEM;
        }
    };
    let kernel_stack_virt = crate::mm::vm::HIGHER_HALF_BASE + kernel_stack_phys;
    let kernel_stack_top = (kernel_stack_virt + KERNEL_STACK_SIZE) as u64;

    let caller_process_id = match crate::thread::get_thread_process_id(caller) {
        Some(process_id) => process_id,
        None => {
            log_error!(
                LOG_ORIGIN,
                "thread_create rejected: caller {} has no registered process_id",
                caller
            );
            return EINVAL;
        }
    };

    let caller_addr_space = match crate::process::get_process_pml4(caller_process_id) {
        Some(pml4_phys) => {
            let cached_pml4 = crate::thread::get_thread_address_space(caller).unwrap_or(0);
            debug_assert_eq!(
                cached_pml4,
                pml4_phys,
                "thread_create must seed child threads from the caller process primary PML4, not a divergent cache: thread {} process {} cache 0x{:X} process 0x{:X}",
                caller,
                caller_process_id,
                cached_pml4,
                pml4_phys
            );
            pml4_phys
        }
        None => {
            log_error!(
                LOG_ORIGIN,
                "thread_create rejected: caller {} process {} has no registered primary PML4",
                caller,
                caller_process_id
            );
            return EINVAL;
        }
    };

    let stack_ptr = match validate_user_addr(stack_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };

    if crate::mm::validate_page_alignment(stack_ptr.as_usize()).is_err() {
        log_warn!(
            LOG_ORIGIN,
            "thread_create rejected: stack top {:#X} is not page-aligned for guarded userspace stacks",
            stack_ptr.as_u64()
        );
        return EINVAL;
    }

    let user_stack = crate::thread::UserStackMetadata::default_for_top(stack_ptr.as_usize());
    let stack_span_len = (user_stack.top - user_stack.guard_base) as usize;
    if atom_abi::validate_user_range(user_stack.guard_base as usize, stack_span_len).is_err() {
        log_warn!(
            LOG_ORIGIN,
            "thread_create rejected: stack range guard=0x{:X} usable=0x{:X}-0x{:X} is outside userspace",
            user_stack.guard_base,
            user_stack.usable_base,
            user_stack.top
        );
        return EINVAL;
    }

    if crate::mm::vm::query_mapping_in_pml4(caller_addr_space as usize, user_stack.guard_base as usize).is_ok() {
        log_warn!(
            LOG_ORIGIN,
            "thread_create rejected: stack guard page 0x{:X} is already mapped",
            user_stack.guard_base
        );
        return EINVAL;
    }

    for page_idx in 0..user_stack.mapped_pages {
        let page_va = user_stack.usable_base as usize + page_idx * crate::mm::pmm::PAGE_SIZE;
        if crate::mm::vm::query_mapping_in_pml4(caller_addr_space as usize, page_va).is_err() {
            log_warn!(
                LOG_ORIGIN,
                "thread_create rejected: stack page 0x{:X} is not mapped in caller address space 0x{:X}",
                page_va,
                caller_addr_space
            );
            return EINVAL;
        }
    }

    // Create a userspace (Ring 3) context for the child thread.
    // sys_thread_create is only callable from userspace, so child threads
    // must also run in Ring 3 with the process primary address space.
    let context = crate::thread::CpuContext::new_user(
        entry_point,
        stack_ptr.as_u64(),
        caller_addr_space,
    );

    let tid = crate::thread::ThreadId::new();
    let cap_table = crate::cap::create_capability_table(tid);

    // Write stack canary
    unsafe {
        const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let bottom = kernel_stack_top - KERNEL_STACK_SIZE as u64;
        let canary_addr = bottom as *mut u64;
        core::ptr::write_volatile(canary_addr, STACK_CANARY);
    }

    let thread = crate::thread::Thread {
        id: tid,
        process_id: Some(caller_process_id),
        state: crate::thread::ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_SIZE,
        address_space: caller_addr_space,
        priority: crate::thread::ThreadPriority::Normal,
        name: "user_thread",
        capability_table: cap_table,
        is_userspace: true,
        user_stack: Some(user_stack),
    };

    crate::thread::add_thread(thread);
    crate::sched::mark_thread_ready(tid);

    log_info!(
        LOG_ORIGIN,
        "thread_create succeeded: new thread id={}",
        tid
    );

    tid.raw()
}

fn cleanup_failed_fork_child_address_space(child_pml4: usize) {
    crate::mm::vma::with_address_space_ops_lock(child_pml4, || {
        for account in crate::mm::vma::drain_materialized_pages(child_pml4) {
            let _ = crate::mm::vm::unmap_page_in_pml4(child_pml4, account.vaddr.as_usize());
            let _ = crate::mm::vma::free_page_account(account);
        }
        crate::mm::vma::destroy_vma_map(child_pml4);
        let _ = crate::mm::pmm::unregister_active_pml4(child_pml4);
        let _ = crate::mm::pmm::free_page(child_pml4);
    });
}

fn build_fork_child_context(frame: &SyscallSavedFrame, child_pml4: u64) -> crate::thread::CpuContext {
    let mut context = crate::thread::CpuContext::new_user(frame.user_rip, frame.user_rsp, child_pml4);
    context.rax = 0;
    context.rbx = frame.user_rbx;
    context.rcx = 0;
    context.rdx = frame.user_rdx;
    context.rsi = frame.user_rsi;
    context.rdi = frame.user_rdi;
    context.rbp = frame.user_rbp;
    context.rsp = frame.user_rsp;
    context.r8 = frame.user_r8;
    context.r9 = frame.user_r9;
    context.r10 = frame.user_r10;
    context.r11 = 0;
    context.r12 = frame.user_r12;
    context.r13 = frame.user_r13;
    context.r14 = frame.user_r14;
    context.r15 = frame.user_r15;
    context.rip = frame.user_rip;
    context.rflags = frame.user_rflags | 0x200;
    context.cs = frame.user_cs as u16;
    context.ss = frame.user_ss as u16;
    context.ds = crate::arch::gdt::USER_DATA_SELECTOR;
    context.es = crate::arch::gdt::USER_DATA_SELECTOR;
    context.fs = crate::arch::gdt::USER_DATA_SELECTOR;
    context.gs = crate::arch::gdt::USER_DATA_SELECTOR;
    context.cr3 = child_pml4;
    context
}

fn sys_fork(frame: &SyscallSavedFrame) -> u64 {
    const LOG_ORIGIN: &str = "syscall:fork";
    use crate::mm::{pmm, vm, vma};
    use crate::thread::{Thread, ThreadId, ThreadState};

    let parent_tid = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    if !crate::thread::is_userspace_thread(parent_tid) {
        log_warn!(LOG_ORIGIN, "fork rejected: caller {} is not userspace", parent_tid);
        return EINVAL;
    }

    let parent_pid = match crate::thread::get_thread_process_id(parent_tid) {
        Some(pid) => pid,
        None => return EINVAL,
    };
    let parent_pml4 = match crate::process::get_process_pml4(parent_pid) {
        Some(pml4) => pml4 as usize,
        None => return EINVAL,
    };

    let child_pml4 = match pmm::alloc_page_zeroed() {
        Some(p) => p,
        None => return ENOMEM,
    };

    if vm::clone_kernel_mappings(child_pml4).is_err() {
        let _ = pmm::free_page(child_pml4);
        return ENOMEM;
    }

    if pmm::register_active_pml4(child_pml4).is_err() {
        let _ = pmm::free_page(child_pml4);
        return ENOMEM;
    }

    vma::create_vma_map(child_pml4);

    let child_tid = ThreadId::new();
    let child_pid = crate::process::ProcessId::from(child_tid);

    let cow_stats = match vma::with_address_space_ops_lock(parent_pml4, || {
        vma::clone_vmas_for_fork(parent_pml4, child_pml4, parent_pid, child_pid)?;
        vma::clone_cow_private_mappings_for_fork(parent_pml4, child_pml4, parent_pid, child_pid)
    }) {
        Ok(stats) => stats,
        Err(err) => {
            log_warn!(
                LOG_ORIGIN,
                "fork failed during COW clone: parent_pid={} child_pid={} err={:?}",
                parent_pid,
                child_pid,
                err
            );
            cleanup_failed_fork_child_address_space(child_pml4);
            return if matches!(err, vma::VmaError::OutOfMemory) {
                ENOMEM
            } else {
                EINVAL
            };
        }
    };

    const KERNEL_STACK_SIZE: usize = 64 * 1024;
    let stack_pages = KERNEL_STACK_SIZE / pmm::PAGE_SIZE;
    let child_kernel_stack_phys = match pmm::alloc_pages(stack_pages) {
        Some(p) => p,
        None => {
            cleanup_failed_fork_child_address_space(child_pml4);
            return ENOMEM;
        }
    };
    let child_kernel_stack_top =
        (vm::HIGHER_HALF_BASE + child_kernel_stack_phys + KERNEL_STACK_SIZE) as u64;

    unsafe {
        const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let bottom = child_kernel_stack_top - KERNEL_STACK_SIZE as u64;
        core::ptr::write_volatile(bottom as *mut u64, STACK_CANARY);
    }

    let child_context = build_fork_child_context(frame, child_pml4 as u64);
    let parent_priority = crate::sched::get_thread_priority(parent_tid);
    let parent_stack_layout = crate::thread::get_thread_user_stack(parent_tid);

    let child_thread = Thread {
        id: child_tid,
        process_id: Some(child_pid),
        state: ThreadState::Ready,
        context: child_context,
        kernel_stack: child_kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_SIZE,
        address_space: child_pml4 as u64,
        priority: parent_priority,
        name: "fork_child",
        capability_table: crate::cap::create_capability_table(child_tid),
        is_userspace: true,
        user_stack: parent_stack_layout,
    };

    crate::thread::add_thread(child_thread);

    if let Some(parent_process) = crate::process::get_process(parent_pid) {
        let _ = crate::process::set_process_memory_limit(child_pid, parent_process.memory_limit_pages);

        for cap_handle in parent_process.capability_table.list() {
            let Some(mut cloned_cap) = parent_process.capability_table.get(cap_handle).cloned() else {
                continue;
            };
            cloned_cap.owner = child_pid;
            if crate::process::add_process_capability(child_pid, cloned_cap.clone()).is_ok() {
                let _ = crate::thread::mirror_process_capability_to_threads(child_pid, cloned_cap);
            }
        }
    }

    crate::sched::mark_thread_ready(child_tid);

    log_info!(
        LOG_ORIGIN,
        "fork success: parent_pid={} child_pid={} parent_tid={} child_tid={} parent_cr3=0x{:X} child_cr3=0x{:X} shared_pages={} cow_pages={} readonly_shared={}",
        parent_pid,
        child_pid,
        parent_tid,
        child_tid,
        parent_pml4,
        child_pml4,
        cow_stats.shared_pages,
        cow_stats.cow_pages,
        cow_stats.readonly_shared_pages
    );

    child_pid.raw()
}

fn sys_ipc_create_port() -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_create_port()"
    );

    let owner = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_create_port rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::create_port(owner);

    log_info!(
        LOG_ORIGIN,
        "ipc_create_port succeeded: port_id={}",
        port_id
    );

    let ipc_resource = crate::cap::ResourceType::IpcPort {
        port_id: port_id.raw(),
    };

    let permissions =
        crate::cap::CapPermissions::READ.union(crate::cap::CapPermissions::WRITE);

    match crate::cap::create_root_capability(ipc_resource, owner, permissions) {
        Ok(cap) => {
            match crate::thread::add_thread_capability(owner, cap) {
                Ok(cap_handle) => {
                    log_debug!(
                        LOG_ORIGIN,
                        "ipc_create_port: auto-granted IPC capability handle={}",
                        cap_handle
                    );
                }
                Err(_) => {
                    log_warn!(
                        LOG_ORIGIN,
                        "ipc_create_port: failed to attach capability to thread {}",
                        owner
                    );
                }
            }
        }
        Err(_) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_create_port: failed to create root IPC capability"
            );
        }
    }

    port_id.raw()
}

/// Create an IPC port with a specific reserved ID (1-255).
/// Used by well-known system services to get deterministic port IDs.
fn sys_ipc_create_port_with_id(requested_id: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_create_port_with_id(id={})",
        requested_id
    );

    let owner = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_create_port_with_id rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = match crate::ipc::create_port_with_id(owner, requested_id) {
        Ok(id) => id,
        Err(e) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_create_port_with_id failed for id={}: {}",
                requested_id, e
            );
            return EINVAL;
        }
    };

    log_info!(
        LOG_ORIGIN,
        "ipc_create_port_with_id succeeded: port_id={}",
        port_id
    );

    // Auto-grant IPC capability for this port
    let ipc_resource = crate::cap::ResourceType::IpcPort {
        port_id: port_id.raw(),
    };

    let permissions =
        crate::cap::CapPermissions::READ.union(crate::cap::CapPermissions::WRITE);

    match crate::cap::create_root_capability(ipc_resource, owner, permissions) {
        Ok(cap) => {
            if crate::thread::add_thread_capability(owner, cap).is_err() {
                log_warn!(
                    LOG_ORIGIN,
                    "ipc_create_port_with_id: failed to attach capability to thread {}",
                    owner
                );
            }
        }
        Err(_) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_create_port_with_id: failed to create root IPC capability"
            );
        }
    }

    port_id.raw()
}

fn sys_ipc_close_port(port_id_raw: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_close_port(port_id={})",
        port_id_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_close_port rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    match crate::ipc::close_port(port_id, caller) {
        Ok(_) => {
            log_info!(
                LOG_ORIGIN,
                "ipc_close_port succeeded: port_id={}, caller={}",
                port_id,
                caller
            );
            ESUCCESS
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_close_port failed: invalid port_id={}",
                port_id
            );
            EINVAL
        }

        Err(crate::ipc::IpcError::PermissionDenied) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_close_port denied: caller={} lacks permission for port_id={}",
                caller,
                port_id
            );
            EPERM
        }

        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_close_port failed: unexpected error {:?} (port_id={}, caller={})",
                e,
                port_id,
                caller
            );
            EINVAL
        }
    }
}

fn sys_ipc_send(
    port_id_raw: u64,
    msg_type: u64,
    payload_len: u64,
    timeout_ms: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_send(port={}, type={}, len={}, timeout_ms={})",
        port_id_raw,
        msg_type,
        payload_len,
        timeout_ms
    );

    if payload_len > crate::ipc::MAX_MESSAGE_SIZE as u64 {
        log_warn!(
            LOG_ORIGIN,
            "ipc_send rejected: payload too large (len={}, max={})",
            payload_len,
            crate::ipc::MAX_MESSAGE_SIZE
        );
        return EMSGSIZE;
    }

    let sender = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    log_debug!(
        LOG_ORIGIN,
        "ipc_send capability validated (caller={}, port_id={})",
        sender,
        port_id
    );

    // TODO: sys_ipc_send ignores payload_len — always sends empty payload.
    // sys_ipc_send_async is the only send path that actually copies user data.
    let payload = alloc::vec::Vec::new();
    let message = crate::ipc::Message::new(sender, msg_type as u32, payload);

    match crate::ipc::send_message(port_id, message) {
        Ok(_) => {
            log_debug!(
                LOG_ORIGIN,
                "ipc_send delivered (caller={}, port_id={})",
                sender,
                port_id
            );
            ESUCCESS
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send failed: invalid port_id={}",
                port_id
            );
            EINVAL
        }

        Err(crate::ipc::IpcError::MessageTooLarge) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send failed: message too large after copy"
            );
            EMSGSIZE
        }

        Err(crate::ipc::IpcError::QueueFull) |
        Err(crate::ipc::IpcError::WouldBlock) => {
            if timeout_ms == 0 {
                log_debug!(
                    LOG_ORIGIN,
                    "ipc_send would block (caller={}, port_id={})",
                    sender,
                    port_id
                );
                EWOULDBLOCK
            } else {
                log_debug!(
                    LOG_ORIGIN,
                    "ipc_send timed out after {} ms (caller={}, port_id={})",
                    timeout_ms,
                    sender,
                    port_id
                );
                ETIMEDOUT
            }
        }

        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_send failed: unexpected error {:?} (caller={}, port_id={})",
                e,
                sender,
                port_id
            );
            EINVAL
        }
    }
}

fn sys_ipc_recv(
    port_id_raw: u64,
    buffer_ptr: u64,
    buffer_size: u64,
    timeout_ms: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_recv(port={}, size={}, timeout_ms={})",
        port_id_raw,
        buffer_size,
        timeout_ms
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_recv rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);
    let user_buffer_ptr = if buffer_ptr == 0 {
        None
    } else {
        Some(match validate_user_ptr::<u8>(buffer_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        })
    };

    log_debug!(
        LOG_ORIGIN,
        "ipc_recv capability validated (caller={}, port_id={})",
        caller,
        port_id
    );

    let priority = crate::sched::get_thread_priority(caller);
    let deadline = if timeout_ms == u64::MAX {
        None
    } else {
        let ticks = timeout_ms.div_ceil(10);
        Some(crate::interrupts::get_ticks() + ticks)
    };

    let copy_message = |msg: crate::ipc::Message| -> u64 {
        let bytes_to_copy =
            core::cmp::min(msg.payload.len(), buffer_size as usize);

        if bytes_to_copy > 0 {
            if let Some(user_buf_ptr) = user_buffer_ptr {
                let user_buf_range = match validate_user_byte_range(user_buf_ptr, bytes_to_copy) {
                    Ok(range) => range,
                    Err(e) => return e,
                };
                if let Err(e) = write_buffer_to_user(user_buf_range, &msg.payload[..bytes_to_copy]) {
                    return e;
                }
            }
        }

        log_debug!(
            LOG_ORIGIN,
            "ipc_recv delivered {} bytes (caller={}, port_id={})",
            bytes_to_copy,
            caller,
            port_id
        );

        bytes_to_copy as u64
    };

    match crate::ipc::try_receive_message(port_id, caller) {
        Ok(Some(msg)) => {
            copy_message(msg)
        }

        Ok(None) => {
            if timeout_ms == 0 {
                log_debug!(
                    LOG_ORIGIN,
                    "ipc_recv would block (caller={}, port_id={})",
                    caller,
                    port_id
                );
                return EWOULDBLOCK;
            }

            log_debug!(
                LOG_ORIGIN,
                "ipc_recv blocking (caller={}, port_id={}, timeout_ms={})",
                caller,
                port_id,
                timeout_ms
            );

            match crate::ipc::block_receive(port_id, caller, priority, deadline) {
                Ok(_) => {
                    crate::thread::set_thread_state(
                        caller,
                        crate::thread::ThreadState::Blocked
                    );
                    let (prev, next) = crate::sched::on_timer_tick();

                    if let (Some(prev_id), Some(next_id)) = (prev, next) {
                        if prev_id != next_id {
                            crate::sched::perform_context_switch(prev_id, next_id);
                        }
                    }

                    match crate::ipc::try_receive_message(port_id, caller) {
                        Ok(Some(msg)) => copy_message(msg),
                        Ok(None) => {
                            log_debug!(
                                LOG_ORIGIN,
                                "ipc_recv timed out (caller={}, port_id={})",
                                caller,
                                port_id
                            );
                            ETIMEDOUT
                        }
                        Err(crate::ipc::IpcError::InvalidPort) => EINVAL,
                        Err(e) => {
                            log_error!(
                                LOG_ORIGIN,
                                "ipc_recv failed after block: {:?} (caller={}, port_id={})",
                                e,
                                caller,
                                port_id
                            );
                            EINVAL
                        }
                    }
                }

                Err(crate::ipc::IpcError::PortBusy) => {
                    log_debug!(
                        LOG_ORIGIN,
                        "ipc_recv port busy (caller={}, port_id={})",
                        caller,
                        port_id
                    );
                    EBUSY
                }

                Err(crate::ipc::IpcError::DeadlockDetected) => {
                    log_warn!(
                        LOG_ORIGIN,
                        "ipc_recv deadlock detected (caller={}, port_id={})",
                        caller,
                        port_id
                    );
                    EDEADLK
                }

                Err(e) => {
                    log_error!(
                        LOG_ORIGIN,
                        "ipc_recv block failed: {:?} (caller={}, port_id={})",
                        e,
                        caller,
                        port_id
                    );
                    EINVAL
                }
            }
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_recv failed: invalid port_id={}",
                port_id
            );
            EINVAL
        }

        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_recv failed: unexpected error {:?} (caller={}, port_id={})",
                e,
                caller,
                port_id
            );
            EINVAL
        }
    }
}

fn sys_ipc_send_async(
    port_id_raw: u64,
    msg_type: u64,
    payload_ptr: u64,
    payload_len: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_send_async(port={}, type={}, len={})",
        port_id_raw,
        msg_type,
        payload_len
    );

    if payload_len > crate::ipc::MAX_MESSAGE_SIZE as u64 {
        log_warn!(
            LOG_ORIGIN,
            "ipc_send_async rejected: payload too large (len={}, max={})",
            payload_len,
            crate::ipc::MAX_MESSAGE_SIZE
        );
        return EMSGSIZE;
    }

    let sender = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send_async rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    log_debug!(
        LOG_ORIGIN,
        "ipc_send_async capability validated (caller={}, port_id={})",
        sender,
        port_id
    );

    let payload_range = if payload_len == 0 {
        None
    } else {
        let payload_ptr = match validate_user_ptr::<u8>(payload_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        };
        Some(match validate_user_byte_range(payload_ptr, payload_len as usize) {
            Ok(range) => range,
            Err(e) => return e,
        })
    };
    let payload = match payload_range {
        Some(range) => match copy_buffer_from_user(range, crate::ipc::MAX_MESSAGE_SIZE) {
            Ok(buf) => buf,
            Err(e) => return e,
        },
        None => alloc::vec::Vec::new(),
    };

    let message = crate::ipc::Message::new(sender, msg_type as u32, payload);

    match crate::ipc::send_message_async(port_id, message) {
        Ok(_) => {
            log_debug!(
                LOG_ORIGIN,
                "ipc_send_async queued (caller={}, port_id={})",
                sender,
                port_id
            );
            ESUCCESS
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send_async failed: invalid port_id={}",
                port_id
            );
            EINVAL
        }

        Err(crate::ipc::IpcError::MessageTooLarge) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_send_async failed: message too large after copy"
            );
            EMSGSIZE
        }

        Err(crate::ipc::IpcError::QueueFull) |
        Err(crate::ipc::IpcError::WouldBlock) => {
            log_debug!(
                LOG_ORIGIN,
                "ipc_send_async would block (caller={}, port_id={})",
                sender,
                port_id
            );
            EWOULDBLOCK
        }

        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_send_async failed: unexpected error {:?} (caller={}, port_id={})",
                e,
                sender,
                port_id
            );
            EINVAL
        }
    }
}

fn sys_ipc_try_recv(
    port_id_raw: u64,
    buffer_ptr: u64,
    buffer_size: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "ipc_try_recv(port={}, size={})",
        port_id_raw,
        buffer_size
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_try_recv rejected: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);
    let user_buffer_ptr = if buffer_ptr == 0 {
        None
    } else {
        Some(match validate_user_ptr::<u8>(buffer_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        })
    };

    match crate::ipc::try_receive_message(port_id, caller) {
        Ok(Some(msg)) => {
            let bytes_to_copy =
                core::cmp::min(msg.payload.len(), buffer_size as usize);

            if bytes_to_copy > 0 {
                if let Some(buffer_ptr) = user_buffer_ptr {
                    let buffer_range = match validate_user_byte_range(buffer_ptr, bytes_to_copy) {
                        Ok(range) => range,
                        Err(e) => return e,
                    };
                    if let Err(e) = write_buffer_to_user(buffer_range, &msg.payload[..bytes_to_copy]) {
                        return e;
                    }
                }
            }

            log_debug!(
                LOG_ORIGIN,
                "ipc_try_recv delivered {} bytes (caller={}, port_id={})",
                bytes_to_copy,
                caller,
                port_id
            );

            bytes_to_copy as u64
        }

        Ok(None) => {
            // No message available - return immediately (non-blocking)
            EWOULDBLOCK
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                LOG_ORIGIN,
                "ipc_try_recv failed: invalid port_id={}",
                port_id
            );
            EINVAL
        }

        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "ipc_try_recv failed: unexpected error {:?} (caller={}, port_id={})",
                e,
                caller,
                port_id
            );
            EINVAL
        }
    }
}

#[repr(C)]
struct RawIpcTraceEvent {
    timestamp_ms: u64,
    kind: u64,
    port_id: u64,
    sender: u64,
    receiver: u64,
    size: u64,
}

impl From<&crate::ipc::IpcTraceEvent> for RawIpcTraceEvent {
    fn from(event: &crate::ipc::IpcTraceEvent) -> Self {
        Self {
            timestamp_ms: event.timestamp_ms,
            kind: event.kind.as_u64(),
            port_id: event.port.raw(),
            sender: event.sender.raw(),
            receiver: event.receiver.map(|id| id.raw()).unwrap_or(0),
            size: event.size as u64,
        }
    }
}

fn sys_ipc_trace_read(buffer_ptr: u64, max_events: u64) -> u64 {
    log_info!(
        "syscall",
        "ipc_trace_read(buffer={:#x}, max={})",
        buffer_ptr,
        max_events
    );

    if max_events == 0 {
        return 0;
    }

    let user_buffer_ptr = if buffer_ptr == 0 {
        None
    } else {
        Some(match validate_user_ptr::<RawIpcTraceEvent>(buffer_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        })
    };
    let events = crate::ipc::read_trace(max_events as usize);
    let available = events.len();

    if let Some(buffer_ptr) = user_buffer_ptr {
        let to_copy = core::cmp::min(available, max_events as usize);
        let write_bytes = match to_copy.checked_mul(core::mem::size_of::<RawIpcTraceEvent>()) {
            Some(v) => v,
            None => return EINVAL,
        };
        let _buffer_range = match validate_user_byte_range(buffer_ptr.cast::<u8>(), write_bytes) {
            Ok(range) => range,
            Err(e) => return e,
        };
        unsafe {
            let buffer = buffer_ptr.as_mut_ptr();
            for (idx, event) in events.iter().take(to_copy).enumerate() {
                buffer.add(idx).write(RawIpcTraceEvent::from(event));
            }
        }
    }

    available as u64
}

#[repr(C)]
struct RawIpcPortStats {
    messages_sent: u64,
    messages_received: u64,
    bytes_sent: u64,
    bytes_received: u64,
    min_latency_ms: u64,
    max_latency_ms: u64,
    avg_latency_ms: u64,
    messages_per_second: u64,
}

impl From<crate::ipc::IpcPortStats> for RawIpcPortStats {
    fn from(stats: crate::ipc::IpcPortStats) -> Self {
        Self {
            messages_sent: stats.messages_sent,
            messages_received: stats.messages_received,
            bytes_sent: stats.bytes_sent,
            bytes_received: stats.bytes_received,
            min_latency_ms: stats.min_latency_ms,
            max_latency_ms: stats.max_latency_ms,
            avg_latency_ms: stats.avg_latency_ms,
            messages_per_second: stats.messages_per_second,
        }
    }
}

fn sys_ipc_port_stats(port_id_raw: u64, stats_ptr: u64) -> u64 {
    log_info!(
        "syscall",
        "ipc_port_stats(port={}, buffer={:#x})",
        port_id_raw,
        stats_ptr
    );

    let user_stats_ptr = if stats_ptr == 0 {
        None
    } else {
        Some(match validate_user_ptr::<RawIpcPortStats>(stats_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        })
    };
    let port_id = crate::ipc::PortId::from_raw(port_id_raw);
    match crate::ipc::get_port_stats(port_id) {
        Ok(stats) => {
            log_debug!(
                "syscall",
                "ipc_port_stats: sent={} recv={} avg={}ms",
                stats.messages_sent,
                stats.messages_received,
                stats.avg_latency_ms
            );

            if let Some(stats_ptr) = user_stats_ptr {
                let _stats_range = match validate_user_byte_range(
                    stats_ptr.cast::<u8>(),
                    core::mem::size_of::<RawIpcPortStats>(),
                ) {
                    Ok(range) => range,
                    Err(e) => return e,
                };
                unsafe {
                    stats_ptr.as_mut_ptr().write(stats.into());
                }
            }

            ESUCCESS
        }
        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!(
                "syscall",
                "ipc_port_stats: invalid port id={}",
                port_id_raw
            );
            EINVAL
        }
        Err(err) => {
            log_error!(
                "syscall",
                "ipc_port_stats: unexpected error: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn ipc_send_batch_core(
    port_id: crate::ipc::PortId,
    sender: crate::thread::ThreadId,
    _messages_ptr: atom_abi::UserPtr<u8>,
    count: usize,
) -> Result<usize, crate::ipc::IpcError> {
    let mut messages = alloc::vec::Vec::with_capacity(count);
    for i in 0..count {
        messages.push(crate::ipc::Message::new(sender, i as u32, alloc::vec![i as u8]));
    }
    crate::ipc::send_batch(port_id, messages)
}

fn ipc_recv_batch_core(
    port_id: crate::ipc::PortId,
    caller: crate::thread::ThreadId,
    _buffer_ptr: atom_abi::UserPtr<u8>,
    max_count: usize,
) -> Result<usize, crate::ipc::IpcError> {
    crate::ipc::receive_batch(port_id, caller, max_count).map(|messages| messages.len())
}

fn sys_ipc_send_batch(port_id_raw: u64, messages_ptr: u64, count: u64) -> u64 {
    // messages_ptr is not decoded yet by this syscall implementation, but the
    // boundary is ABI-validated and typed to prevent raw-pointer drift.
    log_info!(
        "syscall",
        "ipc_send_batch(port={}, messages={:#x}, count={})",
        port_id_raw,
        messages_ptr,
        count
    );

    if count == 0 {
        log_debug!("syscall", "ipc_send_batch: empty batch");
        return ESUCCESS;
    }
    if count > crate::ipc::MAX_BATCH_SIZE as u64 {
        log_warn!(
            "syscall",
            "ipc_send_batch: batch too large (count={}, max={})",
            count,
            crate::ipc::MAX_BATCH_SIZE
        );
        return EINVAL;
    }
    let batch_count = count as usize;

    let messages_ptr = match validate_user_ptr::<u8>(messages_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let _messages_range = match validate_user_byte_range(messages_ptr, 1) {
        Ok(range) => range,
        Err(e) => return e,
    };

    let sender = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "ipc_send_batch: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    match ipc_send_batch_core(port_id, sender, messages_ptr, batch_count) {
        Ok(sent_count) => {
            log_debug!(
                "syscall",
                "ipc_send_batch: sent {} messages",
                sent_count
            );
            sent_count as u64
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!("syscall", "ipc_send_batch: invalid port {}", port_id_raw);
            EINVAL
        }
        Err(crate::ipc::IpcError::BatchTooLarge) => {
            log_warn!("syscall", "ipc_send_batch: batch too large (post-check)");
            EINVAL
        }
        Err(crate::ipc::IpcError::QueueFull) => {
            log_debug!("syscall", "ipc_send_batch: queue full");
            EWOULDBLOCK
        }
        Err(err) => {
            log_error!(
                "syscall",
                "ipc_send_batch: unexpected error: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_ipc_recv_batch(port_id_raw: u64, buffer_ptr: u64, max_count: u64) -> u64 {
    // buffer_ptr is not decoded yet by this syscall implementation, but the
    // boundary is ABI-validated and typed to prevent raw-pointer drift.
    log_info!(
        "syscall",
        "ipc_recv_batch(port={}, buffer={:#x}, max={})",
        port_id_raw,
        buffer_ptr,
        max_count
    );

    if max_count == 0 {
        log_debug!("syscall", "ipc_recv_batch: max_count = 0");
        return 0;
    }
    if max_count > crate::ipc::MAX_BATCH_SIZE as u64 {
        log_warn!(
            "syscall",
            "ipc_recv_batch: batch size too large (max_count={}, limit={})",
            max_count,
            crate::ipc::MAX_BATCH_SIZE
        );
        return EINVAL;
    }
    let batch_max_count = max_count as usize;

    let buffer_ptr = match validate_user_ptr::<u8>(buffer_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let _buffer_range = match validate_user_byte_range(buffer_ptr, 1) {
        Ok(range) => range,
        Err(e) => return e,
    };

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "ipc_recv_batch: no current thread");
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    match ipc_recv_batch_core(port_id, caller, buffer_ptr, batch_max_count) {
        Ok(count) => {
            log_debug!(
                "syscall",
                "ipc_recv_batch: received {} messages",
                count
            );
            count as u64
        }

        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!("syscall", "ipc_recv_batch: invalid port {}", port_id_raw);
            EINVAL
        }
        Err(err) => {
            log_error!(
                "syscall",
                "ipc_recv_batch: unexpected error: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_ipc_send_with_cap(
    port_id_raw: u64,
    msg_type: u64,
    payload_len: u64,
    cap_handle_raw: u64,
    mode_or_perms: u64,
) -> u64 {
    log_info!(
        "syscall",
        "ipc_send_with_cap(port={}, type={}, cap={:#x}, mode={})",
        port_id_raw,
        msg_type,
        cap_handle_raw,
        mode_or_perms
    );

    if payload_len > crate::ipc::MAX_MESSAGE_SIZE as u64 {
        log_warn!(
            "syscall",
            "ipc_send_with_cap: message too large (len={}, max={})",
            payload_len,
            crate::ipc::MAX_MESSAGE_SIZE
        );
        return EMSGSIZE;
    }

    let sender = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "ipc_send_with_cap: no current thread");
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);
    let cap_handle = crate::cap::CapHandle::from_raw(cap_handle_raw);
    if let Err(e) = policy::validate_ipc_send_with_cap_permissions(
        sender,
        port_id,
        port_id_raw,
        cap_handle,
        cap_handle_raw,
    ) {
        return e;
    }

    let payload = alloc::vec::Vec::new();
    let is_move = (mode_or_perms >> 32) != 0;
    let message = if is_move {
        log_debug!(
            "syscall",
            "ipc_send_with_cap: delegating capability via MOVE"
        );
        crate::ipc::Message::new_with_move(
            sender,
            msg_type as u32,
            payload,
            cap_handle,
        )
    } else {
        let reduced_perms = crate::cap::CapPermissions::from_bits(mode_or_perms as u32);
        log_debug!(
            "syscall",
            "ipc_send_with_cap: delegating capability via GRANT (perms={:#x})",
            reduced_perms.bits()
        );
        crate::ipc::Message::new_with_grant(
            sender,
            msg_type as u32,
            payload,
            cap_handle,
            reduced_perms,
        )
    };

    match crate::ipc::send_message(port_id, message) {
        Ok(_) => {
            log_debug!("syscall", "ipc_send_with_cap: success");
            ESUCCESS
        }
        Err(crate::ipc::IpcError::InvalidPort) => {
            log_warn!("syscall", "ipc_send_with_cap: invalid port {}", port_id_raw);
            EINVAL
        }
        Err(crate::ipc::IpcError::MessageTooLarge) => {
            log_warn!("syscall", "ipc_send_with_cap: message too large (post-check)");
            EMSGSIZE
        }
        Err(err) => {
            log_error!(
                "syscall",
                "ipc_send_with_cap: unexpected error: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_cap_create(resource_type: u64, resource_id: u64, permissions: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_create(type={}, id={:#x}, perms={:#x})",
        resource_type,
        resource_id,
        permissions
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_create: no current thread");
            return EINVAL;
        }
    };

    let resource = match policy::resolve_cap_create_resource(resource_type, resource_id, caller) {
        Ok(resource) => resource,
        Err(e) => return e,
    };

    let perms = crate::cap::CapPermissions::from_bits(permissions as u32);

    match crate::cap::create_root_capability(resource, caller, perms) {
        Ok(cap) => {
            let handle = cap.handle;

            match crate::thread::add_thread_capability(caller, cap) {
                Ok(_) => {
                    log_debug!(
                        "syscall",
                        "cap_create: created capability handle={}",
                        handle
                    );
                    handle.raw()
                }
                Err(err) => {
                    log_error!(
                        "syscall",
                        "cap_create: failed to add capability to thread table: {:?}",
                        err
                    );
                    EINVAL
                }
            }
        }
        Err(err) => {
            log_error!(
                "syscall",
                "cap_create: failed to create capability: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_cap_check(handle_raw: u64, required_perms: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_check(handle={:#x}, perms={:#x})",
        handle_raw,
        required_perms
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_check: no current thread");
            return 0;
        }
    };

    let handle = crate::cap::CapHandle::from_raw(handle_raw);
    let perms = crate::cap::CapPermissions::from_bits(required_perms as u32);

    match crate::thread::validate_thread_capability(caller, handle, perms) {
        Ok(()) => {
            log_debug!("syscall", "cap_check: validated handle={:#x} perms={:#x}", handle_raw, required_perms);
            1
        }
        Err(e) => {
            log_warn!("syscall", "cap_check: denied handle={:#x} perms={:#x} reason={:?}", handle_raw, required_perms, e);
            0
        }
    }
}

fn sys_cap_revoke(handle_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_revoke(handle={:#x})",
        handle_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_revoke: no current thread");
            return EINVAL;
        }
    };

    let handle = crate::cap::CapHandle::from_raw(handle_raw);

    match crate::cap::revoke_capability(handle, caller) {
        Ok(report) => {
            let count = report.revoked.len();
            log_debug!(
                "syscall",
                "cap_revoke: status={:?} root={} revoked={} missing={} failed={} callbacks={} callback_failures={}",
                report.status,
                report.root,
                report.revoked.len(),
                report.missing.len(),
                report.failed.len(),
                report.callbacks_executed(),
                report.callback_failures()
            );
            if matches!(report.status, crate::cap::RevokeStatus::Failed) {
                return EINVAL;
            }
            count as u64
        }
        Err(err) => {
            log_warn!(
                "syscall",
                "cap_revoke: capability not found or not revocable: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_cap_derive(parent_handle_raw: u64, new_owner_raw: u64, reduced_perms: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_derive(parent={:#x}, owner={}, perms={:#x})",
        parent_handle_raw, new_owner_raw, reduced_perms
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    let parent_handle = crate::cap::CapHandle::from_raw(parent_handle_raw);
    let new_owner = crate::thread::ThreadId::from_raw(new_owner_raw);
    let perms = crate::cap::CapPermissions::from_bits(reduced_perms as u32);

    match crate::cap::derive_capability(parent_handle, caller, new_owner, perms) {
        Ok(child_handle) => {
            log_info!("syscall", "cap_derive: created child {}", child_handle);
            child_handle.raw()
        }
        Err(crate::cap::CapError::NotFound) => {
            log_info!("syscall", "cap_derive: parent capability not found");
            EINVAL
        }
        Err(crate::cap::CapError::NotOwner) => {
            log_info!("syscall", "cap_derive: caller is not the owner");
            EPERM
        }
        Err(crate::cap::CapError::PermissionDenied) => {
            log_info!("syscall", "cap_derive: insufficient permissions");
            EPERM
        }
        Err(_) => {
            log_info!("syscall", "cap_derive: unknown error");
            EINVAL
        }
    }
}

fn sys_cap_transfer(cap_handle_raw: u64, target_tid_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_transfer(handle={:#x}, target={})",
        cap_handle_raw,
        target_tid_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_transfer: no current thread");
            return EINVAL;
        }
    };

    let cap_handle = crate::cap::CapHandle::from_raw(cap_handle_raw);
    let target = crate::thread::ThreadId::from_raw(target_tid_raw);

    if crate::thread::find_thread(target).is_none() {
        log_warn!(
            "syscall",
            "cap_transfer: target thread not found (target={})",
            target_tid_raw
        );
        return EINVAL;
    }

    match crate::cap::transfer_capability(cap_handle, caller, target) {
        Ok(_) => {
            log_debug!(
                "syscall",
                "cap_transfer: transfer successful (handle={:#x}, target={})",
                cap_handle_raw,
                target_tid_raw
            );
            ESUCCESS
        }
        Err(crate::cap::CapError::NotFound) => {
            log_warn!(
                "syscall",
                "cap_transfer: capability not found (handle={:#x})",
                cap_handle_raw
            );
            EINVAL
        }
        Err(crate::cap::CapError::NotOwner) => {
            log_warn!(
                "syscall",
                "cap_transfer: caller is not the owner (handle={:#x})",
                cap_handle_raw
            );
            EPERM
        }
        Err(crate::cap::CapError::PermissionDenied) => {
            log_warn!(
                "syscall",
                "cap_transfer: insufficient permissions (missing GRANT)"
            );
            EPERM
        }
        Err(err) => {
            log_error!(
                "syscall",
                "cap_transfer: unexpected error: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_cap_list(buffer_ptr: u64, buffer_size: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_list(buffer={:#x}, size={})",
        buffer_ptr,
        buffer_size
    );

    let stats = crate::cap::get_capability_stats();

    log_debug!(
        "syscall",
        "cap_list: total={} (T:{} M:{} I:{} IRQ:{} D:{} DMA:{})",
        stats.total,
        stats.thread_caps,
        stats.memory_caps,
        stats.ipc_caps,
        stats.irq_caps,
        stats.device_caps,
        stats.dma_caps
    );

    stats.total as u64
}

fn sys_cap_query_parent(handle_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_query_parent(handle={:#x})",
        handle_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_query_parent: no current thread");
            return EINVAL;
        }
    };

    let handle = crate::cap::CapHandle::from_raw(handle_raw);

    if let Err(e) = policy::validate_cap_query_ownership(
        caller,
        handle,
        handle_raw,
        "cap_query_parent",
    ) {
        return e;
    }

    match crate::cap::query_parent(handle) {
        Ok(Some(parent_handle)) => {
            log_debug!(
                "syscall",
                "cap_query_parent: parent handle={}",
                parent_handle
            );
            parent_handle.raw()
        }
        Ok(None) => {
            log_debug!(
                "syscall",
                "cap_query_parent: root capability"
            );
            0
        }
        Err(err) => {
            log_warn!(
                "syscall",
                "cap_query_parent: capability not found or invalid: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_cap_query_children(handle_raw: u64, buffer_ptr: u64, buffer_size: u64) -> u64 {
    log_info!(
        "syscall",
        "cap_query_children(handle={:#x}, buffer={:#x}, size={})",
        handle_raw,
        buffer_ptr,
        buffer_size
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_query_children: no current thread");
            return EINVAL;
        }
    };

    let handle = crate::cap::CapHandle::from_raw(handle_raw);

    if let Err(e) = policy::validate_cap_query_ownership(
        caller,
        handle,
        handle_raw,
        "cap_query_children",
    ) {
        return e;
    }

    match crate::cap::query_children(handle) {
        Ok(children) => {
            let count = children.len();
            log_debug!(
                "syscall",
                "cap_query_children: found {} children",
                count
            );

            if buffer_ptr != 0 && buffer_size > 0 {
                let to_copy = core::cmp::min(count, buffer_size as usize);
                let write_bytes = match to_copy.checked_mul(core::mem::size_of::<u64>()) {
                    Some(bytes) => bytes,
                    None => return EINVAL,
                };
                let buffer_ptr = match validate_user_addr(buffer_ptr) {
                    Ok(ptr) => ptr,
                    Err(e) => return e,
                };
                if validate_user_range(buffer_ptr.as_u64(), write_bytes).is_err() {
                    return EINVAL;
                }
                unsafe {
                    let buffer = buffer_ptr.as_mut_ptr::<u64>();
                    for (i, child) in children.iter().enumerate().take(to_copy) {
                        *buffer.add(i) = child.raw();
                    }
                }
                log_debug!(
                    "syscall",
                    "cap_query_children: copied {} handles to buffer",
                    to_copy
                );
            }

            count as u64
        }
        Err(err) => {
            log_warn!(
                "syscall",
                "cap_query_children: capability not found or invalid: {:?}",
                err
            );
            EINVAL
        }
    }
}

fn sys_shared_region_create(size: u64) -> u64 {
    log_info!(
        "syscall",
        "shared_region_create(size={})",
        size
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "shared_region_create: no current thread"
            );
            return EINVAL;
        }
    };

    let caller_process =
        match policy::require_shared_region_caller_process(caller, "shared_region_create") {
            Ok(process_id) => process_id,
            Err(e) => return e,
        };

    match crate::shared_mem::create_region(caller_process, size as usize) {
        Ok(region_id) => {
            log_debug!(
                "syscall",
                "shared_region_create: created region {:?} with size {} bytes for process {}",
                region_id,
                size,
                caller_process
            );
            region_id.raw()
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "shared_region_create: failed - {:?}",
                e
            );
            match e {
                crate::shared_mem::SharedMemError::InvalidSize => EINVAL,
                crate::shared_mem::SharedMemError::OutOfMemory => ENOMEM,
                _ => EINVAL,
            }
        }
    }
}

fn sys_shared_region_map(region_id_raw: u64, virt_addr: u64, flags_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "shared_region_map(region={}, virt={:#x}, flags={:#x})",
        region_id_raw,
        virt_addr,
        flags_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "shared_region_map: no current thread"
            );
            return EINVAL;
        }
    };

    let caller_process =
        match policy::require_shared_region_caller_process(caller, "shared_region_map") {
            Ok(process_id) => process_id,
            Err(e) => return e,
        };

    // Get the caller's PML4 (address space)
    let caller_pml4 = match crate::thread::get_thread_address_space(caller) {
        Some(pml4) => pml4,
        None => {
            log_error!("syscall", "shared_region_map: caller thread {} not found", caller);
            return EINVAL;
        }
    };

    // Log current CR3 vs caller's address space (debug the bug!)
    let current_cr3 = unsafe {
        let cr3: u64;
        core::arch::asm!("mov {}, cr3", out(reg) cr3);
        cr3
    };
    let current_pml4 = crate::arch::cr3_to_pml4_phys(current_cr3);
    
    log_info!(
        "syscall",
        "shared_region_map: current_cr3={:#X} current_pml4={:#X} caller_pml4={:#X} match={}",
        current_cr3,
        current_pml4,
        caller_pml4,
        current_pml4 == caller_pml4
    );

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);
    let flags = crate::shared_mem::RegionFlags::from_raw(flags_raw);
    let requested_addr = if virt_addr == 0 {
        None
    } else {
        match validate_user_addr(virt_addr) {
            Ok(addr) => Some(addr),
            Err(e) => return e,
        }
    };

    match crate::shared_mem::map_region_in_pml4(region_id, caller_process, caller_pml4, requested_addr, flags) {
        Ok(mapped_va) => {
            // Sanity-check: the kernel must never hand userspace the null address.
            // find_free_va starts above the identity-map ceiling (>= 0x20000000)
            // so this should never trigger, but we assert defensively.
            if mapped_va == 0 {
                log_error!(
                    "syscall",
                    "shared_region_map: BUG — mapped_va=0 for region {:?}! Rejecting.",
                    region_id
                );
                return EINVAL;
            }
            if validate_user_return_addr(mapped_va as atom_abi::UserVirtAddr).is_err() {
                log_error!(
                    "syscall",
                    "[ABI VIOLATION] shared_region_map returned invalid userspace VA {:#X} for region {:?}",
                    mapped_va,
                    region_id
                );
                return EINVAL;
            }
            log_info!(
                "syscall",
                "[ABI] return addr={:#x} pid={} syscall=shared_region_map",
                mapped_va,
                caller_process
            );
            log_info!(
                "syscall",
                "shared_region_map: region {:?} mapped at actual_va={:#X} (requested={:#x})",
                region_id,
                mapped_va,
                virt_addr
            );
            // Return the actual mapped VA.  For auto-assign (virt_addr==0) the
            // caller needs this to know where the mapping ended up.  For
            // explicit VA requests the returned value equals the requested VA.
            // User-space VA values are always below SYSCALL_ERROR_THRESHOLD
            // (u64::MAX - 256), so there is no ambiguity with error codes.
            mapped_va as u64
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "shared_region_map: failed - {:?}",
                e
            );
            match e {
                crate::shared_mem::SharedMemError::InvalidRegion => EINVAL,
                crate::shared_mem::SharedMemError::Unaligned => EINVAL,
                crate::shared_mem::SharedMemError::AlreadyMapped => EBUSY,
                crate::shared_mem::SharedMemError::AddressInUse => EBUSY,
                crate::shared_mem::SharedMemError::OutOfMemory => ENOMEM,
                crate::shared_mem::SharedMemError::NoFreeVirtualAddress => ENOMEM,
                _ => EINVAL,
            }
        }
    }
}

fn sys_shared_region_unmap(region_id_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "shared_region_unmap(region={})",
        region_id_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "shared_region_unmap: no current thread"
            );
            return EINVAL;
        }
    };

    let caller_process =
        match policy::require_shared_region_caller_process(caller, "shared_region_unmap") {
            Ok(process_id) => process_id,
            Err(e) => return e,
        };

    let caller_pml4 = match crate::thread::get_thread_address_space(caller) {
        Some(pml4) => pml4,
        None => {
            log_error!(
                "syscall",
                "shared_region_unmap: caller thread {} not found",
                caller
            );
            return EINVAL;
        }
    };

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);

    match crate::shared_mem::unmap_region(region_id, caller_process, caller_pml4) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "shared_region_unmap: unmapped region {:?}",
                region_id
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "shared_region_unmap: failed - {:?}",
                e
            );
            match e {
                crate::shared_mem::SharedMemError::InvalidRegion => EINVAL,
                crate::shared_mem::SharedMemError::NotMapped => EINVAL,
                _ => EINVAL,
            }
        }
    }
}

fn sys_shared_region_destroy(region_id_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "shared_region_destroy(region={})",
        region_id_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "shared_region_destroy: no current thread"
            );
            return EINVAL;
        }
    };

    let caller_process =
        match policy::require_shared_region_caller_process(caller, "shared_region_destroy") {
            Ok(process_id) => process_id,
            Err(e) => return e,
        };

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);

    match crate::shared_mem::destroy_region(region_id, caller_process) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "shared_region_destroy: destroyed region {:?}",
                region_id
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "shared_region_destroy: failed - {:?}",
                e
            );
            match e {
                crate::shared_mem::SharedMemError::InvalidRegion => EINVAL,
                crate::shared_mem::SharedMemError::PermissionDenied => EPERM,
                crate::shared_mem::SharedMemError::RegionInUse => EBUSY,
                _ => EINVAL,
            }
        }
    }
}

fn sys_addrspace_create() -> u64 {
    log_info!(
        "syscall",
        "addrspace_create()"
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "addrspace_create: no current thread"
            );
            return EINVAL;
        }
    };

    match crate::mm::addrspace::create_address_space(caller) {
        Ok(as_id) => {
            log_debug!(
                "syscall",
                "addrspace_create: created address space {:?}",
                as_id
            );
            as_id.raw()
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "addrspace_create: failed - {:?}",
                e
            );
            match e {
                crate::mm::addrspace::AddressSpaceError::OutOfMemory => ENOMEM,
                _ => EINVAL,
            }
        }
    }
}

fn sys_addrspace_destroy(as_id_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "addrspace_destroy(as={})",
        as_id_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "addrspace_destroy: no current thread"
            );
            return EINVAL;
        }
    };

    let as_id = crate::mm::addrspace::AddressSpaceId::from_raw(as_id_raw);

    match crate::mm::addrspace::destroy_address_space(as_id, caller) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "addrspace_destroy: destroyed address space {:?}",
                as_id
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "addrspace_destroy: failed - {:?}",
                e
            );
            match e {
                crate::mm::addrspace::AddressSpaceError::NotFound => EINVAL,
                crate::mm::addrspace::AddressSpaceError::PermissionDenied => EPERM,
                crate::mm::addrspace::AddressSpaceError::InUse => EBUSY,
                _ => EINVAL,
            }
        }
    }
}

fn sys_map_region(
    as_id_raw: u64,
    virt_addr: u64,
    phys_addr: u64,
    size: u64,
    flags_raw: u64,
) -> u64 {
    log_info!(
        "syscall",
        "map_region(as={}, virt=0x{:X}, phys=0x{:X}, size={}, flags=0x{:X})",
        as_id_raw,
        virt_addr,
        phys_addr,
        size,
        flags_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "map_region: no current thread");
            return EINVAL;
        }
    };

    let as_id = crate::mm::addrspace::AddressSpaceId::from_raw(as_id_raw);
    let user_range = match atom_abi::validate_user_range(virt_addr as usize, size as usize) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    // Security: memory isolation enforced by AddressSpaceManager::is_owned_by()
    // (addrspace.rs:311). MemoryRegion caps use exact-match and are never granted;
    // range-based redesign is future work.

    let mut flags = crate::mm::vm::PageFlags::from_bits(flags_raw);
    flags |= crate::mm::vm::PageFlags::PRESENT | crate::mm::vm::PageFlags::USER;

    match crate::mm::addrspace::map_region(
        as_id,
        caller,
        user_range,
        phys_addr as usize,
        flags,
    ) {
        Ok(()) => {
            log_debug!("syscall", "map_region: success");
            ESUCCESS
        }
        Err(e) => {
            log_warn!("syscall", "map_region: failed - {:?}", e);
            match e {
                crate::mm::addrspace::AddressSpaceError::OutOfMemory => ENOMEM,
                crate::mm::addrspace::AddressSpaceError::PermissionDenied => EPERM,
                crate::mm::addrspace::AddressSpaceError::NotFound => EINVAL,
                _ => EINVAL,
            }
        }
    }
}

fn sys_unmap_region(as_id_raw: u64, virt_addr: u64, size: u64) -> u64 {
    log_info!(
        "syscall",
        "unmap_region(as={}, virt=0x{:X}, size={})",
        as_id_raw,
        virt_addr,
        size
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "unmap_region: no current thread"
            );
            return EINVAL;
        }
    };

    let as_id = crate::mm::addrspace::AddressSpaceId::from_raw(as_id_raw);
    let user_range = match atom_abi::validate_user_range(virt_addr as usize, size as usize) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    // Security: memory isolation enforced by AddressSpaceManager::is_owned_by()
    // (addrspace.rs:311). MemoryRegion caps use exact-match and are never granted;
    // range-based redesign is future work.

    match crate::mm::addrspace::unmap_region(
        as_id,
        caller,
        user_range,
    ) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "unmap_region: success"
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "unmap_region: failed - {:?}",
                e
            );
            match e {
                crate::mm::addrspace::AddressSpaceError::NotFound => EINVAL,
                crate::mm::addrspace::AddressSpaceError::PermissionDenied => EPERM,
                crate::mm::addrspace::AddressSpaceError::InvalidAddress => EINVAL,
                crate::mm::addrspace::AddressSpaceError::InvalidSize => EINVAL,
                crate::mm::addrspace::AddressSpaceError::NotMapped => EINVAL,
                _ => EINVAL,
            }
        }
    }
}

fn sys_remap_region(as_id_raw: u64, old_virt: u64, new_virt: u64, size: u64) -> u64 {
    log_info!(
        "syscall",
        "remap_region(as={}, old=0x{:X}, new=0x{:X}, size={})",
        as_id_raw,
        old_virt,
        new_virt,
        size
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(
                "syscall",
                "remap_region: no current thread"
            );
            return EINVAL;
        }
    };

    let as_id = crate::mm::addrspace::AddressSpaceId::from_raw(as_id_raw);
    let old_range = match atom_abi::validate_user_range(old_virt as usize, size as usize) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };
    let new_range = match atom_abi::validate_user_range(new_virt as usize, size as usize) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    // Security: memory isolation enforced by AddressSpaceManager::is_owned_by()
    // (addrspace.rs:311). MemoryRegion caps use exact-match and are never granted;
    // range-based redesign is future work.

    match crate::mm::addrspace::remap_region(
        as_id,
        caller,
        old_range,
        new_range,
    ) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "remap_region: success"
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "remap_region: failed - {:?}",
                e
            );
            match e {
                crate::mm::addrspace::AddressSpaceError::NotFound => EINVAL,
                crate::mm::addrspace::AddressSpaceError::PermissionDenied => EPERM,
                crate::mm::addrspace::AddressSpaceError::InvalidAddress => EINVAL,
                crate::mm::addrspace::AddressSpaceError::InvalidSize => EINVAL,
                crate::mm::addrspace::AddressSpaceError::KernelSpaceViolation => EPERM,
                crate::mm::addrspace::AddressSpaceError::NotMapped => EINVAL,
                _ => EINVAL,
            }
        }
    }
}

fn sys_register_fault_handler(port_id_raw: u64) -> u64 {
    log_info!(
        "syscall",
        "register_fault_handler(port={})",
        port_id_raw
    );

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_warn!(
                "syscall",
                "register_fault_handler: no current thread"
            );
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    match crate::mm::policy::register_page_fault_handler(port_id, caller) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "register_fault_handler: port {:?} now receiving page faults",
                port_id
            );
            ESUCCESS
        }
        Err(e) => {
            log_warn!(
                "syscall",
                "register_fault_handler failed: {:?}",
                e
            );
            match e {
                crate::mm::policy::MemoryPolicyError::InvalidPort => EINVAL,
                crate::mm::policy::MemoryPolicyError::PermissionDenied => EPERM,
                _ => EINVAL,
            }
        }
    }
}

// ============================================================================
// IRQ Handler Registration for Userspace Drivers
// ============================================================================

use spin::Mutex;
use alloc::collections::{BTreeMap, BTreeSet};

struct IrqBinding {
    owner: crate::thread::ThreadId,
    port: u64,
    pending: AtomicBool,
}

/// Registered IRQ handlers - maps IRQ number to IrqBinding
static IRQ_BINDINGS: Mutex<BTreeMap<u8, IrqBinding>> = Mutex::new(BTreeMap::new());
static CLEANED_FD_PROCESSES: Mutex<BTreeSet<crate::process::ProcessId>> =
    Mutex::new(BTreeSet::new());

/// Register an IRQ handler for userspace
fn sys_register_irq_handler(irq: u8, notification_port: u64) -> u64 {
    if let Err(e) = policy::validate_irq_registration(irq) {
        return e;
    }

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    let mut bindings = IRQ_BINDINGS.lock();

    if bindings.contains_key(&irq) {
        log_warn!(
            "syscall",
            "IRQ {} already has registered handler",
            irq
        );
        return EBUSY;
    }

    bindings.insert(irq, IrqBinding {
        owner: caller,
        port: notification_port,
        pending: AtomicBool::new(false),
    });

    log_info!(
        "syscall",
        "Thread {} registered as handler for IRQ {} (port {})",
        caller,
        irq,
        notification_port
    );

    ESUCCESS
}

/// Unregister an IRQ handler
fn sys_unregister_irq_handler(irq: u8) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    let mut bindings = IRQ_BINDINGS.lock();

    if let Some(binding) = bindings.get(&irq) {
        if let Err(e) = policy::validate_irq_owner(binding.owner, caller) {
            return e;
        }
        bindings.remove(&irq);
        log_info!(
            "syscall",
            "Thread {} unregistered handler for IRQ {}",
            caller,
            irq
        );
        ESUCCESS
    } else {
        EINVAL
    }
}

/// Called from interrupt handlers to notify userspace of IRQ
pub fn notify_irq_handler(irq: u8) {
    let bindings = IRQ_BINDINGS.lock();

    if let Some(binding) = bindings.get(&irq) {
        // Coalesced delivery: only send if not already pending
        if binding.pending.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            // Send notification via IPC port
            let port_id = crate::ipc::PortId::from_raw(binding.port);

            // Create a libipc-compatible IrqNotification message (MessageType 20)
            // Header: [msg_type (4 bytes), payload_size (4 bytes), sequence (4 bytes)]
            // Followed by 1 byte for the IRQ number.
            let mut payload = alloc::vec![0u8; 13];
            payload[0..4].copy_from_slice(&20u32.to_le_bytes()); // IrqNotification
            payload[4..8].copy_from_slice(&1u32.to_le_bytes());  // payload_size = 1
            // sequence = 0 at [8..12]
            payload[12] = irq;

            let msg = crate::ipc::Message::new(
                crate::thread::ThreadId::from_raw(0), // Kernel sender
                20u32, // Message type is IrqNotification
                payload,
            );

            // Non-blocking send - we're in interrupt context
            if let Err(e) = crate::ipc::send_message_async(port_id, msg) {
                log_debug!(
                    "syscall",
                    "Failed to notify IRQ {} handler: {:?}",
                    irq,
                    e
                );
                // Reset pending if send failed
                binding.pending.store(false, Ordering::SeqCst);
            }
        }
    }
}

/// Check if an IRQ has a userspace handler registered
pub fn has_userspace_irq_handler(irq: u8) -> bool {
    let bindings = IRQ_BINDINGS.lock();
    bindings.contains_key(&irq)
}

// ============================================================================
// Framebuffer Mapping for Userspace
// ============================================================================

/// Map framebuffer to userspace address
fn sys_map_framebuffer_to_user(user_buffer: u64) -> u64 {
    use crate::graphics;

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    // Get framebuffer info
    let fb_info = match graphics::with_framebuffer(|fb| {
        (
            fb.address() as usize,
            fb.width(),
            fb.height(),
            fb.stride(),
            fb.bytes_per_pixel(),
        )
    }) {
        Some(info) => info,
        None => return EINVAL,
    };

    let (address, width, height, stride, bpp) = fb_info;

    // Calculate framebuffer size
    let fb_size = (stride as usize) * (height as usize) * bpp;

    // The framebuffer is already mapped in kernel space
    // For userspace access, we need to remap with USER flag
    // For now, just return the info - the framebuffer is identity-mapped

    // Write info to user buffer if provided
    if user_buffer != 0 {
        let user_buffer = match validate_user_addr(user_buffer) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        };
        if validate_user_range(user_buffer.as_u64(), 6 * core::mem::size_of::<u64>()).is_err() {
            return EINVAL;
        }

        let info_ptr = user_buffer.as_mut_ptr::<u64>();
        unsafe {
            core::ptr::write_volatile(info_ptr, address as u64);
            core::ptr::write_volatile(info_ptr.add(1), width as u64);
            core::ptr::write_volatile(info_ptr.add(2), height as u64);
            core::ptr::write_volatile(info_ptr.add(3), stride as u64);
            core::ptr::write_volatile(info_ptr.add(4), bpp as u64);
            core::ptr::write_volatile(info_ptr.add(5), fb_size as u64);
        }
    }

    log_info!(
        "syscall",
        "Thread {} mapped framebuffer: addr={:#X} {}x{} stride={} bpp={} size={}",
        caller,
        address,
        width,
        height,
        stride,
        bpp,
        fb_size
    );

    ESUCCESS
}

// ============================================================================
// Event-Based Input Primitives for Userspace Drivers
// ============================================================================

/// IRQ occurrence counters for userspace polling
static IRQ_COUNTS: Mutex<BTreeMap<u8, u64>> = Mutex::new(BTreeMap::new());

/// Increment IRQ count (called from interrupt handlers)
pub fn increment_irq_count(irq: u8) {
    let mut counts = IRQ_COUNTS.lock();
    *counts.entry(irq).or_insert(0) += 1;
}

/// Get current IRQ count for a registered handler
/// Userspace can use this to detect new events without IPC overhead
fn sys_get_irq_count(irq: u8) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    // Verify caller owns this IRQ handler
    let bindings = IRQ_BINDINGS.lock();
    match bindings.get(&irq) {
        Some(binding) if policy::validate_irq_owner(binding.owner, caller).is_ok() => {
            drop(bindings);
            let counts = IRQ_COUNTS.lock();
            counts.get(&irq).copied().unwrap_or(0)
        }
        Some(_) => EPERM,
        None => EINVAL,
    }
}

/// Wait for any of multiple IPC ports to have data
///
/// Args:
///   ports_ptr: Pointer to array of port IDs to wait on
///   count: Number of ports in the array
///   timeout_ms: Timeout in milliseconds (0 = no wait, u64::MAX = infinite)
///
/// Returns:
///   Index of the port with data (0-based), or error code
fn sys_ipc_wait_any(ports_ptr: u64, count: u64, timeout_ms: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    if count == 0 || count > 64 {
        return EINVAL;
    }

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    // Validate user-space read range for the port ID array
    let read_bytes = match (count as usize).checked_mul(core::mem::size_of::<u64>()) {
        Some(v) => v,
        None => return EINVAL,
    };
    let ports_ptr = match validate_user_ptr::<u64>(ports_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let _ports_range = match validate_user_byte_range(ports_ptr.cast::<u8>(), read_bytes) {
        Ok(range) => range,
        Err(e) => return e,
    };

    // Read port IDs from userspace
    let mut ports = alloc::vec::Vec::with_capacity(count as usize);
    unsafe {
        let ptr = ports_ptr.as_ptr();
        for i in 0..count as usize {
            ports.push(crate::ipc::PortId::from_raw(*ptr.add(i)));
        }
    }

    // Calculate deadline
    let deadline = if timeout_ms == u64::MAX {
        None
    } else if timeout_ms == 0 {
        Some(crate::interrupts::get_ticks()) // Immediate check only
    } else {
        let ticks = timeout_ms.div_ceil(10);
        Some(crate::interrupts::get_ticks() + ticks)
    };

    // Polling loop - check each port for messages (peek without consuming)
    loop {
        for (idx, port_id) in ports.iter().enumerate() {
            // Use has_message to check without consuming the message
            // This allows userspace to receive the message via try_recv after wait_any returns
            match crate::ipc::has_message(*port_id) {
                Ok(true) => {
                    // Found a message! Unregister and return the port index
                    crate::ipc::unregister_wait_any(caller, &ports);
                    log_debug!(
                        LOG_ORIGIN,
                        "ipc_wait_any: port {} (index {}) has message",
                        port_id,
                        idx
                    );
                    return idx as u64;
                }
                Ok(false) => continue,
                Err(_) => continue, // Skip invalid ports
            }
        }

        // Check timeout
        if let Some(deadline_tick) = deadline {
            if crate::interrupts::get_ticks() >= deadline_tick {
                crate::ipc::unregister_wait_any(caller, &ports);
                if timeout_ms == 0 {
                    return EWOULDBLOCK;
                } else {
                    return ETIMEDOUT;
                }
            }
        }

        // Register as waiting on all ports BEFORE blocking
        // This allows senders to wake us up
        crate::ipc::register_wait_any(caller, &ports);

        // Block this thread and yield to scheduler
        // Mark as blocked so scheduler won't immediately pick us
        crate::thread::set_thread_state(caller, crate::thread::ThreadState::Blocked);

        // Try to switch to another thread
        let (prev, next) = crate::sched::on_timer_tick();
        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                // Switch to different thread
                crate::sched::perform_context_switch(prev_id, next_id);
            } else {
                // No other thread to run - wait for interrupt (avoid busy-loop)
                // Enable interrupts and halt until next interrupt
                unsafe {
                    core::arch::asm!(
                        "sti",      // Enable interrupts
                        "hlt",      // Halt until interrupt
                        "cli",      // Disable interrupts again
                        options(nomem, nostack)
                    );
                }
            }
        } else {
            // No threads available - halt and wait
            unsafe {
                core::arch::asm!(
                    "sti",
                    "hlt",
                    "cli",
                    options(nomem, nostack)
                );
            }
        }

        // Back from blocking - mark as ready and unregister
        crate::thread::set_thread_state(caller, crate::thread::ThreadState::Ready);
        // Note: We'll re-register on the next iteration if we loop again
    }
}

/// Spawn a new process from a registered driver
///
/// This syscall creates a new process by loading an ATXF executable from
/// the driver registry. The driver must have been loaded by the bootloader.
///
/// # Arguments
/// * `name_ptr` - Pointer to the driver name (null-terminated or length-bounded)
/// * `name_len` - Length of the driver name
///
/// # Returns
/// * On success: The new process ID (PID)
/// * On failure: Error code (EINVAL, ENOTFOUND, ENOMEM)
fn sys_spawn_process(name_ptr: u64, name_len: usize) -> u64 {
    const LOG_ORIGIN: &str = "syscall:spawn";

    log_info!(
        LOG_ORIGIN,
        "spawn_process(name_ptr={:#X}, name_len={})",
        name_ptr,
        name_len
    );

    // Validate arguments
    if name_len == 0 || name_len > 64 {
        log_warn!(LOG_ORIGIN, "spawn_process: invalid arguments");
        return EINVAL;
    }
    let name_ptr = match validate_user_ptr::<u8>(name_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let name_range = match validate_user_byte_range(name_ptr, name_len) {
        Ok(range) => range,
        Err(_) => {
            log_warn!(LOG_ORIGIN, "spawn_process: invalid userspace range");
            return EINVAL;
        }
    };
    if name_range.len() != name_len {
        log_warn!(LOG_ORIGIN, "spawn_process: invalid userspace range");
        return EINVAL;
    }

    // Copy name from userspace
    // SAFETY: name_range is ABI-validated and guaranteed canonical.
    let name_bytes = unsafe { core::slice::from_raw_parts(name_range.base().as_ptr::<u8>(), name_range.len()) };
    let name = match core::str::from_utf8(name_bytes) {
        Ok(s) => s.trim_end_matches('\0'),
        Err(_) => {
            log_warn!(LOG_ORIGIN, "spawn_process: invalid UTF-8 in name");
            return EINVAL;
        }
    };

    log_info!(LOG_ORIGIN, "Looking up driver: '{}'", name);

    // First try to load from filesystem (dynamic loading)
    // Then fall back to boot-loaded driver registry
    if crate::drivers::fat32::is_available() {
        // Try loading from filesystem
        let path = alloc::format!("/drivers/{}.atxf", name);
        log_info!(LOG_ORIGIN, "Trying to load from filesystem: {}", path);

        if let Some(data) = crate::drivers::fat32::open(&path) {
            log_info!(LOG_ORIGIN, "Loaded {} bytes from filesystem", data.len());
            return spawn_from_image(&data, name);
        }
        // Fall back to registry if not found in filesystem
        log_debug!(LOG_ORIGIN, "Not found in filesystem, trying registry");
    }

    // Load from boot-loaded driver registry
    match load_from_registry(name) {
        Ok(sections) => {
            log_info!(
                LOG_ORIGIN,
                "Executable parsed: text={} bytes, data={} bytes, bss={} bytes, entry=0x{:X}",
                sections.text.len(),
                sections.data.len(),
                sections.bss_size,
                sections.entry_offset
            );
            match spawn_process_internal(name, &sections) {
                Ok(pid) => {
                    log_info!(LOG_ORIGIN, "Process '{}' spawned successfully with PID {}", name, pid);
                    pid.raw()
                }
                Err(e) => {
                    log_error!(LOG_ORIGIN, "spawn_process: failed to spawn: {:?}", e);
                    e
                }
            }
        }
        Err(e) => e,
    }
}

/// Spawn a process from raw image data
fn spawn_from_image(data: &[u8], name: &str) -> u64 {
    const LOG_ORIGIN: &str = "syscall:spawn";

    let sections = match crate::executable::parse_image(data) {
        Ok(s) => s,
        Err(e) => {
            log_error!(LOG_ORIGIN, "spawn_process: failed to parse executable: {:?}", e);
            return EINVAL;
        }
    };

    log_info!(
        LOG_ORIGIN,
        "Executable parsed: text={} bytes, data={} bytes, bss={} bytes, entry=0x{:X}",
        sections.text.len(),
        sections.data.len(),
        sections.bss_size,
        sections.entry_offset
    );

    match spawn_process_internal(name, &sections) {
        Ok(pid) => {
            log_info!(LOG_ORIGIN, "Process '{}' spawned successfully with PID {}", name, pid);
            pid.raw()
        }
        Err(e) => {
            log_error!(LOG_ORIGIN, "spawn_process: failed to spawn: {:?}", e);
            e
        }
    }
}

/// Load driver from boot-loaded registry
fn load_from_registry(name: &str) -> Result<crate::executable::ExecutableSections<'_>, u64> {
    let driver_image = crate::driver_registry::get_driver_image(name)
        .ok_or(ENOTFOUND)?;

    log_info!(
        "syscall:spawn",
        "Found driver '{}' in registry: ptr={:p}, size={}",
        name,
        driver_image.ptr,
        driver_image.size
    );

    let image_bytes = unsafe {
        core::slice::from_raw_parts(driver_image.ptr, driver_image.size)
    };

    crate::executable::parse_image(image_bytes).map_err(|e| {
        log_error!("syscall:spawn", "Failed to parse executable: {:?}", e);
        EINVAL
    })
}

/// Map driver name to static string for Thread struct
fn get_static_driver_name(name: &str) -> &'static str {
    match name {
        "init" => "init",
        "namesvc" => "namesvc",
        "service_manager" => "service_manager",
        "fsd" => "fsd",
        "ui_shell" => "ui_shell",
        "terminal" => "terminal",
        "keyboard" => "keyboard",
        "mouse" => "mouse",
        "display" => "display",
        "browser" => "browser",
        "files" => "files",
        "settings" => "settings",
        "app_launcher" => "app_launcher",
        "app" => "app",
        "hello_atxf" => "hello_atxf",
        _ => "unknown",
    }
}

/// Internal function to create a new userspace process from parsed sections.
///
/// This function now integrates with the VMA subsystem:
/// - Text, data, and BSS sections are still eagerly mapped (they contain data)
/// - The user stack uses a fixed mapped region with one unmapped guard page
/// - A heap VMA is pre-registered for brk() support
fn spawn_process_internal(
    name: &str,
    sections: &crate::executable::ExecutableSections,
) -> Result<crate::thread::ThreadId, u64> {
    // Get static name for the thread
    let static_name = get_static_driver_name(name);
    use crate::cap::{self, CapPermissions, InputDeviceType, ResourceType};
    use crate::executable::USER_EXEC_LOAD_BASE;
    use crate::mm::pmm::{self, align_up, PAGE_SIZE};
    use crate::mm::vm::{self, PageFlags};
    use crate::mm::vma::{self, PageSource, Vma, VmaBacking, VmaPermissions};
    use crate::thread::{CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};

    const KERNEL_STACK_PAGES: usize = 16;  // 64KB kernel stack to handle deep call stacks
    const USER_STACK_TOP: usize = atom_abi::USER_STACK_TOP as usize;

    let pid = ThreadId::new();
    let process_id = crate::process::ProcessId::from(pid);

    // Each process gets its own address space - load at the standard base address
    // since each process has isolated virtual memory
    let text_base = USER_EXEC_LOAD_BASE;
    let user_stack_top = USER_STACK_TOP;
    let stack_layout = crate::thread::UserStackMetadata::default_for_top(user_stack_top);
    let user_stack_base = stack_layout.usable_base as usize;

    log_info!(
        "spawn",
        "Creating process '{}' (pid={}) at text=0x{:X}, stack=0x{:X}",
        name,
        pid,
        text_base,
        user_stack_top
    );

    // Create a new address space for this process
    let new_pml4_phys = pmm::alloc_pages_zeroed(1).ok_or(ENOMEM)?;

    // Clone kernel mappings to the new address space
    vm::clone_kernel_mappings(new_pml4_phys).map_err(|_| ENOMEM)?;

    // Register the PML4 as protected (Req 2.4)
    pmm::register_active_pml4(new_pml4_phys).map_err(|_| ENOMEM)?;

    // Create VMA map for this address space
    vma::create_bootstrap_process_vma_map(process_id, new_pml4_phys);

    log_info!(
        "spawn",
        "Created new address space for '{}': PML4=0x{:X}",
        name,
        new_pml4_phys
    );

    // Allocate and map text section in the NEW address space
    let text_size = align_up(sections.text.len().max(1));
    let text_pages = text_size / PAGE_SIZE;

    let text_phys = pmm::alloc_pages_zeroed(text_pages)
        .ok_or(ENOMEM)?;

    // Copy text section content (use higher-half address to avoid broken identity mapping)
    unsafe {
        core::ptr::copy_nonoverlapping(
            sections.text.as_ptr(),
            vm::phys_to_virt_ptr(text_phys) as *mut u8,
            sections.text.len(),
        );
    }

    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let phys = text_phys + i * PAGE_SIZE;
        vm::remap_page_in_pml4(new_pml4_phys, virt, phys, PageFlags::PRESENT | PageFlags::USER)
            .map_err(|_| ENOMEM)?;
    }

    // Register text VMA
    vma::insert_bootstrap_process_vma(process_id, new_pml4_phys, Vma {
        start: text_base,
        end: text_base + text_size,
        perms: VmaPermissions::read_exec(),
        backing: VmaBacking::Anonymous,
        label: "text",
    }).map_err(|e| {
        log_error!("spawn", "Failed to insert text VMA for '{}': {:?}", name, e);
        ENOMEM
    })?;
    vma::account_pre_mapped_range(
        process_id,
        new_pml4_phys,
        text_base,
        text_base + text_size,
        PageSource::Anonymous,
    ).map_err(|e| {
        log_error!("spawn", "Failed to account text pages for '{}': {:?}", name, e);
        ENOMEM
    })?;

    // Allocate and map data section
    let data_base = align_up(text_base + text_size);
    let data_size = align_up(sections.data.len().max(1));
    let data_pages = data_size / PAGE_SIZE;

    if !sections.data.is_empty() {
        let data_phys = pmm::alloc_pages_zeroed(data_pages)
            .ok_or(ENOMEM)?;

        // Copy data section content
        unsafe {
            core::ptr::copy_nonoverlapping(
                sections.data.as_ptr(),
                vm::phys_to_virt_ptr(data_phys) as *mut u8,
                sections.data.len(),
            );
        }

        for i in 0..data_pages {
            let virt = data_base + i * PAGE_SIZE;
            let phys = data_phys + i * PAGE_SIZE;
            vm::remap_page_in_pml4(
                new_pml4_phys,
                virt,
                phys,
                PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
            ).map_err(|_| ENOMEM)?;
        }

        // Register data VMA
        vma::insert_bootstrap_process_vma(process_id, new_pml4_phys, Vma {
            start: data_base,
            end: data_base + data_size,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "data",
        }).map_err(|e| {
            log_error!("spawn", "Failed to insert data VMA for '{}': {:?}", name, e);
            ENOMEM
        })?;
        vma::account_pre_mapped_range(
            process_id,
            new_pml4_phys,
            data_base,
            data_base + data_size,
            PageSource::Anonymous,
        ).map_err(|e| {
            log_error!("spawn", "Failed to account data pages for '{}': {:?}", name, e);
            ENOMEM
        })?;
    }

    // Allocate and map BSS section
    let bss_base = align_up(data_base + data_size);
    let bss_size = sections.bss_size.max(1);
    let bss_pages = align_up(bss_size) / PAGE_SIZE;

    let bss_phys = pmm::alloc_pages_zeroed(bss_pages)
        .ok_or(ENOMEM)?;

    for i in 0..bss_pages {
        let virt = bss_base + i * PAGE_SIZE;
        let phys = bss_phys + i * PAGE_SIZE;
        vm::remap_page_in_pml4(
            new_pml4_phys,
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        ).map_err(|_| ENOMEM)?;
    }

    // Register BSS VMA
    vma::insert_bootstrap_process_vma(process_id, new_pml4_phys, Vma {
        start: bss_base,
        end: bss_base + align_up(bss_size),
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "bss",
    }).map_err(|e| {
        log_error!("spawn", "Failed to insert bss VMA for '{}': {:?}", name, e);
        ENOMEM
    })?;
    vma::account_pre_mapped_range(
        process_id,
        new_pml4_phys,
        bss_base,
        bss_base + align_up(bss_size),
        PageSource::Anonymous,
    ).map_err(|e| {
        log_error!("spawn", "Failed to account bss pages for '{}': {:?}", name, e);
        ENOMEM
    })?;

    // Allocate the full userspace stack while leaving the guard page unmapped.
    let stack_phys = pmm::alloc_pages_zeroed(atom_abi::DEFAULT_USER_STACK_PAGES)
        .ok_or(ENOMEM)?;

    for i in 0..atom_abi::DEFAULT_USER_STACK_PAGES {
        let virt = user_stack_base + i * PAGE_SIZE;
        let phys = stack_phys + i * PAGE_SIZE;
        vm::remap_page_in_pml4(
            new_pml4_phys,
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        ).map_err(|_| ENOMEM)?;
    }

    // Register the mapped stack range only. The guard page below it remains unmapped.
    vma::insert_bootstrap_process_vma(process_id, new_pml4_phys, Vma {
        start: user_stack_base,
        end: user_stack_top,
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "stack",
    }).map_err(|e| {
        log_error!("spawn", "Failed to insert stack VMA for '{}': {:?}", name, e);
        ENOMEM
    })?;
    vma::account_pre_mapped_range(
        process_id,
        new_pml4_phys,
        user_stack_base,
        user_stack_top,
        PageSource::Anonymous,
    ).map_err(|e| {
        log_error!("spawn", "Failed to account stack pages for '{}': {:?}", name, e);
        ENOMEM
    })?;

    // Register a heap VMA (starts empty, grows via brk())
    let heap_start = atom_abi::USER_HEAP_START as usize;
    let bss_end = bss_base + align_up(bss_size);
    if bss_end > heap_start {
        log_error!(
            "spawn",
            "Executable '{}' too large: BSS end 0x{:X} exceeds USER_HEAP_START 0x{:X}",
            name,
            bss_end,
            heap_start
        );
        return Err(ENOMEM);
    }
    vma::insert_bootstrap_process_vma(process_id, new_pml4_phys, Vma {
        start: heap_start,
        end: heap_start + PAGE_SIZE, // Minimal initial size
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "heap",
    }).map_err(|e| {
        log_error!("spawn", "Failed to insert heap VMA for '{}': {:?}", name, e);
        ENOMEM
    })?;

    // Allocate kernel stack
    // CRITICAL: Use higher-half virtual address for kernel stack, not identity-mapped address.
    let kernel_stack_phys = pmm::alloc_pages(KERNEL_STACK_PAGES)
        .ok_or(ENOMEM)?;
    let kernel_stack_virt = vm::HIGHER_HALF_BASE + kernel_stack_phys;
    let kernel_stack_top = (kernel_stack_virt + KERNEL_STACK_PAGES * PAGE_SIZE) as u64;

    // Calculate entry point
    let entry_point = text_base + sections.entry_offset;

    log_info!(
        "spawn",
        "Process memory: text=0x{:X}-0x{:X}, data=0x{:X}, bss=0x{:X}, stack_guard=0x{:X}-0x{:X}, stack=0x{:X}-0x{:X} ({}KB), entry=0x{:X}",
        text_base,
        text_base + text_size,
        data_base,
        bss_base,
        stack_layout.guard_base,
        stack_layout.usable_base,
        user_stack_base,
        user_stack_top,
        stack_layout.usable_size() / 1024,
        entry_point
    );

    // Create CPU context for Ring 3 execution with the NEW address space
    let context = CpuContext::new_user(
        entry_point as u64,
        user_stack_top as u64,
        new_pml4_phys as u64,
    );

    // CRITICAL: Write stack canary (spawn_process creates Thread manually, bypassing Thread::new)
    unsafe {
        const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let bottom = kernel_stack_top - (KERNEL_STACK_PAGES * PAGE_SIZE) as u64;
        let canary_addr = bottom as *mut u64;
        core::ptr::write_volatile(canary_addr, STACK_CANARY);

        // Verify write
        let readback = core::ptr::read_volatile(canary_addr);

        if readback != STACK_CANARY {
            log_error!(
                "spawn",
                "tid={} name={} Canary read-back mismatch! Got {:#X} expected {:#X}",
                pid, static_name, readback, STACK_CANARY
            );
        }
    }

    // Create the thread with its own address space
    let thread = Thread {
        id: pid,
        process_id: Some(process_id),
        state: ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: new_pml4_phys as u64,
        priority: ThreadPriority::Normal,
        name: static_name,
        capability_table: cap::create_capability_table(pid),
        is_userspace: true,
        user_stack: Some(stack_layout),
    };

    // The thread must exist in THREAD_LIST before capabilities can be inserted
    // into its capability table. mark_thread_ready is deferred until all grants
    // are complete so the thread cannot be scheduled before it has its caps.
    crate::thread::add_thread(thread);

    // Register the very first spawned process as privileged (init).
    // PRIVILEGED_PID is guarded by compare_exchange so subsequent spawns are no-ops.
    policy::register_privileged_process(pid);

    // Grant capabilities
    //
    // Over-provisioning note (P1-A TODO):
    //   Framebuffer, InputDevice, and IoPort caps are currently granted to ALL
    //   processes so that the existing user-space stack continues to work while
    //   the dispatcher policy gates (P2-A) are put in place.  Once the system is
    //   stable with gates enforced, tighten the grants below to:
    //     Framebuffer  → "ui_shell", "display" only
    //     Keyboard     → "keyboard" only
    //     Mouse        → "mouse" only
    //     IoPort PS/2  → "keyboard", "mouse" only
    //
    // FsNamespace cap is already scoped: only "fsd" receives it.  All other
    // processes must go through fsd via IPC to access the filesystem.

    // FsNamespace capability — granted only to the filesystem daemon (fsd).
    // This cap is the gate for SYS_KERN_FS_{READ_FILE,LIST_DIR,STAT_PATH}
    // which fsd uses to communicate directly with the kernel FAT32 driver.
    if name == "fsd" {
        let fs_resource = ResourceType::FsNamespace { namespace_id: 0 };
        let fs_perms = CapPermissions::READ.union(CapPermissions::WRITE).union(CapPermissions::GRANT);
        if let Ok(cap) = cap::create_root_capability(fs_resource, pid, fs_perms) {
            let _ = crate::thread::add_thread_capability(pid, cap);
            log_info!("spawn", "Granted FsNamespace cap to fsd (pid={})", pid);
        }
    }

    // PCI Device capabilities
    if name == "nic_driver" {
        // Find VirtIO-net device
        if let Some(dev) = crate::drivers::pci::find_device(0x1AF4, 0x1000) {
            let res = ResourceType::Device { bdf: dev.bdf() };
            if let Ok(cap) = cap::create_root_capability(res, pid, CapPermissions::ALL) {
                let _ = crate::thread::add_thread_capability(pid, cap);
                log_info!("spawn", "Granted DeviceCap(VirtIO-net) to nic_driver");
            }
        }
    }

    // Framebuffer capability
    if let Some((address, width, height, stride, bpp)) = crate::graphics::get_framebuffer_info() {
        let fb_resource = ResourceType::Framebuffer {
            address: address as u64,
            width,
            height,
            stride,
            bytes_per_pixel: bpp as u8,
        };
        let fb_perms = CapPermissions::READ.union(CapPermissions::WRITE);
        if let Ok(cap) = cap::create_root_capability(fb_resource, pid, fb_perms) {
            let _ = crate::thread::add_thread_capability(pid, cap);
        }
    }

    // Keyboard capability
    let kbd_resource = ResourceType::InputDevice {
        device_type: InputDeviceType::Keyboard,
    };
    if let Ok(cap) = cap::create_root_capability(kbd_resource, pid, CapPermissions::READ) {
        let _ = crate::thread::add_thread_capability(pid, cap);
    }

    // Mouse capability
    let mouse_resource = ResourceType::InputDevice {
        device_type: InputDeviceType::Mouse,
    };
    if let Ok(cap) = cap::create_root_capability(mouse_resource, pid, CapPermissions::READ) {
        let _ = crate::thread::add_thread_capability(pid, cap);
    }

    // I/O port capabilities for PS/2 hardware access
    // TODO: restrict IoPort caps to PS/2 driver only (least privilege)
    let io_perms = CapPermissions::READ.union(CapPermissions::WRITE);
    for &port in &[0x60u16, 0x64u16] {
        let io_resource = ResourceType::IoPort { port };
        if let Ok(cap) = cap::create_root_capability(io_resource, pid, io_perms) {
            let _ = crate::thread::add_thread_capability(pid, cap);
        }
    }

    // Thread is fully initialized — mark it schedulable
    crate::sched::mark_thread_ready(pid);

    log_info!("spawn", "Process '{}' (pid={}) scheduled with VMA-backed memory", name, pid);

    Ok(pid)
}

// ---------------------------------------------------------------------------
// SYS_SPAWN_FROM_PATH — spawn an ATXF executable located at an arbitrary
// filesystem path.
//
// Design rationale:
//   SYS_SPAWN_PROCESS can only look up drivers by a short name and searches a
//   fixed directory (/drivers/<name>.atxf).  Applications that are placed in
//   user-accessible directories (e.g. /apps/hello.atxf, /home/downloads/…)
//   would never be found that way.  SYS_SPAWN_FROM_PATH accepts a full path
//   and is the primitive used by the app_launcher service to execute any ATXF
//   binary that is present on the filesystem at runtime.
//
// Security model:
//   This syscall does NOT perform caller-identity checks itself.  Privilege
//   restriction is enforced at the IPC layer: only app_launcher (a trusted
//   system service) is expected to call this syscall.  In a future hardening
//   pass a capability check (ResourceType::Spawn or similar) can be added
//   here to prevent rogue processes from abusing it.
//
// Error returns:
//   EINVAL   — path is NULL, empty, too long, or not valid UTF-8
//   ENOTSUP  — path does not end with ".atxf" (wrong file type)
//   ENOENT   — file not found on the filesystem
//   ENOMEM   — not enough physical memory to map the new process
//   EIO      — FAT32 driver not available
//
// ---------------------------------------------------------------------------
fn sys_spawn_from_path(path_ptr: u64, path_len: usize) -> u64 {
    const LOG_ORIGIN: &str = "syscall:spawn_from_path";
    const MAX_PATH: usize = 256;

    // ── 1. Validate raw pointer and length ─────────────────────────────────
    if path_len == 0 || path_len > MAX_PATH {
        log_warn!(
            LOG_ORIGIN,
            "spawn_from_path: invalid arguments (ptr={:#X}, len={})",
            path_ptr,
            path_len
        );
        return EINVAL;
    }
    let path_ptr = match validate_user_ptr::<u8>(path_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let path_range = match validate_user_byte_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };

    // ── 3. Copy path bytes from userspace into a kernel-owned buffer ───────
    //
    // Reading directly through the raw pointer is safe only after the range
    // check above has confirmed the address lies in the canonical user half.
    // We copy into a fixed-size stack buffer so that later code never holds
    // a reference into user memory (which could be concurrently modified).
    let mut path_buf = [0u8; MAX_PATH];
    unsafe {
        core::ptr::copy_nonoverlapping(
            path_range.base().as_ptr::<u8>(),
            path_buf.as_mut_ptr(),
            path_range.len(),
        );
    }
    let path_bytes = &path_buf[..path_range.len()];

    let path = match core::str::from_utf8(path_bytes) {
        Ok(s) => s.trim_end_matches('\0'),
        Err(_) => {
            log_warn!(LOG_ORIGIN, "spawn_from_path: path is not valid UTF-8");
            return EINVAL;
        }
    };

    if path.is_empty() {
        log_warn!(LOG_ORIGIN, "spawn_from_path: empty path after trimming");
        return EINVAL;
    }

    // ── 3. Enforce .atxf extension ─────────────────────────────────────────
    if !path.ends_with(".atxf") {
        log_warn!(
            LOG_ORIGIN,
            "spawn_from_path: '{}' does not have .atxf extension",
            path
        );
        return ENOTSUP;
    }

    log_info!(LOG_ORIGIN, "spawn_from_path: loading '{}'", path);

    // ── 4. Read file from FAT32 ────────────────────────────────────────────
    if !crate::drivers::fat32::is_available() {
        log_error!(LOG_ORIGIN, "spawn_from_path: FAT32 driver not available");
        return EIO;
    }

    let image_data = match crate::drivers::fat32::open(path) {
        Some(data) => data,
        None => {
            log_warn!(LOG_ORIGIN, "spawn_from_path: file not found: '{}'", path);
            return ENOENT;
        }
    };

    log_info!(
        LOG_ORIGIN,
        "spawn_from_path: read {} bytes from '{}'",
        image_data.len(),
        path
    );

    // ── 5. Parse the ATXF image ────────────────────────────────────────────
    let sections = match crate::executable::parse_image(&image_data) {
        Ok(s) => s,
        Err(crate::executable::ExecError::InvalidMagic) => {
            log_warn!(LOG_ORIGIN, "spawn_from_path: '{}' is not a valid ATXF file (bad magic)", path);
            return EINVAL;
        }
        Err(crate::executable::ExecError::UnsupportedVersion(v)) => {
            log_warn!(
                LOG_ORIGIN,
                "spawn_from_path: '{}' uses unsupported ATXF version {}",
                path,
                v
            );
            return EINVAL;
        }
        Err(e) => {
            log_warn!(LOG_ORIGIN, "spawn_from_path: parse error for '{}': {:?}", path, e);
            return EINVAL;
        }
    };

    log_info!(
        LOG_ORIGIN,
        "spawn_from_path: ATXF validated — text={} data={} bss={} entry=0x{:X}",
        sections.text.len(),
        sections.data.len(),
        sections.bss_size,
        sections.entry_offset
    );

    // ── 6. Derive a display name from the filename ─────────────────────────
    // Take the last path component without the ".atxf" suffix.
    let basename = path.rfind('/').map(|p| &path[p + 1..]).unwrap_or(path);
    let app_name = basename.strip_suffix(".atxf").unwrap_or(basename);

    // Spawn with generic name — Thread::name is &'static str so we use "app".
    // The actual path is logged above for diagnostics.
    match spawn_process_internal("app", &sections) {
        Ok(pid) => {
            log_info!(
                LOG_ORIGIN,
                "spawn_from_path: '{}' started as pid={}",
                app_name,
                pid
            );
            pid.raw()
        }
        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "spawn_from_path: failed to spawn '{}': code={}",
                app_name,
                e
            );
            e
        }
    }
}

/// Get system memory information
/// Returns total and free memory in KB via pointer
///
/// # Arguments
/// * `info_ptr` - Pointer to array of 2 u64 values [total_kb, free_kb]
///
/// # Returns
/// * ESUCCESS if successful
/// * EINVAL if pointer is invalid
fn sys_get_memory_info(info_ptr: u64) -> u64 {
    let info_ptr = match validate_user_addr(info_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(info_ptr.as_u64(), 2 * core::mem::size_of::<u64>()).is_err() {
        return EINVAL;
    }

    let (total_kb, free_kb) = crate::mm::pmm::get_memory_stats();

    let info_ptr = info_ptr.as_mut_ptr::<u64>();
    unsafe {
        *info_ptr.offset(0) = total_kb;
        *info_ptr.offset(1) = free_kb;
    }

    ESUCCESS
}

/// List all processes/threads
///
/// # Arguments
/// * `buffer` - Pointer to array of ProcessInfo structs
/// * `max_count` - Maximum number of entries to write
///
/// # Returns
/// * Number of processes written, or EINVAL if buffer is null
fn sys_list_processes(buffer: u64, max_count: usize) -> u64 {
    if max_count == 0 {
        return EINVAL;
    }
    let buffer = match validate_user_addr(buffer) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };

    // Create a temporary buffer on stack
    let mut temp_buffer = [crate::thread::ProcessInfo {
        pid: 0,
        state: 0,
        name: [0u8; 32],
    }; 32]; // Support up to 32 processes

    let actual_count = max_count.min(32);

    // Validate the full user-space write range with checked arithmetic
    let write_bytes = match actual_count.checked_mul(core::mem::size_of::<crate::thread::ProcessInfo>()) {
        Some(v) => v,
        None => return EINVAL,
    };
    if validate_user_range(buffer.as_u64(), write_bytes).is_err() {
        return EINVAL;
    }

    let count = crate::thread::list_processes(&mut temp_buffer[..actual_count]);

    // Copy to userspace buffer
    let buffer = buffer.as_mut_ptr::<crate::thread::ProcessInfo>();
    unsafe {
        for (i, item) in temp_buffer.iter().enumerate().take(count) {
            *buffer.add(i) = *item;
        }
    }

    count as u64
}

/// Get total number of processes/threads
fn sys_get_process_count() -> u64 {
    crate::thread::process_count() as u64
}

/// Read kernel log buffer
///
/// # Arguments
/// * `buffer` - Pointer to buffer to write log entries
/// * `max_len` - Maximum bytes to write
///
/// # Returns
/// * Number of bytes written, or EINVAL if buffer is null
fn sys_read_klog(buffer: u64, max_len: usize) -> u64 {
    if max_len == 0 {
        return EINVAL;
    }
    let buffer = match validate_user_addr(buffer) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(buffer.as_u64(), max_len).is_err() {
        return EINVAL;
    }

    let log_data = crate::log::read_log_buffer();
    let copy_len = log_data.len().min(max_len);

    unsafe {
        core::ptr::copy_nonoverlapping(log_data.as_ptr(), buffer.as_mut_ptr::<u8>(), copy_len);
    }

    copy_len as u64
}

/// Get CPU brand string
///
/// # Arguments
/// * `buffer` - Pointer to buffer to write brand string
/// * `max_len` - Maximum bytes to write
///
/// # Returns
/// * Number of bytes written, or EINVAL if buffer is null
fn sys_get_cpu_brand(buffer: u64, max_len: usize) -> u64 {
    if max_len == 0 {
        return EINVAL;
    }
    let buffer = match validate_user_addr(buffer) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(buffer.as_u64(), max_len).is_err() {
        return EINVAL;
    }

    let brand = crate::system::info().cpu_brand();
    let copy_len = brand.len().min(max_len);

    unsafe {
        core::ptr::copy_nonoverlapping(brand.as_ptr(), buffer.as_mut_ptr::<u8>(), copy_len);
    }

    copy_len as u64
}

// ---------------------------------------------------------------------------
// Virtual Memory Management Syscalls (mmap/munmap/mprotect/brk)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessContextError {
    NoCurrentThread,
    NoCurrentProcess,
    MissingAddressSpace,
    Pml4Drift {
        thread_pml4: crate::mm::vma::VirtAddr,
        process_pml4: crate::mm::vma::VirtAddr,
        tid: crate::thread::ThreadId,
        pid: crate::process::ProcessId,
    },
}

#[derive(Debug, Clone, Copy)]
struct ProcessVmaContext {
    tid: crate::thread::ThreadId,
    pid: crate::process::ProcessId,
    pml4: usize,
}

fn process_context_error_kind(error: ProcessContextError) -> KernelOperationalError {
    match error {
        ProcessContextError::NoCurrentThread | ProcessContextError::NoCurrentProcess => {
            KernelOperationalError::ContextCorruption
        }
        ProcessContextError::MissingAddressSpace | ProcessContextError::Pml4Drift { .. } => {
            KernelOperationalError::AddressSpaceCorruption
        }
    }
}

fn contain_mm_operational_error(error: ProcessContextError) -> u64 {
    let (tid, pid) = match error {
        ProcessContextError::Pml4Drift { tid, pid, .. } => (Some(tid), Some(pid)),
        _ => (crate::sched::current_thread(), best_effort_current_process()),
    };

    log_error!(
        "syscall",
        "[MM_CONTEXT_CORRUPTION] error={:?} tid={:?} pid={:?}",
        error,
        tid,
        pid
    );

    contain_operational_violation(
        "mm_syscall_context",
        process_context_error_kind(error),
        pid,
        tid,
    );
    ESUCCESS
}

fn current_process_vma_context() -> Result<ProcessVmaContext, ProcessContextError> {
    let tid = crate::sched::current_thread().ok_or(ProcessContextError::NoCurrentThread)?;
    let pid = crate::thread::get_thread_process_id(tid).ok_or(ProcessContextError::NoCurrentProcess)?;
    let process_pml4 = crate::process::get_process_pml4(pid).ok_or(ProcessContextError::MissingAddressSpace)?;
    let thread_pml4 = crate::thread::get_thread_address_space(tid).ok_or(ProcessContextError::MissingAddressSpace)?;

    if process_pml4 == 0 || thread_pml4 == 0 {
        return Err(ProcessContextError::MissingAddressSpace);
    }

    if thread_pml4 != process_pml4 {
        return Err(ProcessContextError::Pml4Drift {
            thread_pml4: crate::mm::vma::VirtAddr::new(thread_pml4 as usize),
            process_pml4: crate::mm::vma::VirtAddr::new(process_pml4 as usize),
            tid,
            pid,
        });
    }

    Ok(ProcessVmaContext {
        tid,
        pid,
        pml4: process_pml4 as usize,
    })
}

/// mmap(addr_hint, length, prot, flags) -> mapped_addr | errno
///
/// Maps anonymous private memory into the calling process's virtual address space.
/// If addr_hint is 0, the kernel chooses a suitable address.
/// If MAP_FIXED is set, the mapping is placed exactly at addr_hint.
///
/// **Supported flags:** `MAP_ANONYMOUS | MAP_PRIVATE` (optionally `| MAP_FIXED`).
/// Any other combination (e.g. `MAP_SHARED`, missing `MAP_ANONYMOUS`,
/// or unknown flag bits) is rejected with `EINVAL`.
fn sys_mmap(
    addr_hint: atom_abi::UserVirtAddr,
    length: u64,
    prot: u64,
    flags: u64,
) -> atom_abi::UserVirtAddr {
    use crate::mm::vma::{self, Vma, VmaBacking, VmaPermissions};
    use crate::mm::pmm::PAGE_SIZE;

    let length = length as usize;
    if length == 0 {
        return EINVAL;
    }

    // -----------------------------------------------------------------------
    // Flag validation — reject unsupported/dangerous combinations early so
    // user code gets a clear EINVAL rather than undefined behaviour later.
    //
    // Supported:  MAP_ANONYMOUS | MAP_PRIVATE  (optionally | MAP_FIXED)
    // Unsupported: MAP_SHARED, file-backed mappings (no MAP_ANONYMOUS), or
    //              any unknown flag bits.
    // -----------------------------------------------------------------------
    const KNOWN_FLAGS: u64 = atom_abi::MAP_ANONYMOUS | atom_abi::MAP_PRIVATE | atom_abi::MAP_FIXED;

    // MAP_ANONYMOUS must be set — we have no fd/file backing.
    if flags & atom_abi::MAP_ANONYMOUS == 0 {
        return EINVAL;
    }
    // MAP_PRIVATE must be set — MAP_SHARED is not supported.
    if flags & atom_abi::MAP_PRIVATE == 0 {
        return EINVAL;
    }
    // Reject any flag bits we don't know about.
    if flags & !KNOWN_FLAGS != 0 {
        return EINVAL;
    }

    // Round up to page boundary
    let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let vma_ctx = match current_process_vma_context() {
        Ok(ctx) => ctx,
        Err(err) => return contain_mm_operational_error(err),
    };
    let tid = vma_ctx.tid;
    let process_id = vma_ctx.pid;
    let pml4 = vma_ctx.pml4;
    if let Err(e) = policy::enforce_vma_memory_operation_allowed(process_id, tid) {
        return e;
    }

    // Build VMA permissions from prot flags
    let mut perms = VmaPermissions::NONE;
    if prot & atom_abi::PROT_READ != 0 {
        perms = perms.union(VmaPermissions::READ);
    }
    if prot & atom_abi::PROT_WRITE != 0 {
        perms = perms.union(VmaPermissions::WRITE);
    }
    if prot & atom_abi::PROT_EXEC != 0 {
        perms = perms.union(VmaPermissions::EXEC);
    }

    let fixed = flags & atom_abi::MAP_FIXED != 0;

    let fixed_range = if fixed {
        let addr = addr_hint as usize;
        let user_range = match atom_abi::validate_user_mmap_range(addr, length) {
            Ok(range) => range,
            Err(_) => {
                log_error!(
                    "syscall",
                    "[MMAP_ERROR] reason=invalid_abi_mmap_range addr={:#X} size={} hint={:#X} flags={:#X}",
                    addr_hint,
                    length,
                    addr_hint,
                    flags
                );
                return EINVAL;
            }
        };
        Some((user_range.start(), user_range.end_exclusive()))
    } else {
        None
    };

    let hint_start = if addr_hint != 0 {
        match atom_abi::validate_user_mmap_hint(addr_hint as usize) {
            Ok(hint) => hint.as_usize(),
            _ => atom_abi::USER_MMAP_START as usize,
        }
    } else {
        atom_abi::USER_MMAP_START as usize
    };

    let mapped_addr = match vma::with_address_space_ops_lock(pml4, || -> Result<atom_abi::UserVirtAddr, u64> {
        let (virt_addr, virt_end, alloc_mode) = if let Some((addr, end)) = fixed_range {
            // MAP_FIXED: replace anything overlapping this range and then insert.
            let removed = match vma::remove_process_vma_range(process_id, addr, end) {
                Ok(removed) => removed,
                Err(_) => {
                    log_error!(
                        "syscall",
                        "[MMAP_ERROR] reason=remove_range_failed addr={:#X} size={} hint={:#X} flags={:#X}",
                        addr_hint,
                        length,
                        addr_hint,
                        flags
                    );
                    return Err(EINVAL);
                }
            };
            for old_vma in &removed {
                unmap_vma_pages(process_id, pml4, old_vma);
            }
            (addr, end, "fixed")
        } else {
            let addr = match vma::find_process_free_region(
                process_id,
                hint_start,
                atom_abi::USER_MMAP_END as usize,
                length,
            ) {
                Ok(Some(addr)) => addr,
                Ok(None) => {
                    log_error!(
                        "syscall",
                        "[MMAP_ERROR] reason=no_free_region addr={:#X} size={} hint={:#X} flags={:#X}",
                        hint_start,
                        length,
                        addr_hint,
                        flags
                    );
                    return Err(ENOMEM);
                }
                Err(_) => {
                    log_error!(
                        "syscall",
                        "[MMAP_ERROR] reason=find_free_region_failed addr={:#X} size={} hint={:#X} flags={:#X}",
                        hint_start,
                        length,
                        addr_hint,
                        flags
                    );
                    return Err(EPERM);
                }
            };
            let end = match addr.checked_add(length) {
                Some(end) => end,
                None => {
                    log_error!(
                        "syscall",
                        "[MMAP_ERROR] reason=overflow addr={:#X} size={} hint={:#X} flags={:#X}",
                        addr,
                        length,
                        addr_hint,
                        flags
                    );
                    return Err(EINVAL);
                }
            };
            (addr, end, "first_fit")
        };

        if virt_end > atom_abi::USER_MMAP_END as usize {
            log_error!(
                "syscall",
                "[MMAP_CONTEXT_CORRUPTION] pid={} tid={} reason=allocation_window_escape start=0x{:X} end=0x{:X} limit=0x{:X}",
                process_id,
                tid,
                virt_addr,
                virt_end,
                atom_abi::USER_MMAP_END
            );
            contain_operational_violation(
                "sys_mmap_window",
                KernelOperationalError::AddressSpaceCorruption,
                Some(process_id),
                Some(tid),
            );
            return Err(ESUCCESS);
        }

        log_info!(
            "syscall",
            "[MMAP_ALLOC] base={:#X} size={} next={:#X} mode={}",
            virt_addr,
            length,
            virt_end,
            alloc_mode
        );

        let overlap = match vma::process_range_overlaps(process_id, virt_addr, virt_end) {
            Ok(found) => found,
            Err(_) => return Err(EPERM),
        };
        log_debug!(
            "syscall",
            "[MMAP_ALLOC_VALIDATE] base=0x{:X} size={} end=0x{:X} overlap={}",
            virt_addr,
            length,
            virt_end,
            overlap
        );
        if overlap {
            log_error!(
                "syscall",
                "[MMAP_ERROR] reason=allocator_overlap base=0x{:X} size={} end=0x{:X} flags=0x{:X}",
                virt_addr,
                length,
                virt_end,
                flags
            );
            return Err(ENOMEM);
        }

        let mapped_addr = virt_addr as atom_abi::UserVirtAddr;
        if validate_mmap_return_range(mapped_addr, length).is_err() {
            log_error!(
                "syscall",
                "[MMAP_ERROR] reason=invalid_return_addr addr={:#X} size={} hint={:#X} flags={:#X}",
                mapped_addr,
                length,
                addr_hint,
                flags
            );
            log_error!(
                "syscall",
                "[ABI VIOLATION] [MMAP] pid={} invalid mmap VA selected: addr={:#X} size={} hint={:#X}",
                process_id,
                mapped_addr,
                length,
                addr_hint
            );
            return Err(EINVAL);
        }

        let new_vma = Vma {
            start: virt_addr,
            end: virt_end,
            perms,
            backing: VmaBacking::Anonymous,
            label: "mmap",
        };

        log_debug!(
            "syscall",
            "[VMA_INSERT_ATTEMPT] pid={} pml4=0x{:X} start=0x{:X} end=0x{:X} prot=0x{:X} flags=0x{:X} backing=Anonymous label=mmap",
            process_id,
            pml4,
            new_vma.start,
            new_vma.end,
            prot,
            flags
        );

        if let Err(err) = vma::insert_process_vma(process_id, new_vma) {
            log_error!(
                "syscall",
                "[MMAP_BUG] insert failed after region selection: pid={} pml4=0x{:X} base=0x{:X} end=0x{:X} size={} err={:?}",
                process_id,
                pml4,
                virt_addr,
                virt_end,
                length,
                err
            );
            log_error!(
                "syscall",
                "[VMA_INSERT] result=err pid={} pml4=0x{:X} start=0x{:X} end=0x{:X} err={:?}",
                process_id,
                pml4,
                virt_addr,
                virt_end,
                err
            );
            return Err(ENOMEM);
        }

        log_debug!(
            "syscall",
            "[VMA_INSERT] result=ok pid={} pml4=0x{:X} start=0x{:X} end=0x{:X}",
            process_id,
            pml4,
            virt_addr,
            virt_end
        );

        Ok(mapped_addr)
    }) {
        Ok(mapped_addr) => mapped_addr,
        Err(errno) => return errno,
    };

    log_debug!(
        "syscall",
        "[ABI] return addr={:#x} pid={} syscall=mmap",
        mapped_addr,
        process_id
    );
    log_info!(
        "syscall",
        "[MMAP] pid={} addr={:#X} size={} hint={:#X} flags={:#X}",
        process_id,
        mapped_addr,
        length,
        addr_hint,
        flags
    );
    mapped_addr
}

/// munmap(addr, length) -> 0 | errno
///
/// Unmaps a previously mapped region, freeing both the VMA and any
/// physical pages that were demand-paged into it.
fn sys_munmap(addr: u64, length: u64) -> u64 {
    use crate::mm::vma;
    use crate::mm::pmm::PAGE_SIZE;

    let addr = addr as usize;
    let length = length as usize;

    if length == 0 {
        return EINVAL;
    }

    let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let user_range = match atom_abi::validate_user_range(addr, length) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };
    let end = user_range.end_exclusive();
    if crate::mm::validate_page_range(addr, end).is_err() {
        return EINVAL;
    }

    let vma_ctx = match current_process_vma_context() {
        Ok(ctx) => ctx,
        Err(err) => return contain_mm_operational_error(err),
    };
    let process_id = vma_ctx.pid;
    let pml4 = vma_ctx.pml4;
    if let Err(e) = policy::enforce_vma_memory_operation_allowed(process_id, vma_ctx.tid) {
        return e;
    }

    match vma::with_address_space_ops_lock(pml4, || -> Result<(), u64> {
        let removed = vma::remove_process_vma_range(process_id, addr, end).map_err(|_| EINVAL)?;
        if removed.is_empty() {
            return Err(EINVAL);
        }

        // Unmap physical pages
        for old_vma in &removed {
            unmap_vma_pages(process_id, pml4, old_vma);
        }

        Ok(())
    }) {
        Ok(()) => ESUCCESS,
        Err(code) => code,
    }
}

/// mprotect(addr, length, prot) -> 0 | errno
///
/// Changes the protection on a virtual memory region.
fn sys_mprotect(addr: u64, length: u64, prot: u64) -> u64 {
    use crate::mm::vma::{self, VmaPermissions};
    use crate::mm::vm::{self, PageFlags};
    use crate::mm::pmm::PAGE_SIZE;

    let addr = addr as usize;
    let length = length as usize;

    if length == 0 {
        return EINVAL;
    }

    let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let user_range = match atom_abi::validate_user_range(addr, length) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };
    let end = user_range.end_exclusive();
    if crate::mm::validate_page_range(addr, end).is_err() {
        return EINVAL;
    }

    let vma_ctx = match current_process_vma_context() {
        Ok(ctx) => ctx,
        Err(err) => return contain_mm_operational_error(err),
    };
    let process_id = vma_ctx.pid;
    let pml4 = vma_ctx.pml4;
    if let Err(e) = policy::enforce_vma_memory_operation_allowed(process_id, vma_ctx.tid) {
        return e;
    }

    let mut perms = VmaPermissions::NONE;
    if prot & atom_abi::PROT_READ != 0 {
        perms = perms.union(VmaPermissions::READ);
    }
    if prot & atom_abi::PROT_WRITE != 0 {
        perms = perms.union(VmaPermissions::WRITE);
    }
    if prot & atom_abi::PROT_EXEC != 0 {
        perms = perms.union(VmaPermissions::EXEC);
    }

    match vma::with_address_space_ops_lock(pml4, || -> Result<(), u64> {
        vma::set_process_permissions(process_id, addr, end, perms).map_err(|_| EINVAL)?;

        // Update PTEs for all already-resident pages in the affected range.
        let mut page_flags = PageFlags::PRESENT | PageFlags::USER;
        if perms.contains(VmaPermissions::WRITE) {
            page_flags |= PageFlags::WRITABLE;
        }
        if !perms.contains(VmaPermissions::EXEC) {
            page_flags |= PageFlags::NO_EXECUTE;
        }

        let mut page_addr = addr;
        while page_addr < addr + length {
            if let Ok((phys, _old_flags)) = vm::query_mapping_in_pml4(pml4, page_addr) {
                let _ = vm::remap_page_in_pml4(pml4, page_addr, phys, page_flags);
            }
            page_addr += PAGE_SIZE;
        }

        Ok(())
    }) {
        Ok(()) => ESUCCESS,
        Err(code) => code,
    }
}

/// brk(new_brk) -> current_brk | errno
///
/// Adjusts the program break (heap end). If new_brk is 0, returns the
/// current break. Otherwise, extends or shrinks the heap.
///
/// The heap VMA is identified by the "heap" label.
fn sys_brk(new_brk: u64) -> u64 {
    use crate::mm::vma::{self, Vma, VmaBacking, VmaPermissions};
    use crate::mm::pmm::PAGE_SIZE;

    let vma_ctx = match current_process_vma_context() {
        Ok(ctx) => ctx,
        Err(err) => return contain_mm_operational_error(err),
    };
    let process_id = vma_ctx.pid;
    let pml4 = vma_ctx.pml4;
    if let Err(e) = policy::enforce_vma_memory_operation_allowed(process_id, vma_ctx.tid) {
        return e;
    }

    let heap_start = atom_abi::USER_HEAP_START as usize;

    // Find existing heap VMA
    let current_brk = match vma::find_process_vma(process_id, heap_start) {
        Some(vma) if vma.label == "heap" => vma.end,
        _ => {
            // No heap VMA exists yet. If new_brk == 0, return the default start.
            if new_brk == 0 {
                return heap_start as u64;
            }

            // Create initial heap VMA
            let new_brk_aligned = ((new_brk as usize) + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            if new_brk_aligned <= heap_start {
                return heap_start as u64;
            }

            let heap_vma = Vma {
                start: heap_start,
                end: new_brk_aligned,
                perms: VmaPermissions::read_write(),
                backing: VmaBacking::Anonymous,
                label: "heap",
            };

            match vma::with_address_space_ops_lock(pml4, || {
                vma::insert_process_vma(process_id, heap_vma)
            }) {
                Ok(()) => return new_brk_aligned as u64,
                Err(_) => return ENOMEM,
            }
        }
    };

    if new_brk == 0 {
        return current_brk as u64;
    }

    let new_brk_aligned = ((new_brk as usize) + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    if new_brk_aligned <= heap_start {
        return current_brk as u64;
    }

    if new_brk_aligned == current_brk {
        return current_brk as u64;
    }

    let new_heap = Vma {
        start: heap_start,
        end: new_brk_aligned,
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "heap",
    };

    match vma::with_address_space_ops_lock(pml4, || -> Result<(), ()> {
        // Remove old heap VMA and replace with resized one
        vma::remove_process_vma(process_id, heap_start).map_err(|_| ())?;

        if new_brk_aligned < current_brk {
            // Shrinking: unmap pages in the released range
            let shrunk_start = new_brk_aligned;
            let shrunk_end = current_brk;
            for page in (shrunk_start..shrunk_end).step_by(PAGE_SIZE) {
                if let Ok((phys, _)) = crate::mm::vm::query_mapping_in_pml4(pml4, page) {
                    if crate::mm::vm::unmap_page_in_pml4(pml4, page).is_ok() {
                        if let Some(account) = vma::take_materialized_page(pml4, page) {
                            let _ = vma::free_page_account(account);
                        } else {
                            let _ = crate::mm::pmm::free_page(phys);
                            let _ = vma::account_process_unmap(process_id);
                        }
                    }
                }
            }
        }

        vma::insert_process_vma(process_id, new_heap).map_err(|_| ())?;
        Ok(())
    }) {
        Ok(()) => new_brk_aligned as u64,
        Err(_) => ENOMEM,
    }
}

/// Helper: unmap all physical pages for a VMA by walking the page table
fn unmap_vma_pages(
    process_id: crate::process::ProcessId,
    pml4: usize,
    vma: &crate::mm::vma::Vma,
) {
    use crate::mm::pmm::PAGE_SIZE;

    for page in (vma.start..vma.end).step_by(PAGE_SIZE) {
        // Query if the page is mapped
        if let Ok((phys, _)) = crate::mm::vm::query_mapping_in_pml4(pml4, page) {
            if crate::mm::vm::unmap_page_in_pml4(pml4, page).is_ok() {
                if let Some(account) = crate::mm::vma::take_materialized_page(pml4, page) {
                    let _ = crate::mm::vma::free_page_account(account);
                } else {
                    // Fallback for legacy/non-accounted pages.
                    match vma.backing {
                        crate::mm::vma::VmaBacking::Device { .. } => {}
                        _ => {
                            let _ = crate::mm::pmm::free_page(phys);
                        }
                    }
                    let _ = crate::mm::vma::account_process_unmap(process_id);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem syscalls — Production-ready IPC forwarding to fsd
// ---------------------------------------------------------------------------

use alloc::vec::Vec;

#[inline]
fn map_user_address_error(err: atom_abi::UserAddressError) -> u64 {
    match err {
        atom_abi::UserAddressError::NonCanonical
        | atom_abi::UserAddressError::BelowUserMin
        | atom_abi::UserAddressError::AboveUserMax
        | atom_abi::UserAddressError::Overflow
        | atom_abi::UserAddressError::Unaligned
        | atom_abi::UserAddressError::EmptyRange => EINVAL,
    }
}

#[inline]
fn validate_user_addr(addr: u64) -> Result<atom_abi::UserVAddr, u64> {
    // Syscall boundary: raw userspace values are converted to ABI-typed values here.
    atom_abi::validate_user_addr(addr as usize).map_err(map_user_address_error)
}

#[inline]
fn validate_user_ptr<T>(addr: u64) -> Result<atom_abi::UserPtr<T>, u64> {
    atom_abi::validate_user_ptr::<T>(addr as usize).map_err(map_user_address_error)
}

#[inline]
fn validate_user_range(ptr: u64, size: usize) -> Result<atom_abi::UserRange, u64> {
    // Syscall boundary: raw userspace ranges are converted to ABI-typed values here.
    atom_abi::validate_user_range(ptr as usize, size).map_err(map_user_address_error)
}

#[inline]
fn validate_user_byte_range(ptr: atom_abi::UserPtr<u8>, len: usize) -> Result<atom_abi::UserRange, u64> {
    ptr.byte_range(len).map_err(map_user_address_error)
}

#[inline]
fn validate_user_return_addr(addr: atom_abi::UserVirtAddr) -> Result<atom_abi::UserVAddr, atom_abi::UserAddressError> {
    atom_abi::validate_user_return_addr(addr as usize)
}

#[inline]
fn validate_mmap_return_range(
    addr: atom_abi::UserVirtAddr,
    size: usize,
) -> Result<atom_abi::UserRange, atom_abi::UserAddressError> {
    atom_abi::validate_user_mmap_range(addr as usize, size)
}

// Helper: safely copy string from userspace
fn copy_string_from_user(src_range: atom_abi::UserRange) -> Result<alloc::string::String, u64> {
    let len = src_range.len();
    if len == 0 || len > FS_MAX_PATH_LEN {
        return Err(ENAMETOOLONG);
    }

    // SAFETY: src_range is ABI-validated and guaranteed canonical.
    match core::str::from_utf8(unsafe { core::slice::from_raw_parts(src_range.base().as_ptr::<u8>(), len) }) {
        Ok(s) => Ok(alloc::string::String::from(s)),
        Err(_) => Err(EINVAL),
    }
}

// Helper: safely copy buffer from userspace
fn copy_buffer_from_user(src_range: atom_abi::UserRange, max_len: usize) -> Result<Vec<u8>, u64> {
    let len = src_range.len();
    if len == 0 || len > max_len {
        return Err(EINVAL);
    }

    // SAFETY: src_range is ABI-validated and guaranteed canonical.
    Ok(unsafe { core::slice::from_raw_parts(src_range.base().as_ptr::<u8>(), len).to_vec() })
}

// Helper: safely write buffer to userspace
fn write_buffer_to_user(dst_range: atom_abi::UserRange, src: &[u8]) -> Result<(), u64> {
    if src.is_empty() {
        return Ok(());
    }
    if dst_range.len() != src.len() {
        return Err(EINVAL);
    }

    // Sanity cap: reject absurdly large copies (> 64 MB) that are almost
    // certainly the result of a length calculation bug somewhere upstream.
    const MAX_SINGLE_COPY: usize = 64 * 1024 * 1024;
    if src.len() > MAX_SINGLE_COPY {
        log_error!("write_buf",
            "write_buffer_to_user: rejecting copy of {} bytes (max {})",
            src.len(), MAX_SINGLE_COPY);
        return Err(EINVAL);
    }

    // SAFETY: dst_range is ABI-validated and guaranteed canonical.
    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst_range.base().as_mut_ptr::<u8>(), src.len())
    }

    Ok(())
}

// ============================================================================
// Kernel FS backend syscalls — provide fsd with access to the kernel's
// FAT32 driver.  Only fsd should call these.
// ============================================================================

/// Read a file from the kernel FAT32 driver.
/// Returns the number of bytes read into the user buffer.
fn sys_kern_fs_read_file(path_ptr: u64, path_len: usize, buf_ptr: u64, buf_len: usize) -> u64 {
    const LOG_ORIGIN: &str = "kern_fs";

    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if buf_len == 0 {
        return EINVAL;
    }
    let buf_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let _buf_range = match validate_user_byte_range(buf_ptr, buf_len) {
        Ok(range) => range,
        Err(e) => return e,
    };

    log_debug!(LOG_ORIGIN, "kern_fs_read_file(path=\"{}\")", path);

    match crate::drivers::fat32::open(path) {
        Some(data) => {
            let to_copy = core::cmp::min(data.len(), buf_len);
            let dst_range = match validate_user_byte_range(buf_ptr, to_copy) {
                Ok(range) => range,
                Err(e) => return e,
            };
            if let Err(e) = write_buffer_to_user(dst_range, &data[..to_copy]) {
                return e;
            }
            to_copy as u64
        }
        None => ENOENT,
    }
}

/// List a directory from the kernel FAT32 driver.
/// Writes packed directory entry records into the user buffer.
///
/// Each record: [ino(4) | rec_len(2) | name_len(1) | file_type(1) | name...]
/// (4-byte aligned)
///
/// Returns total bytes written, or an error code.
fn sys_kern_fs_list_dir(path_ptr: u64, path_len: usize, buf_ptr: u64, buf_len: usize) -> u64 {
    const LOG_ORIGIN: &str = "kern_fs";

    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if buf_len == 0 {
        return EINVAL;
    }
    let buf_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let _buf_range = match validate_user_byte_range(buf_ptr, buf_len) {
        Ok(range) => range,
        Err(e) => return e,
    };

    log_debug!(LOG_ORIGIN, "kern_fs_list_dir(path=\"{}\")", path);

    // list_directory expects path without leading /
    let list_path = if path == "/" { "" } else { path };

    match crate::drivers::fat32::list_directory(list_path) {
        Some(entries) => {
            let mut pos = 0usize;
            for (ino_counter, entry_name) in (1_u32..).zip(entries.iter()) {
                // Parse name and type from the "name/" format
                let (name, ftype) = if entry_name.ends_with('/') {
                    (&entry_name[..entry_name.len() - 1], 2u8) // directory
                } else {
                    (entry_name.as_str(), 1u8) // regular file
                };

                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len().min(255);
                let raw_len = 8 + name_len;
                let rec_len = (raw_len + 3) & !3; // 4-byte align

                if pos + rec_len > buf_len {
                    break; // buffer full
                }

                // Build record in a stack buffer then copy to user
                let mut rec = [0u8; 272]; // 8 header + 255 name + padding
                rec[..rec_len].fill(0);
                rec[0..4].copy_from_slice(&ino_counter.to_le_bytes());
                rec[4..6].copy_from_slice(&(rec_len as u16).to_le_bytes());
                rec[6] = name_len as u8;
                rec[7] = ftype;
                rec[8..8 + name_len].copy_from_slice(&name_bytes[..name_len]);

                let dst_ptr = match buf_ptr.checked_add_bytes(pos) {
                    Ok(ptr) => ptr,
                    Err(err) => return map_user_address_error(err),
                };
                let dst_range = match validate_user_byte_range(dst_ptr, rec_len) {
                    Ok(range) => range,
                    Err(e) => return e,
                };
                if let Err(e) = write_buffer_to_user(dst_range, &rec[..rec_len]) {
                    return e;
                }

                pos += rec_len;
            }

            pos as u64
        }
        None => ENOENT,
    }
}

/// Stat a path using the kernel FAT32 driver.
/// Writes an 80-byte stat buffer to userspace.
///
/// For FAT32, we return: size, mode (S_IFDIR or S_IFREG + 0o755), and
/// inode = 1 (FAT32 has no real inode numbers).
fn sys_kern_fs_stat_path(path_ptr: u64, path_len: usize, stat_ptr: u64) -> u64 {
    const LOG_ORIGIN: &str = "kern_fs";

    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let stat_ptr = match validate_user_ptr::<u8>(stat_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let stat_range = match validate_user_byte_range(stat_ptr, 80) {
        Ok(range) => range,
        Err(e) => return e,
    };

    log_debug!(LOG_ORIGIN, "kern_fs_stat_path(path=\"{}\")", path);

    // Use stat_path to get metadata without reading file contents
    fill_stat_to_user(path, stat_range)
}

/// Internal helper: stat a path and write the 80-byte result to userspace.
/// Uses `fat32::stat_path()` so NO file data is read.
fn fill_stat_to_user(path: &str, stat_range: atom_abi::UserRange) -> u64 {
    let mut buf = [0u8; 80];
    if stat_range.len() != buf.len() {
        return EINVAL;
    }

    // Root directory special case
    if path == "/" || path.is_empty() {
        buf[8..16].copy_from_slice(&2u64.to_le_bytes()); // inode = 2
        let mode: u32 = 0o040755;
        buf[40..44].copy_from_slice(&mode.to_le_bytes());
        buf[48..52].copy_from_slice(&2u32.to_le_bytes()); // nlinks
        buf[56..60].copy_from_slice(&512u32.to_le_bytes()); // blksize
        return match write_buffer_to_user(stat_range, &buf) {
            Ok(_) => ESUCCESS,
            Err(e) => e,
        };
    }

    // stat_path reads only directory entries, never file content
    match crate::drivers::fat32::stat_path(path) {
        Some(st) => {
            buf[0..8].copy_from_slice(&st.size.to_le_bytes()); // size
            buf[8..16].copy_from_slice(&1u64.to_le_bytes()); // inode
            let mode: u32 = if st.is_dir { 0o040755 } else { 0o100644 };
            buf[40..44].copy_from_slice(&mode.to_le_bytes());
            let nlinks: u32 = if st.is_dir { 2 } else { 1 };
            buf[48..52].copy_from_slice(&nlinks.to_le_bytes());
            buf[56..60].copy_from_slice(&512u32.to_le_bytes()); // blksize
            match write_buffer_to_user(stat_range, &buf) {
                Ok(_) => ESUCCESS,
                Err(e) => e,
            }
        }
        None => ENOENT,
    }
}

fn sys_kern_fs_write_file(path_ptr: u64, path_len: usize, buf_ptr: u64, buf_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let data = if buf_len == 0 {
        Vec::new()
    } else {
        let buf_ptr = match validate_user_ptr::<u8>(buf_ptr) {
            Ok(ptr) => ptr,
            Err(e) => return e,
        };
        let buf_range = match validate_user_byte_range(buf_ptr, buf_len) {
            Ok(range) => range,
            Err(e) => return e,
        };
        match copy_buffer_from_user(buf_range, buf_len) {
            Ok(v) => v,
            Err(e) => return e,
        }
    };

    if crate::drivers::fat32::write_file(path, &data) {
        ESUCCESS
    } else {
        EIO
    }
}

fn sys_kern_fs_mkdir(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if crate::drivers::fat32::stat_path(path).is_some() {
        return EEXIST;
    }
    if crate::drivers::fat32::mkdir(path) {
        ESUCCESS
    } else {
        EIO
    }
}

fn sys_kern_fs_rmdir(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    match crate::drivers::fat32::stat_path(path) {
        Some(st) if !st.is_dir => ENOTDIR,
        Some(_) => {
            if crate::drivers::fat32::rmdir(path) { ESUCCESS } else { ENOTEMPTY }
        }
        None => ENOENT,
    }
}

fn sys_kern_fs_unlink(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    match crate::drivers::fat32::stat_path(path) {
        Some(st) if st.is_dir => EISDIR,
        Some(_) => {
            if crate::drivers::fat32::unlink(path) { ESUCCESS } else { EIO }
        }
        None => ENOENT,
    }
}

fn sys_kern_fs_rename(old_path_ptr: u64, old_path_len: usize, new_path_ptr: u64, new_path_len: usize) -> u64 {
    let old_range = match validate_user_path_range(old_path_ptr, old_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (old_buf, old_len) = match copy_path_from_user(old_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let new_range = match validate_user_path_range(new_path_ptr, new_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (new_buf, new_len) = match copy_path_from_user(new_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old_path = path_buf_as_str(&old_buf, old_len);
    let new_path = path_buf_as_str(&new_buf, new_len);

    if crate::drivers::fat32::stat_path(old_path).is_none() {
        return ENOENT;
    }
    if crate::drivers::fat32::stat_path(new_path).is_some() {
        return EEXIST;
    }
    if crate::drivers::fat32::rename(old_path, new_path) {
        ESUCCESS
    } else {
        EXDEV
    }
}

fn sys_kern_fs_sync() -> u64 {
    if crate::drivers::fat32::sync() {
        ESUCCESS
    } else {
        EIO
    }
}

fn sys_kern_block_read(lba: u64, sector_count: usize, buf_ptr: u64, buf_len: usize) -> u64 {
    const SECTOR_SIZE: usize = 512;
    const MAX_SECTORS_PER_CALL: usize = 4096;
    const MAX_AHCI_XFER: usize = 128;

    if sector_count == 0 || sector_count > MAX_SECTORS_PER_CALL {
        return EINVAL;
    }
    let byte_count = match sector_count.checked_mul(SECTOR_SIZE) {
        Some(v) => v,
        None => return EINVAL,
    };
    if buf_len < byte_count {
        return EINVAL;
    }

    let dst_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let dst_range = match validate_user_byte_range(dst_ptr, byte_count) {
        Ok(range) => range,
        Err(e) => return e,
    };

    let mut out = Vec::with_capacity(byte_count);
    let mut remaining = sector_count;
    let mut current_lba = lba;

    while remaining > 0 {
        let step = core::cmp::min(remaining, MAX_AHCI_XFER);
        let chunk = match crate::drivers::ahci::read_sectors(current_lba, step as u16) {
            Some(v) => v,
            None => return EIO,
        };
        if chunk.len() != step * SECTOR_SIZE {
            return EIO;
        }
        out.extend_from_slice(&chunk);
        remaining -= step;
        current_lba = current_lba.saturating_add(step as u64);
    }

    if out.len() != byte_count {
        return EIO;
    }
    if let Err(e) = write_buffer_to_user(dst_range, &out) {
        return e;
    }
    out.len() as u64
}

fn sys_kern_block_write(lba: u64, sector_count: usize, buf_ptr: u64, buf_len: usize) -> u64 {
    const SECTOR_SIZE: usize = 512;
    const MAX_SECTORS_PER_CALL: usize = 4096;
    const MAX_AHCI_XFER: usize = 128;

    if sector_count == 0 || sector_count > MAX_SECTORS_PER_CALL {
        return EINVAL;
    }
    let byte_count = match sector_count.checked_mul(SECTOR_SIZE) {
        Some(v) => v,
        None => return EINVAL,
    };
    if buf_len < byte_count {
        return EINVAL;
    }

    let src_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    let src_range = match validate_user_byte_range(src_ptr, byte_count) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let data = match copy_buffer_from_user(src_range, byte_count) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let mut written_sectors = 0usize;
    while written_sectors < sector_count {
        let step = core::cmp::min(sector_count - written_sectors, MAX_AHCI_XFER);
        let start = written_sectors * SECTOR_SIZE;
        let end = start + step * SECTOR_SIZE;
        if !crate::drivers::ahci::write_sectors(lba + written_sectors as u64, &data[start..end]) {
            return EIO;
        }
        written_sectors += step;
    }
    ESUCCESS
}

fn sys_kern_block_flush() -> u64 {
    if crate::drivers::ahci::flush_cache() {
        ESUCCESS
    } else {
        EIO
    }
}

// ============================================================================
// FSD forwarding path (authoritative POSIX FS syscall path)
// ============================================================================

const FS_PORT_ID: u64 = atom_abi::PORT_FS_SERVICE as u64;
const FS_REQ_TIMEOUT_MS: u64 = 5000;
const FS_WIRE_HEADER_SIZE: usize = 16;
const FS_WIRE_REPLY_PORT_SIZE: usize = 8;
const FS_REPLY_BASE_SIZE: usize = 16;
const FS_STAT_WIRE_SIZE: usize = 80;

const FS_MSG_OPEN: u32 = 1100;
const FS_MSG_CLOSE: u32 = 1102;
const FS_MSG_READ: u32 = 1104;
const FS_MSG_WRITE: u32 = 1106;
const FS_MSG_SEEK: u32 = 1108;
const FS_MSG_STAT: u32 = 1110;
const FS_MSG_FSTAT: u32 = 1112;
const FS_MSG_MKDIR: u32 = 1114;
const FS_MSG_RMDIR: u32 = 1116;
const FS_MSG_UNLINK: u32 = 1118;
const FS_MSG_RENAME: u32 = 1120;
const FS_MSG_READDIR: u32 = 1122;
const FS_MSG_TRUNCATE: u32 = 1124;
const FS_MSG_FSYNC: u32 = 1126;

static FS_WIRE_SEQUENCE: core::sync::atomic::AtomicU32 =
    core::sync::atomic::AtomicU32::new(1);

#[inline]
fn fs_max_write_chunk() -> usize {
    crate::ipc::MAX_MESSAGE_SIZE
        .saturating_sub(FS_WIRE_HEADER_SIZE)
        .saturating_sub(FS_WIRE_REPLY_PORT_SIZE)
        .saturating_sub(16) // fs write request fixed fields: fd + count
}

#[inline]
fn fs_max_read_chunk() -> usize {
    crate::ipc::MAX_MESSAGE_SIZE.saturating_sub(FS_REPLY_BASE_SIZE)
}

fn fs_ipc_wait_message(
    port_id: crate::ipc::PortId,
    caller: crate::thread::ThreadId,
    timeout_ms: u64,
) -> Result<crate::ipc::Message, u64> {
    if let Ok(Some(msg)) = crate::ipc::try_receive_message(port_id, caller) {
        return Ok(msg);
    }

    let priority = crate::sched::get_thread_priority(caller);
    let deadline = if timeout_ms == u64::MAX {
        None
    } else {
        Some(crate::interrupts::get_ticks() + timeout_ms.div_ceil(10))
    };

    match crate::ipc::block_receive(port_id, caller, priority, deadline) {
        Ok(()) => {
            crate::thread::set_thread_state(caller, crate::thread::ThreadState::Blocked);
            let (prev, next) = crate::sched::on_timer_tick();
            if let (Some(prev_id), Some(next_id)) = (prev, next) {
                if prev_id != next_id {
                    crate::sched::perform_context_switch(prev_id, next_id);
                }
            }
            match crate::ipc::try_receive_message(port_id, caller) {
                Ok(Some(msg)) => Ok(msg),
                Ok(None) => Err(ETIMEDOUT),
                Err(crate::ipc::IpcError::InvalidPort) => Err(ENODEV),
                Err(_) => Err(EIO),
            }
        }
        Err(crate::ipc::IpcError::InvalidPort) => Err(ENODEV),
        Err(crate::ipc::IpcError::DeadlockDetected) => Err(EDEADLK),
        Err(crate::ipc::IpcError::PortBusy) => Err(EBUSY),
        Err(_) => Err(EIO),
    }
}

fn fsd_rpc(msg_type: u32, req_payload: &[u8]) -> Result<Vec<u8>, u64> {
    let caller = crate::sched::current_thread().ok_or(EINVAL)?;
    let fs_port = crate::ipc::PortId::from_raw(FS_PORT_ID);
    let reply_port = crate::ipc::create_port(caller);

    let mut payload = Vec::with_capacity(FS_WIRE_REPLY_PORT_SIZE + req_payload.len());
    payload.extend_from_slice(&reply_port.raw().to_le_bytes());
    payload.extend_from_slice(req_payload);

    let sequence = FS_WIRE_SEQUENCE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    let mut wire = Vec::with_capacity(FS_WIRE_HEADER_SIZE + payload.len());
    wire.extend_from_slice(&1u32.to_le_bytes()); // MessageHeader.version
    wire.extend_from_slice(&msg_type.to_le_bytes());
    wire.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    wire.extend_from_slice(&sequence.to_le_bytes());
    wire.extend_from_slice(&payload);

    let send_result = crate::ipc::send_message(fs_port, crate::ipc::Message::new(caller, 0, wire));
    if send_result.is_err() {
        let _ = crate::ipc::close_port(reply_port, caller);
        return Err(match send_result {
            Err(crate::ipc::IpcError::InvalidPort) => ENODEV,
            Err(crate::ipc::IpcError::QueueFull | crate::ipc::IpcError::WouldBlock) => EBUSY,
            _ => EIO,
        });
    }

    let recv = fs_ipc_wait_message(reply_port, caller, FS_REQ_TIMEOUT_MS);
    let _ = crate::ipc::close_port(reply_port, caller);
    recv.map(|m| m.payload)
}

fn fsd_parse_reply_u64(reply: &[u8]) -> Result<u64, u64> {
    if reply.len() < FS_REPLY_BASE_SIZE {
        return Err(EIO);
    }
    let error = u64::from_le_bytes(reply[0..8].try_into().unwrap());
    if error != ESUCCESS {
        return Err(error);
    }
    Ok(u64::from_le_bytes(reply[8..16].try_into().unwrap()))
}

fn fsd_parse_reply_data(reply: &[u8]) -> Result<(usize, &[u8]), u64> {
    if reply.len() < FS_REPLY_BASE_SIZE {
        return Err(EIO);
    }
    let error = u64::from_le_bytes(reply[0..8].try_into().unwrap());
    if error != ESUCCESS {
        return Err(error);
    }
    let n = u64::from_le_bytes(reply[8..16].try_into().unwrap()) as usize;
    if reply.len() < FS_REPLY_BASE_SIZE + n {
        return Err(EIO);
    }
    Ok((n, &reply[16..16 + n]))
}

fn fsd_parse_reply_stat(reply: &[u8]) -> Result<&[u8], u64> {
    if reply.len() < 8 + FS_STAT_WIRE_SIZE {
        return Err(EIO);
    }
    let error = u64::from_le_bytes(reply[0..8].try_into().unwrap());
    if error != ESUCCESS {
        return Err(error);
    }
    Ok(&reply[8..8 + FS_STAT_WIRE_SIZE])
}

fn fsd_sys_fs_open(path_ptr: u64, path_len: usize, flags: u32, mode: u32) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let mut req = Vec::with_capacity(4 + path.len() + 8);
    req.extend_from_slice(&(path.len() as u32).to_le_bytes());
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(&flags.to_le_bytes());
    req.extend_from_slice(&mode.to_le_bytes());

    match fsd_rpc(FS_MSG_OPEN, &req).and_then(|r| fsd_parse_reply_u64(&r)) {
        Ok(fd) => fd,
        Err(e) => e,
    }
}

fn fsd_sys_fs_close(fd: u64) -> u64 {
    let req = fd.to_le_bytes();
    match fsd_rpc(FS_MSG_CLOSE, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_read(fd: u64, buf_ptr: u64, count: usize) -> u64 {
    if count == 0 {
        return 0;
    }
    let buf_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let _buf_range = match validate_user_byte_range(buf_ptr, count) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let request_count = core::cmp::min(count, fs_max_read_chunk());
    let mut req = [0u8; 16];
    req[0..8].copy_from_slice(&fd.to_le_bytes());
    req[8..16].copy_from_slice(&(request_count as u64).to_le_bytes());

    let reply = match fsd_rpc(FS_MSG_READ, &req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (n, data) = match fsd_parse_reply_data(&reply) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // n=0 means EOF — return 0 without touching the user buffer.
    if n == 0 {
        return 0;
    }

    let dst_range = match validate_user_byte_range(buf_ptr, n) {
        Ok(range) => range,
        Err(e) => return e,
    };
    if let Err(e) = write_buffer_to_user(dst_range, data) {
        return e;
    }
    n as u64
}

fn fsd_sys_fs_write(fd: u64, buf_ptr: u64, count: usize) -> u64 {
    if count == 0 {
        return 0;
    }
    let src_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let _src_range = match validate_user_byte_range(src_ptr, count) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let request_count = core::cmp::min(count, fs_max_write_chunk());
    let limited_range = match validate_user_byte_range(src_ptr, request_count) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let src_bytes = match copy_buffer_from_user(limited_range, request_count) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let mut req = Vec::with_capacity(16 + src_bytes.len());
    req.extend_from_slice(&fd.to_le_bytes());
    req.extend_from_slice(&(src_bytes.len() as u64).to_le_bytes());
    req.extend_from_slice(&src_bytes);

    match fsd_rpc(FS_MSG_WRITE, &req).and_then(|r| fsd_parse_reply_u64(&r)) {
        Ok(n) => n,
        Err(e) => e,
    }
}

fn fsd_sys_fs_seek(fd: u64, offset: i64, whence: u32) -> u64 {
    let mut req = [0u8; 20];
    req[0..8].copy_from_slice(&fd.to_le_bytes());
    req[8..16].copy_from_slice(&offset.to_le_bytes());
    req[16..20].copy_from_slice(&whence.to_le_bytes());
    match fsd_rpc(FS_MSG_SEEK, &req).and_then(|r| fsd_parse_reply_u64(&r)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_stat(path_ptr: u64, path_len: usize, stat_ptr: u64) -> u64 {
    let stat_ptr = match validate_user_ptr::<u8>(stat_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let stat_range = match validate_user_byte_range(stat_ptr, FS_STAT_WIRE_SIZE) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let mut req = Vec::with_capacity(4 + path.len());
    req.extend_from_slice(&(path.len() as u32).to_le_bytes());
    req.extend_from_slice(path.as_bytes());

    let reply = match fsd_rpc(FS_MSG_STAT, &req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let stat = match fsd_parse_reply_stat(&reply) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = write_buffer_to_user(stat_range, stat) {
        return e;
    }
    ESUCCESS
}

fn fsd_sys_fs_fstat(fd: u64, stat_ptr: u64) -> u64 {
    let stat_ptr = match validate_user_ptr::<u8>(stat_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let stat_range = match validate_user_byte_range(stat_ptr, FS_STAT_WIRE_SIZE) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let req = fd.to_le_bytes();
    let reply = match fsd_rpc(FS_MSG_FSTAT, &req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let stat = match fsd_parse_reply_stat(&reply) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = write_buffer_to_user(stat_range, stat) {
        return e;
    }
    ESUCCESS
}

fn fsd_sys_fs_mkdir(path_ptr: u64, path_len: usize, mode: u32) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let mut req = Vec::with_capacity(4 + path.len() + 4);
    req.extend_from_slice(&(path.len() as u32).to_le_bytes());
    req.extend_from_slice(path.as_bytes());
    req.extend_from_slice(&mode.to_le_bytes());

    match fsd_rpc(FS_MSG_MKDIR, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_rmdir(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let mut req = Vec::with_capacity(4 + path.len());
    req.extend_from_slice(&(path.len() as u32).to_le_bytes());
    req.extend_from_slice(path.as_bytes());

    match fsd_rpc(FS_MSG_RMDIR, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_unlink(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    let mut req = Vec::with_capacity(4 + path.len());
    req.extend_from_slice(&(path.len() as u32).to_le_bytes());
    req.extend_from_slice(path.as_bytes());

    match fsd_rpc(FS_MSG_UNLINK, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_rename(old_path_ptr: u64, old_path_len: usize, new_path_ptr: u64, new_path_len: usize) -> u64 {
    let old_range = match validate_user_path_range(old_path_ptr, old_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (old_buf, old_len) = match copy_path_from_user(old_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let new_range = match validate_user_path_range(new_path_ptr, new_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (new_buf, new_len) = match copy_path_from_user(new_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old_path = path_buf_as_str(&old_buf, old_len);
    let new_path = path_buf_as_str(&new_buf, new_len);

    let mut req = Vec::with_capacity(8 + old_path.len() + new_path.len());
    req.extend_from_slice(&(old_path.len() as u32).to_le_bytes());
    req.extend_from_slice(old_path.as_bytes());
    req.extend_from_slice(&(new_path.len() as u32).to_le_bytes());
    req.extend_from_slice(new_path.as_bytes());

    match fsd_rpc(FS_MSG_RENAME, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_readdir(dirfd: u64, dirent_ptr: u64, count: usize) -> u64 {
    if count == 0 {
        return EINVAL;
    }
    let dirent_ptr = match validate_user_ptr::<u8>(dirent_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let _dirent_range = match validate_user_byte_range(dirent_ptr, count) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let request_count = core::cmp::min(count, fs_max_read_chunk());
    let mut req = [0u8; 16];
    req[0..8].copy_from_slice(&dirfd.to_le_bytes());
    req[8..16].copy_from_slice(&(request_count as u64).to_le_bytes());

    let reply = match fsd_rpc(FS_MSG_READDIR, &req) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (n, data) = match fsd_parse_reply_data(&reply) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let dst_range = match validate_user_byte_range(dirent_ptr, n) {
        Ok(range) => range,
        Err(e) => return e,
    };
    if let Err(e) = write_buffer_to_user(dst_range, data) {
        return e;
    }
    n as u64
}

fn fsd_sys_fs_truncate(fd: u64, length: u64) -> u64 {
    let mut req = [0u8; 16];
    req[0..8].copy_from_slice(&fd.to_le_bytes());
    req[8..16].copy_from_slice(&length.to_le_bytes());
    match fsd_rpc(FS_MSG_TRUNCATE, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

fn fsd_sys_fs_fsync(fd: u64) -> u64 {
    let req = fd.to_le_bytes();
    match fsd_rpc(FS_MSG_FSYNC, &req).and_then(|r| fsd_parse_reply_u64(&r).map(|_| ESUCCESS)) {
        Ok(v) => v,
        Err(e) => e,
    }
}

// ============================================================================
// Kernel-side file descriptor table
//
// Instead of routing FS syscalls through IPC to fsd (which requires fragile
// in-kernel blocking + context-switch), the syscalls talk directly to the
// kernel's FAT32 driver.  This is reliable, fast, and avoids the entire
// class of IPC deadlock / format-mismatch bugs.
//
// The fd table uses a fixed-size static array to avoid heap allocations that
// could collide with userspace virtual addresses.
// ============================================================================

const MAX_KERNEL_FDS: usize = 128;
const MAX_PATH_BUF: usize = 256;

#[derive(Clone, Copy)]
struct FdOwnerContext {
    process_id: crate::process::ProcessId,
    address_space_pml4: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FdContextError {
    NoCurrentThread,
    NoCurrentProcess,
    InvalidOwner,
    FdTableMissing,
    OwnershipMismatch,
    CorruptedState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FdPathError {
    User(u64),
    Context(FdContextError),
}

impl From<FdContextError> for FdPathError {
    fn from(value: FdContextError) -> Self {
        FdPathError::Context(value)
    }
}

fn classify_fd_context_error(error: FdContextError) -> Result<u64, KernelOperationalError> {
    match error {
        FdContextError::OwnershipMismatch => Ok(EBADF),
        FdContextError::NoCurrentThread | FdContextError::NoCurrentProcess => {
            Err(KernelOperationalError::ContextCorruption)
        }
        FdContextError::InvalidOwner => Err(KernelOperationalError::OwnershipCorruption),
        FdContextError::FdTableMissing => Err(KernelOperationalError::InternalInvariantViolation),
        FdContextError::CorruptedState => Err(KernelOperationalError::AddressSpaceCorruption),
    }
}

fn map_fd_error(err: FdPathError) -> u64 {
    match err {
        FdPathError::User(errno) => errno,
        FdPathError::Context(ctx_err) => match classify_fd_context_error(ctx_err) {
            Ok(errno) => errno,
            Err(op_err) => {
                log_error!(
                    "syscall",
                    "[FD_CONTEXT_CORRUPTION] error={:?} pid={:?} tid={:?}",
                    ctx_err,
                    best_effort_current_process(),
                    crate::sched::current_thread()
                );
                contain_operational_violation(
                    "fd_syscall_context",
                    op_err,
                    best_effort_current_process(),
                    crate::sched::current_thread(),
                );
                ESUCCESS
            }
        },
    }
}

fn current_fd_owner_context() -> Result<FdOwnerContext, FdContextError> {
    policy::current_fd_owner_context()
}

fn fd_owner_process(fd: &KernelFd) -> Result<crate::process::ProcessId, FdContextError> {
    policy::fd_owner_process(fd)
}

fn validate_fd_process_alignment(fd: &KernelFd) -> Result<(), FdContextError> {
    policy::validate_fd_process_alignment(fd)
}

/// A kernel-side open file/directory (zero-heap, stored in .bss).
#[derive(Clone)]
struct KernelFd {
    in_use: bool,
    owner_process: Option<crate::process::ProcessId>,
    path: [u8; MAX_PATH_BUF],
    path_len: usize,
    is_dir: bool,
    flags: u32,
    offset: usize,
    /// Cached file data pointer (heap-allocated). 0 = not cached.
    cache_ptr: usize,
    /// Cached file data length in bytes.
    cache_len: usize,
    /// Whether the cached contents must be flushed back to disk.
    dirty: bool,
}

impl KernelFd {
    const EMPTY: Self = Self {
        in_use: false,
        owner_process: None,
        path: [0u8; MAX_PATH_BUF],
        path_len: 0,
        is_dir: false,
        flags: 0,
        offset: 0,
        cache_ptr: 0,
        cache_len: 0,
        dirty: false,
    };

    fn path_str(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("")
    }

    /// Free the cached file data if present.
    fn free_cache(&mut self) {
        if self.cache_ptr != 0 && self.cache_len > 0 {
            unsafe {
                let layout = core::alloc::Layout::from_size_align_unchecked(
                    self.cache_len, 1,
                );
                alloc::alloc::dealloc(self.cache_ptr as *mut u8, layout);
            }
        }
        self.cache_ptr = 0;
        self.cache_len = 0;
        self.dirty = false;
    }
}

/// Global fd table in .bss — no heap allocation.
static KERNEL_FD_TABLE: spin::Mutex<[KernelFd; MAX_KERNEL_FDS]> =
    spin::Mutex::new([KernelFd::EMPTY; MAX_KERNEL_FDS]);

#[inline]
fn validate_user_path_range(ptr: u64, len: usize) -> Result<atom_abi::UserRange, u64> {
    if len == 0 || len > MAX_PATH_BUF {
        return Err(ENAMETOOLONG);
    }
    validate_user_range(ptr, len)
}

/// Copy a path string from userspace into a stack-allocated fixed buffer.
/// Returns (buffer, length) on success.  No heap allocation.
fn copy_path_from_user(path_range: atom_abi::UserRange) -> Result<([u8; MAX_PATH_BUF], usize), u64> {
    let len = path_range.len();
    if len == 0 || len > MAX_PATH_BUF {
        return Err(ENAMETOOLONG);
    }

    // SAFETY: path_range is ABI-validated and guaranteed canonical.
    let src = unsafe { core::slice::from_raw_parts(path_range.base().as_ptr::<u8>(), len) };

    // Validate UTF-8
    if core::str::from_utf8(src).is_err() {
        return Err(EINVAL);
    }
    let mut buf = [0u8; MAX_PATH_BUF];
    buf[..len].copy_from_slice(src);
    Ok((buf, len))
}

// Compile-time architectural guard: these helpers must consume ABI-typed inputs.
const _COPY_STRING_FROM_USER_TYPED: fn(atom_abi::UserRange) -> Result<alloc::string::String, u64> =
    copy_string_from_user;
const _COPY_BUFFER_FROM_USER_TYPED: fn(atom_abi::UserRange, usize) -> Result<Vec<u8>, u64> =
    copy_buffer_from_user;
const _COPY_PATH_FROM_USER_TYPED: fn(atom_abi::UserRange) -> Result<([u8; MAX_PATH_BUF], usize), u64> =
    copy_path_from_user;
const _WRITE_BUFFER_TO_USER_TYPED: fn(atom_abi::UserRange, &[u8]) -> Result<(), u64> =
    write_buffer_to_user;
const _IPC_SEND_BATCH_CORE_TYPED: fn(
    crate::ipc::PortId,
    crate::thread::ThreadId,
    atom_abi::UserPtr<u8>,
    usize,
) -> Result<usize, crate::ipc::IpcError> = ipc_send_batch_core;
const _IPC_RECV_BATCH_CORE_TYPED: fn(
    crate::ipc::PortId,
    crate::thread::ThreadId,
    atom_abi::UserPtr<u8>,
    usize,
) -> Result<usize, crate::ipc::IpcError> = ipc_recv_batch_core;

/// Get a &str from a (buf, len) pair returned by copy_path_from_user
#[inline]
fn path_buf_as_str(buf: &[u8; MAX_PATH_BUF], len: usize) -> &str {
    // Safety: copy_path_from_user already validated UTF-8
    unsafe { core::str::from_utf8_unchecked(&buf[..len]) }
}

fn validate_fd_ownership(idx: usize, table: &[KernelFd; MAX_KERNEL_FDS]) -> Result<(), FdPathError> {
    policy::validate_fd_ownership(idx, table)
}

fn validate_fd_ownership_with_owner(
    idx: usize,
    table: &[KernelFd; MAX_KERNEL_FDS],
    caller: FdOwnerContext,
) -> Result<(), FdPathError> {
    policy::validate_fd_ownership_with_owner(idx, table, caller)
}

fn note_fd_process_activity(process_id: crate::process::ProcessId) {
    CLEANED_FD_PROCESSES.lock().remove(&process_id);
}

fn begin_fd_process_cleanup(process_id: crate::process::ProcessId) -> bool {
    CLEANED_FD_PROCESSES.lock().insert(process_id)
}

fn get_fd_entry(
    idx: usize,
) -> Result<(spin::MutexGuard<'static, [KernelFd; MAX_KERNEL_FDS]>, usize), FdPathError> {
    if MAX_KERNEL_FDS == 0 {
        return Err(FdContextError::FdTableMissing.into());
    }
    if idx >= MAX_KERNEL_FDS {
        return Err(FdPathError::User(EBADF));
    }

    let table = KERNEL_FD_TABLE.lock();

    if !table[idx].in_use {
        return Err(FdPathError::User(EBADF));
    }

    validate_fd_ownership(idx, &table)?;

    Ok((table, idx))
}

pub(crate) fn close_fds_for_process(process_id: crate::process::ProcessId) {
    if !begin_fd_process_cleanup(process_id) {
        log_debug!(
            "syscall",
            "FD cleanup already ran for process {} - skipping duplicate close_fds_for_process",
            process_id
        );
        return;
    }

    let mut table = KERNEL_FD_TABLE.lock();
    for fd in table.iter_mut() {
        if fd.in_use && fd_owner_process(fd) == Ok(process_id) {
            fd.free_cache();
            *fd = KernelFd::EMPTY;
        }
    }
}

/// Allocate an fd slot (returns 3..MAX_KERNEL_FDS-1, or EMFILE on full).
fn alloc_kernel_fd(path: &str, is_dir: bool, flags: u32) -> Result<u64, FdPathError> {
    let owner = current_fd_owner_context()?;

    note_fd_process_activity(owner.process_id);
    let mut table = KERNEL_FD_TABLE.lock();
    // fd 0/1/2 reserved for stdin/out/err
    for i in 3..MAX_KERNEL_FDS {
        if !table[i].in_use {
            table[i].in_use = true;
            table[i].owner_process = Some(owner.process_id);
            validate_fd_process_alignment(&table[i])?;
            let plen = path.len().min(MAX_PATH_BUF);
            table[i].path[..plen].copy_from_slice(&path.as_bytes()[..plen]);
            table[i].path_len = plen;
            table[i].is_dir = is_dir;
            table[i].flags = flags;
            table[i].offset = 0;
            table[i].dirty = false;
            return Ok(i as u64);
        }
    }
    Err(FdPathError::User(EMFILE))
}

fn ensure_fd_cache(table: &mut [KernelFd; MAX_KERNEL_FDS], idx: usize) -> Result<(), u64> {
    if table[idx].cache_ptr != 0 || table[idx].cache_len != 0 {
        return Ok(());
    }

    let path_buf = table[idx].path;
    let path_len = table[idx].path_len;
    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };

    let data = crate::drivers::fat32::open(path).ok_or(EIO)?;
    let len = data.len();
    if len == 0 {
        table[idx].cache_ptr = 0;
        table[idx].cache_len = 0;
        return Ok(());
    }

    let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(len, 1) };
    let ptr = unsafe { alloc::alloc::alloc(layout) };
    if ptr.is_null() {
        return Err(EIO);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), ptr, len);
    }
    table[idx].cache_ptr = ptr as usize;
    table[idx].cache_len = len;
    Ok(())
}

fn resize_fd_cache(table: &mut [KernelFd; MAX_KERNEL_FDS], idx: usize, new_len: usize) -> Result<(), u64> {
    let old_ptr = table[idx].cache_ptr;
    let old_len = table[idx].cache_len;

    if new_len == old_len {
        return Ok(());
    }

    if new_len == 0 {
        if old_ptr != 0 && old_len > 0 {
            unsafe {
                let layout = core::alloc::Layout::from_size_align_unchecked(old_len, 1);
                alloc::alloc::dealloc(old_ptr as *mut u8, layout);
            }
        }
        table[idx].cache_ptr = 0;
        table[idx].cache_len = 0;
        return Ok(());
    }

    let layout = unsafe { core::alloc::Layout::from_size_align_unchecked(new_len, 1) };
    let new_ptr = unsafe { alloc::alloc::alloc(layout) };
    if new_ptr.is_null() {
        return Err(EIO);
    }

    unsafe {
        if old_ptr != 0 && old_len > 0 {
            core::ptr::copy_nonoverlapping(old_ptr as *const u8, new_ptr, old_len.min(new_len));
            let old_layout = core::alloc::Layout::from_size_align_unchecked(old_len, 1);
            alloc::alloc::dealloc(old_ptr as *mut u8, old_layout);
        }
        if new_len > old_len {
            core::ptr::write_bytes(new_ptr.add(old_len), 0, new_len - old_len);
        }
    }

    table[idx].cache_ptr = new_ptr as usize;
    table[idx].cache_len = new_len;
    Ok(())
}

fn flush_fd_locked(table: &mut [KernelFd; MAX_KERNEL_FDS], idx: usize) -> u64 {
    if !table[idx].dirty || table[idx].is_dir {
        return ESUCCESS;
    }

    let path_buf = table[idx].path;
    let path_len = table[idx].path_len;
    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };
    let data = if table[idx].cache_ptr == 0 || table[idx].cache_len == 0 {
        &[][..]
    } else {
        unsafe {
            core::slice::from_raw_parts(table[idx].cache_ptr as *const u8, table[idx].cache_len)
        }
    };

    if crate::drivers::fat32::write_file(path, data) {
        table[idx].dirty = false;
        ESUCCESS
    } else {
        EIO
    }
}

// ============================================================================
// Direct-to-FAT32 filesystem syscalls
// ============================================================================

/// Open a file or directory
fn sys_fs_open(path_ptr: u64, path_len: usize, flags: u32, _mode: u32) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";

    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    log_debug!(LOG_ORIGIN, "sys_fs_open(path=\"{}\", flags={:#x})", path, flags);

    let o_directory: u32 = 0x10000;
    let o_creat: u32 = 0x0040;
    let o_excl: u32 = 0x0080;
    let o_trunc: u32 = 0x0200;
    let o_append: u32 = 0x0400;
    let is_dir = (flags & o_directory) != 0 || path == "/" || path.ends_with('/');

    if is_dir {
        let list_path = if path == "/" || path.is_empty() {
            ""
        } else {
            path.trim_start_matches('/')
        };
        if path != "/" && !path.is_empty()
            && crate::drivers::fat32::list_directory(list_path).is_none() {
                log_debug!(LOG_ORIGIN, "directory not found: \"{}\"", path);
                return ENOENT;
            }
    } else if crate::drivers::fat32::stat_path(path).is_none() {
        if (flags & o_creat) == 0 {
            log_debug!(LOG_ORIGIN, "file not found: \"{}\"", path);
            return ENOENT;
        }
        if !crate::drivers::fat32::write_file(path, &[]) {
            return EIO;
        }
    } else if (flags & o_creat) != 0 && (flags & o_excl) != 0 {
        return EEXIST;
    }

    let store_path = path.trim_end_matches('/');
    let fd = match alloc_kernel_fd(store_path, is_dir, flags) {
        Ok(fd) => fd,
        Err(e) => return map_fd_error(e),
    };

    if !is_dir && (flags & o_trunc) != 0 {
        let idx = fd as usize;
        let mut table = KERNEL_FD_TABLE.lock();
        if let Err(e) = ensure_fd_cache(&mut table, idx) {
            table[idx].free_cache();
            table[idx] = KernelFd::EMPTY;
            return e;
        }
        if let Err(e) = resize_fd_cache(&mut table, idx, 0) {
            table[idx].free_cache();
            table[idx] = KernelFd::EMPTY;
            return e;
        }
        table[idx].dirty = true;
        table[idx].offset = 0;
    } else if !is_dir && (flags & o_append) != 0 {
        let idx = fd as usize;
        let mut table = KERNEL_FD_TABLE.lock();
        if let Err(e) = ensure_fd_cache(&mut table, idx) {
            table[idx].free_cache();
            table[idx] = KernelFd::EMPTY;
            return e;
        }
        table[idx].offset = table[idx].cache_len;
    }

    log_debug!(LOG_ORIGIN, "sys_fs_open OK: fd={} dir={}", fd, is_dir);

    // WAD diagnostic: log file size on open
    if path.contains("doom1") || path.contains("DOOM1") {
        let trimmed = path.trim_start_matches('/');
        match crate::drivers::fat32::stat_path(trimmed) {
            Some(st) => log_info!(LOG_ORIGIN, "[WAD-DIAG] opened '{}' fd={} file_size={}", path, fd, st.size),
            None => log_warn!(LOG_ORIGIN, "[WAD-DIAG] opened '{}' fd={} but stat_path FAILED", path, fd),
        }
    }

    fd
}

/// Close a file descriptor
fn sys_fs_close(fd: u64) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";
    log_debug!(LOG_ORIGIN, "sys_fs_close(fd={})", fd);

    let idx = fd as usize;
    let (mut table, idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };

    let flush = flush_fd_locked(&mut table, idx);
    if flush != ESUCCESS {
        return flush;
    }

    table[idx].free_cache();
    table[idx].owner_process = None;
    table[idx].in_use = false;
    ESUCCESS
}

/// Read from file descriptor
///
/// File data is cached in the fd table on first read to avoid re-reading
/// the entire file from the FAT32 driver on every read() call.  This is
/// critical for large files (e.g. 4 MiB WAD) where repeated contiguous
/// page allocations would fragment physical memory and eventually fail.
fn sys_fs_read(fd: u64, buf_ptr: u64, count: usize) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";
    log_debug!(LOG_ORIGIN, "sys_fs_read(fd={}, buf_ptr={:#X}, count={})", fd, buf_ptr, count);

    // ── Argument validation ────────────────────────────────────────────────
    if count == 0 {
        return 0; // POSIX: read(count=0) returns 0, not EINVAL
    }

    let buf_user_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(_) => {
            log_warn!(LOG_ORIGIN, "sys_fs_read: invalid buf_ptr");
            return EINVAL;
        }
    };
    let buf_range = match validate_user_byte_range(buf_user_ptr, count) {
        Ok(range) => range,
        Err(_) => {
            log_warn!(
                LOG_ORIGIN,
                "sys_fs_read: buf range start={:#X} len={} rejected by ABI",
                buf_ptr,
                count
            );
            return EINVAL;
        }
    };
    if buf_range.len() != count {
        log_warn!(
            LOG_ORIGIN,
            "sys_fs_read: buf range start={:#X} len={} rejected by ABI",
            buf_ptr,
            count
        );
        return EINVAL;
    }

    let idx = fd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    let caller = match current_fd_owner_context() {
        Ok(owner) => owner,
        Err(e) => return map_fd_error(e.into()),
    };

    // ── Obtain file data (cached or fresh) ─────────────────────────────────
    // Keep this lock for the entire read path so cache_ptr/cache_len/offset
    // remain stable while we dereference and copy to userspace.
    let mut table = KERNEL_FD_TABLE.lock();
    if !table[idx].in_use {
        return EBADF;
    }

    if let Err(err) = validate_fd_ownership_with_owner(idx, &table, caller) {
        return map_fd_error(err);
    }

    if table[idx].is_dir {
        return EISDIR;
    }

    // Ensure file data is cached
    if table[idx].cache_ptr == 0 {
        // Cache miss — read from FAT32 and cache
        let path_buf = table[idx].path;
        let path_len = table[idx].path_len;
        let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };

        match crate::drivers::fat32::open(path) {
            Some(data) => {
                let flen = data.len();
                if flen == 0 {
                    // Empty file — nothing to cache
                    table[idx].cache_ptr = 0;
                    table[idx].cache_len = 0;
                } else {
                    // Allocate cache and copy file data
                    let layout = unsafe {
                        core::alloc::Layout::from_size_align_unchecked(flen, 1)
                    };
                    let ptr = unsafe { alloc::alloc::alloc(layout) };
                    if ptr.is_null() {
                        log_warn!(LOG_ORIGIN,
                            "sys_fs_read: failed to allocate {} bytes for fd {} cache",
                            flen, fd);
                        return EIO;
                    }
                    unsafe {
                        core::ptr::copy_nonoverlapping(data.as_ptr(), ptr, flen);
                    }

                    table[idx].cache_ptr = ptr as usize;
                    table[idx].cache_len = flen;

                    log_debug!(LOG_ORIGIN,
                        "sys_fs_read: cached {} bytes for fd {}", flen, fd);
                }
            }
            None => {
                log_warn!(LOG_ORIGIN,
                    "sys_fs_read: fat32::open failed for fd {}", fd);
                return EIO;
            }
        }
    }

    let data_ptr = table[idx].cache_ptr;
    let file_len = table[idx].cache_len;

    // ── Produce the data slice from cache ──────────────────────────────────
    if file_len == 0 || data_ptr == 0 {
        return 0; // empty file
    }

    let data: &[u8] = unsafe {
        core::slice::from_raw_parts(data_ptr as *const u8, file_len)
    };

    let offset = table[idx].offset;
    if offset >= file_len {
        return 0; // EOF
    }

    let available = file_len - offset;
    let to_read = available.min(count);

    // Copy data to user buffer
    let src_slice = &data[offset..offset + to_read];
    let dst_range = match validate_user_byte_range(buf_user_ptr, src_slice.len()) {
        Ok(range) => range,
        Err(e) => return e,
    };
    if let Err(e) = write_buffer_to_user(dst_range, src_slice) {
        return e;
    }

    // Update offset while still holding fd-table lock
    table[idx].offset += to_read;

    log_debug!(LOG_ORIGIN, "sys_fs_read OK: fd={} returned {} bytes", fd, to_read);
    to_read as u64
}

fn sys_fs_write(fd: u64, buf_ptr: u64, count: usize) -> u64 {
    if count == 0 {
        return 0;
    }
    let buf_ptr = match validate_user_ptr::<u8>(buf_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let src_range = match validate_user_byte_range(buf_ptr, count) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let idx = fd as usize;
    let (mut table, idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };

    if table[idx].is_dir {
        return EISDIR;
    }
    if (table[idx].flags & 0x0001) == 0 && (table[idx].flags & 0x0002) == 0 {
        return EACCES;
    }

    if let Err(e) = ensure_fd_cache(&mut table, idx) {
        return e;
    }

    let append = (table[idx].flags & 0x0400) != 0;
    let write_offset = if append { table[idx].cache_len } else { table[idx].offset };
    let new_len = write_offset.saturating_add(count);
    if let Err(e) = resize_fd_cache(&mut table, idx, new_len) {
        return e;
    }

    if count > 0 {
        // SAFETY: src_range is ABI-validated and guaranteed canonical.
        let src = unsafe { core::slice::from_raw_parts(src_range.base().as_ptr::<u8>(), count) };
        let dst = unsafe {
            core::slice::from_raw_parts_mut(
                (table[idx].cache_ptr + write_offset) as *mut u8,
                count,
            )
        };
        dst.copy_from_slice(src);
    }

    table[idx].offset = write_offset + count;
    table[idx].dirty = true;
    count as u64
}

/// Get file status by path
fn sys_fs_stat(path_ptr: u64, path_len: usize, stat_ptr: u64) -> u64 {
    // Delegate directly to the kernel FAT32 stat implementation
    sys_kern_fs_stat_path(path_ptr, path_len, stat_ptr)
}

/// Read directory entries
fn sys_fs_readdir(dirfd: u64, dirent_ptr: u64, count: usize) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";
    log_debug!(LOG_ORIGIN, "sys_fs_readdir(dirfd={}, count={})", dirfd, count);

    if count == 0 {
        return EINVAL;
    }
    let dirent_ptr = match validate_user_ptr::<u8>(dirent_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let _dirent_range = match validate_user_byte_range(dirent_ptr, count) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let idx = dirfd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    // Get path and is_dir without holding lock during FAT32 I/O
    let (path_buf, path_len, is_dir) = {
        let table = KERNEL_FD_TABLE.lock();

        if !table[idx].in_use {
            return EBADF;
        }

        if let Err(e) = validate_fd_ownership(idx, &table) {
            return map_fd_error(e);
        }

        (table[idx].path, table[idx].path_len, table[idx].is_dir)
    };

    if !is_dir {
        return ENOTDIR;
    }

    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };

    // Convert path for FAT32 driver (expects "" for root, no leading /)
    let list_path = if path == "/" || path.is_empty() {
        ""
    } else {
        path.trim_start_matches('/')
    };

    match crate::drivers::fat32::list_directory(list_path) {
        Some(entries) => {
            let mut pos = 0usize;
            let mut ino_counter: u32 = 1;

            // Stack buffer for building individual dirent records (max 8 + 255 name + padding)
            let mut rec = [0u8; 272]; // 8 header + 255 name + up to 3 padding + 6 spare

            for entry_name in &entries {
                // Parse name and type from "name/" format
                let (name, ftype) = if entry_name.ends_with('/') {
                    (&entry_name[..entry_name.len() - 1], 2u8) // directory
                } else {
                    (entry_name.as_str(), 1u8) // regular file
                };

                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len().min(255);
                let raw_len = 8 + name_len;
                let rec_len = (raw_len + 3) & !3; // 4-byte align

                if pos + rec_len > count {
                    break; // user buffer full
                }

                // Build dirent record: [ino(4)|rec_len(2)|name_len(1)|ftype(1)|name]
                // Zero out the record area first
                rec[..rec_len].fill(0);
                rec[0..4].copy_from_slice(&ino_counter.to_le_bytes());
                rec[4..6].copy_from_slice(&(rec_len as u16).to_le_bytes());
                rec[6] = name_len as u8;
                rec[7] = ftype;
                rec[8..8 + name_len].copy_from_slice(&name_bytes[..name_len]);

                let dst_ptr = match dirent_ptr.checked_add_bytes(pos) {
                    Ok(ptr) => ptr,
                    Err(err) => return map_user_address_error(err),
                };
                let dst_range = match validate_user_byte_range(dst_ptr, rec_len) {
                    Ok(range) => range,
                    Err(e) => return e,
                };
                if let Err(e) = write_buffer_to_user(dst_range, &rec[..rec_len]) {
                    return e;
                }

                pos += rec_len;
                ino_counter += 1;
            }

            log_debug!(LOG_ORIGIN, "sys_fs_readdir OK: {} bytes, {} entries", pos, ino_counter - 1);
            pos as u64
        }
        None => ENOENT,
    }
}

fn sys_fs_mkdir(path_ptr: u64, path_len: usize, _mode: u32) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);
    if crate::drivers::fat32::stat_path(path).is_some() {
        return EEXIST;
    }
    if crate::drivers::fat32::mkdir(path) {
        ESUCCESS
    } else {
        EIO
    }
}

fn sys_fs_unlink(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);
    match crate::drivers::fat32::stat_path(path) {
        Some(st) if st.is_dir => EISDIR,
        Some(_) => {
            if crate::drivers::fat32::unlink(path) { ESUCCESS } else { EIO }
        }
        None => ENOENT,
    }
}

fn sys_fs_rename(old_path_ptr: u64, old_path_len: usize, new_path_ptr: u64, new_path_len: usize) -> u64 {
    let old_range = match validate_user_path_range(old_path_ptr, old_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (old_buf, old_len) = match copy_path_from_user(old_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let new_range = match validate_user_path_range(new_path_ptr, new_path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (new_buf, new_len) = match copy_path_from_user(new_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let old_path = path_buf_as_str(&old_buf, old_len);
    let new_path = path_buf_as_str(&new_buf, new_len);

    if crate::drivers::fat32::stat_path(old_path).is_none() {
        return ENOENT;
    }
    if crate::drivers::fat32::stat_path(new_path).is_some() {
        return EEXIST;
    }
    if crate::drivers::fat32::rename(old_path, new_path) {
        ESUCCESS
    } else {
        EXDEV
    }
}

fn sys_fs_fsync(fd: u64) -> u64 {
    let idx = fd as usize;
    let (mut table, idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };
    let flush_result = flush_fd_locked(&mut table, idx);
    if flush_result != ESUCCESS {
        return flush_result;
    }

    if crate::drivers::fat32::sync() {
        ESUCCESS
    } else {
        EIO
    }
}

// Stub implementations for remaining syscalls (not in priority list)

/// Seek in file descriptor
fn sys_fs_seek(fd: u64, offset: i64, whence: u32) -> u64 {
    let idx = fd as usize;
    let (mut table, idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };

    let new_offset: i64 = match whence {
        0 => offset,                 // SEEK_SET
        1 => {                        // SEEK_CUR
            let cur = table[idx].offset as i64;
            cur + offset
        }
        2 => {                        // SEEK_END
            // Fast path: use the cached file length when already populated.
            let cached_len = table[idx].cache_len;
            if cached_len > 0 {
                let result = cached_len as i64 + offset;
                if result < 0 {
                    return EINVAL;
                }
                table[idx].offset = result as usize;
                return result as u64;
            }

            // Slow path: cache is empty (first seek before any read).
            // Read the full file via fat32::open — the same path used by
            // sys_fs_read — so we get an authoritative length AND pre-warm
            // the cache for the subsequent read call.
            let (path_buf, path_len) = (table[idx].path, table[idx].path_len);
            drop(table);

            let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };
            let file_data = match crate::drivers::fat32::open(path) {
                Some(d) => d,
                None => {
                    log_warn!("fs_syscall",
                        "sys_fs_seek SEEK_END: fat32::open('{}') failed", path);
                    return EIO;
                }
            };
            let file_len = file_data.len();

            // Allocate a kernel heap cache for the file data.
            let cache_ptr: usize = if file_len > 0 {
                let layout = unsafe {
                    core::alloc::Layout::from_size_align_unchecked(file_len, 1)
                };
                let ptr = unsafe { alloc::alloc::alloc(layout) };
                if ptr.is_null() {
                    log_warn!("fs_syscall",
                        "sys_fs_seek SEEK_END: alloc failed for {} bytes", file_len);
                    return EIO;
                }
                unsafe { core::ptr::copy_nonoverlapping(file_data.as_ptr(), ptr, file_len); }
                ptr as usize
            } else {
                0
            };
            drop(file_data);

            let result = file_len as i64 + offset;

            // Re-acquire lock to install the cache and update the offset.
            let mut table = KERNEL_FD_TABLE.lock();

            if !table[idx].in_use {
                if cache_ptr != 0 {
                    unsafe {
                        let layout = core::alloc::Layout::from_size_align_unchecked(file_len, 1);
                        alloc::alloc::dealloc(cache_ptr as *mut u8, layout);
                    }
                }
                return EBADF;
            }

            if let Err(e) = validate_fd_ownership(idx, &table) {
                if cache_ptr != 0 {
                    unsafe {
                        let layout = core::alloc::Layout::from_size_align_unchecked(file_len, 1);
                        alloc::alloc::dealloc(cache_ptr as *mut u8, layout);
                    }
                }
                return map_fd_error(e);
            }

            // Install cache only if not already populated (guard against races).
            if table[idx].cache_ptr == 0 && cache_ptr != 0 {
                table[idx].cache_ptr = cache_ptr;
                table[idx].cache_len = file_len;
            } else if cache_ptr != 0 {
                // Another call already populated the cache; free the duplicate.
                unsafe {
                    let layout = core::alloc::Layout::from_size_align_unchecked(file_len, 1);
                    alloc::alloc::dealloc(cache_ptr as *mut u8, layout);
                }
            }

            if result < 0 {
                return EINVAL;
            }
            table[idx].offset = result as usize;
            return result as u64;
        }
        _ => return EINVAL,
    };

    if new_offset < 0 {
        return EINVAL;
    }

    table[idx].offset = new_offset as usize;
    new_offset as u64
}

/// Get file status by descriptor
fn sys_fs_fstat(fd: u64, stat_ptr: u64) -> u64 {
    let stat_ptr = match validate_user_ptr::<u8>(stat_ptr) {
        Ok(ptr) => ptr,
        Err(_) => return EINVAL,
    };
    let stat_range = match validate_user_byte_range(stat_ptr, 80) {
        Ok(range) => range,
        Err(_) => return EINVAL,
    };

    let idx = fd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    // Get path from fd table
    let (path_buf, path_len) = {
        let table = KERNEL_FD_TABLE.lock();

        if !table[idx].in_use {
            return EBADF;
        }

        if let Err(e) = validate_fd_ownership(idx, &table) {
            return map_fd_error(e);
        }

        (table[idx].path, table[idx].path_len)
    };

    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };
    let stat_path = if path.is_empty() { "/" } else { path };

    fill_stat_to_user(stat_path, stat_range)
}

fn sys_fs_rmdir(path_ptr: u64, path_len: usize) -> u64 {
    let path_range = match validate_user_path_range(path_ptr, path_len) {
        Ok(range) => range,
        Err(e) => return e,
    };
    let (path_buf, path_blen) = match copy_path_from_user(path_range) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);
    match crate::drivers::fat32::stat_path(path) {
        Some(st) if !st.is_dir => ENOTDIR,
        Some(_) => {
            if crate::drivers::fat32::rmdir(path) { ESUCCESS } else { ENOTEMPTY }
        }
        None => ENOENT,
    }
}

/// Truncate file
fn sys_fs_truncate(fd: u64, _length: u64) -> u64 {
    let idx = fd as usize;
    let (_table, _idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };

    ENOTSUP
}

/// Mount filesystem
fn sys_fs_mount(_source_ptr: u64, _source_len: usize, _target_ptr: u64, _target_len: usize, _fstype_ptr: u64, _fstype_len: usize) -> u64 {
    ENOTSUP
}

/// Unmount filesystem
fn sys_fs_umount(_path_ptr: u64, _path_len: usize) -> u64 {
    ENOTSUP
}

/// Change file permissions
fn sys_fs_chmod(_path_ptr: u64, _path_len: usize, _mode: u32) -> u64 {
    ENOTSUP
}

/// Duplicate file descriptor
fn sys_fs_dup(fd: u64) -> u64 {
    let idx = fd as usize;
    let (_table, _idx) = match get_fd_entry(idx) {
        Ok(v) => v,
        Err(e) => return map_fd_error(e),
    };

    ENOTSUP
}

/// Duplicate file descriptor to specific number
fn sys_fs_dup2(oldfd: u64, newfd: u64) -> u64 {
    let old_idx = oldfd as usize;
    let new_idx = newfd as usize;

    if old_idx >= MAX_KERNEL_FDS || new_idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    let table = KERNEL_FD_TABLE.lock();

    if !table[old_idx].in_use {
        return EBADF;
    }

    if let Err(e) = validate_fd_ownership(old_idx, &table) {
        return map_fd_error(e);
    }

    if table[new_idx].in_use {
        if let Err(e) = validate_fd_ownership(new_idx, &table) {
            return map_fd_error(e);
        }
    }

    drop(table);

    ENOTSUP
}

/// Create hard link
fn sys_fs_link(_oldpath_ptr: u64, _oldpath_len: usize, _newpath_ptr: u64, _newpath_len: usize) -> u64 {
    ENOTSUP
}

/// Create symbolic link
fn sys_fs_symlink(_target_ptr: u64, _target_len: usize, _linkpath_ptr: u64, _linkpath_len: usize) -> u64 {
    ENOTSUP
}

/// Read symbolic link
fn sys_fs_readlink(_path_ptr: u64, _path_len: usize, _buf_ptr: u64, _buf_size: usize) -> u64 {
    ENOTSUP
}

/// Update file times
fn sys_fs_utimes(_path_ptr: u64, _path_len: usize, _atime: i64, _mtime: i64) -> u64 {
    ENOTSUP
}

/// Get filesystem statistics
fn sys_fs_statvfs(_path_ptr: u64, _path_len: usize, _buf_ptr: u64) -> u64 {
    ENOTSUP
}
// ============================================================================
// Video Mode Management Syscall Handlers
// ============================================================================
//
// These syscalls form the kernel interface for the display settings subsystem.
// They delegate to the graphics module which in turn uses the BGA driver.
//
// Security model:
//   - Mode changes are permitted from any process (no capability check).
//     In a production system, mode changes would require a DISPLAY capability.
//     For the current MVP, we trust the display driver (which runs early).
//   - User pointers are validated before dereferencing.

/// SYS_SET_VIDEO_MODE: Set a new video mode via BGA driver.
///
/// arg0 encoding: (height as u64) << 16 | (width as u64)
///   Bits [15:0]:  width  (u16)
///   Bits [31:16]: height (u16)
/// arg1: bpp (u64, must be 32 for current driver)
///
/// Returns ESUCCESS on success, or:
///   EINVAL   – invalid resolution or BPP
///   ENOTSUP  – backend does not support mode changes (GOP only)
///   ENOMEM   – LFB too small for requested mode
fn sys_set_video_mode(packed_res: u64, bpp_raw: u64) -> u64 {
    let width  = (packed_res & 0xFFFF) as u16;
    let height = ((packed_res >> 16) & 0xFFFF) as u16;
    let bpp    = bpp_raw as u8;

    log_debug!("syscall", "SYS_SET_VIDEO_MODE: {}x{}x{}", width, height, bpp);

    match crate::graphics::set_video_mode(width, height, bpp) {
        Ok(()) => {
            log_info!("syscall", "Video mode set: {}x{}x{}", width, height, bpp);
            ESUCCESS
        }
        Err(e) => {
            log_warn!("syscall", "SYS_SET_VIDEO_MODE failed: {:?}", e);
            match e {
                crate::graphics::VideoModeError::InvalidResolution  => EINVAL,
                crate::graphics::VideoModeError::UnsupportedBpp     => EINVAL,
                crate::graphics::VideoModeError::ExceedsLfbSize     => ENOMEM,
                crate::graphics::VideoModeError::DeviceNotAvailable => ENODEV,
                crate::graphics::VideoModeError::HardwareRejected   => EINVAL,
                crate::graphics::VideoModeError::BackendUnsupported => ENOTSUP,
            }
        }
    }
}

/// SYS_GET_VIDEO_MODES: Enumerate available video modes.
///
/// arg0: pointer to output buffer (array of 5 × u32 entries)
/// arg1: maximum number of modes to write
///
/// Each mode entry is 5 × u32 = 20 bytes:
///   [0] width
///   [1] height
///   [2] bpp
///   [3] refresh_rate
///   [4] flags (bit 0: LFB available — always 1 for BGA modes)
///
/// Returns: number of modes written (>= 0), or EINVAL if buf_ptr is null.
fn sys_get_video_modes(buf_ptr: u64, max_modes: usize) -> u64 {
    if max_modes == 0 {
        return 0;
    }
    let buf_ptr = match validate_user_addr(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };

    // Use a kernel-side buffer to avoid writing directly from the mode table
    let mut kernel_modes = [crate::drivers::bga::VideoMode { width: 0, height: 0, bpp: 0, refresh_rate: 0 }; 32];
    let count = crate::graphics::get_available_modes(&mut kernel_modes).min(max_modes);

    // Validate the full user-space write range with checked arithmetic
    let write_bytes = match count.checked_mul(5).and_then(|v| v.checked_mul(core::mem::size_of::<u32>())) {
        Some(v) => v,
        None => return EINVAL,
    };
    if validate_user_range(buf_ptr.as_u64(), write_bytes).is_err() {
        return EINVAL;
    }

    let buf_ptr = buf_ptr.as_mut_ptr::<u32>();
    unsafe {
        for (i, m) in kernel_modes.iter().enumerate().take(count) {
            let base = buf_ptr.add(i * 5);
            base.add(0).write_volatile(m.width  as u32);
            base.add(1).write_volatile(m.height as u32);
            base.add(2).write_volatile(m.bpp    as u32);
            base.add(3).write_volatile(m.refresh_rate as u32);
            base.add(4).write_volatile(1u32); // LFB always available
        }
    }

    count as u64
}

/// SYS_GET_CURRENT_VIDEO_MODE: Write current video mode info to userspace.
///
/// arg0: pointer to a 32-byte output struct (8 × u32 fields, matching KernelVideoInfo)
///
/// Layout written to user buffer:
///   Bytes [0..8]   lfb_phys        (u64)
///   Bytes [8..12]  width           (u32)
///   Bytes [12..16] height          (u32)
///   Bytes [16..20] pitch           (u32) bytes per scanline
///   Bytes [20..24] bytes_per_pixel (u32)
///   Bytes [24..28] bga_available   (u32) 1 if BGA is present
///   Bytes [28..32] mode_active     (u32) 1 if a mode has been set
///
/// Returns ESUCCESS, or EINVAL if buf_ptr is null.
fn sys_get_current_video_mode(buf_ptr: u64) -> u64 {
    let buf_ptr = match validate_user_addr(buf_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(buf_ptr.as_u64(), 4 * core::mem::size_of::<u64>()).is_err() {
        return EINVAL;
    }

    let info = crate::graphics::get_kernel_video_info();

    let buf_ptr = buf_ptr.as_mut_ptr::<u64>();
    unsafe {
        // lfb_phys (u64, 8 bytes)
        buf_ptr.add(0).write_volatile(info.lfb_phys);
        // Pack width + height into a single u64 write (little-endian)
        let wh = (info.width as u64) | ((info.height as u64) << 32);
        buf_ptr.add(1).write_volatile(wh);
        // Pack pitch + bytes_per_pixel
        let pp = (info.pitch as u64) | ((info.bytes_per_pixel as u64) << 32);
        buf_ptr.add(2).write_volatile(pp);
        // Pack bga_available + mode_active
        let flags = (info.bga_available as u64) | ((info.mode_active as u64) << 32);
        buf_ptr.add(3).write_volatile(flags);
    }

    ESUCCESS
}

/// SYS_VIDEO_MODE_COUNT: Return the total number of available video modes.
fn sys_video_mode_count() -> u64 {
    crate::graphics::available_mode_count() as u64
}

// ---------------------------------------------------------------------------
// Infrastructure / Networking Phase 1 syscall handlers
// ---------------------------------------------------------------------------

fn sys_pci_get_bar(dev_cap_handle: u64, index: u8, info_ptr: u64) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let handle = crate::cap::CapHandle::from_raw(dev_cap_handle);

    // Validate capability: must be Device type
    let bdf = match crate::thread::validate_thread_capability(caller, handle, crate::cap::CapPermissions::READ) {
        Ok(()) => {
            let cap = crate::cap::lookup_capability(handle).unwrap();
            match cap.resource {
                crate::cap::ResourceType::Device { bdf } => bdf,
                _ => return EPERM,
            }
        }
        Err(_) => return EPERM,
    };

    let bus = (bdf >> 8) as u8;
    let dev = ((bdf >> 3) & 0x1F) as u8;
    let func = (bdf & 0x07) as u8;

    let bar = match crate::drivers::pci::get_bar_info(bus, dev, func, index) {
        Some(b) => b,
        None => return EINVAL,
    };

    let info_ptr = match validate_user_addr(info_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(info_ptr.as_u64(), core::mem::size_of::<atom_abi::PciBarInfo>()).is_err() {
        return EINVAL;
    }

    // Auto-create MMIO capability if it's MMIO
    let mmio_cap = if bar.is_mmio {
        let mmio_res = crate::cap::ResourceType::MemoryRegion {
            virt_addr: 0, // not yet mapped
            phys_addr: bar.base,
            size: bar.size as usize,
        };
        match crate::cap::create_root_capability(mmio_res, caller, crate::cap::CapPermissions::ALL) {
            Ok(cap) => {
                match crate::thread::add_thread_capability(caller, cap) {
                    Ok(h) => h.raw(),
                    Err(_) => 0,
                }
            }
            Err(_) => 0,
        }
    } else {
        0
    };

    let out = atom_abi::PciBarInfo {
        base_addr: bar.base,
        size: bar.size,
        mmio_cap,
        is_mmio: if bar.is_mmio { 1 } else { 0 },
        is_64bit: if bar.is_64bit { 1 } else { 0 },
        prefetchable: if bar.prefetchable { 1 } else { 0 },
    };

    unsafe {
        *(info_ptr.as_mut_ptr::<atom_abi::PciBarInfo>()) = out;
    }

    ESUCCESS
}

fn sys_device_bind_irq(dev_cap_handle: u64, info_ptr: u64) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let handle = crate::cap::CapHandle::from_raw(dev_cap_handle);

    let bdf = match crate::thread::validate_thread_capability(caller, handle, crate::cap::CapPermissions::READ) {
        Ok(()) => {
            let cap = crate::cap::lookup_capability(handle).unwrap();
            match cap.resource {
                crate::cap::ResourceType::Device { bdf } => bdf,
                _ => return EPERM,
            }
        }
        Err(_) => return EPERM,
    };

    let bus = (bdf >> 8) as u8;
    let dev = ((bdf >> 3) & 0x1F) as u8;
    let func = (bdf & 0x07) as u8;

    let pci_dev = match crate::drivers::pci::find_by_bdf(bdf) {
        Some(d) => d,
        None => return EINVAL,
    };

    if pci_dev.irq_line == 0 || pci_dev.irq_line == 0xFF {
        return ENOTSUP;
    }

    // Create Irq capability
    let irq_res = crate::cap::ResourceType::Irq { irq_num: pci_dev.irq_line };
    let irq_cap = match crate::cap::create_root_capability(irq_res, caller, crate::cap::CapPermissions::ALL) {
        Ok(cap) => {
            match crate::thread::add_thread_capability(caller, cap) {
                Ok(h) => h,
                Err(_) => return ENOMEM,
            }
        }
        Err(_) => return ENOMEM,
    };

    let info_ptr = match validate_user_addr(info_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(info_ptr.as_u64(), core::mem::size_of::<atom_abi::IrqBindInfo>()).is_err() {
        return EINVAL;
    }

    let out = atom_abi::IrqBindInfo {
        irq_cap: irq_cap.raw(),
        vector: pci_dev.irq_line as u32,
        reserved: 0,
    };

    unsafe {
        *(info_ptr.as_mut_ptr::<atom_abi::IrqBindInfo>()) = out;
    }

    ESUCCESS
}

fn sys_irq_listen(irq_cap_handle: u64, port_id: u64) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let handle = crate::cap::CapHandle::from_raw(irq_cap_handle);

    let irq_num = match crate::thread::validate_thread_capability(caller, handle, crate::cap::CapPermissions::READ) {
        Ok(()) => {
            let cap = crate::cap::lookup_capability(handle).unwrap();
            match cap.resource {
                crate::cap::ResourceType::Irq { irq_num } => irq_num,
                _ => return EPERM,
            }
        }
        Err(_) => return EPERM,
    };

    let mut bindings = IRQ_BINDINGS.lock();
    if bindings.contains_key(&irq_num) {
        return EBUSY;
    }

    bindings.insert(irq_num, IrqBinding {
        owner: caller,
        port: port_id,
        pending: AtomicBool::new(false),
    });
    ESUCCESS
}

fn sys_irq_ack(irq_cap_handle: u64) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let handle = crate::cap::CapHandle::from_raw(irq_cap_handle);

    let irq_num = match crate::thread::validate_thread_capability(caller, handle, crate::cap::CapPermissions::WRITE) {
        Ok(()) => {
            let cap = crate::cap::lookup_capability(handle).unwrap();
            match cap.resource {
                crate::cap::ResourceType::Irq { irq_num } => irq_num,
                _ => return EPERM,
            }
        }
        Err(_) => return EPERM,
    };

    let bindings = IRQ_BINDINGS.lock();
    if let Some(binding) = bindings.get(&irq_num) {
        if binding.owner != caller {
            return EPERM;
        }
        binding.pending.store(false, Ordering::SeqCst);
        ESUCCESS
    } else {
        EINVAL
    }
}

fn sys_dma_alloc(params_ptr: u64, info_ptr: u64) -> u64 {
    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let caller_process = match crate::thread::get_thread_process_id(caller) {
        Some(pid) => pid,
        None => return EPERM,
    };

    let params_ptr = match validate_user_addr(params_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(params_ptr.as_u64(), core::mem::size_of::<atom_abi::DmaAllocParams>()).is_err() {
        return EINVAL;
    }
    let params = unsafe { *(params_ptr.as_ptr::<atom_abi::DmaAllocParams>()) };

    if params.size == 0 || params.size > 1024 * 1024 * 16 { // Cap at 16MB for now
        return EINVAL;
    }

    let pages = (params.size as usize).div_ceil(crate::mm::pmm::PAGE_SIZE);
    let phys_addr = match crate::mm::pmm::alloc_pages_zeroed(pages) {
        Some(p) => p,
        None => return ENOMEM,
    };

    // Create DmaBuffer capability
    let dma_res = crate::cap::ResourceType::DmaBuffer {
        phys_addr: phys_addr as u64,
        size: params.size as usize,
    };
    let dma_cap = match crate::cap::create_root_capability(dma_res, caller, crate::cap::CapPermissions::ALL) {
        Ok(cap) => {
            match crate::thread::add_thread_capability(caller, cap) {
                Ok(h) => h,
                Err(_) => {
                    let _ = crate::mm::pmm::free_pages(phys_addr, pages);
                    return ENOMEM;
                }
            }
        }
        Err(_) => {
            let _ = crate::mm::pmm::free_pages(phys_addr, pages);
            return ENOMEM;
        }
    };

    // Map into userspace
    let va_hint = 0; // Kernel chooses
    let flags = crate::mm::vm::PageFlags::PRESENT | crate::mm::vm::PageFlags::WRITABLE | crate::mm::vm::PageFlags::USER;

    let pml4 = match crate::thread::get_thread_address_space(caller) {
        Some(p) => p,
        None => return EINVAL,
    };

    let user_va = match crate::mm::vma::find_process_free_region(caller_process, atom_abi::USER_MMAP_START as usize, atom_abi::USER_MMAP_END as usize, pages * crate::mm::pmm::PAGE_SIZE) {
        Ok(Some(addr)) => addr,
        _ => return ENOMEM,
    };

    for i in 0..pages {
        let v = user_va + i * crate::mm::pmm::PAGE_SIZE;
        let p = phys_addr + i * crate::mm::pmm::PAGE_SIZE;
        if let Err(_) = crate::mm::vm::map_page_in_pml4(pml4 as usize, v, p, flags) {
            return ENOMEM;
        }
    }

    // Register VMA
    let vma = crate::mm::vma::Vma {
        start: user_va,
        end: user_va + pages * crate::mm::pmm::PAGE_SIZE,
        perms: crate::mm::vma::VmaPermissions::read_write(),
        backing: crate::mm::vma::VmaBacking::Device { phys_base: phys_addr },
        label: "dma",
    };
    if let Err(_) = crate::mm::vma::insert_process_vma(caller_process, vma) {
        return ENOMEM;
    }

    let info_ptr = match validate_user_addr(info_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(info_ptr.as_u64(), core::mem::size_of::<atom_abi::DmaMappingInfo>()).is_err() {
        return EINVAL;
    }

    let out = atom_abi::DmaMappingInfo {
        user_va: user_va as u64,
        phys_addr: phys_addr as u64,
        size: params.size,
    };

    unsafe {
        *(info_ptr.as_mut_ptr::<atom_abi::DmaMappingInfo>()) = out;
    }

    dma_cap.raw()
}

fn sys_dma_map(dma_cap_handle: u64, info_ptr: u64) -> u64 {
    // For MVP, sys_dma_alloc already maps it.
    // This would be used if transferring the capability to another process.
    ENOSYS
}

fn sys_dma_free(dma_cap_handle: u64) -> u64 {
    // TODO: implement revocation callback for DmaBuffer to free physical pages and unmap
    ENOSYS
}

fn sys_map_mmio(mmio_cap_handle: u64, out_addr_ptr: u64) -> u64 {
    // For simplicity in MVP, we might allow mapping a BAR directly if holding DeviceCap
    // or we implement the MmioRegionCap derivation.
    // Let's use the MmioRegionCap approach.

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };
    let caller_process = match crate::thread::get_thread_process_id(caller) {
        Some(pid) => pid,
        None => return EPERM,
    };
    let pml4 = match crate::thread::get_thread_address_space(caller) {
        Some(p) => p,
        None => return EINVAL,
    };

    let handle = crate::cap::CapHandle::from_raw(mmio_cap_handle);
    let (phys_addr, size) = match crate::thread::validate_thread_capability(caller, handle, crate::cap::CapPermissions::READ) {
        Ok(()) => {
            let cap = crate::cap::lookup_capability(handle).unwrap();
            match cap.resource {
                crate::cap::ResourceType::MemoryRegion { phys_addr, size, .. } => (phys_addr, size),
                _ => return EPERM,
            }
        }
        Err(_) => return EPERM,
    };

    let pages = size.div_ceil(crate::mm::pmm::PAGE_SIZE);
    let user_va = match crate::mm::vma::find_process_free_region(caller_process, atom_abi::USER_MMAP_START as usize, atom_abi::USER_MMAP_END as usize, pages * crate::mm::pmm::PAGE_SIZE) {
        Ok(Some(addr)) => addr,
        _ => return ENOMEM,
    };

    let flags = crate::mm::vm::PageFlags::PRESENT | crate::mm::vm::PageFlags::WRITABLE | crate::mm::vm::PageFlags::USER | crate::mm::vm::PageFlags::CACHE_DISABLE;

    for i in 0..pages {
        let v = user_va + i * crate::mm::pmm::PAGE_SIZE;
        let p = (phys_addr as usize) + i * crate::mm::pmm::PAGE_SIZE;
        if let Err(_) = crate::mm::vm::map_page_in_pml4(pml4 as usize, v, p, flags) {
            return ENOMEM;
        }
    }

    let out_addr_ptr = match validate_user_addr(out_addr_ptr) {
        Ok(ptr) => ptr,
        Err(e) => return e,
    };
    if validate_user_range(out_addr_ptr.as_u64(), 8).is_err() {
        return EINVAL;
    }

    unsafe {
        *(out_addr_ptr.as_mut_ptr::<u64>()) = user_va as u64;
    }

    ESUCCESS
}

// ============================================================================
// Tests — Syscall Hardening (Phase 6)
// ============================================================================

#[cfg(test)]
mod syscall_hardening_tests {
    use super::*;

    // ── Property: SyscallError::to_errno() is always non-zero ────────────────
    // For any SyscallError variant, to_errno() must return a non-zero value.
    // Zero means success (ESUCCESS), so returning zero for an error is a bug.

    #[test]
    fn syscall_error_to_errno_is_nonzero_for_all_variants() {
        let variants = [
            SyscallError::AddressSpaceDrift,
            SyscallError::ProcessMetadataCorrupted,
            SyscallError::ThreadProcessMismatch,
            SyscallError::InvalidUserReturnAddress,
            SyscallError::AdmissionDenied,
            SyscallError::CapabilityRevokePartial,
            SyscallError::InvalidPointer,
            SyscallError::PermissionDenied,
            SyscallError::InternalInconsistency("test"),
        ];

        for variant in &variants {
            let errno = variant.to_errno();
            assert_ne!(
                errno, 0,
                "SyscallError::{:?}.to_errno() returned 0 (ESUCCESS) — this is a bug",
                variant
            );
        }
    }

    // ── Property: SyscallError::to_errno() never overflows u64 ───────────────
    // All errno values must be small positive integers, not near u64::MAX.
    // Values near u64::MAX are used as error sentinels in some syscall paths.

    #[test]
    fn syscall_error_to_errno_is_small_positive() {
        let variants = [
            SyscallError::AddressSpaceDrift,
            SyscallError::ProcessMetadataCorrupted,
            SyscallError::ThreadProcessMismatch,
            SyscallError::InvalidUserReturnAddress,
            SyscallError::AdmissionDenied,
            SyscallError::CapabilityRevokePartial,
            SyscallError::InvalidPointer,
            SyscallError::PermissionDenied,
            SyscallError::InternalInconsistency("test"),
        ];

        for variant in &variants {
            let errno = variant.to_errno();
            assert!(
                errno < 1000,
                "SyscallError::{:?}.to_errno() = {} is unexpectedly large (should be a small POSIX errno)",
                variant,
                errno
            );
        }
    }

    // ── Property: SyscallContextError variants are distinct ──────────────────
    // Each SyscallContextError variant must be distinguishable from the others.

    #[test]
    fn syscall_context_error_variants_are_distinct() {
        let variants = [
            SyscallContextError::ProcessContextMismatch,
            SyscallContextError::InvalidUserReturnAddress,
            SyscallContextError::MissingAddressSpace,
            SyscallContextError::ThreadMetadataDrift,
        ];

        // Check all pairs are distinct
        for i in 0..variants.len() {
            for j in 0..variants.len() {
                if i != j {
                    assert_ne!(
                        variants[i], variants[j],
                        "SyscallContextError variants at index {} and {} are equal — they must be distinct",
                        i, j
                    );
                }
            }
        }
    }

    // ── Property: normalize_syscall_result maps known errno values ────────────

    #[test]
    fn normalize_syscall_result_maps_enomem_to_admission_denied() {
        let result = normalize_syscall_result(ENOMEM);
        assert_eq!(
            result,
            Some(SyscallError::AdmissionDenied),
            "ENOMEM should map to SyscallError::AdmissionDenied"
        );
    }

    #[test]
    fn normalize_syscall_result_maps_eperm_to_permission_denied() {
        let result = normalize_syscall_result(EPERM);
        assert_eq!(
            result,
            Some(SyscallError::PermissionDenied),
            "EPERM should map to SyscallError::PermissionDenied"
        );
    }

    #[test]
    fn normalize_syscall_result_maps_ebusy_to_capability_revoke_partial() {
        let result = normalize_syscall_result(EBUSY);
        assert_eq!(
            result,
            Some(SyscallError::CapabilityRevokePartial),
            "EBUSY should map to SyscallError::CapabilityRevokePartial"
        );
    }

    #[test]
    fn normalize_syscall_result_maps_einval_to_invalid_pointer() {
        let result = normalize_syscall_result(EINVAL);
        assert_eq!(
            result,
            Some(SyscallError::InvalidPointer),
            "EINVAL should map to SyscallError::InvalidPointer"
        );
    }

    #[test]
    fn normalize_syscall_result_returns_none_for_success() {
        let result = normalize_syscall_result(ESUCCESS);
        assert_eq!(
            result, None,
            "ESUCCESS should not be classified as an error"
        );
    }

    #[test]
    fn normalize_syscall_result_returns_none_for_enosys() {
        let result = normalize_syscall_result(ENOSYS);
        assert_eq!(
            result, None,
            "ENOSYS should not be classified (unimplemented syscall is not a subsystem error)"
        );
    }

    // ── Property: context_error_code returns distinct values ─────────────────

    #[test]
    fn context_error_code_returns_distinct_values() {
        let codes = [
            context_error_code(SyscallContextError::ProcessContextMismatch),
            context_error_code(SyscallContextError::InvalidUserReturnAddress),
            context_error_code(SyscallContextError::MissingAddressSpace),
            context_error_code(SyscallContextError::ThreadMetadataDrift),
        ];

        // All codes must be non-zero
        for &code in &codes {
            assert_ne!(code, 0, "context_error_code must never return 0");
        }

        // All codes must be distinct
        for i in 0..codes.len() {
            for j in 0..codes.len() {
                if i != j {
                    assert_ne!(
                        codes[i], codes[j],
                        "context_error_code values at index {} and {} are equal — they must be distinct",
                        i, j
                    );
                }
            }
        }
    }

    // ── Example test: validate_syscall_context rejects unknown thread ─────────
    // When a thread ID has no associated process, validate_syscall_context
    // must return Err(SyscallContextError::ThreadMetadataDrift).

    #[test]
    fn validate_syscall_context_rejects_unknown_thread() {
        // Use a thread ID that is very unlikely to exist in the registry
        let nonexistent_tid = crate::thread::ThreadId::from_raw(u64::MAX - 42);
        let result = validate_syscall_context(nonexistent_tid, 0xDEAD_BEEF_0000);
        assert!(
            result.is_err(),
            "validate_syscall_context must return Err for a thread with no registered process"
        );
        assert_eq!(
            result.unwrap_err(),
            SyscallContextError::ThreadMetadataDrift,
            "Unknown thread should produce ThreadMetadataDrift"
        );
    }

    #[test]
    fn abi_user_range_wrapper_rejects_non_canonical_pointers() {
        let invalid_non_canonical = 0x0001_0000_0000_0000u64;
        assert_eq!(validate_user_range(invalid_non_canonical, 8), Err(EINVAL));
    }

    #[test]
    fn abi_mmap_return_validation_enforces_window_and_alignment() {
        assert!(
            validate_mmap_return_range(atom_abi::USER_MMAP_START, atom_abi::USER_PAGE_SIZE).is_ok()
        );
        assert_eq!(
            validate_mmap_return_range(atom_abi::USER_MMAP_END, atom_abi::USER_PAGE_SIZE),
            Err(atom_abi::UserAddressError::AboveUserMax)
        );
        assert_eq!(
            validate_mmap_return_range(atom_abi::USER_MMAP_START + 1, atom_abi::USER_PAGE_SIZE),
            Err(atom_abi::UserAddressError::Unaligned)
        );
    }
}
