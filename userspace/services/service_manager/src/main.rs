//! Service Manager (service_manager)
//!
//! The service manager is responsible for:
//! - Spawning services
//! - Monitoring service health (restart on crash)
//! - Registering services with the name service
//! - Providing an IPC API for service control
//!
//! ## API (via IPC messages)
//!
//! - StartService(name) - Start a service by name
//! - StopService(name) - Stop a running service
//! - RestartService(name) - Restart a service
//! - ListServices() - List all managed services and their status
//! - ServiceStatus(name) - Get status of a specific service
//!
//! ## Well-known port: 2 (SERVICE_MANAGER_PORT, reserved)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

// Simple bump allocator for userspace
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 128 * 1024; // 128 KB heap

    #[repr(align(4096))]
    struct Heap {
        data: UnsafeCell<[u8; HEAP_SIZE]>,
        next: UnsafeCell<usize>,
    }

    unsafe impl Sync for Heap {}

    static HEAP: Heap = Heap {
        data: UnsafeCell::new([0; HEAP_SIZE]),
        next: UnsafeCell::new(0),
    };

    pub struct BumpAllocator;

    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let next = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;

            let align = layout.align();
            let size = layout.size();

            let current = *next;
            let aligned = (current + align - 1) & !(align - 1);

            if aligned + size > HEAP_SIZE {
                return null_mut();
            }

            *next = aligned + size;
            heap_start.add(aligned)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator doesn't free
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Service Definitions
// ============================================================================

/// Maximum number of managed services
const MAX_SERVICES: usize = 32;
/// Maximum service name length
const MAX_NAME_LEN: usize = 32;

/// Service state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ServiceState {
    /// Service is not running and not managed
    Stopped = 0,
    /// Service is starting up
    Starting = 1,
    /// Service is running normally
    Running = 2,
    /// Service has crashed and will be restarted
    Crashed = 3,
    /// Service is being stopped
    Stopping = 4,
    /// Service failed to start
    Failed = 5,
}

impl ServiceState {
    fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Stopped),
            1 => Some(Self::Starting),
            2 => Some(Self::Running),
            3 => Some(Self::Crashed),
            4 => Some(Self::Stopping),
            5 => Some(Self::Failed),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Crashed => "crashed",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }
}

/// Restart policy for a service
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RestartPolicy {
    /// Never restart
    Never = 0,
    /// Restart on crash only
    OnCrash = 1,
    /// Always restart (even on clean exit)
    Always = 2,
}

/// A managed service
#[derive(Clone)]
struct ManagedService {
    /// Service name (used for spawn)
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    /// Process ID (0 if not running)
    pid: u64,
    /// IPC port (0 if not registered)
    port: u64,
    /// Current state
    state: ServiceState,
    /// Restart policy
    restart_policy: RestartPolicy,
    /// Number of restarts
    restart_count: u32,
    /// Last start time (ticks)
    last_start: u64,
    /// Is this slot active?
    active: bool,
}

impl ManagedService {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NAME_LEN],
            name_len: 0,
            pid: 0,
            port: 0,
            state: ServiceState::Stopped,
            restart_policy: RestartPolicy::OnCrash,
            restart_count: 0,
            last_start: 0,
            active: false,
        }
    }

    fn name_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len]) }
    }

    fn set_name(&mut self, name: &str) {
        let len = name.len().min(MAX_NAME_LEN);
        self.name[..len].copy_from_slice(&name.as_bytes()[..len]);
        self.name_len = len;
    }
}

/// Service registry
struct ServiceRegistry {
    services: [ManagedService; MAX_SERVICES],
    count: usize,
}

impl ServiceRegistry {
    const fn new() -> Self {
        const EMPTY: ManagedService = ManagedService::empty();
        Self {
            services: [EMPTY; MAX_SERVICES],
            count: 0,
        }
    }

