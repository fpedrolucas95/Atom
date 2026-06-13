// I/O port access syscalls
//
// These syscalls allow userspace drivers to access hardware I/O ports.
// Access is controlled by the kernel's capability system - only authorized
// ports can be accessed.

use crate::error::{SyscallError, SyscallResult, EINVAL, EPERM, ESUCCESS};
use crate::raw::{numbers::*, syscall2, syscall3};

/// Read a byte from an I/O port
///
/// Returns the byte read, or an error if access is denied.
/// Only ports authorized by the kernel can be accessed.
pub fn port_read_u8(port: u16) -> SyscallResult<u8> {
    port_read(port, 1).map(|v| v as u8)
}

/// Read a 16-bit word from an I/O port (single `inw`).
///
/// Required by devices whose registers only respond to word accesses
/// (e.g. the AC'97 mixer and several bus-master status registers).
pub fn port_read_u16(port: u16) -> SyscallResult<u16> {
    port_read(port, 2).map(|v| v as u16)
}

/// Read a 32-bit dword from an I/O port (single `inl`).
pub fn port_read_u32(port: u16) -> SyscallResult<u32> {
    port_read(port, 4)
}

fn port_read(port: u16, size: u8) -> SyscallResult<u32> {
    let result = unsafe { syscall2(SYS_IO_PORT_READ, port as u64, size as u64) };

    if result == EPERM {
        Err(SyscallError::PermissionDenied)
    } else if result == EINVAL {
        Err(SyscallError::InvalidArgument)
    } else {
        Ok(result as u32)
    }
}

/// Write a byte to an I/O port
///
/// Returns Ok(()) on success, or an error if access is denied.
/// Only ports authorized by the kernel can be accessed.
pub fn port_write_u8(port: u16, value: u8) -> SyscallResult<()> {
    port_write(port, value as u32, 1)
}

/// Write a 16-bit word to an I/O port (single `outw`).
pub fn port_write_u16(port: u16, value: u16) -> SyscallResult<()> {
    port_write(port, value as u32, 2)
}

/// Write a 32-bit dword to an I/O port (single `outl`).
pub fn port_write_u32(port: u16, value: u32) -> SyscallResult<()> {
    port_write(port, value, 4)
}

fn port_write(port: u16, value: u32, size: u8) -> SyscallResult<()> {
    let result = unsafe { syscall3(SYS_IO_PORT_WRITE, port as u64, value as u64, size as u64) };

    if result == ESUCCESS {
        Ok(())
    } else if result == EPERM {
        Err(SyscallError::PermissionDenied)
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
