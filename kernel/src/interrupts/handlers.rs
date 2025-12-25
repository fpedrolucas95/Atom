// Interrupt and Exception Handlers
//
// Centralizes the kernel’s interrupt/exception entry points and dispatch logic.
// Provides:
// - A Rust-side exception handler that prints full CPU state and halts
// - Periodic timer interrupt handling for scheduling and IPC timekeeping
// - Keyboard IRQ handling and a dummy vector handler for testing
//
// Key structures:
// - `InterruptStackFrame`: minimal frame matching x86-interrupt ABI expectations
//   (RIP/CS/RFLAGS/RSP/SS) for hardware-saved state.
// - `InterruptFrame`: full register snapshot layout matching the assembly
//   stub’s push order, including exception number and error code.
//
// Exception handling flow:
// - `rust_exception_handler(exception_number, error_code, stack_ptr)` receives
//   a raw pointer to the saved `InterruptFrame` and dumps registers to serial.
// - Uses `EXCEPTION_NAMES` for human-readable vector names; assumes the vector
//   is < 32 and indexes directly (important for correctness).
// - Special-cases common faults:
//   - Page Fault (#PF, vector 14): reads CR2 and decodes error-code bits
//   - General Protection Fault (#GP, vector 13): prints selector info if any
// - Ends by halting forever (`loop { halt(); }`), turning exceptions into a
//   fail-stop crash with a useful diagnostic printout.
//
// Timer handling:
// - `TICKS` is a global tick counter incremented on each timer interrupt.
// - Calls into `sched::on_timer_tick()` to drive preemption/time slicing.
// - Calls `ipc::on_timer_tick(get_ticks())` to advance IPC timeouts/timers.
// - Always signals EOI via `apic::send_eoi()` to re-arm the interrupt line.
//
// Keyboard handling:
// - `keyboard_interrupt_handler` delegates to `keyboard::handle_interrupt()`
//   and then signals EOI.
// - Keeping this short reduces time spent in IRQ context and avoids latency.
//
// Debug/testing hooks:
// - `dummy_interrupt_handler_0x69` provides a minimal handler for a specific
//   vector (useful to validate IDT wiring and EOI correctness).
// - `print_stack_trace` dumps 16 u64 words from a provided stack pointer,
//   intended as a lightweight post-mortem aid (best-effort, not symbolic).
//
// Safety and correctness notes:
// - `TICKS` is `static mut` and updated without atomics; safe only if interrupts
//   are the sole writer and reads tolerate races, or if called with interrupts
//   disabled when required.
// - `stack_ptr` is trusted as pointing to a valid `InterruptFrame`; mismatches
//   between the assembly stub layout and this struct will corrupt diagnostics.
// - `halt()` inside an infinite loop ensures the CPU stays quiescent after a
//   fatal exception, preventing further memory corruption.

use crate::arch::{gdt, halt};
use crate::ipc;
use crate::keyboard;
use crate::mouse;
use crate::mm;
use crate::sched;
#[allow(unused_imports)]
use crate::util::UI_DIRTY;
use crate::{log_debug, log_info, log_panic, log_warn};
use core::sync::atomic::{AtomicBool, Ordering};
use crate::interrupts::LOG_ORIGIN;

const EXCEPTION_NAMES: [&str; 32] = [
    "#DE - Divide Error",
    "#DB - Debug",
    "NMI - Non-Maskable Interrupt",
    "#BP - Breakpoint",
    "#OF - Overflow",
    "#BR - Bound Range Exceeded",
    "#UD - Invalid Opcode",
    "#NM - Device Not Available",
    "#DF - Double Fault",
    "Coprocessor Segment Overrun",
    "#TS - Invalid TSS",
    "#NP - Segment Not Present",
    "#SS - Stack-Segment Fault",
    "#GP - General Protection Fault",
    "#PF - Page Fault",
    "Reserved",
    "#MF - x87 FPU Floating-Point Error",
    "#AC - Alignment Check",
    "#MC - Machine Check",
    "#XM - SIMD Floating-Point Exception",
    "#VE - Virtualization Exception",
    "#CP - Control Protection Exception",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
];

#[repr(C)]
pub struct InterruptStackFrame {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

#[repr(C)]
pub struct InterruptFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9:  u64,
    r8:  u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,

