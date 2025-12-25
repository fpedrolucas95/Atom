// Userspace PS/2 Keyboard Driver
//
// Complete implementation of PS/2 keyboard handling in userspace.
// Uses scan code set 1 for compatibility.
//
// Key features:
// - Scancode to ASCII translation
// - Modifier key tracking (Shift, Ctrl, Alt, Caps Lock)
// - Key buffer with overflow protection
// - US keyboard layout

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// ============================================================================
// Syscall Numbers
// ============================================================================

const SYS_THREAD_YIELD: u64 = 0;
const SYS_THREAD_EXIT: u64 = 1;
const SYS_KEYBOARD_POLL: u64 = 36;
const SYS_DEBUG_LOG: u64 = 39;

const EWOULDBLOCK: u64 = u64::MAX - 8;

// ============================================================================
// Keyboard State
// ============================================================================

const BUFFER_SIZE: usize = 64;

#[derive(Clone, Copy)]
pub struct KeyEvent {
    pub scancode: u8,
    pub ascii: u8,
    pub pressed: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

struct KeyboardState {
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps_lock: bool,
    extended: bool,
    buffer: [KeyEvent; BUFFER_SIZE],
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
            buffer: [KeyEvent {
                scancode: 0,
                ascii: 0,
                pressed: false,
                shift: false,
                ctrl: false,
                alt: false,
            }; BUFFER_SIZE],
            head: 0,
            tail: 0,
        }
    }

    fn push(&mut self, event: KeyEvent) {
        let next_head = (self.head + 1) % BUFFER_SIZE;
        if next_head != self.tail {
            self.buffer[self.head] = event;
            self.head = next_head;
        }
    }

    fn pop(&mut self) -> Option<KeyEvent> {
        if self.head == self.tail {
            None
        } else {
            let event = self.buffer[self.tail];
            self.tail = (self.tail + 1) % BUFFER_SIZE;
            Some(event)
        }
    }

    fn process_scancode(&mut self, scancode: u8) {
        // Handle extended prefix
        if scancode == 0xE0 {
            self.extended = true;
            return;
        }

        let extended = self.extended;
        self.extended = false;

        let is_release = (scancode & 0x80) != 0;
        let code = scancode & 0x7F;

        // Handle modifier keys
        match code {
            0x2A | 0x36 => {
                // Left/Right Shift
                self.shift = !is_release;
                return;
            }
            0x1D => {
                // Ctrl
                self.ctrl = !is_release;
                return;
            }
            0x38 => {
                // Alt
                self.alt = !is_release;
                return;
            }
            0x3A => {
                // Caps Lock (toggle on press only)
                if !is_release {
                    self.caps_lock = !self.caps_lock;
                }
                return;
            }
            _ => {}
        }

        // Translate to ASCII
        let ascii = if is_release {
            0
        } else {
            translate_scancode(code, self.shift, self.caps_lock, extended)
        };

        let event = KeyEvent {
            scancode,
            ascii,
            pressed: !is_release,
            shift: self.shift,
            ctrl: self.ctrl,
            alt: self.alt,
        };

        self.push(event);
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
        0x0E => 0x08, // Backspace
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

fn keyboard_poll() -> Option<u8> {
    let result = unsafe { syscall0(SYS_KEYBOARD_POLL) };
    if result == EWOULDBLOCK {
        None
    } else {
        Some(result as u8)
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

static mut KEYBOARD: KeyboardState = KeyboardState::new();

// ============================================================================
// Public API
// ============================================================================

/// Get the next key event from the buffer
pub fn get_key_event() -> Option<KeyEvent> {
    unsafe { KEYBOARD.pop() }
}

// ============================================================================
// Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    debug_log("Keyboard Driver: Starting PS/2 keyboard driver");

    // Main driver loop
    loop {
        // Poll for keyboard scancodes from kernel
        while let Some(scancode) = keyboard_poll() {
            unsafe {
                KEYBOARD.process_scancode(scancode);
            }
        }

        // Check for events to process
        while let Some(event) = unsafe { KEYBOARD.pop() } {
            if event.pressed && event.ascii != 0 {
                // In a full implementation, send via IPC to interested processes
                // For now, just log significant keypresses
                if event.ascii == 0x1B {
                    debug_log("Keyboard: Escape pressed");
                }
            }
        }

        thread_yield();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    debug_log("Keyboard Driver: PANIC!");
    thread_exit(0xFF)
}
