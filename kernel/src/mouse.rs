// Temporary PS/2 mouse driver
//
// This module implements a minimal kernel-space PS/2 mouse driver based on
// well-established OSDev reference implementations. It provides basic mouse
// movement tracking with a simple and deterministic design, suitable for
// early kernel development and diagnostics.
//
// Key responsibilities:
// - Initialize the PS/2 auxiliary (mouse) device
// - Enable IRQ12 and packet streaming mode
// - Receive and assemble 3-byte PS/2 mouse packets
// - Decode relative X/Y movement deltas
// - Expose movement data for consumption by higher-level subsystems
//
// Design and implementation:
// - Follows the standard 3-byte PS/2 mouse packet format
// - Mouse bytes are treated directly as signed 8-bit values
// - Packet validation enforces bit-3 synchronization
// - Overflow packets are detected and discarded
// - Uses polling-style waits with bounded spin loops
// - Relies on direct I/O port access to the i8042 controller
//
// Safety and correctness notes:
// - All hardware I/O is performed inside explicit `unsafe` blocks
// - Interrupt handler drains all available AUX bytes per IRQ
// - Global mouse state is protected by a spinlock
// - Atomic counters are used for lightweight diagnostics
// - Zero-movement packets are ignored to reduce noise
//
// Limitations and future considerations:
// - Supports only basic PS/2 mouse features (no scroll wheel or buttons)
// - No absolute positioning or acceleration
// - No hot-plug or device reinitialization handling
// - Designed for single-device, single-consumer usage
//
// References:
// - OSDev Wiki, “PS/2 Mouse”
//   https://wiki.osdev.org/PS/2_Mouse
// - OSDev Forum discussion on PS/2 mouse handling
//   https://forum.osdev.org/viewtopic.php?t=10247
// - OSDev Forum discussion on PS/2 packet decoding and sign handling
//   https://forum.osdev.org/viewtopic.php?t=24277
//
// Public interface:
// - `init` to initialize the PS/2 mouse device
// - `handle_interrupt` as the IRQ12 handler
// - `drain_delta` to retrieve accumulated mouse movement

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use spin::Mutex;
use x86_64::instructions::interrupts;

use crate::{log_debug, log_info, log_warn};

const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;
const PS2_DATA_PORT: u16 = 0x60;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_ENABLE_AUX: u8 = 0xA8;

const AUX_PREFIX: u8 = 0xD4;
const AUX_ENABLE_PACKET_STREAM: u8 = 0xF4;
const AUX_SET_DEFAULTS: u8 = 0xF6;
const WAIT_INPUT_EMPTY_SPINS: u32 = 50_000;
const WAIT_ANY_OUTPUT_SPINS: u32 = 50_000;
#[allow(dead_code)]
const WAIT_AUX_OUTPUT_SPINS: u32 = 200_000;

#[derive(Clone, Copy)]
pub struct MouseDelta {
    dx: i8,
    dy: i8,
}

struct MouseState {
    packet: [u8; 3],
    cycle: u8, 
    delta: Option<MouseDelta>,
}

impl MouseState {
    const fn new() -> Self {
        Self {
            packet: [0; 3],
            cycle: 0,
            delta: None,
        }
    }
}

static MOUSE: Mutex<MouseState> = Mutex::new(MouseState::new());
static MOUSE_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
static PACKET_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    {
        let mut state = MOUSE.lock();
        *state = MouseState::new();
    }

    drain_aux_buffer();
    enable_aux_channel();
    enable_interrupts_in_controller();

    if !set_defaults_and_enable() {
        log_warn!("mouse", "PS/2 mouse init failed");
        return;
    }

    log_info!("mouse", "PS/2 mouse initialized (default settings, IRQ12 enabled)");
}

pub fn handle_interrupt() {
    let count = MOUSE_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);

    if count <= 10 || count % 100 == 0 {
        log_debug!("mouse", "Mouse IRQ12 fired (count={})", count);
    }

    let mut state = MOUSE.lock();

    while aux_data_available() {
        let byte = read_data_port();
        process_mouse_byte(&mut state, byte);
    }
}

fn aux_data_available() -> bool {
    let status = read_status();
    status & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA) == (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)
}

fn drain_aux_buffer() {
    while aux_data_available() {
        let _ = read_data_port();
    }
}

fn process_mouse_byte(state: &mut MouseState, byte: u8) {
    match state.cycle {
        0 => {
            if byte & 0x08 == 0 {
                return; 
            }
            state.packet[0] = byte;
            state.cycle = 1;
        }
        1 => {
            state.packet[1] = byte;
            state.cycle = 2;
        }
        2 => {
            state.packet[2] = byte;
            state.cycle = 0;
            finalize_packet(state);
        }
        _ => {
            state.cycle = 0;
        }
    }
}

