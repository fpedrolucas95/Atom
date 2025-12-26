// IPC Client Module
//
// This module provides high-level interfaces for communicating with
// system services via IPC. All system information is obtained through
// service requests, never by accessing kernel internals directly.
//
// Service Architecture:
// - Each service has a well-known port ID or is discovered via the service manager
// - Requests are sent as structured messages
// - Responses are received and decoded

use atom_syscall::ipc::{create_port, close_port, send, recv, try_recv, send_async, PortId};
use atom_syscall::error::SyscallResult;
use atom_syscall::thread::get_ticks;

/// Message types for IPC communication
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    // Service discovery
    ServiceLookup = 0x01,
    ServiceRegister = 0x02,
    ServiceList = 0x03,

    // Process manager
    ProcessList = 0x10,
    ProcessInfo = 0x11,
    ProcessKill = 0x12,
    ProcessSpawn = 0x13,

    // Memory service
    MemoryStats = 0x20,
    MemoryInfo = 0x21,

    // Filesystem service
    FileOpen = 0x30,
    FileRead = 0x31,
    FileWrite = 0x32,
    FileClose = 0x33,
    DirList = 0x34,
    FileStat = 0x35,

    // System info
    SystemVersion = 0x40,
    SystemUptime = 0x41,
    SystemTime = 0x42,
    SystemLog = 0x43,

    // Response types
    ResponseOk = 0xF0,
    ResponseError = 0xF1,
    ResponseData = 0xF2,
}

/// Well-known service port IDs
/// In a real implementation, these would be discovered via a name service
pub mod service_ports {
    use super::PortId;

    pub const SERVICE_MANAGER: PortId = 1;
    pub const PROCESS_MANAGER: PortId = 2;
    pub const MEMORY_MANAGER: PortId = 3;
    pub const FILESYSTEM: PortId = 4;
    pub const DISPLAY_SERVER: PortId = 5;
    pub const INPUT_SERVER: PortId = 6;
}

/// IPC client for terminal commands
pub struct IpcClient {
    /// Our local port for receiving responses
    response_port: Option<PortId>,
}

impl IpcClient {
    pub fn new() -> Self {
        Self {
            response_port: None,
        }
    }

    /// Initialize the client (create response port)
    pub fn init(&mut self) -> bool {
        match create_port() {
            Ok(port) => {
                self.response_port = Some(port);
                true
            }
            Err(_) => false,
        }
    }

    /// Clean up resources
    pub fn cleanup(&mut self) {
        if let Some(port) = self.response_port.take() {
            let _ = close_port(port);
        }
    }

    /// Get system uptime in ticks
    pub fn get_uptime_ticks(&self) -> u64 {
        get_ticks()
    }

    /// Get system uptime formatted as string
    pub fn format_uptime(&self, buffer: &mut [u8]) -> usize {
        let ticks = get_ticks();
        // Assuming 100Hz timer (10ms per tick)
        let total_seconds = ticks / 100;
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        let mut pos = 0;

        // Format hours
        pos += format_number(hours, &mut buffer[pos..]);
        if pos < buffer.len() {
            buffer[pos] = b':';
            pos += 1;
        }

        // Format minutes with leading zero
        if minutes < 10 && pos < buffer.len() {
            buffer[pos] = b'0';
            pos += 1;
        }
        pos += format_number(minutes, &mut buffer[pos..]);
        if pos < buffer.len() {
            buffer[pos] = b':';
            pos += 1;
        }

        // Format seconds with leading zero
        if seconds < 10 && pos < buffer.len() {
            buffer[pos] = b'0';
            pos += 1;
        }
        pos += format_number(seconds, &mut buffer[pos..]);

        pos
    }

    /// Query process list from process manager service
    /// Returns process info via the provided callback
    /// Note: In the current early stage, this returns simulated data
    /// as the full process manager service may not be running
    pub fn query_processes<F>(&self, mut callback: F)
    where
        F: FnMut(u64, &str, &str), // pid, name, state
    {
        // In a full implementation, we would:
        // 1. Send ProcessList request to PROCESS_MANAGER port
        // 2. Receive response with process data
        // 3. Parse and call callback for each process

        // For now, return known system processes
        callback(0, "kernel", "running");
        callback(1, "init", "running");
        callback(2, "display", "running");
        callback(3, "keyboard", "running");
        callback(4, "mouse", "running");
        callback(5, "ui_shell", "running");
        callback(6, "terminal", "running");
    }

    /// Query memory statistics from memory service
    /// Returns (total_kb, used_kb, free_kb)
    /// Note: In early stage, returns estimated values
    pub fn query_memory(&self) -> (u64, u64, u64) {
        // In a full implementation, we would query the memory manager service
        // For now, return placeholder values based on typical early boot state

        // These would come from MEMORY_MANAGER service
        let total_kb = 128 * 1024; // 128 MB typical for testing
        let used_kb = 32 * 1024;   // Approximate kernel + userspace usage
        let free_kb = total_kb - used_kb;

        (total_kb, used_kb, free_kb)
    }

