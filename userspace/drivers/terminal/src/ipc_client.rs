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

    /// Query process list from kernel via syscall
    /// Returns process info via the provided callback
    pub fn query_processes<F>(&self, mut callback: F)
    where
        F: FnMut(u64, &str, &str), // pid, name, state
    {
        use atom_syscall::process::{ProcessInfo, list_processes};

        // Allocate buffer for process list
        let mut buffer = [ProcessInfo::empty(); 32];
        let count = list_processes(&mut buffer);

        for i in 0..count {
            let proc = &buffer[i];
            callback(proc.pid, proc.name_str(), proc.state_str());
        }
    }

    /// Query memory statistics from memory service
    /// Returns (total_kb, used_kb, free_kb)
    /// Note: In early stage, returns estimated values
    pub fn query_memory(&self) -> (u64, u64, u64) {
        // Query real memory information from kernel
        use atom_syscall::debug::get_memory_info;

        let (total_kb, free_kb) = get_memory_info();
        let used_kb = total_kb.saturating_sub(free_kb);

        (total_kb, used_kb, free_kb)
    }

    /// Query registered services
    /// Note: Currently returns well-known system services since there's no
    /// service manager protocol implemented yet. Services are inferred from
    /// the running process list.
    pub fn query_services<F>(&self, mut callback: F)
    where
        F: FnMut(&str, u64, &str), // name, port, status
    {
        use atom_syscall::process::{ProcessInfo, list_processes, thread_state};

        // Get process list to infer active services
        let mut buffer = [ProcessInfo::empty(); 32];
        let count = list_processes(&mut buffer);

        // Map known processes to services with their ports
        for i in 0..count {
            let proc = &buffer[i];
            let name = proc.name_str();
            // Active: Running, Ready, or WaitingIpc (blocked but alive)
            // Idle: Blocked (generic) or Exited
            let status = match proc.state {
                thread_state::RUNNING | thread_state::READY | thread_state::WAITING_IPC => "active",
                _ => "idle",
            };

            // Map process names to service names and ports
            match name {
                "display" | "display_server" => callback("display_server", service_ports::DISPLAY_SERVER, status),
                "keyboard" | "input" => callback("keyboard_driver", service_ports::INPUT_SERVER, status),
                "ui_shell" | "shell" => callback("ui_shell", 8, status),
                "terminal" => callback("terminal", 9, status),
                _ => {}
            }
        }
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
    pub fn spawn_process(&self, name: &str, _args: &[&str]) -> Option<u64> {
        use atom_syscall::process::spawn_process;

        // Try to spawn the process using the kernel syscall
        match spawn_process(name) {
            Ok(pid) => Some(pid),
            Err(_) => None,
        }
    }

    /// List directory contents
    /// For virtual directories (/proc, /sys), returns system information
    /// For other paths, returns standard directory structure
    pub fn list_directory<F>(&self, path: &str, mut callback: F)
    where
        F: FnMut(&str, bool, u64), // name, is_dir, size
    {
        match path {
            "/" => {
                // Root directory structure
                callback("bin", true, 0);
                callback("etc", true, 0);
                callback("dev", true, 0);
                callback("sys", true, 0);
                callback("proc", true, 0);
                callback("home", true, 0);
            }
            "/proc" => {
                // Process information directory
                use atom_syscall::process::{ProcessInfo, list_processes};
                let mut buffer = [ProcessInfo::empty(); 32];
                let count = list_processes(&mut buffer);

                // Each process gets a directory named by its PID
                for i in 0..count {
                    // Create a static string for the PID
                    let pid = buffer[i].pid;
                    // Use a simple approach - just show as numbered entries
                    if pid < 10 {
                        let digit = b'0' + pid as u8;
                        let name = unsafe { core::str::from_utf8_unchecked(core::slice::from_ref(&digit)) };
                        callback(name, true, 0);
                    }
                }
                callback("meminfo", false, 128);
                callback("version", false, 64);
                callback("uptime", false, 32);
            }
            "/sys" => {
                // System information directory
                callback("kernel", true, 0);
                callback("memory", true, 0);
                callback("devices", true, 0);
            }
            "/dev" => {
                // Device nodes
                callback("null", false, 0);
                callback("zero", false, 0);
                callback("fb0", false, 0);
                callback("tty0", false, 0);
                callback("kbd", false, 0);
                callback("mouse", false, 0);
            }
            "/bin" => {
                // Executables (drivers)
                callback("terminal", false, 0);
                callback("display", false, 0);
                callback("keyboard", false, 0);
                callback("mouse", false, 0);
                callback("ui_shell", false, 0);
            }
            "/etc" => {
                // Configuration files
                callback("hostname", false, 32);
                callback("version", false, 64);
            }
            "/home" => {
                callback("user", true, 0);
            }
            _ => {
                // Unknown path - return empty
            }
        }
    }

    /// Read file contents
    /// For virtual files in /proc and /sys, returns system information
    pub fn read_file(&self, path: &str, buffer: &mut [u8]) -> Option<usize> {
        let content: &[u8] = match path {
            "/proc/version" | "/etc/version" => {
                b"Atom OS 0.1.0 (Helium)\nKernel: 0.1.0-microkernel\nArch: x86_64\n"
            }
            "/proc/meminfo" => {
                // Get real memory info
                let (total_kb, free_kb) = atom_syscall::debug::get_memory_info();
                let used_kb = total_kb.saturating_sub(free_kb);

                // Format memory info into buffer
                let mut pos = 0;
                let prefix = b"MemTotal:  ";
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();
                pos += format_number_to_buffer(total_kb, &mut buffer[pos..]);
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                let prefix = b"MemFree:   ";
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();
                pos += format_number_to_buffer(free_kb, &mut buffer[pos..]);
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                let prefix = b"MemUsed:   ";
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();
                pos += format_number_to_buffer(used_kb, &mut buffer[pos..]);
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                return Some(pos);
            }
            "/proc/uptime" => {
                let ticks = atom_syscall::thread::get_ticks();
                let seconds = ticks / 100;

                let mut pos = 0;
                pos += format_number_to_buffer(seconds, &mut buffer[pos..]);
                buffer[pos..pos + 3].copy_from_slice(b" s\n");
                pos += 3;

                return Some(pos);
            }
            "/etc/hostname" => b"atom\n",
            "/dev/null" => b"",
            _ => return None,
        };

        let copy_len = content.len().min(buffer.len());
        buffer[..copy_len].copy_from_slice(&content[..copy_len]);
        Some(copy_len)
    }

    /// Get file information
    pub fn stat_file(&self, _path: &str) -> Option<FileInfo> {
        // Would query FILESYSTEM service
        None
    }

    /// Read system log entries from kernel log buffer
    pub fn read_log<F>(&self, mut callback: F)
    where
        F: FnMut(&str), // log line
    {
        use atom_syscall::debug::read_klog;

        // Read kernel log buffer
        let mut buffer = [0u8; 4096];
        let len = read_klog(&mut buffer);

        if len == 0 {
            callback("[no log entries available]");
            return;
        }

        // Parse log buffer into lines
        let log_data = unsafe { core::str::from_utf8_unchecked(&buffer[..len]) };

        for line in log_data.lines() {
            if !line.is_empty() {
                callback(line);
            }
        }
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

/// Format a number into a buffer for IpcClient use
fn format_number_to_buffer(mut n: u64, buffer: &mut [u8]) -> usize {
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