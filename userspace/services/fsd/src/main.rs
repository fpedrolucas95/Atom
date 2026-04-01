//! Filesystem Daemon (fsd)
//!
//! The filesystem daemon is responsible for:
//! - Creating and managing the filesystem IPC port (PORT_FS_SERVICE = 3)
//! - Registering with namesvc for service discovery
//! - Mounting the root FAT32 volume on startup
//! - Receiving filesystem requests via IPC and routing through the VFS

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

    const HEAP_SIZE: usize = 512 * 1024; // 512 KB heap for fsd

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
            let size  = layout.size();

            let current = *next;
            let aligned = (current + align - 1) & !(align - 1);

            if aligned + size > HEAP_SIZE {
                return null_mut();
            }

            *next = aligned + size;
            heap_start.add(aligned)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ── Module declarations ───────────────────────────────────────────────────────

mod block_client;
mod block_device;      // kept for journal; not used by FatFs directly
mod cache;
mod capabilities;
mod error;
mod fat32;
mod fat32_ops;
mod fd_table;
mod ipc;
mod journal;
mod journal_tests;
mod mounts;
mod vfs;

use atom_abi::PORT_FS_SERVICE;
use block_client::BlockClient;
use capabilities::CapabilityEnforcer;
use fat32::FatFs;
use ipc::FsdIpcHandler;
use vfs::{MountFlags, Vfs};

// ── Logging ───────────────────────────────────────────────────────────────────

fn log(msg: &str) { atom_syscall::debug::log(msg); }

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

fn main() -> ! {
    log("========================================");
    log("Filesystem Daemon (fsd) starting");
    log("========================================");

    // ── Create FS service port ────────────────────────────────────────────

    let fs_port = match atom_syscall::ipc::create_port_with_id(PORT_FS_SERVICE) {
        Ok(p) => {
            log(&format!("fsd: created FS port {}", p));
            p
        }
        Err(_) => match atom_syscall::ipc::create_port() {
            Ok(p) => {
                log(&format!("fsd: allocated dynamic FS port {}", p));
                p
            }
            Err(_) => {
                log("fsd: ERROR — cannot create FS port");
                loop { atom_syscall::thread::yield_now(); }
            }
        },
    };

    // Register with namesvc
    if let Err(_) = libipc::protocol::register_service("fsd", fs_port) {
        log("fsd: WARNING — failed to register with namesvc");
    }

    // ── Build Vfs + CapabilityEnforcer ────────────────────────────────────

    let mut vfs  = Vfs::new();
    let mut caps = CapabilityEnforcer::new();

    // ── Mount root FAT32 volume ───────────────────────────────────────────
    //
    // Step 1: identify the block device to get sector geometry.
    // Step 2: mount FAT32 at "/".  Partition starts at LBA 0 for an
    //         unpartitioned image; adjust if booting from a partitioned disk.
    // Step 3: log the parsed BPB parameters (Phase 1 checkpoint).

    let mut block = BlockClient::new(0 /*device_id: first disk*/);

    // Step 1 — Identify block device
    match block.identify() {
        Ok(info) => {
            let model = core::str::from_utf8(&info.model)
                .unwrap_or("Unknown")
                .trim_end_matches(' ');
            log(&format!(
                "fsd: block device \"{}\" — {} sectors × {} B/sector ({} MiB){}",
                model,
                info.total_sectors,
                info.sector_size,
                info.total_sectors * info.sector_size as u64 / (1024 * 1024),
                if info.read_only != 0 { " [READ-ONLY]" } else { "" },
            ));
        }
        Err(e) => {
            // Block service may not be ready yet; continue with default 512 B/sector.
            // FatVolume::mount() will fail cleanly if the device is truly absent.
            log(&format!("fsd: WARNING — block identify failed ({:?}), assuming 512 B/sector", e));
        }
    }

    // Step 2 — Mount FAT32
    match FatFs::mount(block, 0 /*partition_start*/, false /*rw*/, 0 /*mount_id*/) {
        Ok(fat_fs) => {
            // Step 3 — Log volume params (Phase 1 checkpoint)
            for line in fat32::bpb::format_volume_summary(fat_fs.volume_params(), "fsd: ") {
                log(&line);
            }
            if fat_fs.volume_was_dirty() {
                log("fsd: WARNING — volume was not cleanly unmounted (dirty bit set)");
            }

            let flags = MountFlags { read_only: false, no_exec: false, synchronous: false };
            match vfs.mount("/", flags, "/dev/block0", alloc::boxed::Box::new(fat_fs)) {
                Ok(mid) => log(&format!("fsd: FAT32 mounted at / (mount_id {})", mid)),
                Err(e)  => log(&format!("fsd: WARNING — VFS mount failed: {:?}", e)),
            }
        }
        Err(e) => log(&format!("fsd: WARNING — FAT32 mount failed: {:?}", e)),
    }

    log("fsd: ready, entering main loop");

    main_loop(fs_port, &mut vfs, &mut caps);
}

// ── IPC service loop ───────────────────────────────────────────────────────────

fn main_loop(
    fs_port: atom_syscall::ipc::PortId,
    vfs:     &mut Vfs,
    caps:    &mut CapabilityEnforcer,
) -> ! {
    let mut buffer = [0u8; libipc::MAX_MESSAGE_SIZE];

    loop {
        match atom_syscall::ipc::recv(fs_port, &mut buffer) {
            Ok(n) if n >= libipc::messages::MessageHeader::SIZE => {
                let Some(header) = libipc::messages::MessageHeader::from_bytes(&buffer[..n]) else {
                    continue;
                };

                let payload_start = libipc::messages::MessageHeader::SIZE;
                let payload = &buffer[payload_start..n];

                if payload.len() < 8 { continue; }
                let reply_port = u64::from_le_bytes(payload[..8].try_into().unwrap());
                let request_data = &payload[8..];

                // Advance capability TTL tick for every request
                caps.tick();

                let mut handler = FsdIpcHandler::new(vfs, caps);
                let response = handler.handle_request(header.msg_type, request_data);

                if let Err(_) = atom_syscall::ipc::send(reply_port, &response) {
                    log("fsd: failed to send reply");
                }
            }
            Ok(_) => {} // message too small; ignore
            Err(atom_syscall::SyscallError::WouldBlock) |
            Err(atom_syscall::SyscallError::TimedOut) => {
                atom_syscall::thread::yield_now();
            }
            Err(_) => {
                log("fsd: FATAL recv error");
                loop { atom_syscall::thread::yield_now(); }
            }
        }
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    atom_syscall::debug::log("fsd: PANIC");
    loop { atom_syscall::thread::yield_now(); }
}

