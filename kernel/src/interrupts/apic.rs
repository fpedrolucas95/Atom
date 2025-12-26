// Advanced Programmable Interrupt Controller (APIC) Support
//
// Implements interrupt controller initialization and management for x86_64.
// This module configures and operates the Local APIC and I/O APIC when
// available, falling back to the legacy PIC and PIT when necessary.

use super::{KEYBOARD_INTERRUPT_VECTOR, MOUSE_INTERRUPT_VECTOR, TIMER_INTERRUPT_VECTOR};
use crate::{log_debug, log_info, log_warn};

#[allow(dead_code)]
const APIC_BASE: u64 = 0xFEE00000;
const APIC_ID: u32 = 0x20;
const APIC_VERSION: u32 = 0x30;
const APIC_TPR: u32 = 0x80;
const APIC_EOI: u32 = 0xB0;
const APIC_SPURIOUS: u32 = 0xF0;
const APIC_LVT_TIMER: u32 = 0x320;
const APIC_TIMER_INIT: u32 = 0x380;
#[allow(dead_code)]
const APIC_TIMER_CURRENT: u32 = 0x390;
const APIC_TIMER_DIV: u32 = 0x3E0;
const APIC_SW_ENABLE: u32 = 0x100;

const IOAPIC_BASE: u64 = 0xFEC00000;
const IOAPIC_IOREGSEL: u32 = 0x00;
const IOAPIC_IOWIN: u32 = 0x10;

const TIMER_MODE_PERIODIC: u32 = 0x20000;

static mut APIC_VIRT_BASE: u64 = APIC_BASE;
static mut IOAPIC_VIRT_BASE: u64 = IOAPIC_BASE;
static mut APIC_ENABLED: bool = false;
static mut PIC_ACTIVE: bool = false;

/* ---------------- APIC MMIO helpers ---------------- */

#[inline]
unsafe fn apic_read(offset: u32) -> u32 {
    let addr = (APIC_VIRT_BASE + offset as u64) as *const u32;
    core::ptr::read_volatile(addr)
}

#[inline]
unsafe fn apic_write(offset: u32, value: u32) {
    let addr = (APIC_VIRT_BASE + offset as u64) as *mut u32;
    core::ptr::write_volatile(addr, value);
}

#[allow(dead_code)]
pub fn dump_apic_state() {
    unsafe {
        let isr = apic_read(0x100);
        let irr = apic_read(0x200);
        let tpr = apic_read(APIC_TPR);

        log_debug!("apic", "APIC State Dump:");
        log_debug!("apic", "  ISR[31:0]   = 0x{:08X}", isr);
        log_debug!("apic", "  IRR[31:0]   = 0x{:08X}", irr);
        log_debug!("apic", "  TPR         = 0x{:08X}", tpr);
        log_debug!("apic", "  LVT Timer   = 0x{:08X}", apic_read(APIC_LVT_TIMER));
    }
}

#[inline]
unsafe fn ioapic_write(index: u32, value: u32) {
    let sel = (IOAPIC_VIRT_BASE + IOAPIC_IOREGSEL as u64) as *mut u32;
    let win = (IOAPIC_VIRT_BASE + IOAPIC_IOWIN as u64) as *mut u32;
    core::ptr::write_volatile(sel, index);
    core::ptr::write_volatile(win, value);
}

#[inline]
unsafe fn ioapic_read(index: u32) -> u32 {
    let sel = (IOAPIC_VIRT_BASE + IOAPIC_IOREGSEL as u64) as *mut u32;
    let win = (IOAPIC_VIRT_BASE + IOAPIC_IOWIN as u64) as *mut u32;
    core::ptr::write_volatile(sel, index);
    core::ptr::read_volatile(win)
}

/* ---------------- APIC detection ---------------- */

fn is_apic_supported() -> bool {
    unsafe {
        let edx: u32;
        core::arch::asm!(
        "push rbx",
        "mov eax, 1",
        "cpuid",
        "pop rbx",
        out("eax") _,
        out("ecx") _,
        out("edx") edx,
        );
        (edx & (1 << 9)) != 0
    }
}

