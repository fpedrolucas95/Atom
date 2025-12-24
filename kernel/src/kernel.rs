// Kernel entry point and system initialization
//
// This file defines the main kernel entry point (`kmain`) and orchestrates
// the full system initialization sequence after control is transferred
// from the bootloader to the kernel.
//
// It is responsible for bringing up all core subsystems, establishing a
// safe execution environment, and finally handing control over to the
// scheduler once the system is fully operational.
//
// Key responsibilities:
// - Serve as the kernel entry point after boot
// - Initialize early I/O (serial, VGA, logging)
// - Initialize physical and virtual memory management
// - Configure CPU state (GDT, stacks, interrupt handling)
// - Initialize scheduler, threading, and capability system
// - Bring up interrupts, timer, and basic input devices
// - Initialize syscalls, IPC, and shared memory subsystems
// - Launch the first user-space process (init)
// - Transfer execution permanently to the scheduler
//
// Design and implementation:
// - Kernel is `no_std` and `no_main`, fully self-hosted
// - Initialization follows a strict, explicit ordering
// - Interrupts are enabled only after handlers are installed
// - Failures during critical phases result in immediate halt
// - Scheduler owns execution after `start_scheduling`
//
// Safety and correctness notes:
// - Boot-provided structures are treated as immutable
// - Kernel stacks and critical mappings are explicitly validated
// - The system does not continue if the init process fails
// - Panic handler halts the CPU to avoid undefined behavior
//
// Limitations and future considerations:
// - Initialization is single-core and non-parallel
// - Assumes UEFI-based boot on supported architectures
// - No late reinitialization or recovery paths
// - Early boot logging is verbose and not optimized for production
//
// Public interface:
// - `kmain` as the kernel entry point
// - Global panic handler for fatal errors

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]
#![feature(abi_x86_interrupt)]

extern crate alloc;

mod arch;
mod boot;
mod vga;
mod mm;
mod serial;
mod build_info;
mod interrupts;
mod keyboard;
mod mouse;
mod log;
mod graphics;
mod thread;
mod sched;
mod syscall;
mod ipc;
mod cap;
mod shared_mem;
mod system;
mod executable;
mod init_process;
mod service_manager;
mod ui;
mod util;

#[cfg(target_arch = "x86_64")]
#[path = "../../arch/x86_64/uefi.rs"]
mod uefi;

use crate::arch::{current_rsp, halt, read_cr3};
use crate::arch::gdt;
use crate::boot::{BootInfo, MemoryMap};
use core::panic::PanicInfo;

const LOG_KERNEL_INIT: &str = "kernel:init";
const LOG_MM: &str = "vmm";
const LOG_APIC: &str = "apic";
const LOG_SCHED: &str = "sched";
const LOG_INIT_PROC: &str = "init";

#[global_allocator]
static ALLOCATOR: mm::heap::KernelAllocator = mm::heap::KernelAllocator;

#[no_mangle]
pub unsafe extern "C" fn kmain(boot_info: &'static BootInfo) -> ! {
    unsafe {
        let port: u16 = 0x3F8;
        core::arch::asm!(
        "out dx, al",
        in("dx") port,
        in("al") b'K',
        options(nomem, nostack, preserves_flags)
        );
    }

    serial::init();
    system::init(boot_info.cpu, boot_info.boot_method);

    log_info!(LOG_KERNEL_INIT, "{}", build_info::BOOT_BANNER);

    vga::init();
    mm::init(&boot_info.memory_map);

    if boot_info.framebuffer_present {
        let fb = &boot_info.framebuffer;
        if mm::vm::map_framebuffer(fb.address, fb.size) {
            graphics::init(fb);
            graphics::init_terminal();
        }
    }

    gdt::init(current_rsp());
    mm::vm::ensure_current_stack_mapped(64);

    log::init();
    if boot_info.verbose {
        log::set_level(log::LogLevel::Debug);
        log::enable_vga_output();
    }

    display_uefi_memory_map(&boot_info.memory_map);
    display_memory_stats();

    thread::init();
    init_scheduler();
    cap::init();

    interrupts::init();
    interrupts::init_timer(100);

    log_info!(LOG_APIC, "Enabling interrupts...");
    interrupts::enable();

    keyboard::init();
    mouse::init();

    syscall::init();
    ipc::init();
    shared_mem::init();

    log_info!(LOG_INIT_PROC, "Calling init_process::launch_init()...");
    match init_process::launch_init(boot_info) {
        Ok(init) => {
            log_info!(LOG_INIT_PROC, "Init process launched (pid={})", init.pid);
        }
        Err(e) => {
            log_panic!(LOG_INIT_PROC, "FATAL: Init process launch failed: {:?}", e);
            log_panic!(LOG_INIT_PROC, "System cannot continue without init. Halting.");
            loop {
                halt();
            }
        }
    }
    
    extern "C" fn ui_thread_entry() -> ! {
        ui::run_userspace_shell()
    }

    let ui_stack = mm::pmm::alloc_pages(8).expect("Failed to allocate UI stack");
    let ui_stack_top = ui_stack + (8 * mm::pmm::PAGE_SIZE);
    let cr3 = read_cr3();
    let entry_u64 = (ui_thread_entry as *const () as usize) as u64;

    let ui_thread = thread::Thread::new(
        entry_u64,
        ui_stack_top as u64,
        8 * mm::pmm::PAGE_SIZE,
        cr3,
        thread::ThreadPriority::High,
        "ui",
    );

    let _ui_tid = sched::add_thread(ui_thread);
    log_info!(LOG_KERNEL_INIT, "Handing over to scheduler.");
    start_scheduling();

}

fn init_scheduler() {
    extern "C" fn idle_thread_entry() -> ! {
        loop {
            unsafe { core::arch::asm!("hlt"); }
        }
    }

    let idle_stack = mm::pmm::alloc_pages(4).expect("Failed to allocate idle stack");
    let idle_stack_top = idle_stack + (4 * mm::pmm::PAGE_SIZE);
    let cr3 = read_cr3();

    let idle_thread = thread::Thread::new(
        (idle_thread_entry as *const () as usize) as u64,
        idle_stack_top as u64,
        4 * mm::pmm::PAGE_SIZE,
        cr3,
        thread::ThreadPriority::Idle,
        "idle",
    );

    sched::init(idle_thread);
    log_info!(LOG_SCHED, "Scheduler initialized with idle thread");
}

fn start_scheduling() -> ! {
    log_info!(LOG_SCHED, "Starting dispatcher...");
    if let Some(first) = sched::schedule() {
        if let Some(stack) = thread::kernel_stack_top(first) {
            gdt::set_rsp0(stack);
        }

        thread::jump_to_thread(first);
    }

    log_panic!(LOG_SCHED, "No threads to schedule!");
    loop {
        halt();
    }
}

fn display_uefi_memory_map(memory_map: &MemoryMap) {
    let mut conventional = 0u64;
    for descriptor in memory_map.descriptors() {
        if descriptor.typ == 7 {
            conventional += descriptor.number_of_pages * 4096;
        }
    }
    log_info!(LOG_KERNEL_INIT, "Usable RAM: {} MB", conventional / (1024 * 1024));
}

fn display_memory_stats() {
    let (total, free) = mm::pmm::get_stats();
    log_info!(LOG_MM, "PMM: {}/{} pages free", free, total);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log_error!("PANIC", "{}", info);
    loop {
        halt();
    }
}