    /// Find a service by name
    fn find(&self, name: &str) -> Option<usize> {
        for (i, svc) in self.services.iter().enumerate() {
            if svc.active && svc.name_len == name.len() {
                if &svc.name[..svc.name_len] == name.as_bytes() {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Find a service by PID
    fn find_by_pid(&self, pid: u64) -> Option<usize> {
        for (i, svc) in self.services.iter().enumerate() {
            if svc.active && svc.pid == pid {
                return Some(i);
            }
        }
        None
    }

    /// Register a new service (does not start it)
    fn register(&mut self, name: &str, policy: RestartPolicy) -> Result<usize, &'static str> {
        if name.len() > MAX_NAME_LEN {
            return Err("name too long");
        }

        // Check if already exists
        if self.find(name).is_some() {
            return Err("already registered");
        }

        // Find empty slot
        for (i, svc) in self.services.iter_mut().enumerate() {
            if !svc.active {
                svc.set_name(name);
                svc.state = ServiceState::Stopped;
                svc.restart_policy = policy;
                svc.restart_count = 0;
                svc.pid = 0;
                svc.port = 0;
                svc.active = true;
                self.count += 1;
                return Ok(i);
            }
        }

        Err("registry full")
    }

    /// Get mutable reference to service
    fn get_mut(&mut self, idx: usize) -> Option<&mut ManagedService> {
        if idx < MAX_SERVICES && self.services[idx].active {
            Some(&mut self.services[idx])
        } else {
            None
        }
    }

    /// Get reference to service
    fn get(&self, idx: usize) -> Option<&ManagedService> {
        if idx < MAX_SERVICES && self.services[idx].active {
            Some(&self.services[idx])
        } else {
            None
        }
    }

    /// List all active services
    fn list(&self) -> Vec<(String, ServiceState, u64)> {
        let mut result = Vec::new();
        for svc in self.services.iter() {
            if svc.active {
                result.push((
                    String::from(svc.name_str()),
                    svc.state,
                    svc.pid,
                ));
            }
        }
        result
    }
}

// Global registry
static mut REGISTRY: ServiceRegistry = ServiceRegistry::new();

// ============================================================================
// Service Control
// ============================================================================

/// Start a service by spawning its process
fn start_service(name: &str) -> Result<u64, &'static str> {
    // Find or register the service
    let idx = unsafe {
        match REGISTRY.find(name) {
            Some(i) => i,
            None => REGISTRY.register(name, RestartPolicy::OnCrash)?,
        }
    };

    let svc = unsafe { REGISTRY.get_mut(idx).unwrap() };

    // Check if already running
    if svc.state == ServiceState::Running || svc.state == ServiceState::Starting {
        return Err("already running");
    }

    // Update state
    svc.state = ServiceState::Starting;
    svc.last_start = atom_syscall::thread::get_ticks();

    // Spawn the process
    match atom_syscall::process::spawn_process(name) {
        Ok(pid) => {
            svc.pid = pid;
            svc.state = ServiceState::Running;
            atom_syscall::debug::log("svcmgr: spawned service");
            Ok(pid)
        }
        Err(_) => {
            svc.state = ServiceState::Failed;
            atom_syscall::debug::log("svcmgr: failed to spawn service");
            Err("spawn failed")
        }
    }
}

/// Stop a service
fn stop_service(name: &str) -> Result<(), &'static str> {
    let idx = unsafe {
        REGISTRY.find(name).ok_or("not found")?
    };

    let svc = unsafe { REGISTRY.get_mut(idx).unwrap() };

    if svc.state != ServiceState::Running {
        return Err("not running");
    }

    svc.state = ServiceState::Stopping;

    // TODO: Send termination signal to process via IPC
    // For now, just mark as stopped
    svc.state = ServiceState::Stopped;
    svc.pid = 0;

    Ok(())
}

/// Check service health and restart if needed
fn check_services() {
    // Get list of all processes
    let mut proc_buffer = [atom_syscall::process::ProcessInfo::empty(); 32];
    let proc_count = atom_syscall::process::list_processes(&mut proc_buffer);

    unsafe {
        for svc in REGISTRY.services.iter_mut() {
            if !svc.active || svc.state != ServiceState::Running {
                continue;
            }

            // Check if process is still alive
            let mut found = false;
            for i in 0..proc_count {
                if proc_buffer[i].pid == svc.pid {
                    found = true;
                    break;
                }
            }

            if !found {
                // Process died
                svc.state = ServiceState::Crashed;
                svc.pid = 0;
                atom_syscall::debug::log("svcmgr: service crashed");

                // Check restart policy
                if svc.restart_policy == RestartPolicy::OnCrash
                    || svc.restart_policy == RestartPolicy::Always
                {
                    svc.restart_count += 1;
                    // Restart it
                    let name_copy = svc.name_str();
                    let _ = start_service(name_copy);
                }
            }
        }
    }
}

// ============================================================================
// IPC Protocol
// ============================================================================

/// Service manager message types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmMessageType {
    /// Start a service: name (string)
    StartService = 700,
    /// Stop a service: name (string)
    StopService = 701,
    /// Restart a service: name (string)
    RestartService = 702,
    /// List all services
    ListServices = 703,
    /// Get service status: name (string)
    ServiceStatus = 704,
    /// Register service port: name (string), port (u64)
    RegisterPort = 705,
    /// Response: success with data
    Response = 710,
    /// Response: error
    Error = 711,
}

