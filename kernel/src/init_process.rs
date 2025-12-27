// kernel/src/init_process.rs
//
// Init Process Bootstrap - Microkernel UI Shell Loader
//
// This module is responsible for loading and executing the UI shell as the
// first userspace process. The kernel does NOT contain any UI logic - all
// rendering, window management, and input handling runs in userspace.
//
// Key responsibilities:
// - Load ui_shell.atxf from the boot payload provided by the bootloader
// - Parse and validate the ATXF executable format
// - Create a proper userspace address space for the UI shell
// - Grant framebuffer and input capabilities via the capability system
// - Transfer control to the userspace process in Ring 3
//
// CRITICAL: This module must NOT:
// - Contain any UI/graphics code
// - Provide fallback shells or compositors
// - Mock or simulate UI functionality
// - Use the embedded init image as a UI replacement
//
// If the UI shell cannot be loaded, the system MUST fail loudly.

use crate::boot::BootInfo;
use crate::cap::{self, CapPermissions, InputDeviceType, ResourceType};
use crate::executable::{self, ExecError, LoadedExecutable, ATXF_MAGIC};
use crate::mm::pmm::{self, align_up, PAGE_SIZE};
use crate::mm::vm::{self, PageFlags};
use crate::sched;
use crate::thread::{self, CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::{graphics, log_error, log_info, log_panic};

const LOG_ORIGIN: &str = "init";

const USER_STACK_PAGES: usize = 4;
const USER_STACK_SIZE: usize = USER_STACK_PAGES * PAGE_SIZE;
const USER_STACK_TOP: usize = 0x0000_8000_0000;
const KERNEL_STACK_PAGES: usize = 8;

/// Result of launching the init process (UI shell)
#[allow(dead_code)]
pub struct InitProcess {
    pub pid: ThreadId,
    pub entry_point: usize,
    pub user_stack_top: usize,
    pub kernel_stack_top: u64,
}

/// Errors that can occur during init process launch
#[derive(Debug)]
pub enum InitError {
    /// No boot payload was provided by the bootloader
    NoBootPayload,
    /// The boot payload is not a valid ATXF executable
    InvalidExecutable(ExecError),
    /// Failed to allocate memory for the process
    MemoryAllocationFailed,
    /// Failed to create thread
    ThreadCreationFailed,
    /// Failed to grant required capabilities
    CapabilityError,
    /// Framebuffer not available (required for UI shell)
    NoFramebuffer,
}

/// Launch the init process (UI shell) from the boot payload.
///
/// This function loads ui_shell.atxf from the boot image, creates a userspace
/// process, grants it the necessary capabilities (framebuffer, input), and
/// schedules it for execution.
///
/// # Panics
///
/// This function will cause a system halt if:
/// - No boot payload is provided
/// - The boot payload is not a valid ATXF executable
/// - Memory allocation fails
/// - Capability granting fails
///
/// The system CANNOT continue without a UI shell - there are no fallbacks.
pub fn launch_init(boot_info: &BootInfo) -> Result<InitProcess, InitError> {
    log_info!(LOG_ORIGIN, "===========================================");
    log_info!(LOG_ORIGIN, "MICROKERNEL INIT: Loading UI shell from boot payload");
    log_info!(LOG_ORIGIN, "===========================================");

    // Step 1: Validate boot payload exists
    if !boot_info.init_payload.is_present() {
        log_panic!(
            LOG_ORIGIN,
            "FATAL: No boot payload provided by bootloader!"
        );
        log_panic!(
            LOG_ORIGIN,
            "The kernel requires ui_shell.atxf to be loaded by the bootloader."
        );
        log_panic!(
            LOG_ORIGIN,
            "Cannot continue without a UI shell. System halting."
        );
        return Err(InitError::NoBootPayload);
    }

    // Step 2: Validate framebuffer is available
    if !boot_info.framebuffer_present {
        log_panic!(
            LOG_ORIGIN,
            "FATAL: No framebuffer available!"
        );
        log_panic!(
            LOG_ORIGIN,
            "The UI shell requires framebuffer access. System halting."
        );
        return Err(InitError::NoFramebuffer);
    }

    let payload_ptr = boot_info.init_payload.ptr;
    let payload_size = boot_info.init_payload.size;

    log_info!(
        LOG_ORIGIN,
        "Boot payload: ptr=0x{:X}, size={} bytes",
        payload_ptr as usize,
        payload_size
    );

    // Step 3: Validate ATXF magic before full parsing
    let payload_bytes =
        unsafe { core::slice::from_raw_parts(payload_ptr, payload_size) };

    if payload_size < 4 {
        log_panic!(
            LOG_ORIGIN,
            "FATAL: Boot payload too small ({} bytes) - not a valid ATXF executable",
            payload_size
        );
        return Err(InitError::InvalidExecutable(ExecError::Truncated));
    }

    let magic = u32::from_le_bytes([
        payload_bytes[0],
        payload_bytes[1],
        payload_bytes[2],
        payload_bytes[3],
    ]);

    if magic != ATXF_MAGIC {
        log_panic!(
            LOG_ORIGIN,
            "FATAL: Invalid ATXF magic: 0x{:08X} (expected 0x{:08X})",
            magic,
            ATXF_MAGIC
        );
        log_panic!(
            LOG_ORIGIN,
            "The boot payload is not a valid Atom executable."
        );
        return Err(InitError::InvalidExecutable(ExecError::InvalidMagic));
    }

    log_info!(LOG_ORIGIN, "ATXF magic validated: 0x{:08X}", magic);

    // Step 4: Parse the executable
    let sections = match executable::parse_image(payload_bytes) {
        Ok(s) => s,
        Err(e) => {
            log_panic!(
                LOG_ORIGIN,
                "FATAL: Failed to parse ATXF executable: {:?}",
                e
            );
            return Err(InitError::InvalidExecutable(e));
        }
    };

    log_info!(
        LOG_ORIGIN,
        "Executable parsed: text={} bytes, data={} bytes, bss={} bytes, entry_offset=0x{:X}",
        sections.text.len(),
        sections.data.len(),
        sections.bss_size,
        sections.entry_offset
    );

    // Step 5: Create the UI shell process
    let pid = ThreadId::new();
    log_info!(LOG_ORIGIN, "Creating UI shell process with PID {}", pid);

    let init = create_ui_shell_process(pid, &sections, boot_info)?;

    log_info!(
        LOG_ORIGIN,
        "UI shell process created: entry=0x{:X}, stack=0x{:X}",
        init.entry_point,
        init.user_stack_top
    );

    // Step 6: Grant capabilities to the UI shell
    grant_ui_shell_capabilities(pid, boot_info)?;

    log_info!(LOG_ORIGIN, "===========================================");
    log_info!(LOG_ORIGIN, "UI shell ready for execution");
    log_info!(LOG_ORIGIN, "Microkernel architecture: All UI runs in userspace");
    log_info!(LOG_ORIGIN, "===========================================");

    Ok(init)
}

/// Create the UI shell userspace process
fn create_ui_shell_process(
    pid: ThreadId,
    sections: &executable::ExecutableSections,
    _boot_info: &BootInfo,
) -> Result<InitProcess, InitError> {
    // Use kernel's page table for now (simplified approach)
    let kernel_cr3 = crate::arch::read_cr3() as usize;

    // Load executable into memory
    let executable = load_ui_shell_executable(sections)?;
    let user_stack_top = allocate_user_stack()?;
    let kernel_stack_top = allocate_kernel_stack()?;

    // Create CPU context for Ring 3 execution
    let context = CpuContext::new_user(
        executable.entry_point as u64,
        user_stack_top as u64,
        kernel_cr3 as u64,
    );

    log_info!(
        LOG_ORIGIN,
        "User context: RIP=0x{:016X} RSP=0x{:016X} CS=0x{:04X} SS=0x{:04X}",
        context.rip,
        context.rsp,
        context.cs,
        context.ss
    );

    // Create the thread with its own capability table
    let thread = Thread {
        id: pid,
        state: ThreadState::Ready,
        context,
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: kernel_cr3 as u64,
        priority: ThreadPriority::High, // UI shell gets high priority
        name: "ui_shell",
        capability_table: cap::create_capability_table(pid),
    };

    thread::add_thread(thread);
    sched::mark_thread_ready(pid);

    Ok(InitProcess {
        pid,
        entry_point: executable.entry_point,
        user_stack_top,
        kernel_stack_top,
    })
}

/// Load the UI shell executable sections into memory
fn load_ui_shell_executable(
    sections: &executable::ExecutableSections,
) -> Result<LoadedExecutable, InitError> {
    let text_base = executable::USER_EXEC_LOAD_BASE;
    let text_size = align_up(sections.text.len().max(1));
    let text_pages = text_size / PAGE_SIZE;

    log_info!(
        LOG_ORIGIN,
        "Loading .text: base=0x{:X}, size={}, pages={}",
        text_base,
        text_size,
        text_pages
    );

    // Allocate and map text section
    let text_phys = pmm::alloc_pages_zeroed(text_pages)
        .ok_or(InitError::MemoryAllocationFailed)?;

    // Copy text section content
    unsafe {
        core::ptr::copy_nonoverlapping(
            sections.text.as_ptr(),
            text_phys as *mut u8,
            sections.text.len(),
        );
    }

    // Unmap any existing mappings and map text section
    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let _ = vm::unmap_page(virt);
    }

    for i in 0..text_pages {
        let virt = text_base + i * PAGE_SIZE;
        let phys = text_phys + i * PAGE_SIZE;
        vm::map_page(virt, phys, PageFlags::PRESENT | PageFlags::USER)
            .map_err(|_| InitError::MemoryAllocationFailed)?;
    }

    // Allocate and map DATA section (initialized data - strings, globals, etc.)
    let data_base = align_up(text_base + text_size);
    let data_size = align_up(sections.data.len().max(1));
    let data_pages = data_size / PAGE_SIZE;

    if !sections.data.is_empty() {
        log_info!(
            LOG_ORIGIN,
            "Loading .data: base=0x{:X}, size={}, pages={}",
            data_base,
            data_size,
            data_pages
        );

        let data_phys = pmm::alloc_pages_zeroed(data_pages)
            .ok_or(InitError::MemoryAllocationFailed)?;

        // Copy data section content
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
            vm::map_page(
                virt,
                phys,
                PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
            )
            .map_err(|_| InitError::MemoryAllocationFailed)?;
        }
    }

    // Allocate and map BSS section (zero-initialized data)
    let bss_base = align_up(data_base + data_size);
    let bss_size = sections.bss_size.max(1);
    let bss_pages = align_up(bss_size) / PAGE_SIZE;

    log_info!(
        LOG_ORIGIN,
        "Loading .bss: base=0x{:X}, size={}, pages={}",
        bss_base,
        bss_size,
        bss_pages
    );

    let bss_phys = pmm::alloc_pages_zeroed(bss_pages)
        .ok_or(InitError::MemoryAllocationFailed)?;

    for i in 0..bss_pages {
        let virt = bss_base + i * PAGE_SIZE;
        let phys = bss_phys + i * PAGE_SIZE;
        let _ = vm::unmap_page(virt);
        vm::map_page(
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        )
        .map_err(|_| InitError::MemoryAllocationFailed)?;
    }

    let entry_point = text_base + sections.entry_offset;

    log_info!(
        LOG_ORIGIN,
        "Executable loaded: text=0x{:X}, data=0x{:X}, bss=0x{:X}, entry=0x{:X}",
        text_base,
        data_base,
        bss_base,
        entry_point
    );

    Ok(LoadedExecutable {
        entry_point,
        text_base,
        data_base,
        bss_base,
    })
}

