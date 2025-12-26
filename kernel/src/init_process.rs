// kernel/src/init_process.rs
//
// Init Process Bootstrap (Phase 6.2)
//
// Wires up the very first user-space process (PID 1) using the minimal
// executable format implemented in Phase 6.1. The goal is to validate the
// end-to-end path from boot-provided payload (or an embedded fallback) to a
// runnable user thread living in its own address space.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::boot::BootInfo;
use crate::executable::{self, ExecError, LoadedExecutable};
use crate::mm::addrspace::{self, AddressSpaceId};
use crate::mm::{pmm, vm};
use crate::mm::vm::PageFlags;
use crate::sched;
use crate::service_manager::{self, ServiceSpec};
use crate::thread::{self, CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::{log_error, log_info, log_warn};
use crate::mm::pmm::{align_up, PAGE_SIZE};

const LOG_ORIGIN: &str = "init";
const USER_STACK_PAGES: usize = 4;
const USER_STACK_SIZE: usize = USER_STACK_PAGES * PAGE_SIZE;
const USER_STACK_TOP: usize = 0x0000_8000_0000;
const KERNEL_STACK_PAGES: usize = 8;
const SERVICE_STACK_PAGES: usize = 4;

#[derive(Clone)]
struct ServiceThreadContext {
    name: String,
    capabilities: Vec<String>,
}

static SERVICE_THREADS: spin::Mutex<BTreeMap<ThreadId, ServiceThreadContext>> =
    spin::Mutex::new(BTreeMap::new());

#[allow(dead_code)]
pub struct InitProcess {
    pub pid: ThreadId,
    pub address_space: AddressSpaceId,
    pub entry_point: usize,
    pub user_stack_top: usize,
    pub kernel_stack_top: u64,
}

#[allow(dead_code)]                     
#[derive(Debug)]
pub enum InitError {
    ExecutableLoadFailed(ExecError),
    StackAllocationFailed,
    ThreadCreationFailed,
}

pub fn launch_init(boot_info: &BootInfo) -> Result<InitProcess, InitError> {
    log_info!(LOG_ORIGIN, "launch_init() called");

    let pid = ThreadId::new();

    let init = create_init_process(pid, boot_info)
        .map_err(InitError::ExecutableLoadFailed)?;

    log_info!(
        LOG_ORIGIN,
        "Init process ready: pid={}, entry=0x{:X}, user_stack=0x{:X}",
        init.pid,
        init.entry_point,
        init.user_stack_top
    );

    service_manager::initialize_and_report();
    launch_ui_service();
    bootstrap_manifest_services();
    respond_to_basic_syscalls();

    Ok(init)
}

fn create_init_process(pid: ThreadId, boot_info: &BootInfo) -> Result<InitProcess, ExecError> {
    // Use kernel's page table directly (no separate address space for now)
    let kernel_cr3 = crate::arch::read_cr3() as usize;

    // Load executable into KERNEL page table
    let executable = load_payload_into_kernel(pid, boot_info)?;
    let user_stack_top = map_user_stack_into_kernel(pid)?;
    let kernel_stack_top = allocate_kernel_stack()?;

    let context = CpuContext::new_user(
        executable.entry_point as u64,
        user_stack_top as u64,
        kernel_cr3 as u64,  // Use kernel CR3, not user PML4
    );

    log_info!(
        LOG_ORIGIN,
        "Init context created: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X} CR3={:#016X}",
        context.rip,
        context.rsp,
        context.cs,
        context.ss,
        context.cr3
    );

    let thread = Thread {
        id: pid,
        state: ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: kernel_cr3 as u64,
        priority: ThreadPriority::Normal,
        name: "init",
        capability_table: crate::cap::create_capability_table(pid),
    };

    thread::add_thread(thread);
    sched::mark_thread_ready(pid);

    // Use a dummy AddressSpaceId for compatibility
    let address_space = AddressSpaceId::from_raw(0);

    Ok(InitProcess {
        pid,
        address_space,
        entry_point: executable.entry_point,
        user_stack_top,
        kernel_stack_top,
    })
}

fn load_payload_into_kernel(
    _pid: ThreadId,
    boot_info: &BootInfo,
) -> Result<LoadedExecutable, ExecError> {
    let image = if boot_info.init_payload.is_present() {
        log_info!(LOG_ORIGIN, "Loading init payload provided by bootloader");
        unsafe {
            core::slice::from_raw_parts(
                boot_info.init_payload.ptr,
                boot_info.init_payload.size,
            )
        }
    } else {
        log_warn!(LOG_ORIGIN, "Bootloader did not provide init payload; using embedded image");
        executable::embedded_init_image()
    };

    let sections = executable::parse_image(image)?;

    log_info!(
        LOG_ORIGIN,
        "Parsed executable: text_len={}, data_len={}, bss_size={}, entry_offset={}",
        sections.text.len(),
        sections.data.len(),
        sections.bss_size,
        sections.entry_offset
    );

    // -------------------------------
    // TEXT
    // -------------------------------
    let text_base = executable::USER_EXEC_LOAD_BASE;
    let text_size = align_up(sections.text.len().max(1));
    let text_pages = text_size / PAGE_SIZE;

    let (total, free) = pmm::get_stats();
    log_info!(
        LOG_ORIGIN,
        "PMM before text alloc: {}/{} free, requesting {} pages",
        free, total, text_pages
    );

    let text_phys = pmm::alloc_pages_zeroed(text_pages)
        .ok_or(ExecError::OutOfMemory)?;

    log_info!(LOG_ORIGIN, "Text allocated at phys 0x{:X}", text_phys);

    unsafe {
        core::ptr::copy_nonoverlapping(
            sections.text.as_ptr(),
            text_phys as *mut u8,
            sections.text.len(),
        );
    }

    // 🔥 FIX CRÍTICO 🔥
    // Garantir que a faixa do USER_EXEC_LOAD_BASE não está mapeada
    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let _ = vm::unmap_page(virt);
    }

    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let phys = text_phys + i * PAGE_SIZE;

        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER)
            .map_err(|e| {
                log_error!(
                    LOG_ORIGIN,
                    "map_page(.text) FAILED: i={} virt=0x{:X} phys=0x{:X} err={:?}",
                    i,
                    virt,
                    phys,
                    e
                );
                ExecError::OutOfMemory
            })?;
    }

    // -------------------------------
    // BSS
    // -------------------------------
    let bss_base = align_up(text_base + text_size);
    let bss_size = sections.bss_size.max(1);
    let bss_pages = align_up(bss_size) / PAGE_SIZE;

    let bss_phys = pmm::alloc_pages_zeroed(bss_pages)
        .ok_or(ExecError::OutOfMemory)?;

    for i in 0..bss_pages {
        let virt = bss_base + i * PAGE_SIZE;
        let phys = bss_phys + i * PAGE_SIZE;

        let _ = vm::unmap_page(virt);

        vm::map_page(
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        )
            .map_err(|_| ExecError::OutOfMemory)?;
    }

    let entry_point = text_base + sections.entry_offset;

    log_info!(
        LOG_ORIGIN,
        "Executable loaded into kernel page table: text=0x{:X}, bss=0x{:X}, entry=0x{:X}",
        text_base,
        bss_base,
        entry_point
    );

    Ok(LoadedExecutable {
        entry_point,
        text_base,
        data_base: bss_base,
        bss_base,
    })
}

