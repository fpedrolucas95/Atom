// kernel/src/interrupts/apic.rs
// Advanced Programmable Interrupt Controller (APIC) support.
//
// Drives the interrupt controllers on x86_64 and is written to scale from a
// single core up to a many-core SMP machine:
//
//   * The Local APIC is configured per-CPU.  The boot CPU (BSP) calls `init()`;
//     every application processor (AP) calls `init_local_current_cpu()` during
//     SMP bring-up.  The shared global state (controller mode, MMIO base) is
//     written once on the BSP and only read afterwards, so APs need no extra
//     synchronization beyond the boot barrier that already orders SIPI delivery.
//
//   * Both Local APIC access modes are supported behind a single abstraction:
//       - xAPIC  : 32-bit registers via memory-mapped I/O at 0xFEE00000.
//       - x2APIC : the same logical registers via MSRs 0x800..0x83F.
//     x2APIC is selected automatically when the CPU advertises it
//     (CPUID.01H:ECX[21]).  It is the architecturally recommended path on large
//     systems: it removes the ICR delivery-status poll and exposes 32-bit APIC
//     IDs, which matter once a machine has many logical processors.
//
//   * When no APIC is present the module falls back to the legacy 8259A PIC and
//     the 8254 PIT.
//
// No `static mut` is used.  All mutable global state lives in atomics, which is
// both sound under SMP and accepted by the Rust 2024 edition (which rejects
// references to `static mut`).
//
// Reference: Intel(R) 64 and IA-32 Architectures Software Developer's Manual,
// Vol. 3A, Chapter 11 "Advanced Programmable Interrupt Controller (APIC)".

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

use super::{
    KEYBOARD_INTERRUPT_VECTOR,
    MOUSE_INTERRUPT_VECTOR,
    RESCHEDULE_INTERRUPT_VECTOR,
    TIMER_INTERRUPT_VECTOR,
};
use crate::{log_debug, log_info, log_warn};

const LOG_ORIGIN: &str = "apic";

/* ------------------------------------------------------------------ */
/*  IA32_APIC_BASE MSR                                                 */
/* ------------------------------------------------------------------ */

const IA32_APIC_BASE_MSR: u32 = 0x1B;
const APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10; // EXTD
const APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11; // EN
const APIC_BASE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/* ------------------------------------------------------------------ */
/*  Local APIC register offsets (xAPIC MMIO byte offsets).            */
/*  In x2APIC mode the matching MSR is 0x800 + (offset >> 4).          */
/* ------------------------------------------------------------------ */

const REG_ID: u32 = 0x020;
const REG_VERSION: u32 = 0x030;
const REG_TPR: u32 = 0x080;
const REG_EOI: u32 = 0x0B0;
const REG_SPURIOUS: u32 = 0x0F0;
const REG_ISR0: u32 = 0x100;
const REG_IRR0: u32 = 0x200;
const REG_ICR_LOW: u32 = 0x300;
const REG_ICR_HIGH: u32 = 0x310;
const REG_LVT_TIMER: u32 = 0x320;
const REG_TIMER_INIT: u32 = 0x380;
const REG_TIMER_CURRENT: u32 = 0x390;
const REG_TIMER_DIV: u32 = 0x3E0;

const SPURIOUS_VECTOR: u32 = 0xFF;
const SPURIOUS_SW_ENABLE: u32 = 1 << 8; // APIC software enable
const LVT_MASKED: u32 = 1 << 16;
const TIMER_MODE_PERIODIC: u32 = 1 << 17; // 0x20000
const ICR_DELIVERY_STATUS: u32 = 1 << 12;

/* x2APIC MSR base + the single 64-bit ICR MSR. */
const X2APIC_MSR_BASE: u32 = 0x800;
const X2APIC_ICR_MSR: u32 = 0x830;

/* ------------------------------------------------------------------ */
/*  I/O APIC                                                           */
/* ------------------------------------------------------------------ */

const IOAPIC_PHYS_BASE: u64 = 0xFEC0_0000;
const IOAPIC_IOREGSEL: u32 = 0x00;
const IOAPIC_IOWIN: u32 = 0x10;
const IOAPIC_REDTBL_BASE: u32 = 0x10; // entry N => regs (BASE + 2N, BASE + 2N + 1)

