// Interrupt and Exception Handlers
//
// Centralizes the kernel’s interrupt/exception entry points and dispatch logic.
// Provides:
// - A Rust-side exception handler that prints full CPU state and halts
// - Periodic timer interrupt handling for scheduling and IPC timekeeping
// - Keyboard IRQ handling and a dummy vector handler for testing
//
// Key structures:
// - `InterruptStackFrame`: minimal 5-field view of the hardware-saved state
//   (RIP/CS/RFLAGS/RSP/SS); kept for documentation purposes but the active
//   interrupt pipeline uses `InterruptFrame` exclusively.
// - `InterruptFrame`: full register snapshot layout matching the assembly
//   stub’s push order, including exception number and error code.
//
// Exception handling flow:
// - Each exception stub in `handlers.asm` pushes (vector, error_code) and all
//   GPRs via `PUSH_ALL`, then calls `rust_exception_handler(frame)` with a
//   single pointer to the resulting `InterruptFrame` on the stack.
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
// - The low-level timer ISR is `rust_timer_interrupt_handler`.
// - It wakes sleeping threads and advances IPC timers, but does not context
//   switch directly from the timer ISR. Context switches happen at cooperative
//   yield/syscall/idle boundaries where `thread::perform_context_switch` can
//   save a normal call frame instead of an interrupt frame.
// - It calls `ipc::on_timer_tick(get_ticks())` to advance IPC timeouts/timers.
// - It always signals EOI via `apic::send_eoi()` to re-arm the interrupt line.
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
// - `TICKS` is atomic and incremented from timer IRQs on all CPUs.
// - `stack_ptr` is trusted as pointing to a valid `InterruptFrame`; mismatches
//   between the assembly stub layout and this struct will corrupt diagnostics.
// - `halt()` inside an infinite loop ensures the CPU stays quiescent after a
//   fatal exception, preventing further memory corruption.

use crate::arch::{gdt, halt};
use crate::ipc;
use crate::input;
use crate::mm;
use crate::sched;
#[allow(unused_imports)]
use crate::util::UI_DIRTY;
use crate::{log_debug, log_error, log_info, log_panic, log_warn};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::interrupts::LOG_ORIGIN;

// ---------------------------------------------------------------------------
// Fault loop detector
//
// Tracks how many consecutive page faults the same (pid, page) pair has
// generated without being resolved. If a single page faults more than
// FAULT_LOOP_THRESHOLD times in a row, the process is killed to prevent
// a fault storm from freezing the entire system.
//
// The counter is stored as a per-CPU approximation using a small fixed-size
// table keyed by (pid ^ page_addr) % FAULT_LOOP_SLOTS. Hash collisions are
// acceptable — a false positive kills a process unnecessarily (rare), a false
// negative allows a few extra faults before detection (bounded).
// ---------------------------------------------------------------------------

const FAULT_LOOP_SLOTS: usize = 64;
const FAULT_LOOP_THRESHOLD: u8 = 8;

#[derive(Clone, Copy)]
struct FaultSlot {
    pid_raw: u64,
    page_addr: usize,
    count: u8,
}

impl FaultSlot {
    const EMPTY: Self = Self { pid_raw: 0, page_addr: 0, count: 0 };
}

static FAULT_LOOP_TABLE: spin::Mutex<[FaultSlot; FAULT_LOOP_SLOTS]> =
    spin::Mutex::new([FaultSlot::EMPTY; FAULT_LOOP_SLOTS]);

/// Record a fault for (pid, page_addr). Returns true if the fault loop
/// threshold has been exceeded and the process should be killed.
fn record_fault_and_check_loop(
    pid: crate::process::ProcessId,
    page_addr: usize,
) -> bool {
    let slot_idx = ((pid.raw() ^ page_addr as u64) as usize) % FAULT_LOOP_SLOTS;
    let mut table = FAULT_LOOP_TABLE.lock();
    let slot = &mut table[slot_idx];

    if slot.pid_raw == pid.raw() && slot.page_addr == page_addr {
        slot.count = slot.count.saturating_add(1);
        slot.count >= FAULT_LOOP_THRESHOLD
    } else {
        // New (pid, page) pair — reset slot
        *slot = FaultSlot { pid_raw: pid.raw(), page_addr, count: 1 };
        false
    }
}

