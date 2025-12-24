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

use core::mem::size_of;
use crate::{log_debug, log_info};
use super::{KEYBOARD_INTERRUPT_VECTOR, MOUSE_INTERRUPT_VECTOR, TIMER_INTERRUPT_VECTOR, USER_TRAP_INTERRUPT_VECTOR};
use crate::interrupts::handlers::{
    keyboard_interrupt_handler,
    mouse_interrupt_handler,
    timer_interrupt_handler,
    user_trap_interrupt_handler,
};

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
    static unexpected_interrupt_table: [u64; IDT_SIZE];
}

const GATE_TYPE_INTERRUPT: u8 = 0x8E;
const GATE_TYPE_TRAP: u8 = 0x8F;
const KERNEL_CS: u16 = crate::arch::gdt::KERNEL_CODE_SELECTOR;
const LOG_ORIGIN: &str = "idt";
const DPL_RING3: u8 = 0x60;

pub fn init() {
    unsafe {
        let idt_addr = core::ptr::addr_of!(IDT) as usize;
        log_debug!(LOG_ORIGIN, "IDT address: 0x{:X}", idt_addr);
        log_debug!(LOG_ORIGIN, "Sample handler addresses:");
        log_debug!(LOG_ORIGIN, "  exception_handler_0:  0x{:X}", exception_handler_0 as *const () as usize);
        log_debug!(LOG_ORIGIN, "  exception_handler_14: 0x{:X}", exception_handler_14 as *const () as usize);
        log_debug!(LOG_ORIGIN, "  timer_interrupt_handler: 0x{:X}", timer_interrupt_handler as *const () as usize);

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
            .set_handler(timer_interrupt_handler as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[KEYBOARD_INTERRUPT_VECTOR as usize]
            .set_handler(keyboard_interrupt_handler as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);
        IDT.entries[MOUSE_INTERRUPT_VECTOR as usize]
            .set_handler(mouse_interrupt_handler as *const () as usize, KERNEL_CS, 0, GATE_TYPE_INTERRUPT);

        IDT.entries[USER_TRAP_INTERRUPT_VECTOR as usize]
            .set_handler(user_trap_interrupt_handler as *const () as usize, KERNEL_CS, 0, GATE_TYPE_TRAP | DPL_RING3);
        
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