const REDIR_DEST_PHYSICAL: u32 = 0; // dest mode bit 11 = 0
const REDIR_ACTIVE_LOW: u32 = 1 << 13;
const REDIR_LEVEL_TRIGGER: u32 = 1 << 15;
const REDIR_MASKED: u32 = 1 << 16;

// Legacy ISA -> Global System Interrupt mapping (no ACPI override parsed).
// On real hardware the MADT may override these; see the note in `init()`.
const GSI_PIT: u32 = 2; // IRQ0 is usually remapped to GSI2
const GSI_KEYBOARD: u32 = 1; // IRQ1
const GSI_MOUSE: u32 = 12; // IRQ12

/* ------------------------------------------------------------------ */
/*  Global, set-once state (no `static mut`).                          */
/* ------------------------------------------------------------------ */

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum ApicMode {
    Disabled = 0,
    XApic = 1,
    X2Apic = 2,
}

static APIC_MODE: AtomicU8 = AtomicU8::new(ApicMode::Disabled as u8);
static XAPIC_BASE: AtomicU64 = AtomicU64::new(0);
static IOAPIC_BASE: AtomicU64 = AtomicU64::new(IOAPIC_PHYS_BASE);
static PIC_ACTIVE: AtomicBool = AtomicBool::new(false);
static TIMER_CALIBRATED_COUNT: AtomicU32 = AtomicU32::new(0);

#[inline]
fn mode() -> ApicMode {
    match APIC_MODE.load(Ordering::Acquire) {
        1 => ApicMode::XApic,
        2 => ApicMode::X2Apic,
        _ => ApicMode::Disabled,
    }
}

#[inline]
fn set_mode(m: ApicMode) {
    APIC_MODE.store(m as u8, Ordering::Release);
}

/* ------------------------------------------------------------------ */
/*  Low-level CPU access primitives                                    */
/* ------------------------------------------------------------------ */

#[inline]
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo,
        out("edx") hi,
        options(nomem, nostack, preserves_flags),
    );
    ((hi as u64) << 32) | (lo as u64)
}

#[inline]
unsafe fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo,
        in("edx") hi,
        options(nomem, nostack, preserves_flags),
    );
}

/// `cpuid` wrapper. RBX is reserved by LLVM on x86_64, so it is saved/restored
/// around the instruction (the canonical inline-asm idiom).
#[inline]
fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    let eax: u32;
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "mov {ebx_out:e}, ebx",
            "pop rbx",
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            ebx_out = out(reg) ebx,
            options(preserves_flags),
        );
    }
    (eax, ebx, ecx, edx)
}

#[inline]
fn apic_supported() -> bool {
    let (_, _, _, edx) = cpuid(1, 0);
    (edx & (1 << 9)) != 0
}

#[inline]
fn x2apic_supported() -> bool {
    let (_, _, ecx, _) = cpuid(1, 0);
    (ecx & (1 << 21)) != 0
}

/* ------------------------------------------------------------------ */
/*  Local APIC register access (mode-aware)                            */
/* ------------------------------------------------------------------ */

#[inline]
unsafe fn lapic_read(reg: u32) -> u32 {
    match mode() {
        ApicMode::X2Apic => rdmsr(X2APIC_MSR_BASE + (reg >> 4)) as u32,
        _ => {
            let base = XAPIC_BASE.load(Ordering::Relaxed);
            core::ptr::read_volatile((base + reg as u64) as *const u32)
        }
    }
}

#[inline]
unsafe fn lapic_write(reg: u32, value: u32) {
    match mode() {
        ApicMode::X2Apic => wrmsr(X2APIC_MSR_BASE + (reg >> 4), value as u64),
        _ => {
            let base = XAPIC_BASE.load(Ordering::Relaxed);
            core::ptr::write_volatile((base + reg as u64) as *mut u32, value);
        }
    }
}

/// Read the Local APIC ID of the executing CPU.
///
/// xAPIC stores the 8-bit ID in bits 31:24; x2APIC exposes the full 32-bit ID
/// directly in MSR 0x802.
#[inline]
pub fn local_apic_id() -> u32 {
    unsafe {
        match mode() {
            ApicMode::X2Apic => lapic_read(REG_ID),
            ApicMode::XApic => lapic_read(REG_ID) >> 24,
            ApicMode::Disabled => 0,
        }
    }
}

