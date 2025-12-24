// x86_64 Architecture Primitives
//
// Provides low-level, architecture-specific CPU and I/O primitives for
// the x86_64 platform. This module exposes a minimal set of unsafe helpers
// required by the kernel to interact directly with the processor and
// legacy hardware interfaces.
//
// Key responsibilities:
// - Provide CPU control primitives (halt, interrupt enable/disable)
// - Expose access to architectural state (RFLAGS)
// - Offer memory ordering and spin-wait helpers
// - Implement raw port I/O for legacy devices and debugging
// - Centralize x86_64 inline assembly behind a clean Rust API
//
// Design principles:
// - Extremely thin wrappers over hardware instructions
// - `#[inline(always)]` to ensure zero-overhead abstractions
// - No allocation, no global state, no policy logic
// - Unsafe operations are explicit and tightly scoped
//
// Implementation details:
// - `halt()` issues `hlt` to stop the CPU until the next interrupt
// - `irq_disable()` / `irq_enable()` wrap `cli` / `sti`
// - `rflags()` snapshots the current RFLAGS register via push/pop
// - `mfence()` provides a full memory fence for ordering guarantees
// - `inb`, `outb`, `outl` perform raw port-mapped I/O
// - `cpu_relax()` emits `pause` for efficient spin loops
//
// Debugging support:
// - `qemu_debugcon_putc()` writes to port 0xE9, enabling instant debug
//   output in QEMU without relying on serial or VGA
//
// Correctness and safety notes:
// - All functions using inline assembly are marked `unsafe` where required
// - `nomem`/`nostack`/`preserves_flags` options document side-effect guarantees
// - Intended for use only by trusted kernel code
//
// Submodules:
// - `uefi`: x86_64-specific UEFI helpers and CPU discovery routines
//
// This module forms the lowest abstraction layer of the kernel on x86_64
// and must remain small, predictable, and mechanically correct.

#![no_std]

use core::arch::asm;

pub mod uefi;

#[inline(always)]
pub fn halt() {
    unsafe {
        asm!("hlt", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn irq_disable() {
    unsafe {
        asm!("cli", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn irq_enable() {
    unsafe {
        asm!("sti", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub fn rflags() -> u64 {
    let r: u64;
    unsafe {
        asm!(
            "pushfq",
            "pop {}",
            out(reg) r,
            options(nomem, preserves_flags)
        );
    }
    r
}

#[inline(always)]
pub fn mfence() {
    unsafe {
        asm!("mfence", options(nomem, nostack, preserves_flags));
    }
}

#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags));
    value
}

#[inline(always)]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub unsafe fn outl(port: u16, value: u32) {
    asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags));
}

#[inline(always)]
pub fn qemu_debugcon_putc(byte: u8) {
    unsafe { outb(0xE9, byte) }
}

#[inline(always)]
pub fn cpu_relax() {
    unsafe {
        asm!("pause", options(nomem, nostack, preserves_flags));
    }
}

pub mod uefi;