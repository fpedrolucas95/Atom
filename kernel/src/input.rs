// Input Subsystem - Minimal IRQ Buffer for Userspace Drivers
//
// This module provides a minimal kernel-side input buffer that captures raw
// hardware events from keyboard and mouse IRQs. The actual driver logic runs
// entirely in userspace - this module only provides IRQ-safe buffering.
//
// Design philosophy:
// - Kernel does NOT interpret scancodes or mouse packets
// - Kernel only buffers raw bytes from hardware I/O ports
// - Userspace drivers poll these buffers via syscalls
// - This maintains microkernel architecture with minimal kernel code
//
// Key responsibilities:
// - Buffer raw PS/2 scancodes from keyboard IRQ (IRQ1)
// - Buffer raw PS/2 bytes from mouse IRQ (IRQ12)
// - Provide syscall-accessible polling interface
// - Handle IRQ-safe ring buffer operations
//
// Public interface:
// - `init()` - Initialize the input subsystem
// - `on_keyboard_irq()` - Called from keyboard interrupt handler
// - `on_mouse_irq()` - Called from mouse interrupt handler
// - `poll_keyboard_byte()` - Syscall: get next keyboard byte
// - `poll_mouse_byte()` - Syscall: get next mouse byte

use spin::Mutex;
use crate::log_info;

// PS/2 ports
const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const STATUS_OUTPUT_FULL: u8 = 0x01;
const STATUS_AUX_DATA: u8 = 0x20;

// Ring buffer capacity (must be power of 2 for efficiency)
const KEYBOARD_BUFFER_SIZE: usize = 128;
const MOUSE_BUFFER_SIZE: usize = 256;

/// Ring buffer for raw input bytes
struct RingBuffer<const N: usize> {
    buffer: [u8; N],
    head: usize,
    tail: usize,
}

impl<const N: usize> RingBuffer<N> {
    const fn new() -> Self {
        Self {
            buffer: [0; N],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, byte: u8) -> bool {
        let next_head = (self.head + 1) % N;
        if next_head == self.tail {
            return false; // Buffer full, drop byte
        }
        self.buffer[self.head] = byte;
        self.head = next_head;
        true
    }

    fn pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            return None; // Buffer empty
        }
        let byte = self.buffer[self.tail];
        self.tail = (self.tail + 1) % N;
        Some(byte)
    }

    #[allow(dead_code)]
    fn len(&self) -> usize {
        if self.head >= self.tail {
            self.head - self.tail
        } else {
            N - self.tail + self.head
        }
    }

    #[allow(dead_code)]
    fn is_empty(&self) -> bool {
        self.head == self.tail
    }
}

// Global input buffers protected by spinlocks
static KEYBOARD_BUFFER: Mutex<RingBuffer<KEYBOARD_BUFFER_SIZE>> = 
    Mutex::new(RingBuffer::new());
static MOUSE_BUFFER: Mutex<RingBuffer<MOUSE_BUFFER_SIZE>> = 
    Mutex::new(RingBuffer::new());

/// Initialize the input subsystem
pub fn init() {
    log_info!("input", "Input subsystem initialized (userspace driver model)");
    log_info!("input", "Keyboard buffer: {} bytes, Mouse buffer: {} bytes",
        KEYBOARD_BUFFER_SIZE, MOUSE_BUFFER_SIZE);
}

/// Read PS/2 status register
#[inline]
fn read_status() -> u8 {
    unsafe {
        let status: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") status,
            in("dx") PS2_STATUS_PORT,
            options(nomem, nostack, preserves_flags)
        );
        status
    }
}

/// Read PS/2 data register
#[inline]
fn read_data() -> u8 {
    unsafe {
        let data: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") data,
            in("dx") PS2_DATA_PORT,
            options(nomem, nostack, preserves_flags)
        );
        data
    }
}

/// Called from keyboard interrupt handler (IRQ1)
/// Reads all available bytes and buffers them for userspace
pub fn on_keyboard_irq() {
    let mut buf = KEYBOARD_BUFFER.lock();
    
    // Read all available keyboard data
    while read_status() & STATUS_OUTPUT_FULL != 0 {
        // Check it's not mouse data
        if read_status() & STATUS_AUX_DATA != 0 {
            break; // This is mouse data, not keyboard
        }
        
        let scancode = read_data();
        buf.push(scancode);
    }
}

