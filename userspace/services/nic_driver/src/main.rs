//! VirtIO-net Legacy Driver (Userland)
//!
//! Implements a basic VirtIO-net driver running in userspace.
//! Uses the new infrastructure syscalls for PCI, IRQ, and DMA.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use atom_syscall::debug::log;
use libring::{RingHeader, RingEntry, RingProducer, RingConsumer};
use libipc::protocol::{register_service, try_recv_message, get_payload};
use libipc::messages::{MessageType, NetAssignRingsMsg};

// Simple bump allocator for userspace
mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 256 * 1024; // 256 KB heap

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

// ============================================================================
// VirtIO-net Legacy Constants
// ============================================================================

const VIRTIO_PCI_STATUS:         u16 = 18;
const VIRTIO_STATUS_ACK:         u8 = 1;
const VIRTIO_STATUS_DRIVER:      u8 = 2;

// ============================================================================
// Driver Implementation
// ============================================================================

struct DriverState<'a> {
    io_base: u16,
    irq_cap: u64,
    tx_consumer: Option<RingConsumer<'a>>,
    rx_producer: Option<RingProducer<'a>>,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    log("nic_driver: starting VirtIO-net Legacy driver");

    // 1. Find Device Capability
    let mut device_cap = 0;
    for h in 1..20 {
        if atom_syscall::cap::check(h, 1) != 0 {
            device_cap = h;
            break;
        }
    }

    if device_cap == 0 {
        log("nic_driver: FAILED to find DeviceCap");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }

    // 2. Get BAR info
    let mut bar_info = atom_syscall::atom_abi::PciBarInfo::default();
    if atom_syscall::raw::pci_get_bar(device_cap, 0, &mut bar_info) != 0 {
        log("nic_driver: FAILED to get BAR0 info");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }

    let io_base = bar_info.base_addr as u16;

    // 3. Initialize VirtIO Device (Skeleton)
    unsafe {
        atom_syscall::io::port_write_u8(io_base + VIRTIO_PCI_STATUS, 0).ok();
        atom_syscall::io::port_write_u8(io_base + VIRTIO_PCI_STATUS, VIRTIO_STATUS_ACK).ok();
        atom_syscall::io::port_write_u8(io_base + VIRTIO_PCI_STATUS, VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER).ok();
    }

    // 4. Bind IRQ
    let mut irq_info = atom_syscall::atom_abi::IrqBindInfo::default();
    atom_syscall::raw::device_bind_irq(device_cap, &mut irq_info);

    // 5. Register with Namesvc
    let port = atom_syscall::ipc::create_port().expect("failed to create port");
    register_service("nic_driver", port).expect("failed to register nic_driver");

    log("nic_driver: initialized, waiting for netd");

    let mut state = DriverState {
        io_base,
        irq_cap: irq_info.irq_cap,
        tx_consumer: None,
        rx_producer: None,
    };

    // 6. Main Loop
    let mut buffer = [0u8; 512];
    loop {
        let ports = [port];
        match atom_syscall::ipc::wait_any(&ports, 100) {
            Ok(_) => {
                if let Ok(Some((header, len))) = try_recv_message(port, &mut buffer) {
                    if header.msg_type == MessageType::NetAssignRings {
                        let payload = get_payload(&buffer, len);
                        if let Some(msg) = NetAssignRingsMsg::from_bytes(payload) {
                            log("nic_driver: received rings assignment");
                            setup_rings(&mut state, msg);
                        }
                    }
                }
            }
            Err(_) => {}
        }

        // Logical Loopback: Polling TX Ring and reflecting to RX Ring
        if let (Some(tx_cons), Some(rx_prod)) = (&mut state.tx_consumer, &mut state.rx_producer) {
            while tx_cons.can_pop() && rx_prod.can_push() {
                if let Some(entry) = tx_cons.pop() {
                    log("nic_driver: looping back packet");
                    rx_prod.push(entry).ok();
                }
            }
        }
    }
}

fn setup_rings(state: &mut DriverState, msg: NetAssignRingsMsg) {
    let region_id = msg.region_id;
    let va = match atom_syscall::shared_mem::map_region(region_id, 0, atom_syscall::shared_mem::RegionFlags::READ | atom_syscall::shared_mem::RegionFlags::WRITE) {
        Ok(v) => v,
        Err(_) => {
            log("nic_driver: FAILED to map netd shared region");
            return;
        }
    };

    let ring_capacity = msg.ring_capacity as usize;
    let header_size = core::mem::size_of::<RingHeader>();
    let entries_size = ring_capacity * core::mem::size_of::<RingEntry>();

    let tx_header_ptr = va as *const RingHeader;
    let rx_header_ptr = (va + header_size as u64) as *const RingHeader;
    let tx_entries_ptr = (va + (header_size * 2) as u64) as *const RingEntry;
    let rx_entries_ptr = (va + (header_size * 2 + entries_size) as u64) as *mut RingEntry;

    state.tx_consumer = Some(RingConsumer::new(unsafe { &*tx_header_ptr }, unsafe { core::slice::from_raw_parts(tx_entries_ptr, ring_capacity) }));
    state.rx_producer = Some(RingProducer::new(unsafe { &*rx_header_ptr }, unsafe { core::slice::from_raw_parts_mut(rx_entries_ptr, ring_capacity) }));

    log("nic_driver: rings configured");
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("nic_driver: PANIC!");
    loop {
        atom_syscall::thread::sleep_ms(1000);
    }
}
