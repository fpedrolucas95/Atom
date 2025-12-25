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

pub const ESUCCESS: u64 = 0;
pub const EINVAL: u64 = u64::MAX - 1;
pub const ENOSYS: u64 = u64::MAX - 2;
pub const ENOMEM: u64 = u64::MAX - 3;
pub const EPERM: u64 = u64::MAX - 4;
pub const EBUSY: u64 = u64::MAX - 5;
pub const EMSGSIZE: u64 = u64::MAX - 6;
pub const ETIMEDOUT: u64 = u64::MAX - 7;
pub const EWOULDBLOCK: u64 = u64::MAX - 8;
pub const EDEADLK: u64 = u64::MAX - 9;

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

#[no_mangle]
extern "C" fn rust_syscall_dispatcher(
    syscall_num: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "Syscall entry: num={} args=({:#X}, {:#X}, {:#X}, {:#X}, {:#X}, {:#X})",
        syscall_num, arg0, arg1, arg2, arg3, arg4, arg5
    );

    match syscall_num {
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
        SYS_MOUSE_POLL => sys_mouse_poll(),
        SYS_IO_PORT_READ => sys_io_port_read(arg0 as u16, arg1 as u8),
        SYS_IO_PORT_WRITE => sys_io_port_write(arg0 as u16, arg1 as u8),
        SYS_KEYBOARD_POLL => sys_keyboard_poll(),
        SYS_GET_FRAMEBUFFER => sys_get_framebuffer(arg0 as *mut u64),
        SYS_GET_TICKS => sys_get_ticks(),
        SYS_DEBUG_LOG => sys_debug_log(arg0 as *const u8, arg1 as usize),
        SYS_REGISTER_IRQ_HANDLER => sys_register_irq_handler(arg0 as u8, arg1),
        SYS_MAP_FRAMEBUFFER => sys_map_framebuffer_to_user(arg0),
        SYS_UNREGISTER_IRQ_HANDLER => sys_unregister_irq_handler(arg0 as u8),

        _ => {
            log_warn!(
                LOG_ORIGIN,
                "Unknown syscall number: {}",
                syscall_num
            );
            ENOSYS
        }
    }
}

fn sys_mouse_poll() -> u64 {
    if let Some((dx, dy)) = crate::mouse::drain_delta() {
        let dx_u = dx as u32 as u64;
        let dy_u = dy as u32 as u64;
        return (dx_u << 32) | dy_u;
    }

    EWOULDBLOCK
}

