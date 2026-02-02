// Serial Port Driver (Kernel Debug I/O)
//
// Implements a minimal serial port driver for kernel debugging output.

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
        #[cfg(target_arch = "x86_64")]
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
        #[cfg(target_arch = "x86_64")]
        unsafe { inb(self.base + 5) & 0x20 != 0 }

        #[cfg(not(target_arch = "x86_64"))]
        true
    }

    pub fn write_byte(&self, byte: u8) {
        while !self.is_transmit_empty() {
            core::hint::spin_loop();
        }

        #[cfg(target_arch = "x86_64")]
        unsafe {
            outb(self.base, byte);
        }

        #[cfg(not(target_arch = "x86_64"))]
        let _ = byte;
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

    #[cfg(not(target_arch = "x86_64"))]
    let _ = (port, value);
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    #[cfg(target_arch = "x86_64")]
    {
        let ret: u8;
        core::arch::asm!(
            "in al, dx",
            out("al") ret,
            in("dx") port,
            options(nomem, nostack, preserves_flags)
        );
        ret
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = port;
        0
    }
}

pub fn init() {
    SERIAL1.lock().init();
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;

    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!("cli", options(nomem, nostack));
    }

    {
        SERIAL1.lock().write_fmt(args).unwrap();
    }

    #[cfg(target_arch = "x86_64")]
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
