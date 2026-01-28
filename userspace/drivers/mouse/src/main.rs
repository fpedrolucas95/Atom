// Userspace PS/2 Mouse Driver
//
// This driver runs entirely in Ring 3 (userspace) and:
// - Polls raw mouse bytes from kernel input buffer via SYS_MOUSE_POLL
// - Assembles 3-byte PS/2 packets into movement and button events
// - Dispatches mouse events to the desktop environment via IPC
//
// # Architecture
//
// ```text
// Kernel IRQ Buffer ──> Mouse Driver ──> Desktop Environment
//    (raw bytes)         (parsing)       (IPC messages)
// ```

#![no_std]
#![no_main]

extern crate alloc;

// Simple bump allocator for userspace
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 64 * 1024; // 64 KB heap

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self { heap: UnsafeCell::new([0; HEAP_SIZE]), next: AtomicUsize::new(0) }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + layout.size();
            if new_next > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(current, new_next, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

use core::panic::PanicInfo;

// Use the atom_syscall library for all kernel interactions
use atom_syscall::input::MouseDriver;
use atom_syscall::ipc::{create_port, PortId, send};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

use libipc::messages::{MessageType, MouseMoveEvent, MouseButtonEvent, MouseButton};
use libipc::protocol::send_message_async;
use libipc::well_known;

fn main() -> ! {
    log("Mouse Driver: Starting userspace driver");

    // Well-known port: compositor event_port is always port 1
    // (created first in Compositor::new())
    const COMPOSITOR_EVENT_PORT: PortId = 1;

    // Create our IPC port and register with name service
    if let Ok(our_port) = create_port() {
        register_with_namesvc("mouse", our_port);
    }

    let mut driver = MouseDriver::new();
    let mut prev_buttons = [false; 3]; // Left, Right, Middle

    log("Mouse Driver: Entering poll loop");

    loop {
        // Poll for mouse events using syscall 33
        if let Some(event) = driver.poll_event() {
            // 1. Handle movement
            if event.dx != 0 || event.dy != 0 {
                let move_msg = MouseMoveEvent {
                    x: 0, // Absolute position managed by compositor
                    y: 0,
                    dx: event.dx as i16,
                    dy: event.dy as i16,
                };
                let _ = send_message_async(COMPOSITOR_EVENT_PORT, MessageType::MouseMove, &move_msg.to_bytes());
            }

            // 2. Handle buttons
            let current_buttons = [event.left_button, event.right_button, event.middle_button];
            for i in 0..3 {
                if current_buttons[i] != prev_buttons[i] {
                    let msg_type = if current_buttons[i] {
                        MessageType::MouseButtonDown
                    } else {
                        MessageType::MouseButtonUp
                    };

                    let button_msg = MouseButtonEvent {
                        button: MouseButton::from_u8(i as u8).unwrap(),
                        x: 0,
                        y: 0,
                    };
                    let _ = send_message_async(COMPOSITOR_EVENT_PORT, msg_type, &button_msg.to_bytes());
                }
            }
            prev_buttons = current_buttons;
        }

        yield_now();
    }
}

/// Register this service with the name service
fn register_with_namesvc(service_name: &str, port: PortId) {
    let mut msg = [0u8; 64];
    let msg_type = 600u32; // NsRegister
    msg[0..4].copy_from_slice(&msg_type.to_le_bytes());
    msg[4..12].copy_from_slice(&port.to_le_bytes());
    msg[12..16].copy_from_slice(&(service_name.len() as u32).to_le_bytes());
    msg[16..16 + service_name.len()].copy_from_slice(service_name.as_bytes());

    let _ = send(well_known::NAME_SERVICE, &msg[..16 + service_name.len()]);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Mouse Driver: PANIC!");
    exit(0xFF);
}
