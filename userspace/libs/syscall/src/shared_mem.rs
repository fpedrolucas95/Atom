//! Shared Memory Syscalls

use crate::error::{SyscallError, SyscallResult, EBUSY, EINVAL, ENOMEM, EPERM, ESUCCESS};
use crate::raw::{numbers::*, syscall1, syscall3};

pub type RegionId = u64;

bitflags::bitflags! {
    pub struct RegionFlags: u64 {
        const READ = 1 << 0;
        const WRITE = 1 << 1;
        const EXECUTE = 1 << 2;
    }
}

pub fn create_region(size: usize) -> SyscallResult<RegionId> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_CREATE, size as u64) };
    if result >= atom_abi::SYSCALL_ERROR_THRESHOLD {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        Ok(result)
    }
}

pub fn map_region(id: RegionId, addr: u64, flags: RegionFlags) -> SyscallResult<u64> {
    let result = unsafe { syscall3(SYS_SHARED_REGION_MAP, id, addr, flags.bits()) };
    if result >= atom_abi::SYSCALL_ERROR_THRESHOLD {
        match result {
            x if x == EINVAL => Err(SyscallError::InvalidArgument),
            x if x == EBUSY => Err(SyscallError::WouldBlock),
            x if x == ENOMEM => Err(SyscallError::OutOfMemory),
            _ => Err(SyscallError::Unknown(result)),
        }
    } else {
        Ok(result)
    }
}

pub fn unmap_region(id: RegionId) -> SyscallResult<()> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_UNMAP, id) };
    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

pub fn destroy_region(id: RegionId) -> SyscallResult<()> {
    let result = unsafe { syscall1(SYS_SHARED_REGION_DESTROY, id) };
    if result == ESUCCESS {
        Ok(())
    } else if result == EPERM {
        Err(SyscallError::PermissionDenied)
    } else if result == EBUSY {
        Err(SyscallError::WouldBlock)
    } else {
        Err(SyscallError::InvalidArgument)
    }
}
