// I/O port access syscalls
//
// These syscalls allow userspace drivers to access hardware I/O ports.
// Access is controlled by the kernel's capability system - only authorized
// ports can be accessed.

use crate::error::{ESUCCESS, EPERM, EINVAL, SyscallError, SyscallResult};
use crate::raw::{syscall2, numbers::*};

/// Read a byte from an I/O port
///
/// Returns the byte read, or an error if access is denied.
/// Only ports authorized by the kernel can be accessed.
pub fn port_read_u8(port: u16) -> SyscallResult<u8> {
    let result = unsafe {
        syscall2(SYS_IO_PORT_READ, port as u64, 1)
    };

    if result == EPERM {
        Err(SyscallError::PermissionDenied)
    } else if result == EINVAL {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(result as u8)
    }
}

/// Write a byte to an I/O port
///
/// Returns Ok(()) on success, or an error if access is denied.
/// Only ports authorized by the kernel can be accessed.
pub fn port_write_u8(port: u16, value: u8) -> SyscallResult<()> {
    let result = unsafe {
        syscall2(SYS_IO_PORT_WRITE, port as u64, value as u64)
    };

    if result == ESUCCESS {
        Ok(())
    } else if result == EPERM {
        Err(SyscallError::PermissionDenied)
    } else if result == EINVAL {
        Err(SyscallError::InvalidArgument)
    } else {
        Err(SyscallError::InvalidArgument)
    }
}

// ============================================================================
// PS/2 Controller Helpers
// ============================================================================

/// PS/2 controller ports
pub mod ps2 {
    pub const DATA_PORT: u16 = 0x60;
    pub const STATUS_PORT: u16 = 0x64;
    pub const COMMAND_PORT: u16 = 0x64;

    /// Status register bits
    pub const STATUS_OUTPUT_FULL: u8 = 1 << 0;
    pub const STATUS_INPUT_FULL: u8 = 1 << 1;
    pub const STATUS_AUX_DATA: u8 = 1 << 5;
}

/// Check if PS/2 data is available (output buffer full)
pub fn ps2_data_available() -> SyscallResult<bool> {
    let status = port_read_u8(ps2::STATUS_PORT)?;
    Ok(status & ps2::STATUS_OUTPUT_FULL != 0)
}

/// Check if PS/2 input buffer is empty (ready for command)
pub fn ps2_can_send() -> SyscallResult<bool> {
    let status = port_read_u8(ps2::STATUS_PORT)?;
    Ok(status & ps2::STATUS_INPUT_FULL == 0)
}

/// Wait for PS/2 input buffer to be ready
pub fn ps2_wait_input() -> SyscallResult<()> {
    for _ in 0..10000 {
        if ps2_can_send()? {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(SyscallError::TimedOut)
}

/// Wait for PS/2 output buffer to have data
pub fn ps2_wait_output() -> SyscallResult<()> {
    for _ in 0..10000 {
        if ps2_data_available()? {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(SyscallError::TimedOut)
}

/// Read PS/2 data port
pub fn ps2_read_data() -> SyscallResult<u8> {
    port_read_u8(ps2::DATA_PORT)
}

/// Write to PS/2 data port
pub fn ps2_write_data(data: u8) -> SyscallResult<()> {
    ps2_wait_input()?;
    port_write_u8(ps2::DATA_PORT, data)
}

/// Write PS/2 command
pub fn ps2_write_command(cmd: u8) -> SyscallResult<()> {
    ps2_wait_input()?;
    port_write_u8(ps2::COMMAND_PORT, cmd)
}

/// Read PS/2 status register
pub fn ps2_read_status() -> SyscallResult<u8> {
    port_read_u8(ps2::STATUS_PORT)
}

/// Send command to auxiliary device (mouse)
/// This writes 0xD4 to command port, then the command to data port
pub fn ps2_write_aux_command(cmd: u8) -> SyscallResult<()> {
    ps2_write_command(0xD4)?; // Next byte goes to aux device
    ps2_write_data(cmd)
}