/// Clear the fault loop counter for a (pid, page) pair after successful resolution.
fn clear_fault_loop_counter(pid: crate::process::ProcessId, page_addr: usize) {
    let slot_idx = ((pid.raw() ^ page_addr as u64) as usize) % FAULT_LOOP_SLOTS;
    let mut table = FAULT_LOOP_TABLE.lock();
    let slot = &mut table[slot_idx];
    if slot.pid_raw == pid.raw() && slot.page_addr == page_addr {
        *slot = FaultSlot::EMPTY;
    }
}

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

// Kept for documentation: mirrors the five hardware-pushed words that the CPU
// saves on the stack when delivering an interrupt.  The active pipeline uses
// `InterruptFrame` (which embeds these same fields at its tail), so this
// struct is never instantiated in code.
#[allow(dead_code)]
#[repr(C)]
pub struct InterruptStackFrame {
    pub instruction_pointer: u64,
    pub code_segment: u64,
    pub cpu_flags: u64,
    pub stack_pointer: u64,
    pub stack_segment: u64,
}

/// Represents the full CPU state saved during an interrupt or exception.
/// This structure must exactly match the stack layout created by the assembly
/// stubs in `handlers.asm` to ensure correct ABI interoperability.
///
/// Memory layout (from low to high addresses):
#[repr(C)]
pub struct InterruptFrame {
    /* Pushed by PUSH_ALL macro in handlers.asm */
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

    /* Pushed by the assembly stub */
    pub exception_number: u64, // The interrupt vector (e.g., 0x0E for Page Fault)
    pub error_code: u64,       // Exception-specific error code (or 0 dummy)

    /* Pushed by the CPU hardware on interrupt/exception */
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    /// The stack pointer at the time of the interrupt.
    /// Note: In x86_64 long mode, RSP is always pushed by the hardware,
    /// ensuring a consistent stack frame layout regardless of privilege changes.
    pub rsp: u64,
    /// The stack segment selector at the time of the interrupt.
    /// Note: In x86_64 long mode, SS is always pushed by the hardware.
    pub ss: u64,
}

/// Compile-time validation of the `InterruptFrame` layout.
/// Ensures that the struct size and field offsets correspond exactly to the
/// expected x86_64 architectural and assembly-defined stack frames.
const _: () = {
    use core::mem::{size_of, offset_of};

    // INVARIANT: InterruptFrame must be exactly 176 bytes (22 × 8) — structural, not operational.
    assert!(
        size_of::<InterruptFrame>() == 22 * 8,
        "InterruptFrame size mismatch: expected exactly 176 bytes (22 * 8)"
    );

    // Offset checks for each architectural block
    // 1. Registers pushed by PUSH_ALL (RSP points here initially)
    // INVARIANT: r15 must be at offset 0 (first register pushed by PUSH_ALL) — structural, not operational.
    assert!(offset_of!(InterruptFrame, r15) == 0);
    // INVARIANT: rax must be at offset 14*8 (last register pushed by PUSH_ALL) — structural, not operational.
    assert!(offset_of!(InterruptFrame, rax) == 14 * 8);

    // 2. Information pushed by the assembly stubs
    // INVARIANT: exception_number must be at offset 15*8 (pushed by assembly stubs) — structural, not operational.
    assert!(offset_of!(InterruptFrame, exception_number) == 15 * 8);
    // INVARIANT: error_code must be at offset 16*8 (pushed by assembly stubs) — structural, not operational.
    assert!(offset_of!(InterruptFrame, error_code)       == 16 * 8);

    // 3. Hardware-saved state (IRET frame)
    // INVARIANT: rip must be at offset 17*8 (hardware IRET frame) — structural, not operational.
    assert!(offset_of!(InterruptFrame, rip)    == 17 * 8);
    // INVARIANT: cs must be at offset 18*8 (hardware IRET frame) — structural, not operational.
    assert!(offset_of!(InterruptFrame, cs)     == 18 * 8);
    // INVARIANT: rflags must be at offset 19*8 (hardware IRET frame) — structural, not operational.
    assert!(offset_of!(InterruptFrame, rflags) == 19 * 8);
    // INVARIANT: rsp must be at offset 20*8 (hardware IRET frame) — structural, not operational.
    assert!(offset_of!(InterruptFrame, rsp)    == 20 * 8);
    // INVARIANT: ss must be at offset 21*8 (hardware IRET frame) — structural, not operational.
    assert!(offset_of!(InterruptFrame, ss)     == 21 * 8);
};