/// Called from mouse interrupt handler (IRQ12)
/// Reads all available bytes and buffers them for userspace
pub fn on_mouse_irq() {
    let mut buf = MOUSE_BUFFER.lock();
    let mut count = 0u32;
    let mut bytes_debug = [0u8; 16];
    
    // Read all available mouse data (marked with AUX bit)
    while read_status() & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA) == 
          (STATUS_OUTPUT_FULL | STATUS_AUX_DATA) {
        let byte = read_data();
        if (count as usize) < bytes_debug.len() {
            bytes_debug[count as usize] = byte;
        }
        buf.push(byte);
        count += 1;
    }
    
    if count > 0 {
        // Log raw bytes for debugging
        if count >= 3 {
            crate::serial_println!("[IRQ12] {} bytes: [{:02X} {:02X} {:02X}]",
                count, bytes_debug[0], bytes_debug[1], bytes_debug[2]);
        } else {
            crate::serial_println!("[IRQ12] {} bytes", count);
        }
    }
}

/// Poll for next keyboard byte (called from syscall)
/// Returns None if buffer is empty
pub fn poll_keyboard_byte() -> Option<u8> {
    KEYBOARD_BUFFER.lock().pop()
}

/// Poll for next mouse byte (called from syscall)
/// Returns None if buffer is empty
pub fn poll_mouse_byte() -> Option<u8> {
    MOUSE_BUFFER.lock().pop()
}

// ============================================================================
// Structured Event Types for Desktop Environment
// ============================================================================

/// Keyboard event with scancode and press/release state
#[derive(Debug, Clone, Copy)]
pub struct KeyEvent {
    pub scancode: u8,
    pub pressed: bool,
}

/// Mouse event with movement deltas and button state
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    pub delta_x: i16,
    pub delta_y: i16,
    pub left_button: bool,
    pub right_button: bool,
    pub middle_button: bool,
}

// Static state for mouse packet assembly
static MOUSE_PACKET_STATE: Mutex<MousePacketState> = Mutex::new(MousePacketState::new());

struct MousePacketState {
    bytes: [u8; 3],
    index: usize,
}

impl MousePacketState {
    const fn new() -> Self {
        Self {
            bytes: [0; 3],
            index: 0,
        }
    }

    fn reset(&mut self) {
        self.index = 0;
    }

    fn add_byte(&mut self, byte: u8) -> Option<MouseEvent> {
        // First byte must have bit 3 set (always 1 in standard PS/2 mouse)
        if self.index == 0 && (byte & 0x08) == 0 {
            // Invalid first byte, skip it
            return None;
        }

        self.bytes[self.index] = byte;
        self.index += 1;

        if self.index >= 3 {
            // Complete packet
            let packet = self.bytes;
            self.index = 0;

            // Parse the packet
            let flags = packet[0];
            let dx_raw = packet[1] as i16;
            let dy_raw = packet[2] as i16;

            // Apply sign extension if needed
            let delta_x = if flags & 0x10 != 0 {
                dx_raw - 256
            } else {
                dx_raw
            };

            let delta_y = if flags & 0x20 != 0 {
                dy_raw - 256
            } else {
                dy_raw
            };

            // PS/2 Y axis is inverted (positive = up), negate for screen coords
            Some(MouseEvent {
                delta_x,
                delta_y: -delta_y,
                left_button: flags & 0x01 != 0,
                right_button: flags & 0x02 != 0,
                middle_button: flags & 0x04 != 0,
            })
        } else {
            None
        }
    }
}

/// Poll for next keyboard event
/// Parses raw scancodes into KeyEvent structures
pub fn poll_key_event() -> Option<KeyEvent> {
    let byte = poll_keyboard_byte()?;

    // Check for break code (key release) - 0xF0 prefix for set 2, or high bit for set 1
    if byte == 0xF0 {
        // Scancode set 2 release prefix - get next byte
        if let Some(scancode) = poll_keyboard_byte() {
            return Some(KeyEvent {
                scancode,
                pressed: false,
            });
        }
        return None;
    }

    // Scancode set 1: high bit indicates release
    let pressed = byte & 0x80 == 0;
    let scancode = byte & 0x7F;

    Some(KeyEvent {
        scancode,
        pressed,
    })
}

/// Poll for next mouse event
/// Parses raw PS/2 bytes into MouseEvent structures
pub fn poll_mouse_event() -> Option<MouseEvent> {
    let mut state = MOUSE_PACKET_STATE.lock();

    // Try to complete a packet
    while let Some(byte) = MOUSE_BUFFER.lock().pop() {
        if let Some(event) = state.add_byte(byte) {
            return Some(event);
        }
    }

    None
}

