// Input Subsystem - PS/2 Controller and Mouse/Keyboard IRQ Buffering
//
// This module implements the full PS/2 controller initialization and provides
// IRQ-safe ring buffers for raw hardware events. The actual driver logic
// (packet parsing, event generation) runs in userspace.
//
// PS/2 Mouse initialization follows the OSDev specification:
// 1. Disable devices during configuration
// 2. Configure controller (Compaq status byte: enable IRQ1 + IRQ12)
// 3. Re-enable ports
// 4. Reset keyboard, set scancode set 2
// 5. Reset mouse to defaults
// 6. Detect scroll wheel via magic sample rate sequence (200, 100, 80)
// 7. Detect 4th/5th buttons via second magic sequence (200, 200, 80)
// 8. Configure sample rate, resolution, scaling
// 9. Enable data reporting (streaming mode)
// 10. Re-verify controller config (guards against config corruption)
//
// The detected mouseID (0, 3, or 4) determines packet size:
//   - ID 0: 3-byte packets (basic mouse)
//   - ID 3: 4-byte packets (scroll wheel, Z-axis)
//   - ID 4: 4-byte packets (scroll wheel + 4th/5th buttons)
//
// Public interface:
// - `init()` - Initialize the input subsystem
// - `init_ps2_mouse_full()` - Full PS/2 controller init
// - `on_keyboard_irq()` - Called from keyboard interrupt handler
// - `on_mouse_irq()` - Called from mouse interrupt handler
// - `poll_keyboard_byte()` - Syscall: get next keyboard byte
// - `poll_mouse_byte()` - Syscall: get next mouse byte
// - `get_mouse_id()` - Return detected mouseID (0, 3, or 4)

use spin::Mutex;
use core::sync::atomic::{AtomicU8, Ordering};
use crate::log_info;

// ============================================================================
// PS/2 Hardware Constants
// ============================================================================

/// PS/2 data port - reads/writes data bytes
const PS2_DATA_PORT: u16 = 0x60;
/// PS/2 status/command port - reads status, writes commands
const PS2_STATUS_PORT: u16 = 0x64;

/// Status register bit: output buffer full (data available to read)
const STATUS_OUTPUT_FULL: u8 = 0x01;
/// Status register bit: input buffer full (controller busy, wait before writing)
const STATUS_INPUT_FULL: u8 = 0x02;
/// Status register bit: data came from auxiliary (mouse) port
const STATUS_AUX_DATA: u8 = 0x20;

/// PS/2 controller commands (written to STATUS_PORT)
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_KEYBOARD: u8 = 0xAD;
const CMD_ENABLE_KEYBOARD: u8 = 0xAE;
const CMD_DISABLE_MOUSE: u8 = 0xA7;
const CMD_ENABLE_MOUSE: u8 = 0xA8;
const CMD_SEND_TO_MOUSE: u8 = 0xD4;

/// PS/2 mouse commands (sent via CMD_SEND_TO_MOUSE)
const MOUSE_CMD_RESET: u8 = 0xFF;
const MOUSE_CMD_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_CMD_DISABLE_STREAMING: u8 = 0xF5;
const MOUSE_CMD_ENABLE_STREAMING: u8 = 0xF4;
const MOUSE_CMD_SET_SAMPLE_RATE: u8 = 0xF3;
const MOUSE_CMD_GET_ID: u8 = 0xF2;
const MOUSE_CMD_SET_RESOLUTION: u8 = 0xE8;
const MOUSE_CMD_SET_SCALING_1_1: u8 = 0xE6;

/// PS/2 response codes
const PS2_ACK: u8 = 0xFA;
const PS2_SELF_TEST_PASS: u8 = 0xAA;
const PS2_SELF_TEST_FAIL: u8 = 0xFC;

/// Controller config bits
const CONFIG_IRQ1_ENABLE: u8 = 0x01;     // bit 0: keyboard interrupt
const CONFIG_IRQ12_ENABLE: u8 = 0x02;    // bit 1: mouse interrupt
const CONFIG_KEYBOARD_CLOCK: u8 = 0x10;  // bit 4: disable keyboard clock
const CONFIG_MOUSE_CLOCK: u8 = 0x20;     // bit 5: disable mouse clock
const CONFIG_TRANSLATION: u8 = 0x40;     // bit 6: scancode translation

/// Timeout for waiting on PS/2 controller (iterations)
const PS2_TIMEOUT: u32 = 100_000;

// ============================================================================
// Ring Buffers
// ============================================================================

const KEYBOARD_BUFFER_SIZE: usize = 128;
const MOUSE_BUFFER_SIZE: usize = 256;