fn get_apic_base() -> u64 {
    unsafe {
        let mut low: u32;
        let mut high: u32;
        core::arch::asm!(
        "rdmsr",
        in("ecx") 0x1B_u32,
        out("eax") low,
        out("edx") high,
        );
        ((high as u64) << 32) | (low as u64)
    }
}

unsafe fn enable_apic() {
    let spurious = apic_read(APIC_SPURIOUS);
    apic_write(APIC_SPURIOUS, spurious | APIC_SW_ENABLE | 0xFF);
    apic_write(APIC_TPR, 0);
}

/* ---------------- PIC definitions ---------------- */

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;

/* ---------------- Public initialization ---------------- */

pub fn init() {
    const LOG_ORIGIN: &str = "apic";

    if !is_apic_supported() {
        log_warn!(LOG_ORIGIN, "APIC not supported, falling back to PIC");
        init_pic(true);
        return;
    }

    log_info!(LOG_ORIGIN, "Initializing Local APIC");

    let apic_base = get_apic_base() & 0xFFFFFF000;

    unsafe {
        APIC_VIRT_BASE = apic_base;
        enable_apic();

        let id = apic_read(APIC_ID) >> 24;
        let version = apic_read(APIC_VERSION) & 0xFF;

        log_debug!(LOG_ORIGIN, "APIC ID: {}", id);
        log_debug!(LOG_ORIGIN, "APIC version: {:#X}", version);

        APIC_ENABLED = true;
    }

    log_info!(LOG_ORIGIN, "Local APIC initialized");
    log_info!(LOG_ORIGIN, "Initializing I/O APIC");

    unsafe {
        outb(0x22, 0x70);
        io_wait();
        let val = inb(0x23);
        io_wait();
        outb(0x23, val | 0x01);
        io_wait();

        log_info!(
            LOG_ORIGIN,
            "IMCR set: ISA IRQs routed to APIC/IOAPIC (val=0x{:02X})",
            val | 0x01
        );

        IOAPIC_VIRT_BASE = IOAPIC_BASE;

        ioapic_write(0x12, KEYBOARD_INTERRUPT_VECTOR as u32);
        ioapic_write(0x13, 0x0000_0000);
        
        ioapic_write(0x28, MOUSE_INTERRUPT_VECTOR as u32); 
        ioapic_write(0x29, 0x0000_0000);

        let redtbl_low = ioapic_read(0x28);
        let redtbl_high = ioapic_read(0x29);
        log_info!(LOG_ORIGIN, "IRQ12 I/O APIC redtbl[12]: low=0x{:08X}, high=0x{:08X}", redtbl_low, redtbl_high);
        if (redtbl_low & 0x10000) != 0 {
            log_warn!(LOG_ORIGIN, "IRQ12 está MASCARADO no I/O APIC! (bit16=1)");
        } else {
            log_info!(LOG_ORIGIN, "IRQ12 NÃO mascarado (bit16=0), vetor={}", redtbl_low & 0xFF);
        }

        crate::serial_println!("[TRACE] About to mask IRQ10 via ioapic_write(0x14)...");
        ioapic_write(0x14, 0x0001_0000);
        crate::serial_println!("[TRACE] ioapic_write(0x14) done");
        ioapic_write(0x15, 0x0000_0000);
        crate::serial_println!("[TRACE] ioapic_write(0x15) done");
    }

    crate::serial_println!("[TRACE] About to call disable_legacy_pic()...");
    unsafe { disable_legacy_pic(); }

    log_info!(LOG_ORIGIN, "APIC subsystem initialized (PIC disabled)");
}

#[allow(dead_code)]
unsafe fn enable_imcr_ioapic_routing() {
    const IMCR_ADDR: u16 = 0x22;
    const IMCR_DATA: u16 = 0x23;
    const IMCR_SELECT: u8 = 0x70;

    outb(IMCR_ADDR, IMCR_SELECT);
    io_wait();

    let current = inb(IMCR_DATA);
    outb(IMCR_DATA, current | 0x01);
    io_wait();

    log_info!("apic", "IMCR set: ISA IRQs routed to APIC/IOAPIC (val={:#04X})", current | 0x01);
}

