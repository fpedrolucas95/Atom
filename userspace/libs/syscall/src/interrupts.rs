// Interrupt handling syscalls

use crate::raw::{syscall2, numbers::*};
use crate::error::{ESUCCESS, SyscallError, SyscallResult};
use crate::ipc::PortId;

/// Register an IPC port to receive notifications for a hardware IRQ.
///
/// IRQ numbers allowed are typically 1 (keyboard) and 12 (mouse).
/// Notifications are sent as IPC messages with type = IRQ number.
pub fn register_irq_handler(irq: u8, port: PortId) -> SyscallResult<()> {
    let result = unsafe { syscall2(SYS_REGISTER_IRQ_HANDLER, irq as u64, port) };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::PermissionDenied)
    }
}

/// Unregister a previously registered IRQ handler.
pub fn unregister_irq_handler(irq: u8) -> SyscallResult<()> {
    use crate::raw::syscall1;
    let result = unsafe { syscall1(SYS_UNREGISTER_IRQ_HANDLER, irq as u64) };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(SyscallError::InvalidArgument)
    }
}
