#![allow(dead_code)]

use core::mem::size_of;

const DOUBLE_FAULT_IST_INDEX: usize = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 16384;

#[repr(align(16))]
#[derive(Clone, Copy)]
struct AlignedStack([u8; DOUBLE_FAULT_STACK_SIZE]);

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Tss {
    _reserved_0: u32,
    pub rsp0: u64,
    rsp1: u64,
    rsp2: u64,
    _reserved_1: u64,
    ist: [u64; 7],
    _reserved_2: u64,
    _reserved_3: u16,
    pub iomap_base: u16,
}

impl Tss {
    const fn new() -> Self {
        Self {
            _reserved_0: 0,
            rsp0: 0,
            rsp1: 0,
            rsp2: 0,
            _reserved_1: 0,
            ist: [0; 7],
            _reserved_2: 0,
            _reserved_3: 0,
            iomap_base: 0,
        }
    }
}

const GDT_KERNEL_CODE: u64 = 0x00AF9A000000FFFF;
const GDT_KERNEL_DATA: u64 = 0x00AF92000000FFFF;
const GDT_USER_CODE: u64 = 0x00AFFA000000FFFF;
const GDT_USER_DATA: u64 = 0x00AFF2000000FFFF;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_DATA_SELECTOR: u16 = 0x18 | 3;
pub const USER_CODE_SELECTOR: u16 = 0x20 | 3;
pub const TSS_SELECTOR: u16 = 0x28;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Gdt {
    entries: [u64; 7],
}

impl Gdt {
    const fn new() -> Self {
        Self {
            entries: [
                0,
                GDT_KERNEL_CODE,
                GDT_KERNEL_DATA,
                GDT_USER_DATA,  // 0x18 → USER_DATA_SELECTOR = 0x1B (must precede code for SYSRETQ)
                GDT_USER_CODE,  // 0x20 → USER_CODE_SELECTOR = 0x23
                0,
                0,
            ],
        }
    }
}

const MAX_CPUS: usize = crate::smp::MAX_CPUS;

static mut GDT_TABLE: [Gdt; MAX_CPUS] = [Gdt::new(); MAX_CPUS];
static mut DOUBLE_FAULT_STACKS: [AlignedStack; MAX_CPUS] =
    [AlignedStack([0; DOUBLE_FAULT_STACK_SIZE]); MAX_CPUS];
static mut TSS_TABLE: [Tss; MAX_CPUS] = [Tss::new(); MAX_CPUS];

#[inline]
fn current_cpu_index() -> usize {
    let cpu = crate::smp::current_cpu_id();
    if cpu < MAX_CPUS { cpu } else { 0 }
}

pub fn init(tss_rsp0: u64) {
    let cpu = current_cpu_index();
    init_for_cpu(cpu, tss_rsp0);
}

pub fn init_for_cpu(cpu_id: usize, tss_rsp0: u64) {
    let cpu = cpu_id.min(MAX_CPUS - 1);

    unsafe {
        let tss = &mut TSS_TABLE[cpu];
        tss.rsp0 = tss_rsp0 & !0xF;
        tss.ist[DOUBLE_FAULT_IST_INDEX] = double_fault_stack_top(cpu);
        tss.iomap_base = size_of::<Tss>() as u16;

        write_tss_descriptor(cpu);
        load_gdt_and_segments(cpu);
        load_tr();
    }
}

unsafe fn write_tss_descriptor(cpu: usize) {
    let tss_addr = core::ptr::addr_of!(TSS_TABLE[cpu]) as u64;
    let limit = (size_of::<Tss>() - 1) as u64;

    let low = limit & 0xFFFF
        | ((tss_addr & 0xFFFFFF) << 16)
        | (0x89u64 << 40)
        | ((limit & 0xF0000) << 32)
        | ((tss_addr & 0xFF000000) << 32);

    let high = tss_addr >> 32;

    GDT_TABLE[cpu].entries[5] = low;
    GDT_TABLE[cpu].entries[6] = high;
}

unsafe fn load_gdt_and_segments(cpu: usize) {
    let ptr = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: core::ptr::addr_of!(GDT_TABLE[cpu]) as u64,
    };

    core::arch::asm!(
        "lgdt [{gdt_ptr}]",
        "push {code}",
        "lea {tmp}, [rip + 2f]",
        "push {tmp}",
        "retfq",
        "2:",
        "mov ax, {data}",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        gdt_ptr = in(reg) &ptr,
        code = const KERNEL_CODE_SELECTOR,
        data = const KERNEL_DATA_SELECTOR,
        tmp = lateout(reg) _,
        options(preserves_flags)
    );
}

unsafe fn load_tr() {
    core::arch::asm!("ltr ax", in("ax") TSS_SELECTOR, options(nostack, preserves_flags));
}

pub fn set_rsp0(rsp0: u64) {
    set_rsp0_for_cpu(current_cpu_index(), rsp0);
}

pub fn set_rsp0_for_cpu(cpu_id: usize, rsp0: u64) {
    let cpu = cpu_id.min(MAX_CPUS - 1);
    unsafe {
        TSS_TABLE[cpu].rsp0 = rsp0 & !0xF;
    }
}

unsafe fn double_fault_stack_top(cpu: usize) -> u64 {
    // DOUBLE_FAULT_STACKS is a static in BSS, already at a higher-half virtual address.
    // Adding HIGHER_HALF_BASE again would produce a non-canonical address → #GP.
    let stack_ptr = core::ptr::addr_of!(DOUBLE_FAULT_STACKS[cpu]) as u64;
    stack_ptr + DOUBLE_FAULT_STACK_SIZE as u64
}
