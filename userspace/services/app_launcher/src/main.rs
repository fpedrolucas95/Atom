//! App Launcher Service
//!
//! A privileged system service that spawns ATXF applications from the
//! filesystem on behalf of unprivileged clients (e.g. the file manager).
//!
//! # Security model
//!
//! `app_launcher` is the *only* process that calls `SYS_SPAWN_FROM_PATH`.
//! Unprivileged processes (fileman, ui_shell, …) never call that syscall
//! directly; instead they send an `AppLaunchRequest` IPC message to this
//! service and receive an `AppLaunchReply` with the result.
//!
//! This separation means that:
//!   - The file manager cannot spawn arbitrary processes on its own.
//!   - The launcher can enforce policies (extension check, path sanitisation,
//!     future allow-lists, …) before forwarding the request to the kernel.
//!   - Capabilities required for process creation remain confined to this
//!     single, auditable service.
//!
//! # IPC protocol
//!
//! The service registers itself as "app_launcher" with the name service and
//! accepts `AppLaunchRequest` (type 1200) messages.  Each request carries:
//!   - `reply_port`  — port to send the `AppLaunchReply` (type 1201) back to
//!   - `path`        — absolute filesystem path to the `.atxf` binary
//!
//! The service validates the request, calls `SYS_SPAWN_FROM_PATH`, and sends
//! a reply with either the new PID or a descriptive error code + message.
//!
//! # Boot integration
//!
//! `init` spawns this service during Phase 1 (core services), right after
//! `service_manager`.  The service must be ready before the file manager or
//! any other client that might double-click an `.atxf` file.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::thread::exit;
use atom_syscall::debug::log;
use atom_syscall::process::spawn_from_path;

use libipc::messages::{
    MessageHeader, MessageType,
    AppLaunchRequestMsg, AppLaunchReplyMsg,
    launch_status,
    APP_LAUNCH_VERSION,
};

// ============================================================================
// Bump allocator — 256 KB heap
// ============================================================================