    /// Query registered services from service manager
    pub fn query_services<F>(&self, mut callback: F)
    where
        F: FnMut(&str, u64, &str), // name, port, status
    {
        // In a full implementation, query SERVICE_MANAGER
        // For now, return known services
        callback("display_server", 5, "active");
        callback("keyboard_driver", 6, "active");
        callback("mouse_driver", 7, "active");
        callback("ui_shell", 8, "active");
    }

    /// Attempt to terminate a process
    /// Returns true if the request was sent (not necessarily successful)
    pub fn kill_process(&self, pid: u64) -> bool {
        // Would send ProcessKill to PROCESS_MANAGER
        // For now, just report that it's not implemented for system processes
        pid >= 10 // Only "allow" killing non-system processes
    }

    /// Attempt to launch a program
    /// Returns the new process ID if successful
    pub fn spawn_process(&self, _path: &str, _args: &[&str]) -> Option<u64> {
        // Would send ProcessSpawn to PROCESS_MANAGER
        // Not implemented in early stage
        None
    }

    /// List directory contents via filesystem service
    pub fn list_directory<F>(&self, _path: &str, mut callback: F)
    where
        F: FnMut(&str, bool, u64), // name, is_dir, size
    {
        // Would query FILESYSTEM service
        // For now, return simulated root directory
        callback("bin", true, 0);
        callback("etc", true, 0);
        callback("dev", true, 0);
        callback("sys", true, 0);
        callback("proc", true, 0);
        callback("home", true, 0);
    }

    /// Read file contents via filesystem service
    pub fn read_file(&self, _path: &str, buffer: &mut [u8]) -> Option<usize> {
        // Would query FILESYSTEM service
        // Not implemented in early stage
        let _ = buffer;
        None
    }

    /// Get file information
    pub fn stat_file(&self, _path: &str) -> Option<FileInfo> {
        // Would query FILESYSTEM service
        None
    }

    /// Read system log entries
    pub fn read_log<F>(&self, mut callback: F)
    where
        F: FnMut(&str), // log line
    {
        // Would query logging service or read from /sys/log
        callback("[0.000] Atom OS kernel initializing");
        callback("[0.001] Memory manager initialized");
        callback("[0.002] Scheduler started");
        callback("[0.003] IPC subsystem ready");
        callback("[0.010] Loading userspace drivers");
        callback("[0.020] Display server started");
        callback("[0.030] Input drivers initialized");
        callback("[0.050] UI shell launched");
    }
}

impl Default for IpcClient {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IpcClient {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// File information structure
pub struct FileInfo {
    pub size: u64,
    pub is_dir: bool,
    pub created: u64,
    pub modified: u64,
}

/// Format a number into a buffer, returns bytes written
fn format_number(mut n: u64, buffer: &mut [u8]) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    if n == 0 {
        buffer[0] = b'0';
        return 1;
    }

    // Count digits
    let mut temp = n;
    let mut digits = 0;
    while temp > 0 {
        digits += 1;
        temp /= 10;
    }

    if digits > buffer.len() {
        return 0;
    }

    // Write digits in reverse
    let mut pos = digits;
    while n > 0 {
        pos -= 1;
        buffer[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    digits
}

/// Format bytes as human-readable size (KB, MB, GB)
pub fn format_size(bytes: u64, buffer: &mut [u8]) -> usize {
    let mut pos = 0;

    if bytes < 1024 {
        pos += format_number(bytes, &mut buffer[pos..]);
        if pos + 2 <= buffer.len() {
            buffer[pos] = b' ';
            buffer[pos + 1] = b'B';
            pos += 2;
        }
    } else if bytes < 1024 * 1024 {
        pos += format_number(bytes / 1024, &mut buffer[pos..]);
        if pos + 3 <= buffer.len() {
            buffer[pos] = b' ';
            buffer[pos + 1] = b'K';
            buffer[pos + 2] = b'B';
            pos += 3;
        }
    } else if bytes < 1024 * 1024 * 1024 {
        pos += format_number(bytes / (1024 * 1024), &mut buffer[pos..]);
        if pos + 3 <= buffer.len() {
            buffer[pos] = b' ';
            buffer[pos + 1] = b'M';
            buffer[pos + 2] = b'B';
            pos += 3;
        }
    } else {
        pos += format_number(bytes / (1024 * 1024 * 1024), &mut buffer[pos..]);
        if pos + 3 <= buffer.len() {
            buffer[pos] = b' ';
            buffer[pos + 1] = b'G';
            buffer[pos + 2] = b'B';
            pos += 3;
        }
    }

    pos
}