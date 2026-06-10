// kernel/src/interrupts/handlers.rs
// Interrupt and Exception Handlers
//
// Rust-side interrupt dispatch for the x86_64 assembly stubs in handlers.asm.
//
// This file intentionally keeps the ABI surface stable:
// - `InterruptFrame` has the exact 176-byte layout built by handlers.asm.
// - All exported handlers remain `#[no_mangle] extern "C"`.
// - IRQ handlers always send EOI after device work.
// - Timer/reschedule IRQs do not context-switch directly from an interrupt
//   frame; they only mark scheduler state and return to a safe handoff point.
//
// Exception policy:
// - Page faults run through one coherent path.
// - Resolved faults return and retry the faulting instruction.
// - Fatal userspace faults are diagnosed first, then the offending thread is
//   terminated and the scheduler is asked for the next runnable thread.
// - Fatal kernel faults produce a register/fault dump and halt.
//
// The important fix versus the old flow is that fatal userspace #PF no longer
// terminates inside the VMA fast path before diagnostics run. This makes the
// null-function-pointer-call detector reachable and useful.

use crate::arch::{gdt, halt};
use crate::input;
use crate::ipc;
use crate::mm;
use crate::sched;
#[allow(unused_imports)]
use crate::util::UI_DIRTY;
use crate::{log_debug, log_error, log_info, log_panic, log_warn};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::interrupts::LOG_ORIGIN;

const EXCEPTION_LOG_ORIGIN: &str = "exception";

// ---------------------------------------------------------------------------
// Fault loop detector
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
    const EMPTY: Self = Self {
        pid_raw: 0,
        page_addr: 0,
        count: 0,
    };
}

static FAULT_LOOP_TABLE: spin::Mutex<[FaultSlot; FAULT_LOOP_SLOTS]> =
    spin::Mutex::new([FaultSlot::EMPTY; FAULT_LOOP_SLOTS]);

fn record_fault_and_check_loop(pid: crate::process::ProcessId, page_addr: usize) -> bool {
    let slot_idx = ((pid.raw() ^ page_addr as u64) as usize) % FAULT_LOOP_SLOTS;
    let mut table = FAULT_LOOP_TABLE.lock();
    let slot = &mut table[slot_idx];

    if slot.pid_raw == pid.raw() && slot.page_addr == page_addr {
        slot.count = slot.count.saturating_add(1);
        slot.count >= FAULT_LOOP_THRESHOLD
    } else {
        *slot = FaultSlot {
            pid_raw: pid.raw(),
            page_addr,
            count: 1,
        };
        false
    }
}

fn clear_fault_loop_counter(pid: crate::process::ProcessId, page_addr: usize) {
    let slot_idx = ((pid.raw() ^ page_addr as u64) as usize) % FAULT_LOOP_SLOTS;
    let mut table = FAULT_LOOP_TABLE.lock();
    let slot = &mut table[slot_idx];
    if slot.pid_raw == pid.raw() && slot.page_addr == page_addr {
        *slot = FaultSlot::EMPTY;
    }
}

// ---------------------------------------------------------------------------
// Exception names and frame ABI
// ---------------------------------------------------------------------------

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

#[allow(dead_code)]
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
    // Pushed by PUSH_ALL in handlers.asm.
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,

    // Pushed by handlers.asm.
    pub exception_number: u64,
    pub error_code: u64,

    // Pushed by the CPU hardware.
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

const _: () = {
    use core::mem::{offset_of, size_of};

    assert!(
        size_of::<InterruptFrame>() == 22 * 8,
        "InterruptFrame size mismatch: expected exactly 176 bytes"
    );

    assert!(offset_of!(InterruptFrame, r15) == 0);
    assert!(offset_of!(InterruptFrame, rax) == 14 * 8);
    assert!(offset_of!(InterruptFrame, exception_number) == 15 * 8);
    assert!(offset_of!(InterruptFrame, error_code) == 16 * 8);
    assert!(offset_of!(InterruptFrame, rip) == 17 * 8);
    assert!(offset_of!(InterruptFrame, cs) == 18 * 8);
    assert!(offset_of!(InterruptFrame, rflags) == 19 * 8);
    assert!(offset_of!(InterruptFrame, rsp) == 20 * 8);
    assert!(offset_of!(InterruptFrame, ss) == 21 * 8);
};

