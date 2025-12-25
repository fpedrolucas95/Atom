// Userspace API - System Call Wrappers
//
// This module provides safe wrappers around syscalls for use by userspace
// code that runs in ring 3. All interaction with kernel services must go
// through these syscall interfaces.
//
// IMPORTANT: Code using this module MUST run in ring 3 (CPL=3).
// Calling these from kernel mode will work but defeats the purpose.

#![allow(dead_code)]

use crate::syscall::{
    ESUCCESS, EWOULDBLOCK, EINVAL, EPERM,
    SYS_THREAD_YIELD, SYS_THREAD_EXIT, SYS_GET_TICKS,
    SYS_MOUSE_POLL, SYS_KEYBOARD_POLL, SYS_GET_FRAMEBUFFER,
    SYS_IO_PORT_READ, SYS_IO_PORT_WRITE, SYS_DEBUG_LOG,
};

/// Raw syscall invocation
#[inline(always)]
pub unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
pub unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
pub unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
pub unsafe fn syscall3(num: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        in("rsi") arg1,
        in("rdx") arg2,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

// ============================================================================
// Thread Management
// ============================================================================

/// Yield CPU to scheduler
pub fn sys_yield() {
    unsafe {
        syscall0(SYS_THREAD_YIELD);
    }
}

/// Exit current thread with code
pub fn sys_exit(code: u64) -> ! {
    unsafe {
        syscall1(SYS_THREAD_EXIT, code);
    }
    loop {
        core::hint::spin_loop();
    }
}

/// Get system ticks (timer interrupts since boot)
pub fn sys_get_ticks() -> u64 {
    unsafe {
        syscall0(SYS_GET_TICKS)
    }
}

// ============================================================================
// Input Polling
// ============================================================================

/// Poll mouse delta. Returns Some((dx, dy)) if movement available.
pub fn sys_mouse_poll() -> Option<(i32, i32)> {
    let result = unsafe { syscall0(SYS_MOUSE_POLL) };

    if result == EWOULDBLOCK {
        None
    } else {
        let dx = (result >> 32) as i32;
        let dy = result as i32;
        Some((dx, dy))
    }
}

/// Poll keyboard scancode. Returns Some(scancode) if available.
pub fn sys_keyboard_poll() -> Option<u8> {
    let result = unsafe { syscall0(SYS_KEYBOARD_POLL) };

    if result == EWOULDBLOCK {
        None
    } else {
        Some(result as u8)
    }
}

// ============================================================================
// Graphics / Framebuffer
// ============================================================================

/// Framebuffer information returned by syscall
#[repr(C)]
pub struct FramebufferInfo {
    pub address: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub bytes_per_pixel: u32,
}

/// Get framebuffer information for direct graphics access
pub fn sys_get_framebuffer() -> Option<FramebufferInfo> {
    let mut info = [0u64; 5];
    let result = unsafe {
        syscall1(SYS_GET_FRAMEBUFFER, info.as_mut_ptr() as u64)
    };

    if result == ESUCCESS {
        Some(FramebufferInfo {
            address: info[0] as usize,
            width: info[1] as u32,
            height: info[2] as u32,
            stride: info[3] as u32,
            bytes_per_pixel: info[4] as u32,
        })
    } else {
        None
    }
}

// ============================================================================
// I/O Ports (for userspace drivers)
// ============================================================================

/// Read a byte from an I/O port (restricted to allowed ports)
pub fn sys_io_port_read(port: u16) -> Result<u8, u64> {
    let result = unsafe {
        syscall2(SYS_IO_PORT_READ, port as u64, 1)
    };

    if result == EPERM {
        Err(EPERM)
    } else if result == EINVAL {
        Err(EINVAL)
    } else {
        Ok(result as u8)
    }
}

/// Write a byte to an I/O port (restricted to allowed ports)
pub fn sys_io_port_write(port: u16, value: u8) -> Result<(), u64> {
    let result = unsafe {
        syscall2(SYS_IO_PORT_WRITE, port as u64, value as u64)
    };

    if result == ESUCCESS {
        Ok(())
    } else {
        Err(result)
    }
}

// ============================================================================
// Debug
// ============================================================================

/// Write debug message to kernel log
pub fn sys_debug_log(msg: &str) {
    unsafe {
        syscall2(SYS_DEBUG_LOG, msg.as_ptr() as u64, msg.len() as u64);
    }
}

// ============================================================================
// Userspace Driver Support
// ============================================================================

/// PS/2 keyboard ports
pub const PS2_DATA_PORT: u16 = 0x60;
pub const PS2_STATUS_PORT: u16 = 0x64;

/// Read PS/2 status register
pub fn ps2_read_status() -> Result<u8, u64> {
    sys_io_port_read(PS2_STATUS_PORT)
}

/// Read PS/2 data port
pub fn ps2_read_data() -> Result<u8, u64> {
    sys_io_port_read(PS2_DATA_PORT)
}

/// Write to PS/2 command port
pub fn ps2_write_command(cmd: u8) -> Result<(), u64> {
    sys_io_port_write(PS2_STATUS_PORT, cmd)
}

/// Write to PS/2 data port
pub fn ps2_write_data(data: u8) -> Result<(), u64> {
    sys_io_port_write(PS2_DATA_PORT, data)
}
