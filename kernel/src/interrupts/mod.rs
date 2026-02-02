// Interrupt Subsystem Orchestration
//
// Acts as the top-level coordination module for the kernel interrupt system.
// This module ties together IDT setup, interrupt controller initialization,
// and runtime interrupt control behind a simple, coherent API.

#[cfg(target_arch = "x86_64")]
pub mod idt;
#[cfg(target_arch = "x86_64")]
pub mod handlers;
#[cfg(target_arch = "x86_64")]
pub mod apic;

#[cfg(target_arch = "aarch64")]
pub mod gic;
#[cfg(target_arch = "aarch64")]
pub mod handlers_aarch64;

use crate::{log_info};

const LOG_ORIGIN: &str = "apic";

pub const TIMER_INTERRUPT_VECTOR: u8 = 32;
pub const KEYBOARD_INTERRUPT_VECTOR: u8 = 33;
pub const MOUSE_INTERRUPT_VECTOR: u8 = 44;
pub const USER_TRAP_INTERRUPT_VECTOR: u8 = 0x68;

pub fn init() {
    log_info!(LOG_ORIGIN, "Initializing interrupt system...");

    #[cfg(target_arch = "x86_64")]
    {
        idt::init();
        apic::init();
    }

    #[cfg(target_arch = "aarch64")]
    {
        gic::init();
    }

    log_info!(LOG_ORIGIN, "Interrupt system initialized.");
}

pub fn init_timer(frequency_hz: u32) {
    #[cfg(target_arch = "x86_64")]
    apic::init_timer(frequency_hz);

    #[cfg(target_arch = "aarch64")]
    {
        let _ = frequency_hz;
    }
}

pub fn enable() {
    #[cfg(target_arch = "x86_64")]
    apic::enable_interrupts();

    #[cfg(target_arch = "aarch64")]
    crate::arch::irq_enable();
}

#[allow(dead_code)]
pub fn disable() {
    #[cfg(target_arch = "x86_64")]
    apic::disable_interrupts();

    #[cfg(target_arch = "aarch64")]
    crate::arch::irq_disable();
}

pub fn get_ticks() -> u64 {
    #[cfg(target_arch = "x86_64")]
    return handlers::get_ticks();

    #[cfg(target_arch = "aarch64")]
    handlers_aarch64::get_ticks()
}
