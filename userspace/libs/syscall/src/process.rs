// Process management syscalls

use crate::raw::{syscall0, syscall2, numbers::*};
use crate::error::{SyscallError, SyscallResult};

/// Process ID type
pub type ProcessId = u64;

/// Thread state constants (must match kernel ThreadState encoding)
pub mod thread_state {
    pub const RUNNING: u8 = 0;
    pub const READY: u8 = 1;
    pub const BLOCKED: u8 = 2;
    pub const EXITED: u8 = 3;
    pub const WAITING_IPC: u8 = 4;
}

/// Process/thread information returned by list_processes
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessInfo {
    pub pid: u64,
    /// Thread state: 0=Running, 1=Ready, 2=Blocked, 3=Exited, 4=WaitingIpc
    pub state: u8,
    pub name: [u8; 32],
}

impl ProcessInfo {
    pub const fn empty() -> Self {
        Self {
            pid: 0,
            state: 0,
            name: [0u8; 32],
        }
    }

    /// Get process name as string slice
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        // Safety: kernel guarantees valid UTF-8 in name field
        unsafe { core::str::from_utf8_unchecked(&self.name[..len]) }
    }

    /// Get state as string
    pub fn state_str(&self) -> &'static str {
        match self.state {
            thread_state::RUNNING => "running",
            thread_state::READY => "ready",
            thread_state::BLOCKED => "blocked",
            thread_state::EXITED => "exited",
            thread_state::WAITING_IPC => "waiting_ipc",
            _ => "unknown",
        }
    }
}

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

/// List all processes/threads in the system
///
/// # Arguments
/// * `buffer` - Buffer to receive ProcessInfo structs
///
/// # Returns
/// * Number of processes written to buffer
pub fn list_processes(buffer: &mut [ProcessInfo]) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    let result = unsafe {
        syscall2(
            SYS_LIST_PROCESSES,
            buffer.as_mut_ptr() as u64,
            buffer.len() as u64
        )
    };

    // Check for error
    if result >= u64::MAX - 100 {
        return 0;
    }

    result as usize
}

/// Get total number of processes/threads
pub fn get_process_count() -> usize {
    let result = unsafe { syscall0(SYS_GET_PROCESS_COUNT) };
    result as usize
}