unsafe fn disable_legacy_pic() {
    const LOG_ORIGIN: &str = "apic";

    crate::serial_println!("[TRACE] disable_legacy_pic: step 1 - ICW1");
    outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
    io_wait();
    outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
    io_wait();

    crate::serial_println!("[TRACE] disable_legacy_pic: step 2 - ICW2 (remap)");
    outb(PIC1_DATA, 0x20);
    io_wait();
    outb(PIC2_DATA, 0x28);
    io_wait();

    crate::serial_println!("[TRACE] disable_legacy_pic: step 3 - ICW3 (cascade)");
    outb(PIC1_DATA, 4);
    io_wait();
    outb(PIC2_DATA, 2);
    io_wait();

    crate::serial_println!("[TRACE] disable_legacy_pic: step 4 - ICW4");
    outb(PIC1_DATA, ICW4_8086);
    io_wait();
    outb(PIC2_DATA, ICW4_8086);
    io_wait();

    crate::serial_println!("[TRACE] disable_legacy_pic: step 5 - mask all");
    outb(PIC1_DATA, 0xFF);
    outb(PIC2_DATA, 0xFF);

    crate::serial_println!("[TRACE] disable_legacy_pic: done");
    PIC_ACTIVE = false;

    log_info!(
        LOG_ORIGIN,
        "Legacy PIC fully disabled (remapped + masked)"
    );
}

/* ---------------- EOI handling ---------------- */

pub fn send_eoi() {
    unsafe {
        if APIC_ENABLED {
            let eoi = (APIC_VIRT_BASE + APIC_EOI as u64) as *mut u32;
            core::ptr::write_volatile(eoi, 0);
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        } else if PIC_ACTIVE {
            send_pic_eoi();
        }
    }
}

/* ---------------- Timers ---------------- */

pub fn init_timer(frequency_hz: u32) {
    unsafe {
        if APIC_ENABLED {
            init_apic_timer(frequency_hz);
        } else {
            init_pit_timer(frequency_hz);
        }
    }
}

unsafe fn init_apic_timer(_frequency_hz: u32) {
    apic_write(APIC_TIMER_DIV, 0x3);
    apic_write(
        APIC_LVT_TIMER,
        (TIMER_INTERRUPT_VECTOR as u32) | TIMER_MODE_PERIODIC,
    );
    apic_write(APIC_TIMER_INIT, 10_000_000);
}

/* ---------------- PIC fallback ---------------- */

fn init_pic(enable_timer_irq: bool) {
    const LOG_ORIGIN: &str = "apic";

    log_info!(LOG_ORIGIN, "Initializing legacy PIC");

    unsafe {
        outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();
        outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
        io_wait();

        outb(PIC1_DATA, 32);
        io_wait();
        outb(PIC2_DATA, 40);
        io_wait();

        outb(PIC1_DATA, 4);
        io_wait();
        outb(PIC2_DATA, 2);
        io_wait();

        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        let mut mask1 = 0xFF;
        if enable_timer_irq {
            mask1 &= !0x01;
        }

        outb(PIC1_DATA, mask1);
        outb(PIC2_DATA, 0xFF);

        PIC_ACTIVE = true;
    }
}

unsafe fn send_pic_eoi() {
    outb(PIC1_CMD, 0x20);
}

/* ---------------- PIT ---------------- */

unsafe fn init_pit_timer(frequency_hz: u32) {
    let freq = frequency_hz.max(1);
    let divisor = 1_193_182 / freq;

    outb(0x43, 0x36);
    outb(0x40, (divisor & 0xFF) as u8);
    outb(0x40, (divisor >> 8) as u8);
}

/* ---------------- Port I/O ---------------- */

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port);
    value
}

#[inline]
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value);
}

#[inline]
unsafe fn io_wait() {
    outb(0x80, 0);
}

/* ---------------- Interrupt control ---------------- */

pub fn enable_interrupts() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

#[allow(dead_code)]
pub fn disable_interrupts() {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}