fn map_user_stack_into_kernel(_pid: ThreadId) -> Result<usize, ExecError> {
    let virt_base = USER_STACK_TOP - USER_STACK_SIZE;
    let phys_base = pmm::alloc_pages_zeroed(USER_STACK_PAGES).ok_or(ExecError::OutOfMemory)?;

    for i in 0..USER_STACK_PAGES {
        let virt = virt_base + i * PAGE_SIZE;
        let phys = phys_base + i * PAGE_SIZE;
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE)
            .map_err(|_| ExecError::OutOfMemory)?;
    }

    log_info!(
        LOG_ORIGIN,
        "Init user stack mapped into kernel: virt=0x{:X}-0x{:X} ({} pages)",
        virt_base, USER_STACK_TOP, USER_STACK_PAGES
    );

    Ok(USER_STACK_TOP)
}

#[allow(dead_code)]
fn map_user_stack_with_guard() -> Result<usize, ExecError> {
    let guard_page = USER_STACK_TOP - USER_STACK_SIZE - PAGE_SIZE;
    let stack_base = USER_STACK_TOP - USER_STACK_SIZE;

    log_info!(
        LOG_ORIGIN,
        "User stack: guard=0x{:X} stack=0x{:X}-0x{:X}",
        guard_page,
        stack_base,
        USER_STACK_TOP
    );

    Ok(USER_STACK_TOP)
}

