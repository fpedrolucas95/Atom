// Userspace PS/2 Mouse Driver for Atom OS
//
// This driver polls raw mouse bytes from the kernel via syscalls and
// dispatches mouse events via IPC to the compositor.
//
// The kernel handles low-level IRQ and hardware initialization.
// This driver handles packet assembly and event routing.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::ipc::{create_port, PortId};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use atom_syscall::input::mouse_poll_byte;

use libipc::messages::{MessageType, MouseMoveEvent, MouseButtonEvent, MouseButton};
use libipc::protocol::{send_message_async, register_service};

// ============================================================================
// Simple Bump Allocator for Userspace
// ============================================================================

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

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! { loop {} }

// ============================================================================
// Mouse State
// ============================================================================

#[derive(Clone, Copy, Default)]
pub struct MouseState {
    pub delta_x: i16,
    pub delta_y: i16,
    pub left_button: bool,
    pub right_button: bool,
    pub middle_button: bool,
}

struct MouseDriver {
    packet: [u8; 3],
    cycle: u8,
    state: MouseState,
    our_port: PortId,
}

impl MouseDriver {
    fn new(our_port: PortId) -> Self {
        Self {
            packet: [0; 3],
            cycle: 0,
            state: MouseState::default(),
            our_port,
        }
    }

    fn run(&mut self) -> ! {
        let mut prev_state = MouseState::default();

        loop {
            // Poll for raw bytes from kernel buffer
            while let Some(byte) = mouse_poll_byte() {
                if let Some(new_state) = self.process_byte(byte) {
                    // Send move event if deltas are non-zero
                    if new_state.delta_x != 0 || new_state.delta_y != 0 {
                        let move_msg = MouseMoveEvent {
                            x: 0, y: 0,
                            dx: new_state.delta_x,
                            dy: new_state.delta_y,
                        };
                        let _ = send_message_async(self.our_port, MessageType::MouseMove, &move_msg.to_bytes());
                    }

                    // Send button events if state changed
                    if new_state.left_button != prev_state.left_button {
                        let msg_type = if new_state.left_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Left, x: 0, y: 0 };
                        let _ = send_message_async(self.our_port, msg_type, &btn_msg.to_bytes());
                    }
                    if new_state.right_button != prev_state.right_button {
                        let msg_type = if new_state.right_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Right, x: 0, y: 0 };
                        let _ = send_message_async(self.our_port, msg_type, &btn_msg.to_bytes());
                    }
                    if new_state.middle_button != prev_state.middle_button {
                        let msg_type = if new_state.middle_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Middle, x: 0, y: 0 };
                        let _ = send_message_async(self.our_port, msg_type, &btn_msg.to_bytes());
                    }

                    prev_state = new_state;
                }
            }
            yield_now();
        }
    }

    fn process_byte(&mut self, byte: u8) -> Option<MouseState> {
        match self.cycle {
            0 => {
                // Bit 3 must be 1 in the first byte of a PS/2 packet
                if byte & 0x08 != 0 {
                    self.packet[0] = byte;
                    self.cycle = 1;
                }
                None
            }
            1 => {
                self.packet[1] = byte;
                self.cycle = 2;
                None
            }
            2 => {
                self.packet[2] = byte;
                self.cycle = 0;

                let flags = self.packet[0];

                // Check for overflow bits
                if (flags & 0xC0) != 0 { return None; }

                // Extract deltas with sign extension
                let mut dx = self.packet[1] as i16;
                if flags & 0x10 != 0 { dx -= 256; }

                let mut dy = self.packet[2] as i16;
                if flags & 0x20 != 0 { dy -= 256; }

                self.state.delta_x = dx;
                self.state.delta_y = dy;
                self.state.left_button = (flags & 0x01) != 0;
                self.state.right_button = (flags & 0x02) != 0;
                self.state.middle_button = (flags & 0x04) != 0;

                Some(self.state)
            }
            _ => { self.cycle = 0; None }
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("Mouse Driver: Starting...");

    // Create our IPC port and register as "mouse" service
    let our_port = match create_port() {
        Ok(port) => port,
        Err(_) => {
            log("Mouse Driver: Failed to create IPC port");
            exit(1);
        }
    };

    if let Err(_) = register_service("mouse", our_port) {
        log("Mouse Driver: Failed to register service");
        exit(1);
    }

    log("Mouse Driver: Service registered, starting poll loop");

    let mut driver = MouseDriver::new(our_port);
    driver.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Mouse Driver: PANIC!");
    exit(0xFF);
}
