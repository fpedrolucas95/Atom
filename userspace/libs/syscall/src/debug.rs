// Debug and logging syscalls

use crate::raw::{syscall2, numbers::*};

/// Send a debug log message to the kernel
///
/// This is useful for debugging userspace programs.
/// Messages will appear in the kernel's serial output.
pub fn log(message: &str) {
    unsafe {
        syscall2(SYS_DEBUG_LOG, message.as_ptr() as u64, message.len() as u64);
    }
}

/// Log a message with a prefix tag
pub fn log_tagged(tag: &str, message: &str) {
    // Simple implementation - just log the message
    // In a real implementation, we might format this differently
    log(message);
}

/// Macro for debug logging (similar to println!)
#[macro_export]
macro_rules! debug_print {
    ($($arg:tt)*) => {{
        // For now, just a stub - would need alloc for formatting
    }};
}

/// Get system memory information
/// Returns (total_kb, free_kb)
pub fn get_memory_info() -> (u64, u64) {
    use crate::raw::syscall1;

    let mut info = [0u64; 2];
    let result = unsafe {
        syscall1(SYS_GET_MEMORY_INFO, info.as_mut_ptr() as u64)
    };

    // Check if syscall succeeded (ESUCCESS = 0)
    if result == 0 {
        (info[0], info[1])
    } else {
        // Return default values on error
        (0, 0)
    }
}

/// Read kernel log buffer
/// Returns the number of bytes read
pub fn read_klog(buffer: &mut [u8]) -> usize {
    use crate::raw::syscall2;

    if buffer.is_empty() {
        return 0;
    }

    let result = unsafe {
        syscall2(
            SYS_READ_KLOG,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64
        )
    };

    // Check for error (high values indicate error)
    if crate::error::is_syscall_error(result) {
        return 0;
    }

    result as usize
}

/// Get CPU brand string
/// Returns the number of bytes written to the buffer
pub fn get_cpu_brand(buffer: &mut [u8]) -> usize {
    use crate::raw::syscall2;

    if buffer.is_empty() {
        return 0;
    }

    let result = unsafe {
        syscall2(
            SYS_GET_CPU_BRAND,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64
        )
    };

    // Check for error
    if crate::error::is_syscall_error(result) {
        return 0;
    }

    result as usize
}