impl SmMessageType {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            700 => Some(Self::StartService),
            701 => Some(Self::StopService),
            702 => Some(Self::RestartService),
            703 => Some(Self::ListServices),
            704 => Some(Self::ServiceStatus),
            705 => Some(Self::RegisterPort),
            710 => Some(Self::Response),
            711 => Some(Self::Error),
            _ => None,
        }
    }
}

/// Parse name-only request: [msg_type: u32][name_len: u32][name: bytes]
fn parse_name_request(data: &[u8]) -> Option<&str> {
    if data.len() < 8 {
        return None;
    }
    let name_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if data.len() < 8 + name_len {
        return None;
    }
    core::str::from_utf8(&data[8..8 + name_len]).ok()
}

/// Parse RegisterPort: [msg_type: u32][port: u64][name_len: u32][name: bytes]
fn parse_register_port(data: &[u8]) -> Option<(&str, u64)> {
    if data.len() < 16 {
        return None;
    }
    let port = u64::from_le_bytes([
        data[4], data[5], data[6], data[7],
        data[8], data[9], data[10], data[11],
    ]);
    let name_len = u32::from_le_bytes([data[12], data[13], data[14], data[15]]) as usize;
    if data.len() < 16 + name_len {
        return None;
    }
    let name = core::str::from_utf8(&data[16..16 + name_len]).ok()?;
    Some((name, port))
}

/// Build success response: [msg_type: u32][pid: u64]
fn build_success_response(pid: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&(SmMessageType::Response as u32).to_le_bytes());
    buf[4..12].copy_from_slice(&pid.to_le_bytes());
    buf
}

/// Build error response: [msg_type: u32][error_code: u32]
fn build_error_response(code: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&(SmMessageType::Error as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&code.to_le_bytes());
    buf
}

/// Build status response: [msg_type: u32][state: u8][pid: u64][restart_count: u32]
fn build_status_response(state: ServiceState, pid: u64, restart_count: u32) -> [u8; 17] {
    let mut buf = [0u8; 17];
    buf[0..4].copy_from_slice(&(SmMessageType::Response as u32).to_le_bytes());
    buf[4] = state as u8;
    buf[5..13].copy_from_slice(&pid.to_le_bytes());
    buf[13..17].copy_from_slice(&restart_count.to_le_bytes());
    buf
}

/// Build list response
fn build_list_response(services: &[(String, ServiceState, u64)]) -> Vec<u8> {
    // Format: [msg_type: u32][count: u32][entries...]
    // Entry: [state: u8][pid: u64][name_len: u32][name: bytes]
    let mut buf = Vec::new();
    buf.extend_from_slice(&(SmMessageType::Response as u32).to_le_bytes());
    buf.extend_from_slice(&(services.len() as u32).to_le_bytes());

    for (name, state, pid) in services {
        buf.push(*state as u8);
        buf.extend_from_slice(&pid.to_le_bytes());
        buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
    }

    buf
}