// ---------------------------------------------------------------------------
// Top-level handlers called from assembly
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn rust_unexpected_interrupt_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    let vector = frame.exception_number;

    #[cfg(debug_assertions)]
    if vector > 255 {
        log_panic!(
            "interrupt",
            "ABI mismatch: vector={:#X} but expected 0..255; check handlers.asm frame layout",
            vector
        );
    }

    if vector > 255 {
        super::apic::send_eoi();
        log_warn!(LOG_ORIGIN, "Invalid vector {} received", vector);
        return;
    }

    if vector == 0xFF {
        // Spurious local-APIC vector: no EOI.
        return;
    }

    log_warn!(
        LOG_ORIGIN,
        "Unexpected vector {} at RIP={:#X} CS={:#X} CPL={}",
        vector,
        frame.rip,
        frame.cs,
        frame.cs & 0x3
    );

    super::apic::send_eoi();
}

#[no_mangle]
pub extern "C" fn rust_exception_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    let vector = frame.exception_number;
    let error_code = frame.error_code;
    let from_userspace = is_from_userspace(frame);

    if (vector as usize) >= EXCEPTION_NAMES.len() {
        log_panic!(EXCEPTION_LOG_ORIGIN, "Bad exception vector: {}", vector);
        dump_core_registers(frame);
        halt_forever();
    }

    if vector == 14 {
        let cr2 = read_cr2();

        match try_resolve_page_fault(frame, cr2, error_code, from_userspace) {
            PageFaultAction::Resolved => return,
            PageFaultAction::Fatal(result) => {
                dump_exception_header(frame, vector, error_code, from_userspace);
                dump_core_registers(frame);
                diagnose_page_fault(frame, cr2, error_code, from_userspace, result);

                if from_userspace {
                    terminate_userspace_thread(
                        frame,
                        crate::thread::TerminationReason::PageFault {
                            address: cr2,
                            error_code,
                            rip: frame.rip,
                        },
                        "fatal userspace page fault",
                    );
                }

                log_panic!(EXCEPTION_LOG_ORIGIN, "Fatal kernel page fault");
                halt_forever();
            }
        }
    }

    dump_exception_header(frame, vector, error_code, from_userspace);
    dump_core_registers(frame);

    if vector == 13 {
        diagnose_general_protection_fault(frame, error_code, from_userspace)
    }

    if from_userspace {
        let reason = if vector == 13 {
            crate::thread::TerminationReason::GeneralProtectionFault {
                error_code,
                rip: frame.rip,
            }
        } else {
            crate::thread::TerminationReason::Exception {
                vector,
                error_code,
                rip: frame.rip,
            }
        };

        terminate_userspace_thread(frame, reason, "fatal userspace exception");
    }

    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "System halted due to fatal kernel exception: {}",
        EXCEPTION_NAMES[vector as usize]
    );
    halt_forever();
}

// ---------------------------------------------------------------------------
// Page-fault path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageFaultAction {
    Resolved,
    Fatal(mm::vma::FaultResult),
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

fn try_resolve_page_fault(
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
    from_userspace: bool,
) -> PageFaultAction {
    let Some(tid) = sched::current_thread() else {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    };

    if from_userspace {
        return handle_userspace_page_fault(tid, frame, cr2, error_code);
    }

    // Kernel-mode faults can still be legitimate faults against the current
    // process address space, e.g. copy-to-user/copy-from-user or lazily backed
    // VMAs touched while executing in kernel mode.
    handle_current_address_space_page_fault(tid, frame, cr2, error_code)
}

