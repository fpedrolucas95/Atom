use crate::error::{EINVAL, ENOMEM, SyscallError, SyscallResult};
use crate::raw::{numbers::*, syscall2, syscall3, syscall4};
use atom_abi::{has_suspicious_user_high_bits, is_canonical, is_syscall_error, is_user_mmap_addr, is_valid_user_va, UserVirtAddr};

#[inline]
fn validate_kernel_mapped_addr(addr: UserVirtAddr) -> SyscallResult<UserVirtAddr> {
    if !is_canonical(addr) {
        return Err(SyscallError::Unknown(addr));
    }

    if !is_valid_user_va(addr) {
        return Err(SyscallError::Unknown(addr));
    }

    if !is_user_mmap_addr(addr) {
        return Err(SyscallError::Unknown(addr));
    }

    if has_suspicious_user_high_bits(addr) {
        return Err(SyscallError::Unknown(addr));
    }

    Ok(addr)
}

/// Map anonymous private memory and return the exact userspace virtual address
/// chosen by the kernel.
pub fn mmap_anon(length: usize, prot: u64, flags: u64) -> SyscallResult<UserVirtAddr> {
    let result = unsafe { syscall4(SYS_MMAP, 0, length as u64, prot, flags) };

    if is_syscall_error(result) {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        validate_kernel_mapped_addr(result)
    }
}

pub fn munmap(addr: UserVirtAddr, length: usize) -> SyscallResult<()> {
    let result = unsafe { syscall2(SYS_MUNMAP, addr, length as u64) };
    if result == 0 {
        Ok(())
    } else {
        Err(SyscallError::from_raw(result))
    }
}

pub fn mprotect(addr: UserVirtAddr, length: usize, prot: u64) -> SyscallResult<()> {
    let result = unsafe { syscall3(SYS_MPROTECT, addr, length as u64, prot) };
    if result == 0 {
        Ok(())
    } else {
        Err(SyscallError::from_raw(result))
    }
}
