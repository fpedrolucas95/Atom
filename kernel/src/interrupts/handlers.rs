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

use crate::arch::halt;
use crate::ipc;
use crate::input;
use crate::mm;
use crate::sched;
#[allow(unused_imports)]
use crate::util::UI_DIRTY;
use crate::{log_debug, log_info, log_panic, log_warn};
use core::sync::atomic::AtomicBool;
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
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9:  u64,
    pub r8:  u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    pub exception_number: u64,
    pub error_code: u64,

    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
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

    // Check if exception came from user space (CPL=3)
    let from_userspace = (frame.cs & 0x3) == 0x3;

    log_panic!(
        LOG_ORIGIN,
        "CPU exception: {} (vector={}, from_userspace={})",
        EXCEPTION_NAMES[exception_number as usize],
        exception_number,
        from_userspace
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

            // If from userspace, kill the thread instead of halting the system
            if from_userspace {
                if let Some(tid) = sched::current_thread() {
                    log_panic!(
                        LOG_ORIGIN,
                        "User-space page fault - terminating thread {}",
                        tid
                    );

                    // Attempt notification first
                    let _ = mm::policy::notify_page_fault(tid, cr2, error_code, frame.rip);

                    // Terminate the faulting thread
                    crate::thread::terminate_entity(
                        tid,
                        crate::thread::TerminationReason::PageFault {
                            address: cr2,
                            error_code,
                            rip: frame.rip,
                        }
                    );

                    // Switch to next thread
                    let (_, next) = sched::on_timer_tick();
                    if let Some(next_id) = next {
                        log_info!(LOG_ORIGIN, "Switching to thread {}", next_id);
                        crate::sched::perform_context_switch(tid, next_id);
                    }

                    log_panic!(LOG_ORIGIN, "No threads available after killing faulting thread");
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

            // If from userspace, kill the thread instead of halting the system
            if from_userspace {
                if let Some(tid) = sched::current_thread() {
                    log_panic!(
                        LOG_ORIGIN,
                        "User-space general protection fault - terminating thread {}",
                        tid
                    );

                    // Terminate the faulting thread
                    crate::thread::terminate_entity(
                        tid,
                        crate::thread::TerminationReason::GeneralProtectionFault {
                            error_code,
                            rip: frame.rip,
                        }
                    );

                    // Switch to next thread
                    let (_, next) = sched::on_timer_tick();
                    if let Some(next_id) = next {
                        log_info!(LOG_ORIGIN, "Switching to thread {}", next_id);
                        crate::sched::perform_context_switch(tid, next_id);
                    }

                    log_panic!(LOG_ORIGIN, "No threads available after killing faulting thread");
                }
            } else {
                // Kernel GP fault - dump stack and halt
                log_panic!(LOG_ORIGIN, "Dumping kernel stack from RSP={:#016X}:", frame.rsp);
                unsafe {
                    let stack_ptr = frame.rsp as *const u64;
                    for i in 0..10 {
                        let addr = stack_ptr.offset(i as isize);
                        if let Some(val) = (addr as *const u64).as_ref() {
                            log_panic!(
                                LOG_ORIGIN,
                                "  [RSP+{:#04X}] = {:#016X}{}",
                                i * 8,
                                *val,
                                match i {
                                    0 => " (should be RIP if this is IRET frame)",
                                    1 => " (should be CS=0x1B if IRET frame)",
                                    2 => " (should be RFLAGS if IRET frame)",
                                    3 => " (should be user RSP if IRET frame)",
                                    4 => " (should be SS=0x23 if IRET frame)",
                                    _ => ""
                                }
                            );
                        }
                    }
                }
            }
        }

        _ => {
            // For other exceptions, kill userspace threads but halt on kernel exceptions
            if from_userspace {
                if let Some(tid) = sched::current_thread() {
                    log_panic!(
                        LOG_ORIGIN,
                        "User-space exception - terminating thread {}",
                        tid
                    );

                    // Terminate the faulting thread
                    crate::thread::terminate_entity(
                        tid,
                        crate::thread::TerminationReason::Exception {
                            vector: exception_number,
                            error_code,
                            rip: frame.rip,
                        }
                    );

                    // Switch to next thread
                    let (_, next) = sched::on_timer_tick();
                    if let Some(next_id) = next {
                        log_info!(LOG_ORIGIN, "Switching to thread {}", next_id);
                        crate::sched::perform_context_switch(tid, next_id);
                    }

                    log_panic!(LOG_ORIGIN, "No threads available after killing faulting thread");
                }
            }
        }
    }

    log_panic!(
        LOG_ORIGIN,
        "System halted due to fatal exception in kernel space"
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
    // Legacy handler for boot/early phase
    unsafe {
        TICKS += 1;
    }
    ipc::on_timer_tick(get_ticks());
    super::apic::send_eoi();
}

#[no_mangle]
pub extern "C" fn rust_timer_handler(frame: *const InterruptFrame) {
    unsafe {
        TICKS += 1;
    }

    ipc::on_timer_tick(get_ticks());

    // Check if coming from user mode (CPL=3)
    let from_userspace = unsafe { ((*frame).cs & 0x3) == 0x3 };

    if from_userspace {
        // Preemptible context - signal EOI and try to switch
        super::apic::send_eoi();
        sched::drive_preemptive_tick(frame);
    } else {
        // Kernel context - do NOT preempt kernel for now to maintain stability
        super::apic::send_eoi();
    }
}

pub extern "x86-interrupt" fn keyboard_interrupt_handler(_frame: &mut InterruptStackFrame) {

    // Buffer raw keyboard data for userspace driver
    input::on_keyboard_irq();

    // Notify userspace handler if registered
    if crate::syscall::has_userspace_irq_handler(1) {
        crate::syscall::notify_irq_handler(1);
    }

    super::apic::send_eoi();
}

pub extern "x86-interrupt" fn mouse_interrupt_handler(_frame: &mut InterruptStackFrame) {

    // Buffer raw mouse data for userspace driver
    input::on_mouse_irq();

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