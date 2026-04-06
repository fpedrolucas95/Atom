//! Capability Syscalls

use crate::raw::{syscall2, numbers::*};

pub type CapHandle = u64;

pub fn check(handle: CapHandle, perms: u64) -> u64 {
    unsafe { syscall2(SYS_CAP_CHECK, handle, perms) }
}

/// List capability handles owned by the current process.
/// Returns the number of handles written into `buf`.
pub fn list(buf: &mut [u64]) -> usize {
    let count = unsafe {
        syscall2(SYS_CAP_LIST, buf.as_mut_ptr() as u64, (buf.len() * 8) as u64)
    };
    count as usize
}
