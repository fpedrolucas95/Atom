// Canonical x86_64 Port I/O Primitives
//
// This is the single authoritative implementation of raw port-mapped I/O for
// x86_64. Both the kernel arch layer (kernel/src/arch/mod.rs) and the
// standalone x86_64 module (arch/x86_64/mod.rs) re-export from here so that
// the assembly encoding is maintained in exactly one place.

use core::arch::asm;

/// Read a byte (8-bit) from an x86 I/O port.
#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

/// Write a byte (8-bit) to an x86 I/O port.
#[inline(always)]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

/// Read a word (16-bit) from an x86 I/O port.
#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags));
    value
}

/// Write a word (16-bit) to an x86 I/O port.
#[inline(always)]
pub unsafe fn outw(port: u16, value: u16) {
    asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags));
}

/// Read a dword (32-bit) from an x86 I/O port.
#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags));
    value
}

/// Write a dword (32-bit) to an x86 I/O port.
#[inline(always)]
pub unsafe fn outl(port: u16, value: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
}
