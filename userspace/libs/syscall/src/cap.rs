//! Capability Syscalls

use crate::error::{SyscallResult};
use crate::raw::{syscall2, numbers::*};

pub type CapHandle = u64;

pub fn check(handle: CapHandle, perms: u64) -> u64 {
    unsafe { syscall2(SYS_CAP_CHECK, handle, perms) }
}
