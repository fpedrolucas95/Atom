// IPC (Inter-Process Communication) syscalls

use crate::error::{ESUCCESS, EPERM, EINVAL, ENOTFOUND, EWOULDBLOCK, EMSGSIZE, ETIMEDOUT, SyscallError, SyscallResult};
use crate::raw::{syscall0, syscall1, syscall4, numbers::*};

/// Port identifier
pub type PortId = u64;

/// Create a new IPC port
///
/// Returns the port ID on success.
pub fn create_port() -> SyscallResult<PortId> {
    let result = unsafe { syscall0(SYS_IPC_CREATE_PORT) };

    if result == 0 || crate::error::is_syscall_error(result) {
        Err(SyscallError::OutOfMemory)
    } else {
        Ok(result)
    }
}

/// Create an IPC port with a specific reserved ID (1-255).
///
/// Used by well-known system services (e.g., namesvc on Port 1) to get
/// deterministic, pre-assigned port IDs instead of sequentially allocated ones.
pub fn create_port_with_id(id: PortId) -> SyscallResult<PortId> {
    use crate::raw::{syscall1, numbers::SYS_IPC_CREATE_PORT_WITH_ID};
    let result = unsafe { syscall1(SYS_IPC_CREATE_PORT_WITH_ID, id) };

    if result == 0 || crate::error::is_syscall_error(result) {
        Err(SyscallError::InvalidArgument)
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
        x if x == ENOTFOUND => Err(SyscallError::NotFound),
        x if x == EMSGSIZE => Err(SyscallError::MessageTooLarge),
        x if x == EWOULDBLOCK => Err(SyscallError::WouldBlock),
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

    if crate::error::is_syscall_error(result) {
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

/// Receive a message from a port with timeout
///
/// Waits up to timeout_ms milliseconds for a message.
/// Returns the number of bytes received.
pub fn recv_with_timeout(port: PortId, buffer: &mut [u8], timeout_ms: u64) -> SyscallResult<usize> {
    // Arguments: port, buffer_ptr, buffer_len, timeout_ms
    let result = unsafe {
        syscall4(SYS_IPC_RECV, port, buffer.as_mut_ptr() as u64, buffer.len() as u64, timeout_ms)
    };

    if crate::error::is_syscall_error(result) {
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
    } else if crate::error::is_syscall_error(result) {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(Some(result as usize))
    }
}

/// Kernel-generated metadata describing the sender of a received message.
/// Re-exported from the shared ABI so receivers don't trust self-reported
/// identity in the payload.
pub use atom_abi::IpcEnvelope;

/// Receive a message together with its kernel-generated [`IpcEnvelope`].
///
/// Non-blocking. On success returns the number of payload bytes copied into
/// `buffer` and fills `envelope` with the authenticated sender identity. The
/// envelope's `sender_process`, `sender_thread`, `sender_service_id` and
/// `transferred_capability` come straight from kernel state and cannot be
/// forged by the sender. Returns `Ok(None)` when no message is queued.
pub fn recv_envelope(
    port: PortId,
    envelope: &mut IpcEnvelope,
    buffer: &mut [u8],
) -> SyscallResult<Option<usize>> {
    let result = unsafe {
        syscall4(
            SYS_IPC_RECV_ENVELOPE,
            port,
            envelope as *mut IpcEnvelope as u64,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64,
        )
    };

    if result == EWOULDBLOCK {
        Ok(None)
    } else if crate::error::is_syscall_error(result) {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(Some(result as usize))
    }
}

/// Ask the kernel whether `name` is the canonical name or a declared alias for
/// `service_id`, per the SystemServiceManifest. Authoritative allowlist used by
/// namesvc to authorise a registration request.
pub fn service_name_allowed(service_id: u64, name: &str) -> bool {
    use crate::raw::syscall3;
    let result = unsafe {
        syscall3(
            SYS_SERVICE_NAME_ALLOWED,
            service_id,
            name.as_ptr() as u64,
            name.len() as u64,
        )
    };
    result == 1
}

/// Return the process id that owns `port`, or 0 if the port has no owner.
/// Used to verify that a service announcing a port actually owns it.
pub fn port_owner(port: PortId) -> u64 {
    unsafe { syscall1(SYS_IPC_PORT_OWNER, port) }
}

/// Return `true` if process `pid` exists and has not terminated. Used to
/// confirm a previous registration owner is gone before allowing replacement.
pub fn process_alive(pid: u64) -> bool {
    let result = unsafe { syscall1(SYS_PROCESS_ALIVE, pid) };
    result == 1
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
        x if x == EPERM => Err(SyscallError::PermissionDenied),
        x if x == EINVAL => Err(SyscallError::InvalidArgument),
        x if x == ENOTFOUND => Err(SyscallError::NotFound),
        x if x == EMSGSIZE => Err(SyscallError::MessageTooLarge),
        x if x == EWOULDBLOCK => Err(SyscallError::WouldBlock),
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
