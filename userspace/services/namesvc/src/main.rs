//! Name Service (namesvc)
//!
//! Service discovery for Atom OS. Services register their IPC ports here and
//! clients look them up by name.
//!
//! # Authenticated registration (PR2)
//!
//! `namesvc` no longer trusts the registration payload. Every `NsRegister` /
//! `NsUnregister` is authorised using the kernel-generated [`IpcEnvelope`]:
//!
//!   1. the sender must have a kernel-assigned `ServiceId`
//!      (`envelope.sender_service_id != 0`);
//!   2. the requested name must be the canonical name or a declared alias for
//!      that identity (checked against the kernel `SystemServiceManifest` via
//!      `service_name_allowed`);
//!   3. the announced port must actually be owned by the sender process
//!      (`port_owner(port) == envelope.sender_process`);
//!   4. an existing registration whose owner is still alive cannot be
//!      overwritten; it may only be replaced once the kernel confirms the
//!      previous owner is gone (`process_alive == false`).
//!
//! `Unregister` is likewise authenticated: only the current owner process may
//! remove its own registration.
//!
//! There is no insecure dynamic fallback: if `namesvc` cannot bind reserved
//! `Port(1)`, the boot fails loudly.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use atom_syscall::ipc::{
    self, port_owner, process_alive, recv_envelope, service_name_allowed, IpcEnvelope, PortId,
};
use libipc::messages::{
    MessageHeader, MessageType, NsLookupMsg, NsRegisterMsg, NsResponseMsg, NsUnregisterMsg,
};
use libipc::protocol::{get_payload, send_message};

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

/// Reserved well-known port for namesvc itself.
const NAMESVC_PORT: u64 = 1;

fn log(msg: &str) {
    atom_syscall::debug::log(msg);
}

#[derive(Clone)]
struct ServiceEntry {
    name: [u8; MAX_NAME_LEN],
    name_len: usize,
    port: u64,
    /// Process that owns this registration. Authenticated from the kernel IPC
    /// envelope at registration time; used for overwrite/unregister checks.
    owner_process: u64,
    active: bool,
}