/* ------------------------------------------------------------------ */
/*  I/O APIC access                                                    */
/* ------------------------------------------------------------------ */

#[inline]
unsafe fn ioapic_write(index: u32, value: u32) {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    let sel = (base + IOAPIC_IOREGSEL as u64) as *mut u32;
    let win = (base + IOAPIC_IOWIN as u64) as *mut u32;
    core::ptr::write_volatile(sel, index);
    core::ptr::write_volatile(win, value);
}

#[inline]
#[allow(dead_code)]
unsafe fn ioapic_read(index: u32) -> u32 {
    let base = IOAPIC_BASE.load(Ordering::Relaxed);
    let sel = (base + IOAPIC_IOREGSEL as u64) as *mut u32;
    let win = (base + IOAPIC_IOWIN as u64) as *const u32;
    core::ptr::write_volatile(sel, index);
    core::ptr::read_volatile(win)
}

/// Program one I/O APIC redirection entry.
///
/// `gsi`       Global System Interrupt (== redirection table index).
/// `vector`    IDT vector to deliver (must be >= 0x20).
/// `dest_apic` destination Local APIC ID (physical destination mode).
/// `level`/`active_low` ISA edge/high by default; set for level-triggered lines.
unsafe fn ioapic_set_redirect(
    gsi: u32,
    vector: u8,
    dest_apic: u32,
    masked: bool,
    level: bool,
    active_low: bool,
) {
    let low_index = IOAPIC_REDTBL_BASE + gsi * 2;
    let high_index = low_index + 1;

    let mut low = vector as u32 | REDIR_DEST_PHYSICAL;
    if active_low {
        low |= REDIR_ACTIVE_LOW;
    }
    if level {
        low |= REDIR_LEVEL_TRIGGER;
    }
    if masked {
        low |= REDIR_MASKED;
    }

    // Destination APIC ID lives in bits 56:63 of the 64-bit entry => bits 24:31
    // of the high 32-bit register. The legacy I/O APIC only carries an 8-bit
    // destination; systems with > 255 logical CPUs require interrupt remapping.
    ioapic_write(high_index, (dest_apic & 0xFF) << 24);
    ioapic_write(low_index, low);
}

/* ------------------------------------------------------------------ */
/*  Legacy 8259A PIC                                                   */
/* ------------------------------------------------------------------ */

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
const ICW4_8086: u8 = 0x01;

const PIT_FREQ: u32 = 1_193_182; // 8254 base oscillator (Hz)

/* ------------------------------------------------------------------ */
/*  Public initialization                                              */
/* ------------------------------------------------------------------ */

/// Initialize the interrupt controllers on the boot CPU.
pub fn init() {
    if !apic_supported() {
        log_warn!(LOG_ORIGIN, "APIC not reported by CPUID; falling back to PIC + PIT");
        init_pic(true);
        return;
    }

    // 1. Bring up the boot CPU's Local APIC (selects x2APIC if available).
    init_local_current_cpu();

    // 2. Remap then fully mask the legacy PIC BEFORE unmasking I/O APIC lines,
    //    so an ISA IRQ can never be delivered through both controllers.
    unsafe {
        disable_legacy_pic();
        set_imcr_to_apic();
        configure_ioapic();
    }

    log_info!(LOG_ORIGIN, "APIC subsystem initialized (mode={})", mode_name());
}

/// Bring up the Local APIC on the *current* CPU.
///
/// Called by the BSP from `init()` and by every AP during SMP bring-up. Safe to
/// call once per CPU; the shared mode/base state is established on first call.
pub fn init_local_current_cpu() {
    unsafe {
        let mut base = rdmsr(IA32_APIC_BASE_MSR);
        base |= APIC_BASE_GLOBAL_ENABLE;

        if x2apic_supported() {
            base |= APIC_BASE_X2APIC_ENABLE;
            wrmsr(IA32_APIC_BASE_MSR, base);
            set_mode(ApicMode::X2Apic);
        } else {
            // xAPIC: the LAPIC is reachable through MMIO at this physical base.
            // We assume the memory manager has identity-/UC-mapped this page;
            // see the module-level note. The same physical base addresses each
            // CPU's *own* LAPIC, so a single global value is correct under SMP.
            let phys = base & APIC_BASE_ADDR_MASK;
            XAPIC_BASE.store(phys, Ordering::Relaxed);
            wrmsr(IA32_APIC_BASE_MSR, base);
            set_mode(ApicMode::XApic);
        }

        // Spurious-interrupt vector register: set the vector and the software
        // enable bit. Then accept every priority class (TPR = 0).
        lapic_write(REG_SPURIOUS, SPURIOUS_SW_ENABLE | SPURIOUS_VECTOR);
        lapic_write(REG_TPR, 0);

        let id = local_apic_id();
        let version = lapic_read(REG_VERSION) & 0xFF;
        log_debug!(LOG_ORIGIN, "LAPIC up: id={} version={:#X} mode={}", id, version, mode_name());
    }

    log_info!(LOG_ORIGIN, "Local APIC online (id={})", local_apic_id());
}

