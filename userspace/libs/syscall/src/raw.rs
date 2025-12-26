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
}

/// Raw syscall with no arguments
#[inline(always)]
pub unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        in("rsi") arg1,
        in("rdx") arg2,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        in("rsi") arg1,
        in("rdx") arg2,
        in("r10") arg3,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        in("rsi") arg1,
        in("rdx") arg2,
        in("r10") arg3,
        in("r8") arg4,
        out("rcx") _,
        out("r11") _,
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
        in("rdi") arg0,
        in("rsi") arg1,
        in("rdx") arg2,
        in("r10") arg3,
        in("r8") arg4,
        in("r9") arg5,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}
