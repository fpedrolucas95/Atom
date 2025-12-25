// Temporary PS/2 Keyboard Driver (Kernel-Space)
//
// Provides a minimal PS/2 keyboard driver with basic ASCII translation
// (set 1 scancodes) for early development, debugging, and boot-time
// interaction.
//
// Important design note:
// This driver is temporary and intentionally simplistic.
// In the final architecture, **hardware drivers (including keyboard input)
// will live in user space**, not in the kernel. This kernel-side driver
// exists only to bootstrap the system and support early diagnostics.
//
// Key responsibilities (temporary):
// - Read raw scancodes from PS/2 I/O ports (0x60 / 0x64)
// - Translate scancodes into ASCII characters
// - Track modifier state (Shift, Ctrl, Alt, Caps Lock)
// - Buffer input characters in a small ring buffer
// - Integrate with the interrupt handler for keyboard IRQs
//
// Design characteristics:
// - Uses direct port I/O via inline assembly
// - Maintains global keyboard state protected by a spinlock
// - Fixed-size circular buffer to avoid dynamic allocation
// - Best-effort ASCII mapping (US layout, no localization)
//
// Limitations (by design):
// - PS/2 only (no USB, HID, or modern input devices)
// - No key release events exposed to consumers
// - No international layouts or Unicode support
// - No capability checks or per-process input routing
//
// Future direction:
// - Keyboard input will be handled by a user-space driver
// - Kernel will expose only a generic IRQ + input event mechanism
// - Input will be delivered via IPC with capability-based access
// - This file will eventually be removed or reduced to a stub
//
// Safety and correctness notes:
// - All hardware access is confined to small `unsafe` blocks
// - Ring buffer drops input on overflow to preserve kernel stability
// - Intended for single-consumer use during early boot

use spin::Mutex;

use crate::{log_debug, log_info};

const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const BUFFER_CAPACITY: usize = 64;

struct KeyboardState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps_lock: bool,
    extended: bool,
    buffer: [u8; BUFFER_CAPACITY],
    head: usize,
    tail: usize,
}

impl KeyboardState {
    const fn new() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            caps_lock: false,
            extended: false,
            buffer: [0; BUFFER_CAPACITY],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, byte: u8) {
        let next_head = (self.head + 1) % BUFFER_CAPACITY;
        if next_head != self.tail {
            self.buffer[self.head] = byte;
            self.head = next_head;
        }
    }
}

static KEYBOARD: Mutex<KeyboardState> = Mutex::new(KeyboardState::new());

// Raw scancode buffer for userspace polling
static SCANCODE_BUFFER: Mutex<([u8; 64], usize, usize)> = Mutex::new(([0; 64], 0, 0));

pub fn init() {
    let mut state = KEYBOARD.lock();
    *state = KeyboardState::new();
    log_info!("keyboard", "Keyboard driver ready (PS/2 set 1)");
}

/// Push a raw scancode to the buffer for userspace consumption
pub fn push_raw_scancode(scancode: u8) {
    let mut buf = SCANCODE_BUFFER.lock();
    let head = buf.1;
    let tail = buf.2;
    let next_head = (head + 1) % 64;
    if next_head != tail {
        buf.0[head] = scancode;
        buf.1 = next_head;
    }
}

/// Poll for a raw scancode from userspace
pub fn poll_scancode() -> Option<u8> {
    let mut buf = SCANCODE_BUFFER.lock();
    let head = buf.1;
    let tail = buf.2;
    if head != tail {
        let scancode = buf.0[tail];
        buf.2 = (tail + 1) % 64;
        Some(scancode)
    } else {
        None
    }
}

pub fn handle_interrupt() {
    process_pending_scancodes();
    
}

fn process_pending_scancodes() {
    let mut state = KEYBOARD.lock();

    while let Some(scancode) = read_scancode() {
        // Push raw scancode to buffer for userspace
        drop(state); // Release lock before pushing
        push_raw_scancode(scancode);
        state = KEYBOARD.lock();
        
        process_scancode(scancode, &mut state);
    }
}

fn read_scancode() -> Option<u8> {
    unsafe {
        let status: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") status,
            in("dx") PS2_STATUS_PORT,
            options(nomem, nostack, preserves_flags)
        );

        if status & 0x01 == 0 {
            return None;
        }

        let data: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") data,
            in("dx") PS2_DATA_PORT,
            options(nomem, nostack, preserves_flags)
        );

        Some(data)
    }
}

