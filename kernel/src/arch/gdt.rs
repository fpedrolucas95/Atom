// Global Descriptor Table (GDT) and Task State Segment (TSS)
//
// Provides x86_64 segmentation and task-state initialization for the kernel.
// Although segmentation is largely unused in long mode, the GDT remains
// mandatory for defining privilege levels and loading the TSS.
//
// Key responsibilities:
// - Define kernel and user code/data segments with correct privilege levels
// - Set up a 64-bit Task State Segment (TSS) for stack switching on interrupts
// - Install the GDT in the CPU using `lgdt` and reload segment registers
// - Load the TSS selector into the task register (`ltr`)
//
// Design and implementation details:
// - Uses `#[repr(C, packed)]` to match the exact hardware-defined layouts
// - GDT entries are raw 64-bit descriptors encoded manually
// - TSS descriptor spans two GDT entries (low/high), as required in x86_64
// - Static mutable GDT and TSS are used, requiring careful `unsafe` access
// - Kernel and user selectors are predefined and reused across the kernel
//
// Security and correctness notes:
// - User segments are marked with DPL=3, kernel segments with DPL=0
// - The TSS defines `rsp0`, ensuring safe stack switching on privilege changes
// - The I/O permission bitmap is disabled by setting `iomap_base` past the TSS
// - Correct GDT/TSS setup is critical for interrupt handling and isolation

#![allow(dead_code)]

use core::mem::size_of;

const DOUBLE_FAULT_IST_INDEX: usize = 0;
const DOUBLE_FAULT_STACK_SIZE: usize = 4096;

#[repr(align(16))]
struct AlignedStack([u8; DOUBLE_FAULT_STACK_SIZE]);

#[repr(C, packed)]
struct DescriptorTablePointer {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
#[derive(Default)]
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

const GDT_KERNEL_CODE: u64 = 0x00AF9A000000FFFF;
const GDT_KERNEL_DATA: u64 = 0x00AF92000000FFFF;
const GDT_USER_CODE: u64   = 0x00AFFA000000FFFF;
const GDT_USER_DATA: u64   = 0x00AFF2000000FFFF;

pub const KERNEL_CODE_SELECTOR: u16 = 0x08;
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;
pub const USER_CODE_SELECTOR: u16   = 0x18 | 3;
pub const USER_DATA_SELECTOR: u16   = 0x20 | 3;
pub const TSS_SELECTOR: u16         = 0x28;

#[repr(C, align(16))]
struct Gdt {
    entries: [u64; 7],
}

static mut GDT: Gdt = Gdt {
    entries: [
        0,
        GDT_KERNEL_CODE,
        GDT_KERNEL_DATA,
        GDT_USER_CODE,
        GDT_USER_DATA,
        0,
        0,
    ],
};

static mut DOUBLE_FAULT_STACK: AlignedStack = AlignedStack([0; DOUBLE_FAULT_STACK_SIZE]);
static mut TSS: Tss = Tss {
    _reserved_0: 0,
    rsp0: 0,
    rsp1: 0,
    rsp2: 0,
    _reserved_1: 0,
    ist: [0; 7],
    _reserved_2: 0,
    _reserved_3: 0,
    iomap_base: 0,
};

pub fn init(tss_rsp0: u64) {
    unsafe {
        TSS.rsp0 = tss_rsp0 & !0xF;

        TSS.ist[DOUBLE_FAULT_IST_INDEX] = double_fault_stack_top();
        TSS.iomap_base = size_of::<Tss>() as u16;

        write_tss_descriptor();
        load_gdt_and_segments();
        load_tr();
    }
}

unsafe fn write_tss_descriptor() {
    let tss_addr = core::ptr::addr_of!(TSS) as u64;
    let limit = (size_of::<Tss>() - 1) as u64;

    let low = limit & 0xFFFF
        | ((tss_addr & 0xFFFFFF) << 16)
        | (0x89u64 << 40)
        | ((limit & 0xF0000) << 32)
        | ((tss_addr & 0xFF000000) << 32);

    let high = tss_addr >> 32;

    GDT.entries[5] = low;
    GDT.entries[6] = high;
}

unsafe fn load_gdt_and_segments() {
    let ptr = DescriptorTablePointer {
        limit: (size_of::<Gdt>() - 1) as u16,
        base: (&raw const GDT) as u64,
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
    unsafe {
        TSS.rsp0 = rsp0 & !0xF;
    }
}

unsafe fn double_fault_stack_top() -> u64 {
    let stack_ptr = core::ptr::addr_of!(DOUBLE_FAULT_STACK) as *const u8;
    stack_ptr.add(DOUBLE_FAULT_STACK_SIZE) as u64
}