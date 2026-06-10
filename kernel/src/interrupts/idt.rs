// kernel/src/interrupts/idt.rs
// Interrupt Descriptor Table (IDT) setup for x86_64.
//
// A single IDT is built once on the boot CPU and shared by every core: APs call
// `load_current_cpu()` to point their IDTR at the same table. This is the
// standard SMP arrangement — the descriptors are identical across CPUs, and the
// per-CPU divergence (which LAPIC handles a vector) is resolved by hardware, not
// by separate tables.
//
// The table is a private `static` accessed only through raw pointers
// (`addr_of!` / `addr_of_mut!`), so the code never forms a reference to mutable
// static storage and is accepted by the Rust 2024 edition's `static_mut_refs`
// rule.
//
// ── Architectural constraint: vector partitioning ──────────────────────────
// x86_64 permanently reserves vectors 0x00–0x1F for CPU exceptions. A hardware
// IRQ routed to a vector in that range would alias an exception handler. The
// invariant "every IRQ vector >= 0x20" is enforced at three layers: build time
// (kernel/build.rs), compile time (the const asserts below), and runtime (the
// const block at the top of `init`).
//
// Reference: Intel(R) 64 and IA-32 SDM, Vol. 3A, §6.3 and Table 6-1.

use core::mem::size_of;

use super::{
    KEYBOARD_INTERRUPT_VECTOR, MOUSE_INTERRUPT_VECTOR, RESCHEDULE_INTERRUPT_VECTOR,
    TIMER_INTERRUPT_VECTOR, USER_TRAP_INTERRUPT_VECTOR,
};
use crate::{log_debug, log_info};

// ── Compile-time vector safety assertions ──────────────────────────────────
const _: () = assert!(
    TIMER_INTERRUPT_VECTOR >= 0x20,
    "TIMER_INTERRUPT_VECTOR must be >= 0x20: x86_64 reserves 0x00-0x1F for CPU \
     exceptions; an IRQ there aliases an exception handler. Fix kernel/build.rs. \
     Ref: Intel SDM Vol. 3A §6.3, Table 6-1.",
);
const _: () = assert!(
    KEYBOARD_INTERRUPT_VECTOR >= 0x20,
    "KEYBOARD_INTERRUPT_VECTOR must be >= 0x20. Fix kernel/build.rs.",
);
const _: () = assert!(
    MOUSE_INTERRUPT_VECTOR >= 0x20,
    "MOUSE_INTERRUPT_VECTOR must be >= 0x20. Fix kernel/build.rs.",
);
const _: () = assert!(
    RESCHEDULE_INTERRUPT_VECTOR >= 0x20,
    "RESCHEDULE_INTERRUPT_VECTOR must be >= 0x20. Fix kernel/build.rs.",
);
const _: () = assert!(
    USER_TRAP_INTERRUPT_VECTOR >= 0x20,
    "USER_TRAP_INTERRUPT_VECTOR must be >= 0x20. Fix kernel/build.rs.",
);

const IDT_SIZE: usize = 256;
const DOUBLE_FAULT_IST: u8 = 1;

const GATE_TYPE_INTERRUPT: u8 = 0x8E; // present, DPL=0, 64-bit interrupt gate
const GATE_TYPE_TRAP: u8 = 0x8F; // present, DPL=0, 64-bit trap gate
const DPL_RING3: u8 = 0x60; // OR into type_attr to allow `int` from CPL=3
const KERNEL_CS: u16 = crate::arch::gdt::KERNEL_CODE_SELECTOR;
const LOG_ORIGIN: &str = "idt";

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const fn new() -> Self {
        IdtEntry {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    fn set_handler(&mut self, handler: usize, selector: u16, ist: u8, type_attr: u8) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFF_FFFF) as u32;
        self.selector = selector;
        self.ist = ist & 0x07;
        self.type_attr = type_attr;
        self.reserved = 0;
    }
}

#[repr(C, align(16))]
struct Idt {
    entries: [IdtEntry; IDT_SIZE],
}