/// Initialize PS/2 controller for mouse support
/// This is minimal initialization - full driver logic is in userspace
pub fn init_ps2_mouse() {
    // Enable auxiliary device (mouse)
    wait_for_input_buffer();
    write_command(0xA8); // Enable aux port
    
    // Enable interrupts for keyboard and mouse
    wait_for_input_buffer();
    write_command(0x20); // Read config
    wait_for_output_buffer();
    let config = read_data();
    
    wait_for_input_buffer();
    write_command(0x60); // Write config
    wait_for_input_buffer();
    write_data(config | 0x02); // Enable aux interrupt (bit 1)
    
    // Enable mouse packet streaming
    send_mouse_command(0xF4); // Enable data reporting
    
    log_info!("input", "PS/2 mouse initialized for userspace driver");
}

fn wait_for_input_buffer() {
    for _ in 0..10000 {
        if read_status() & 0x02 == 0 {
            return;
        }
    }
}

fn wait_for_output_buffer() {
    for _ in 0..10000 {
        if read_status() & 0x01 != 0 {
            return;
        }
    }
}

fn write_command(cmd: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") PS2_STATUS_PORT,
            in("al") cmd,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn write_data(data: u8) {
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") PS2_DATA_PORT,
            in("al") data,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn send_mouse_command(cmd: u8) {
    wait_for_input_buffer();
    write_command(0xD4); // Send to mouse
    wait_for_input_buffer();
    write_data(cmd);
    // Wait for ACK
    wait_for_output_buffer();
    let _ = read_data(); // Consume ACK (0xFA)
}

/// Send a mouse command with a data byte
fn send_mouse_command_with_data(cmd: u8, data: u8) {
    send_mouse_command(cmd);
    // Send data byte to mouse
    wait_for_input_buffer();
    write_command(0xD4); // Send to mouse
    wait_for_input_buffer();
    write_data(data);
    // Wait for ACK
    wait_for_output_buffer();
    let _ = read_data(); // Consume ACK
}

/// Read a byte from mouse (with timeout)
fn mouse_read() -> u8 {
    wait_for_output_buffer();
    read_data()
}

/// Initialize PS/2 controller for mouse support with 1:1 scaling
/// Based on SANiK's PS/2 Mouse code and OSDev documentation
pub fn init_ps2_mouse_full() {
    log_info!("input", "Initializing PS/2 mouse with 1:1 scaling...");
    
    // Drain any pending data first (before IRQs are enabled)
    for _ in 0..100 {
        if read_status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = read_data();
    }
    
    // 1. Enable the auxiliary mouse device
    wait_for_input_buffer();
    write_command(0xA8); // Enable aux port
    
    // 2. Enable the interrupts (compaq status byte)
    wait_for_input_buffer();
    write_command(0x20); // Get compaq status byte
    wait_for_output_buffer();
    let status = read_data();
    
    // Set bit 1 (enable IRQ12), clear bit 5 (enable mouse clock)
    let new_status = (status | 0x02) & !0x20;
    
    wait_for_input_buffer();
    write_command(0x60); // Set compaq status byte
    wait_for_input_buffer();
    write_data(new_status);
    
    // 3. Tell mouse to use default settings
    send_mouse_command(0xF6);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    
    // 4. Set scaling 1:1 (0xE6) - LINEAR movement, no acceleration
    send_mouse_command(0xE6);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    log_info!("input", "PS/2 mouse: Scaling set to 1:1 (linear)");
    
    // 5. Set resolution to 8 count/mm (0x03) for higher precision
    send_mouse_command(0xE8);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    wait_for_input_buffer();
    write_command(0xD4);
    wait_for_input_buffer();
    write_data(0x03); // 8 count/mm
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    log_info!("input", "PS/2 mouse: Resolution set to 8 count/mm");
    
    // 6. Set sample rate to 100 samples/sec
    send_mouse_command(0xF3);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    wait_for_input_buffer();
    write_command(0xD4);
    wait_for_input_buffer();
    write_data(100);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    log_info!("input", "PS/2 mouse: Sample rate set to 100/sec");
    
    // 7. Enable the mouse (start streaming packets)
    send_mouse_command(0xF4);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge
    
    // 8. Clear any leftover ACK bytes from the buffer
    // (some ACKs may have been captured by IRQ12 during init)
    {
        let mut buf = MOUSE_BUFFER.lock();
        while buf.pop().is_some() {}
    }
    
    // Also drain hardware buffer
    for _ in 0..10 {
        if read_status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = read_data();
    }
    
    log_info!("input", "PS/2 mouse initialized: 1:1 scaling, 8 count/mm, 100 samples/sec");
}
