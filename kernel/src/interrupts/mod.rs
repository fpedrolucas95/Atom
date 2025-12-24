// Interrupt Subsystem Orchestration
//
// Acts as the top-level coordination module for the kernel interrupt system.
// This module ties together IDT setup, interrupt controller initialization,
// and runtime interrupt control behind a simple, coherent API.
//
// Key responsibilities:
// - Initialize all interrupt-related subsystems in the correct order
// - Expose a stable, high-level interface to the rest of the kernel
// - Forward architecture-specific operations to the APIC/PIC layer
// - Centralize access to global interrupt state and diagnostics
//
// Initialization flow:
// - `init()` installs the IDT first, ensuring exception safety
// - Then initializes the interrupt controller (APIC or PIC fallback)
// - Logs progress to aid early-boot debugging
//
// Design principles:
// - Clear separation of concerns between IDT, handlers, and controller logic
// - Minimal logic: this module delegates rather than re-implements
// - Avoids leaking architecture-specific details to higher layers
//
// Runtime services:
// - `init_timer()` configures the system timer at a requested frequency
// - `enable()` / `disable()` globally toggle CPU interrupts
// - `get_ticks()` exposes the global timer tick counter
// - `timer_current_count()` provides low-level timer introspection
//
// Debug and verification support:
// - `debug_dump_state()` prints APIC/PIC register state
// - `verify_idt_mapping()` validates virtual memory mappings for the IDT
//
// Correctness and safety notes:
// - IDT must be initialized before enabling interrupts to avoid faults
// - Interrupts should remain disabled during early boot until `init()` completes
// - This module intentionally contains no `unsafe` code; all unsafe hardware
//   access is encapsulated in lower-level modules

pub mod idt;
pub mod handlers;
pub mod apic;

use crate::{log_info};

const LOG_ORIGIN: &str = "apic";

pub const TIMER_INTERRUPT_VECTOR: u8 = 32;
pub const KEYBOARD_INTERRUPT_VECTOR: u8 = 33;
pub const MOUSE_INTERRUPT_VECTOR: u8 = 44;
pub const USER_TRAP_INTERRUPT_VECTOR: u8 = 0x68;

pub fn init() {
    log_info!(LOG_ORIGIN, "Initializing interrupt system...");

    idt::init();
    apic::init();

    log_info!(LOG_ORIGIN, "Interrupt system initialized.");
}

pub fn init_timer(frequency_hz: u32) {
    apic::init_timer(frequency_hz);
}

pub fn enable() {
    apic::enable_interrupts();
}

#[allow(dead_code)]
pub fn disable() {
    apic::disable_interrupts();
}

pub fn get_ticks() -> u64 {
    handlers::get_ticks()
}

