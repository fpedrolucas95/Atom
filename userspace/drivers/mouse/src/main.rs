// Userspace PS/2 Mouse Driver
//
// Complete implementation of PS/2 mouse protocol based on OSDev Wiki reference.
// Provides 1:1 mouse movement with no acceleration (linear scaling).

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::io::{port_read_u8, port_write_u8};
use atom_syscall::ipc::{create_port, PortId};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

use libipc::messages::{MessageType, MouseMoveEvent, MouseButtonEvent, MouseButton};
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
// PS/2 Controller Constants
// ============================================================================

const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_AUX_PREFIX: u8 = 0xD4;

const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_STREAMING: u8 = 0xF4;
const MOUSE_SET_SAMPLE_RATE: u8 = 0xF3;
const MOUSE_GET_ID: u8 = 0xF2;
const MOUSE_SET_RESOLUTION: u8 = 0xE8;
const MOUSE_SET_SCALING_1_1: u8 = 0xE6;

const MOUSE_ACK: u8 = 0xFA;

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
    initialized: bool,
    compositor_port: PortId,
}

impl MouseDriver {
    fn new(compositor_port: PortId) -> Self {
        Self {
            packet: [0; 3],
            cycle: 0,
            state: MouseState::default(),
            initialized: false,
            compositor_port,
        }
    }

    fn init(&mut self) -> bool {
        log("Mouse: Initializing...");
        self.drain_buffer();
        self.send_controller_command(CMD_ENABLE_AUX);
        
        self.send_controller_command(CMD_READ_CONFIG);
        if !self.wait_for_output() { return false; }
        let config = self.read_data();
        let new_config = (config | 0x03) & !0x20;
        self.send_controller_command(CMD_WRITE_CONFIG);
        self.write_data(new_config);

        if !self.mouse_command(MOUSE_SET_DEFAULTS) { return false; }
        if !self.mouse_command(MOUSE_SET_SCALING_1_1) { return false; }
        if !self.mouse_command(MOUSE_SET_RESOLUTION) { return false; }
        if !self.mouse_write_data(0x02) { return false; }
        if !self.mouse_command(MOUSE_SET_SAMPLE_RATE) { return false; }
        if !self.mouse_write_data(100) { return false; }
        if !self.mouse_command(MOUSE_ENABLE_STREAMING) { return false; }

        self.initialized = true;
        log("Mouse: Ready");
        true
    }

    fn run(&mut self) -> ! {
        let mut prev_state = MouseState::default();

        loop {
            if self.aux_data_available() {
                let byte = self.read_data();
                if let Some(new_state) = self.process_byte(byte) {
                    // Send move event if deltas are non-zero
                    if new_state.delta_x != 0 || new_state.delta_y != 0 {
                        let move_msg = MouseMoveEvent {
                            x: 0, y: 0,
                            dx: new_state.delta_x,
                            dy: new_state.delta_y,
                        };
                        let _ = send_message_async(self.compositor_port, MessageType::MouseMove, &move_msg.to_bytes());
                    }

                    // Send button events if state changed
                    if new_state.left_button != prev_state.left_button {
                        let msg_type = if new_state.left_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Left, x: 0, y: 0 };
                        let _ = send_message_async(self.compositor_port, msg_type, &btn_msg.to_bytes());
                    }
                    if new_state.right_button != prev_state.right_button {
                        let msg_type = if new_state.right_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Right, x: 0, y: 0 };
                        let _ = send_message_async(self.compositor_port, msg_type, &btn_msg.to_bytes());
                    }
                    if new_state.middle_button != prev_state.middle_button {
                        let msg_type = if new_state.middle_button { MessageType::MouseButtonDown } else { MessageType::MouseButtonUp };
                        let btn_msg = MouseButtonEvent { button: MouseButton::Middle, x: 0, y: 0 };
                        let _ = send_message_async(self.compositor_port, msg_type, &btn_msg.to_bytes());
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
                if (flags & 0xC0) != 0 { return None; }
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

    fn drain_buffer(&self) {
        for _ in 0..100 {
            if !self.aux_data_available() { break; }
            let _ = self.read_data();
        }
    }

    fn aux_data_available(&self) -> bool {
        let status = port_read_u8(PS2_STATUS_PORT).unwrap_or(0);
        (status & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)) == (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)
    }

    fn wait_for_input_empty(&self) -> bool {
        for _ in 0..50000 {
            if (port_read_u8(PS2_STATUS_PORT).unwrap_or(0) & STATUS_INPUT_FULL) == 0 { return true; }
        }
        false
    }

    fn wait_for_output(&self) -> bool {
        for _ in 0..50000 {
            if (port_read_u8(PS2_STATUS_PORT).unwrap_or(0) & STATUS_OUTPUT_FULL) != 0 { return true; }
        }
        false
    }

    fn send_controller_command(&self, cmd: u8) {
        self.wait_for_input_empty();
        let _ = port_write_u8(PS2_COMMAND_PORT, cmd);
    }

    fn write_data(&self, data: u8) {
        self.wait_for_input_empty();
        let _ = port_write_u8(PS2_DATA_PORT, data);
    }

    fn read_data(&self) -> u8 {
        port_read_u8(PS2_DATA_PORT).unwrap_or(0)
    }

    fn mouse_command(&self, cmd: u8) -> bool {
        self.send_controller_command(CMD_AUX_PREFIX);
        self.write_data(cmd);
        if !self.wait_for_output() { return false; }
        self.read_data() == MOUSE_ACK
    }

    fn mouse_write_data(&self, data: u8) -> bool {
        self.send_controller_command(CMD_AUX_PREFIX);
        self.write_data(data);
        if !self.wait_for_output() { return false; }
        self.read_data() == MOUSE_ACK
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("Mouse Driver: Starting...");

    if let Ok(port) = create_port() {
        let _ = register_service("mouse", port);
    }

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

    let mut driver = MouseDriver::new(compositor_port);
    if !driver.init() {
        log("Mouse Driver: Failed to initialize hardware");
        exit(1);
    }

    driver.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Mouse Driver: PANIC!");
    exit(0xFF);
}
