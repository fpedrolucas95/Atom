// Raw syscall invocation primitives
//
// These functions provide direct access to the syscall instruction.
// They are unsafe because:
// - The caller must ensure syscall numbers and arguments are valid
// - Invalid syscalls may cause undefined behavior

/// Syscall numbers (must match kernel/src/syscall/mod.rs)
pub mod numbers {
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
    pub const SYS_IPC_WAIT_ANY: u64 = 43;
    pub const SYS_GET_IRQ_COUNT: u64 = 44;
    pub const SYS_SPAWN_PROCESS: u64 = 45;
    pub const SYS_GET_MEMORY_INFO: u64 = 46;
    pub const SYS_LIST_PROCESSES: u64 = 47;
    pub const SYS_GET_PROCESS_COUNT: u64 = 48;
    pub const SYS_READ_KLOG: u64 = 49;
    pub const SYS_MOUSE_GET_ID: u64 = 50;
    pub const SYS_IPC_CREATE_PORT_WITH_ID: u64 = 51;
    pub const SYS_GET_CPU_BRAND: u64 = 52;
    pub const SYS_GET_CPU_ID: u64 = 90;
    pub const SYS_GET_CPU_COUNT: u64 = 91;
    pub const SYS_SET_THREAD_AFFINITY: u64 = 92;
    pub const SYS_GET_THREAD_AFFINITY: u64 = 93;

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
    pub const SYS_PCI_QUERY_DEVICE:       u64 = 89;

    // Filesystem syscalls (53-76)
    pub const SYS_FS_OPEN:     u64 = 53;
    pub const SYS_FS_CLOSE:    u64 = 54;
    pub const SYS_FS_READ:     u64 = 55;
    pub const SYS_FS_WRITE:    u64 = 56;
    pub const SYS_FS_SEEK:     u64 = 57;
    pub const SYS_FS_STAT:     u64 = 58;
    pub const SYS_FS_FSTAT:    u64 = 59;
    pub const SYS_FS_MKDIR:    u64 = 60;
    pub const SYS_FS_RMDIR:    u64 = 61;
    pub const SYS_FS_UNLINK:   u64 = 62;
    pub const SYS_FS_RENAME:   u64 = 63;
    pub const SYS_FS_READDIR:  u64 = 64;
    pub const SYS_FS_TRUNCATE: u64 = 65;
    pub const SYS_FS_FSYNC:    u64 = 66;
    pub const SYS_FS_MOUNT:    u64 = 67;
    pub const SYS_FS_UMOUNT:   u64 = 68;
    pub const SYS_FS_CHMOD:    u64 = 69;
    pub const SYS_FS_DUP:      u64 = 70;
    pub const SYS_FS_DUP2:     u64 = 71;
    pub const SYS_FS_LINK:     u64 = 72;
    pub const SYS_FS_SYMLINK:  u64 = 73;
    pub const SYS_FS_READLINK: u64 = 74;
    pub const SYS_FS_UTIMES:   u64 = 75;
    pub const SYS_FS_STATVFS:  u64 = 76;

    // Kernel FS backend syscalls (used by fsd to access kernel FAT32 driver)
    pub const SYS_KERN_FS_READ_FILE: u64 = 200;
    pub const SYS_KERN_FS_LIST_DIR:  u64 = 201;
    pub const SYS_KERN_FS_STAT_PATH: u64 = 202;
    pub const SYS_KERN_FS_WRITE_FILE: u64 = 204;
    pub const SYS_KERN_FS_MKDIR:      u64 = 205;
    pub const SYS_KERN_FS_RMDIR:      u64 = 206;
    pub const SYS_KERN_FS_UNLINK:     u64 = 207;
    pub const SYS_KERN_FS_RENAME:     u64 = 208;
    pub const SYS_KERN_FS_SYNC:       u64 = 209;
    pub const SYS_KERN_BLOCK_READ:    u64 = 210;
    pub const SYS_KERN_BLOCK_WRITE:   u64 = 211;
    pub const SYS_KERN_BLOCK_FLUSH:   u64 = 212;

    // App launcher — spawn ATXF by absolute filesystem path.
    // Intended for use by the privileged app_launcher service only; unprivileged
    // applications must request launches through the app_launcher IPC service.
    pub const SYS_SPAWN_FROM_PATH: u64 = 203;

    // ---------------------------------------------------------------------------
    // Video mode management syscalls (BGA/VBE_DISPI)
    // ---------------------------------------------------------------------------

    /// Set video mode: packed_res=(height<<16|width), bpp -> ESUCCESS | errno
    pub const SYS_SET_VIDEO_MODE:         u64 = 77;
    /// Get available modes into u32 buffer: (buf_ptr, max_count) -> count | errno
    pub const SYS_GET_VIDEO_MODES:        u64 = 78;
    /// Get current video mode info into u64 buffer: (buf_ptr) -> ESUCCESS | errno
    pub const SYS_GET_CURRENT_VIDEO_MODE: u64 = 79;
    /// Get total count of available modes: () -> count
    pub const SYS_VIDEO_MODE_COUNT:       u64 = 80;

