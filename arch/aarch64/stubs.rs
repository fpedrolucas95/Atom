// Temporary stubs for AArch64 kernel

use crate::thread::CpuContext;

#[no_mangle]
pub extern "C" fn switch_context(_old_context: *mut CpuContext, _new_context: *const CpuContext) {
    // TODO: Implement AArch64 context switch
    loop {}
}

#[no_mangle]
pub extern "C" fn switch_to_context(_new_context: *const CpuContext) -> ! {
    // TODO: Implement AArch64 jump to context
    loop {}
}

#[no_mangle]
pub extern "C" fn enter_user_aarch64(_pc: u64, _sp: u64, _ttbr0: u64) -> ! {
    // TODO: Implement AArch64 user mode entry
    loop {}
}

#[no_mangle]
pub extern "C" fn syscall_entry_aarch64() {
    // TODO: Implement AArch64 syscall entry
    loop {}
}