/// Route the legacy ISA lines we care about to the boot CPU. A full driver
/// would parse the ACPI MADT for I/O APIC base(s) and Interrupt Source
/// Overrides; here we use the conventional ISA->GSI identity (with IRQ0->GSI2)
/// and target the BSP. Other CPUs receive interrupts via IPIs / LAPIC timer.
unsafe fn configure_ioapic() {
    let dest = local_apic_id();

    // The periodic tick comes from the LAPIC timer, so keep the PIT line masked.
    ioapic_set_redirect(GSI_PIT, 0, dest, true, false, false);

    ioapic_set_redirect(GSI_KEYBOARD, KEYBOARD_INTERRUPT_VECTOR, dest, false, false, false);
    ioapic_set_redirect(GSI_MOUSE, MOUSE_INTERRUPT_VECTOR, dest, false, false, false);

    log_info!(
        LOG_ORIGIN,
        "I/O APIC routed to LAPIC {}: kbd(gsi{})->{} mouse(gsi{})->{}",
        dest,
        GSI_KEYBOARD,
        KEYBOARD_INTERRUPT_VECTOR,
        GSI_MOUSE,
        MOUSE_INTERRUPT_VECTOR
    );
}

/// On legacy MP systems an IMCR selects whether ISA IRQs reach the PIC or the
/// I/O APIC. Setting it to "APIC mode" is harmless on systems without one.
unsafe fn set_imcr_to_apic() {
    const IMCR_ADDR: u16 = 0x22;
    const IMCR_DATA: u16 = 0x23;
    const IMCR_SELECT: u8 = 0x70;

    outb(IMCR_ADDR, IMCR_SELECT);
    io_wait();
    let cur = inb(IMCR_DATA);
    outb(IMCR_DATA, cur | 0x01);
    io_wait();
    log_debug!(LOG_ORIGIN, "IMCR set to APIC routing (val={:#04X})", cur | 0x01);
}

/// Remap the 8259A vectors out of the CPU-exception range, then mask every
/// line. This neutralizes the PIC so only the APIC delivers interrupts.
unsafe fn disable_legacy_pic() {
    outb(PIC1_CMD, ICW1_INIT | ICW1_ICW4);
    io_wait();
    outb(PIC2_CMD, ICW1_INIT | ICW1_ICW4);
    io_wait();

    outb(PIC1_DATA, 0x20); // master -> 0x20..0x27
    io_wait();
    outb(PIC2_DATA, 0x28); // slave  -> 0x28..0x2F
    io_wait();

    outb(PIC1_DATA, 4); // slave at IRQ2
    io_wait();
    outb(PIC2_DATA, 2); // cascade identity
    io_wait();

    outb(PIC1_DATA, ICW4_8086);
    io_wait();
    outb(PIC2_DATA, ICW4_8086);
    io_wait();

    outb(PIC1_DATA, 0xFF); // mask all
    outb(PIC2_DATA, 0xFF);

    PIC_ACTIVE.store(false, Ordering::Release);
    log_info!(LOG_ORIGIN, "Legacy PIC remapped and fully masked");
}

/* ------------------------------------------------------------------ */
/*  Inter-processor interrupts (SMP)                                   */
/* ------------------------------------------------------------------ */

#[inline]
fn wait_icr_idle() {
    // Only meaningful for xAPIC: x2APIC has no ICR delivery-status bit.
    while unsafe { (lapic_read(REG_ICR_LOW) & ICR_DELIVERY_STATUS) != 0 } {
        core::hint::spin_loop();
    }
}

