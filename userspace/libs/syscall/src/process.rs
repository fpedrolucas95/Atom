// Process management syscalls

use crate::raw::{syscall2, numbers::*};
use crate::error::{SyscallError, SyscallResult};

/// Process ID type
pub type ProcessId = u64;

/// Spawn a new process from a registered driver
///
/// This function requests the kernel to spawn a new process by loading
/// an ATXF executable that was registered at boot time by the bootloader.
///
/// # Arguments
/// * `name` - The name of the driver to spawn (e.g., "terminal")
///
/// # Returns
/// * `Ok(ProcessId)` - The PID of the newly spawned process
/// * `Err(SyscallError)` - If the spawn failed
///
/// # Example
/// ```
/// if let Ok(pid) = spawn_process("terminal") {
///     // Terminal process was spawned with the given PID
/// }
/// ```
pub fn spawn_process(name: &str) -> SyscallResult<ProcessId> {
    let name_ptr = name.as_ptr() as u64;
    let name_len = name.len() as u64;

    let result = unsafe {
        syscall2(SYS_SPAWN_PROCESS, name_ptr, name_len)
    };

    // Check for errors - error codes are high values (u64::MAX - N)
    if result >= u64::MAX - 100 {
        return Err(SyscallError::from_raw(result));
    }

    // Valid PID returned
    Ok(result)
}
