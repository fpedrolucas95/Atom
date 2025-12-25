// Userspace PS/2 Mouse Driver
//
// Complete implementation of PS/2 mouse protocol based on OSDev Wiki reference.
// Provides 1:1 mouse movement with no acceleration (linear scaling).
//
// This driver runs entirely in Ring 3 (userspace) and communicates with
// the kernel via the atom_syscall library. It is a TRUE userspace binary,
// not code that runs inside the kernel.
//
// References:
// - https://wiki.osdev.org/Mouse_Input
// - https://wiki.osdev.org/PS/2_Mouse
//
// Key features:
// - Full PS/2 mouse initialization sequence
// - 3-byte packet parsing with sign extension
// - 1:1 movement scaling (scaling 1:1 enabled)
// - Button state tracking (left, right, middle)
// - Overflow detection and packet validation

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// Use the atom_syscall library for all kernel interactions
use atom_syscall::io::{port_read_u8, port_write_u8, ps2};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

// ============================================================================
// PS/2 Controller Constants
// ============================================================================

const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;

// Status register bits
const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

// PS/2 Controller commands
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_AUX_PREFIX: u8 = 0xD4;

// Mouse commands
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
}

impl MouseDriver {
    const fn new() -> Self {
        Self {
            packet: [0; 3],
            cycle: 0,
            state: MouseState {
                delta_x: 0,
                delta_y: 0,
                left_button: false,
                right_button: false,
                middle_button: false,
            },
            initialized: false,
        }
    }

    /// Initialize the PS/2 mouse with 1:1 scaling
    fn init(&mut self) -> bool {
        log("Mouse: Starting PS/2 mouse initialization");

        // Drain any pending data
        self.drain_buffer();

        // Enable auxiliary device
        self.send_controller_command(CMD_ENABLE_AUX);
        
        // Read and modify controller config to enable IRQ12
        self.send_controller_command(CMD_READ_CONFIG);
        if !self.wait_for_output() {
            log("Mouse: Failed to read controller config");
            return false;
        }
        let config = self.read_data();
        
        // Set bit 1 (enable IRQ12) and bit 0 (enable IRQ1)
        // Clear bit 5 (enable mouse clock)
        let new_config = (config | 0x03) & !0x20;
        
        self.send_controller_command(CMD_WRITE_CONFIG);
        self.write_data(new_config);

        // Set defaults
        if !self.mouse_command(MOUSE_SET_DEFAULTS) {
            log("Mouse: SET_DEFAULTS failed");
            return false;
        }
        log("Mouse: SET_DEFAULTS OK");

        // Set 1:1 scaling (linear, no acceleration)
        if !self.mouse_command(MOUSE_SET_SCALING_1_1) {
            log("Mouse: SET_SCALING_1_1 failed");
            return false;
        }
        log("Mouse: SET_SCALING_1_1 OK");

        // Set resolution to 4 count/mm (value 0x02)
        if !self.mouse_command(MOUSE_SET_RESOLUTION) {
            log("Mouse: SET_RESOLUTION command failed");
            return false;
        }
        if !self.mouse_write_data(0x02) {
            log("Mouse: SET_RESOLUTION data failed");
            return false;
        }
        log("Mouse: Resolution set to 4 count/mm");

        // Set sample rate to 100 samples/sec
        if !self.mouse_command(MOUSE_SET_SAMPLE_RATE) {
            log("Mouse: SET_SAMPLE_RATE command failed");
            return false;
        }
        if !self.mouse_write_data(100) {
            log("Mouse: SET_SAMPLE_RATE data failed");
            return false;
        }
        log("Mouse: Sample rate set to 100/sec");

        // Enable streaming mode
        if !self.mouse_command(MOUSE_ENABLE_STREAMING) {
            log("Mouse: ENABLE_STREAMING failed");
            return false;
        }
        log("Mouse: Streaming enabled");

        self.initialized = true;
        log("Mouse: PS/2 mouse initialization complete (1:1 scaling)");
        true
    }

    /// Process a mouse data byte
    fn process_byte(&mut self, byte: u8) -> Option<MouseState> {
        match self.cycle {
            0 => {
                // First byte: check bit 3 (always 1 for alignment)
                if byte & 0x08 == 0 {
                    // Misaligned packet, skip
                    return None;
                }
                self.packet[0] = byte;
                self.cycle = 1;
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
                self.finalize_packet()
            }
            _ => {
                self.cycle = 0;
                None
            }
        }
    }

