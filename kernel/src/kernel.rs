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
mod input;  // Minimal input buffer for userspace drivers
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
mod util;
mod shell;  // Embedded userspace shell (runs in Ring 3)

// Userspace drivers run in Ring 3 using the atom_syscall library.
// See userspace/drivers/ for the actual driver implementations.

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

    // Initialize input subsystem (minimal kernel-side buffer for userspace drivers)
    input::init();
    input::init_ps2_mouse_full(); // Use full initialization with 1:1 scaling

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
    
    // Create userspace UI thread running in ring 3
    let ui_result = create_userspace_ui_thread();
    match ui_result {
        Ok(tid) => {
            log_info!(LOG_KERNEL_INIT, "Userspace UI thread created (tid={})", tid);
        }
        Err(e) => {
            log_error!(LOG_KERNEL_INIT, "Failed to create userspace UI thread: {}", e);
            // Fallback to embedded shell (runs in Ring 3)
            extern "C" fn ui_thread_entry() -> ! {
                shell::shell_entry()
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

            sched::add_thread(ui_thread);
        }
    }

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

/// Create a userspace UI thread that runs in ring 3
/// 
/// This properly sets up the UI shell to run in userspace by:
/// 1. Remapping shell code pages as USER-accessible (in-place, no copy)
/// 2. Allocating a USER stack
/// 3. Creating a Ring 3 context with proper selectors
///
/// Note: We remap the kernel code pages containing shell_entry as USER
/// because the code was compiled with absolute addresses. Copying to a
/// new address would break all relative and absolute references.
fn create_userspace_ui_thread() -> Result<thread::ThreadId, &'static str> {
    use mm::pmm::PAGE_SIZE;
    use mm::vm::{self, PageFlags};

    const KERNEL_STACK_PAGES: usize = 8;
    const USER_STACK_PAGES: usize = 4;
    const CODE_PAGES: usize = 256; // Pages to remap as USER (1MB to cover shell + all dependencies)
    const CODE_PAGES_BEFORE: usize = 16; // Pages BEFORE entry to cover auxiliary functions
    
    // Virtual address for user stack (allocated fresh)
    const USER_STACK_TOP: usize = 0x90000000; // 2.25GB
    
    let cr3 = read_cr3();

    // --- 1. Get shell entry point and remap surrounding pages as USER ---
    let shell_entry_addr = shell::shell_entry as *const () as usize;
    let shell_page_base = mm::pmm::align_down(shell_entry_addr);
    
    // Start mapping BEFORE the entry point to cover auxiliary functions
    // (linker may place helper functions like draw_cursor before shell_entry)
    let remap_start = shell_page_base.saturating_sub(CODE_PAGES_BEFORE * PAGE_SIZE);
    let total_pages = CODE_PAGES + CODE_PAGES_BEFORE;
    
    log_info!(
        LOG_KERNEL_INIT,
        "UI shell entry at {:#X}, remapping {} pages as USER starting at {:#X}",
        shell_entry_addr,
        total_pages,
        remap_start
    );
    
    // Remap shell code pages with USER flag
    // This allows Ring 3 to execute the kernel code at its original address
    for i in 0..total_pages {
        let virt = remap_start + i * PAGE_SIZE;
        if let Err(e) = vm::remap_page_user(virt) {
            log_info!(LOG_KERNEL_INIT, "Remap page {:#X} result: {:?}", virt, e);
            // Continue - some pages might not be mapped
        }
    }
    
    log_info!(
        LOG_KERNEL_INIT,
        "UI shell code remapped: virt={:#X}-{:#X}",
        remap_start,
        remap_start + total_pages * PAGE_SIZE
    );

    // --- 2. Allocate USER stack ---
    let stack_phys = mm::pmm::alloc_pages_zeroed(USER_STACK_PAGES)
        .ok_or("Failed to allocate user stack")?;
    
    let stack_base = USER_STACK_TOP - USER_STACK_PAGES * PAGE_SIZE;
    
    for i in 0..USER_STACK_PAGES {
        let virt = stack_base + i * PAGE_SIZE;
        let phys = stack_phys + i * PAGE_SIZE;
        
        let _ = vm::unmap_page(virt);
        
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE)
            .map_err(|_| "Failed to map stack page")?;
    }
    
    log_info!(
        LOG_KERNEL_INIT,
        "UI shell stack mapped: virt={:#X}-{:#X}",
        stack_base,
        USER_STACK_TOP
    );

    // --- 3. Allocate kernel stack for syscall handling ---
    let kernel_stack_phys = mm::pmm::alloc_pages(KERNEL_STACK_PAGES)
        .ok_or("Failed to allocate kernel stack")?;
    let kernel_stack_top = (kernel_stack_phys + KERNEL_STACK_PAGES * PAGE_SIZE) as u64;

    // --- 4. Create Ring 3 context ---
    // Entry point is the original shell_entry address (now USER-accessible)
    let user_entry = shell_entry_addr as u64;
    let user_stack = USER_STACK_TOP as u64;
    
    log_info!(
        LOG_KERNEL_INIT,
        "Creating userspace UI thread: entry={:#X} stack={:#X} CR3={:#X}",
        user_entry,
        user_stack,
        cr3
    );

    let tid = thread::ThreadId::new();
    let ui_thread = thread::Thread {
        id: tid,
        state: thread::ThreadState::Ready,
        context: thread::CpuContext::new_user(user_entry, user_stack, cr3),
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: cr3,
        priority: thread::ThreadPriority::High,
        name: "ui_shell",
        capability_table: cap::create_capability_table(tid),
    };

    // Use sched::add_thread to properly register the thread with its priority
    sched::add_thread(ui_thread);

    log_info!(
        LOG_KERNEL_INIT,
        "Userspace UI thread created (tid={}) in Ring 3",
        tid
    );

    Ok(tid)
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