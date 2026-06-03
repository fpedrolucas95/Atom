#![allow(dead_code)]

use alloc::vec::Vec;
use core::mem::size_of;
use spin::Mutex;

use crate::interrupts::apic;
use crate::{log_info, log_warn};

pub const MAX_CPUS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CpuState {
    Booting = 0,
    Online = 1,
    Idle = 2,
    Offline = 3,
    Panic = 4,
}

#[derive(Clone, Copy, Debug)]
pub struct CpuRecord {
    pub apic_id: u32,
    pub state: CpuState,
    pub online: bool,
}

impl CpuRecord {
    pub const fn empty() -> Self {
        Self {
            apic_id: u32::MAX,
            state: CpuState::Offline,
            online: false,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuLocalAsm {
    pub current_kstack: u64,
    pub temp_user_rsp: u64,
    pub cpu_id: u64,
    _reserved: u64,
}

impl CpuLocalAsm {
    pub const fn new() -> Self {
        Self {
            current_kstack: 0,
            temp_user_rsp: 0,
            cpu_id: 0,
            _reserved: 0,
        }
    }
}

const LOG_ORIGIN: &str = "smp";

static CPU_TABLE: Mutex<[CpuRecord; MAX_CPUS]> = Mutex::new([CpuRecord::empty(); MAX_CPUS]);
static APIC_TO_CPU: Mutex<[u16; 256]> = Mutex::new([u16::MAX; 256]);
static CPU_COUNT: Mutex<usize> = Mutex::new(1);

#[no_mangle]
pub static mut CPU_LOCAL_ASM_STATE: [CpuLocalAsm; MAX_CPUS] = [CpuLocalAsm::new(); MAX_CPUS];

const IA32_GS_BASE: u32 = 0xC000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

#[inline]
unsafe fn write_msr(msr: u32, value: u64) {
    let low = value as u32;
    let high = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") low,
        in("edx") high,
        options(nostack, preserves_flags)
    );
}

#[inline]
fn set_kernel_gs_base(ptr: u64) {
    unsafe {
        write_msr(IA32_KERNEL_GS_BASE, ptr);
    }
}

pub fn init_cpu_local_syscall_state(cpu_id: usize, initial_kstack: u64) {
    let cpu = cpu_id.min(MAX_CPUS - 1);
    unsafe {
        CPU_LOCAL_ASM_STATE[cpu].current_kstack = initial_kstack;
        CPU_LOCAL_ASM_STATE[cpu].temp_user_rsp = 0;
        CPU_LOCAL_ASM_STATE[cpu].cpu_id = cpu as u64;

        // Use higher-half mirror address for the per-CPU data structure
        let ptr = crate::mm::vm::phys_to_virt_ptr(core::ptr::addr_of!(CPU_LOCAL_ASM_STATE[cpu]) as usize) as u64;

        // When in kernel mode (now), IA32_GS_BASE is the active GS base.
        // IA32_KERNEL_GS_BASE is the one used by SWAPGS when entering from Ring 3.
        write_msr(IA32_GS_BASE, ptr);
        write_msr(IA32_KERNEL_GS_BASE, ptr);
    }
}

pub fn update_current_kstack(kstack: u64) {
    let cpu = current_cpu_id().min(MAX_CPUS - 1);
    unsafe {
        CPU_LOCAL_ASM_STATE[cpu].current_kstack = kstack;
    }
}

#[repr(C, packed)]
struct RsdpV2 {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    _reserved: [u8; 3],
}

#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

#[repr(C, packed)]
struct MadtHeader {
    header: SdtHeader,
    local_apic_addr: u32,
    flags: u32,
}

#[repr(C, packed)]
struct MadtEntryHeader {
    typ: u8,
    length: u8,
}

#[repr(C, packed)]
struct MadtLocalApic {
    typ: u8,
    length: u8,
    acpi_processor_id: u8,
    apic_id: u8,
    flags: u32,
}

fn parse_madt(madt_phys: u64) -> Vec<u32> {
    let mut ids = Vec::new();
    let madt_ptr = crate::mm::vm::phys_to_virt_ptr(madt_phys as usize) as *const MadtHeader;
    let madt = unsafe { &*madt_ptr };

    let mut cursor = (madt_ptr as usize + size_of::<MadtHeader>()) as *const u8;
    let end = madt_ptr as usize + madt.header.length as usize;

    while (cursor as usize) + size_of::<MadtEntryHeader>() <= end {
        let entry = unsafe { &*(cursor as *const MadtEntryHeader) };
        if entry.length < size_of::<MadtEntryHeader>() as u8 {
            break;
        }

        if entry.typ == 0 && (entry.length as usize) >= size_of::<MadtLocalApic>() {
            let lapic = unsafe { &*(cursor as *const MadtLocalApic) };
            if (lapic.flags & 0x1) != 0 {
                let apic_id = lapic.apic_id as u32;
                if !ids.contains(&apic_id) {
                    ids.push(apic_id);
                }
            }
        }

        cursor = unsafe { cursor.add(entry.length as usize) };
    }

    ids
}

pub fn detect_cpu_apic_ids(rsdp_addr: u64) -> Vec<u32> {
    let mut ids = Vec::new();

    if rsdp_addr == 0 {
        ids.push(apic::local_apic_id());
        return ids;
    }

    let rsdp_ptr = crate::mm::vm::phys_to_virt_ptr(rsdp_addr as usize) as *const RsdpV2;
    let rsdp = unsafe { &*rsdp_ptr };

    if &rsdp.signature != b"RSD PTR " {
        log_warn!(LOG_ORIGIN, "RSDP signature invalid, fallback to BSP only");
        ids.push(apic::local_apic_id());
        return ids;
    }

    let mut madt_phys = 0u64;

    if rsdp.revision >= 2 && rsdp.xsdt_address != 0 {
        // Use XSDT (64-bit addresses)
        let xsdt_phys = rsdp.xsdt_address;
        let xsdt_ptr = crate::mm::vm::phys_to_virt_ptr(xsdt_phys as usize) as *const SdtHeader;
        let xsdt = unsafe { &*xsdt_ptr };

        if &xsdt.signature == b"XSDT" {
            let entries = (xsdt.length as usize).saturating_sub(size_of::<SdtHeader>()) / 8;
            let entries_ptr = (xsdt_ptr as usize + size_of::<SdtHeader>()) as *const u64;

            for i in 0..entries {
                let table_phys = unsafe { *entries_ptr.add(i) };
                if table_phys == 0 { continue; }
                let hdr_ptr = crate::mm::vm::phys_to_virt_ptr(table_phys as usize) as *const SdtHeader;
                let hdr = unsafe { &*hdr_ptr };
                if &hdr.signature == b"APIC" {
                    madt_phys = table_phys;
                    break;
                }
            }
        }
    }

    if madt_phys == 0 && rsdp.rsdt_address != 0 {
        // Use RSDT (32-bit addresses)
        let rsdt_phys = rsdp.rsdt_address as u64;
        let rsdt_ptr = crate::mm::vm::phys_to_virt_ptr(rsdt_phys as usize) as *const SdtHeader;
        let rsdt = unsafe { &*rsdt_ptr };

        if &rsdt.signature == b"RSDT" {
            let entries = (rsdt.length as usize).saturating_sub(size_of::<SdtHeader>()) / 4;
            let entries_ptr = (rsdt_ptr as usize + size_of::<SdtHeader>()) as *const u32;

            for i in 0..entries {
                let table_phys = unsafe { *entries_ptr.add(i) } as u64;
                if table_phys == 0 { continue; }
                let hdr_ptr = crate::mm::vm::phys_to_virt_ptr(table_phys as usize) as *const SdtHeader;
                let hdr = unsafe { &*hdr_ptr };
                if &hdr.signature == b"APIC" {
                    madt_phys = table_phys;
                    break;
                }
            }
        }
    }

    if madt_phys != 0 {
        ids = parse_madt(madt_phys);
    }

    if ids.is_empty() {
        ids.push(apic::local_apic_id());
    }

    ids
}

pub fn initialize_cpu_registry(apic_ids: &[u32]) {
    let bsp_apic = apic::local_apic_id();

    let mut table = CPU_TABLE.lock();
    let mut apic_map = APIC_TO_CPU.lock();

    *table = [CpuRecord::empty(); MAX_CPUS];
    *apic_map = [u16::MAX; 256];

    let mut inserted = 0usize;

    for &apic_id in apic_ids.iter() {
        if inserted >= MAX_CPUS {
            break;
        }

        if (apic_id as usize) >= apic_map.len() {
            continue;
        }

        table[inserted] = CpuRecord {
            apic_id,
            state: if apic_id == bsp_apic {
                CpuState::Online
            } else {
                CpuState::Booting
            },
            online: apic_id == bsp_apic,
        };

        apic_map[apic_id as usize] = inserted as u16;
        inserted += 1;
    }

    if inserted == 0 {
        table[0] = CpuRecord {
            apic_id: bsp_apic,
            state: CpuState::Online,
            online: true,
        };
        if (bsp_apic as usize) < apic_map.len() {
            apic_map[bsp_apic as usize] = 0;
        }
        inserted = 1;
    }

    *CPU_COUNT.lock() = inserted;

    log_info!(
        LOG_ORIGIN,
        "CPU registry initialized: detected={} bsp_apic_id={}",
        inserted,
        bsp_apic
    );
}

pub fn current_cpu_id() -> usize {
    let apic_id = apic::local_apic_id() as usize;
    let map = APIC_TO_CPU.lock();
    if apic_id >= map.len() {
        return 0;
    }
    let mapped = map[apic_id];
    if mapped == u16::MAX {
        0
    } else {
        mapped as usize
    }
}

pub fn cpu_count() -> usize {
    *CPU_COUNT.lock()
}

pub fn cpu_apic_id(cpu_id: usize) -> Option<u32> {
    let table = CPU_TABLE.lock();
    table.get(cpu_id).and_then(|cpu| {
        if cpu.apic_id == u32::MAX {
            None
        } else {
            Some(cpu.apic_id)
        }
    })
}

pub fn mark_cpu_online_by_apic(apic_id: u32) {
    let mut table = CPU_TABLE.lock();
    let map = APIC_TO_CPU.lock();
    let idx = match map.get(apic_id as usize) {
        Some(v) => *v,
        None => return,
    };
    if idx == u16::MAX {
        return;
    }
    let cpu = &mut table[idx as usize];
    cpu.online = true;
    cpu.state = CpuState::Online;
}

pub fn set_cpu_state(cpu_id: usize, state: CpuState) {
    let mut table = CPU_TABLE.lock();
    if let Some(cpu) = table.get_mut(cpu_id) {
        cpu.state = state;
    }
}

pub fn is_cpu_online(cpu_id: usize) -> bool {
    let table = CPU_TABLE.lock();
    table.get(cpu_id).map(|c| c.online).unwrap_or(false)
}

pub fn online_cpu_count() -> usize {
    let table = CPU_TABLE.lock();
    table.iter().filter(|c| c.online).count()
}

const TRAMPOLINE_PHYS: usize = 0x8000;
const TRAMPOLINE_VECTOR: u8 = (TRAMPOLINE_PHYS >> 12) as u8;
const TRAMPOLINE_PML4_MAGIC: u32 = 0xA1B2_C3D4;
const TRAMPOLINE_STACK_MAGIC: u64 = 0x1122_3344_5566_7788;
const TRAMPOLINE_ENTRY_MAGIC: u64 = 0x8877_6655_4433_2211;
const AP_BOOT_TIMEOUT_SPINS: usize = 5_000_000;

const AP_TRAMPOLINE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ap_trampoline.bin"));

fn delay_spin(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

fn patch_u32(blob: &mut [u8], marker: u32, value: u32) {
    let marker_bytes = marker.to_le_bytes();
    for i in 0..=blob.len().saturating_sub(4) {
        if blob[i..i + 4] == marker_bytes {
            blob[i..i + 4].copy_from_slice(&value.to_le_bytes());
            break;
        }
    }
}

fn patch_u64(blob: &mut [u8], marker: u64, value: u64) {
    let marker_bytes = marker.to_le_bytes();
    for i in 0..=blob.len().saturating_sub(8) {
        if blob[i..i + 8] == marker_bytes {
            blob[i..i + 8].copy_from_slice(&value.to_le_bytes());
            break;
        }
    }
}

fn prepare_trampoline(stack_top: u64, entry: u64) {
    if AP_TRAMPOLINE.len() > crate::mm::pmm::PAGE_SIZE {
        panic!("AP trampoline larger than one page");
    }

    if !crate::mm::pmm::reserve_page(TRAMPOLINE_PHYS) {
        panic!("Failed to reserve AP trampoline page at {:#X}", TRAMPOLINE_PHYS);
    }

    let mut page = [0u8; crate::mm::pmm::PAGE_SIZE];
    page[..AP_TRAMPOLINE.len()].copy_from_slice(AP_TRAMPOLINE);

    let cr3 = crate::arch::read_cr3();
    let pml4_phys_full = (cr3 & crate::arch::CR3_PML4_ADDR_MASK) as usize;
    let pml4_phys = pml4_phys_full as u32;

    patch_u32(&mut page, TRAMPOLINE_PML4_MAGIC, pml4_phys);
    patch_u64(&mut page, TRAMPOLINE_STACK_MAGIC, stack_top);
    patch_u64(&mut page, TRAMPOLINE_ENTRY_MAGIC, entry);

    let dst = crate::mm::vm::phys_to_virt_ptr(TRAMPOLINE_PHYS) as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(page.as_ptr(), dst, page.len());
    }

    // Conventional-memory identity mappings carry the NX bit. The AP executes
    // this page after enabling paging, so the identity mapping must be
    // executable. remap_page_in_pml4 creates the entry if absent (e.g. when
    // the firmware did not report 0x8000 as a usable RAM region).
    crate::mm::vm::remap_page_in_pml4(
        pml4_phys_full,
        TRAMPOLINE_PHYS,
        TRAMPOLINE_PHYS,
        crate::mm::vm::PageFlags::kernel_rw(),
    )
    .expect("failed to make AP trampoline page executable");
}

fn alloc_stack_top(pages: usize) -> Option<u64> {
    let stack_phys = crate::mm::pmm::alloc_pages(pages)?;
    Some((crate::mm::vm::HIGHER_HALF_BASE + stack_phys + pages * crate::mm::pmm::PAGE_SIZE) as u64)
}

pub fn bringup_aps() {
    let total = cpu_count();
    if total <= 1 {
        log_info!(LOG_ORIGIN, "SMP: single-core topology, no AP startup needed");
        return;
    }

    log_info!(LOG_ORIGIN, "Starting AP bootstrap for {} additional CPU(s)", total - 1);

    let bsp = current_cpu_id();

    for cpu_id in 0..total {
        if cpu_id == bsp {
            continue;
        }

        if is_cpu_online(cpu_id) {
            continue;
        }

        let Some(apic_id) = cpu_apic_id(cpu_id) else {
            continue;
        };

        let Some(bootstrap_stack) = alloc_stack_top(4) else {
            log_warn!(LOG_ORIGIN, "AP{} startup skipped: failed to allocate bootstrap stack", cpu_id);
            set_cpu_state(cpu_id, CpuState::Offline);
            continue;
        };

        set_cpu_state(cpu_id, CpuState::Booting);

        prepare_trampoline(bootstrap_stack, smp_ap_entry as *const () as usize as u64);

        apic::send_init_ipi(apic_id);
        delay_spin(100_000);
        apic::send_startup_ipi(apic_id, TRAMPOLINE_VECTOR);
        delay_spin(20_000);
        apic::send_startup_ipi(apic_id, TRAMPOLINE_VECTOR);

        let mut online = false;
        for _ in 0..AP_BOOT_TIMEOUT_SPINS {
            if is_cpu_online(cpu_id) {
                online = true;
                break;
            }
            core::hint::spin_loop();
        }

        if online {
            log_info!(LOG_ORIGIN, "AP cpu_id={} apic_id={} online", cpu_id, apic_id);
        } else {
            log_warn!(LOG_ORIGIN, "AP cpu_id={} apic_id={} failed to start", cpu_id, apic_id);
            set_cpu_state(cpu_id, CpuState::Offline);
        }
    }

    log_info!(
        LOG_ORIGIN,
        "SMP startup finished: online={}/{}",
        online_cpu_count(),
        cpu_count()
    );
}

#[no_mangle]
pub extern "C" fn smp_ap_entry() -> ! {
    crate::interrupts::disable();

    let apic_id = apic::local_apic_id();
    let cpu_id = current_cpu_id();

    crate::arch::gdt::init_for_cpu(cpu_id, crate::arch::current_rsp());
    crate::interrupts::idt::load_current_cpu();
    apic::init_local_current_cpu();
    crate::syscall::init();

    let Some(idle_stack_top) = alloc_stack_top(4) else {
        log_warn!(LOG_ORIGIN, "AP{} failed to allocate idle stack", cpu_id);
        loop {
            crate::arch::halt();
        }
    };

    init_cpu_local_syscall_state(cpu_id, idle_stack_top);

    let idle_thread = crate::thread::Thread::new(
        crate::idle_thread_entry as *const () as u64,
        idle_stack_top,
        4 * crate::mm::pmm::PAGE_SIZE,
        crate::arch::read_cr3(),
        crate::thread::ThreadPriority::Idle,
        "idle-ap",
    );

    let idle_id = crate::sched::init_secondary_cpu(cpu_id, idle_thread);

    mark_cpu_online_by_apic(apic_id);
    crate::interrupts::init_timer(100);
    crate::interrupts::enable();

    log_info!(LOG_ORIGIN, "AP{} entering scheduler with idle thread {}", cpu_id, idle_id);

    crate::thread::jump_to_thread(idle_id)
}
