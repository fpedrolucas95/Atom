// Userspace PS/2 Mouse Driver for Atom OS
//
// This driver polls raw mouse bytes from the kernel via syscalls,
// assembles them into PS/2 packets, and dispatches mouse events
// via IPC to the compositor.
//
// Supports all PS/2 mouse modes as detected by the kernel:
//   mouseID 0: 3-byte packets (basic 3-button mouse)
//   mouseID 3: 4-byte packets (scroll wheel, Z-axis movement)
//   mouseID 4: 4-byte packets (scroll wheel + 4th/5th buttons)
//
// Packet format (first 3 bytes, always present):
//   byte 0: [Y_ovf | X_ovf | Y_sign | X_sign | 1 | Mid | Right | Left]
//   byte 1: X movement value (unsigned, sign in byte 0 bit 4)
//   byte 2: Y movement value (unsigned, sign in byte 0 bit 5)
//
// 4th byte (mouseID 3): signed 8-bit Z movement
// 4th byte (mouseID 4): [0|0|btn5|btn4|Z3|Z2|Z1|Z0] (4-bit signed Z)
//
// Delta sign extension uses the OSDev recommended formula:
//   rel_x = raw_x - ((flags << 4) & 0x100)
//   rel_y = raw_y - ((flags << 3) & 0x100)
//
// Architecture:
//   Hardware IRQ12 -> Kernel ring buffer -> mouse_poll_byte() syscall
//   -> This driver (packet assembly + decoding)
//   -> IPC events to compositor (MouseMove, MouseButtonDown/Up, MouseScroll)

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
use atom_syscall::input::{mouse_poll_byte, mouse_get_id};

use libipc::messages::{
    MessageType, MouseMoveEvent, MouseButtonEvent, MouseScrollEvent, MouseButton,
};
use libipc::protocol::{send_message_async, register_service, lookup_service};

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

/// Full mouse state from a decoded PS/2 packet
#[derive(Clone, Copy, Default)]
struct MouseState {
    dx: i32,
    dy: i32,
    dz: i32,
    left: bool,
    right: bool,
    middle: bool,
    button4: bool,
    button5: bool,
}

/// PS/2 mouse packet assembler and decoder
struct PacketDecoder {
    packet: [u8; 4],
    cycle: u8,
    packet_size: u8, // 3 or 4 bytes
    mouse_id: u8,    // 0, 3, or 4
}

impl PacketDecoder {
    fn new(mouse_id: u8) -> Self {
        Self {
            packet: [0; 4],
            cycle: 0,
            packet_size: if mouse_id >= 3 { 4 } else { 3 },
            mouse_id,
        }
    }

    /// Feed a raw byte and return a decoded state when a full packet is ready.
    fn feed(&mut self, byte: u8) -> Option<MouseState> {
        match self.cycle {
            0 => {
                // First byte: bit 3 must be set (PS/2 "always 1" alignment bit)
                if byte & 0x08 != 0 {
                    self.packet[0] = byte;
                    self.cycle = 1;
                }
                // Otherwise, discard and resync
                None
            }
            1 => {
                self.packet[1] = byte;
                self.cycle = 2;
                None
            }
            2 => {
                self.packet[2] = byte;
                if self.packet_size == 3 {
                    self.cycle = 0;
                    return self.decode();
                }
                self.cycle = 3;
                None
            }
            3 => {
                self.packet[3] = byte;
                self.cycle = 0;
                self.decode()
            }
            _ => { self.cycle = 0; None }
        }
    }

    /// Decode the assembled packet into a MouseState.
    fn decode(&self) -> Option<MouseState> {
        let flags = self.packet[0];

        // Discard overflow packets (X or Y overflow bits set)
        if flags & 0xC0 != 0 {
            return None;
        }

        // OSDev recommended sign extension:
        //   rel_x = raw_x - ((flags << 4) & 0x100)
        //   rel_y = raw_y - ((flags << 3) & 0x100)
        let dx = (self.packet[1] as i32) - (((flags as i32) << 4) & 0x100);
        let dy = (self.packet[2] as i32) - (((flags as i32) << 3) & 0x100);

        let left = flags & 0x01 != 0;
        let right = flags & 0x02 != 0;
        let middle = flags & 0x04 != 0;

        let mut dz: i32 = 0;
        let mut button4 = false;
        let mut button5 = false;

        if self.packet_size == 4 {
            let b4 = self.packet[3];
            match self.mouse_id {
                3 => {
                    // mouseID 3: signed 8-bit Z movement
                    dz = b4 as i8 as i32;
                }
                4 => {
                    // mouseID 4: bits 4-5 are extra buttons, low nibble is signed 4-bit Z
                    button4 = b4 & 0x10 != 0;
                    button5 = b4 & 0x20 != 0;
                    let z_raw = b4 & 0x0F;
                    dz = if z_raw & 0x08 != 0 {
                        (z_raw | 0xF0) as i8 as i32
                    } else {
                        z_raw as i32
                    };
                }
                _ => {}
            }
        }

        Some(MouseState { dx, dy, dz, left, right, middle, button4, button5 })
    }
}