const HEAP_SIZE: usize = 256 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0u8; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let new_end = aligned + size;
            if new_end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(cur, new_end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    log("app_launcher: FATAL — heap exhausted");
    loop {}
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("app_launcher: PANIC — service crashed");
    loop {}
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

// ============================================================================
// Helpers
// ============================================================================

/// Maximum IPC message buffer.  AppLaunchRequestMsg::SIZE = 264,
/// AppLaunchReplyMsg::SIZE = 76.  Add MessageHeader::SIZE (16) and pad.
const BUF_SIZE: usize = MessageHeader::SIZE + AppLaunchRequestMsg::SIZE + 16;

/// Send a reply to the port embedded in the request.
fn send_reply(reply_port: PortId, reply: &AppLaunchReplyMsg) {
    let hdr = MessageHeader::new(MessageType::AppLaunchReply, AppLaunchReplyMsg::SIZE as u32);
    let mut buf = [0u8; MessageHeader::SIZE + AppLaunchReplyMsg::SIZE];
    buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    buf[MessageHeader::SIZE..].copy_from_slice(&reply.to_bytes());
    if let Err(_) = send(reply_port, &buf) {
        log("app_launcher: WARNING — failed to deliver reply to client");
    }
}

// ============================================================================
// Request handler
// ============================================================================

fn handle_launch_request(buf: &[u8], len: usize) {
    // ── Parse the request payload ──────────────────────────────────────────
    let payload_start = MessageHeader::SIZE;
    if len < payload_start + AppLaunchRequestMsg::SIZE {
        log("app_launcher: received truncated AppLaunchRequest — ignoring");
        return;
    }
    let req = match AppLaunchRequestMsg::from_bytes(&buf[payload_start..]) {
        Some(r) => r,
        None => {
            log("app_launcher: failed to deserialise AppLaunchRequest — ignoring");
            return;
        }
    };

    let reply_port = req.reply_port as PortId;

    // ── Protocol version check ─────────────────────────────────────────────
    if req.protocol_version != APP_LAUNCH_VERSION {
        log("app_launcher: unsupported protocol version in request");
        send_reply(
            reply_port,
            &AppLaunchReplyMsg::error(
                launch_status::LAUNCH_ERR_INTERNAL,
                "unsupported protocol version",
            ),
        );
        return;
    }

    // ── Extract path ───────────────────────────────────────────────────────
    let path = match req.path_str() {
        Some(p) if !p.is_empty() => p,
        _ => {
            log("app_launcher: request contained invalid or empty path");
            send_reply(
                reply_port,
                &AppLaunchReplyMsg::error(
                    launch_status::LAUNCH_ERR_BADPATH,
                    "path is invalid or empty",
                ),
            );
            return;
        }
    };

    // ── Validate extension ─────────────────────────────────────────────────
    if !path.ends_with(".atxf") {
        let msg = format!("'{}': not an .atxf file", path);
        log("app_launcher: rejected launch — file is not .atxf");
        send_reply(
            reply_port,
            &AppLaunchReplyMsg::error(launch_status::LAUNCH_ERR_BADPATH, &msg),
        );
        return;
    }

    // Log the request
    {
        let log_msg = format!("app_launcher: launching '{}'", path);
        log(&log_msg);
    }

    // ── Invoke the kernel syscall ──────────────────────────────────────────
    match spawn_from_path(path) {
        Ok(pid) => {
            let msg = format!("app_launcher: '{}' started with PID {}", path, pid);
            log(&msg);
            send_reply(reply_port, &AppLaunchReplyMsg::success(pid));
        }
        Err(e) => {
            // Translate the SyscallError to a launch_status code and a
            // human-readable message that the UI can surface to the user.
            use atom_syscall::error::SyscallError;
            let (status, text) = match e {
                SyscallError::NotFound => (
                    launch_status::LAUNCH_ERR_NOTFOUND,
                    "file not found on filesystem",
                ),
                SyscallError::InvalidArgument => (
                    launch_status::LAUNCH_ERR_INVALID,
                    "file is not a valid ATXF image",
                ),
                SyscallError::OutOfMemory => (
                    launch_status::LAUNCH_ERR_NOMEM,
                    "out of memory — cannot spawn process",
                ),
                // NotImplemented is used for ENOTSUP (wrong extension) and
                // EIO (FAT32 driver not ready) — both are mapped here by the wrapper.
                SyscallError::NotImplemented => (
                    launch_status::LAUNCH_ERR_NOFS,
                    "filesystem unavailable or wrong file type",
                ),
                _ => (
                    launch_status::LAUNCH_ERR_INTERNAL,
                    "internal error while spawning process",
                ),
            };
            let log_msg = format!("app_launcher: ERROR launching '{}' — {}", path, text);
            log(&log_msg);
            send_reply(reply_port, &AppLaunchReplyMsg::error(status, text));
        }
    }
}

// ============================================================================
// Main service loop
// ============================================================================

fn main() -> ! {
    log("app_launcher: starting");

    // Create our listening port
    let port = match create_port() {
        Ok(p) => p,
        Err(_) => {
            log("app_launcher: FATAL — could not create IPC port");
            exit(1);
        }
    };

    // Register with the name service so clients can discover us
    if let Err(_) = libipc::protocol::register_service("app_launcher", port) {
        log("app_launcher: WARNING — failed to register with name service; clients may not find us");
    } else {
        log("app_launcher: registered as 'app_launcher'");
    }

    log("app_launcher: ready — waiting for launch requests");

    let mut buf = [0u8; BUF_SIZE];
    let ports = [port];

    loop {
        // Drain all pending messages before blocking
        loop {
            match try_recv(port, &mut buf) {
                Ok(Some(len)) => {
                    if len < MessageHeader::SIZE {
                        continue; // too short, skip
                    }
                    let hdr = match MessageHeader::from_bytes(&buf) {
                        Some(h) => h,
                        None => continue,
                    };
                    match hdr.msg_type {
                        MessageType::AppLaunchRequest => {
                            handle_launch_request(&buf, len);
                        }
                        other => {
                            let log_msg = format!(
                                "app_launcher: unexpected message type {:?} — ignoring",
                                other as u32
                            );
                            log(&log_msg);
                        }
                    }
                }
                Ok(None) => break,     // no more messages
                Err(_) => break,       // port error, stop draining
            }
        }

        // Block until the next message arrives (100 ms timeout)
        let _ = wait_any(&ports, 100);
    }
}