#[no_mangle]
pub extern "C" fn rust_unexpected_interrupt_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    let vector = frame.exception_number;
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

    let cpl = frame.cs & 0x3;

    if vector == 0xFF {
        super::apic::send_eoi();
        return;
    }

    log_warn!(
        LOG_ORIGIN,
        "Unexpected vector {} at RIP={:#X} (CPL={})",
        vector,
        frame.rip,
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

    // -----------------------------------------------------------------------
    // Page fault handling policy:
    // - Faults are resolved by the VMA pipeline (classify -> admit -> materialize).
    // - Resolved faults return to the faulting instruction.
    // - Unresolvable userspace faults terminate the faulting thread/process.
    // - Kernel-mode faults on user VMAs can still resolve through the same pipeline.
    // -----------------------------------------------------------------------
    if exception_number == 14 {
        let cr2 = read_cr2();

        if from_userspace {
            if let Some(tid) = sched::current_thread() {
                if handle_userspace_page_fault(tid, frame, cr2, error_code) {
                    // Resolved — POP_ALL + iretq will retry the instruction.
                    return;
                }
            }
        }

        // Kernel-mode faults may still be legitimate copy-to-user / VMA
        // demand-paging activity. The resolver itself decides whether the
        // address maps to a valid VMA.
        if let Some(tid) = sched::current_thread() {
            if let Some(pml4) = crate::thread::get_thread_address_space(tid) {
                let pml4 = crate::arch::cr3_to_pml4_phys(pml4);
                if pml4 != 0 {
                    if let Some(pid) = crate::thread::get_thread_process_id(tid) {
                        let ctx = mm::vma::FaultContext::from_x86_error(
                            pid,
                            mm::vma::AddressSpaceId::new(pml4 as usize),
                            cr2 as usize,
                            error_code,
                        );
                        if matches!(
                            mm::vma::handle_page_fault(ctx, error_code),
                            mm::vma::FaultResult::Resolved
                        ) {
                            // Resolved — POP_ALL + iretq will retry the instruction.
                            return;
                        }
                    }
                }
            }
        }
        // Fall through: genuinely unresolvable — log and terminate.
    }

    log_panic!(
        LOG_ORIGIN,
        "CPU exception: {} (vector={}, from_userspace={})",
        EXCEPTION_NAMES[exception_number as usize],
        exception_number,
        from_userspace
    );

    log_panic!(LOG_ORIGIN, "Error code: {:#X}", error_code);

    log_panic!(
        LOG_ORIGIN,
        "RIP={:#018X}  CS={:#06X}  RFLAGS={:#018X}  RSP={:#018X}  SS={:#06X}",
        frame.rip, frame.cs, frame.rflags, frame.rsp, frame.ss
    );
    log_panic!(
        LOG_ORIGIN,
        "RAX={:#018X}  RBX={:#018X}  RCX={:#018X}  RDX={:#018X}",
        frame.rax, frame.rbx, frame.rcx, frame.rdx
    );
    log_panic!(
        LOG_ORIGIN,
        "RSI={:#018X}  RDI={:#018X}  RBP={:#018X}",
        frame.rsi, frame.rdi, frame.rbp
    );
    log_panic!(
        LOG_ORIGIN,
        "R8={:#018X}   R9={:#018X}   R10={:#018X}  R11={:#018X}",
        frame.r8, frame.r9, frame.r10, frame.r11
    );
    log_panic!(
        LOG_ORIGIN,
        "R12={:#018X}  R13={:#018X}  R14={:#018X}  R15={:#018X}",
        frame.r12, frame.r13, frame.r14, frame.r15
    );

    match exception_number {
        14 => {
            let cr2 = read_cr2();

            let cr3: u64 = unsafe {
                let v: u64;
                core::arch::asm!("mov {0}, cr3", out(reg) v, options(nomem, nostack, preserves_flags));
                v
            };

            let pf_present  = error_code & 0x1  != 0;
            let pf_write    = error_code & 0x2  != 0;
            let pf_user     = error_code & 0x4  != 0;
            let pf_reserved = error_code & 0x8  != 0;
            let pf_ifetch   = error_code & 0x10 != 0;

            // ---------------------------------------------------------------
            // Unresolvable fault — demand paging was already attempted in
            // the fast path above; if we reach here the fault is genuine.
            // ---------------------------------------------------------------
            log_panic!(
                LOG_ORIGIN,
                "Page Fault at address {:#016X}",
                cr2
            );

            log_panic!(
                LOG_ORIGIN,
                "Faulting instruction RIP={:#016X} RSP={:#016X}",
                frame.rip,
                frame.rsp
            );

            // Decode error code for rapid diagnosis.
            log_panic!(
                LOG_ORIGIN,
                "PF error_code={:#X}: present={} write={} user={} reserved={} ifetch={}",
                error_code,
                pf_present,
                pf_write,
                pf_user,
                pf_reserved,
                pf_ifetch
            );

            // CR3 identifies which address space faulted.
            let tid_opt = sched::current_thread();
            log_panic!(
                LOG_ORIGIN,
                "CR2={:#016X}  CR3={:#016X}  TID={:?}  from_userspace={}",
                cr2,
                cr3,
                tid_opt,
                from_userspace
            );

            // ---------------------------------------------------------------
            // NULL FUNCTION POINTER CALL DETECTION
            //
            // When a userspace thread executes `CALL RAX` (or any indirect
            // call/jump) with RAX==0, the CPU pushes the return address onto
            // the user stack and then attempts to fetch the instruction at
            // VA 0x0.  With the null-page guard active (VA 0 unmapped), this
            // produces:
            //   CR2 = 0x0  (faulting address)
            //   RIP = 0x0  (instruction pointer == faulting address for ifetch)
            //   error_code bit 4 (I/D) = 1  (instruction fetch)
            //   error_code bit 2 (U/S) = 1  (CPL=3, userspace)
            //   error_code bit 0 (P)   = 0  (page not present)
            //
            // The return address at [RSP] tells us which CALL instruction
            // triggered the null pointer call.  We safely translate RSP
            // through the process page tables to read it from kernel space,
            // avoiding any risk of double fault from directly dereferencing
            // an unmapped user address.
            //
            // With the null page mapped (pre-guard-page patch), the same
            // scenario produces P=1 and I/D=1 instead (NX violation or
            // supervisor-only page execution attempt).
            // ---------------------------------------------------------------
            let is_null_call = cr2 == 0
                && frame.rip == 0
                && pf_ifetch
                && pf_user
                && from_userspace;

            // Also detect the pre-guard variant (null page still present)
            let is_null_call_present = cr2 == 0
                && frame.rip == 0
                && pf_ifetch
                && pf_present
                && pf_user
                && from_userspace;

            if is_null_call || is_null_call_present {
                log_panic!(
                    LOG_ORIGIN,
                    "=== NULL FUNCTION POINTER CALL DETECTED (userspace) ==="
                );

                if is_null_call_present {
                    log_panic!(
                        LOG_ORIGIN,
                        "DIAGNOSIS: null page is PRESENT in CR3={:#X} — \
                         ensure clone_kernel_mappings applies null-page guard \
                         (unmap_page_in_pml4(dst_pml4, 0)).",
                        cr3
                    );
                }

                // Log all GPRs for maximum diagnostic value. Any register
                // could have been the source of the null indirect call.
                log_panic!(
                    LOG_ORIGIN,
                    "GPRs: RAX={:#018X} RBX={:#018X} RCX={:#018X} RDX={:#018X}",
                    frame.rax, frame.rbx, frame.rcx, frame.rdx
                );
                log_panic!(
                    LOG_ORIGIN,
                    "GPRs: RSI={:#018X} RDI={:#018X} RBP={:#018X} RSP={:#018X}",
                    frame.rsi, frame.rdi, frame.rbp, frame.rsp
                );
                log_panic!(
                    LOG_ORIGIN,
                    "GPRs: R8={:#018X}  R9={:#018X}  R10={:#018X} R11={:#018X}",
                    frame.r8, frame.r9, frame.r10, frame.r11
                );
                log_panic!(
                    LOG_ORIGIN,
                    "GPRs: R12={:#018X} R13={:#018X} R14={:#018X} R15={:#018X}",
                    frame.r12, frame.r13, frame.r14, frame.r15
                );

                // Extract return address from user stack.
                // The CALL instruction pushed the return address at [RSP]
                // before jumping to address 0.  We walk the faulting
                // process's page tables (CR3) to translate the user RSP
                // to a physical address, then access it via the kernel's
                // higher-half identity map — no risk of double fault.
                let user_rsp = frame.rsp;

                // Validate only alignment; mapping validity is decided by VMA/PTE lookup.
                let rsp_aligned = (user_rsp & 0x7) == 0;

                if rsp_aligned {
                    // Walk the page tables to translate user RSP → physical
                    let rsp_page = (user_rsp & !0xFFF) as usize;
                    let rsp_offset = (user_rsp & 0xFFF) as usize;

                    // Ensure the 8-byte read doesn't cross a page boundary
                    let crosses_page = rsp_offset > (0x1000 - 8);

                    if !crosses_page {
                        // Use the process's own PML4 (from CR3) to translate
                        if let Some(pte_raw) = mm::vm::read_pte_in_pml4(cr3 as usize, rsp_page) {
                            let pte_present = pte_raw & 0x1 != 0;
                            if pte_present {
                                let phys_base = (pte_raw & 0x000F_FFFF_FFFF_F000) as usize;
                                let phys_addr = phys_base + rsp_offset;

                                // Access via kernel's higher-half mapping
                                let virt_addr = mm::vm::phys_to_virt_ptr(phys_addr);
                                let ret_addr = unsafe { *(virt_addr as *const u64) };

                                log_panic!(
                                    LOG_ORIGIN,
                                    "NULL CALL return address: {:#018X}  (from user RSP={:#X})",
                                    ret_addr,
                                    user_rsp
                                );
                                log_panic!(
                                    LOG_ORIGIN,
                                    ">>> Run: addr2line -e doom.elf {:#X}  (or doom.atxf)",
                                    ret_addr
                                );

                                // Dump a few more stack words for call-chain context
                                // (up to 8 words, staying within the same page)
                                let remaining_in_page = 0x1000 - rsp_offset;
                                let max_words = core::cmp::min(remaining_in_page / 8, 8);
                                if max_words > 1 {
                                    log_panic!(LOG_ORIGIN, "User stack dump (top {} words):", max_words);
                                    for i in 0..max_words {
                                        let word_offset = rsp_offset + i * 8;
                                        let word_virt = mm::vm::phys_to_virt_ptr(phys_base + word_offset);
                                        let word_val = unsafe { *(word_virt as *const u64) };
                                        log_panic!(
                                            LOG_ORIGIN,
                                            "  [RSP+{:#04X}] = {:#018X}{}",
                                            i * 8,
                                            word_val,
                                            if i == 0 { "  <-- return address (CALL site)" } else { "" }
                                        );
                                    }
                                }
                            } else {
                                log_panic!(
                                    LOG_ORIGIN,
                                    "Cannot read return address: RSP page {:#X} not present in PML4 {:#X}",
                                    rsp_page,
                                    cr3
                                );
                            }
                        } else {
                            log_panic!(
                                LOG_ORIGIN,
                                "Cannot read return address: page table walk failed for RSP={:#X} in PML4 {:#X}",
                                user_rsp,
                                cr3
                            );
                        }
                    } else {
                        log_panic!(
                            LOG_ORIGIN,
                            "Cannot read return address: RSP={:#X} crosses page boundary (offset={:#X})",
                            user_rsp,
                            rsp_offset
                        );
                    }
                } else {
                    log_panic!(
                        LOG_ORIGIN,
                        "Cannot read return address: RSP={:#X} invalid alignment (aligned={})",
                        user_rsp,
                        rsp_aligned
                    );
                }

                log_panic!(
                    LOG_ORIGIN,
                    "=== END NULL CALL DIAGNOSIS ==="
                );
            }

            // If from userspace, kill the thread instead of halting the system
            if from_userspace {
                if let Some(tid) = tid_opt {
                    log_panic!(
                        LOG_ORIGIN,
                        "User-space page fault (unresolvable) - terminating thread {}",
                        tid
                    );

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
                        if let Some(val) = addr.as_ref() {
                            log_panic!(
                                LOG_ORIGIN,
                                "  [RSP+{:#04X}] = {:#016X}{}",
                                i * 8,
                                *val,
                                match i {
                                    0 => " (should be RIP if this is IRET frame)",
                                    1 => " (should be CS=0x23 if IRET frame)",
                                    2 => " (should be RFLAGS if IRET frame)",
                                    3 => " (should be user RSP if IRET frame)",
                                    4 => " (should be SS=0x1B if IRET frame)",
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

static TICKS: AtomicU64 = AtomicU64::new(0);
static USER_MODE_INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[allow(dead_code)]
static INTERRUPT_SWITCH_SKIP_LOGGED: AtomicBool = AtomicBool::new(false);

#[inline]
fn read_cr2() -> u64 {
    let cr2: u64;
    unsafe {
        core::arch::asm!(
            "mov {}, cr2",
            out(reg) cr2,
            options(nomem, nostack, preserves_flags)
        );
    }
    cr2
}

fn terminate_faulting_userspace_thread(
    tid: crate::thread::ThreadId,
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
    fault_result: mm::vma::FaultResult,
) -> ! {
    let process_id = crate::thread::get_thread_process_id(tid);
    log_panic!(
        LOG_ORIGIN,
        "USER PAGE FAULT -> terminating PID {:?} TID {}",
        process_id,
        tid
    );
    log_panic!(
        LOG_ORIGIN,
        "Fault context: RIP={:#016X} RSP={:#016X} CR2={:#016X} ERR={:#X} RESULT={:?}",
        frame.rip,
        frame.rsp,
        cr2,
        error_code,
        fault_result
    );

    let reason = crate::thread::TerminationReason::PageFault {
        address: cr2,
        error_code,
        rip: frame.rip,
    };

    crate::thread::terminate_entity(tid, reason);

    let (_, next) = sched::on_timer_tick();
    if let Some(next_id) = next {
        log_info!(LOG_ORIGIN, "Switching to next thread {}", next_id);
        crate::sched::perform_context_switch(tid, next_id);
    }

    log_panic!(
        LOG_ORIGIN,
        "No runnable threads available after terminating faulting userspace thread {}",
        tid
    );

    loop {
        halt();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultOutcome {
    Resolved,
    Fatal(mm::vma::FaultResult),
}

fn classify_fault_outcome(result: mm::vma::FaultResult) -> FaultOutcome {
    match result {
        mm::vma::FaultResult::Resolved => FaultOutcome::Resolved,
        mm::vma::FaultResult::InvalidAddress
        | mm::vma::FaultResult::ProtectionViolation
        | mm::vma::FaultResult::OutOfMemory
        | mm::vma::FaultResult::NotHandled => FaultOutcome::Fatal(result),
    }
}

fn handle_userspace_page_fault(
    tid: crate::thread::ThreadId,
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
) -> bool {
    let current_cr3_raw = crate::arch::read_cr3();
    let current_cr3 = crate::arch::cr3_to_pml4_phys(current_cr3_raw);
    if current_cr3 == 0 {
        terminate_faulting_userspace_thread(
            tid,
            frame,
            cr2,
            error_code,
            mm::vma::FaultResult::InvalidAddress
        );
    }

    if current_cr3_raw != current_cr3 {
        log_debug!(
            LOG_ORIGIN,
            "[PF] cr3_sanitized raw={:#x} pml4={:#x}",
            current_cr3_raw,
            current_cr3
        );
    }

    let pid = match crate::thread::get_thread_process_id(tid) {
        Some(pid) => pid,
        None => {
            terminate_faulting_userspace_thread(
                tid,
                frame,
                cr2,
                error_code,
                mm::vma::FaultResult::InvalidAddress
            );
        }
    };

    // ABI boundary hardening: userspace fault addresses must satisfy the
    // canonical userspace address contract before entering the VMA pipeline.
    if let Err(err) = atom_abi::validate_user_addr(cr2 as usize) {
        log_warn!(
            LOG_ORIGIN,
            "[PF] reject_non_abi_user_fault pid={} cr3={:#x} addr={:#x} rip={:#x} err={:#x} abi_err={:?}",
            pid,
            current_cr3,
            cr2,
            frame.rip,
            error_code,
            err
        );
        terminate_faulting_userspace_thread(
            tid,
            frame,
            cr2,
            error_code,
            mm::vma::FaultResult::InvalidAddress,
        );
    }

    let ctx = mm::vma::FaultContext::from_x86_error(
        pid,
        mm::vma::AddressSpaceId::new(current_cr3 as usize),
        cr2 as usize,
        error_code,
    );

    let page_addr = (cr2 as usize) & !(crate::mm::pmm::PAGE_SIZE - 1);

    // Check for fault storm before attempting resolution.
    if record_fault_and_check_loop(pid, page_addr) {
        log_error!(
            LOG_ORIGIN,
            "[PF] FAULT_LOOP pid={} cr3={:#x} addr={:#x} page={:#x} rip={:#x} err={:#x} — killing process",
            pid,
            current_cr3,
            cr2,
            page_addr,
            frame.rip,
            error_code,
        );
        clear_fault_loop_counter(pid, page_addr);
        terminate_faulting_userspace_thread(
            tid,
            frame,
            cr2,
            error_code,
            mm::vma::FaultResult::InvalidAddress,
        );
    }

    let result = mm::vma::handle_page_fault(ctx, error_code);

    match classify_fault_outcome(result) {
        FaultOutcome::Resolved => {
            clear_fault_loop_counter(pid, page_addr);
            log_debug!(
                LOG_ORIGIN,
                "[PF] pid={} cr3={:#x} addr={:#x} rip={:#x} err={:#x} result={:?} action=resume",
                pid,
                current_cr3,
                cr2,
                frame.rip,
                error_code,
                result
            );
            true
        }
        FaultOutcome::Fatal(fatal_result) => {
            log_error!(
                LOG_ORIGIN,
                "[PF] FATAL pid={} cr3={:#x} addr={:#x} rip={:#x} err={:#x} result={:?} action=terminate",
                pid,
                current_cr3,
                cr2,
                frame.rip,
                error_code,
                fatal_result
            );
            terminate_faulting_userspace_thread(tid, frame, cr2, error_code, fatal_result);
        }
    }
}

#[no_mangle]
pub extern "C" fn rust_timer_interrupt_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    let coming_from_user = (frame.cs & 0x3) == 0x3;

    if coming_from_user {
        let cs_valid = (frame.cs as u16) == gdt::USER_CODE_SELECTOR;
        let ss_valid = (frame.ss as u16) == gdt::USER_DATA_SELECTOR;
        let rip_canonical = is_canonical(frame.rip);
        let rsp_canonical = is_canonical(frame.rsp);

        if !(cs_valid && ss_valid && rip_canonical && rsp_canonical) {
            log_warn!(
                "interrupt",
                "Timer frame sanity check failed: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X} canonical_rip={} canonical_rsp={} cs_ok={} ss_ok={}",
                frame.rip,
                frame.rsp,
                frame.cs,
                frame.ss,
                rip_canonical,
                rsp_canonical,
                cs_valid,
                ss_valid
            );
        }
    } else {
        // Same-privilege interrupt (Ring 0 -> Ring 0).
        // Sanity check kernel segments. SS can be 0 or KERNEL_DATA.
        let cs_valid = (frame.cs as u16) == gdt::KERNEL_CODE_SELECTOR;
        let ss_valid = (frame.ss as u16) == gdt::KERNEL_DATA_SELECTOR || frame.ss == 0;
        let rip_canonical = is_canonical(frame.rip);
        let rsp_canonical = is_canonical(frame.rsp);

        if !(cs_valid && ss_valid && rip_canonical && rsp_canonical) {
             log_warn!(
                "interrupt",
                "Timer frame kernel sanity check failed: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X}",
                frame.rip,
                frame.rsp,
                frame.cs,
                frame.ss
            );
        }
    }

    if coming_from_user
        && USER_MODE_INTERRUPTED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let cpl = frame.cs & 0x3;
        log_info!(
            "interrupt",
            "Timer interrupted user context: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
            frame.rip,
            frame.cs,
            frame.ss,
            cpl
        );
    }

    TICKS.fetch_add(1, Ordering::Relaxed);

    ipc::on_timer_tick(get_ticks());

    // Wake threads whose sleep deadline has been reached.
    sched::wake_sleeping_threads();

    // CRITICAL: EOI must be sent BEFORE driving preemption/scheduling.
    // This allows other interrupts to fire even if we switch away from this thread.
    super::apic::send_eoi();

    // Do not context-switch directly from the interrupt frame.  The current
    // switch_context path saves a normal function-call return address, not the
    // interrupted userspace RIP from this InterruptFrame.  Switching here can
    // therefore resume a thread inside the ISR path and becomes unstable under
    // SMP load.  Syscalls, explicit yields, sleeps and idle wakeups remain the
    // safe scheduler handoff points.
}


#[no_mangle]
pub extern "C" fn rust_reschedule_interrupt_handler(_frame: *const InterruptFrame) {
    crate::sched::on_reschedule_interrupt();
    super::apic::send_eoi();

    // A reschedule IPI may interrupt arbitrary kernel/idle code. Switching
    // directly from that interrupt frame would save the thread context at the
    // ISR return point instead of at a normal yield boundary. The interrupted
    // thread observes the pending flag and reschedules on its next cooperative
    // yield/syscall/idle boundary.
}

#[no_mangle]
pub extern "C" fn rust_keyboard_interrupt_handler(_frame: *const InterruptFrame) {
    // Buffer raw keyboard data for userspace driver
    input::on_keyboard_irq();

    // Notify userspace handler if registered
    if crate::syscall::has_userspace_irq_handler(1) {
        crate::syscall::notify_irq_handler(1);
    }

    super::apic::send_eoi();
}

#[no_mangle]
pub extern "C" fn rust_mouse_interrupt_handler(_frame: *const InterruptFrame) {
    // Buffer raw mouse data for userspace driver
    input::on_mouse_irq();

    // Notify userspace handler if registered
    if crate::syscall::has_userspace_irq_handler(12) {
        crate::syscall::notify_irq_handler(12);
    }

    super::apic::send_eoi();
}

#[no_mangle]
pub extern "C" fn rust_user_trap_interrupt_handler(
    frame: *const InterruptFrame
) {
    let frame = unsafe { &*frame };
    let cpl = frame.cs & 0x3;

    log_info!(
        "interrupt",
        "User trap INT 0x68: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
        frame.rip,
        frame.cs,
        frame.ss,
        cpl
    );

    super::apic::send_eoi();
}

pub fn get_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
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
    atom_abi::is_canonical(addr)
}
