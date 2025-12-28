// IPC (Inter-Process Communication) syscalls

use crate::error::{ESUCCESS, EPERM, EINVAL, EWOULDBLOCK, EMSGSIZE, ETIMEDOUT, SyscallError, SyscallResult};
use crate::raw::{syscall0, syscall1, syscall4, numbers::*};

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

/// Send a message to a port (asynchronously with data payload)
///
/// Uses the async syscall which properly transfers data.
pub fn send(port: PortId, data: &[u8]) -> SyscallResult<()> {
    // Use send_async syscall which properly handles data payload
    // Arguments: port, msg_type (0 = raw data), payload_ptr, payload_len
    let result = unsafe {
        syscall4(SYS_IPC_SEND_ASYNC, port, 0, data.as_ptr() as u64, data.len() as u64)
    };

    match result {
        x if x == ESUCCESS => Ok(()),
        x if x == EPERM => Err(SyscallError::PermissionDenied),
        x if x == EINVAL => Err(SyscallError::InvalidArgument),
        x if x == EMSGSIZE => Err(SyscallError::MessageTooLarge),
        _ => Err(SyscallError::Unknown(result)),
    }
}

/// Receive a message from a port
///
/// Blocks until a message is available.
/// Returns the number of bytes received.
pub fn recv(port: PortId, buffer: &mut [u8]) -> SyscallResult<usize> {
    // Arguments: port, buffer_ptr, buffer_len, timeout_ms (0 = block forever)
    let result = unsafe {
        syscall4(SYS_IPC_RECV, port, buffer.as_mut_ptr() as u64, buffer.len() as u64, 0)
    };

    if result >= u64::MAX - 10 {
        if result == EWOULDBLOCK {
            Err(SyscallError::WouldBlock)
        } else if result == ETIMEDOUT {
            Err(SyscallError::TimedOut)
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
    use crate::raw::syscall3;
    // Arguments: port, buffer_ptr, buffer_len (non-blocking - no timeout arg)
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
    // Arguments: port, msg_type (0 = raw data), payload_ptr, payload_len
    let result = unsafe {
        syscall4(SYS_IPC_SEND_ASYNC, port, 0, data.as_ptr() as u64, data.len() as u64)
    };

    match result {
        x if x == ESUCCESS => Ok(()),
        x if x == EINVAL => Err(SyscallError::InvalidArgument),
        x if x == EMSGSIZE => Err(SyscallError::MessageTooLarge),
        _ => Err(SyscallError::Unknown(result)),
    }
}

/// Wait for any of multiple ports to have data
///
/// Blocks until one of the ports has a message available.
/// Returns the index of the port with data.
pub fn wait_any(ports: &[PortId], timeout_ms: u64) -> SyscallResult<usize> {
    use crate::raw::syscall3;
    use crate::raw::numbers::SYS_IPC_WAIT_ANY;

    if ports.is_empty() || ports.len() > 64 {
        return Err(SyscallError::InvalidArgument);
    }

    let result = unsafe {
        syscall3(
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
    } else if result == ETIMEDOUT {
        Err(SyscallError::TimedOut)
    } else {
        Err(SyscallError::InvalidArgument)
    }
}
