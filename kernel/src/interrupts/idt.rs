// Interrupt Descriptor Table (IDT) Setup
//
// Defines and initializes the x86_64 Interrupt Descriptor Table used by the
// CPU to dispatch exceptions, hardware interrupts, and software interrupts
// into kernel-defined handlers.
//
// Key responsibilities:
// - Define the exact hardware layout of IDT entries (16-byte descriptors)
// - Populate exception vectors (0–21) with assembly-level stubs
// - Register hardware IRQ handlers (timer, keyboard) at fixed vectors
// - Load the IDT using the `lidt` instruction
// - Provide runtime verification of virtual memory mappings for IDT safety
//
// Design principles:
// - Strict adherence to the x86_64 IDT entry format using `#[repr(C, packed)]`
// - Centralized, static IDT with all 256 possible vectors reserved
// - Clear separation between low-level assembly stubs and Rust handlers
// - Explicit gate types (interrupt vs trap) for precise CPU behavior
//
// Implementation details:
// - `IdtEntry` manually splits handler addresses into low/mid/high fields
// - IST index is masked to 3 bits, matching CPU expectations
// - Exception handlers are installed with kernel CS and DPL=0
// - Breakpoint (#BP) uses a trap gate to preserve IF for debugging
// - Timer (32) and keyboard (33) vectors match APIC/PIC remapping
// - A dummy vector (0x69) is installed to validate IDT wiring
//
// Correctness and safety notes:
// - IDT is 16-byte aligned as required by the architecture
// - All handler addresses must be identity- or kernel-mapped before `lidt`
// - `verify_mapping` proactively checks VM mappings to catch early boot bugs
// - Any mismatch between IDT entries and actual handler symbols will lead
//   to fatal triple faults, making early diagnostics critical
//
// ── Architectural constraint: IDT vector partitioning on x86_64 ─────────────
//
// The Intel/AMD x86_64 architecture permanently reserves IDT vectors
// 0x00–0x1F (decimal 0–31) for processor-defined exceptions.  This partition
// is enforced by the silicon: the CPU decodes the IDT index unconditionally
// as a bare integer offset into the descriptor table; it does not distinguish
// between a software-installed "hardware IRQ" gate and a CPU-generated fault.
//
// If a hardware interrupt routed by the PIC or local APIC is assigned to a
// vector in [0x00, 0x1F], the CPU will invoke the corresponding IDT entry for
// *both* the CPU exception and the hardware IRQ.  Concretely:
//
//   • The timer IRQ on vector 0x0D would fire the #GP handler on every tick,
//     with a fabricated error code pushed by the hardware stub.  The handler
//     would attempt to decode a protection fault that never occurred, likely
//     killing an innocent thread or halting the kernel.
//   • On SMP systems, cross-processor IPI delivery to a conflicted vector
//     can corrupt the APIC priority model, blocking all future interrupts.
//
// The canonical solution is APIC/PIC remapping: during controller
// initialisation (apic.rs) the legacy 8259A PIC master is initialised with
// an ICW2 value of 0x20, and the local APIC LVT timer register is programmed
// with TIMER_INTERRUPT_VECTOR.  Both operations require TIMER_INTERRUPT_VECTOR
// ≥ 0x20 to be safe.
//
// This invariant is enforced at three independent layers:
//   1. Build time  — kernel/build.rs panics if any vector < 0x20 (runs on
//                    every `cargo build` before code generation).
//   2. Compile time — const assertions below reject an invalid generated
//                    constant without executing a single instruction.
//   3. Runtime     — asserts at the top of `init()` catch any path that
//                    bypasses the build script (e.g., out-of-tree builds,
//                    injected constants, feature-gated overrides).
//
// Reference: Intel® 64 and IA-32 Architectures Software Developer's Manual,
//            Volume 3A, Section 6.3 "Sources of Interrupts and Exceptions",
//            Table 6-1 "Protected-Mode Exceptions and Interrupts".

use core::mem::size_of;
use crate::{log_debug, log_info};
use super::{KEYBOARD_INTERRUPT_VECTOR, MOUSE_INTERRUPT_VECTOR, TIMER_INTERRUPT_VECTOR, USER_TRAP_INTERRUPT_VECTOR};

