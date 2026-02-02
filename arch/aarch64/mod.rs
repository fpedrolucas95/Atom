// AArch64 Architecture Primitives
//
// Provides low-level, architecture-specific CPU primitives for the AArch64 platform.

#![no_std]

use core::arch::asm;

pub mod uefi;

#[inline(always)]
pub fn halt() {
    unsafe {
        asm!("wfi", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn irq_disable() {
    unsafe {
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn irq_enable() {
    unsafe {
        asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn rflags() -> u64 {
    let r: u64;
    unsafe {
        asm!("mrs {}, daif", out(reg) r, options(nomem, preserves_flags));
    }
    r
}

#[inline(always)]
pub fn mfence() {
    unsafe {
        asm!("dmb sy", options(nomem, nostack, preserves_flags));
    }
}

// AArch64 does not have port I/O. These are stubs or use MMIO if needed.
#[inline(always)]
pub unsafe fn inb(_port: u16) -> u8 {
    0
}

#[inline(always)]
pub unsafe fn outb(_port: u16, _value: u8) {
    // MMIO could be used here for specific platforms
}

#[inline(always)]
pub unsafe fn outl(_port: u16, _value: u32) {
}

#[inline(always)]
pub fn qemu_debugcon_putc(byte: u8) {
    // On AArch64 QEMU, we might want to write to a serial port (e.g., PL011)
    // For now, this is a stub.
}

#[inline(always)]
pub fn cpu_relax() {
    unsafe {
        asm!("yield", options(nomem, nostack, preserves_flags));
    }
}
