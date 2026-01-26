//! Userspace PS/2 Keyboard Driver
//!
//! This driver runs entirely in Ring 3 (userspace) and:
//! - Polls raw scancodes from kernel input buffer
//! - Translates scancodes to ASCII (US layout)
//! - Tracks modifier keys (Shift, Ctrl, Alt, Caps Lock)
//! - Dispatches key events to the desktop environment via IPC
//!
//! # Architecture
//!
//! ```text
//! Kernel IRQ Buffer ──> Keyboard Driver ──> Desktop Environment
//!    (raw bytes)         (translation)       (IPC messages)
//! ```

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

// Simple bump allocator
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

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

use core::panic::PanicInfo;

use atom_syscall::input::keyboard_poll;
use atom_syscall::ipc::{create_port, send_async, PortId};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

use libipc::messages::{KeyEvent, KeyModifiers, MessageType, MessageHeader};
use libipc::protocol::send_message_async;
use libipc::well_known;
use atom_syscall::ipc::send;

// ============================================================================
// Keyboard State
// ============================================================================

const BUFFER_SIZE: usize = 64;

struct KeyboardState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps_lock: bool,
    extended: bool,
}

impl KeyboardState {
    const fn new() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            caps_lock: false,
            extended: false,
        }
    }

    fn modifiers(&self) -> KeyModifiers {
        KeyModifiers {
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
            caps_lock: self.caps_lock,
        }
    }

    fn process_scancode(&mut self, scancode: u8) -> Option<(u8, u8, bool)> {
        // Handle extended prefix
        if scancode == 0xE0 {
            self.extended = true;
            return None;
        }

        let extended = self.extended;
        self.extended = false;

        let is_release = (scancode & 0x80) != 0;
        let code = scancode & 0x7F;

        // Handle modifier keys
        match code {
            0x2A | 0x36 => {
                self.shift = !is_release;
                return None;
            }
            0x1D => {
                self.ctrl = !is_release;
                return None;
            }
            0x38 => {
                self.alt = !is_release;
                return None;
            }
            0x3A => {
                if !is_release {
                    self.caps_lock = !self.caps_lock;
                }
                return None;
            }
            _ => {}
        }

        // Translate to ASCII
        let ascii = if is_release {
            0
        } else {
            translate_scancode(code, self.shift, self.caps_lock, extended)
        };

        Some((scancode, ascii, !is_release))
    }
}

