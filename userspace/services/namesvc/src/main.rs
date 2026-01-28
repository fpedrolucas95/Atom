//! Name Service (namesvc)
//!
//! A simple name service for Atom OS that provides service discovery.
//! Services register their IPC ports here, and clients can look them up by name.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
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

// ============================================================================
// Main Service Loop
// ============================================================================

fn handle_request(header: MessageHeader, payload: &[u8]) {
    match header.msg_type {
        MessageType::NsRegister => {
            if let Some(msg) = NsRegisterMsg::from_bytes(payload) {
                atom_syscall::debug::log("namesvc: registering service");
                let _ = unsafe { REGISTRY.register(&msg.name, msg.port) };
            }
        }
        MessageType::NsLookup => {
            if let Some(msg) = NsLookupMsg::from_bytes(payload) {
                atom_syscall::debug::log("namesvc: lookup service");
                let found_port = unsafe { REGISTRY.lookup(&msg.name) }.unwrap_or(0);
                let resp = NsResponseMsg { port: found_port };
                let _ = send_message(msg.reply_port, MessageType::NsResponse, &resp.to_bytes());
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
    // Create our service port - this MUST be the first port created in the system to be Port(1)
    let port = match atom_syscall::ipc::create_port() {
        Ok(p) => p,
        Err(_) => loop { atom_syscall::thread::sleep_ms(1000); }
    };

    atom_syscall::debug::log("namesvc: started on Port(1)");

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