#[allow(dead_code)]
fn map_user_stack(pid: ThreadId, address_space: AddressSpaceId) -> Result<usize, ExecError> {
    let virt_base = USER_STACK_TOP - USER_STACK_SIZE;
    let phys_base = pmm::alloc_pages_zeroed(USER_STACK_PAGES).ok_or(ExecError::OutOfMemory)?;

    addrspace::map_region(
        address_space,
        pid,
        virt_base,
        phys_base,
        USER_STACK_SIZE,
        PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
    )
    .map_err(ExecError::AddressSpace)?;

    log_info!(
        LOG_ORIGIN,
        "Init user stack mapped: virt=0x{:X}-0x{:X} ({} pages) -> phys=0x{:X}",
        virt_base,
        USER_STACK_TOP,
        USER_STACK_PAGES,
        phys_base
    );

    Ok(USER_STACK_TOP)
}

fn allocate_kernel_stack() -> Result<u64, ExecError> {
    let phys = pmm::alloc_pages(KERNEL_STACK_PAGES).ok_or(ExecError::OutOfMemory)?;
    let size = KERNEL_STACK_PAGES * PAGE_SIZE;
    let top = (phys + size) as u64;

    log_info!(
        LOG_ORIGIN,
        "Init kernel stack allocated: phys=0x{:X} size={} bytes",
        phys,
        size
    );

    Ok(top)
}

fn bootstrap_manifest_services() {
    match service_manager::init_embedded_manifest() {
        Ok(manager) => {
            let mut launched = 0usize;

            for name in manager.startup_plan() {
                if name == "ui_shell" {
                    log_info!(
                        LOG_ORIGIN,
                        "Skipping manifest entry '{}' because UI shell is launched directly",
                        name
                    );
                    continue;
                }

                if let Some(spec) = manager.manifest().service(name) {
                    match spawn_service_thread(spec) {
                        Ok(tid) => {
                            log_info!(
                                LOG_ORIGIN,
                                "Boot service '{}' scheduled as thread {}",
                                name,
                                tid
                            );
                            launched += 1;
                        }
                        Err(err) => {
                            log_error!(
                                LOG_ORIGIN,
                                "Failed to spawn service '{}': {:?}",
                                name,
                                err
                            );
                        }
                    }
                }
            }

            log_info!(
                LOG_ORIGIN,
                "Manifest services scheduled: {} launched ({} declared)",
                launched,
                manager.manifest().count()
            );
        }
        Err(err) => {
            log_error!(
                LOG_ORIGIN,
                "Service manager manifest not available, skipping service bootstrap: {:?}",
                err
            );
        }
    }
}

fn launch_ui_service() {
    log_info!(LOG_ORIGIN, "Launching UI shell from embedded binary...");

    match create_ui_shell_process() {
        Ok(tid) => {
            log_info!(LOG_ORIGIN, "UI shell launched as userspace process (tid={})", tid);
        }
        Err(e) => {
            log_error!(LOG_ORIGIN, "Failed to launch UI shell: {:?}", e);
        }
    }
}