fn handle_message(data: &[u8]) -> Vec<u8> {
    if data.len() < 4 {
        return build_error_response(1).to_vec();
    }

    let msg_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

    match SmMessageType::from_u32(msg_type) {
        Some(SmMessageType::StartService) => {
            if let Some(name) = parse_name_request(data) {
                match start_service(name) {
                    Ok(pid) => build_success_response(pid).to_vec(),
                    Err(_) => build_error_response(2).to_vec(),
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(SmMessageType::StopService) => {
            if let Some(name) = parse_name_request(data) {
                match stop_service(name) {
                    Ok(()) => build_success_response(0).to_vec(),
                    Err(_) => build_error_response(3).to_vec(),
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(SmMessageType::RestartService) => {
            if let Some(name) = parse_name_request(data) {
                let _ = stop_service(name);
                match start_service(name) {
                    Ok(pid) => build_success_response(pid).to_vec(),
                    Err(_) => build_error_response(2).to_vec(),
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(SmMessageType::ListServices) => {
            let services = unsafe { REGISTRY.list() };
            build_list_response(&services)
        }
        Some(SmMessageType::ServiceStatus) => {
            if let Some(name) = parse_name_request(data) {
                if let Some(idx) = unsafe { REGISTRY.find(name) } {
                    let svc = unsafe { REGISTRY.get(idx).unwrap() };
                    build_status_response(svc.state, svc.pid, svc.restart_count).to_vec()
                } else {
                    build_error_response(4).to_vec() // Not found
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(SmMessageType::RegisterPort) => {
            if let Some((name, port)) = parse_register_port(data) {
                if let Some(idx) = unsafe { REGISTRY.find(name) } {
                    let svc = unsafe { REGISTRY.get_mut(idx).unwrap() };
                    svc.port = port;
                    build_success_response(port).to_vec()
                } else {
                    build_error_response(4).to_vec()
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        _ => build_error_response(5).to_vec(), // Unknown message
    }
}

// ============================================================================
// Built-in Services Configuration
// ============================================================================

/// Initialize default service registry entries.
/// Note: The init process (PID 1) is responsible for spawning all services.
/// The service_manager only tracks and supervises services that have been
/// spawned by init. It does NOT spawn services at boot - init does that.
fn init_default_services() {
    atom_syscall::debug::log("svcmgr: registering known service names");

    // Pre-register known service names for monitoring
    // Init will spawn these; service_manager tracks their lifecycle
    let known_services: &[&str] = &[
        "namesvc",
        "keyboard",
        "mouse",
        "display",
        "ui_shell",
        "terminal",
    ];

    for name in known_services {
        unsafe {
            let _ = REGISTRY.register(name, RestartPolicy::OnCrash);
        }
    }

    atom_syscall::debug::log("svcmgr: service registry ready");
}

// ============================================================================
// Main Service Loop
// ============================================================================

/// Well-known port for the service manager (reserved ID 2)
pub const SERVICE_MANAGER_PORT: u64 = 2;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    atom_syscall::debug::log("svcmgr: Service Manager starting");

    // Create our IPC port with the reserved well-known ID (Port 2)
    let port = match atom_syscall::ipc::create_port_with_id(SERVICE_MANAGER_PORT) {
        Ok(p) => p,
        Err(_) => {
            atom_syscall::debug::log("svcmgr: WARN - reserved Port(2) failed, using dynamic port");
            match atom_syscall::ipc::create_port() {
                Ok(p) => p,
                Err(_) => {
                    atom_syscall::debug::log("svcmgr: failed to create port");
                    loop {
                        atom_syscall::thread::sleep_ms(1000);
                    }
                }
            }
        }
    };

    atom_syscall::debug::log("svcmgr: port created");

    // Initialize and start default services
    init_default_services();

    // Message buffer
    let mut buffer = [0u8; 256];
    let mut last_health_check = atom_syscall::thread::get_ticks();

    // Main loop - use blocking receive with timeout
    loop {
        // Wait for messages with 1 second timeout
        // This allows periodic health checks without busy-looping
        let ports = [port];
        match atom_syscall::ipc::wait_any(&ports, 1000) {
            Ok(_idx) => {
                // Message available - receive it
                if let Ok(Some(len)) = atom_syscall::ipc::try_recv(port, &mut buffer) {
                    let response = handle_message(&buffer[..len]);
                    // TODO: Send response back to sender
                    let _ = response;
                }
            }
            Err(_) => {
                // Timeout - this is expected for periodic health checks
            }
        }

        // Check service health every ~10 seconds
        let current_ticks = atom_syscall::thread::get_ticks();
        if current_ticks.saturating_sub(last_health_check) >= 1000 {
            check_services();
            last_health_check = current_ticks;
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    atom_syscall::debug::log("svcmgr: PANIC!");
    loop {
        atom_syscall::thread::sleep_ms(1000);
    }
}