    exception_number: u64,
    error_code: u64,

    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

const _: () = {
    let expected_size = 22 * size_of::<u64>();
    assert!(size_of::<InterruptFrame>() == expected_size);
};

#[no_mangle]
pub extern "C" fn rust_unexpected_interrupt_handler(
    vector: u64,
    stack_ptr: *const InterruptStackFrame,
) {
    #[cfg(debug_assertions)]
    {
        if vector > 255 {
            log_panic!(
                "interrupt",
                "ABI MISMATCH DETECTED: vector={:#X} (expected 0-255). Check assembly calling convention!",
                vector
            );
        }
    }

    if vector > 255 {
        super::apic::send_eoi();
        log_warn!(LOG_ORIGIN, "Invalid vector {} received (likely ABI bug)", vector);
        return;
    }

    let cpl = unsafe { (*stack_ptr).code_segment & 0x3 };

    if vector == 0xFF {
        super::apic::send_eoi();
        return;
    }

    log_warn!(
        LOG_ORIGIN,
        "Unexpected vector {} at RIP={:#X} (CPL={})",
        vector,
        unsafe { (*stack_ptr).instruction_pointer },
        cpl
    );

    super::apic::send_eoi();
}

#[no_mangle]
pub extern "C" fn rust_exception_handler(frame: *const InterruptFrame) {
    const LOG_ORIGIN: &str = "exception";

    let frame = unsafe { &*frame };
    let exception_number = frame.exception_number;
    let error_code = frame.error_code;

    if (exception_number as usize) >= EXCEPTION_NAMES.len() {
            log_panic!(
            LOG_ORIGIN,
            "Bad exception vector: {} (frame corruption)",
            exception_number
        );
            log_panic!(
            LOG_ORIGIN,
            "Raw frame: RIP={:#016X} CS={:#016X} RSP={:#016X} SS={:#016X}",
            frame.rip,
            frame.cs,
            frame.rsp,
            frame.ss
        );
            loop { halt(); }
        }
    
        log_panic!(
        LOG_ORIGIN,
        "CPU exception: {} (vector={})",
        EXCEPTION_NAMES[exception_number as usize],
        exception_number
    );

    log_panic!(LOG_ORIGIN, "Error code: {:#X}", error_code);

    log_debug!(
        LOG_ORIGIN,
        "Registers: RAX={:#016X} RBX={:#016X} RCX={:#016X} RDX={:#016X}",
        frame.rax, frame.rbx, frame.rcx, frame.rdx
    );
    log_debug!(
        LOG_ORIGIN,
        "Registers: RSI={:#016X} RDI={:#016X} RBP={:#016X} RSP={:#016X}",
        frame.rsi, frame.rdi, frame.rbp, frame.rsp
    );
    log_debug!(
        LOG_ORIGIN,
        "Registers: R8={:#016X} R9={:#016X} R10={:#016X} R11={:#016X}",
        frame.r8, frame.r9, frame.r10, frame.r11
    );
    log_debug!(
        LOG_ORIGIN,
        "Registers: R12={:#016X} R13={:#016X} R14={:#016X} R15={:#016X}",
        frame.r12, frame.r13, frame.r14, frame.r15
    );

    log_debug!(
        LOG_ORIGIN,
        "Execution state: RIP={:#016X} CS={:#04X} RFLAGS={:#016X} SS={:#04X}",
        frame.rip, frame.cs, frame.rflags, frame.ss
    );

    match exception_number {
        14 => {
            let cr2: u64;
            unsafe {
                core::arch::asm!(
                    "mov {}, cr2",
                    out(reg) cr2,
                    options(nomem, nostack, preserves_flags)
                );
            }

            log_panic!(
                LOG_ORIGIN,
                "Page Fault at address {:#016X}",
                cr2
            );

            log_debug!(
                LOG_ORIGIN,
                "PF flags: present={}, write={}, user={}, reserved={}, instr_fetch={}",
                error_code & 0x1 != 0,
                error_code & 0x2 != 0,
                error_code & 0x4 != 0,
                error_code & 0x8 != 0,
                error_code & 0x10 != 0
            );

            if error_code & 0x4 != 0 {
                if let Some(tid) = sched::current_thread() {
                    match mm::policy::notify_page_fault(tid, cr2, error_code, frame.rip) {
                        Ok(()) => log_debug!(
                            LOG_ORIGIN,
                            "Page fault notification delivered to user-space policy handler"
                        ),
                        Err(e) => log_warn!(
                            LOG_ORIGIN,
                            "Failed to notify user-space policy handler about page fault: {:?}",
                            e
                        ),
                    }
                } else {
                    log_warn!(
                        LOG_ORIGIN,
                        "Page fault from user space but no current thread; notification skipped"
                    );
                }
            }
        }

        13 => {
            log_panic!(
                LOG_ORIGIN,
                "General Protection Fault"
            );

            if error_code != 0 {
                log_debug!(
                    LOG_ORIGIN,
                    "Segment selector: {:#X}",
                    error_code
                );
            }
        }

        _ => {}
    }

    log_panic!(
        LOG_ORIGIN,
        "System halted due to fatal exception"
    );

    loop {
        halt();
    }
}

static mut TICKS: u64 = 0;
static USER_MODE_INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[allow(dead_code)]
static INTERRUPT_SWITCH_SKIP_LOGGED: AtomicBool = AtomicBool::new(false);

pub extern "x86-interrupt" fn timer_interrupt_handler(_frame: &mut InterruptStackFrame) {
    let coming_from_user = (_frame.code_segment & 0x3) == 0x3;

    if coming_from_user {
        let cs_valid = (_frame.code_segment as u16) == gdt::USER_CODE_SELECTOR;
        let ss_valid = (_frame.stack_segment as u16) == gdt::USER_DATA_SELECTOR;
        let rip_canonical = is_canonical(_frame.instruction_pointer);
        let rsp_canonical = is_canonical(_frame.stack_pointer);

        if !(cs_valid && ss_valid && rip_canonical && rsp_canonical) {
            log_warn!(
                "interrupt",
                "Timer frame sanity check failed: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X} canonical_rip={} canonical_rsp={} cs_ok={} ss_ok={}",
                _frame.instruction_pointer,
                _frame.stack_pointer,
                _frame.code_segment,
                _frame.stack_segment,
                rip_canonical,
                rsp_canonical,
                cs_valid,
                ss_valid
            );
        }
    }

    if coming_from_user
        && USER_MODE_INTERRUPTED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let cpl = _frame.code_segment & 0x3;
        log_info!(
            "interrupt",
            "Timer interrupted user context: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
            _frame.instruction_pointer,
            _frame.code_segment,
            _frame.stack_segment,
            cpl
        );
    }