fn handle_userspace_page_fault(
    tid: crate::thread::ThreadId,
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
) -> PageFaultAction {
    let current_cr3_raw = crate::arch::read_cr3();
    let current_cr3 = crate::arch::cr3_to_pml4_phys(current_cr3_raw);
    if current_cr3 == 0 {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    }

    if current_cr3_raw != current_cr3 {
        log_debug!(
            LOG_ORIGIN,
            "[PF] cr3_sanitized raw={:#x} pml4={:#x}",
            current_cr3_raw,
            current_cr3
        );
    }

    let Some(pid) = crate::thread::get_thread_process_id(tid) else {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    };

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
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    }

    let page_addr = (cr2 as usize) & !(crate::mm::pmm::PAGE_SIZE - 1);
    if record_fault_and_check_loop(pid, page_addr) {
        log_error!(
            LOG_ORIGIN,
            "[PF] FAULT_LOOP pid={} cr3={:#x} addr={:#x} page={:#x} rip={:#x} err={:#x}",
            pid,
            current_cr3,
            cr2,
            page_addr,
            frame.rip,
            error_code
        );
        clear_fault_loop_counter(pid, page_addr);
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    }

    let ctx = mm::vma::FaultContext::from_x86_error(
        pid,
        mm::vma::AddressSpaceId::new(current_cr3 as usize),
        cr2 as usize,
        error_code,
    );

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
            PageFaultAction::Resolved
        }
        FaultOutcome::Fatal(fatal_result) => {
            clear_fault_loop_counter(pid, page_addr);
            log_error!(
                LOG_ORIGIN,
                "[PF] FATAL pid={} cr3={:#x} addr={:#x} rip={:#x} err={:#x} result={:?} action=diagnose_then_terminate",
                pid,
                current_cr3,
                cr2,
                frame.rip,
                error_code,
                fatal_result
            );
            PageFaultAction::Fatal(fatal_result)
        }
    }
}

fn handle_current_address_space_page_fault(
    tid: crate::thread::ThreadId,
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
) -> PageFaultAction {
    let Some(address_space) = crate::thread::get_thread_address_space(tid) else {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    };
    let pml4 = crate::arch::cr3_to_pml4_phys(address_space);
    if pml4 == 0 {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    }

    let Some(pid) = crate::thread::get_thread_process_id(tid) else {
        return PageFaultAction::Fatal(mm::vma::FaultResult::InvalidAddress);
    };

    let ctx = mm::vma::FaultContext::from_x86_error(
        pid,
        mm::vma::AddressSpaceId::new(pml4 as usize),
        cr2 as usize,
        error_code,
    );

    let result = mm::vma::handle_page_fault(ctx, error_code);
    match classify_fault_outcome(result) {
        FaultOutcome::Resolved => {
            log_debug!(
                LOG_ORIGIN,
                "[PF] kernel_context_resolved pid={} pml4={:#x} addr={:#x} rip={:#x} err={:#x}",
                pid,
                pml4,
                cr2,
                frame.rip,
                error_code
            );
            PageFaultAction::Resolved
        }
        FaultOutcome::Fatal(fatal_result) => PageFaultAction::Fatal(fatal_result),
    }
}

#[derive(Debug, Clone, Copy)]
struct PageFaultBits {
    present: bool,
    write: bool,
    user: bool,
    reserved: bool,
    ifetch: bool,
    protection_key: bool,
    shadow_stack: bool,
    sgx: bool,
}

impl PageFaultBits {
    fn decode(error_code: u64) -> Self {
        Self {
            present: error_code & (1 << 0) != 0,
            write: error_code & (1 << 1) != 0,
            user: error_code & (1 << 2) != 0,
            reserved: error_code & (1 << 3) != 0,
            ifetch: error_code & (1 << 4) != 0,
            protection_key: error_code & (1 << 5) != 0,
            shadow_stack: error_code & (1 << 6) != 0,
            sgx: error_code & (1 << 15) != 0,
        }
    }
}

