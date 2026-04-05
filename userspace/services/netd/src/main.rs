//! Network Daemon (netd)
//!
//! Responsible for running the TCP/IP stack (lwIP in later phases)
//! and managing the network interface. For Phase 1, it implements
//! a simple IPC protocol with the `nic_driver` and provides a loopback
//! test.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use atom_syscall::debug::log;
use libring::{RingHeader, RingEntry, RingProducer, RingConsumer};
use libipc::protocol::{register_service, lookup_service, send_message, try_recv_message};
use libipc::messages::{MessageType, NetAssignRingsMsg};

// Simple bump allocator
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 512 * 1024; // 512 KB

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
        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    log("netd: starting network daemon");

    // 1. Create shared memory region for Ring Buffers
    // Layout: [TX Header] [RX Header] [TX Entries] [RX Entries]
    let ring_capacity = 32;
    let header_size = core::mem::size_of::<RingHeader>();
    let entries_size = ring_capacity * core::mem::size_of::<RingEntry>();
    let region_size = (header_size * 2 + entries_size * 2 + 4095) & !4095;

    let region_id = match atom_syscall::shared_mem::create_region(region_size) {
        Ok(id) => id,
        Err(_) => {
            log("netd: FAILED to create shared region");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    let va = match atom_syscall::shared_mem::map_region(region_id, 0, atom_syscall::shared_mem::RegionFlags::READ | atom_syscall::shared_mem::RegionFlags::WRITE) {
        Ok(v) => v,
        Err(_) => {
            log("netd: FAILED to map shared region");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    log("netd: shared memory mapped for rings");

    // 2. Initialize Rings
    let tx_header_ptr = va as *mut RingHeader;
    let rx_header_ptr = (va + header_size as u64) as *mut RingHeader;
    let tx_entries_ptr = (va + (header_size * 2) as u64) as *mut RingEntry;
    let rx_entries_ptr = (va + (header_size * 2 + entries_size) as u64) as *mut RingEntry;

    unsafe {
        core::ptr::write_bytes(va as *mut u8, 0, region_size);
        (*tx_header_ptr).capacity = ring_capacity;
        (*rx_header_ptr).capacity = ring_capacity;
    }

    let mut tx_producer = RingProducer::new(unsafe { &*tx_header_ptr }, unsafe { core::slice::from_raw_parts_mut(tx_entries_ptr, ring_capacity) });
    let mut rx_consumer = RingConsumer::new(unsafe { &*rx_header_ptr }, unsafe { core::slice::from_raw_parts(rx_entries_ptr, ring_capacity) });

    // 3. Register with Namesvc
    let port = atom_syscall::ipc::create_port().expect("failed to create port");
    register_service("netd", port).expect("failed to register netd");

    log("netd: registered with namesvc, waiting for nic_driver");

    // 4. Wait for nic_driver to appear and assign rings
    let mut driver_port = 0;
    while driver_port == 0 {
        if let Ok(p) = lookup_service("nic_driver") {
            driver_port = p;
        } else {
            atom_syscall::thread::sleep_ms(100);
        }
    }
    log("netd: found nic_driver, assigning rings");

    let assign_msg = NetAssignRingsMsg {
        region_id: region_id,
        ring_capacity: ring_capacity as u32,
    };
    send_message(driver_port, MessageType::NetAssignRings, &assign_msg.to_bytes()).ok();

    // 5. Main Event Loop
    let mut buffer = [0u8; 512];
    let mut last_heartbeat = atom_syscall::thread::get_ticks();

    loop {
        let ports = [port];
        match atom_syscall::ipc::wait_any(&ports, 100) {
            Ok(_) => {
                if let Ok(Some((header, _))) = try_recv_message(port, &mut buffer) {
                    if header.msg_type == MessageType::NetDriverReady {
                        log("netd: nic_driver reported READY");
                    }
                }
            }
            Err(_) => {}
        }

        // Logical Loopback Test:
        // Every 2 seconds, send a "Heartbeat" packet to nic_driver via TX ring,
        // and check if we received it back in RX ring.
        let now = atom_syscall::thread::get_ticks();
        if now - last_heartbeat > 200 { // 2 seconds (100Hz)
            last_heartbeat = now;

            if tx_producer.can_push() {
                let mut entry = RingEntry {
                    len: 9,
                    flags: 0,
                    offset: 0,
                    data: [0u8; 1528],
                };
                entry.data[0..9].copy_from_slice(b"HEARTBEAT");
                tx_producer.push(entry).ok();
                log("netd: sent HEARTBEAT packet to TX ring");
            }
        }

        while rx_consumer.can_pop() {
            if let Some(entry) = rx_consumer.pop() {
                if entry.len >= 9 && &entry.data[0..9] == b"HEARTBEAT" {
                    log("netd: RECEIVED HEARTBEAT packet back from RX ring! (Loopback OK)");
                }
            }
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log("netd: PANIC!");
    if let Some(loc) = info.location() {
        log(loc.file());
    }
    loop {
        atom_syscall::thread::sleep_ms(1000);
    }
}
