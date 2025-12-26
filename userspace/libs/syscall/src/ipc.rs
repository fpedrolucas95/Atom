// IPC (Inter-Process Communication) syscalls

use crate::error::{ESUCCESS, EPERM, EINVAL, EWOULDBLOCK, SyscallError, SyscallResult};
use crate::raw::{syscall0, syscall1, syscall2, syscall3, numbers::*};

/// Port identifier
pub type PortId = u64;

/// Create a new IPC port
///
/// Returns the port ID on success.
pub fn create_port() -> SyscallResult<PortId> {
    let result = unsafe { syscall0(SYS_IPC_CREATE_PORT) };

    if result == 0 || result >= u64::MAX - 10 {
        Err(SyscallError::OutOfMemory)
    } else {
        Ok(result)
    }
}

/// Close an IPC port
pub fn close_port(port: PortId) -> SyscallResult<()> {
    let result = unsafe { syscall1(SYS_IPC_CLOSE_PORT, port) };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

/// Send a message to a port
///
/// Blocks until the message is delivered.
pub fn send(port: PortId, data: &[u8]) -> SyscallResult<()> {
    let result = unsafe {
        syscall3(SYS_IPC_SEND, port, data.as_ptr() as u64, data.len() as u64)
    };

    if result == ESUCCESS {
        Ok(())
    } else if result == EPERM {
        Err(SyscallError::PermissionDenied)
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

/// Receive a message from a port
///
/// Blocks until a message is available.
/// Returns the number of bytes received.
pub fn recv(port: PortId, buffer: &mut [u8]) -> SyscallResult<usize> {
    let result = unsafe {
        syscall3(SYS_IPC_RECV, port, buffer.as_mut_ptr() as u64, buffer.len() as u64)
    };

    if result >= u64::MAX - 10 {
        if result == EWOULDBLOCK {
            Err(SyscallError::WouldBlock)
        } else {
            Err(SyscallError::InvalidArgument)
        }
    } else {
        Ok(result as usize)
    }
}

/// Try to receive a message without blocking
///
/// Returns None if no message is available.
pub fn try_recv(port: PortId, buffer: &mut [u8]) -> SyscallResult<Option<usize>> {
    let result = unsafe {
        syscall3(SYS_IPC_TRY_RECV, port, buffer.as_mut_ptr() as u64, buffer.len() as u64)
    };

    if result == EWOULDBLOCK {
        Ok(None)
    } else if result >= u64::MAX - 10 {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(Some(result as usize))
    }
}

/// Send a message asynchronously
///
/// Returns immediately without waiting for delivery.
pub fn send_async(port: PortId, data: &[u8]) -> SyscallResult<()> {
    let result = unsafe {
        syscall3(SYS_IPC_SEND_ASYNC, port, data.as_ptr() as u64, data.len() as u64)
    };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

/// Wait for any of multiple ports to have data
///
/// Blocks until one of the ports has a message available.
/// Returns the index of the port with data.
pub fn wait_any(ports: &[PortId], timeout_ms: u64) -> SyscallResult<usize> {
    use crate::raw::numbers::SYS_IPC_WAIT_ANY;

    if ports.is_empty() || ports.len() > 64 {
        return Err(SyscallError::InvalidArgument);
    }

    let result = unsafe {
        crate::raw::syscall3(
            SYS_IPC_WAIT_ANY,
            ports.as_ptr() as u64,
            ports.len() as u64,
            timeout_ms,
        )
    };

    if result < ports.len() as u64 {
        Ok(result as usize)
    } else if result == EWOULDBLOCK {
        Err(SyscallError::WouldBlock)
    } else if result == crate::error::ETIMEDOUT {
        Err(SyscallError::TimedOut)
    } else {
        Err(SyscallError::InvalidArgument)
    }
}
