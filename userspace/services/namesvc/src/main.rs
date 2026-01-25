//! Name Service (namesvc)
//!
//! A simple name service for Atom OS that provides service discovery.
//! Services register their IPC ports here, and clients can look them up by name.
//!
//! ## API (via IPC messages)
//!
//! - Register(name, port) - Register a service name with a port
//! - Lookup(name) - Find the port for a service
//! - List() - List all registered services
//!
//! ## Well-known port: 100 (NAME_SERVICE_PORT)

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

    const HEAP_SIZE: usize = 64 * 1024; // 64 KB heap

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
            // Bump allocator doesn't free - simple but effective for services
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Name Registry
// ============================================================================

/// Maximum number of registered services
const MAX_SERVICES: usize = 64;
/// Maximum length of service name
const MAX_NAME_LEN: usize = 32;

/// A registered service entry
#[derive(Clone)]
struct ServiceEntry {
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    port: u64,
    active: bool,
}

impl ServiceEntry {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NAME_LEN],
            name_len: 0,
            port: 0,
            active: false,
        }
    }

    fn name_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len]) }
    }

    fn matches(&self, name: &str) -> bool {
        if !self.active || name.len() != self.name_len {
            return false;
        }
        &self.name[..self.name_len] == name.as_bytes()
    }
}

/// Name registry - stores all registered services
struct NameRegistry {
    entries: [ServiceEntry; MAX_SERVICES],
    count: usize,
}

impl NameRegistry {
    const fn new() -> Self {
        const EMPTY: ServiceEntry = ServiceEntry::empty();
        Self {
            entries: [EMPTY; MAX_SERVICES],
            count: 0,
        }
    }

    /// Register a service (or update existing registration)
    fn register(&mut self, name: &str, port: u64) -> Result<(), &'static str> {
        if name.len() > MAX_NAME_LEN {
            return Err("name too long");
        }

        // Check if already registered - update port
        for entry in self.entries.iter_mut() {
            if entry.matches(name) {
                entry.port = port;
                return Ok(());
            }
        }

        // Find empty slot
        for entry in self.entries.iter_mut() {
            if !entry.active {
                entry.name[..name.len()].copy_from_slice(name.as_bytes());
                entry.name_len = name.len();
                entry.port = port;
                entry.active = true;
                self.count += 1;
                return Ok(());
            }
        }

        Err("registry full")
    }

    /// Unregister a service
    fn unregister(&mut self, name: &str) -> bool {
        for entry in self.entries.iter_mut() {
            if entry.matches(name) {
                entry.active = false;
                entry.name_len = 0;
                entry.port = 0;
                self.count -= 1;
                return true;
            }
        }
        false
    }

    /// Look up a service by name
    fn lookup(&self, name: &str) -> Option<u64> {
        for entry in self.entries.iter() {
            if entry.matches(name) {
                return Some(entry.port);
            }
        }
        None
    }

    /// List all registered services
    fn list(&self) -> Vec<(String, u64)> {
        let mut result = Vec::new();
        for entry in self.entries.iter() {
            if entry.active {
                result.push((String::from(entry.name_str()), entry.port));
            }
        }
        result
    }
}

// Global registry
static mut REGISTRY: NameRegistry = NameRegistry::new();

// ============================================================================
// IPC Protocol
// ============================================================================

/// Name service message types
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsMessageType {
    /// Register a service: name (string), port (u64)
    Register = 600,
    /// Unregister a service: name (string)
    Unregister = 601,
    /// Lookup a service: name (string) -> port (u64)
    Lookup = 602,
    /// List all services -> array of (name, port)
    List = 603,
    /// Response: success with data
    Response = 610,
    /// Response: error with message
    Error = 611,
}

impl NsMessageType {
    fn from_u32(v: u32) -> Option<Self> {
        match v {
            600 => Some(Self::Register),
            601 => Some(Self::Unregister),
            602 => Some(Self::Lookup),
            603 => Some(Self::List),
            610 => Some(Self::Response),
            611 => Some(Self::Error),
            _ => None,
        }
    }
}

