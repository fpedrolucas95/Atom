//! Name Service (namesvc)
//!
//! A simple name service for Atom OS that provides service discovery.
//! Services register their IPC ports here, and clients can look them up by name.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use atom_syscall::ipc::PortId;
use libipc::messages::{MessageType, NsRegisterMsg, NsLookupMsg, NsResponseMsg, MessageHeader};
use libipc::protocol::{send_message, try_recv_message, get_payload};

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
            // Bump allocator doesn't free
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Name Registry
// ============================================================================

const MAX_SERVICES: usize = 64;
const MAX_NAME_LEN: usize = 32;
const MAX_PENDING_LOOKUPS: usize = 32;

/// Represents a lookup request that arrived before the service was registered
#[derive(Clone)]
struct PendingLookup {
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    reply_port: u64,
    active: bool,
}

impl PendingLookup {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NAME_LEN],
            name_len: 0,
            reply_port: 0,
            active: false,
        }
    }

    fn matches_name(&self, name: &str) -> bool {
        if !self.active || name.len() != self.name_len {
            return false;
        }
        &self.name[..self.name_len] == name.as_bytes()
    }
}

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

    fn matches(&self, name: &str) -> bool {
        if !self.active || name.len() != self.name_len {
            return false;
        }
        &self.name[..self.name_len] == name.as_bytes()
    }
}

struct NameRegistry {
    entries: [ServiceEntry; MAX_SERVICES],
}

impl NameRegistry {
    const fn new() -> Self {
        const EMPTY: ServiceEntry = ServiceEntry::empty();
        Self {
            entries: [EMPTY; MAX_SERVICES],
        }
    }

    fn register(&mut self, name: &str, port: u64) -> Result<(), &'static str> {
        if name.len() > MAX_NAME_LEN {
            return Err("name too long");
        }

        // Update existing or find empty slot
        for entry in self.entries.iter_mut() {
            if entry.matches(name) {
                entry.port = port;
                return Ok(());
            }
        }

        for entry in self.entries.iter_mut() {
            if !entry.active {
                entry.name[..name.len()].copy_from_slice(name.as_bytes());
                entry.name_len = name.len();
                entry.port = port;
                entry.active = true;
                return Ok(());
            }
        }

        Err("registry full")
    }

    fn lookup(&self, name: &str) -> Option<u64> {
        for entry in self.entries.iter() {
            if entry.matches(name) {
                return Some(entry.port);
            }
        }
        None
    }
}

static mut REGISTRY: NameRegistry = NameRegistry::new();

/// Queue of pending lookup requests (waiting for services to register)
struct PendingQueue {
    lookups: [PendingLookup; MAX_PENDING_LOOKUPS],
}

impl PendingQueue {
    const fn new() -> Self {
        const EMPTY: PendingLookup = PendingLookup::empty();
        Self {
            lookups: [EMPTY; MAX_PENDING_LOOKUPS],
        }
    }

    /// Add a pending lookup request
    fn add(&mut self, name: &str, reply_port: u64) -> Result<(), &'static str> {
        if name.len() > MAX_NAME_LEN {
            return Err("name too long");
        }

        for lookup in self.lookups.iter_mut() {
            if !lookup.active {
                lookup.name[..name.len()].copy_from_slice(name.as_bytes());
                lookup.name_len = name.len();
                lookup.reply_port = reply_port;
                lookup.active = true;
                return Ok(());
            }
        }

        Err("pending queue full")
    }

    /// Get all pending lookups for a service name and mark them inactive
    fn get_and_clear(&mut self, name: &str) -> [Option<u64>; MAX_PENDING_LOOKUPS] {
        let mut results = [None; MAX_PENDING_LOOKUPS];
        let mut idx = 0;

        for lookup in self.lookups.iter_mut() {
            if lookup.active && lookup.matches_name(name) {
                results[idx] = Some(lookup.reply_port);
                idx += 1;
                lookup.active = false;
            }
        }

        results
    }
}

static mut PENDING_QUEUE: PendingQueue = PendingQueue::new();


// ============================================================================
// Main Service Loop
// ============================================================================