/// Allocate user stack for the UI shell process
fn allocate_user_stack() -> Result<usize, InitError> {
    let virt_base = USER_STACK_TOP - USER_STACK_SIZE;
    let phys_base = pmm::alloc_pages_zeroed(USER_STACK_PAGES)
        .ok_or(InitError::MemoryAllocationFailed)?;

    for i in 0..USER_STACK_PAGES {
        let virt = virt_base + i * PAGE_SIZE;
        let phys = phys_base + i * PAGE_SIZE;
        vm::map_page(
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        )
        .map_err(|_| InitError::MemoryAllocationFailed)?;
    }

    log_info!(
        LOG_ORIGIN,
        "User stack allocated: 0x{:X}-0x{:X} ({} pages)",
        virt_base,
        USER_STACK_TOP,
        USER_STACK_PAGES
    );

    Ok(USER_STACK_TOP)
}

/// Allocate kernel stack for syscall handling
fn allocate_kernel_stack() -> Result<u64, InitError> {
    let phys = pmm::alloc_pages(KERNEL_STACK_PAGES)
        .ok_or(InitError::MemoryAllocationFailed)?;
    let size = KERNEL_STACK_PAGES * PAGE_SIZE;
    let top = (phys + size) as u64;

    log_info!(
        LOG_ORIGIN,
        "Kernel stack allocated: phys=0x{:X}, size={} bytes",
        phys,
        size
    );

    Ok(top)
}

