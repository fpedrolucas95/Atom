// Kernel Utilities
//
// Provides common utility functions and primitives used across the kernel.
//
// Key features:
// - Interrupt-safe critical sections
// - Global flags for cross-subsystem signaling

use core::arch::asm;
use core::sync::atomic::{AtomicBool};

#[inline(always)]
pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    let rflags: u64;

    unsafe {
        asm!(
            "pushfq",
            "pop {}",
            out(reg) rflags,
            options(nomem, preserves_flags)
        );

        asm!("cli", options(nomem, nostack));
    }

    let result = f();

    unsafe {
        if (rflags & (1 << 9)) != 0 {
            asm!("sti", options(nomem, nostack));
        }
    }

    result
}
                            
pub static UI_DIRTY: AtomicBool = AtomicBool::new(false);
