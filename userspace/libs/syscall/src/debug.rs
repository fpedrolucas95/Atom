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
