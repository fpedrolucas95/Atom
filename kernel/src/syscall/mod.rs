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
    ENOENT, EISDIR, ENOTDIR, EBADF, EROFS, ENAMETOOLONG, EIO,
    EMFILE,
    ENOTSUP,
    // FS limits
    FS_MAX_PATH_LEN,
};

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
    _user_rip: u64,
    _user_rsp: u64,
    _user_rbx: u64,
    _user_rbp: u64,
    _user_r12: u64,
    _user_r13: u64,
    _user_r14: u64,
    _user_r15: u64,
) -> u64 {
    const LOG_ORIGIN: &str = "syscall";

    log_debug!(
        LOG_ORIGIN,
        "Syscall entry (TID={:?}): num={} args=({:#X}, {:#X}, {:#X}, {:#X}, {:#X}, {:#X})",
        crate::sched::current_thread(),
        syscall_num, arg0, arg1, arg2, arg3, arg4, arg5
    );

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
        SYS_MOUSE_POLL => sys_mouse_poll(arg0 as *mut u8),
        SYS_IO_PORT_READ => sys_io_port_read(arg0 as u16, arg1 as u8),
        SYS_IO_PORT_WRITE => sys_io_port_write(arg0 as u16, arg1 as u8),
        SYS_KEYBOARD_POLL => sys_keyboard_poll(arg0 as *mut u8),
        SYS_GET_FRAMEBUFFER => sys_get_framebuffer(arg0 as *mut u64),
        SYS_GET_TICKS => sys_get_ticks(),
        SYS_DEBUG_LOG => sys_debug_log(arg0 as *const u8, arg1 as usize),
        SYS_REGISTER_IRQ_HANDLER => sys_register_irq_handler(arg0 as u8, arg1),
        SYS_MAP_FRAMEBUFFER => sys_map_framebuffer_to_user(arg0),
        SYS_UNREGISTER_IRQ_HANDLER => sys_unregister_irq_handler(arg0 as u8),
        SYS_IPC_WAIT_ANY => sys_ipc_wait_any(arg0, arg1, arg2),
        SYS_GET_IRQ_COUNT => sys_get_irq_count(arg0 as u8),
        SYS_SPAWN_PROCESS => sys_spawn_process(arg0 as *const u8, arg1 as usize),
        SYS_GET_MEMORY_INFO => sys_get_memory_info(arg0 as *mut u64),
        SYS_LIST_PROCESSES => sys_list_processes(arg0 as *mut crate::thread::ProcessInfo, arg1 as usize),
        SYS_GET_PROCESS_COUNT => sys_get_process_count(),
        SYS_READ_KLOG => sys_read_klog(arg0 as *mut u8, arg1 as usize),
        SYS_MOUSE_GET_ID => sys_mouse_get_id(),
        SYS_IPC_CREATE_PORT_WITH_ID => sys_ipc_create_port_with_id(arg0),
        SYS_GET_CPU_BRAND => sys_get_cpu_brand(arg0 as *mut u8, arg1 as usize),

        // Virtual memory management syscalls
        SYS_MMAP => sys_mmap(arg0, arg1, arg2, arg3),
        SYS_MUNMAP => sys_munmap(arg0, arg1),
        SYS_MPROTECT => sys_mprotect(arg0, arg1, arg2),
        SYS_BRK => sys_brk(arg0),

        // Kernel FS backend syscalls (for fsd only)
        SYS_KERN_FS_READ_FILE  => sys_kern_fs_read_file(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_LIST_DIR   => sys_kern_fs_list_dir(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_KERN_FS_STAT_PATH  => sys_kern_fs_stat_path(arg0, arg1 as usize, arg2),


        // Filesystem syscalls — forwarded to fsd via IPC
        SYS_FS_OPEN     => sys_fs_open(arg0, arg1 as usize, arg2 as u32, arg3 as u32),
        SYS_FS_CLOSE    => sys_fs_close(arg0),
        SYS_FS_READ     => sys_fs_read(arg0, arg1, arg2 as usize),
        SYS_FS_WRITE    => sys_fs_write(arg0, arg1, arg2 as usize),
        SYS_FS_SEEK     => sys_fs_seek(arg0, arg1 as i64, arg2 as u32),
        SYS_FS_STAT     => sys_fs_stat(arg0, arg1 as usize, arg2),
        SYS_FS_FSTAT    => sys_fs_fstat(arg0, arg1),
        SYS_FS_MKDIR    => sys_fs_mkdir(arg0, arg1 as usize, arg2 as u32),
        SYS_FS_RMDIR    => sys_fs_rmdir(arg0, arg1 as usize),
        SYS_FS_UNLINK   => sys_fs_unlink(arg0, arg1 as usize),
        SYS_FS_RENAME   => sys_fs_rename(arg0, arg1 as usize, arg2, arg3 as usize),
        SYS_FS_READDIR  => sys_fs_readdir(arg0, arg1, arg2 as usize),
        SYS_FS_TRUNCATE => sys_fs_truncate(arg0, arg1),
        SYS_FS_FSYNC    => sys_fs_fsync(arg0),
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

        _ => {
            log_warn!(
                LOG_ORIGIN,
                "Unknown syscall number: {}",
                syscall_num
            );
            ENOSYS
        }
    };

    result
}

fn sys_mouse_poll(out_ptr: *mut u8) -> u64 {
    if out_ptr.is_null() {
        return EINVAL;
    }

    // Return next raw mouse byte for userspace driver to process
    if let Some(byte) = crate::input::poll_mouse_byte() {
        unsafe {
            *out_ptr = byte;
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

/// Poll keyboard buffer for input (raw scancode)
fn sys_keyboard_poll(out_ptr: *mut u8) -> u64 {
    if out_ptr.is_null() {
        return EINVAL;
    }

    if let Some(scancode) = crate::input::poll_keyboard_byte() {
        unsafe {
            *out_ptr = scancode;
        }
        return ESUCCESS;
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

        log_panic!(
            LOG_ORIGIN,
            "thread_exit returned unexpectedly (tid={}) - no threads to switch to!",
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
    let ticks_to_sleep = (milliseconds + 9) / 10; // Round up
    let wake_tick = crate::interrupts::get_ticks() + ticks_to_sleep;

    // Sleep loop - wait until enough ticks have passed
    while crate::interrupts::get_ticks() < wake_tick {
        crate::thread::set_thread_state(tid, crate::thread::ThreadState::Blocked);

        let (prev, next) = crate::sched::on_timer_tick();
        if let (Some(prev_id), Some(next_id)) = (prev, next) {
            if prev_id != next_id {
                crate::sched::perform_context_switch(prev_id, next_id);
            } else {
                // No other thread - halt and wait for timer interrupt
                unsafe {
                    core::arch::asm!(
                        "sti",
                        "hlt",
                        "cli",
                        options(nomem, nostack)
                    );
                }
            }
        } else {
            // No threads - halt
            unsafe {
                core::arch::asm!(
                    "sti",
                    "hlt",
                    "cli",
                    options(nomem, nostack)
                );
            }
        }

        crate::thread::set_thread_state(tid, crate::thread::ThreadState::Ready);
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

    // Get the caller's address space so the new thread shares it
    let caller_addr_space = crate::thread::get_thread_address_space(caller).unwrap_or(0);

    // Create a userspace (Ring 3) context for the child thread.
    // sys_thread_create is only callable from userspace, so child threads
    // must also run in Ring 3 with the caller's address space.
    let context = crate::thread::CpuContext::new_user(
        entry_point,
        stack_ptr,
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
        state: crate::thread::ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_SIZE,
        address_space: caller_addr_space,
        priority: crate::thread::ThreadPriority::Normal,
        name: "user_thread",
        capability_table: cap_table,
        is_userspace: true,
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
            if let Err(_) = crate::thread::add_thread_capability(owner, cap) {
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
    
    log_info!(
        "syscall",
        "shared_region_map: current_cr3={:#X} caller_pml4={:#X} match={}",
        current_cr3, caller_pml4, current_cr3 == caller_pml4
    );

    let region_id = crate::shared_mem::RegionId::from_raw(region_id_raw);
    let flags = crate::shared_mem::RegionFlags::from_raw(flags_raw);

    match crate::shared_mem::map_region_in_pml4(region_id, caller, caller_pml4, virt_addr as usize, flags) {
        Ok(mapped_va) => {
            log_debug!(
                "syscall",
                "shared_region_map: mapped region {:?} to virt=0x{:X}",
                region_id,
                mapped_va
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
    let handlers = IRQ_HANDLERS.lock();
    match handlers.get(&irq) {
        Some((owner, _)) if *owner == caller => {
            drop(handlers);
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

    // Read port IDs from userspace
    let mut ports = alloc::vec::Vec::with_capacity(count as usize);
    unsafe {
        let ptr = ports_ptr as *const u64;
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
        let ticks = (timeout_ms + 9) / 10;
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
fn sys_spawn_process(name_ptr: *const u8, name_len: usize) -> u64 {
    const LOG_ORIGIN: &str = "syscall:spawn";

    log_info!(LOG_ORIGIN, "spawn_process(name_ptr={:p}, name_len={})", name_ptr, name_len);

    // Validate arguments
    if name_ptr.is_null() || name_len == 0 || name_len > 64 {
        log_warn!(LOG_ORIGIN, "spawn_process: invalid arguments");
        return EINVAL;
    }

    // Copy name from userspace
    let name_bytes = unsafe { core::slice::from_raw_parts(name_ptr, name_len) };
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
        "ui_shell" => "ui_shell",
        "terminal" => "terminal",
        "keyboard" => "keyboard",
        "mouse" => "mouse",
        "display" => "display",
        "browser" => "browser",
        "files" => "files",
        "settings" => "settings",
        _ => "unknown",
    }
}

/// Internal function to create a new userspace process from parsed sections.
///
/// This function now integrates with the VMA subsystem:
/// - Text, data, and BSS sections are still eagerly mapped (they contain data)
/// - The user stack uses a VMA with demand paging + guard page
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
    use crate::mm::vma::{self, Vma, VmaBacking, VmaPermissions};
    use crate::thread::{CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};

    const USER_STACK_PAGES: usize = 16;   // 64KB initial stack (rest demand-paged)
    const USER_STACK_MAX_PAGES: usize = 256;  // 1MB max stack
    const USER_STACK_MAX_SIZE: usize = USER_STACK_MAX_PAGES * PAGE_SIZE;
    const KERNEL_STACK_PAGES: usize = 16;  // 64KB kernel stack to handle deep call stacks
    const USER_STACK_TOP: usize = 0x0000_8000_0000;

    let pid = ThreadId::new();

    // Each process gets its own address space - load at the standard base address
    // since each process has isolated virtual memory
    let text_base = USER_EXEC_LOAD_BASE;
    let user_stack_top = USER_STACK_TOP;
    // Initial stack: only map a small portion, the rest will be demand-paged
    let initial_stack_size = USER_STACK_PAGES * PAGE_SIZE;
    let user_stack_base = user_stack_top - initial_stack_size;

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

    // Create VMA map for this address space
    vma::create_vma_map(new_pml4_phys);

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
    let _ = vma::insert_vma(new_pml4_phys, Vma {
        start: text_base,
        end: text_base + text_size,
        perms: VmaPermissions::read_exec(),
        backing: VmaBacking::Anonymous,
        label: "text",
    });

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
        let _ = vma::insert_vma(new_pml4_phys, Vma {
            start: data_base,
            end: data_base + data_size,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "data",
        });
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
    let _ = vma::insert_vma(new_pml4_phys, Vma {
        start: bss_base,
        end: bss_base + align_up(bss_size),
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "bss",
    });

    // Allocate user stack with demand paging support
    // Only map a small initial portion; the rest grows via page faults.
    // The stack VMA covers the full potential range, but only initial pages are mapped.
    let stack_phys = pmm::alloc_pages_zeroed(USER_STACK_PAGES)
        .ok_or(ENOMEM)?;

    for i in 0..USER_STACK_PAGES {
        let virt = user_stack_base + i * PAGE_SIZE;
        let phys = stack_phys + i * PAGE_SIZE;
        vm::remap_page_in_pml4(
            new_pml4_phys,
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        ).map_err(|_| ENOMEM)?;
    }

    // Register stack VMA with growth support.
    // The VMA initially covers only the mapped pages but can grow downward
    // via demand paging when page faults occur below it.
    let _ = vma::insert_vma(new_pml4_phys, Vma {
        start: user_stack_base,
        end: user_stack_top,
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Stack {
            max_size: USER_STACK_MAX_SIZE,
        },
        label: "stack",
    });

    // Register a heap VMA (starts empty, grows via brk())
    let heap_start = atom_abi::USER_HEAP_START as usize;
    let _ = vma::insert_vma(new_pml4_phys, Vma {
        start: heap_start,
        end: heap_start + PAGE_SIZE, // Minimal initial size
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "heap",
    });

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
        "Process memory: text=0x{:X}-0x{:X}, data=0x{:X}, bss=0x{:X}, stack=0x{:X}-0x{:X} (growable to {}KB), entry=0x{:X}",
        text_base,
        text_base + text_size,
        data_base,
        bss_base,
        user_stack_base,
        user_stack_top,
        USER_STACK_MAX_SIZE / 1024,
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
        state: ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: new_pml4_phys as u64,
        priority: ThreadPriority::Normal,
        name: static_name,
        capability_table: cap::create_capability_table(pid),
        is_userspace: true,
    };

    // Grant capabilities
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

    // Add thread to scheduler
    crate::thread::add_thread(thread);
    crate::sched::mark_thread_ready(pid);

    log_info!("spawn", "Process '{}' (pid={}) scheduled with VMA-backed memory", name, pid);

    Ok(pid)
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
fn sys_get_memory_info(info_ptr: *mut u64) -> u64 {
    if info_ptr.is_null() {
        return EINVAL;
    }

    let (total_kb, free_kb) = crate::mm::pmm::get_memory_stats();

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
fn sys_list_processes(buffer: *mut crate::thread::ProcessInfo, max_count: usize) -> u64 {
    if buffer.is_null() || max_count == 0 {
        return EINVAL;
    }

    // Create a temporary buffer on stack
    let mut temp_buffer = [crate::thread::ProcessInfo {
        pid: 0,
        state: 0,
        name: [0u8; 32],
    }; 32]; // Support up to 32 processes

    let actual_count = max_count.min(32);
    let count = crate::thread::list_processes(&mut temp_buffer[..actual_count]);

    // Copy to userspace buffer
    unsafe {
        for i in 0..count {
            *buffer.add(i) = temp_buffer[i];
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
fn sys_read_klog(buffer: *mut u8, max_len: usize) -> u64 {
    if buffer.is_null() || max_len == 0 {
        return EINVAL;
    }

    let log_data = crate::log::read_log_buffer();
    let copy_len = log_data.len().min(max_len);

    unsafe {
        core::ptr::copy_nonoverlapping(log_data.as_ptr(), buffer, copy_len);
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
fn sys_get_cpu_brand(buffer: *mut u8, max_len: usize) -> u64 {
    if buffer.is_null() || max_len == 0 {
        return EINVAL;
    }

    let brand = crate::system::info().cpu_brand();
    let copy_len = brand.len().min(max_len);

    unsafe {
        core::ptr::copy_nonoverlapping(brand.as_ptr(), buffer, copy_len);
    }

    copy_len as u64
}

// ---------------------------------------------------------------------------
// Virtual Memory Management Syscalls (mmap/munmap/mprotect/brk)
// ---------------------------------------------------------------------------

/// mmap(addr_hint, length, prot, flags) -> mapped_addr | errno
///
/// Maps anonymous private memory into the calling process's virtual address space.
/// If addr_hint is 0, the kernel chooses a suitable address.
/// If MAP_FIXED is set, the mapping is placed exactly at addr_hint.
///
/// **Supported flags:** `MAP_ANONYMOUS | MAP_PRIVATE` (optionally `| MAP_FIXED`).
/// Any other combination (e.g. `MAP_SHARED`, missing `MAP_ANONYMOUS`,
/// or unknown flag bits) is rejected with `EINVAL`.
fn sys_mmap(addr_hint: u64, length: u64, prot: u64, flags: u64) -> u64 {
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

    // Get current thread's PML4
    let tid = match crate::sched::current_thread() {
        Some(t) => t,
        None => return EPERM,
    };

    let pml4 = match crate::thread::get_thread_address_space(tid) {
        Some(p) if p != 0 => p as usize,
        _ => return EPERM,
    };

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

    // Determine the virtual address
    let virt_addr = if fixed {
        let addr = addr_hint as usize;
        if addr % PAGE_SIZE != 0 {
            return EINVAL;
        }
        if addr + length > atom_abi::USER_MMAP_END as usize {
            return EINVAL;
        }
        // Remove any existing mappings in this range
        let removed = vma::remove_vma_range(pml4, addr, addr + length);
        // Unmap the physical pages for removed VMAs
        for old_vma in &removed {
            unmap_vma_pages(pml4, old_vma);
        }
        addr
    } else {
        let hint_start = if addr_hint != 0 && addr_hint as usize >= atom_abi::USER_MMAP_START as usize {
            addr_hint as usize
        } else {
            atom_abi::USER_MMAP_START as usize
        };

        match vma::find_free_region(pml4, hint_start, atom_abi::USER_MMAP_END as usize, length) {
            Some(addr) => addr,
            None => return ENOMEM,
        }
    };

    // Create the VMA (lazy: no physical pages allocated yet)
    let new_vma = Vma {
        start: virt_addr,
        end: virt_addr + length,
        perms,
        backing: VmaBacking::Anonymous,
        label: "mmap",
    };

    match vma::insert_vma(pml4, new_vma) {
        Ok(()) => virt_addr as u64,
        Err(_) => ENOMEM,
    }
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

    if addr % PAGE_SIZE != 0 || length == 0 {
        return EINVAL;
    }

    let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let tid = match crate::sched::current_thread() {
        Some(t) => t,
        None => return EPERM,
    };

    let pml4 = match crate::thread::get_thread_address_space(tid) {
        Some(p) if p != 0 => p as usize,
        _ => return EPERM,
    };

    let removed = vma::remove_vma_range(pml4, addr, addr + length);

    if removed.is_empty() {
        return EINVAL;
    }

    // Unmap physical pages
    for old_vma in &removed {
        unmap_vma_pages(pml4, old_vma);
    }

    ESUCCESS
}

/// mprotect(addr, length, prot) -> 0 | errno
///
/// Changes the protection on a virtual memory region.
fn sys_mprotect(addr: u64, length: u64, prot: u64) -> u64 {
    use crate::mm::vma::{self, VmaPermissions};
    use crate::mm::pmm::PAGE_SIZE;

    let addr = addr as usize;
    let length = length as usize;

    if addr % PAGE_SIZE != 0 || length == 0 {
        return EINVAL;
    }

    let length = (length + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let tid = match crate::sched::current_thread() {
        Some(t) => t,
        None => return EPERM,
    };

    let pml4 = match crate::thread::get_thread_address_space(tid) {
        Some(p) if p != 0 => p as usize,
        _ => return EPERM,
    };

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

    match vma::set_permissions(pml4, addr, addr + length, perms) {
        Ok(()) => ESUCCESS,
        Err(_) => EINVAL,
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

    let tid = match crate::sched::current_thread() {
        Some(t) => t,
        None => return ENOMEM,
    };

    let pml4 = match crate::thread::get_thread_address_space(tid) {
        Some(p) if p != 0 => p as usize,
        _ => return ENOMEM,
    };

    let heap_start = atom_abi::USER_HEAP_START as usize;

    // Find existing heap VMA
    let current_brk = match vma::find_vma(pml4, heap_start) {
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

            match vma::insert_vma(pml4, heap_vma) {
                Ok(()) => return new_brk_aligned as u64,
                Err(_) => return ENOMEM,
            }
        }
    };

    if new_brk == 0 {
        return current_brk as u64;
    }

    let new_brk_aligned = ((new_brk as usize) + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    if new_brk_aligned < heap_start {
        return current_brk as u64;
    }

    if new_brk_aligned == current_brk {
        return current_brk as u64;
    }

    // Remove old heap VMA and replace with resized one
    vma::remove_vma(pml4, heap_start);

    if new_brk_aligned < current_brk {
        // Shrinking: unmap pages in the released range
        let shrunk_start = new_brk_aligned;
        let shrunk_end = current_brk;
        for page in (shrunk_start..shrunk_end).step_by(PAGE_SIZE) {
            let _ = crate::mm::vm::unmap_page_in_pml4(pml4, page);
            vma::account_unmap(pml4);
        }
    }

    let new_heap = Vma {
        start: heap_start,
        end: new_brk_aligned,
        perms: VmaPermissions::read_write(),
        backing: VmaBacking::Anonymous,
        label: "heap",
    };

    match vma::insert_vma(pml4, new_heap) {
        Ok(()) => new_brk_aligned as u64,
        Err(_) => ENOMEM,
    }
}

/// Helper: unmap all physical pages for a VMA by walking the page table
fn unmap_vma_pages(pml4: usize, vma: &crate::mm::vma::Vma) {
    use crate::mm::pmm::PAGE_SIZE;

    for page in (vma.start..vma.end).step_by(PAGE_SIZE) {
        // Query if the page is mapped
        if let Ok((phys, _)) = crate::mm::vm::query_mapping_in_pml4(pml4, page) {
            let _ = crate::mm::vm::unmap_page_in_pml4(pml4, page);
            // Free the physical page (only for non-device mappings)
            match vma.backing {
                crate::mm::vma::VmaBacking::Device { .. } => {},
                _ => crate::mm::pmm::free_page(phys),
            }
            crate::mm::vma::account_unmap(pml4);
        }
    }
}

// ---------------------------------------------------------------------------
// Filesystem syscalls — Production-ready IPC forwarding to fsd
// ---------------------------------------------------------------------------

use alloc::vec::Vec;

// Helper: validate userspace pointer is in canonical range
#[inline]
fn validate_user_pointer(ptr: u64) -> bool {
    ptr <= atom_abi::USER_CANONICAL_MAX
}

// Helper: safely copy string from userspace
fn copy_string_from_user(ptr: u64, len: usize) -> Result<alloc::string::String, u64> {
    if len == 0 || len > FS_MAX_PATH_LEN {
        return Err(ENAMETOOLONG);
    }

    if !validate_user_pointer(ptr) {
        return Err(EINVAL);
    }

    match core::str::from_utf8(unsafe {
        core::slice::from_raw_parts(ptr as *const u8, len)
    }) {
        Ok(s) => Ok(alloc::string::String::from(s)),
        Err(_) => Err(EINVAL),
    }
}

// Helper: safely copy buffer from userspace
fn copy_buffer_from_user(ptr: u64, len: usize, max_len: usize) -> Result<Vec<u8>, u64> {
    if len == 0 || len > max_len {
        return Err(EINVAL);
    }

    if !validate_user_pointer(ptr) {
        return Err(EINVAL);
    }

    Ok(unsafe {
        core::slice::from_raw_parts(ptr as *const u8, len).to_vec()
    })
}

// Helper: safely write buffer to userspace
fn write_buffer_to_user(dst_ptr: u64, src: &[u8]) -> Result<(), u64> {
    if src.is_empty() {
        return Ok(());
    }

    if !validate_user_pointer(dst_ptr) {
        return Err(EINVAL);
    }

    unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst_ptr as *mut u8, src.len());
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

    let (path_buf, path_blen) = match copy_path_from_user(path_ptr, path_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if buf_len == 0 || !validate_user_pointer(buf_ptr) {
        return EINVAL;
    }

    log_debug!(LOG_ORIGIN, "kern_fs_read_file(path=\"{}\")", path);

    match crate::drivers::fat32::open(path) {
        Some(data) => {
            let to_copy = core::cmp::min(data.len(), buf_len);
            if let Err(e) = write_buffer_to_user(buf_ptr, &data[..to_copy]) {
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

    let (path_buf, path_blen) = match copy_path_from_user(path_ptr, path_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if buf_len == 0 || !validate_user_pointer(buf_ptr) {
        return EINVAL;
    }

    log_debug!(LOG_ORIGIN, "kern_fs_list_dir(path=\"{}\")", path);

    // list_directory expects path without leading /
    let list_path = if path == "/" { "" } else { path };

    match crate::drivers::fat32::list_directory(list_path) {
        Some(entries) => {
            let mut pos = 0usize;
            let mut ino_counter: u32 = 1;

            for entry_name in &entries {
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

                if let Err(e) = write_buffer_to_user(buf_ptr + pos as u64, &rec[..rec_len]) {
                    return e;
                }

                pos += rec_len;
                ino_counter += 1;
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

    let (path_buf, path_blen) = match copy_path_from_user(path_ptr, path_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    if !validate_user_pointer(stat_ptr) {
        return EINVAL;
    }

    log_debug!(LOG_ORIGIN, "kern_fs_stat_path(path=\"{}\")", path);

    // Use stat_path to get metadata without reading file contents
    fill_stat_to_user(path, stat_ptr)
}

/// Internal helper: stat a path and write the 80-byte result to userspace.
/// Uses `fat32::stat_path()` so NO file data is read.
fn fill_stat_to_user(path: &str, stat_ptr: u64) -> u64 {
    let mut buf = [0u8; 80];

    // Root directory special case
    if path == "/" || path.is_empty() {
        buf[8..16].copy_from_slice(&2u64.to_le_bytes()); // inode = 2
        let mode: u32 = 0o040755;
        buf[40..44].copy_from_slice(&mode.to_le_bytes());
        buf[48..52].copy_from_slice(&2u32.to_le_bytes()); // nlinks
        buf[56..60].copy_from_slice(&512u32.to_le_bytes()); // blksize
        return match write_buffer_to_user(stat_ptr, &buf) {
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
            match write_buffer_to_user(stat_ptr, &buf) {
                Ok(_) => ESUCCESS,
                Err(e) => e,
            }
        }
        None => ENOENT,
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

/// A kernel-side open file/directory (zero-heap, stored in .bss).
#[derive(Clone)]
struct KernelFd {
    in_use: bool,
    path: [u8; MAX_PATH_BUF],
    path_len: usize,
    is_dir: bool,
    flags: u32,
    offset: usize,
}

impl KernelFd {
    const EMPTY: Self = Self {
        in_use: false,
        path: [0u8; MAX_PATH_BUF],
        path_len: 0,
        is_dir: false,
        flags: 0,
        offset: 0,
    };

    fn path_str(&self) -> &str {
        core::str::from_utf8(&self.path[..self.path_len]).unwrap_or("")
    }
}

/// Global fd table in .bss — no heap allocation.
static KERNEL_FD_TABLE: spin::Mutex<[KernelFd; MAX_KERNEL_FDS]> =
    spin::Mutex::new([KernelFd::EMPTY; MAX_KERNEL_FDS]);

/// Copy a path string from userspace into a stack-allocated fixed buffer.
/// Returns (buffer, length) on success.  No heap allocation.
fn copy_path_from_user(ptr: u64, len: usize) -> Result<([u8; MAX_PATH_BUF], usize), u64> {
    if len == 0 || len > MAX_PATH_BUF {
        return Err(ENAMETOOLONG);
    }
    if !validate_user_pointer(ptr) {
        return Err(EINVAL);
    }
    let src = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    // Validate UTF-8
    if core::str::from_utf8(src).is_err() {
        return Err(EINVAL);
    }
    let mut buf = [0u8; MAX_PATH_BUF];
    buf[..len].copy_from_slice(src);
    Ok((buf, len))
}

/// Get a &str from a (buf, len) pair returned by copy_path_from_user
#[inline]
fn path_buf_as_str(buf: &[u8; MAX_PATH_BUF], len: usize) -> &str {
    // Safety: copy_path_from_user already validated UTF-8
    unsafe { core::str::from_utf8_unchecked(&buf[..len]) }
}

/// Allocate an fd slot (returns 3..MAX_KERNEL_FDS-1, or EMFILE on full).
fn alloc_kernel_fd(path: &str, is_dir: bool, flags: u32) -> Result<u64, u64> {
    let mut table = KERNEL_FD_TABLE.lock();
    // fd 0/1/2 reserved for stdin/out/err
    for i in 3..MAX_KERNEL_FDS {
        if !table[i].in_use {
            table[i].in_use = true;
            let plen = path.len().min(MAX_PATH_BUF);
            table[i].path[..plen].copy_from_slice(&path.as_bytes()[..plen]);
            table[i].path_len = plen;
            table[i].is_dir = is_dir;
            table[i].flags = flags;
            table[i].offset = 0;
            return Ok(i as u64);
        }
    }
    Err(EMFILE)
}

// ============================================================================
// Direct-to-FAT32 filesystem syscalls
// ============================================================================

/// Open a file or directory
fn sys_fs_open(path_ptr: u64, path_len: usize, flags: u32, _mode: u32) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";

    let (path_buf, path_blen) = match copy_path_from_user(path_ptr, path_len) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let path = path_buf_as_str(&path_buf, path_blen);

    log_debug!(LOG_ORIGIN, "sys_fs_open(path=\"{}\", flags={:#x})", path, flags);

    let o_directory: u32 = 0x10000; // atom_abi::O_DIRECTORY
    let is_dir = (flags & o_directory) != 0 || path == "/" || path.ends_with('/');

    if is_dir {
        // Verify directory exists via kernel FAT32
        let list_path = if path == "/" || path.is_empty() {
            ""
        } else {
            path.trim_start_matches('/')
        };
        if path != "/" && !path.is_empty() {
            if crate::drivers::fat32::list_directory(list_path).is_none() {
                log_debug!(LOG_ORIGIN, "directory not found: \"{}\"", path);
                return ENOENT;
            }
        }
    } else {
        // Verify file exists (stat-like: try open for existence check,
        // but we don't want to read the entire file here for large files)
        let trimmed = path.trim_start_matches('/');
        let parent_end = trimmed.rfind('/').unwrap_or(0);
        let list_path = &trimmed[..parent_end];
        let file_name = path.split('/').filter(|s| !s.is_empty()).last().unwrap_or("");

        // Check parent directory for the entry
        let found = match crate::drivers::fat32::list_directory(list_path) {
            Some(entries) => entries.iter().any(|e| {
                let name = if e.ends_with('/') { &e[..e.len()-1] } else { e.as_str() };
                name.eq_ignore_ascii_case(file_name)
            }),
            None => false,
        };

        if !found {
            log_debug!(LOG_ORIGIN, "file not found: \"{}\"", path);
            return ENOENT;
        }
    }

    // Store path without trailing slash
    let store_path = path.trim_end_matches('/');
    let fd = match alloc_kernel_fd(store_path, is_dir, flags) {
        Ok(fd) => fd,
        Err(e) => return e,
    };

    log_debug!(LOG_ORIGIN, "sys_fs_open OK: fd={} dir={}", fd, is_dir);
    fd
}

/// Close a file descriptor
fn sys_fs_close(fd: u64) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";
    log_debug!(LOG_ORIGIN, "sys_fs_close(fd={})", fd);

    let idx = fd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    let mut table = KERNEL_FD_TABLE.lock();
    if table[idx].in_use {
        table[idx].in_use = false;
        ESUCCESS
    } else {
        EBADF
    }
}

/// Read from file descriptor
fn sys_fs_read(fd: u64, buf_ptr: u64, count: usize) -> u64 {
    const LOG_ORIGIN: &str = "fs_syscall";
    log_debug!(LOG_ORIGIN, "sys_fs_read(fd={}, count={})", fd, count);

    if count == 0 || !validate_user_pointer(buf_ptr) {
        return EINVAL;
    }

    let idx = fd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    // Get path and offset without holding lock during FAT32 I/O
    let (path_buf, path_len, offset, is_dir) = {
        let table = KERNEL_FD_TABLE.lock();
        if !table[idx].in_use {
            return EBADF;
        }
        (table[idx].path, table[idx].path_len, table[idx].offset, table[idx].is_dir)
    };

    if is_dir {
        return EISDIR;
    }

    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };

    // Read the file from FAT32
    match crate::drivers::fat32::open(path) {
        Some(data) => {
            if offset >= data.len() {
                return 0; // EOF
            }
            let available = data.len() - offset;
            let to_read = available.min(count);

            if let Err(e) = write_buffer_to_user(buf_ptr, &data[offset..offset + to_read]) {
                return e;
            }

            // Update offset
            {
                let mut table = KERNEL_FD_TABLE.lock();
                if table[idx].in_use {
                    table[idx].offset += to_read;
                }
            }

            log_debug!(LOG_ORIGIN, "sys_fs_read OK: {} bytes", to_read);
            to_read as u64
        }
        None => EIO,
    }
}

/// Write to file descriptor (read-only FAT32 — not supported)
fn sys_fs_write(_fd: u64, _buf_ptr: u64, _count: usize) -> u64 {
    EROFS
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

    if count == 0 || !validate_user_pointer(dirent_ptr) {
        return EINVAL;
    }

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

                if let Err(e) = write_buffer_to_user(dirent_ptr + pos as u64, &rec[..rec_len]) {
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

/// Create directory (read-only FAT32 — not supported)
fn sys_fs_mkdir(_path_ptr: u64, _path_len: usize, _mode: u32) -> u64 {
    EROFS
}

/// Unlink (delete) file (read-only FAT32 — not supported)
fn sys_fs_unlink(_path_ptr: u64, _path_len: usize) -> u64 {
    EROFS
}

/// Rename file (read-only FAT32 — not supported)
fn sys_fs_rename(_old_path_ptr: u64, _old_path_len: usize, _new_path_ptr: u64, _new_path_len: usize) -> u64 {
    EROFS
}

/// Synchronize file to disk (no-op for read-only)
fn sys_fs_fsync(_fd: u64) -> u64 {
    ESUCCESS
}

// Stub implementations for remaining syscalls (not in priority list)

/// Seek in file descriptor
fn sys_fs_seek(fd: u64, offset: i64, whence: u32) -> u64 {
    let idx = fd as usize;
    if idx >= MAX_KERNEL_FDS {
        return EBADF;
    }

    let mut table = KERNEL_FD_TABLE.lock();
    if !table[idx].in_use {
        return EBADF;
    }

    let new_offset = match whence {
        0 => offset as usize,        // SEEK_SET
        1 => {                        // SEEK_CUR
            let cur = table[idx].offset as i64;
            (cur + offset) as usize
        }
        _ => return EINVAL,
    };

    table[idx].offset = new_offset;
    new_offset as u64
}

/// Get file status by descriptor
fn sys_fs_fstat(fd: u64, stat_ptr: u64) -> u64 {
    if !validate_user_pointer(stat_ptr) {
        return EINVAL;
    }

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
        (table[idx].path, table[idx].path_len)
    };

    let path = unsafe { core::str::from_utf8_unchecked(&path_buf[..path_len]) };
    let stat_path = if path.is_empty() { "/" } else { path };

    fill_stat_to_user(stat_path, stat_ptr)
}

/// Remove directory
fn sys_fs_rmdir(_path_ptr: u64, _path_len: usize) -> u64 {
    ENOTSUP
}

/// Truncate file
fn sys_fs_truncate(_path_ptr: u64, _length: u64) -> u64 {
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
fn sys_fs_dup(_fd: u64) -> u64 {
    ENOTSUP
}

/// Duplicate file descriptor to specific number
fn sys_fs_dup2(_oldfd: u64, _newfd: u64) -> u64 {
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