impl ServiceEntry {
    const fn empty() -> Self {
        Self {
            name: [0; MAX_NAME_LEN],
            name_len: 0,
            port: 0,
            owner_process: 0,
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

/// Outcome of an authenticated registration attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegisterOutcome {
    /// New registration created.
    Registered,
    /// Same owner + same port re-registered; no state change.
    Idempotent,
    /// Previous owner confirmed dead; registration replaced.
    Replaced,
    /// Name is owned by a live process — refuse to overwrite (EEXIST).
    Conflict,
    /// Registry is full.
    Full,
}

impl NameRegistry {
    const fn new() -> Self {
        const EMPTY: ServiceEntry = ServiceEntry::empty();
        Self {
            entries: [EMPTY; MAX_SERVICES],
        }
    }

    fn find(&self, name: &str) -> Option<&ServiceEntry> {
        self.entries.iter().find(|e| e.matches(name))
    }

    fn write_slot(slot: &mut ServiceEntry, name: &str, port: u64, owner_process: u64) {
        slot.name[..name.len()].copy_from_slice(name.as_bytes());
        slot.name_len = name.len();
        slot.port = port;
        slot.owner_process = owner_process;
        slot.active = true;
    }

    /// Insert or replace `name`, enforcing the no-overwrite-of-live-owner rule.
    ///
    /// `owner_alive` is a kernel-confirmed liveness check for the *current*
    /// owner of an existing registration (only consulted on conflict).
    fn try_register(
        &mut self,
        name: &str,
        port: u64,
        owner_process: u64,
        owner_alive: impl Fn(u64) -> bool,
    ) -> RegisterOutcome {
        if name.len() > MAX_NAME_LEN {
            return RegisterOutcome::Full;
        }

        // Existing registration?
        if let Some(idx) = self.entries.iter().position(|e| e.matches(name)) {
            let existing = &self.entries[idx];
            if existing.owner_process == owner_process && existing.port == port {
                return RegisterOutcome::Idempotent;
            }
            if owner_alive(existing.owner_process) {
                return RegisterOutcome::Conflict;
            }
            // Previous owner is gone; the name allowlist already guaranteed the
            // new registrant shares the same service identity, so replacement
            // is safe.
            Self::write_slot(&mut self.entries[idx], name, port, owner_process);
            return RegisterOutcome::Replaced;
        }

        // Fresh name → first empty slot.
        if let Some(slot) = self.entries.iter_mut().find(|e| !e.active) {
            Self::write_slot(slot, name, port, owner_process);
            return RegisterOutcome::Registered;
        }

        RegisterOutcome::Full
    }

    /// Remove a registration if `requester` is its current owner.
    /// Returns true if removed.
    fn try_unregister(&mut self, name: &str, requester: u64) -> bool {
        if let Some(slot) = self.entries.iter_mut().find(|e| e.matches(name)) {
            if slot.owner_process == requester {
                *slot = ServiceEntry::empty();
                return true;
            }
        }
        false
    }

    /// Unconditional internal registration, used only for namesvc's own
    /// self-registration where identity is established by the kernel having
    /// granted it `Port(1)`.
    fn register_self(&mut self, name: &str, port: u64, owner_process: u64) {
        if let Some(slot) = self.entries.iter_mut().find(|e| !e.active) {
            Self::write_slot(slot, name, port, owner_process);
        }
    }

    fn lookup(&self, name: &str) -> Option<u64> {
        self.find(name).map(|e| e.port)
    }
}

static mut REGISTRY: NameRegistry = NameRegistry::new();

// ============================================================================
// Main Service Loop
// ============================================================================

fn name_to_str(name: &[u8; 32]) -> &str {
    let len = name.iter().position(|&b| b == 0).unwrap_or(32);
    core::str::from_utf8(&name[..len]).unwrap_or("")
}

/// Authenticate and apply a registration request using the kernel envelope.
fn handle_register(env: &IpcEnvelope, name: &str, port: u64) {
    // (1) Sender must carry a kernel-assigned service identity.
    if env.sender_service_id == 0 {
        log("namesvc: DENY register — sender has no ServiceIdentity (EPERM)");
        return;
    }

    // (2) The name must be allowed for that identity (kernel manifest allowlist).
    if !service_name_allowed(env.sender_service_id, name) {
        let m = alloc::format!(
            "namesvc: DENY register — service_id {} not allowed name '{}' (EPERM)",
            env.sender_service_id,
            name
        );
        log(&m);
        return;
    }

    // (3) The announced port must actually belong to the sender process.
    let real_owner = port_owner(port);
    if real_owner == 0 || real_owner != env.sender_process {
        let m = alloc::format!(
            "namesvc: DENY register — port {} not owned by sender process {} (owner={}) (EPERM)",
            port,
            env.sender_process,
            real_owner
        );
        log(&m);
        return;
    }

    // (4) Apply, refusing to overwrite a live owner.
    let outcome = unsafe {
        REGISTRY.try_register(name, port, env.sender_process, process_alive)
    };

    match outcome {
        RegisterOutcome::Conflict => {
            let m = alloc::format!(
                "namesvc: DENY register — '{}' already owned by a live process (EEXIST)",
                name
            );
            log(&m);
            return;
        }
        RegisterOutcome::Full => {
            log("namesvc: register failed — registry full");
            return;
        }
        RegisterOutcome::Idempotent => {
            let m = alloc::format!("namesvc: '{}' re-registered (idempotent)", name);
            log(&m);
        }
        RegisterOutcome::Replaced => {
            let m = alloc::format!(
                "namesvc: '{}' replaced after previous owner exit (port {})",
                name,
                port
            );
            log(&m);
        }
        RegisterOutcome::Registered => {
            let m = alloc::format!("namesvc: registered '{}' on port {}", name, port);
            log(&m);
        }
    }

}

/// Authenticate and apply an unregister request.
fn handle_unregister(env: &IpcEnvelope, name: &str) {
    if env.sender_service_id == 0 {
        log("namesvc: DENY unregister — sender has no ServiceIdentity (EPERM)");
        return;
    }
    let removed = unsafe { REGISTRY.try_unregister(name, env.sender_process) };
    if removed {
        let m = alloc::format!("namesvc: unregistered '{}'", name);
        log(&m);
    } else {
        let m = alloc::format!(
            "namesvc: DENY unregister — '{}' not owned by sender process {} (EPERM)",
            name,
            env.sender_process
        );
        log(&m);
    }
}

fn handle_request(env: &IpcEnvelope, header: MessageHeader, payload: &[u8]) {
    match header.msg_type {
        MessageType::NsRegister => {
            if let Some(msg) = NsRegisterMsg::from_bytes(payload) {
                let name = name_to_str(&msg.name);
                handle_register(env, name, msg.port);
            }
        }
        MessageType::NsUnregister => {
            if let Some(msg) = NsUnregisterMsg::from_bytes(payload) {
                let name = name_to_str(&msg.name);
                handle_unregister(env, name);
            }
        }
        MessageType::NsLookup => {
            if let Some(msg) = NsLookupMsg::from_bytes(payload) {
                let name = name_to_str(&msg.name);
                // Lookup is public (it grants no authority).
                if let Some(found_port) = unsafe { REGISTRY.lookup(name) } {
                    let resp = NsResponseMsg { port: found_port };
                    let _ = send_message(msg.reply_port, MessageType::NsResponse, &resp.to_bytes());
                } else {
                    // Missing services are reported immediately. Keeping the
                    // caller's temporary reply port queued after its timeout
                    // leaked entries and eventually stalled service discovery.
                    let resp = NsResponseMsg { port: 0 };
                    let _ = send_message(msg.reply_port, MessageType::NsResponse, &resp.to_bytes());
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
    // Bind the reserved well-known port (Port 1). This requires the
    // ReservedPort(1) capability granted by the kernel manifest. There is NO
    // dynamic fallback: if we cannot own Port(1), the system cannot boot.
    let port: PortId = match ipc::create_port_with_id(NAMESVC_PORT) {
        Ok(p) => {
            log("namesvc: bound reserved Port(1)");
            p
        }
        Err(_) => {
            log("namesvc: FATAL — could not bind reserved Port(1); refusing dynamic fallback");
            panic!("namesvc cannot bind Port(1)");
        }
    };

    // Self-register. Our identity is established by the kernel having granted us
    // Port(1); the owner is whoever the kernel says owns that port (i.e. us).
    let self_owner = port_owner(port);
    unsafe {
        REGISTRY.register_self("namesvc", port, self_owner);
    }

    log("namesvc: service port ready, entering main loop");

    let mut buffer = [0u8; 512];
    let mut envelope = IpcEnvelope::default();

    loop {
        let ports = [port];
        match ipc::wait_any(&ports, 1000) {
            Ok(_) => {
                while let Ok(Some(len)) = recv_envelope(port, &mut envelope, &mut buffer) {
                    if len < MessageHeader::SIZE {
                        continue;
                    }
                    if let Some(header) = MessageHeader::from_bytes(&buffer[..MessageHeader::SIZE]) {
                        let payload = get_payload(&buffer, len);
                        handle_request(&envelope, header, payload);
                    }
                }
            }
            Err(_) => {}
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