/// Send an IPI to a specific Local APIC. `dest` is the full APIC ID
/// (8-bit in xAPIC, 32-bit in x2APIC); `icr_low` carries the delivery
/// mode / level / vector bits.
fn send_ipi(dest: u32, icr_low: u32) {
    unsafe {
        match mode() {
            ApicMode::Disabled => {
                log_warn!(LOG_ORIGIN, "Ignoring IPI in PIC mode: dest={} icr={:#X}", dest, icr_low);
            }
            ApicMode::X2Apic => {
                // Single 64-bit ICR write; destination occupies bits 63:32.
                let value = ((dest as u64) << 32) | (icr_low as u64);
                wrmsr(X2APIC_ICR_MSR, value);
            }
            ApicMode::XApic => {
                wait_icr_idle();
                lapic_write(REG_ICR_HIGH, dest << 24);
                lapic_write(REG_ICR_LOW, icr_low);
                wait_icr_idle();
            }
        }
    }
}

pub fn send_init_ipi(apic_id: u32) {
    // Delivery mode INIT (101), level assert, edge.
    send_ipi(apic_id, 0x0000_4500);
}

pub fn send_startup_ipi(apic_id: u32, vector: u8) {
    // Delivery mode STARTUP (110); the start vector forms the trampoline page.
    send_ipi(apic_id, 0x0000_4600 | (vector as u32));
}

pub fn send_reschedule_ipi(apic_id: u32) {
    // Delivery mode Fixed (000), level assert, our reschedule vector.
    send_ipi(apic_id, 0x0000_4000 | (RESCHEDULE_INTERRUPT_VECTOR as u32));
}

/* ------------------------------------------------------------------ */
/*  End Of Interrupt                                                   */
/* ------------------------------------------------------------------ */

pub fn send_eoi() {
    match mode() {
        ApicMode::Disabled => {
            if PIC_ACTIVE.load(Ordering::Acquire) {
                unsafe { send_pic_eoi() };
            }
        }
        _ => unsafe {
            lapic_write(REG_EOI, 0);
            // Ensure the EOI store is globally visible before we possibly
            // re-enable interrupts or switch contexts.
            core::arch::asm!("mfence", options(nostack, preserves_flags));
        },
    }
}

unsafe fn send_pic_eoi() {
    outb(PIC1_CMD, 0x20);
}

/* ------------------------------------------------------------------ */
/*  Timers                                                             */
/* ------------------------------------------------------------------ */

pub fn init_timer(frequency_hz: u32) {
    match mode() {
        ApicMode::Disabled => unsafe { init_pit_timer(frequency_hz) },
        _ => unsafe { init_apic_timer(frequency_hz) },
    }
}

/// Calibrate the LAPIC timer against the PIT (a known 1.193182 MHz clock) and
/// arm it as a periodic source at the requested frequency. The calibration
/// result is cached: APs reuse it directly because every LAPIC on the board is
/// driven by the same bus/crystal clock.
unsafe fn init_apic_timer(frequency_hz: u32) {
    const CALIBRATE_MS: u32 = 10;
    let freq = frequency_hz.max(1);

    // Divide-by-16.
    lapic_write(REG_TIMER_DIV, 0x3);

    // Fast path: reuse a prior calibration.
    let cached = TIMER_CALIBRATED_COUNT.load(Ordering::Acquire);
    if cached != 0 {
        lapic_write(REG_LVT_TIMER, (TIMER_INTERRUPT_VECTOR as u32) | TIMER_MODE_PERIODIC);
        lapic_write(REG_TIMER_INIT, cached);
        log_debug!(LOG_ORIGIN, "APIC timer armed from cache: init={} for {}Hz", cached, freq);
        return;
    }

    // Mask the LVT timer during calibration.
    lapic_write(REG_LVT_TIMER, LVT_MASKED);

    // Program PIT channel 2 in one-shot (mode 0) for the calibration window.
    let pit_count: u16 = (PIT_FREQ * CALIBRATE_MS / 1000) as u16; // ~11932
    let port61 = inb(0x61) & 0xFC; // gate low, speaker off
    outb(0x61, port61);
    outb(0x43, 0xB0); // channel 2, lo/hi byte, mode 0, binary
    outb(0x42, (pit_count & 0xFF) as u8);
    outb(0x42, (pit_count >> 8) as u8);

    // Start the LAPIC timer from its maximum and trigger the PIT.
    lapic_write(REG_TIMER_INIT, 0xFFFF_FFFF);
    outb(0x61, port61 | 0x01); // gate low->high starts the countdown

    // Wait for PIT OUT (bit 5 of port 0x61) to assert.
    while (inb(0x61) & 0x20) == 0 {
        core::hint::spin_loop();
    }

    let elapsed = 0xFFFF_FFFFu32.wrapping_sub(lapic_read(REG_TIMER_CURRENT));
    let ticks_per_sec = (elapsed as u64) * 1000 / (CALIBRATE_MS as u64);
    let initial = (ticks_per_sec / freq as u64) as u32;
    let final_count = if initial == 0 { 1 } else { initial };

    TIMER_CALIBRATED_COUNT.store(final_count, Ordering::Release);

    log_info!(
        LOG_ORIGIN,
        "APIC timer calibrated: ~{} ticks/s, init={} for {}Hz",
        ticks_per_sec,
        final_count,
        freq
    );

    lapic_write(REG_LVT_TIMER, (TIMER_INTERRUPT_VECTOR as u32) | TIMER_MODE_PERIODIC);
    lapic_write(REG_TIMER_INIT, final_count);
}

