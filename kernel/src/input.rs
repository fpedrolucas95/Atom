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

/// Internal helper to drain the PS/2 controller's output buffer and route
/// data to either the keyboard or mouse ring buffer.
fn handle_ps2_input() {
    loop {
        let status = read_status();
        if (status & STATUS_OUTPUT_FULL) == 0 {
            break;
        }

        let data = read_data();
        if (status & STATUS_AUX_DATA) != 0 {
            MOUSE_BUFFER.lock().push(data);
        } else {
            KEYBOARD_BUFFER.lock().push(data);
        }
    }
}

/// Called from keyboard interrupt handler (IRQ1)
pub fn on_keyboard_irq() {
    handle_ps2_input();
}

/// Called from mouse interrupt handler (IRQ12)
pub fn on_mouse_irq() {
    handle_ps2_input();
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
// MICROKERNEL ARCHITECTURE NOTE
// ============================================================================
//
// Event parsing (KeyEvent, MouseEvent) is handled in USERSPACE, not the kernel.
// The kernel only provides:
// - Raw byte buffers populated by IRQ handlers
// - Syscalls to poll raw bytes from these buffers
// - PS/2 hardware initialization
//
// The userspace UI shell (ui_shell.atxf) is responsible for:
// - Parsing raw scancodes into key events
// - Assembling PS/2 packets into mouse events
// - Routing events to windows
// ============================================================================

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

/// Initialize PS/2 controller for keyboard and mouse support
/// Based on SANiK's PS/2 Mouse code and OSDev documentation
pub fn init_ps2_mouse_full() {
    log_info!("input", "Initializing PS/2 controller (keyboard + mouse)...");

    // Drain any pending data first (before IRQs are enabled)
    for _ in 0..100 {
        if read_status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = read_data();
    }

    // 1. Disable devices during configuration
    wait_for_input_buffer();
    write_command(0xAD); // Disable keyboard port
    wait_for_input_buffer();
    write_command(0xA7); // Disable mouse port

    // Flush output buffer again
    for _ in 0..10 {
        if read_status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = read_data();
    }

    // 2. Configure controller (compaq status byte)
    wait_for_input_buffer();
    write_command(0x20); // Get compaq status byte
    wait_for_output_buffer();
    let status = read_data();

    // Set bit 0 (enable IRQ1/keyboard), bit 1 (enable IRQ12/mouse)
    // Set bit 6 (enable translation to Scancode Set 1) - CRITICAL for QEMU!
    // Clear bit 4 (enable keyboard clock), bit 5 (enable mouse clock)
    let new_status = (status | 0x43) & !0x30;  // 0x43 = IRQ1 + IRQ12 + Translation
    log_info!("input", "PS/2 controller status: 0x{:02X} -> 0x{:02X} (IRQ1+IRQ12+Translation enabled)", status, new_status);

    wait_for_input_buffer();
    write_command(0x60); // Set compaq status byte
    wait_for_input_buffer();
    write_data(new_status);

    // 3. Re-enable ports
    wait_for_input_buffer();
    write_command(0xAE); // Enable keyboard port (first PS/2 port)
    wait_for_input_buffer();
    write_command(0xA8); // Enable aux/mouse port (second PS/2 port)
    log_info!("input", "PS/2 ports enabled (keyboard + mouse)");

    // 4. Reset keyboard (0xFF) and wait for self-test
    log_info!("input", "PS/2 keyboard: Sending reset command...");
    wait_for_input_buffer();
    write_data(0xFF); // Reset keyboard

    // Wait for ACK (0xFA)
    wait_for_output_buffer();
    let reset_ack = read_data();
    if reset_ack == 0xFA {
        log_info!("input", "PS/2 keyboard: Reset ACK received");
    } else {
        log_info!("input", "PS/2 keyboard: Reset response: 0x{:02X} (expected 0xFA)", reset_ack);
    }

    // Wait for self-test pass (0xAA) - can take a while
    // Some keyboards also send 0xFC (self-test failed) or just timeout
    for _ in 0..1000 {
        if read_status() & STATUS_OUTPUT_FULL != 0 {
            let selftest = read_data();
            if selftest == 0xAA {
                log_info!("input", "PS/2 keyboard: Self-test passed (0xAA)");
                break;
            } else if selftest == 0xFC {
                log_info!("input", "PS/2 keyboard: Self-test FAILED (0xFC)");
                break;
            } else {
                log_info!("input", "PS/2 keyboard: Self-test response: 0x{:02X}", selftest);
            }
        }
        // Small delay
        for _ in 0..10000 {
            core::hint::spin_loop();
        }
    }

    // 5. Set scancode set 2 (with translation enabled, we get Set 1 codes)
    wait_for_input_buffer();
    write_data(0xF0); // Set scancode set command
    wait_for_output_buffer();
    let _ = read_data(); // ACK

    wait_for_input_buffer();
    write_data(0x02); // Scancode Set 2
    wait_for_output_buffer();
    let scanset_ack = read_data();
    log_info!("input", "PS/2 keyboard: Scancode set 2 response: 0x{:02X}", scanset_ack);

    // 6. Enable keyboard scanning (send 0xF4 to keyboard)
    wait_for_input_buffer();
    write_data(0xF4); // Enable scanning
    wait_for_output_buffer();
    let kbd_ack = read_data();
    if kbd_ack == 0xFA {
        log_info!("input", "PS/2 keyboard: Scanning enabled (ACK received)");
    } else {
        log_info!("input", "PS/2 keyboard: Enable response: 0x{:02X} (expected 0xFA)", kbd_ack);
    }

    // 5. Tell mouse to use default settings
    send_mouse_command(0xF6); // send_mouse_command already consumes ACK

    // 6. Set scaling 1:1 (0xE6) - LINEAR movement, no acceleration
    send_mouse_command(0xE6);
    log_info!("input", "PS/2 mouse: Scaling set to 1:1 (linear)");

    // 7. Set resolution to 8 count/mm (0x03) for higher precision
    send_mouse_command(0xE8);
    // Send data byte for resolution
    wait_for_input_buffer();
    write_command(0xD4);
    wait_for_input_buffer();
    write_data(0x03); // 8 count/mm
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge data byte
    log_info!("input", "PS/2 mouse: Resolution set to 8 count/mm");

    // 8. Set sample rate to 100 samples/sec
    send_mouse_command(0xF3);
    // Send data byte for sample rate
    wait_for_input_buffer();
    write_command(0xD4);
    wait_for_input_buffer();
    write_data(100);
    wait_for_output_buffer();
    let _ = read_data(); // Acknowledge data byte
    log_info!("input", "PS/2 mouse: Sample rate set to 100/sec");

    // 9. Enable the mouse (start streaming packets)
    send_mouse_command(0xF4);

    // 10. Verify final controller configuration
    wait_for_input_buffer();
    write_command(0x20); // Read controller status byte
    wait_for_output_buffer();
    let final_status = read_data();
    log_info!("input", "PS/2 controller final status: 0x{:02X} (IRQ1={}, IRQ12={})",
        final_status,
        if final_status & 0x01 != 0 { "enabled" } else { "DISABLED" },
        if final_status & 0x02 != 0 { "enabled" } else { "DISABLED" }
    );

    // 11. Clear any leftover ACK bytes from the buffer
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
