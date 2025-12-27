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
                // Only launch ui_shell for now - other services are placeholders
                // that cause context corruption. TODO: investigate thread 4 crash
                if name != "ui_shell" {
                    log_info!(
                        LOG_ORIGIN,
                        "Skipping service '{}' (placeholder, not implemented)",
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

    // TODO: Load and execute the userspace ui_shell binary
    // For now, this is a placeholder until process spawning is fully implemented
    // The kernel should NOT have any embedded compositor or UI code
    
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
            log_info!(LOG_ORIGIN, "UI shell service requested");
            log_info!(LOG_ORIGIN, "Userspace UI shell loading not yet implemented");
            log_info!(LOG_ORIGIN, "Kernel should load userspace/drivers/ui_shell binary");
            
            // TODO: Load and execute the userspace ui_shell binary
            // The kernel must NOT contain an embedded compositor
            // Instead, it should:
            // 1. Load the ui_shell ELF binary from disk/boot payload
            // 2. Create a new user process for it
            // 3. Give it capabilities to access framebuffer and input
            // 4. Execute it in userspace
            //
            // For now, just loop to keep the thread alive
            loop {
                sched::yield_current();
            }
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

// ============================================================================
// EMBEDDED COMPOSITOR - DEPRECATED
// ============================================================================
// 
// The code below is an embedded compositor that was temporarily included in
// the kernel for testing. This violates microkernel architecture principles.
//
// ** THIS CODE SHOULD NOT BE USED **
//
// The kernel must NOT contain any UI code. UI components must run in userspace.
// The proper architecture is:
// - kernel: Exposes framebuffer and input via syscalls
// - userspace/drivers/ui_shell: Compositor and window manager
// - userspace/drivers/terminal: Terminal application
// - userspace/drivers/keyboard: Keyboard driver  
// - userspace/drivers/mouse: Mouse driver
//
// TODO: Remove this code entirely once userspace process loading is working
//
// ============================================================================

/*
/// Desktop environment entry point
/// Runs the compositor with window management, input handling, and rendering
fn run_desktop_environment() {
    use crate::graphics::{self, Color};
    use crate::input;

    const LOG_ORIGIN: &str = "desktop";

    log_info!(LOG_ORIGIN, "Desktop environment starting...");

    // Get screen dimensions
    let (width, height) = graphics::get_dimensions().unwrap_or((800, 600));
    log_info!(LOG_ORIGIN, "Screen: {}x{}", width, height);

    // Initialize the compositor
    let mut compositor = Compositor::new(width, height);

    // Create initial windows
    let terminal_id = compositor.create_window(50, 60, 400, 300, "Terminal");

    // Initial render
    compositor.render_all();

    log_info!(LOG_ORIGIN, "Desktop ready, entering event loop");

    // Frame counter for periodic status
    let mut frame_count = 0u64;

    // Main event loop
    loop {
        let mut needs_redraw = false;

        // Process mouse events
        while let Some(event) = input::poll_mouse_event() {
            compositor.handle_mouse_move(event.delta_x, event.delta_y);

            if event.left_button {
                compositor.handle_mouse_click();
            }

            needs_redraw = true;
        }

        // Process keyboard events
        while let Some(event) = input::poll_key_event() {
            if event.pressed {
                log_info!(LOG_ORIGIN, "Key pressed: scancode={:#X}", event.scancode);
                // Send to focused window
                compositor.handle_key(event.scancode);
                needs_redraw = true;
            }
        }

        // Redraw if needed
        if needs_redraw {
            compositor.render_cursor();
        }

        frame_count += 1;

        // Log status periodically
        if frame_count % 1_000_000 == 0 {
            let (cx, cy) = compositor.cursor_position();
            log_info!(LOG_ORIGIN, "Event loop running, frame={}, cursor=({},{})",
                frame_count / 1_000_000, cx, cy);
        }

        // Small delay when no events
        if !needs_redraw {
            for _ in 0..1000 {
                core::hint::spin_loop();
            }
        }
    }
}

// ============================================================================
// Compositor - Window Management and Rendering
// ============================================================================

use crate::graphics::{self, Color};

/// Window structure
struct Window {
    id: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    title: String,
    content_lines: Vec<String>,
    visible: bool,
}

/// Compositor manages all windows and rendering
struct Compositor {
    screen_width: u32,
    screen_height: u32,
    windows: Vec<Window>,
    next_window_id: u32,
    cursor_x: i32,
    cursor_y: i32,
    cursor_saved: [u32; 12 * 16], // Save area under cursor (16x12 pixels)
    cursor_saved_x: u32,
    cursor_saved_y: u32,
    focused_window: Option<u32>,
}

impl Compositor {
    fn new(width: u32, height: u32) -> Self {
        // Draw initial desktop background and panel
        let bg_color = Color::new(46, 52, 64);       // nord0
        let panel_color = Color::new(59, 66, 82);    // nord1
        let accent_color = Color::new(136, 192, 208); // nord8
        let text_color = Color::new(236, 239, 244);  // nord6

        // Clear screen
        graphics::clear_screen(bg_color);

        // Draw top panel
        graphics::fill_rect(0, 0, width, 32, panel_color);
        graphics::draw_string(12, 10, "Atom", accent_color, panel_color);

        let status = "Microkernel v0.1";
        let status_x = width.saturating_sub((status.len() as u32) * 8 + 12);
        graphics::draw_string(status_x, 10, status, text_color, panel_color);

        Self {
            screen_width: width,
            screen_height: height,
            windows: Vec::new(),
            next_window_id: 1,
            cursor_x: (width / 2) as i32,
            cursor_y: (height / 2) as i32,
            cursor_saved: [0; 12 * 16],
            cursor_saved_x: width / 2,
            cursor_saved_y: height / 2,
            focused_window: None,
        }
    }

    fn create_window(&mut self, x: u32, y: u32, width: u32, height: u32, title: &str) -> u32 {
        let id = self.next_window_id;
        self.next_window_id += 1;

        let mut content = Vec::new();
        content.push(String::from("atom@kernel $ _"));

        let window = Window {
            id,
            x,
            y,
            width,
            height,
            title: String::from(title),
            content_lines: content,
            visible: true,
        };

        self.windows.push(window);
        self.focused_window = Some(id);
        id
    }

    fn render_all(&mut self) {
        // Render all windows (back to front)
        for window in &self.windows {
            if window.visible {
                self.render_window(window);
            }
        }

        // Save area under cursor and draw cursor
        self.save_cursor_area();
        self.draw_cursor();
    }

    fn render_window(&self, window: &Window) {
        let window_bg = Color::new(67, 76, 94);      // nord2
        let title_bg = Color::new(76, 86, 106);      // nord3
        let text_color = Color::new(236, 239, 244);  // nord6
        let content_bg = Color::new(46, 52, 64);     // nord0
        let prompt_color = Color::new(163, 190, 140); // nord14 - green

        let title_height = 24u32;

        // Window shadow
        graphics::fill_rect(
            window.x + 4,
            window.y + 4,
            window.width,
            window.height,
            Color::new(30, 34, 42),
        );

        // Window background
        graphics::fill_rect(window.x, window.y, window.width, window.height, window_bg);

        // Title bar
        graphics::fill_rect(window.x, window.y, window.width, title_height, title_bg);
        graphics::draw_string(window.x + 8, window.y + 6, &window.title, text_color, title_bg);

        // Close button
        let close_x = window.x + window.width - 20;
        let close_color = Color::new(191, 97, 106); // nord11 - red
        graphics::fill_rect(close_x, window.y + 4, 16, 16, close_color);
        graphics::draw_string(close_x + 4, window.y + 6, "x", text_color, close_color);

        // Content area
        let content_y = window.y + title_height;
        let content_height = window.height - title_height;
        graphics::fill_rect(window.x, content_y, window.width, content_height, content_bg);

        // Render content lines
        let mut line_y = content_y + 8;
        for line in &window.content_lines {
            if line_y + 8 < window.y + window.height - 8 {
                graphics::draw_string(window.x + 8, line_y, line, prompt_color, content_bg);
                line_y += 12;
            }
        }
    }

    fn save_cursor_area(&mut self) {
        let x = self.cursor_x.max(0) as u32;
        let y = self.cursor_y.max(0) as u32;

        self.cursor_saved_x = x;
        self.cursor_saved_y = y;

        // Read pixels from framebuffer
        for row in 0..12u32 {
            for col in 0..16u32 {
                let px = x + col;
                let py = y + row;
                if px < self.screen_width && py < self.screen_height {
                    let pixel = graphics::read_pixel(px, py);
                    self.cursor_saved[(row * 16 + col) as usize] = pixel;
                }
            }
        }
    }

    fn restore_cursor_area(&self) {
        let x = self.cursor_saved_x;
        let y = self.cursor_saved_y;

        // Write saved pixels back
        for row in 0..12u32 {
            for col in 0..16u32 {
                let px = x + col;
                let py = y + row;
                if px < self.screen_width && py < self.screen_height {
                    let pixel = self.cursor_saved[(row * 16 + col) as usize];
                    graphics::write_pixel(px, py, pixel);
                }
            }
        }
    }

    fn draw_cursor(&self) {
        let accent_color = Color::new(136, 192, 208); // nord8
        let x = self.cursor_x.max(0) as u32;
        let y = self.cursor_y.max(0) as u32;

        // Arrow cursor bitmap
        let cursor_data: [u16; 12] = [
            0b1000000000000000,
            0b1100000000000000,
            0b1110000000000000,
            0b1111000000000000,
            0b1111100000000000,
            0b1111110000000000,
            0b1111111000000000,
            0b1111100000000000,
            0b1101100000000000,
            0b1000110000000000,
            0b0000110000000000,
            0b0000011000000000,
        ];

        for (row, &bits) in cursor_data.iter().enumerate() {
            for col in 0..16 {
                if bits & (0x8000 >> col) != 0 {
                    let px = x + col;
                    let py = y + row as u32;
                    if px < self.screen_width && py < self.screen_height {
                        graphics::draw_pixel(px, py, accent_color);
                    }
                }
            }
        }
    }

    fn render_cursor(&mut self) {
        // Restore old area
        self.restore_cursor_area();

        // Save new area
        self.save_cursor_area();

        // Draw cursor at new position
        self.draw_cursor();
    }

    fn handle_mouse_move(&mut self, dx: i16, dy: i16) {
        self.cursor_x = (self.cursor_x + dx as i32).clamp(0, self.screen_width as i32 - 1);
        self.cursor_y = (self.cursor_y + dy as i32).clamp(0, self.screen_height as i32 - 1);
    }

    fn handle_mouse_click(&mut self) {
        // Check if clicking on a window to focus it
        let cx = self.cursor_x as u32;
        let cy = self.cursor_y as u32;

        for window in self.windows.iter().rev() {
            if window.visible
                && cx >= window.x
                && cx < window.x + window.width
                && cy >= window.y
                && cy < window.y + window.height
            {
                self.focused_window = Some(window.id);
                break;
            }
        }
    }

    fn handle_key(&mut self, _scancode: u8) {
        // TODO: Send to focused window's input buffer
    }

    fn cursor_position(&self) -> (i32, i32) {
        (self.cursor_x, self.cursor_y)
    }
}

*/

// End of deprecated embedded compositor code