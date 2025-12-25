// Userspace PS/2 Mouse Driver
//
// Complete implementation of PS/2 mouse protocol based on OSDev Wiki reference.
// Provides 1:1 mouse movement with no acceleration (linear scaling).
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

// ============================================================================
// Syscall Numbers
// ============================================================================

const SYS_THREAD_YIELD: u64 = 0;
const SYS_THREAD_EXIT: u64 = 1;
const SYS_IO_PORT_READ: u64 = 34;
const SYS_IO_PORT_WRITE: u64 = 35;
const SYS_DEBUG_LOG: u64 = 39;

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
        debug_log("Mouse: Starting PS/2 mouse initialization");

        // Drain any pending data
        self.drain_buffer();

        // Enable auxiliary device
        self.send_controller_command(CMD_ENABLE_AUX);
        
        // Read and modify controller config to enable IRQ12
        self.send_controller_command(CMD_READ_CONFIG);
        if !self.wait_for_output() {
            debug_log("Mouse: Failed to read controller config");
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
            debug_log("Mouse: SET_DEFAULTS failed");
            return false;
        }
        debug_log("Mouse: SET_DEFAULTS OK");

        // Set 1:1 scaling (linear, no acceleration)
        if !self.mouse_command(MOUSE_SET_SCALING_1_1) {
            debug_log("Mouse: SET_SCALING_1_1 failed");
            return false;
        }
        debug_log("Mouse: SET_SCALING_1_1 OK");

        // Set resolution to 4 count/mm (value 0x02)
        if !self.mouse_command(MOUSE_SET_RESOLUTION) {
            debug_log("Mouse: SET_RESOLUTION command failed");
            return false;
        }
        if !self.mouse_write_data(0x02) {
            debug_log("Mouse: SET_RESOLUTION data failed");
            return false;
        }
        debug_log("Mouse: Resolution set to 4 count/mm");

        // Set sample rate to 100 samples/sec
        if !self.mouse_command(MOUSE_SET_SAMPLE_RATE) {
            debug_log("Mouse: SET_SAMPLE_RATE command failed");
            return false;
        }
        if !self.mouse_write_data(100) {
            debug_log("Mouse: SET_SAMPLE_RATE data failed");
            return false;
        }
        debug_log("Mouse: Sample rate set to 100/sec");

        // Enable streaming mode
        if !self.mouse_command(MOUSE_ENABLE_STREAMING) {
            debug_log("Mouse: ENABLE_STREAMING failed");
            return false;
        }
        debug_log("Mouse: Streaming enabled");

        self.initialized = true;
        debug_log("Mouse: PS/2 mouse initialization complete (1:1 scaling)");
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
// IO Port Access via Syscalls
// ============================================================================

fn port_read(port: u16) -> u8 {
    let result = unsafe { syscall2(SYS_IO_PORT_READ, port as u64, 1) };
    result as u8
}

fn port_write(port: u16, value: u8) {
    unsafe { syscall2(SYS_IO_PORT_WRITE, port as u64, value as u64) };
}

fn spin_loop() {
    for _ in 0..100 {
        unsafe { core::arch::asm!("pause"); }
    }
}

// ============================================================================
// Syscall Interface
// ============================================================================

#[inline(always)]
unsafe fn syscall0(num: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline(always)]
unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline(always)]
unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

fn thread_yield() {
    unsafe { syscall0(SYS_THREAD_YIELD); }
}

fn thread_exit(code: u64) -> ! {
    unsafe { 
        syscall1(SYS_THREAD_EXIT, code);
        loop { core::arch::asm!("hlt"); }
    }
}

fn debug_log(msg: &str) {
    unsafe {
        syscall2(SYS_DEBUG_LOG, msg.as_ptr() as u64, msg.len() as u64);
    }
}

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
    debug_log("Mouse Driver: Starting PS/2 mouse driver");

    // Initialize the mouse
    unsafe {
        if !MOUSE_DRIVER.init() {
            debug_log("Mouse Driver: Initialization failed!");
            thread_exit(1);
        }
    }

    debug_log("Mouse Driver: Entering poll loop");

    // Main driver loop - poll for mouse data
    loop {
        if let Some(_state) = poll_mouse() {
            // State has been updated, consumers can read it via get_mouse_state()
            // In a full implementation, this would send IPC messages
        }

        thread_yield();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    debug_log("Mouse Driver: PANIC!");
    thread_exit(0xFF)
}