fn diagnose_page_fault(
    frame: &InterruptFrame,
    cr2: u64,
    error_code: u64,
    from_userspace: bool,
    fault_result: mm::vma::FaultResult,
) {
    let bits = PageFaultBits::decode(error_code);
    let cr3_raw = crate::arch::read_cr3();
    let cr3_pml4 = crate::arch::cr3_to_pml4_phys(cr3_raw);
    let tid = sched::current_thread();

    log_panic!(EXCEPTION_LOG_ORIGIN, "Page Fault at address {:#016X}", cr2);
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "PF result={:?} RIP={:#016X} RSP={:#016X} CR3(raw={:#016X}, pml4={:#016X}) TID={:?}",
        fault_result,
        frame.rip,
        frame.rsp,
        cr3_raw,
        cr3_pml4,
        tid
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "PF error={:#X}: present={} write={} user={} reserved={} ifetch={} pkey={} shadow_stack={} sgx={}",
        error_code,
        bits.present,
        bits.write,
        bits.user,
        bits.reserved,
        bits.ifetch,
        bits.protection_key,
        bits.shadow_stack,
        bits.sgx
    );

    diagnose_null_function_pointer_call(frame, cr2, cr3_pml4, bits, from_userspace);
}

fn diagnose_null_function_pointer_call(
    frame: &InterruptFrame,
    cr2: u64,
    cr3_pml4: u64,
    bits: PageFaultBits,
    from_userspace: bool,
) {
    let null_call_not_present =
        cr2 == 0 && frame.rip == 0 && bits.ifetch && bits.user && !bits.present && from_userspace;

    let null_call_present =
        cr2 == 0 && frame.rip == 0 && bits.ifetch && bits.user && bits.present && from_userspace;

    if !(null_call_not_present || null_call_present) {
        return;
    }

    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "=== NULL FUNCTION POINTER CALL DETECTED (userspace) ==="
    );

    if null_call_present {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "DIAGNOSIS: VA 0 is present/executable enough to fault as protection violation; verify the null-page guard in process address-space cloning"
        );
    } else {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "DIAGNOSIS: indirect call/jump targeted VA 0 and the null-page guard converted it into a not-present instruction-fetch #PF"
        );
    }

    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "GPRs: RAX={:#018X} RBX={:#018X} RCX={:#018X} RDX={:#018X}",
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "GPRs: RSI={:#018X} RDI={:#018X} RBP={:#018X} RSP={:#018X}",
        frame.rsi,
        frame.rdi,
        frame.rbp,
        frame.rsp
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "GPRs: R8={:#018X} R9={:#018X} R10={:#018X} R11={:#018X}",
        frame.r8,
        frame.r9,
        frame.r10,
        frame.r11
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "GPRs: R12={:#018X} R13={:#018X} R14={:#018X} R15={:#018X}",
        frame.r12,
        frame.r13,
        frame.r14,
        frame.r15
    );

    try_dump_user_return_address(frame.rsp, cr3_pml4);

    log_panic!(EXCEPTION_LOG_ORIGIN, "=== END NULL CALL DIAGNOSIS ===");
}