impl Idt {
    const fn new() -> Self {
        Idt {
            entries: [IdtEntry::new(); IDT_SIZE],
        }
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

static mut IDT: Idt = Idt::new();

extern "C" {
    fn exception_handler_0();
    fn exception_handler_1();
    fn exception_handler_2();
    fn exception_handler_3();
    fn exception_handler_4();
    fn exception_handler_5();
    fn exception_handler_6();
    fn exception_handler_7();
    fn exception_handler_8();
    fn exception_handler_9();
    fn exception_handler_10();
    fn exception_handler_11();
    fn exception_handler_12();
    fn exception_handler_13();
    fn exception_handler_14();
    fn exception_handler_16();
    fn exception_handler_17();
    fn exception_handler_18();
    fn exception_handler_19();
    fn exception_handler_20();
    fn exception_handler_21();

    fn irq_handler_32();
    fn irq_handler_33();
    fn irq_handler_44();
    fn irq_handler_45();
    fn irq_handler_104();

    static unexpected_interrupt_table: [u64; IDT_SIZE];
}

/// Build the global IDT (boot CPU) and load it on the current CPU.
pub fn init() {
    // ── Runtime vector safety (belt-and-suspenders) ────────────────────────
    // Verified at compile time too; this also catches out-of-tree builds that
    // ship a stale generated vectors file without re-running build.rs.
    const {
        assert!(TIMER_INTERRUPT_VECTOR >= 0x20);
        assert!(KEYBOARD_INTERRUPT_VECTOR >= 0x20);
        assert!(MOUSE_INTERRUPT_VECTOR >= 0x20);
        assert!(RESCHEDULE_INTERRUPT_VECTOR >= 0x20);
        assert!(USER_TRAP_INTERRUPT_VECTOR >= 0x20);
    }

    unsafe {
        let idt_addr = core::ptr::addr_of!(IDT) as usize;
        log_debug!(LOG_ORIGIN, "IDT @ {:#X}", idt_addr);
        log_debug!(
            LOG_ORIGIN,
            "exc0={:#X} exc14={:#X} irq_timer={:#X}",
            exception_handler_0 as *const () as usize,
            exception_handler_14 as *const () as usize,
            irq_handler_32 as *const () as usize
        );

        // 1. Fill every vector with the catch-all stub from the assembly table.
        let default_handlers = core::ptr::addr_of!(unexpected_interrupt_table) as *const u64;
        let entries_ptr = core::ptr::addr_of_mut!(IDT.entries) as *mut IdtEntry;
        let entries = core::slice::from_raw_parts_mut(entries_ptr, IDT_SIZE);
        for (index, entry) in entries.iter_mut().enumerate() {
            let handler_addr = *default_handlers.add(index) as usize;
            entry.set_handler(handler_addr, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        }

        // 2. Install CPU exception vectors (0–21, skipping the reserved 15).
        //    #BP (3) uses a trap gate so IF is preserved for debuggers.
        let mut exc = |idx: usize, handler: usize, ist: u8, ty: u8| {
            entries[idx].set_handler(handler, KERNEL_CS, ist, ty);
        };
        exc(
            0,
            exception_handler_0 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            1,
            exception_handler_1 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            2,
            exception_handler_2 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            3,
            exception_handler_3 as *const () as usize,
            0,
            GATE_TYPE_TRAP,
        );
        exc(
            4,
            exception_handler_4 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            5,
            exception_handler_5 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            6,
            exception_handler_6 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            7,
            exception_handler_7 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        // #DF runs on a dedicated IST stack so a corrupted RSP cannot triple-fault.
        exc(
            8,
            exception_handler_8 as *const () as usize,
            DOUBLE_FAULT_IST,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            9,
            exception_handler_9 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            10,
            exception_handler_10 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            11,
            exception_handler_11 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            12,
            exception_handler_12 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            13,
            exception_handler_13 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            14,
            exception_handler_14 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            16,
            exception_handler_16 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            17,
            exception_handler_17 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            18,
            exception_handler_18 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            19,
            exception_handler_19 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            20,
            exception_handler_20 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            21,
            exception_handler_21 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );

        // 3. Hardware IRQ vectors (from the generated single source of truth).
        exc(
            TIMER_INTERRUPT_VECTOR as usize,
            irq_handler_32 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            KEYBOARD_INTERRUPT_VECTOR as usize,
            irq_handler_33 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            MOUSE_INTERRUPT_VECTOR as usize,
            irq_handler_44 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );
        exc(
            RESCHEDULE_INTERRUPT_VECTOR as usize,
            irq_handler_45 as *const () as usize,
            0,
            GATE_TYPE_INTERRUPT,
        );

        // 4. User-triggerable software trap (`int 0x68`): DPL=3 so CPL=3 can
        //    invoke it; trap gate keeps IF unchanged.
        exc(
            USER_TRAP_INTERRUPT_VECTOR as usize,
            irq_handler_104 as *const () as usize,
            0,
            GATE_TYPE_TRAP | DPL_RING3,
        );
    }

    load_current_cpu();
    log_info!(LOG_ORIGIN, "IDT initialized with {} entries", IDT_SIZE);
}

/// Load the already-built global IDT on the current CPU.
///
/// APs call this during SMP bring-up; the BSP calls it at the end of `init`.
pub fn load_current_cpu() {
    unsafe {
        let idt_ptr = IdtPointer {
            limit: (size_of::<Idt>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };
        load_idt(&idt_ptr);
    }
}

#[inline]
unsafe fn load_idt(idt_ptr: &IdtPointer) {
    core::arch::asm!(
        "lidt [{}]",
        in(reg) idt_ptr,
        options(readonly, nostack, preserves_flags),
    );
}
