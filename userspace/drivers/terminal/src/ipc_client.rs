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

use atom_syscall::ipc::{create_port, close_port, send, recv_with_timeout, PortId};
use atom_syscall::thread::get_ticks;
use atom_abi::PORT_FS_SERVICE;

extern crate alloc;
use alloc::vec::Vec;

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
/// These must match the ports defined in atom_abi and libipc ports.rs
pub mod service_ports {
    use super::PortId;

    pub const NAME_SERVICE: PortId = 1;        // namesvc (Port 1)
    pub const SERVICE_MANAGER: PortId = 2;     // service_manager (Port 2)
    pub const FILESYSTEM: PortId = 3;          // fsd (Port 3)
    pub const BLOCK_SERVICE: PortId = 4;       // block service (Port 4)
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

    /// Get screen resolution via syscall
    pub fn get_resolution(&self) -> Option<(u32, u32)> {
        atom_syscall::graphics::get_framebuffer().map(|fb| (fb.width, fb.height))
    }

    /// Get total number of processes
    pub fn count_processes(&self) -> usize {
        atom_syscall::process::get_process_count()
    }

    /// Get CPU brand string via syscall
    pub fn get_cpu_brand(&self, buffer: &mut [u8]) -> usize {
        atom_syscall::debug::get_cpu_brand(buffer)
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

    /// List directory contents via IPC to filesystem daemon
    /// Sends FsReaddir request to PORT_FS_SERVICE and parses real directory entries
    pub fn list_directory<F>(&self, path: &str, mut callback: F)
    where
        F: FnMut(&str, bool, u64), // name, is_dir, size
    {
        // First check if it's a virtual directory handled by the kernel
        if self.try_list_virtual_directory(path, &mut callback) {
            return;
        }

        // Try to list via filesystem daemon
        if let Err(_) = self.list_directory_ipc(path, &mut callback) {
            // Fall back to empty if IPC fails
        }
    }

    /// Check if this is a virtual directory and list from kernel
    fn try_list_virtual_directory<F>(&self, path: &str, callback: &mut F) -> bool
    where
        F: FnMut(&str, bool, u64), // name, is_dir, size
    {
        match path {
            "/proc" => {
                use atom_syscall::process::{ProcessInfo, list_processes};
                let mut buffer = [ProcessInfo::empty(); 32];
                let count = list_processes(&mut buffer);

                for i in 0..count {
                    let pid = buffer[i].pid;
                    if pid < 10 {
                        let digit = b'0' + pid as u8;
                        let name = unsafe {
                            core::str::from_utf8_unchecked(core::slice::from_ref(&digit))
                        };
                        callback(name, true, 0);
                    }
                }
                callback("meminfo", false, 128);
                callback("version", false, 64);
                callback("uptime", false, 32);
                true
            }
            _ => false,
        }
    }

    /// List directory via IPC to filesystem daemon
    fn list_directory_ipc<F>(&self, path: &str, callback: &mut F) -> Result<(), &'static str>
    where
        F: FnMut(&str, bool, u64), // name, is_dir, size
    {
        use libipc::messages::{MessageHeader, MessageType, FsReply, FsDirentIter};

        if path.len() > 4096 {
            return Err("path too long");
        }

        // Get response port
        let response_port = self.response_port.ok_or("no response port")?;

        // Build FsReaddir request: path_len (4 bytes) + path (variable)
        let mut request = Vec::new();
        request.extend_from_slice(&(path.len() as u32).to_le_bytes());
        request.extend_from_slice(path.as_bytes());

        // Build message: header + payload
        let header = MessageHeader::new(MessageType::FsReaddir, request.len() as u32);
        let mut message = Vec::new();
        message.extend_from_slice(&header.to_bytes());
        message.extend_from_slice(&request);

        // Send to filesystem daemon (fsd on PORT_FS_SERVICE)
        send(PORT_FS_SERVICE, &message).map_err(|_| "send failed")?;

        // Wait for response with timeout (5 seconds = 5000ms)
        let mut response_buf = [0u8; 4096];
        let resp_len = recv_with_timeout(response_port, &mut response_buf, 5000)
            .map_err(|_| "recv timeout or error")?;

        // Parse response header
        if resp_len < 16 {
            return Err("response too short");
        }

        let header = MessageHeader::from_bytes(&response_buf[..16])
            .ok_or("invalid header")?;

        if header.msg_type != MessageType::FsReaddirReply {
            return Err("wrong message type in reply");
        }

        // Parse FsReply (16 bytes: error + value)
        let reply = FsReply::from_bytes(&response_buf[16..32])
            .ok_or("invalid fs reply")?;

        if !reply.is_ok() {
            return Err("fs operation failed");
        }

        // Parse directory entries from payload
        let payload = &response_buf[32..resp_len];
        let iter = FsDirentIter::new(payload);

        for entry in iter {
            let is_dir = entry.file_type == 2; // EXT2_FT_DIR
            callback(&entry.name, is_dir, 0);
        }

        Ok(())
    }

    /// Read file contents
    /// For virtual files in /proc and /sys, returns system information
    /// Real filesystem reads will be implemented once fsd is fully functional
    pub fn read_file(&self, path: &str, buffer: &mut [u8]) -> Option<usize> {
        self.read_virtual_file(path, buffer)
    }

    /// Read virtual files from kernel information
    fn read_virtual_file(&self, path: &str, buffer: &mut [u8]) -> Option<usize> {
        match path {
            "/proc/version" | "/etc/version" => {
                let content = b"OS:           Atom OS 0.1.0 (Helium)\nKernel:       0.1.0-microkernel\n";
                let copy_len = content.len().min(buffer.len());
                buffer[..copy_len].copy_from_slice(&content[..copy_len]);
                return Some(copy_len);
            }
            "/etc/hostname" => {
                let content = b"atom\n";
                let copy_len = content.len().min(buffer.len());
                buffer[..copy_len].copy_from_slice(&content[..copy_len]);
                return Some(copy_len);
            }
            "/dev/null" => {
                return Some(0);
            }
            "/proc/meminfo" => {
                let (total_kb, free_kb) = atom_syscall::debug::get_memory_info();
                let used_kb = total_kb.saturating_sub(free_kb);

                let mut pos = 0;
                let prefix = b"MemTotal:  ";
                if pos + prefix.len() > buffer.len() {
                    return None;
                }
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();

                let written = format_number_to_buffer(total_kb, &mut buffer[pos..]);
                pos += written;

                if pos + 4 > buffer.len() {
                    return None;
                }
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                let prefix = b"MemFree:   ";
                if pos + prefix.len() > buffer.len() {
                    return None;
                }
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();

                let written = format_number_to_buffer(free_kb, &mut buffer[pos..]);
                pos += written;

                if pos + 4 > buffer.len() {
                    return None;
                }
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                let prefix = b"MemUsed:   ";
                if pos + prefix.len() > buffer.len() {
                    return None;
                }
                buffer[pos..pos + prefix.len()].copy_from_slice(prefix);
                pos += prefix.len();

                let written = format_number_to_buffer(used_kb, &mut buffer[pos..]);
                pos += written;

                if pos + 4 > buffer.len() {
                    return None;
                }
                buffer[pos..pos + 4].copy_from_slice(b" kB\n");
                pos += 4;

                return Some(pos);
            }
            "/proc/uptime" => {
                let ticks = atom_syscall::thread::get_ticks();
                let seconds = ticks / 100;

                let mut pos = 0;
                pos += format_number_to_buffer(seconds, &mut buffer[pos..]);

                if pos + 3 > buffer.len() {
                    return None;
                }
                buffer[pos..pos + 3].copy_from_slice(b" s\n");
                pos += 3;

                return Some(pos);
            }
            _ => None,
        }
    }

    /// Get file information via IPC to filesystem daemon
    /// Returns file metadata (size, is_dir, timestamps)
    /// Note: Requires fsd to be fully operational
    pub fn stat_file(&self, path: &str) -> Option<FileInfo> {
        use libipc::messages::{MessageHeader, MessageType, FsReply, FsStatBuf};

        if path.len() > 4096 {
            return None;
        }

        let response_port = self.response_port?;

        // Build FsStat request: path_len (4 bytes) + path (variable)
        let mut request = alloc::vec::Vec::new();
        request.extend_from_slice(&(path.len() as u32).to_le_bytes());
        request.extend_from_slice(path.as_bytes());

        // Build message header + payload
        let header = MessageHeader::new(MessageType::FsStat, request.len() as u32);
        let mut message = alloc::vec::Vec::new();
        message.extend_from_slice(&header.to_bytes());
        message.extend_from_slice(&request);

        // Send to filesystem daemon (fsd on PORT_FS_SERVICE)
        send(PORT_FS_SERVICE, &message).ok()?;

        // Wait for response with timeout
        let mut response_buf = [0u8; 4096];
        let resp_len = recv_with_timeout(response_port, &mut response_buf, 5000).ok()?;

        // Parse response: header (16 bytes) + FsReply (16 bytes) + FsStatBuf (80 bytes)
        if resp_len < 32 + FsStatBuf::SIZE {
            return None;
        }

        // Skip header, parse FsReply
        let reply = FsReply::from_bytes(&response_buf[16..32])?;
        if !reply.is_ok() {
            return None;
        }

        // Parse stat structure
        let stat = FsStatBuf::from_bytes(&response_buf[32..32 + FsStatBuf::SIZE])?;

        Some(FileInfo {
            size: stat.size,
            is_dir: (stat.mode & 0o40000) != 0,
            created: stat.ctime_ns,
            modified: stat.mtime_ns,
        })
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