/// Create and launch the UI shell as a true userspace process
fn create_ui_shell_process() -> Result<ThreadId, ExecError> {
    const UI_SHELL_STACK_TOP: usize = 0x9000_0000;  // Different from init stack
    const UI_SHELL_STACK_PAGES: usize = 8;  // 32KB stack
    const UI_SHELL_STACK_SIZE: usize = UI_SHELL_STACK_PAGES * PAGE_SIZE;

    let kernel_cr3 = crate::arch::read_cr3() as usize;
    let tid = ThreadId::new();

    // Load the embedded ui_shell binary
    let image = executable::embedded_ui_shell_image();
    let sections = executable::parse_image(image)?;

    log_info!(
        LOG_ORIGIN,
        "UI shell binary: text={} bytes, data={} bytes, bss={} bytes, entry_offset=0x{:X}",
        sections.text.len(),
        sections.data.len(),
        sections.bss_size,
        sections.entry_offset
    );

    // Since init only uses 0x400000-0x402000, and ui_shell uses the same base,
    // we'll load ui_shell at the same base. The init process is just a yield loop
    // that we can effectively replace.
    let text_base = executable::USER_EXEC_LOAD_BASE;
    let text_size = align_up(sections.text.len().max(1));
    let text_pages = text_size / PAGE_SIZE;

    // Allocate and map text section
    let text_phys = pmm::alloc_pages_zeroed(text_pages)
        .ok_or(ExecError::OutOfMemory)?;

    log_info!(LOG_ORIGIN, "UI shell text at phys 0x{:X}", text_phys);

    // Copy text section
    unsafe {
        core::ptr::copy_nonoverlapping(
            sections.text.as_ptr(),
            text_phys as *mut u8,
            sections.text.len(),
        );
    }

    // Unmap and remap text pages as USER
    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let _ = vm::unmap_page(virt);
    }

    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let phys = text_phys + i * PAGE_SIZE;
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER)
            .map_err(|_| ExecError::OutOfMemory)?;
    }

    // Map data section (rodata + got)
    if !sections.data.is_empty() {
        let data_base = align_up(text_base + text_size);
        let data_size = align_up(sections.data.len().max(1));
        let data_pages = data_size / PAGE_SIZE;

        let data_phys = pmm::alloc_pages_zeroed(data_pages)
            .ok_or(ExecError::OutOfMemory)?;

        unsafe {
            core::ptr::copy_nonoverlapping(
                sections.data.as_ptr(),
                data_phys as *mut u8,
                sections.data.len(),
            );
        }

        for i in 0..data_pages {
            let virt = data_base + i * PAGE_SIZE;
            let phys = data_phys + i * PAGE_SIZE;
            let _ = vm::unmap_page(virt);
            vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER)
                .map_err(|_| ExecError::OutOfMemory)?;
        }

        log_info!(LOG_ORIGIN, "UI shell data mapped at 0x{:X}", data_base);
    }

    // Map BSS section
    let bss_base = align_up(text_base + text_size + align_up(sections.data.len()));
    let bss_size = sections.bss_size.max(1);
    let bss_pages = align_up(bss_size) / PAGE_SIZE;

    let bss_phys = pmm::alloc_pages_zeroed(bss_pages)
        .ok_or(ExecError::OutOfMemory)?;

    for i in 0..bss_pages {
        let virt = bss_base + i * PAGE_SIZE;
        let phys = bss_phys + i * PAGE_SIZE;
        let _ = vm::unmap_page(virt);
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE)
            .map_err(|_| ExecError::OutOfMemory)?;
    }

    log_info!(LOG_ORIGIN, "UI shell BSS mapped at 0x{:X} ({} pages)", bss_base, bss_pages);

    // Map user stack
    let stack_base = UI_SHELL_STACK_TOP - UI_SHELL_STACK_SIZE;
    let stack_phys = pmm::alloc_pages_zeroed(UI_SHELL_STACK_PAGES)
        .ok_or(ExecError::OutOfMemory)?;

    for i in 0..UI_SHELL_STACK_PAGES {
        let virt = stack_base + i * PAGE_SIZE;
        let phys = stack_phys + i * PAGE_SIZE;
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE)
            .map_err(|_| ExecError::OutOfMemory)?;
    }

    log_info!(
        LOG_ORIGIN,
        "UI shell stack mapped: 0x{:X}-0x{:X}",
        stack_base,
        UI_SHELL_STACK_TOP
    );

    // Allocate kernel stack for syscall handling
    let kernel_stack_phys = pmm::alloc_pages(KERNEL_STACK_PAGES)
        .ok_or(ExecError::OutOfMemory)?;
    let kernel_stack_top = (kernel_stack_phys + KERNEL_STACK_PAGES * PAGE_SIZE) as u64;

    // Create userspace context
    let entry_point = text_base + sections.entry_offset;
    let context = CpuContext::new_user(
        entry_point as u64,
        UI_SHELL_STACK_TOP as u64,
        kernel_cr3 as u64,
    );

    log_info!(
        LOG_ORIGIN,
        "UI shell context: entry=0x{:X} stack=0x{:X} cr3=0x{:X}",
        entry_point,
        UI_SHELL_STACK_TOP,
        kernel_cr3
    );

    // Create thread
    let thread = Thread {
        id: tid,
        state: ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: kernel_cr3 as u64,
        priority: ThreadPriority::High,  // UI gets priority
        name: "ui_shell",
        capability_table: crate::cap::create_capability_table(tid),
    };

    thread::add_thread(thread);
    sched::mark_thread_ready(tid);

    Ok(tid)
}

