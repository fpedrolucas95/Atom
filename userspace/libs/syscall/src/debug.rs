// Debug and logging syscalls

use crate::raw::{numbers::*, syscall2, syscall3};

pub const MAX_RESOURCE_CPUS: usize = 8;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SystemResourceUsage {
    pub total_ram_kb: u64,
    pub free_ram_kb: u64,
    pub framebuffer_kb: u64,
    pub shared_memory_kb: u64,
    pub cpu_count: u64,
    pub cpu_total_ticks: [u64; MAX_RESOURCE_CPUS],
    pub cpu_idle_ticks: [u64; MAX_RESOURCE_CPUS],
}

impl SystemResourceUsage {
    pub const fn empty() -> Self {
        Self {
            total_ram_kb: 0,
            free_ram_kb: 0,
            framebuffer_kb: 0,
            shared_memory_kb: 0,
            cpu_count: 0,
            cpu_total_ticks: [0; MAX_RESOURCE_CPUS],
            cpu_idle_ticks: [0; MAX_RESOURCE_CPUS],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessResourceUsage {
    pub pid: u64,
    pub private_memory_kb: u64,
    pub cpu_ticks: u64,
    pub name: [u8; 32],
}

impl ProcessResourceUsage {
    pub const fn empty() -> Self {
        Self {
            pid: 0,
            private_memory_kb: 0,
            cpu_ticks: 0,
            name: [0; 32],
        }
    }

    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|byte| *byte == 0).unwrap_or(32);
        unsafe { core::str::from_utf8_unchecked(&self.name[..len]) }
    }
}

pub fn get_resource_usage(
    system: &mut SystemResourceUsage,
    processes: &mut [ProcessResourceUsage],
) -> usize {
    let result = unsafe {
        syscall3(
            SYS_GET_RESOURCE_USAGE,
            system as *mut SystemResourceUsage as u64,
            processes.as_mut_ptr() as u64,
            processes.len() as u64,
        )
    };

    if crate::error::is_syscall_error(result) {
        0
    } else {
        result as usize
    }
}

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
pub fn log_tagged(_tag: &str, message: &str) {
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
    let result = unsafe { syscall1(SYS_GET_MEMORY_INFO, info.as_mut_ptr() as u64) };

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
            buffer.len() as u64,
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
            buffer.len() as u64,
        )
    };

    // Check for error
    if crate::error::is_syscall_error(result) {
        return 0;
    }

    result as usize
}
