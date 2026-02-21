//! Filesystem Daemon (fsd)
//!
//! The filesystem daemon is responsible for:
//! - Creating and managing the filesystem IPC port (PORT_FS_SERVICE = 3)
//! - Registering with namesvc for service discovery
//! - Receiving filesystem requests via IPC
//! - Routing requests through the VFS subsystem
//! - Returning replies with status and data
//!
//! ## Well-known port: 3 (PORT_FS_SERVICE)
//!
//! Handles requests from userspace applications and syscall handlers
//! for file operations (open, read, write, close, stat, readdir, etc.)

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use alloc::format;

// Simple bump allocator for userspace
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 256 * 1024; // 256 KB heap for fsd

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
// Global state for journaling
// ============================================================================

static mut JOURNAL_MANAGER: Option<JournalManager> = None;
static mut BLOCK_DEVICE: Option<BlockDevice> = None;

pub fn get_journal_manager() -> Option<&'static mut JournalManager> {
    unsafe { JOURNAL_MANAGER.as_mut() }
}

pub fn get_block_device() -> Option<&'static BlockDevice> {
    unsafe { BLOCK_DEVICE.as_ref() }
}

// ============================================================================
// Logging helper
// ============================================================================

fn log(msg: &str) {
    atom_syscall::debug::log(msg);
}

// ============================================================================
// Module declarations
// ============================================================================

mod ipc;
mod vfs;
mod mounts;
mod fat32;
mod journal;
mod block_device;
mod fat32_ops;

use ipc::FsdIpcHandler;
use journal::JournalManager;
use block_device::BlockDevice;
use atom_abi::PORT_FS_SERVICE;

// ============================================================================
// Main entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("========================================");
    log("Filesystem Daemon (fsd) starting");
    log("========================================");

    // Initialize block device and journal manager for crash-consistent operations
    unsafe {
        BLOCK_DEVICE = Some(BlockDevice::new(512, 1_000_000));

        if let Some(ref bd) = BLOCK_DEVICE {
            let mut jm = JournalManager::new(bd as *const BlockDevice);
            if !jm.init() {
                log("fsd: ERROR - failed to initialize journal, exiting");
                loop {
                    atom_syscall::thread::yield_now();
                }
            }

            // Check if recovery is needed from previous crash
            if jm.recovery_needed {
                log("fsd: WARNING - running crash recovery...");
                if !jm.recovery_replay() {
                    log("fsd: ERROR - recovery failed");
                    loop {
                        atom_syscall::thread::yield_now();
                    }
                }
                log("fsd: recovery completed successfully");
            }

            JOURNAL_MANAGER = Some(jm);
        }
    }

    // Create the filesystem service port (well-known port 3, with automatic fallback to dynamic)
    let fs_port = match atom_syscall::ipc::create_port_with_id(PORT_FS_SERVICE) {
        Ok(port) => {
            log(&format!("fsd: created FS port with reserved ID {} (dynamic port would have been accepted too)", port));
            port
        }
        Err(_) => {
            // Fallback: if reserved port is busy, use dynamic port assignment
            log("fsd: WARNING - reserved port busy, attempting dynamic port allocation");
            match atom_syscall::ipc::create_port() {
                Ok(port) => {
                    log(&format!("fsd: allocated dynamic FS port ID {}", port));
                    port
                }
                Err(_) => {
                    log("fsd: ERROR - failed to create FS port (both reserved and dynamic), exiting");
                    loop {
                        atom_syscall::thread::yield_now();
                    }
                }
            }
        }
    };

    // Register the port with namesvc for service discovery
    log("fsd: registering with namesvc...");
    if let Err(_) = libipc::protocol::register_service("fsd", fs_port) {
        log("fsd: WARNING - failed to register with namesvc, continuing");
    }

    log("fsd: ready, entering main loop");

    // Initialize VFS subsystem
    match vfs::init_vfs() {
        Ok(()) => log("fsd: VFS subsystem initialized"),
        Err(_) => {
            log("fsd: ERROR - failed to initialize VFS");
            loop {
                atom_syscall::thread::yield_now();
            }
        }
    }

    // Initialize mount table with root filesystem
    let mut mounts_manager = mounts::MountsManager::new();
    log(&format!(
        "fsd: mounted {} mount points",
        mounts_manager.active_mount_count()
    ));

    // Create IPC handler
    let mut ipc_handler = FsdIpcHandler::new(&mut mounts_manager);

    log("fsd: ready to serve filesystem requests with journal-based crash-consistency");

    // Main service loop
    main_loop(fs_port, &mut ipc_handler);
}

/// Main service loop: receive requests, dispatch to handlers, send replies.
/// 
/// Message wire format (from kernel):
///   [MessageHeader(16) | reply_port(8) | request_payload]
///
/// After parsing the header, the payload begins at byte 16.
///   payload[0..8]  = reply_port (LE u64) — where to send the reply
///   payload[8..]   = request-specific data
///
/// The handler returns a serialised response which we send to reply_port.
fn main_loop(fs_port: atom_syscall::ipc::PortId, ipc_handler: &mut FsdIpcHandler) -> ! {
    // Allocate receive buffer once for reuse
    let mut buffer = [0u8; libipc::MAX_MESSAGE_SIZE];

    log("fsd: entering main loop");

    loop {
        // Block until a message arrives
        match atom_syscall::ipc::recv(fs_port, &mut buffer) {
            Ok(bytes_received) => {
                if bytes_received < libipc::messages::MessageHeader::SIZE {
                    log("fsd: received message too small, ignoring");
                    continue;
                }

                // Parse message header (first 16 bytes)
                let header = match libipc::messages::MessageHeader::from_bytes(&buffer[..bytes_received]) {
                    Some(h) => h,
                    None => {
                        log("fsd: failed to parse message header, ignoring");
                        continue;
                    }
                };

                // Payload starts after header
                let payload_start = libipc::messages::MessageHeader::SIZE;
                let payload = &buffer[payload_start..bytes_received];

                // Extract reply_port (first 8 bytes of payload)
                if payload.len() < 8 {
                    log("fsd: payload too small for reply_port, ignoring");
                    continue;
                }
                let reply_port = u64::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                    payload[4], payload[5], payload[6], payload[7],
                ]);

                // The actual request data is after the reply_port
                let request_data = &payload[8..];

                // Dispatch to handler — returns response bytes (FsReply format)
                let response = ipc_handler.handle_request(header.msg_type, request_data);

                // Send raw response bytes back through the reply port.
                // No MessageHeader wrapping — the kernel expects [error(8)|value(8)|data...]
                if let Err(_) = atom_syscall::ipc::send(reply_port, &response) {
                    log("fsd: failed to send reply");
                }
            }
            Err(atom_syscall::SyscallError::WouldBlock) => {
                atom_syscall::thread::yield_now();
            }
            Err(atom_syscall::SyscallError::TimedOut) => {
                atom_syscall::thread::yield_now();
            }
            Err(_e) => {
                log("fsd: FATAL recv error, terminating");
                loop {
                    atom_syscall::thread::yield_now();
                }
            }
        }
    }
}

// ============================================================================
// Panic handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("fsd: PANIC");
    loop {
        atom_syscall::thread::yield_now();
    }
}