fn finalize_packet(state: &mut MouseState) {
    let flags = state.packet[0];

    if flags & 0x08 == 0 {
        return;
    }

    let packet_count = PACKET_COUNT.fetch_add(1, Ordering::Relaxed);

    let x_overflow = flags & 0x40 != 0;
    let y_overflow = flags & 0x80 != 0;
    if x_overflow || y_overflow {
        if packet_count < 5 {
            log_warn!("mouse", "Overflow detected, discarding packet");
        }
        return;
    }
    
    let dx = state.packet[1] as i8;
    let dy = state.packet[2] as i8;

    if packet_count < 20 {
        log_info!("mouse", "Packet #{}: dx={}, dy={} (raw bytes: {:#04X}, {:#04X}, flags={:#04X})",
            packet_count + 1, dx, dy, state.packet[1], state.packet[2], flags);
    }

    if dx == 0 && dy == 0 {
        return;
    }

    state.delta = Some(MouseDelta { dx, dy });
}

pub fn drain_delta() -> Option<(i32, i32)> {
    interrupts::without_interrupts(|| {
        let mut state = MOUSE.lock();

        if let Some(delta) = state.delta.take() {
            Some((delta.dx as i32, delta.dy as i32))
        } else {
            None
        }
    })
}

fn enable_aux_channel() {
    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_COMMAND_PORT,
            in("al") CMD_ENABLE_AUX,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn enable_interrupts_in_controller() {
    let original_config = read_command_byte();
    log_debug!("mouse", "i8042 command byte original: {:#04X}", original_config);

    let mut config = original_config;

    config |= 0x03;
    config &= !0x30;

    write_command_byte(config);

    let readback_config = read_command_byte();
    log_info!(
        "mouse",
        "i8042 command byte: original={:#04X}, set to {:#04X}, readback={:#04X}",
        original_config,
        config,
        readback_config
    );

    if (readback_config & 0x02) == 0 {
        log_warn!("mouse", "IRQ12 (bit1) NOT enabled in i8042 command byte!");
    } else {
        log_info!("mouse", "IRQ12 (bit1) confirmed enabled");
    }
}

fn set_defaults_and_enable() -> bool {
    mouse_write(AUX_SET_DEFAULTS);
    let ack = mouse_read();
    if ack != 0xFA {
        log_warn!("mouse", "SET_DEFAULTS failed: got {:#04X}, expected 0xFA", ack);
        return false;
    }
    log_info!("mouse", "SET_DEFAULTS acknowledged");

    mouse_write(AUX_ENABLE_PACKET_STREAM);
    let ack = mouse_read();
    if ack != 0xFA {
        log_warn!("mouse", "ENABLE_STREAMING failed: got {:#04X}, expected 0xFA", ack);
        return false;
    }
    log_info!("mouse", "ENABLE_STREAMING acknowledged");

    true
}

fn mouse_write(data: u8) {
    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_COMMAND_PORT,
            in("al") AUX_PREFIX,
            options(nomem, nostack, preserves_flags)
        );
    }

    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_DATA_PORT,
            in("al") data,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn mouse_read() -> u8 {
    wait_any_output_full();
    read_data_port()
}

#[allow(dead_code)]
fn wait_for_aux_byte() -> Option<u8> {
    for _ in 0..WAIT_AUX_OUTPUT_SPINS {
        let status = read_status();

        if (status & STATUS_OUTPUT_FULL) != 0 {
            if (status & STATUS_AUX_DATA) != 0 {
                return Some(read_data_port());
            }
        }

        core::hint::spin_loop();
    }

    None
}

fn read_command_byte() -> u8 {
    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_COMMAND_PORT,
            in("al") CMD_READ_CONFIG,
            options(nomem, nostack, preserves_flags)
        );
    }

    wait_any_output_full();
    read_data_port()
}

fn write_command_byte(config: u8) {
    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_COMMAND_PORT,
            in("al") CMD_WRITE_CONFIG,
            options(nomem, nostack, preserves_flags)
        );
    }
    wait_input_empty();
    unsafe {
        asm!(
            "out dx, al",
            in("dx") PS2_DATA_PORT,
            in("al") config,
            options(nomem, nostack, preserves_flags)
        );
    }
}

fn read_status() -> u8 {
    let mut status: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") status,
            in("dx") PS2_STATUS_PORT,
            options(nomem, nostack, preserves_flags)
        );
    }
    status
}

fn read_data_port() -> u8 {
    let mut data: u8;
    unsafe {
        asm!(
            "in al, dx",
            out("al") data,
            in("dx") PS2_DATA_PORT,
            options(nomem, nostack, preserves_flags)
        );
    }
    data
}

fn wait_input_empty() {
    for _ in 0..WAIT_INPUT_EMPTY_SPINS {
        if read_status() & STATUS_INPUT_FULL == 0 {
            return;
        }
        core::hint::spin_loop();
    }
    log_warn!("mouse", "PS/2 controller input buffer did not drain in time");
}

fn wait_any_output_full() {
    for _ in 0..WAIT_ANY_OUTPUT_SPINS {
        if read_status() & STATUS_OUTPUT_FULL != 0 {
            return;
        }
        core::hint::spin_loop();
    }
    log_warn!("mouse", "Timed out waiting for PS/2 controller output");
}