    /// Finalize a complete 3-byte packet
    fn finalize_packet(&mut self) -> Option<MouseState> {
        let flags = self.packet[0];

        // Check alignment bit
        if flags & 0x08 == 0 {
            return None;
        }

        // Check for overflow (discard if set)
        if (flags & 0xC0) != 0 {
            return None;
        }

        // Extract delta X with sign extension
        // Bit 4 of flags indicates X is negative
        let mut dx = self.packet[1] as i16;
        if flags & 0x10 != 0 {
            dx = dx.wrapping_sub(256); // Sign extend
        }

        // Extract delta Y with sign extension
        // Bit 5 of flags indicates Y is negative
        let mut dy = self.packet[2] as i16;
        if flags & 0x20 != 0 {
            dy = dy.wrapping_sub(256); // Sign extend
        }

        // Update state with 1:1 movement (no scaling applied)
        self.state.delta_x = dx;
        self.state.delta_y = dy;
        self.state.left_button = (flags & 0x01) != 0;
        self.state.right_button = (flags & 0x02) != 0;
        self.state.middle_button = (flags & 0x04) != 0;

        Some(self.state)
    }

    /// Drain any pending data from the buffer
    fn drain_buffer(&self) {
        for _ in 0..100 {
            if !self.aux_data_available() {
                break;
            }
            let _ = self.read_data();
        }
    }

    /// Check if auxiliary (mouse) data is available
    fn aux_data_available(&self) -> bool {
        let status = self.read_status();
        (status & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)) == (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)
    }

    /// Wait for input buffer to be empty
    fn wait_for_input_empty(&self) -> bool {
        for _ in 0..50000 {
            if (self.read_status() & STATUS_INPUT_FULL) == 0 {
                return true;
            }
            spin_loop();
        }
        false
    }

    /// Wait for output buffer to have data
    fn wait_for_output(&self) -> bool {
        for _ in 0..50000 {
            if (self.read_status() & STATUS_OUTPUT_FULL) != 0 {
                return true;
            }
            spin_loop();
        }
        false
    }

    /// Send a command to the PS/2 controller
    fn send_controller_command(&self, cmd: u8) {
        self.wait_for_input_empty();
        port_write(PS2_COMMAND_PORT, cmd);
    }

    /// Write data to the PS/2 data port
    fn write_data(&self, data: u8) {
        self.wait_for_input_empty();
        port_write(PS2_DATA_PORT, data);
    }

    /// Read data from the PS/2 data port
    fn read_data(&self) -> u8 {
        port_read(PS2_DATA_PORT)
    }

    /// Read the PS/2 status register
    fn read_status(&self) -> u8 {
        port_read(PS2_STATUS_PORT)
    }

    /// Send a command to the mouse (through auxiliary port)
    fn mouse_command(&self, cmd: u8) -> bool {
        // Send prefix to route to mouse
        self.send_controller_command(CMD_AUX_PREFIX);
        
        // Send actual command
        self.write_data(cmd);
        
        // Wait for ACK
        if !self.wait_for_output() {
            return false;
        }
        
        let response = self.read_data();
        response == MOUSE_ACK
    }

    /// Send data byte to mouse (after command)
    fn mouse_write_data(&self, data: u8) -> bool {
        self.send_controller_command(CMD_AUX_PREFIX);
        self.write_data(data);
        
        if !self.wait_for_output() {
            return false;
        }
        
        let response = self.read_data();
        response == MOUSE_ACK
    }
}

// ============================================================================
// IO Port Access via atom_syscall library
// ============================================================================

fn port_read(port: u16) -> u8 {
    port_read_u8(port).unwrap_or(0)
}

fn port_write(port: u16, value: u8) {
    let _ = port_write_u8(port, value);
}

fn spin_loop() {
    for _ in 0..100 {
        core::hint::spin_loop();
    }
}

// Syscall wrappers are now provided by atom_syscall library

// ============================================================================
// Static Driver Instance
// ============================================================================

static mut MOUSE_DRIVER: MouseDriver = MouseDriver::new();

// ============================================================================
// Public API (for IPC)
// ============================================================================

/// Get the current mouse state (delta since last call, button states)
pub fn get_mouse_state() -> MouseState {
    unsafe {
        let state = MOUSE_DRIVER.state;
        // Reset deltas after reading
        MOUSE_DRIVER.state.delta_x = 0;
        MOUSE_DRIVER.state.delta_y = 0;
        state
    }
}

/// Poll for mouse data (non-blocking)
pub fn poll_mouse() -> Option<MouseState> {
    unsafe {
        if !MOUSE_DRIVER.initialized {
            return None;
        }

        // Check if mouse data is available
        if MOUSE_DRIVER.aux_data_available() {
            let byte = MOUSE_DRIVER.read_data();
            return MOUSE_DRIVER.process_byte(byte);
        }
        
        None
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
    log("Mouse Driver: Starting PS/2 mouse driver");

    // Initialize the mouse
    unsafe {
        if !MOUSE_DRIVER.init() {
            log("Mouse Driver: Initialization failed!");
            exit(1);
        }
    }

    log("Mouse Driver: Entering poll loop");

    // Main driver loop - poll for mouse data
    loop {
        if let Some(_state) = poll_mouse() {
            // State has been updated, consumers can read it via get_mouse_state()
            // In a full implementation, this would send IPC messages
        }

        yield_now();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Mouse Driver: PANIC!");
    exit(0xFF);
}