// ============================================================================
// Mouse Driver
// ============================================================================

struct MouseDriver {
    decoder: PacketDecoder,
    prev: MouseState,
    compositor_port: PortId,
}

impl MouseDriver {
    fn new(mouse_id: u8, compositor_port: PortId) -> Self {
        Self {
            decoder: PacketDecoder::new(mouse_id),
            prev: MouseState::default(),
            compositor_port,
        }
    }

    fn run(&mut self) -> ! {
        log("Mouse Driver: Entering poll loop");

        loop {
            while let Some(byte) = mouse_poll_byte() {
                if let Some(state) = self.decoder.feed(byte) {
                    self.dispatch(state);
                    self.prev = state;
                }
            }
            yield_now();
        }
    }

    /// Dispatch IPC events to compositor based on decoded mouse state.
    fn dispatch(&self, state: MouseState) {
        // Movement event
        if state.dx != 0 || state.dy != 0 {
            let msg = MouseMoveEvent {
                x: 0, y: 0,
                dx: state.dx as i16,
                dy: state.dy as i16,
            };
            let _ = send_message_async(self.compositor_port, MessageType::MouseMove, &msg.to_bytes());
        }

        // Scroll event (Z-axis)
        if state.dz != 0 {
            let msg = MouseScrollEvent {
                dz: state.dz,
                x: 0, y: 0,
            };
            let _ = send_message_async(self.compositor_port, MessageType::MouseScroll, &msg.to_bytes());
        }

        // Button events (send on state change)
        self.check_button(state.left, self.prev.left, MouseButton::Left);
        self.check_button(state.right, self.prev.right, MouseButton::Right);
        self.check_button(state.middle, self.prev.middle, MouseButton::Middle);
        self.check_button(state.button4, self.prev.button4, MouseButton::Button4);
        self.check_button(state.button5, self.prev.button5, MouseButton::Button5);
    }

    /// Send button down/up event if the state changed.
    fn check_button(&self, current: bool, previous: bool, button: MouseButton) {
        if current != previous {
            let msg_type = if current {
                MessageType::MouseButtonDown
            } else {
                MessageType::MouseButtonUp
            };
            let msg = MouseButtonEvent { button, x: 0, y: 0 };
            let _ = send_message_async(self.compositor_port, msg_type, &msg.to_bytes());
        }
    }
}

// ============================================================================
// Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("Mouse Driver: Starting...");

    // 1. Query the kernel for the detected mouseID
    let mouse_id = mouse_get_id();
    match mouse_id {
        0 => log("Mouse Driver: Basic 3-button mouse (mouseID 0, 3-byte packets)"),
        3 => log("Mouse Driver: Scroll wheel mouse (mouseID 3, 4-byte packets)"),
        4 => log("Mouse Driver: 5-button mouse (mouseID 4, 4-byte packets)"),
        _ => log("Mouse Driver: Unknown mouseID, assuming basic"),
    }

    // 2. Register as "mouse" service
    if let Ok(our_port) = create_port() {
        let _ = register_service("mouse", our_port);
        log("Mouse Driver: Service 'mouse' registered");
    }

    // 3. Look up compositor port for sending events
    log("Mouse Driver: Looking up compositor...");
    let compositor_port = loop {
        match lookup_service("compositor") {
            Ok(port) => {
                log("Mouse Driver: Found compositor");
                break port;
            }
            Err(_) => yield_now(),
        }
    };

    // 4. Run the driver
    let mut driver = MouseDriver::new(mouse_id, compositor_port);
    driver.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Mouse Driver: PANIC!");
    exit(0xFF);
}