fn name_to_str(name: &[u8; 32]) -> &str {
    let len = name.iter().position(|&b| b == 0).unwrap_or(32);
    core::str::from_utf8(&name[..len]).unwrap_or("")
}

fn handle_request(header: MessageHeader, payload: &[u8]) {
    match header.msg_type {
        MessageType::NsRegister => {
            if let Some(msg) = NsRegisterMsg::from_bytes(payload) {
                atom_syscall::debug::log("namesvc: registering service");
                let name = name_to_str(&msg.name);
                let _ = unsafe { REGISTRY.register(name, msg.port) };

                // Check if there are any pending lookups for this service
                let pending = unsafe { PENDING_QUEUE.get_and_clear(name) };
                for reply_port_opt in pending.iter() {
                    if let Some(reply_port) = reply_port_opt {
                        let resp = NsResponseMsg { port: msg.port };
                        let _ = send_message(*reply_port, MessageType::NsResponse, &resp.to_bytes());
                        let debug_msg = alloc::format!(
                            "namesvc: responded to pending lookup for '{}' with port {}",
                            name, msg.port
                        );
                        atom_syscall::debug::log(&debug_msg);
                    }
                }
            }
        }
        MessageType::NsLookup => {
            if let Some(msg) = NsLookupMsg::from_bytes(payload) {
                atom_syscall::debug::log("namesvc: lookup service");
                let name = name_to_str(&msg.name);

                // Try immediate lookup
                if let Some(found_port) = unsafe { REGISTRY.lookup(name) } {
                    let resp = NsResponseMsg { port: found_port };
                    let _ = send_message(msg.reply_port, MessageType::NsResponse, &resp.to_bytes());
                    let debug_msg = alloc::format!(
                        "namesvc: immediate lookup for '{}' returned port {}",
                        name, found_port
                    );
                    atom_syscall::debug::log(&debug_msg);
                } else {
                    // Service not found - add to pending queue
                    if unsafe { PENDING_QUEUE.add(name, msg.reply_port) }.is_ok() {
                        let debug_msg = alloc::format!(
                            "namesvc: lookup for '{}' queued as pending",
                            name
                        );
                        atom_syscall::debug::log(&debug_msg);
                    } else {
                        // Queue full - respond with error
                        let resp = NsResponseMsg { port: 0 };
                        let _ = send_message(msg.reply_port, MessageType::NsResponse, &resp.to_bytes());
                        atom_syscall::debug::log("namesvc: pending queue full, returning error");
                    }
                }
            }
        }
        _ => {}
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    // Create our service port with the reserved well-known ID (Port 1).
    // This uses create_port_with_id() for deterministic assignment instead of
    // relying on being the first process to call create_port().
    let port = match atom_syscall::ipc::create_port_with_id(1) {
        Ok(p) => {
            atom_syscall::debug::log("namesvc: created port with reserved ID 1");
            p
        }
        Err(_) => {
            // Fallback: try regular create_port if reserved allocation fails
            atom_syscall::debug::log("namesvc: WARN - reserved Port(1) busy, using dynamic port allocation");
            match atom_syscall::ipc::create_port() {
                Ok(p) => {
                    let msg = alloc::format!("namesvc: allocated dynamic port {}", p);
                    atom_syscall::debug::log(&msg);
                    p
                }
                Err(_) => loop { atom_syscall::thread::sleep_ms(1000); }
            }
        }
    };

    atom_syscall::debug::log("namesvc: service port ready, entering main loop");

    // Self-register
    unsafe {
        let _ = REGISTRY.register("namesvc", port);
    }

    let mut buffer = [0u8; 512];

    loop {
        let ports = [port];
        match atom_syscall::ipc::wait_any(&ports, 1000) {
            Ok(_) => {
                if let Ok(Some((header, len))) = try_recv_message(port, &mut buffer) {
                    let payload = get_payload(&buffer, len);
                    handle_request(header, payload);
                }
            }
            Err(_) => {}
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    atom_syscall::debug::log("namesvc: PANIC!");
    loop { atom_syscall::thread::yield_now(); }
}