fn try_dump_user_return_address(user_rsp: u64, pml4_phys: u64) {
    if pml4_phys == 0 {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot read null-call return address: invalid CR3/PML4"
        );
        return;
    }

    if user_rsp & 0x7 != 0 {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot read null-call return address: unaligned user RSP={:#X}",
            user_rsp
        );
        return;
    }

    let rsp_page = (user_rsp & !0xFFF) as usize;
    let rsp_offset = (user_rsp & 0xFFF) as usize;
    if rsp_offset > 0x1000 - core::mem::size_of::<u64>() {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot read null-call return address: RSP={:#X} crosses page boundary",
            user_rsp
        );
        return;
    }

    let Some(pte_raw) = mm::vm::read_pte_in_pml4(pml4_phys as usize, rsp_page) else {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot read null-call return address: page-table walk failed for RSP={:#X} PML4={:#X}",
            user_rsp,
            pml4_phys
        );
        return;
    };

    if pte_raw & 0x1 == 0 {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot read null-call return address: user RSP page {:#X} not present in PML4 {:#X}",
            rsp_page,
            pml4_phys
        );
        return;
    }

    let phys_base = (pte_raw & 0x000F_FFFF_FFFF_F000) as usize;
    let ret_phys = phys_base + rsp_offset;
    let ret_virt = mm::vm::phys_to_virt_ptr(ret_phys);
    let ret_addr = unsafe { core::ptr::read_volatile(ret_virt as *const u64) };

    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "NULL CALL return address: {:#018X}  (from user RSP={:#X})",
        ret_addr,
        user_rsp
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        ">>> Run: addr2line -e <userspace.elf> {:#X}",
        ret_addr
    );

    let remaining = 0x1000 - rsp_offset;
    let words = core::cmp::min(remaining / core::mem::size_of::<u64>(), 8);
    if words > 0 {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "User stack dump (top {} words):",
            words
        );
        for i in 0..words {
            let word_phys = phys_base + rsp_offset + i * core::mem::size_of::<u64>();
            let word_virt = mm::vm::phys_to_virt_ptr(word_phys);
            let word = unsafe { core::ptr::read_volatile(word_virt as *const u64) };
            log_panic!(
                EXCEPTION_LOG_ORIGIN,
                "  [RSP+{:#04X}] = {:#018X}{}",
                i * 8,
                word,
                if i == 0 {
                    "  <-- return address (CALL site)"
                } else {
                    ""
                }
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Fatal exception support
// ---------------------------------------------------------------------------

fn terminate_userspace_thread(
    frame: &InterruptFrame,
    reason: crate::thread::TerminationReason,
    why: &'static str,
) -> ! {
    let Some(tid) = sched::current_thread() else {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "{} but no current thread exists; halting",
            why
        );
        halt_forever();
    };

    let pid = crate::thread::get_thread_process_id(tid);
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "{} -> terminating PID {:?} TID {} at RIP={:#016X}",
        why,
        pid,
        tid,
        frame.rip
    );

    crate::thread::terminate_entity(tid, reason);

    let (_, next) = sched::on_timer_tick();
    if let Some(next_id) = next {
        log_info!(EXCEPTION_LOG_ORIGIN, "Switching to next thread {}", next_id);
        crate::sched::perform_context_switch(tid, next_id);
    }

    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "No runnable threads available after terminating faulting thread {}",
        tid
    );
    halt_forever();
}

fn dump_exception_header(
    frame: &InterruptFrame,
    vector: u64,
    error_code: u64,
    from_userspace: bool,
) {
    let name = EXCEPTION_NAMES[vector as usize];
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "CPU exception: {} (vector={}, from_userspace={})",
        name,
        vector,
        from_userspace
    );
    log_panic!(EXCEPTION_LOG_ORIGIN, "Error code: {:#X}", error_code);
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "RIP={:#018X} CS={:#06X} RFLAGS={:#018X} RSP={:#018X} SS={:#06X}",
        frame.rip,
        frame.cs,
        frame.rflags,
        frame.rsp,
        frame.ss
    );
}

fn dump_core_registers(frame: &InterruptFrame) {
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "RAX={:#018X} RBX={:#018X} RCX={:#018X} RDX={:#018X}",
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "RSI={:#018X} RDI={:#018X} RBP={:#018X}",
        frame.rsi,
        frame.rdi,
        frame.rbp
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "R8={:#018X} R9={:#018X} R10={:#018X} R11={:#018X}",
        frame.r8,
        frame.r9,
        frame.r10,
        frame.r11
    );
    log_panic!(
        EXCEPTION_LOG_ORIGIN,
        "R12={:#018X} R13={:#018X} R14={:#018X} R15={:#018X}",
        frame.r12,
        frame.r13,
        frame.r14,
        frame.r15
    );
}