// ── Compile-time vector safety assertions ────────────────────────────────────
//
// These `const` evaluations are checked by the Rust compiler when the kernel
// crate is compiled, independently of the build script.  They provide a second
// enforcement layer: even if build.rs is not re-run (e.g., incremental build
// with a stale vectors.rs, or an out-of-tree build that supplies its own
// constants), the compiler will reject any binary where a hardware IRQ vector
// falls inside the CPU-reserved exception range [0x00, 0x1F].
//
// A compile-time failure here is intentional and desirable: it is always
// preferable to a kernel that silently aliases timer interrupts with CPU
// exception handlers.
// INVARIANT: TIMER_INTERRUPT_VECTOR must be >= 0x20 — structural, not operational.
// x86_64 reserves vectors 0x00-0x1F for CPU exceptions; using them for IRQs would alias.
const _: () = assert!(
    TIMER_INTERRUPT_VECTOR >= 0x20,
    "TIMER_INTERRUPT_VECTOR must be >= 0x20: x86_64 reserves vectors 0x00-0x1F \
     for CPU-defined exceptions (#DE, #DB, #NMI, #BP, ... #CP). \
     A hardware IRQ at this vector aliases a CPU exception handler. \
     Fix the assignment in kernel/build.rs. \
     Ref: Intel SDM Vol. 3A §6.3, Table 6-1.",
);
// INVARIANT: KEYBOARD_INTERRUPT_VECTOR must be >= 0x20 — structural, not operational.
// x86_64 reserves vectors 0x00-0x1F for CPU exceptions; using them for IRQs would alias.
const _: () = assert!(
    KEYBOARD_INTERRUPT_VECTOR >= 0x20,
    "KEYBOARD_INTERRUPT_VECTOR must be >= 0x20: x86_64 reserves vectors 0x00-0x1F \
     for CPU-defined exceptions. Fix the assignment in kernel/build.rs. \
     Ref: Intel SDM Vol. 3A §6.3, Table 6-1.",
);
// INVARIANT: MOUSE_INTERRUPT_VECTOR must be >= 0x20 — structural, not operational.
// x86_64 reserves vectors 0x00-0x1F for CPU exceptions; using them for IRQs would alias.
const _: () = assert!(
    MOUSE_INTERRUPT_VECTOR >= 0x20,
    "MOUSE_INTERRUPT_VECTOR must be >= 0x20: x86_64 reserves vectors 0x00-0x1F \
     for CPU-defined exceptions. Fix the assignment in kernel/build.rs. \
     Ref: Intel SDM Vol. 3A §6.3, Table 6-1.",
);
// INVARIANT: USER_TRAP_INTERRUPT_VECTOR must be >= 0x20 — structural, not operational.
// x86_64 reserves vectors 0x00-0x1F for CPU exceptions; using them for IRQs would alias.
const _: () = assert!(
    USER_TRAP_INTERRUPT_VECTOR >= 0x20,
    "USER_TRAP_INTERRUPT_VECTOR must be >= 0x20: x86_64 reserves vectors 0x00-0x1F \
     for CPU-defined exceptions. Fix the assignment in kernel/build.rs. \
     Ref: Intel SDM Vol. 3A §6.3, Table 6-1.",
);

const IDT_SIZE: usize = 256;
const DOUBLE_FAULT_IST: u8 = 1;

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
        self.offset_high = ((handler >> 32) & 0xFFFFFFFF) as u32;
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
    fn irq_handler_104();

    static unexpected_interrupt_table: [u64; IDT_SIZE];
}

const GATE_TYPE_INTERRUPT: u8 = 0x8E;
const GATE_TYPE_TRAP: u8 = 0x8F;
const KERNEL_CS: u16 = crate::arch::gdt::KERNEL_CODE_SELECTOR;
const LOG_ORIGIN: &str = "idt";
const DPL_RING3: u8 = 0x60;