/// Lock-free ring buffer for raw input bytes (power-of-2 size)
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
            return false; // Buffer full
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

    fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
    }
}

// ============================================================================
// Global State
// ============================================================================

static KEYBOARD_BUFFER: Mutex<RingBuffer<KEYBOARD_BUFFER_SIZE>> =
    Mutex::new(RingBuffer::new());
static MOUSE_BUFFER: Mutex<RingBuffer<MOUSE_BUFFER_SIZE>> =
    Mutex::new(RingBuffer::new());

/// Detected mouse ID: 0 = basic, 3 = scroll wheel, 4 = scroll + 5 buttons
static MOUSE_ID: AtomicU8 = AtomicU8::new(0);

// ============================================================================
// Low-Level PS/2 I/O
// ============================================================================

/// Read the PS/2 status register (port 0x64)
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

/// Read a byte from the PS/2 data port (port 0x60)
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

/// Write a command byte to the PS/2 command port (port 0x64)
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

/// Write a data byte to the PS/2 data port (port 0x60)
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

/// Wait until the PS/2 input buffer is empty (ready to accept writes).
/// Must be called before every write to port 0x60 or 0x64.
fn wait_for_input_buffer() -> bool {
    for _ in 0..PS2_TIMEOUT {
        if read_status() & STATUS_INPUT_FULL == 0 {
            return true;
        }
    }
    false
}

/// Wait until the PS/2 output buffer has data (ready to read from port 0x60).
fn wait_for_output_buffer() -> bool {
    for _ in 0..PS2_TIMEOUT {
        if read_status() & STATUS_OUTPUT_FULL != 0 {
            return true;
        }
    }
    false
}

/// Drain any pending data from the PS/2 output buffer
fn flush_output_buffer() {
    for _ in 0..100 {
        if read_status() & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let _ = read_data();
    }
}

// ============================================================================
// Mouse Command Helpers
// ============================================================================

/// Send a command byte to the mouse via the PS/2 controller.
/// Sequence: write 0xD4 to port 0x64 (prefix), then write command to port 0x60.
/// Waits for and returns the ACK byte (should be 0xFA).
fn mouse_write(cmd: u8) -> u8 {
    wait_for_input_buffer();
    write_command(CMD_SEND_TO_MOUSE);
    wait_for_input_buffer();
    write_data(cmd);
    // Wait for ACK
    if wait_for_output_buffer() {
        read_data()
    } else {
        0 // Timeout
    }
}

/// Set mouse sample rate (two-byte command: 0xF3 + rate value).
/// Valid rates: 10, 20, 40, 60, 80, 100, 200 samples/sec.
fn set_mouse_sample_rate(rate: u8) {
    mouse_write(MOUSE_CMD_SET_SAMPLE_RATE);
    mouse_write(rate);
}

/// Read the current mouseID via command 0xF2.
/// After the ACK, the mouse sends one byte: the current ID.
fn read_mouse_id() -> u8 {
    mouse_write(MOUSE_CMD_GET_ID);
    if wait_for_output_buffer() {
        read_data()
    } else {
        0 // Timeout, assume basic mouse
    }
}

// ============================================================================
// Public Interface
// ============================================================================

/// Initialize the input subsystem
pub fn init() {
    log_info!("input", "Input subsystem initialized (userspace driver model)");
    log_info!("input", "Keyboard buffer: {} bytes, Mouse buffer: {} bytes",
        KEYBOARD_BUFFER_SIZE, MOUSE_BUFFER_SIZE);
}

/// Return the detected mouseID (0, 3, or 4).
/// Called from syscall handler.
pub fn get_mouse_id() -> u8 {
    MOUSE_ID.load(Ordering::Relaxed)
}

/// Called from keyboard interrupt handler (IRQ1).
/// Reads available keyboard data and buffers it for userspace.
pub fn on_keyboard_irq() {
    let mut buf = KEYBOARD_BUFFER.lock();

    // Read all available keyboard data (not mouse data)
    while read_status() & STATUS_OUTPUT_FULL != 0 {
        if read_status() & STATUS_AUX_DATA != 0 {
            break; // Mouse data, not keyboard
        }
        let scancode = read_data();
        buf.push(scancode);
    }
}

/// Called from mouse interrupt handler (IRQ12).
/// Reads available mouse data (marked with AUX bit) and buffers it.
pub fn on_mouse_irq() {
    let mut buf = MOUSE_BUFFER.lock();

    // Read all available mouse data
    while read_status() & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)
        == (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)
    {
        let byte = read_data();
        buf.push(byte);
    }
}

