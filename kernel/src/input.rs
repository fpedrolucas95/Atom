// Input Subsystem - Minimal IRQ Buffer for Userspace Drivers
//
// This module provides a minimal kernel-side input buffer that captures raw
// hardware events from keyboard and mouse IRQs. The actual driver logic runs
// entirely in userspace - this module only provides IRQ-safe buffering.
//
// Microkernel Design Philosophy:
// - Kernel provides MECHANISM (IRQ handling, buffering, syscall interface)
// - Userspace provides POLICY (device initialization, event interpretation)
//
// Key responsibilities:
// - Buffer raw PS/2 scancodes from keyboard IRQ (IRQ1)
// - Buffer raw PS/2 bytes from mouse IRQ (IRQ12)
// - Provide syscall-accessible polling interface
// - Handle IRQ-safe ring buffer operations
//
// What the kernel does NOT do (moved to userspace):
// - PS/2 controller initialization
// - Mouse scaling/resolution configuration
// - Scancode interpretation
// - Event routing to applications
//
// Public interface:
// - `init()` - Initialize the input subsystem
// - `on_keyboard_irq()` - Called from keyboard interrupt handler
// - `on_mouse_irq()` - Called from mouse interrupt handler
// - `poll_keyboard_byte()` - Syscall: get next keyboard byte
// - `poll_mouse_byte()` - Syscall: get next mouse byte

use spin::Mutex;
use crate::log_info;

// PS/2 ports (used for reading in IRQ handlers)
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

    /// Clear the buffer
    fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
    }
}

// Global input buffers protected by spinlocks
static KEYBOARD_BUFFER: Mutex<RingBuffer<KEYBOARD_BUFFER_SIZE>> =
    Mutex::new(RingBuffer::new());
static MOUSE_BUFFER: Mutex<RingBuffer<MOUSE_BUFFER_SIZE>> =
    Mutex::new(RingBuffer::new());

/// Initialize the input subsystem
///
/// This only sets up the kernel-side buffers. PS/2 controller initialization
/// is handled by userspace drivers using IO port syscalls.
pub fn init() {
    log_info!("input", "Input subsystem initialized (microkernel model)");
    log_info!("input", "Keyboard buffer: {} bytes, Mouse buffer: {} bytes",
        KEYBOARD_BUFFER_SIZE, MOUSE_BUFFER_SIZE);
    log_info!("input", "PS/2 initialization delegated to userspace drivers");
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
///
/// Reads all available bytes and buffers them for userspace.
/// Does NOT interpret scancodes - that's userspace's job.
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
///
/// Reads all available bytes and buffers them for userspace.
/// Does NOT parse PS/2 packets - that's userspace's job.
pub fn on_mouse_irq() {
    let mut buf = MOUSE_BUFFER.lock();

    // Read all available mouse data (marked with AUX bit)
    while read_status() & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA) ==
          (STATUS_OUTPUT_FULL | STATUS_AUX_DATA) {
        let byte = read_data();
        buf.push(byte);
    }
}

/// Poll for next keyboard byte (called from syscall)
///
/// Returns None if buffer is empty.
/// Userspace is responsible for interpreting scancodes.
pub fn poll_keyboard_byte() -> Option<u8> {
    KEYBOARD_BUFFER.lock().pop()
}

/// Poll for next mouse byte (called from syscall)
///
/// Returns None if buffer is empty.
/// Userspace is responsible for parsing PS/2 packets.
pub fn poll_mouse_byte() -> Option<u8> {
    MOUSE_BUFFER.lock().pop()
}

/// Clear the keyboard buffer
///
/// Used when userspace driver initializes to clear any stale data.
pub fn clear_keyboard_buffer() {
    KEYBOARD_BUFFER.lock().clear();
}

/// Clear the mouse buffer
///
/// Used when userspace driver initializes to clear any stale data.
pub fn clear_mouse_buffer() {
    MOUSE_BUFFER.lock().clear();
}

// Note: PS/2 initialization functions have been removed from the kernel.
//
// In the microkernel architecture:
// - Kernel only provides IRQ handling and raw byte buffering
// - Userspace drivers use SYS_IO_PORT_READ/WRITE to access PS/2 ports
// - Userspace drivers configure scaling, resolution, sample rate
// - Userspace drivers interpret raw bytes into meaningful events
//
// The userspace keyboard driver should:
// 1. Use SYS_IO_PORT_WRITE to configure the keyboard controller
// 2. Poll SYS_KEYBOARD_POLL for raw scancodes
// 3. Translate scancodes to key events (with modifier handling)
// 4. Send events to the desktop environment via IPC
//
// The userspace mouse driver should:
// 1. Use SYS_IO_PORT_WRITE to initialize the PS/2 controller
// 2. Configure mouse settings (scaling, resolution, sample rate)
// 3. Poll SYS_MOUSE_POLL for raw bytes
// 4. Parse 3-byte PS/2 packets into mouse events
// 5. Send events to the desktop environment via IPC