    unsafe {
        TICKS += 1;
    }
    
    ipc::on_timer_tick(get_ticks());

    super::apic::send_eoi();
}

pub extern "x86-interrupt" fn keyboard_interrupt_handler(_frame: &mut InterruptStackFrame) {
    // Always process keyboard data in kernel to buffer it
    keyboard::handle_interrupt();

    // Notify userspace handler if registered
    if crate::syscall::has_userspace_irq_handler(1) {
        crate::syscall::notify_irq_handler(1);
    }

    super::apic::send_eoi();
}

pub extern "x86-interrupt" fn mouse_interrupt_handler(_frame: &mut InterruptStackFrame) {
    // Always process mouse data in kernel to buffer it
    mouse::handle_interrupt();

    // Notify userspace handler if registered
    if crate::syscall::has_userspace_irq_handler(12) {
        crate::syscall::notify_irq_handler(12);
    }

    super::apic::send_eoi();
}

pub extern "x86-interrupt" fn user_trap_interrupt_handler(
    frame: &mut InterruptStackFrame
) {
    let cpl = frame.code_segment & 0x3;

    log_info!(
        "interrupt",
        "User trap INT 0x68: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
        frame.instruction_pointer,
        frame.code_segment,
        frame.stack_segment,
        cpl
    );

    super::apic::send_eoi();
}

pub fn get_ticks() -> u64 {
    unsafe { TICKS }
}

#[allow(dead_code)]
pub fn print_stack_trace(stack_ptr: u64) {
    const LOG_ORIGIN: &str = "exception";

    log_debug!(
        LOG_ORIGIN,
        "Stack trace dump (starting at {:#016X})",
        stack_ptr
    );

    let stack = unsafe {
        core::slice::from_raw_parts(stack_ptr as *const u64, 16)
    };

    for (i, value) in stack.iter().enumerate() {
        log_debug!(
            LOG_ORIGIN,
            "Stack[{}] = {:#016X}",
            i,
            value
        );
    }
}

fn is_canonical(addr: u64) -> bool {
    let sign_extension = addr >> 48;
    sign_extension == 0 || sign_extension == 0xFFFF
}