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
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use alloc::format;

// Reusable, mmap-backed heap allocator (see allocator.rs). Replaces the old
// irreversible bump allocator: memory freed by one request is reclaimed and
// reused by the next, so a flood of requests no longer grows memory
// monotonically until an irreversible crash.
mod allocator;
// Defensive per-request limits (path/payload/read/write/handles/...).
mod limits;

/// Exit codes used when `fsd` terminates so it can be restarted by
/// `service_manager`.
const EXIT_OOM: u64 = 12; // ENOMEM
const EXIT_FATAL: u64 = 70;

/// Out-of-memory handler.
///
/// An infallible allocation failed (the reusable allocator returned null after
/// the pool and every oversized mapping were exhausted). This is treated as an
/// *irrecoverable* daemon state: we log a machine-readable line and exit
/// cleanly so `service_manager` observes the process death and restarts a fresh
/// `fsd`, which re-claims the reserved FS port and re-registers with `namesvc`.
/// We deliberately do NOT spin-loop here — a panic/spin loop would wedge the
/// filesystem permanently.
#[alloc_error_handler]
fn oom(_layout: core::alloc::Layout) -> ! {
    log("fsd: ENOMEM — heap exhausted, exiting for restart");
    atom_syscall::thread::exit(EXIT_OOM);
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
mod backend;

use ipc::FsdIpcHandler;
use backend::UserspaceFatBackend;
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
                atom_syscall::thread::exit(EXIT_FATAL);
            }

            // Check if recovery is needed from previous crash
            if jm.recovery_needed {
                log("fsd: WARNING - running crash recovery...");
                if !jm.recovery_replay() {
                    log("fsd: ERROR - recovery failed");
                    atom_syscall::thread::exit(EXIT_FATAL);
                }
                log("fsd: recovery completed successfully");
            }

            JOURNAL_MANAGER = Some(jm);
        }
    }

    // Create the filesystem service on the well-known fixed port.
    // Kernel FS syscalls route to this ID, so dynamic fallback is incorrect.
    let fs_port = match atom_syscall::ipc::create_port_with_id(PORT_FS_SERVICE) {
        Ok(port) => {
            log(&format!("fsd: created FS port with reserved ID {}", port));
            port
        }
        Err(_) => {
            log("fsd: ERROR - failed to claim reserved FS port; refusing dynamic fallback");
            atom_syscall::thread::exit(EXIT_FATAL);
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
            atom_syscall::thread::exit(EXIT_FATAL);
        }
    }

    // Initialize mount table with root filesystem
    let mut mounts_manager = mounts::MountsManager::new();
    log(&format!(
        "fsd: mounted {} mount points",
        mounts_manager.active_mount_count()
    ));

    // Initialize userspace-owned FAT32 backend on top of raw block I/O.
    if !fat32::init() {
        log("fsd: ERROR - failed to initialize userspace FAT32 backend");
        atom_syscall::thread::exit(EXIT_FATAL);
    }

    // Create IPC handler with pluggable backend implementation.
    let backend = UserspaceFatBackend;
    let mut ipc_handler = FsdIpcHandler::new(&mut mounts_manager, &backend);

    log("fsd: ready to serve filesystem requests (journal logging active; active data path owned by fsd userspace FAT32)");

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
    // Allocate receive buffer on the heap to avoid consuming stack space
    let mut buffer = alloc::vec![0u8; libipc::MAX_MESSAGE_SIZE];

    log("fsd: entering main loop");

    loop {
        // Block until a request arrives (u64::MAX = no deadline).
        // The kernel wakes this thread the moment a message is queued to fs_port.
        match atom_syscall::ipc::recv_with_timeout(fs_port, &mut buffer, u64::MAX) {
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

                match header.msg_type {
                    // Heap-free fast-path: large sequential reads must not
                    // allocate per chunk because fsd uses a bump allocator.
                    libipc::messages::MessageType::FsRead => {
                        let response = ipc_handler.handle_fs_read_noalloc(request_data);
                        if let Err(_) = atom_syscall::ipc::send(reply_port, response) {
                            log("fsd: failed to send read reply");
                        }
                    }
                    _ => {
                        // Dispatch to handler — returns response bytes (FsReply format)
                        let response = ipc_handler.handle_request(header.msg_type, request_data);

                        // Send raw response bytes back through the reply port.
                        // No MessageHeader wrapping — the kernel expects [error(8)|value(8)|data...]
                        if let Err(_) = atom_syscall::ipc::send(reply_port, &response) {
                            log("fsd: failed to send reply");
                        }
                    }
                }
            }
            Err(_e) => {
                log("fsd: FATAL recv error, terminating for restart");
                atom_syscall::thread::exit(EXIT_FATAL);
            }
        }
    }
}

// ============================================================================
// Panic handler
// ============================================================================

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if let Some(loc) = info.location() {
        log(&format!(
            "fsd: PANIC at {}:{}:{}",
            loc.file(),
            loc.line(),
            loc.column()
        ));
    } else {
        log("fsd: PANIC (location unavailable)");
    }
    log(&format!("fsd: PANIC message: {}", info.message()));
    // Exit cleanly so service_manager restarts a fresh fsd instead of leaving
    // the daemon wedged in a spin loop with the FS port held.
    atom_syscall::thread::exit(EXIT_FATAL);
}