fn diagnose_general_protection_fault(
    frame: &InterruptFrame,
    error_code: u64,
    from_userspace: bool,
) {
    log_panic!(EXCEPTION_LOG_ORIGIN, "General Protection Fault");

    if error_code != 0 {
        let selector = error_code & !0x7;
        let table = match (error_code >> 1) & 0x3 {
            0 => "GDT",
            1 => "IDT",
            2 => "LDT",
            3 => "IDT",
            _ => "unknown",
        };
        let external = error_code & 0x1 != 0;
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "GP selector error: raw={:#X} selector={:#X} table={} external={}",
            error_code,
            selector,
            table,
            external
        );
    }

    if !from_userspace {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Kernel #GP stack snapshot from RSP={:#016X}",
            frame.rsp
        );
        dump_kernel_stack_words(frame.rsp, 10);
    }
}

fn dump_kernel_stack_words(stack_ptr: u64, count: usize) {
    if !is_canonical(stack_ptr) {
        log_panic!(
            EXCEPTION_LOG_ORIGIN,
            "Cannot dump kernel stack: non-canonical RSP={:#X}",
            stack_ptr
        );
        return;
    }

    unsafe {
        let stack = stack_ptr as *const u64;
        for i in 0..count {
            let addr = stack.add(i);
            let val = core::ptr::read_volatile(addr);
            log_panic!(
                EXCEPTION_LOG_ORIGIN,
                "  [RSP+{:#04X}] = {:#016X}{}",
                i * 8,
                val,
                match i {
                    0 => " (top of interrupted stack)",
                    1 => "",
                    2 => "",
                    3 => "",
                    4 => "",
                    _ => "",
                }
            );
        }
    }
}

#[inline]
fn halt_forever() -> ! {
    loop {
        halt();
    }
}

// ---------------------------------------------------------------------------
// IRQ counters and snapshots
// ---------------------------------------------------------------------------

static TICKS: AtomicU64 = AtomicU64::new(0);
static USER_MODE_INTERRUPTED: AtomicBool = AtomicBool::new(false);
#[allow(dead_code)]
static INTERRUPT_SWITCH_SKIP_LOGGED: AtomicBool = AtomicBool::new(false);
static IRQ_TIMER_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_TIMER_EOI_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_KEYBOARD_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_KEYBOARD_EOI_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_MOUSE_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_MOUSE_EOI_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_RESCHEDULE_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_RESCHEDULE_EOI_COUNT: AtomicU64 = AtomicU64::new(0);

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct IrqSnapshot {
    pub timer: u64,
    pub timer_eoi: u64,
    pub keyboard: u64,
    pub keyboard_eoi: u64,
    pub mouse: u64,
    pub mouse_eoi: u64,
    pub reschedule: u64,
    pub reschedule_eoi: u64,
}

pub fn irq_snapshot() -> IrqSnapshot {
    IrqSnapshot {
        timer: IRQ_TIMER_COUNT.load(Ordering::Relaxed),
        timer_eoi: IRQ_TIMER_EOI_COUNT.load(Ordering::Relaxed),
        keyboard: IRQ_KEYBOARD_COUNT.load(Ordering::Relaxed),
        keyboard_eoi: IRQ_KEYBOARD_EOI_COUNT.load(Ordering::Relaxed),
        mouse: IRQ_MOUSE_COUNT.load(Ordering::Relaxed),
        mouse_eoi: IRQ_MOUSE_EOI_COUNT.load(Ordering::Relaxed),
        reschedule: IRQ_RESCHEDULE_COUNT.load(Ordering::Relaxed),
        reschedule_eoi: IRQ_RESCHEDULE_EOI_COUNT.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// Hardware IRQ handlers
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn rust_timer_interrupt_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    let coming_from_user = is_from_userspace(frame);

    validate_interrupt_frame(frame, coming_from_user, "Timer");

    if coming_from_user
        && USER_MODE_INTERRUPTED
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        log_info!(
            "interrupt",
            "Timer interrupted user context: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
            frame.rip,
            frame.cs,
            frame.ss,
            frame.cs & 0x3
        );
    }

    IRQ_TIMER_COUNT.fetch_add(1, Ordering::Relaxed);
    TICKS.fetch_add(1, Ordering::Relaxed);
    sched::account_timer_tick();

    ipc::on_timer_tick(get_ticks());
    sched::wake_sleeping_threads();

    // EOI before scheduler/preemption bookkeeping, so the APIC can deliver the
    // next interrupt even if later code marks a reschedule as pending.
    super::apic::send_eoi();
    IRQ_TIMER_EOI_COUNT.fetch_add(1, Ordering::Relaxed);

    // Do not context-switch directly from this interrupt frame. The current
    // context-switch path saves a normal call frame, not the interrupted frame.
}