#[allow(dead_code)]
pub fn timer_current_count() -> u32 {
    match mode() {
        ApicMode::Disabled => 0,
        _ => unsafe { lapic_read(REG_TIMER_CURRENT) },
    }
}

/* ------------------------------------------------------------------ */
/*  PIC / PIT fallback                                                 */
/* ------------------------------------------------------------------ */

fn init_pic(enable_timer_irq: bool) {
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

        let mut mask1 = 0xFFu8;
        if enable_timer_irq {
            mask1 &= !0x01; // unmask IRQ0 (PIT)
        }
        outb(PIC1_DATA, mask1);
        outb(PIC2_DATA, 0xFF);

        PIC_ACTIVE.store(true, Ordering::Release);
    }
}

unsafe fn init_pit_timer(frequency_hz: u32) {
    let freq = frequency_hz.max(1);
    let divisor = (PIT_FREQ / freq) as u16;
    outb(0x43, 0x36); // channel 0, lo/hi, mode 3 (square wave)
    outb(0x40, (divisor & 0xFF) as u8);
    outb(0x40, (divisor >> 8) as u8);
}

/* ------------------------------------------------------------------ */
/*  Port I/O                                                           */
/* ------------------------------------------------------------------ */

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

#[inline]
unsafe fn outb(port: u16, value: u8) {
    core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags));
}

#[inline]
unsafe fn io_wait() {
    outb(0x80, 0);
}

/* ------------------------------------------------------------------ */
/*  Interrupt flag control                                             */
/* ------------------------------------------------------------------ */

pub fn enable_interrupts() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack)) };
}

#[allow(dead_code)]
pub fn disable_interrupts() {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
}

/* ------------------------------------------------------------------ */
/*  Diagnostics                                                        */
/* ------------------------------------------------------------------ */

fn mode_name() -> &'static str {
    match mode() {
        ApicMode::Disabled => "pic",
        ApicMode::XApic => "xapic",
        ApicMode::X2Apic => "x2apic",
    }
}

#[allow(dead_code)]
pub fn dump_apic_state() {
    if mode() == ApicMode::Disabled {
        log_debug!(LOG_ORIGIN, "APIC disabled (PIC mode); no state to dump");
        return;
    }
    unsafe {
        log_debug!(LOG_ORIGIN, "APIC state ({}):", mode_name());
        log_debug!(LOG_ORIGIN, "  ID        = {}", local_apic_id());
        log_debug!(LOG_ORIGIN, "  ISR[31:0] = {:#010X}", lapic_read(REG_ISR0));
        log_debug!(LOG_ORIGIN, "  IRR[31:0] = {:#010X}", lapic_read(REG_IRR0));
        log_debug!(LOG_ORIGIN, "  TPR       = {:#010X}", lapic_read(REG_TPR));
        log_debug!(LOG_ORIGIN, "  LVT Timer = {:#010X}", lapic_read(REG_LVT_TIMER));
    }
}