    // Virtual memory management syscalls
    pub const SYS_MMAP:      u64 = 100;
    pub const SYS_MUNMAP:    u64 = 101;
    pub const SYS_MPROTECT:  u64 = 102;
    pub const SYS_BRK:       u64 = 103;
    pub const SYS_FORK:      u64 = 104;

    // Authenticated IPC (PR2)
    /// recv_envelope(port, env_ptr, buf_ptr, buf_len) -> payload_len | errno
    pub const SYS_IPC_RECV_ENVELOPE:    u64 = 105;
    /// service_name_allowed(service_id, name_ptr, name_len) -> 1 | 0
    pub const SYS_SERVICE_NAME_ALLOWED: u64 = 106;
    /// ipc_port_owner(port_id) -> owner_process_raw | 0
    pub const SYS_IPC_PORT_OWNER:       u64 = 107;
    /// process_alive(pid) -> 1 | 0
    pub const SYS_PROCESS_ALIVE:        u64 = 108;
}

pub fn pci_get_bar(dev_cap: u64, index: u8, info: &mut atom_abi::PciBarInfo) -> u64 {
    unsafe { syscall3(numbers::SYS_PCI_GET_BAR, dev_cap, index as u64, info as *mut _ as u64) }
}

pub fn pci_query_device(dev_cap: u64, info: &mut atom_abi::PciDeviceInfo) -> u64 {
    unsafe { syscall2(numbers::SYS_PCI_QUERY_DEVICE, dev_cap, info as *mut _ as u64) }
}

pub fn device_bind_irq(dev_cap: u64, info: &mut atom_abi::IrqBindInfo) -> u64 {
    unsafe { syscall2(numbers::SYS_DEVICE_BIND_IRQ, dev_cap, info as *mut _ as u64) }
}

pub fn irq_listen(irq_cap: u64, port: u64) -> u64 {
    unsafe { syscall2(numbers::SYS_IRQ_LISTEN, irq_cap, port) }
}

pub fn dma_alloc(params: &atom_abi::DmaAllocParams, info: &mut atom_abi::DmaMappingInfo) -> u64 {
    unsafe { syscall2(numbers::SYS_DMA_ALLOC, params as *const _ as u64, info as *mut _ as u64) }
}

pub fn map_mmio(mmio_cap: u64, out_va: &mut u64) -> u64 {
    unsafe { syscall2(numbers::SYS_MAP_MMIO, mmio_cap, out_va as *mut _ as u64) }
}

pub fn irq_ack(irq_cap: u64) -> u64 {
    unsafe { syscall1(numbers::SYS_IRQ_ACK, irq_cap) }
}

/// Raw syscall with no arguments
///
/// Note: The kernel's syscall handler clobbers all caller-saved registers,
/// not just rcx and r11. We must declare all of them as clobbered.
#[inline(always)]
pub unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
        lateout("rdi") _,
        lateout("rsi") _,
        lateout("rdx") _,
        lateout("r8") _,
        lateout("r9") _,
        lateout("r10") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 1 argument
#[inline(always)]
pub unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        out("rcx") _,
        out("r11") _,
        lateout("rsi") _,
        lateout("rdx") _,
        lateout("r8") _,
        lateout("r9") _,
        lateout("r10") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 2 arguments
#[inline(always)]
pub unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        out("rcx") _,
        out("r11") _,
        lateout("rdx") _,
        lateout("r8") _,
        lateout("r9") _,
        lateout("r10") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 3 arguments
#[inline(always)]
pub unsafe fn syscall3(num: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        inlateout("rdx") arg2 => _,
        out("rcx") _,
        out("r11") _,
        lateout("r8") _,
        lateout("r9") _,
        lateout("r10") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 4 arguments
#[inline(always)]
pub unsafe fn syscall4(num: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        inlateout("rdx") arg2 => _,
        inlateout("r10") arg3 => _,
        out("rcx") _,
        out("r11") _,
        lateout("r8") _,
        lateout("r9") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 5 arguments
#[inline(always)]
pub unsafe fn syscall5(num: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        inlateout("rdx") arg2 => _,
        inlateout("r10") arg3 => _,
        inlateout("r8") arg4 => _,
        out("rcx") _,
        out("r11") _,
        lateout("r9") _,
        options(nostack, preserves_flags)
    );
    result
}

/// Raw syscall with 6 arguments
#[inline(always)]
pub unsafe fn syscall6(num: u64, arg0: u64, arg1: u64, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        inlateout("rdi") arg0 => _,
        inlateout("rsi") arg1 => _,
        inlateout("rdx") arg2 => _,
        inlateout("r10") arg3 => _,
        inlateout("r8") arg4 => _,
        inlateout("r9") arg5 => _,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}
