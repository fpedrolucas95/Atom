// kernel/src/init_process.rs
//
// Init Process Bootstrap (Phase 6.2)
//
// Wires up the very first user-space process (PID 1) using the minimal
// executable format implemented in Phase 6.1. The goal is to validate the
// end-to-end path from boot-provided payload (or an embedded fallback) to a
// runnable user thread living in its own address space.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
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
    // Microkernel architecture: UI components run entirely in userspace.
    // The desktop environment is launched as a userspace service:
    // 1. Input drivers (keyboard, mouse) poll raw events from kernel buffers
    // 2. Desktop compositor manages windows and routes events via IPC
    // 3. Applications receive events from the compositor
    //
    // The kernel does NOT contain any UI code - only raw framebuffer
    // exposure and minimal input buffering.
    log_info!(
        LOG_ORIGIN,
        "Desktop environment will be launched as userspace service"
    );

    // Schedule the desktop environment service
    // This uses the manifest-based service loading system
    // The 'atom_desktop' service handles:
    // - Window management and composition
    // - Input routing from drivers to applications
    // - Focus management
    // - Application launching
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

        if let Err(err) = service_manager::manager().mark_ready(&ctx.name) {
            log_error!(
                LOG_ORIGIN,
                "Service '{}' failed readiness transition: {:?}",
                ctx.name,
                err
            );
        }

        // Check if this is the UI shell service
        if ctx.name == "ui_shell" {
            log_info!(LOG_ORIGIN, "Starting desktop environment (ui_shell)");
            run_desktop_environment();
            // Desktop runs forever, shouldn't return
            return;
        }

        // Other services enter a generic service loop
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

/// Desktop environment entry point
/// Runs the compositor with window management, input handling, and rendering
fn run_desktop_environment() {
    use crate::graphics::{self, Color};
    use crate::input;

    const LOG_ORIGIN: &str = "desktop";

    log_info!(LOG_ORIGIN, "Desktop environment starting...");

    // Nord color theme
    let bg_color = Color::new(46, 52, 64);       // nord0 - background
    let panel_color = Color::new(59, 66, 82);    // nord1 - panel
    let accent_color = Color::new(136, 192, 208); // nord8 - accent/cursor
    let text_color = Color::new(236, 239, 244);  // nord6 - text
    let window_bg = Color::new(67, 76, 94);      // nord2 - window background
    let title_bg = Color::new(76, 86, 106);      // nord3 - title bar

    // Get screen dimensions
    let (width, height) = graphics::get_dimensions().unwrap_or((800, 600));
    log_info!(LOG_ORIGIN, "Screen: {}x{}", width, height);

    // Clear screen with background color
    graphics::clear_screen(bg_color);

    // Draw top panel (32 pixels high)
    let panel_height = 32u32;
    graphics::fill_rect(0, 0, width, panel_height, panel_color);

    // Draw "Atom" branding on left side of panel
    graphics::draw_string(12, 10, "Atom", accent_color, panel_color);

    // Draw status text on right side
    let status_text = "Microkernel v0.1";
    let status_x = width.saturating_sub((status_text.len() as u32) * 8 + 12);
    graphics::draw_string(status_x, 10, status_text, text_color, panel_color);

    // Draw a demo window
    let win_x = 50u32;
    let win_y = 60u32;
    let win_width = 400u32;
    let win_height = 300u32;
    let title_height = 24u32;

    // Window shadow (subtle)
    graphics::fill_rect(win_x + 4, win_y + 4, win_width, win_height, Color::new(30, 34, 42));

    // Window background
    graphics::fill_rect(win_x, win_y, win_width, win_height, window_bg);

    // Title bar
    graphics::fill_rect(win_x, win_y, win_width, title_height, title_bg);
    graphics::draw_string(win_x + 8, win_y + 6, "Terminal", text_color, title_bg);

    // Window close button
    let close_x = win_x + win_width - 20;
    graphics::fill_rect(close_x, win_y + 4, 16, 16, Color::new(191, 97, 106)); // nord11 - red
    graphics::draw_string(close_x + 4, win_y + 6, "x", text_color, Color::new(191, 97, 106));

    // Terminal content area
    let content_y = win_y + title_height;
    let content_height = win_height - title_height;
    graphics::fill_rect(win_x, content_y, win_width, content_height, Color::new(46, 52, 64));

    // Terminal prompt
    graphics::draw_string(win_x + 8, content_y + 8, "atom@kernel $ _", Color::new(163, 190, 140), Color::new(46, 52, 64));

    // Initialize mouse cursor state
    let mut cursor_x: i32 = (width / 2) as i32;
    let mut cursor_y: i32 = (height / 2) as i32;

    // Draw initial cursor
    draw_cursor(cursor_x as u32, cursor_y as u32, accent_color);

    log_info!(LOG_ORIGIN, "Desktop ready, entering event loop");

    // Main event loop
    loop {
        // Process mouse events
        while let Some(event) = input::poll_mouse_event() {
            // Erase old cursor by redrawing background
            // (simplified - just draw a small rect)
            redraw_cursor_area(cursor_x as u32, cursor_y as u32, bg_color, panel_color, panel_height);

            // Update cursor position
            cursor_x = (cursor_x + event.delta_x as i32).clamp(0, width as i32 - 1);
            cursor_y = (cursor_y + event.delta_y as i32).clamp(0, height as i32 - 1);

            // Draw new cursor
            draw_cursor(cursor_x as u32, cursor_y as u32, accent_color);
        }

        // Process keyboard events
        while let Some(event) = input::poll_key_event() {
            if event.pressed {
                // Could handle keyboard input here
                log_info!(LOG_ORIGIN, "Key: scancode={}", event.scancode);
            }
        }

        // Yield to other threads
        sched::drive_cooperative_tick();
    }
}

/// Draw a simple arrow cursor at the given position
fn draw_cursor(x: u32, y: u32, color: crate::graphics::Color) {
    use crate::graphics;

    // Simple 8x12 arrow cursor
    let cursor_data: [u8; 12] = [
        0b10000000,
        0b11000000,
        0b11100000,
        0b11110000,
        0b11111000,
        0b11111100,
        0b11111110,
        0b11111000,
        0b11011000,
        0b10001100,
        0b00001100,
        0b00000110,
    ];

    for (row, &bits) in cursor_data.iter().enumerate() {
        for col in 0..8 {
            if bits & (0x80 >> col) != 0 {
                graphics::draw_pixel(x + col, y + row as u32, color);
            }
        }
    }
}

/// Redraw the area where the cursor was (simplified version)
fn redraw_cursor_area(x: u32, y: u32, bg_color: crate::graphics::Color, panel_color: crate::graphics::Color, panel_height: u32) {
    use crate::graphics;

    // Simple clear of cursor area - just fill with background
    let color = if y < panel_height { panel_color } else { bg_color };
    graphics::fill_rect(x, y, 8, 12, color);
}