/// Grant framebuffer and input capabilities to the UI shell process
fn grant_ui_shell_capabilities(pid: ThreadId, boot_info: &BootInfo) -> Result<(), InitError> {
    log_info!(LOG_ORIGIN, "Granting capabilities to UI shell process");

    // Grant framebuffer capability
    let fb = &boot_info.framebuffer;
    let fb_info = graphics::get_framebuffer_info();

    if let Some((address, width, height, stride, bpp)) = fb_info {
        let fb_resource = ResourceType::Framebuffer {
            address: address as u64,
            width,
            height,
            stride,
            bytes_per_pixel: bpp as u8,
        };

        let fb_perms = CapPermissions::READ.union(CapPermissions::WRITE);
        match cap::create_root_capability(fb_resource, pid, fb_perms) {
            Ok(cap) => {
                thread::add_thread_capability(pid, cap)
                    .map_err(|_| InitError::CapabilityError)?;
                log_info!(
                    LOG_ORIGIN,
                    "Framebuffer capability granted: {}x{} @ 0x{:X}",
                    width,
                    height,
                    address
                );
            }
            Err(e) => {
                log_error!(
                    LOG_ORIGIN,
                    "Failed to create framebuffer capability: {:?}",
                    e
                );
                return Err(InitError::CapabilityError);
            }
        }
    } else {
        log_info!(
            LOG_ORIGIN,
            "No framebuffer info available, using boot info: {}x{}",
            fb.width,
            fb.height
        );
        let fb_resource = ResourceType::Framebuffer {
            address: fb.address as u64,
            width: fb.width,
            height: fb.height,
            stride: fb.pixels_per_scan_line,
            bytes_per_pixel: 4,
        };

        let fb_perms = CapPermissions::READ.union(CapPermissions::WRITE);
        cap::create_root_capability(fb_resource, pid, fb_perms)
            .map_err(|_| InitError::CapabilityError)
            .and_then(|cap| {
                thread::add_thread_capability(pid, cap)
                    .map_err(|_| InitError::CapabilityError)
            })?;
    }

    // Grant keyboard input capability
    let kbd_resource = ResourceType::InputDevice {
        device_type: InputDeviceType::Keyboard,
    };
    let kbd_perms = CapPermissions::READ;
    match cap::create_root_capability(kbd_resource, pid, kbd_perms) {
        Ok(cap) => {
            thread::add_thread_capability(pid, cap)
                .map_err(|_| InitError::CapabilityError)?;
            log_info!(LOG_ORIGIN, "Keyboard input capability granted");
        }
        Err(e) => {
            log_error!(
                LOG_ORIGIN,
                "Failed to create keyboard capability: {:?}",
                e
            );
            return Err(InitError::CapabilityError);
        }
    }

    // Grant mouse input capability
    let mouse_resource = ResourceType::InputDevice {
        device_type: InputDeviceType::Mouse,
    };
    let mouse_perms = CapPermissions::READ;
    match cap::create_root_capability(mouse_resource, pid, mouse_perms) {
        Ok(cap) => {
            thread::add_thread_capability(pid, cap)
                .map_err(|_| InitError::CapabilityError)?;
            log_info!(LOG_ORIGIN, "Mouse input capability granted");
        }
        Err(e) => {
            log_error!(LOG_ORIGIN, "Failed to create mouse capability: {:?}", e);
            return Err(InitError::CapabilityError);
        }
    }

    let stats = cap::get_capability_stats();
    log_info!(
        LOG_ORIGIN,
        "Capability stats: {} total, {} framebuffer, {} input",
        stats.total,
        stats.framebuffer_caps,
        stats.input_caps
    );

    Ok(())
}
