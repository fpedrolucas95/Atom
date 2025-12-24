// Serial Port Driver (Kernel Debug I/O)
//
// Implements a minimal serial port driver for kernel debugging output.
// This module provides low-level access to the legacy COM1 UART (0x3F8)
// and serves as the primary, most reliable logging backend during
// early boot and kernel bring-up.
//
// Key responsibilities:
// - Initialize the COM1 serial port in a known-good configuration
// - Provide byte- and string-level output primitives
// - Integrate with Rust’s `fmt::Write` for formatted output
// - Expose safe macros for kernel-wide serial logging
//
// Design principles:
// - Simplicity and robustness over performance
// - Early-boot safe: no heap allocation, no dependencies on interrupts
// - Deterministic behavior suitable for emulators and real hardware
//
// Implementation details:
// - Direct port I/O via `in` / `out` instructions (x86_64 only)
// - UART is configured for:
//   - 38400 baud (divisor = 3)
//   - 8 data bits, no parity, 1 stop bit (8N1)
// - Transmit FIFO is polled (`is_transmit_empty`) before each write
// - Newlines are normalized to CRLF for terminal compatibility
//
// Concurrency and safety:
// - Global `SERIAL1` is protected by a spinlock
// - Interrupts are temporarily disabled during `_print` to avoid
//   interleaved output from interrupt contexts
// - All hardware access is tightly scoped in small `unsafe` blocks
//
// Logging integration:
// - `_print` is the low-level backend used by the logging subsystem
// - `serial_print!` and `serial_println!` macros provide ergonomic access
// - Serial output is considered the ground-truth log sink
//
// Limitations and future direction:
// - Output-only: no serial input support
// - Legacy UART only; no USB or modern debug transports
// - In the future, serial may become optional or be replaced by a
//   user-space debug/logging service
//
// This module is intentionally minimal and stable, forming the backbone
// of kernel diagnostics even when other subsystems are unavailable.

#![allow(dead_code)]

use core::fmt;

const COM1: u16 = 0x3F8;

pub struct SerialPort {
    base: u16,
}

impl SerialPort {
    pub const fn new(base: u16) -> Self {
        SerialPort { base }
    }

    pub fn init(&self) {
        unsafe {
            outb(self.base + 1, 0x00);
            outb(self.base + 3, 0x80);
            outb(self.base + 0, 0x03);
            outb(self.base + 1, 0x00);
            outb(self.base + 3, 0x03);
            outb(self.base + 2, 0xC7);
            outb(self.base + 4, 0x0B);
            outb(self.base + 4, 0x1E);
            outb(self.base + 0, 0xAE);

            if inb(self.base + 0) != 0xAE {
                return;
            }
            
            outb(self.base + 4, 0x0F);
        }
    }

    fn is_transmit_empty(&self) -> bool {
        unsafe { inb(self.base + 5) & 0x20 != 0 }
    }

    pub fn write_byte(&self, byte: u8) {
        while !self.is_transmit_empty() {
            core::hint::spin_loop();
        }

        unsafe {
            outb(self.base, byte);
        }
    }

    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        SerialPort::write_str(self, s);
        Ok(())
    }
}

pub static SERIAL1: spin::Mutex<SerialPort> = spin::Mutex::new(SerialPort::new(COM1));

#[inline]
unsafe fn outb(port: u16, value: u8) {
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") value,
        options(nomem, nostack, preserves_flags)
    );
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let ret: u8;
    #[cfg(target_arch = "x86_64")]
    core::arch::asm!(
        "in al, dx",
        out("al") ret,
        in("dx") port,
        options(nomem, nostack, preserves_flags)
    );
    ret
}

pub fn init() {
    SERIAL1.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    {
        SERIAL1.lock().write_fmt(args).unwrap();
    }

    unsafe {
        core::arch::asm!("sti", options(nomem, nostack));
    }
}

#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::serial::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($($arg:tt)*) => ($crate::serial_print!("{}\n", format_args!($($arg)*)));
}