/// Parse a Register message: [msg_type: u32][port: u64][name_len: u32][name: bytes]
fn parse_register(data: &[u8]) -> Option<(&str, u64)> {
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

/// Parse a Lookup/Unregister message: [msg_type: u32][name_len: u32][name: bytes]
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

/// Build a response message with port: [msg_type: u32][port: u64]
fn build_port_response(port: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[0..4].copy_from_slice(&(NsMessageType::Response as u32).to_le_bytes());
    buf[4..12].copy_from_slice(&port.to_le_bytes());
    buf
}

/// Build an error response: [msg_type: u32][error_code: u32]
fn build_error_response(code: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[0..4].copy_from_slice(&(NsMessageType::Error as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&code.to_le_bytes());
    buf
}

/// Build a list response
fn build_list_response(entries: &[(String, u64)]) -> Vec<u8> {
    // Format: [msg_type: u32][count: u32][entries...]
    // Entry: [port: u64][name_len: u32][name: bytes]
    let mut buf = Vec::new();
    buf.extend_from_slice(&(NsMessageType::Response as u32).to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for (name, port) in entries {
        buf.extend_from_slice(&port.to_le_bytes());
        buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
    }

    buf
}

// ============================================================================
// Main Service Loop
// ============================================================================

/// Well-known port for the name service
pub const NAME_SERVICE_PORT: u64 = 100;

fn handle_message(data: &[u8], _sender_port: u64) -> Vec<u8> {
    if data.len() < 4 {
        return build_error_response(1).to_vec(); // Invalid message
    }

    let msg_type = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

    match NsMessageType::from_u32(msg_type) {
        Some(NsMessageType::Register) => {
            if let Some((name, port)) = parse_register(data) {
                let result = unsafe { REGISTRY.register(name, port) };
                match result {
                    Ok(()) => build_port_response(port).to_vec(),
                    Err(_) => build_error_response(2).to_vec(), // Registry full
                }
            } else {
                build_error_response(1).to_vec() // Parse error
            }
        }
        Some(NsMessageType::Unregister) => {
            if let Some(name) = parse_name_request(data) {
                let success = unsafe { REGISTRY.unregister(name) };
                if success {
                    build_port_response(0).to_vec()
                } else {
                    build_error_response(3).to_vec() // Not found
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(NsMessageType::Lookup) => {
            if let Some(name) = parse_name_request(data) {
                match unsafe { REGISTRY.lookup(name) } {
                    Some(port) => build_port_response(port).to_vec(),
                    None => build_error_response(3).to_vec(), // Not found
                }
            } else {
                build_error_response(1).to_vec()
            }
        }
        Some(NsMessageType::List) => {
            let entries = unsafe { REGISTRY.list() };
            build_list_response(&entries)
        }
        _ => build_error_response(4).to_vec(), // Unknown message type
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    // Create our service port
    // Note: In the future, we might use a well-known port ID
    let port = match atom_syscall::ipc::create_port() {
        Ok(p) => p,
        Err(_) => {
            // Can't create port - fatal
            loop {
                atom_syscall::thread::sleep_ms(1000);
            }
        }
    };

    // Log startup (via debug syscall if available)
    atom_syscall::debug::log("namesvc: started");

    // Self-register as the name service
    unsafe {
        let _ = REGISTRY.register("namesvc", port);
    }

    // Message buffer
    let mut buffer = [0u8; 256];

    // Main service loop - use blocking receive with timeout
    loop {
        // Use wait_any with timeout to avoid busy-loop
        let ports = [port];
        match atom_syscall::ipc::wait_any(&ports, 100) {
            Ok(_idx) => {
                // Message available - receive it
                if let Ok(Some(len)) = atom_syscall::ipc::try_recv(port, &mut buffer) {
                    let response = handle_message(&buffer[..len], 0);
                    // Send response back (we'd need the sender's port in a real implementation)
                    // For now, we'll rely on request-response port being encoded in message
                    let _ = response; // Response handling TBD
                }
            }
            Err(_) => {
                // Timeout or error - just continue
                // This allows periodic maintenance if needed
            }
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    atom_syscall::debug::log("namesvc: PANIC!");
    loop {
        atom_syscall::thread::yield_now();
    }
}