fn spawn_service_thread(spec: &ServiceSpec) -> Result<ThreadId, ExecError> {
    let stack_phys = pmm::alloc_pages(SERVICE_STACK_PAGES).ok_or(ExecError::OutOfMemory)?;
    let stack_top = stack_phys + SERVICE_STACK_PAGES * PAGE_SIZE;

    let mut registry = SERVICE_THREADS.lock();

    let priority = if spec.name == "ui_shell" {
        ThreadPriority::High
    } else {
        ThreadPriority::Normal
    };

    let thread = Thread::new(
        service_worker as *const() as u64,
        stack_top as u64,
        SERVICE_STACK_PAGES * PAGE_SIZE,
        0,
        priority,
        "svc_worker",
    );

    let tid = thread.id;
    registry.insert(
        tid,
        ServiceThreadContext {
            name: spec.name.clone(),
            capabilities: spec.capabilities.clone(),
        },
    );

    thread::add_thread(thread);
    sched::mark_thread_ready(tid);
    Ok(tid)
}

fn respond_to_basic_syscalls() {
    log_info!(
        LOG_ORIGIN,
        "Init will service basic syscalls for bootstrapped services (yield + ready notifications)"
    );
}

extern "C" fn service_worker() {
    log_info!("svc", "service_worker entered");

    const LOG_ORIGIN: &str = "svc";

    let tid = match sched::current_thread() {
        Some(tid) => tid,
        None => {
            log_error!(LOG_ORIGIN, "Service worker started without current thread context");
            return;
        }
    };

    let context = {
        let registry = SERVICE_THREADS.lock();
        registry.get(&tid).cloned()
    };

    if let Some(ctx) = context {
        log_info!(
            LOG_ORIGIN,
            "Service '{}' (tid={}) initializing with caps {:?}",
            ctx.name,
            tid,
            ctx.capabilities
        );

        log_info!(LOG_ORIGIN, "Checking if service '{}' is ui_shell", ctx.name);

        if let Err(err) = service_manager::manager().mark_ready(&ctx.name) {
            log_error!(
                LOG_ORIGIN,
                "Service '{}' failed readiness transition: {:?}",
                ctx.name,
                err
            );
        }

        // Log service ready status
        // NOTE: The ui_shell is now launched directly in kernel.rs via create_userspace_ui_thread,
        // not via the service manager. This code path handles other services only.
        log_info!(LOG_ORIGIN, "Service '{}' ready, entering service loop", ctx.name);

        loop {
            sched::drive_cooperative_tick();
        }
    } else {
        log_warn!(
            LOG_ORIGIN,
            "Service worker {} missing registry context; yielding",
            tid
        );
        sched::drive_cooperative_tick();
    }
}