#[no_mangle]
pub extern "C" fn rust_reschedule_interrupt_handler(_frame: *const InterruptFrame) {
    IRQ_RESCHEDULE_COUNT.fetch_add(1, Ordering::Relaxed);
    crate::sched::on_reschedule_interrupt();
    super::apic::send_eoi();
    IRQ_RESCHEDULE_EOI_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn rust_keyboard_interrupt_handler(_frame: *const InterruptFrame) {
    IRQ_KEYBOARD_COUNT.fetch_add(1, Ordering::Relaxed);

    input::on_keyboard_irq();

    if crate::syscall::has_userspace_irq_handler(1) {
        crate::syscall::notify_irq_handler(1);
    }

    super::apic::send_eoi();
    IRQ_KEYBOARD_EOI_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn rust_mouse_interrupt_handler(_frame: *const InterruptFrame) {
    IRQ_MOUSE_COUNT.fetch_add(1, Ordering::Relaxed);

    input::on_mouse_irq();

    if crate::syscall::has_userspace_irq_handler(12) {
        crate::syscall::notify_irq_handler(12);
    }

    super::apic::send_eoi();
    IRQ_MOUSE_EOI_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn rust_user_trap_interrupt_handler(frame: *const InterruptFrame) {
    let frame = unsafe { &*frame };
    log_info!(
        "interrupt",
        "User trap INT 0x68: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={}",
        frame.rip,
        frame.cs,
        frame.ss,
        frame.cs & 0x3
    );

    // Software trap: do not send a hardware EOI.
}

// ---------------------------------------------------------------------------
// Public utilities
// ---------------------------------------------------------------------------

pub fn get_ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn print_stack_trace(stack_ptr: u64) {
    log_debug!(
        EXCEPTION_LOG_ORIGIN,
        "Stack trace dump (starting at {:#016X})",
        stack_ptr
    );

    let stack = unsafe { core::slice::from_raw_parts(stack_ptr as *const u64, 16) };
    for (i, value) in stack.iter().enumerate() {
        log_debug!(EXCEPTION_LOG_ORIGIN, "Stack[{}] = {:#016X}", i, value);
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

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

#[inline]
fn is_from_userspace(frame: &InterruptFrame) -> bool {
    (frame.cs & 0x3) == 0x3
}

fn validate_interrupt_frame(frame: &InterruptFrame, coming_from_user: bool, source: &str) {
    let rip_canonical = is_canonical(frame.rip);
    let rsp_canonical = is_canonical(frame.rsp);

    if coming_from_user {
        let cs_valid = (frame.cs as u16) == gdt::USER_CODE_SELECTOR;
        let ss_valid = (frame.ss as u16) == gdt::USER_DATA_SELECTOR;
        if !(cs_valid && ss_valid && rip_canonical && rsp_canonical) {
            log_warn!(
                "interrupt",
                "{} frame sanity check failed: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X} canonical_rip={} canonical_rsp={} cs_ok={} ss_ok={}",
                source,
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
        let cs_valid = (frame.cs as u16) == gdt::KERNEL_CODE_SELECTOR;
        let ss_valid = (frame.ss as u16) == gdt::KERNEL_DATA_SELECTOR || frame.ss == 0;
        if !(cs_valid && ss_valid && rip_canonical && rsp_canonical) {
            log_warn!(
                "interrupt",
                "{} kernel frame sanity check failed: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X}",
                source,
                frame.rip,
                frame.rsp,
                frame.cs,
                frame.ss
            );
        }
    }
}

#[inline]
fn is_canonical(addr: u64) -> bool {
    atom_abi::is_canonical(addr)
}