/// Poll for next keyboard byte (called from syscall).
/// Returns None if buffer is empty.
pub fn poll_keyboard_byte() -> Option<u8> {
    KEYBOARD_BUFFER.lock().pop()
}

/// Poll for next mouse byte (called from syscall).
/// Returns None if buffer is empty.
pub fn poll_mouse_byte() -> Option<u8> {
    MOUSE_BUFFER.lock().pop()
}

// ============================================================================
// Full PS/2 Controller Initialization
// ============================================================================

/// Initialize PS/2 controller for keyboard and mouse support.
///
/// Follows the OSDev wiki PS/2 mouse initialization procedure:
/// - Configures controller interrupts (IRQ1 for keyboard, IRQ12 for mouse)
/// - Resets keyboard and enables scancode set 2 with translation
/// - Resets mouse and detects extensions (scroll wheel, extra buttons)
/// - Configures sample rate (60/sec), resolution (8 count/mm), linear scaling
/// - Enables streaming mode for automatic packet generation
pub fn init_ps2_mouse_full() {
    log_info!("input", "Initializing PS/2 controller (keyboard + mouse)...");

    // ========================================================================
    // Phase 1: Controller Configuration
    // ========================================================================

    // Drain any pending data before touching the controller
    flush_output_buffer();

    // Disable both devices during configuration
    wait_for_input_buffer();
    write_command(CMD_DISABLE_KEYBOARD);
    wait_for_input_buffer();
    write_command(CMD_DISABLE_MOUSE);

    // Flush output buffer again after disabling
    flush_output_buffer();

    // Read the current controller configuration byte (Compaq status byte)
    wait_for_input_buffer();
    write_command(CMD_READ_CONFIG);
    wait_for_output_buffer();
    let config = read_data();

    // Configure: Enable IRQ1 (keyboard) + IRQ12 (mouse) + Translation
    // Clear: Keyboard clock disable + Mouse clock disable
    let new_config = (config | CONFIG_IRQ1_ENABLE | CONFIG_IRQ12_ENABLE | CONFIG_TRANSLATION)
        & !(CONFIG_KEYBOARD_CLOCK | CONFIG_MOUSE_CLOCK);

    log_info!("input", "PS/2 config: 0x{:02X} -> 0x{:02X} (IRQ1+IRQ12+Translation)",
        config, new_config);

    wait_for_input_buffer();
    write_command(CMD_WRITE_CONFIG);
    wait_for_input_buffer();
    write_data(new_config);

    // Re-enable both ports
    wait_for_input_buffer();
    write_command(CMD_ENABLE_KEYBOARD);
    wait_for_input_buffer();
    write_command(CMD_ENABLE_MOUSE);
    log_info!("input", "PS/2 ports enabled (keyboard + mouse)");

    // ========================================================================
    // Phase 2: Keyboard Initialization
    // ========================================================================

    log_info!("input", "PS/2 keyboard: Sending reset...");
    wait_for_input_buffer();
    write_data(MOUSE_CMD_RESET); // 0xFF to keyboard (directly via port 0x60)

    // Wait for ACK (0xFA)
    if wait_for_output_buffer() {
        let ack = read_data();
        if ack == PS2_ACK {
            log_info!("input", "PS/2 keyboard: Reset ACK received");
        } else {
            log_info!("input", "PS/2 keyboard: Reset response: 0x{:02X}", ack);
        }
    }

    // Wait for self-test (0xAA = pass, 0xFC = fail)
    for _ in 0..1000 {
        if read_status() & STATUS_OUTPUT_FULL != 0 {
            let result = read_data();
            if result == PS2_SELF_TEST_PASS {
                log_info!("input", "PS/2 keyboard: Self-test passed (0xAA)");
                break;
            } else if result == PS2_SELF_TEST_FAIL {
                log_info!("input", "PS/2 keyboard: Self-test FAILED (0xFC)");
                break;
            }
        }
        for _ in 0..10_000 { core::hint::spin_loop(); }
    }

    // Set scancode set 2 (with translation enabled, CPU sees Set 1 codes)
    wait_for_input_buffer();
    write_data(0xF0); // Set scancode set command
    wait_for_output_buffer();
    let _ = read_data(); // ACK

    wait_for_input_buffer();
    write_data(0x02); // Scancode Set 2
    wait_for_output_buffer();
    let scanset_ack = read_data();
    log_info!("input", "PS/2 keyboard: Scancode set 2 response: 0x{:02X}", scanset_ack);

    // Enable keyboard scanning
    wait_for_input_buffer();
    write_data(MOUSE_CMD_ENABLE_STREAMING); // 0xF4 enables keyboard scanning too
    wait_for_output_buffer();
    let kbd_ack = read_data();
    log_info!("input", "PS/2 keyboard: Scanning enabled (0x{:02X})", kbd_ack);

    // ========================================================================
    // Phase 3: Mouse Initialization
    // ========================================================================

    log_info!("input", "PS/2 mouse: Initializing...");

    // Set defaults (resets to: packets disabled, 3-byte mode, 100/sec, 4px/mm)
    mouse_write(MOUSE_CMD_SET_DEFAULTS);

    // Disable streaming during configuration
    mouse_write(MOUSE_CMD_DISABLE_STREAMING);

    // ========================================================================
    // Phase 4: Detect Mouse Extensions (Scroll Wheel + Extra Buttons)
    // ========================================================================

    // Magic sequence #1: Enable scroll wheel (mouseID 0 -> 3)
    // Set sample rate to 200, 100, 80, then read ID
    set_mouse_sample_rate(200);
    set_mouse_sample_rate(100);
    set_mouse_sample_rate(80);

    let mouse_id = read_mouse_id();
    log_info!("input", "PS/2 mouse: After wheel detection sequence, mouseID = {}", mouse_id);

    let mut final_id = mouse_id;

    if mouse_id == 3 {
        log_info!("input", "PS/2 mouse: Scroll wheel detected (mouseID 3)");

        // Magic sequence #2: Enable 4th and 5th buttons (mouseID 3 -> 4)
        // Set sample rate to 200, 200, 80, then read ID
        set_mouse_sample_rate(200);
        set_mouse_sample_rate(200);
        set_mouse_sample_rate(80);

        let mouse_id2 = read_mouse_id();
        log_info!("input", "PS/2 mouse: After 5-button detection sequence, mouseID = {}", mouse_id2);

        if mouse_id2 == 4 {
            log_info!("input", "PS/2 mouse: 5-button mouse detected (mouseID 4)");
            final_id = 4;
        } else {
            final_id = 3;
        }
    } else {
        log_info!("input", "PS/2 mouse: Basic 3-button mouse (mouseID 0)");
    }

    MOUSE_ID.store(final_id, Ordering::Relaxed);

    // ========================================================================
    // Phase 5: Configure Mouse Parameters
    // ========================================================================

    // Set scaling 1:1 (linear, no acceleration)
    mouse_write(MOUSE_CMD_SET_SCALING_1_1);
    log_info!("input", "PS/2 mouse: Scaling set to 1:1 (linear)");

    // Set resolution to 8 count/mm (value 0x03) for highest precision
    mouse_write(MOUSE_CMD_SET_RESOLUTION);
    mouse_write(0x03);
    log_info!("input", "PS/2 mouse: Resolution set to 8 count/mm");

    // Set sample rate to 60 samples/sec
    // OSDev docs: human eyes don't see faster than 30 fps, rates < 30 are jerky,
    // rates > 100 waste I/O bus bandwidth. 60 is a good balance.
    set_mouse_sample_rate(60);
    log_info!("input", "PS/2 mouse: Sample rate set to 60/sec");

    // ========================================================================
    // Phase 6: Enable Streaming and Finalize
    // ========================================================================

    // Enable data reporting (mouse starts sending packets on movement/clicks)
    mouse_write(MOUSE_CMD_ENABLE_STREAMING);
    log_info!("input", "PS/2 mouse: Streaming enabled");

    // CRITICAL: Re-write controller configuration byte.
    // Mouse initialization can corrupt the PS/2 controller config byte
    // (observed: 0x47 -> 0x00), disabling both IRQ1 and IRQ12.
    wait_for_input_buffer();
    write_command(CMD_WRITE_CONFIG);
    wait_for_input_buffer();
    write_data(new_config);

    // Verify final controller configuration
    wait_for_input_buffer();
    write_command(CMD_READ_CONFIG);
    wait_for_output_buffer();
    let final_config = read_data();
    log_info!("input", "PS/2 controller final config: 0x{:02X} (IRQ1={}, IRQ12={})",
        final_config,
        if final_config & CONFIG_IRQ1_ENABLE != 0 { "on" } else { "OFF" },
        if final_config & CONFIG_IRQ12_ENABLE != 0 { "on" } else { "OFF" }
    );

    // Clear any leftover ACK bytes from the mouse buffer
    // (IRQ12 may have captured ACKs during initialization)
    {
        let mut buf = MOUSE_BUFFER.lock();
        buf.clear();
    }

    // Drain hardware buffer
    flush_output_buffer();

    log_info!("input", "PS/2 mouse initialized: mouseID={}, 1:1 scaling, 8 count/mm, 60 samples/sec",
        final_id);
}