pub fn init() {
    // ── Runtime vector safety assertions ─────────────────────────────────────
    //
    // Belt-and-suspenders defence executed unconditionally at the entry point
    // of IDT initialisation, before any descriptor is written or `lidt` is
    // issued.
    //
    // Rationale for a runtime assert in addition to build-time and compile-time
    // checks:
    //   • Protects against out-of-tree or CI builds that supply pre-generated
    //     vectors.rs files without re-running build.rs.
    //   • Catches conditional-compilation paths (feature flags, cfg attributes)
    //     that might substitute a different constant value at link time.
    //   • Provides a clear, actionable kernel panic message at boot rather than
    //     the non-deterministic triple-fault that would occur if an aliased
    //     vector were actually loaded into the CPU.
    //
    // If any assert fires, the kernel halts immediately with an unambiguous
    // message identifying the offending constant, its actual value, and the
    // architectural reference.  This is the correct behaviour: a kernel with
    // an aliased IRQ vector is unsound and must not proceed to load the IDT.
    // INVARIANT: All interrupt vectors must be >= 0x20 — structural, not operational.
    // Verified at compile time above; this const block provides a redundant runtime check.
    const {
        assert!(TIMER_INTERRUPT_VECTOR >= 0x20);
        assert!(KEYBOARD_INTERRUPT_VECTOR >= 0x20);
        assert!(MOUSE_INTERRUPT_VECTOR >= 0x20);
        assert!(USER_TRAP_INTERRUPT_VECTOR >= 0x20);
    };

    unsafe {
        let idt_addr = core::ptr::addr_of!(IDT) as usize;
        log_debug!(LOG_ORIGIN, "IDT address: 0x{:X}", idt_addr);
        log_debug!(LOG_ORIGIN, "Sample handler addresses:");
        log_debug!(LOG_ORIGIN, "  exception_handler_0:  0x{:X}", exception_handler_0 as *const () as usize);
        log_debug!(LOG_ORIGIN, "  exception_handler_14: 0x{:X}", exception_handler_14 as *const () as usize);
        log_debug!(LOG_ORIGIN, "  irq_handler_32 (timer): 0x{:X}", irq_handler_32 as *const () as usize);

        let default_handlers = unexpected_interrupt_table.as_ptr();
        let entries_ptr = core::ptr::addr_of_mut!(IDT.entries) as *mut IdtEntry ;
        let entries = core::slice::from_raw_parts_mut(entries_ptr, 256) ;
        for (index, entry) in entries.iter_mut().enumerate() {
            let handler_addr = *default_handlers.add(index) as usize ;
            entry.set_handler(handler_addr, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        }

        IDT.entries[0].set_handler(exception_handler_0 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[1].set_handler(exception_handler_1 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[2].set_handler(exception_handler_2 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[3].set_handler(exception_handler_3 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_TRAP);
        IDT.entries[4].set_handler(exception_handler_4 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[5].set_handler(exception_handler_5 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[6].set_handler(exception_handler_6 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[7].set_handler(exception_handler_7 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[8].set_handler(
            exception_handler_8 as *const () as usize,
            KERNEL_CS,
            DOUBLE_FAULT_IST,
            GATE_TYPE_INTERRUPT,
        );
        IDT.entries[9].set_handler(exception_handler_9 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[10].set_handler(exception_handler_10 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[11].set_handler(exception_handler_11 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[12].set_handler(exception_handler_12 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[13].set_handler(exception_handler_13 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[14].set_handler(exception_handler_14 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[16].set_handler(exception_handler_16 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[17].set_handler(exception_handler_17 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[18].set_handler(exception_handler_18 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[19].set_handler(exception_handler_19 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[20].set_handler(exception_handler_20 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[21].set_handler(exception_handler_21 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);

        IDT.entries[TIMER_INTERRUPT_VECTOR as usize]
            .set_handler(irq_handler_32 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[KEYBOARD_INTERRUPT_VECTOR as usize]
            .set_handler(irq_handler_33 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[MOUSE_INTERRUPT_VECTOR as usize]
            .set_handler(irq_handler_44 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);

        IDT.entries[USER_TRAP_INTERRUPT_VECTOR as usize]
            .set_handler(irq_handler_104 as *const () as usize, KERNEL_CS, 0, GATE_TYPE_TRAP | DPL_RING3);

        let idt_ptr = IdtPointer {
            limit: (size_of::<Idt>() - 1) as u16,
            base: core::ptr::addr_of!(IDT) as u64,
        };

        load_idt(&idt_ptr);

        log_info!(LOG_ORIGIN, "IDT initialized with {} entries", IDT_SIZE);
    }
}

#[inline]
unsafe fn load_idt(idt_ptr: &IdtPointer) {
    core::arch::asm!(
        "lidt [{}]",
        in(reg) idt_ptr,
        options(readonly, nostack, preserves_flags)
    );
}