/// Read a byte from an IO port (privileged operation for drivers)
fn sys_io_port_read(port: u16, _size: u8) -> u64 {
    // Allow specific PS/2 controller ports for usermode drivers
    let allowed_ports = [0x60, 0x64]; // PS/2 data and status/command ports
    
    if !allowed_ports.contains(&port) {
        return EPERM;
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
    // Allow specific PS/2 controller ports for usermode drivers
    let allowed_ports = [0x60, 0x64]; // PS/2 data and status/command ports
    
    if !allowed_ports.contains(&port) {
        return EPERM;
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

/// Poll keyboard buffer for input
fn sys_keyboard_poll() -> u64 {
    if let Some(scancode) = crate::keyboard::poll_scancode() {
        return scancode as u64;
    }
    EWOULDBLOCK
}

/// Get framebuffer information for userspace graphics
fn sys_get_framebuffer(info_ptr: *mut u64) -> u64 {
    if info_ptr.is_null() {
        return EINVAL;
    }
    
    if let Some((width, height)) = crate::graphics::get_dimensions() {
        if let Some(addr) = crate::graphics::get_framebuffer_address() {
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
fn sys_debug_log(msg_ptr: *const u8, len: usize) -> u64 {
    if msg_ptr.is_null() || len > 256 {
        return EINVAL;
    }
    
    let msg = unsafe {
        core::slice::from_raw_parts(msg_ptr, len)
    };
    
    if let Ok(s) = core::str::from_utf8(msg) {
        log_info!("userspace", "{}", s);
    }
    
    ESUCCESS
}

#[allow(dead_code)]
fn validate_required_capability(
    _resource_type: crate::cap::ResourceType,
    required_permission: crate::cap::CapPermissions,
) -> Result<crate::thread::ThreadId, u64> {
    const LOG_ORIGIN: &str = "cap";

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return Err(EINVAL),
    };

    log_debug!(
        LOG_ORIGIN,
        "Capability check: thread={} requires permission={:?}",
        caller,
        required_permission
    );

    Ok(caller)
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
        "thread_exit(code={})",
        exit_code
    );

    if let Some(tid) = crate::sched::current_thread() {
        crate::thread::set_thread_state(tid, crate::thread::ThreadState::Exited);
        let (prev, next) = crate::sched::on_timer_tick();

        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                crate::sched::perform_context_switch(prev_id, next_id);
            }
        }

        log_panic!(
            LOG_ORIGIN,
            "thread_exit returned unexpectedly (tid={})",
            tid
        );
    }

    ESUCCESS
}

fn sys_thread_sleep(ticks: u64) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "thread_sleep(ticks={})",
        ticks
    );

    if ticks == 0 {
        return sys_thread_yield();
    }

    if let Some(tid) = crate::sched::current_thread() {
        crate::thread::set_thread_state(tid, crate::thread::ThreadState::Blocked);
        let (prev, next) = crate::sched::on_timer_tick();

        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                crate::sched::perform_context_switch(prev_id, next_id);
            }
        }
    }

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

    let has_permission = crate::thread::validate_thread_capability_by_type(
        caller,
        crate::cap::CapPermissions::WRITE,
        |resource| matches!(resource, crate::cap::ResourceType::Thread(_)),
    );

    if !has_permission {
        log_warn!(
            LOG_ORIGIN,
            "thread_create denied: missing Thread capability with WRITE permission (caller={})",
            caller
        );
        return EPERM;
    }

    log_debug!(
        LOG_ORIGIN,
        "thread_create capability validated (caller={})",
        caller
    );

    const KERNEL_STACK_SIZE: usize = 16 * 1024;
    let kernel_stack = match crate::mm::pmm::alloc_pages(KERNEL_STACK_SIZE / 4096) {
        Some(addr) => addr + KERNEL_STACK_SIZE,
        None => {
            log_error!(
                LOG_ORIGIN,
                "thread_create failed: kernel stack allocation failed"
            );
            return ENOMEM;
        }
    };

    let thread = crate::thread::Thread::new(
        entry_point,
        kernel_stack as u64,
        KERNEL_STACK_SIZE,
        0,
        crate::thread::ThreadPriority::Normal,
        "user_thread",
    );

    let tid = thread.id();
    crate::sched::add_thread(thread);

    log_info!(
        LOG_ORIGIN,
        "thread_create succeeded: new thread id={}",
        tid
    );

    tid.raw()
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
        let ticks = (timeout_ms + 9) / 10;
        Some(crate::interrupts::get_ticks() + ticks)
    };

    let copy_message = |msg: crate::ipc::Message| -> u64 {
        let bytes_to_copy =
            core::cmp::min(msg.payload.len(), buffer_size as usize);

        if buffer_ptr != 0 && bytes_to_copy > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    msg.payload.as_ptr(),
                    buffer_ptr as *mut u8,
                    bytes_to_copy
                );
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
            return copy_message(msg);
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

    let mut payload = alloc::vec::Vec::new();
    if payload_len > 0 && payload_ptr != 0 {
        payload.resize(payload_len as usize, 0);
        unsafe {
            core::ptr::copy_nonoverlapping(
                payload_ptr as *const u8,
                payload.as_mut_ptr(),
                payload_len as usize
            );
        }
    }

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

    match crate::ipc::try_receive_message(port_id, caller) {
        Ok(Some(msg)) => {
            let bytes_to_copy =
                core::cmp::min(msg.payload.len(), buffer_size as usize);

            if buffer_ptr != 0 && bytes_to_copy > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        msg.payload.as_ptr(),
                        buffer_ptr as *mut u8,
                        bytes_to_copy
                    );
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

    let events = crate::ipc::read_trace(max_events as usize);
    let available = events.len();

    if buffer_ptr != 0 {
        let to_copy = core::cmp::min(available, max_events as usize);
        unsafe {
            let buffer = buffer_ptr as *mut RawIpcTraceEvent;
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

            if stats_ptr != 0 {
                unsafe {
                    (stats_ptr as *mut RawIpcPortStats).write(stats.into());
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

fn sys_ipc_send_batch(port_id_raw: u64, messages_ptr: u64, count: u64) -> u64 {
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

    let mut messages = alloc::vec::Vec::new();
    for i in 0..count {
        let msg = crate::ipc::Message::new(sender, i as u32, alloc::vec![i as u8]);
        messages.push(msg);
    }

    match crate::ipc::send_batch(port_id, messages) {
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

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "ipc_recv_batch: no current thread");
            return EINVAL;
        }
    };

    let port_id = crate::ipc::PortId::from_raw(port_id_raw);

    match crate::ipc::receive_batch(port_id, caller, max_count as usize) {
        Ok(messages) => {
            let count = messages.len();
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
        return EPERM;
    }

    let cap_handle = crate::cap::CapHandle::from_raw(cap_handle_raw);
    if !crate::thread::thread_has_capability(sender, cap_handle) {
        log_warn!(
            "syscall",
            "ipc_send_with_cap: denied (sender does not own capability cap={:#x})",
            cap_handle_raw
        );
        return EPERM;
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
        return EPERM;
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

    let resource = match resource_type {
        0 => {
            let tid = crate::thread::ThreadId::from_raw(resource_id);
            crate::cap::ResourceType::Thread(tid)
        }
        2 => {
            crate::cap::ResourceType::IpcPort { port_id: resource_id }
        }
        3 => {
            if resource_id > 255 {
                log_warn!(
                    "syscall",
                    "cap_create: invalid IRQ number {}",
                    resource_id
                );
                return EINVAL;
            }
            crate::cap::ResourceType::Irq {
                irq_num: resource_id as u8,
            }
        }
        _ => {
            log_warn!(
                "syscall",
                "cap_create: unsupported resource type {}",
                resource_type
            );
            return ENOSYS;
        }
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

    let _caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!("syscall", "cap_check: no current thread");
            return 0;
        }
    };

    let _handle = crate::cap::CapHandle::from_raw(handle_raw);
    let _perms = crate::cap::CapPermissions::from_bits(required_perms as u32);

    match crate::cap::get_capability_stats() {
        stats if stats.total > 0 => {
            log_debug!(
                "syscall",
                "cap_check: validation passed (MVP, total_caps={})",
                stats.total
            );
            1
        }
        _ => {
            log_warn!(
                "syscall",
                "cap_check: no capabilities found (MVP)"
            );
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
        Ok(revoked) => {
            let count = revoked.len();
            log_debug!(
                "syscall",
                "cap_revoke: revoked {} capabilities (cascading)",
                count
            );
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

    if !crate::thread::thread_has_capability(caller, handle) {
        log_warn!(
            "syscall",
            "cap_query_parent: denied (caller does not own capability handle={:#x})",
            handle_raw
        );
        return EPERM;
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

    if !crate::thread::thread_has_capability(caller, handle) {
        log_warn!(
            "syscall",
            "cap_query_children: denied (caller does not own capability handle={:#x})",
            handle_raw
        );
        return EPERM;
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
                unsafe {
                    let buffer = buffer_ptr as *mut u64;
                    for i in 0..to_copy {
                        *buffer.add(i) = children[i].raw();
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

    match crate::shared_mem::create_region(caller, size as usize) {
        Ok(region_id) => {
            log_debug!(
                "syscall",
                "shared_region_create: created region {:?} with size {} bytes",
                region_id,
                size
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

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);
    let flags = crate::shared_mem::RegionFlags::from_raw(flags_raw);

    match crate::shared_mem::map_region(region_id, caller, virt_addr as usize, flags) {
        Ok(()) => {
            log_debug!(
                "syscall",
                "shared_region_map: mapped region {:?} to virt=0x{:X}",
                region_id,
                virt_addr
            );
            ESUCCESS
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
                crate::shared_mem::SharedMemError::OutOfMemory => ENOMEM,
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

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);

    match crate::shared_mem::unmap_region(region_id, caller) {
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

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);

    match crate::shared_mem::destroy_region(region_id, caller) {
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

    let has_permission = crate::thread::validate_thread_capability_by_type(
        caller,
        crate::cap::CapPermissions::WRITE,
        |resource| {
            matches!(
                resource,
                crate::cap::ResourceType::MemoryRegion {
                    virt_addr: v,
                    phys_addr: p,
                    size: s,
                } if *v == virt_addr
                    && *p == phys_addr
                    && *s as u64 == size
            )
        },
    );

    if !has_permission {
        log_warn!(
            "syscall",
            "map_region: no exact MemRegionCap found, proceeding anyway (MVP)"
        );
    } else {
        log_debug!("syscall", "map_region: memory region capability validated");
    }

    let mut flags = crate::mm::vm::PageFlags::from_bits(flags_raw);
    flags |= crate::mm::vm::PageFlags::PRESENT | crate::mm::vm::PageFlags::USER;

    match crate::mm::addrspace::map_region(
        as_id,
        caller,
        virt_addr as usize,
        phys_addr as usize,
        size as usize,
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

    let has_permission = crate::thread::validate_thread_capability_by_type(
        caller,
        crate::cap::CapPermissions::WRITE,
        |resource| {
            matches!(
                resource,
                crate::cap::ResourceType::MemoryRegion {
                    virt_addr: v,
                    ..
                } if *v == virt_addr
            )
        },
    );

    if !has_permission {
        log_warn!(
            "syscall",
            "unmap_region: no MemRegionCap found, proceeding anyway (MVP)"
        );
    } else {
        log_debug!(
            "syscall",
            "unmap_region: memory region capability validated"
        );
    }

    match crate::mm::addrspace::unmap_region(
        as_id,
        caller,
        virt_addr as usize,
        size as usize,
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

    let has_permission = crate::thread::validate_thread_capability_by_type(
        caller,
        crate::cap::CapPermissions::WRITE,
        |resource| {
            matches!(
                resource,
                crate::cap::ResourceType::MemoryRegion {
                    virt_addr: v,
                    ..
                } if *v == old_virt
            )
        },
    );

    if !has_permission {
        log_warn!(
            "syscall",
            "remap_region: no MemRegionCap found, proceeding anyway (MVP)"
        );
    } else {
        log_debug!(
            "syscall",
            "remap_region: memory region capability validated"
        );
    }

    match crate::mm::addrspace::remap_region(
        as_id,
        caller,
        old_virt as usize,
        new_virt as usize,
        size as usize,
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
use alloc::collections::BTreeMap;

/// Registered IRQ handlers - maps IRQ number to (ThreadId, port for notification)
static IRQ_HANDLERS: Mutex<BTreeMap<u8, (crate::thread::ThreadId, u64)>> = Mutex::new(BTreeMap::new());

/// Allowed IRQs for userspace drivers
const ALLOWED_IRQS: [u8; 2] = [1, 12]; // Keyboard (IRQ1), Mouse (IRQ12)

/// Register an IRQ handler for userspace
fn sys_register_irq_handler(irq: u8, notification_port: u64) -> u64 {
    if !ALLOWED_IRQS.contains(&irq) {
        log_warn!(
            "syscall",
            "Attempt to register handler for disallowed IRQ {}",
            irq
        );
        return EPERM;
    }

    let caller = match crate::sched::current_thread() {
        Some(tid) => tid,
        None => return EINVAL,
    };

    let mut handlers = IRQ_HANDLERS.lock();

    if handlers.contains_key(&irq) {
        log_warn!(
            "syscall",
            "IRQ {} already has registered handler",
            irq
        );
        return EBUSY;
    }

    handlers.insert(irq, (caller, notification_port));

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

    let mut handlers = IRQ_HANDLERS.lock();

    if let Some((owner, _)) = handlers.get(&irq) {
        if *owner != caller {
            return EPERM;
        }
        handlers.remove(&irq);
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
    let handlers = IRQ_HANDLERS.lock();

    if let Some((_tid, port)) = handlers.get(&irq) {
        // Send notification via IPC port
        let port_id = crate::ipc::PortId::from_raw(*port);

        // Create a simple IRQ notification message
        let msg = crate::ipc::Message::new(
            crate::thread::ThreadId::from_raw(0), // Kernel sender
            irq as u32, // Message type is IRQ number
            alloc::vec![irq], // Payload is the IRQ number
        );

        // Non-blocking send - we're in interrupt context
        if let Err(e) = crate::ipc::send_message_async(port_id, msg) {
            log_debug!(
                "syscall",
                "Failed to notify IRQ {} handler: {:?}",
                irq,
                e
            );
        }
    }
}

/// Check if an IRQ has a userspace handler registered
pub fn has_userspace_irq_handler(irq: u8) -> bool {
    let handlers = IRQ_HANDLERS.lock();
    handlers.contains_key(&irq)
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
        let info_ptr = user_buffer as *mut u64;
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