fn translate_scancode(code: u8, shift: bool, caps_lock: bool, _extended: bool) -> u8 {
    match code {
        // Numbers
        0x02 => if shift { b'!' } else { b'1' },
        0x03 => if shift { b'@' } else { b'2' },
        0x04 => if shift { b'#' } else { b'3' },
        0x05 => if shift { b'$' } else { b'4' },
        0x06 => if shift { b'%' } else { b'5' },
        0x07 => if shift { b'^' } else { b'6' },
        0x08 => if shift { b'&' } else { b'7' },
        0x09 => if shift { b'*' } else { b'8' },
        0x0A => if shift { b'(' } else { b'9' },
        0x0B => if shift { b')' } else { b'0' },
        0x0C => if shift { b'_' } else { b'-' },
        0x0D => if shift { b'+' } else { b'=' },

        // Special keys
        0x0E => 0x08,  // Backspace
        0x0F => b'\t', // Tab
        0x1C => b'\n', // Enter
        0x39 => b' ',  // Space
        0x01 => 0x1B,  // Escape

        // Punctuation
        0x1A => if shift { b'{' } else { b'[' },
        0x1B => if shift { b'}' } else { b']' },
        0x27 => if shift { b':' } else { b';' },
        0x28 => if shift { b'"' } else { b'\'' },
        0x29 => if shift { b'~' } else { b'`' },
        0x2B => if shift { b'|' } else { b'\\' },
        0x33 => if shift { b'<' } else { b',' },
        0x34 => if shift { b'>' } else { b'.' },
        0x35 => if shift { b'?' } else { b'/' },

        // Letters
        0x10 => letter(b'q', shift, caps_lock),
        0x11 => letter(b'w', shift, caps_lock),
        0x12 => letter(b'e', shift, caps_lock),
        0x13 => letter(b'r', shift, caps_lock),
        0x14 => letter(b't', shift, caps_lock),
        0x15 => letter(b'y', shift, caps_lock),
        0x16 => letter(b'u', shift, caps_lock),
        0x17 => letter(b'i', shift, caps_lock),
        0x18 => letter(b'o', shift, caps_lock),
        0x19 => letter(b'p', shift, caps_lock),
        0x1E => letter(b'a', shift, caps_lock),
        0x1F => letter(b's', shift, caps_lock),
        0x20 => letter(b'd', shift, caps_lock),
        0x21 => letter(b'f', shift, caps_lock),
        0x22 => letter(b'g', shift, caps_lock),
        0x23 => letter(b'h', shift, caps_lock),
        0x24 => letter(b'j', shift, caps_lock),
        0x25 => letter(b'k', shift, caps_lock),
        0x26 => letter(b'l', shift, caps_lock),
        0x2C => letter(b'z', shift, caps_lock),
        0x2D => letter(b'x', shift, caps_lock),
        0x2E => letter(b'c', shift, caps_lock),
        0x2F => letter(b'v', shift, caps_lock),
        0x30 => letter(b'b', shift, caps_lock),
        0x31 => letter(b'n', shift, caps_lock),
        0x32 => letter(b'm', shift, caps_lock),

        _ => 0,
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

// ============================================================================
// Keyboard Driver
// ============================================================================

struct KeyboardDriver {
    state: KeyboardState,
    compositor_port: PortId,
    event_count: u64,
}

impl KeyboardDriver {
    fn new(compositor_port: PortId) -> Self {
        Self {
            state: KeyboardState::new(),
            compositor_port,
            event_count: 0,
        }
    }

    fn run(&mut self) -> ! {
        log("Keyboard Driver: Starting PS/2 keyboard driver");
        log("Keyboard Driver: Connected to compositor event port");

        // Create our own IPC port for receiving commands (for future use)
        let _our_port = create_port().ok();

        log("Keyboard Driver: Entering main loop - waiting for keyboard input");

        let mut poll_count: u64 = 0;
        let mut last_log_count: u64 = 0;

        loop {
            poll_count += 1;

            // Log periodically to show driver is alive
            if poll_count - last_log_count >= 10000 {
                log("Keyboard Driver: Still polling... (no scancodes yet)");
                last_log_count = poll_count;
            }

            // Poll for raw scancodes from kernel
            while let Some(scancode) = keyboard_poll() {
                // Log the raw scancode to debug press vs release
                if scancode & 0x80 != 0 {
                    log("Keyboard Driver: Received RELEASE scancode");
                } else {
                    log("Keyboard Driver: Received PRESS scancode");
                }

                if let Some((sc, ascii, pressed)) = self.state.process_scancode(scancode) {
                    self.event_count += 1;
                    // Create key event
                    let event = KeyEvent {
                        scancode: sc,
                        character: ascii,
                        modifiers: self.state.modifiers(),
                    };

                    // Send to compositor (only key press events to reduce traffic)
                    if pressed {
                        log("Keyboard Driver: Sending PRESS to compositor");
                        let payload = event.to_bytes();
                        let _ = send_message_async(self.compositor_port, MessageType::KeyPress, &payload);
                    } else {
                        log("Keyboard Driver: RELEASE ignored (not sent)");
                    }
                }
            }

            yield_now();
        }
    }
}

// ============================================================================
// Entry Points
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

/// Register this service with the name service
fn register_with_namesvc(service_name: &str, port: PortId) {
    // Build registration message for name service
    // Format: [msg_type: u32][port: u64][name_len: u32][name: bytes]
    let mut msg = [0u8; 64];
    let msg_type = 600u32; // NsRegister
    msg[0..4].copy_from_slice(&msg_type.to_le_bytes());
    msg[4..12].copy_from_slice(&port.to_le_bytes());
    msg[12..16].copy_from_slice(&(service_name.len() as u32).to_le_bytes());
    msg[16..16 + service_name.len()].copy_from_slice(service_name.as_bytes());

    // Try to send to name service (best effort - name service may not be running yet)
    let _ = send(well_known::NAME_SERVICE, &msg[..16 + service_name.len()]);
    log("Keyboard: Registered with name service");
}

fn main() -> ! {
    // Well-known port: compositor event_port is always port 1
    // (created first in Compositor::new())
    const COMPOSITOR_EVENT_PORT: PortId = 1;

    // Create our IPC port and register with name service
    if let Ok(our_port) = create_port() {
        register_with_namesvc("keyboard", our_port);
    }

    let mut driver = KeyboardDriver::new(COMPOSITOR_EVENT_PORT);
    driver.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Keyboard Driver: PANIC!");
    exit(0xFF);
}