fn process_scancode(scancode: u8, state: &mut KeyboardState) {
    if state.extended {
        state.extended = false;
    }

    if scancode == 0xE0 {
        state.extended = true;
        return;
    }

    let is_break = scancode & 0x80 != 0;
    let code = scancode & 0x7F;

    match code {
        0x2A | 0x36 => {
            state.shift = !is_break;
            return;
        }
        0x1D => {
            state.ctrl = !is_break;
            return;
        }
        0x38 => {
            state.alt = !is_break;
            return;
        }
        0x3A => {
            if !is_break {
                state.caps_lock = !state.caps_lock;
            }
            return;
        }
        _ => {}
    }

    if is_break {
        return;
    }

    if let Some(ascii) = translate_scancode(code, state.shift, state.caps_lock) {
        state.push(ascii);
        log_debug!("keyboard", "Scancode 0x{:02X} queued", scancode);
    }
}

fn translate_scancode(scancode: u8, shift: bool, caps_lock: bool) -> Option<u8> {
    match scancode {
        0x02 => Some(if shift { b'!' } else { b'1' }),
        0x03 => Some(if shift { b'@' } else { b'2' }),
        0x04 => Some(if shift { b'#' } else { b'3' }),
        0x05 => Some(if shift { b'$' } else { b'4' }),
        0x06 => Some(if shift { b'%' } else { b'5' }),
        0x07 => Some(if shift { b'^' } else { b'6' }),
        0x08 => Some(if shift { b'&' } else { b'7' }),
        0x09 => Some(if shift { b'*' } else { b'8' }),
        0x0A => Some(if shift { b'(' } else { b'9' }),
        0x0B => Some(if shift { b')' } else { b'0' }),
        0x0C => Some(if shift { b'_' } else { b'-' }),
        0x0D => Some(if shift { b'+' } else { b'=' }),
        0x0E => Some(0x08), 
        0x0F => Some(b'\t'),
        0x10 => Some(letter(b'q', shift, caps_lock)),
        0x11 => Some(letter(b'w', shift, caps_lock)),
        0x12 => Some(letter(b'e', shift, caps_lock)),
        0x13 => Some(letter(b'r', shift, caps_lock)),
        0x14 => Some(letter(b't', shift, caps_lock)),
        0x15 => Some(letter(b'y', shift, caps_lock)),
        0x16 => Some(letter(b'u', shift, caps_lock)),
        0x17 => Some(letter(b'i', shift, caps_lock)),
        0x18 => Some(letter(b'o', shift, caps_lock)),
        0x19 => Some(letter(b'p', shift, caps_lock)),
        0x1A => Some(if shift { b'{' } else { b'[' }),
        0x1B => Some(if shift { b'}' } else { b']' }),
        0x1C => Some(b'\n'),
        0x1E => Some(letter(b'a', shift, caps_lock)),
        0x1F => Some(letter(b's', shift, caps_lock)),
        0x20 => Some(letter(b'd', shift, caps_lock)),
        0x21 => Some(letter(b'f', shift, caps_lock)),
        0x22 => Some(letter(b'g', shift, caps_lock)),
        0x23 => Some(letter(b'h', shift, caps_lock)),
        0x24 => Some(letter(b'j', shift, caps_lock)),
        0x25 => Some(letter(b'k', shift, caps_lock)),
        0x26 => Some(letter(b'l', shift, caps_lock)),
        0x27 => Some(if shift { b':' } else { b';' }),
        0x28 => Some(if shift { b'"' } else { b'\'' }),
        0x29 => Some(if shift { b'~' } else { b'`' }),
        0x2B => Some(if shift { b'|' } else { b'\\' }),
        0x2C => Some(letter(b'z', shift, caps_lock)),
        0x2D => Some(letter(b'x', shift, caps_lock)),
        0x2E => Some(letter(b'c', shift, caps_lock)),
        0x2F => Some(letter(b'v', shift, caps_lock)),
        0x30 => Some(letter(b'b', shift, caps_lock)),
        0x31 => Some(letter(b'n', shift, caps_lock)),
        0x32 => Some(letter(b'm', shift, caps_lock)),
        0x33 => Some(if shift { b'<' } else { b',' }),
        0x34 => Some(if shift { b'>' } else { b'.' }),
        0x35 => Some(if shift { b'?' } else { b'/' }),
        0x39 => Some(b' '),
        _ => None,
    }
}

fn letter(base: u8, shift: bool, caps_lock: bool) -> u8 {
    let upper = shift ^ caps_lock;
    if upper {
        base.to_ascii_uppercase()
    } else {
        base
    }
}