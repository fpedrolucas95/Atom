// kernel/src/init_process.rs
//
// Init Process Bootstrap - Loads the init process (PID 1)
//
// This module is responsible for loading and executing the init process as the
// first userspace process. The kernel does NOT contain any UI or service
// orchestration logic - all of that runs in userspace via the init process.
//
// The init process (init.atxf) is responsible for:
// - Spawning namesvc (service discovery)
// - Spawning service_manager (lifecycle management)
// - Spawning ui_shell (compositor / window manager with unified input)
// - Spawning display driver
// - Spawning applications (terminal)
//
// Key responsibilities of this module:
// - Load init.atxf from the boot payload provided by the bootloader
// - Parse and validate the ATXF executable format
// - Create a proper userspace address space for the init process
// - Grant root capabilities to the init process
// - Transfer control to the userspace process in Ring 3
//
// If the init process cannot be loaded, the system MUST fail loudly.

use crate::boot::BootInfo;
use crate::cap;
use crate::executable::{self, ExecError};
use crate::mm::pmm::{self, PAGE_SIZE};
use crate::mm::vm::{self, PageFlags};
use crate::sched;
use crate::thread::{self, CpuContext, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::{log_error, log_info, log_panic};

const LOG_ORIGIN: &str = "init";

const USER_STACK_PAGES: usize = atom_abi::DEFAULT_USER_STACK_PAGES;
const USER_STACK_TOP: usize = atom_abi::USER_STACK_TOP as usize;
const KERNEL_STACK_PAGES: usize = 16; // 64KB kernel stack to handle deep call stacks

/// Result of launching the init process
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
    #[allow(dead_code)]
    InvalidExecutable(ExecError),
    /// Failed to allocate memory for the process
    MemoryAllocationFailed,
    /// Failed to create thread
    ThreadCreationFailed,
    /// Failed to grant required capabilities
    CapabilityError,
}

/// Launch the init process from the boot payload.
///
/// This function loads init.atxf from the boot image, creates a userspace
/// process, grants it root capabilities, and schedules it for execution.
///
/// The init process will then spawn all other services and drivers:
/// namesvc, service_manager, keyboard, mouse, display, ui_shell, terminal.
///
/// The system CANNOT continue without init - there are no fallbacks.
pub fn launch_init(boot_info: &BootInfo) -> Result<InitProcess, InitError> {
    log_info!(LOG_ORIGIN, "===========================================");
    log_info!(LOG_ORIGIN, "MICROKERNEL: Loading init process (PID 1)");
    log_info!(LOG_ORIGIN, "===========================================");

    // Step 1: Validate boot payload exists
    if !boot_info.init_payload.is_present() {
        log_panic!(LOG_ORIGIN, "FATAL: No boot payload provided by bootloader!");
        log_panic!(
            LOG_ORIGIN,
            "The kernel requires init.atxf to be loaded by the bootloader."
        );
        log_panic!(
            LOG_ORIGIN,
            "Cannot continue without the init process. System halting."
        );
        return Err(InitError::NoBootPayload);
    }

    let payload_ptr = boot_info.init_payload.ptr;
    let payload_size = boot_info.init_payload.size;

    log_info!(
        LOG_ORIGIN,
        "Boot payload: ptr=0x{:X}, size={} bytes",
        payload_ptr as usize,
        payload_size
    );

    // Authenticate and validate the complete ATXF v3 image before mapping.
    let payload_bytes = unsafe { core::slice::from_raw_parts(payload_ptr, payload_size) };

    let image = match executable::parse_image(payload_bytes) {
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
        "ATXF v3 authenticated: segments={}, relocations={}, entry_offset=0x{:X}",
        image.segments.len(),
        image.relocations.len(),
        image.entry_offset
    );

    // Step 4: Create the init process
    let pid = ThreadId::new();
    log_info!(LOG_ORIGIN, "Creating init process with PID {}", pid);

    let init = create_init_process(pid, &image)?;

    log_info!(
        LOG_ORIGIN,
        "Init process created: entry=0x{:X}, stack=0x{:X}",
        init.entry_point,
        init.user_stack_top
    );

    // Step 5: Grant capabilities to the init process
    // Init gets root capabilities so it can spawn and manage all services
    grant_init_capabilities(pid)?;

    log_info!(LOG_ORIGIN, "===========================================");
    log_info!(LOG_ORIGIN, "Init process ready for execution");
    log_info!(LOG_ORIGIN, "Init will spawn: namesvc, service_manager,");
    log_info!(LOG_ORIGIN, "  fsd, app_launcher, ui_shell, display,");
    log_info!(LOG_ORIGIN, "  nic_driver, netd, terminal");
    log_info!(LOG_ORIGIN, "===========================================");

    Ok(init)
}

/// Create the init userspace process
fn create_init_process(
    pid: ThreadId,
    image: &executable::ExecutableImageV2<'_>,
) -> Result<InitProcess, InitError> {
    use crate::mm::vma::{self, PageSource, Vma, VmaBacking, VmaPermissions};

    // Create a new address space for init (no longer shares kernel_cr3)
    let init_pml4_phys = pmm::alloc_pages_zeroed(1).ok_or(InitError::MemoryAllocationFailed)?;
    let process_id = crate::process::ProcessId::from(pid);

    log_info!(
        LOG_ORIGIN,
        "Allocated new PML4 for init: 0x{:X}",
        init_pml4_phys
    );

    // Clone kernel mappings to the new address space
    vm::clone_kernel_mappings(init_pml4_phys).map_err(|_| InitError::MemoryAllocationFailed)?;

    // Register the PML4 as protected (Req 2.4)
    let _ = pmm::register_active_pml4(init_pml4_phys);

    log_info!(LOG_ORIGIN, "Cloned kernel mappings to init's PML4");

    vma::create_bootstrap_process_vma_map(process_id, init_pml4_phys);

    // Load executable into memory (using init's own PML4)
    let executable = executable::load_into_process(image, init_pml4_phys, process_id)
        .map_err(InitError::InvalidExecutable)?;
    let user_stack_top = allocate_user_stack(init_pml4_phys)?;
    let kernel_stack_top = allocate_kernel_stack()?;

    let stack_layout = thread::UserStackMetadata::default_for_top(USER_STACK_TOP);
    let user_stack_base = stack_layout.usable_base as usize;
    let heap_start = atom_abi::USER_HEAP_START as usize;

    vma::insert_bootstrap_process_vma(
        process_id,
        init_pml4_phys,
        Vma {
            start: user_stack_base,
            end: USER_STACK_TOP,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "stack",
        },
    )
    .map_err(|_| InitError::MemoryAllocationFailed)?;
    vma::account_pre_mapped_range(
        process_id,
        init_pml4_phys,
        user_stack_base,
        USER_STACK_TOP,
        PageSource::Anonymous,
    )
    .map_err(|_| InitError::MemoryAllocationFailed)?;

    vma::insert_bootstrap_process_vma(
        process_id,
        init_pml4_phys,
        Vma {
            start: heap_start,
            end: heap_start + PAGE_SIZE,
            perms: VmaPermissions::read_write(),
            backing: VmaBacking::Anonymous,
            label: "heap",
        },
    )
    .map_err(|_| InitError::MemoryAllocationFailed)?;

    // Create CPU context for Ring 3 execution with init's own PML4
    let context = CpuContext::new_user(
        executable.entry_point as u64,
        user_stack_top as u64,
        init_pml4_phys as u64,
    );

    log_info!(
        LOG_ORIGIN,
        "User context: RIP=0x{:016X} RSP=0x{:016X} CS=0x{:04X} SS=0x{:04X}",
        context.rip,
        context.rsp,
        context.cs,
        context.ss
    );

    // CRITICAL: Write stack canary (init_process creates Thread manually, bypassing Thread::new)
    unsafe {
        const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;
        let bottom = kernel_stack_top - (KERNEL_STACK_PAGES * PAGE_SIZE) as u64;
        let canary_addr = bottom as *mut u64;
        core::ptr::write_volatile(canary_addr, STACK_CANARY);

        // Verify write
        let readback = core::ptr::read_volatile(canary_addr);
        crate::serial_println!(
            "[CANARY_SET] tid={} name=init addr={:#X} val={:#X} (expected={:#X}) top={:#X} bottom={:#X} size={}",
            pid, canary_addr as u64, readback, STACK_CANARY,
            kernel_stack_top, bottom, KERNEL_STACK_PAGES * PAGE_SIZE
        );

        if readback != STACK_CANARY {
            crate::serial_println!(
                "[CANARY_SET] WARNING: Canary read-back mismatch! Got {:#X} expected {:#X}",
                readback,
                STACK_CANARY
            );
        }
    }

    // Create the thread with its own capability table and isolated address space
    // Init gets High priority as it orchestrates the entire boot
    let thread = Thread {
        id: pid,
        process_id: Some(process_id),
        state: ThreadState::Ready,
        context: alloc::boxed::Box::new(context),
        kernel_stack: kernel_stack_top,
        kernel_stack_size: KERNEL_STACK_PAGES * PAGE_SIZE,
        address_space: init_pml4_phys as u64,
        priority: ThreadPriority::High,
        name: "init",
        capability_table: cap::create_capability_table(pid),
        is_userspace: true,
        user_stack: Some(stack_layout),
    };

    thread::add_thread(thread);
    sched::mark_thread_ready(pid);

    // Verification: Log init's CR3 and confirm it differs from kernel CR3
    let kernel_cr3 = crate::arch::read_cr3() as usize;
    log_info!(
        LOG_ORIGIN,
        "ISOLATION VERIFICATION: Init PML4=0x{:X}, Kernel CR3=0x{:X} (isolated={})",
        init_pml4_phys,
        kernel_cr3,
        init_pml4_phys != kernel_cr3
    );

    if init_pml4_phys == kernel_cr3 {
        log_panic!(
            LOG_ORIGIN,
            "CRITICAL: Init is using kernel CR3! Isolation failed!"
        );
        return Err(InitError::ThreadCreationFailed);
    }

    // Verify that kernel pages in init's PML4 are supervisor-only (no USER bit)
    // Check a kernel address (the kernel code at higher half)
    let kernel_test_addr = vm::HIGHER_HALF_BASE;
    if let Ok((_phys, flags)) = vm::query_mapping_in_pml4(init_pml4_phys, kernel_test_addr) {
        let has_user_bit = (flags.bits() & PageFlags::USER.bits()) != 0;
        log_info!(
            LOG_ORIGIN,
            "Kernel mapping check: addr=0x{:X}, flags=0x{:X}, USER_bit={}",
            kernel_test_addr,
            flags.bits(),
            has_user_bit
        );

        if has_user_bit {
            log_panic!(
                LOG_ORIGIN,
                "CRITICAL: Kernel pages have USER bit! Init can access kernel memory!"
            );
            return Err(InitError::ThreadCreationFailed);
        }
    }

    log_info!(
        LOG_ORIGIN,
        "✓ Isolation verified: Init has own PML4 and kernel pages are supervisor-only"
    );

    Ok(InitProcess {
        pid,
        entry_point: executable.entry_point,
        user_stack_top,
        kernel_stack_top,
    })
}

/// Allocate user stack for the init process (in the specified PML4)
fn allocate_user_stack(pml4_phys: usize) -> Result<usize, InitError> {
    let stack_layout = thread::UserStackMetadata::default_for_top(USER_STACK_TOP);
    let virt_base = stack_layout.usable_base as usize;
    let phys_base =
        pmm::alloc_pages_zeroed(USER_STACK_PAGES).ok_or(InitError::MemoryAllocationFailed)?;

    for i in 0..USER_STACK_PAGES {
        let virt = virt_base + i * PAGE_SIZE;
        let phys = phys_base + i * PAGE_SIZE;
        vm::remap_page_in_pml4(
            pml4_phys,
            virt,
            phys,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE | PageFlags::NO_EXECUTE,
        )
        .map_err(|_| InitError::MemoryAllocationFailed)?;
    }

    log_info!(
        LOG_ORIGIN,
        "User stack allocated: guard=0x{:X}-0x{:X}, usable=0x{:X}-0x{:X} ({} mapped pages)",
        stack_layout.guard_base,
        stack_layout.usable_base,
        virt_base,
        USER_STACK_TOP,
        USER_STACK_PAGES
    );

    Ok(USER_STACK_TOP)
}

/// Allocate kernel stack for syscall handling
///
/// CRITICAL: Returns a higher-half virtual address, NOT an identity-mapped address.
/// Child processes only share the upper-half PML4 entries (256-511) with the kernel.
/// The lower-half identity mappings are deep-copied at clone time and may diverge
/// as user pages are remapped into the child's address space.  Using a higher-half
/// address guarantees the kernel stack is always reachable regardless of which CR3
/// is active (e.g., during context-switch trampolines that load a new CR3 before
/// building the IRET frame on the current stack).
fn allocate_kernel_stack() -> Result<u64, InitError> {
    let phys = pmm::alloc_pages(KERNEL_STACK_PAGES).ok_or(InitError::MemoryAllocationFailed)?;
    let size = KERNEL_STACK_PAGES * PAGE_SIZE;
    let virt = vm::HIGHER_HALF_BASE + phys;
    let top = (virt + size) as u64;

    log_info!(
        LOG_ORIGIN,
        "Kernel stack allocated: phys=0x{:X}, virt=0x{:X}, size={} bytes",
        phys,
        virt,
        size
    );

    Ok(top)
}

/// Grant capabilities to the init process.
///
/// Least privilege (PR3): init is the bootstrap orchestrator and receives ONLY
/// the bootstrap authority declared in the SystemServiceManifest
/// (`SpawnSystemService` + `ReadKernelLog` + its `ServiceIdentity`). It does NOT
/// receive framebuffer or input capabilities — those are owned by the
/// compositor (ui_shell), granted directly from its own manifest profile at
/// spawn. init never delegates ambient graphics/input authority.
fn grant_init_capabilities(pid: ThreadId) -> Result<(), InitError> {
    log_info!(
        LOG_ORIGIN,
        "Granting bootstrap capabilities to init process"
    );

    // init is the first userspace process, but it is NOT privileged by virtue of
    // being first. Its authority comes entirely from the SystemServiceManifest.
    grant_init_manifest_capabilities(pid)?;

    let stats = cap::get_capability_stats();
    log_info!(
        LOG_ORIGIN,
        "Capability stats: {} total, {} framebuffer, {} input (init holds no fb/input)",
        stats.total,
        stats.framebuffer_caps,
        stats.input_caps
    );

    Ok(())
}

/// Assign init's kernel-defined identity and grant the capabilities declared
/// for it in the [`crate::system_manifest::SystemServiceManifest`].
fn grant_init_manifest_capabilities(pid: ThreadId) -> Result<(), InitError> {
    use crate::system_manifest::{self, SID_INIT};

    let process_id = crate::process::ProcessId::from(pid);
    let entry = system_manifest::lookup_by_id(SID_INIT)
        .expect("init must be declared in the SystemServiceManifest");

    system_manifest::assign_service_identity(process_id, entry.service_id);

    for init_cap in entry.initial_capabilities {
        match cap::create_root_capability(init_cap.resource, pid, init_cap.permissions) {
            Ok(granted) => {
                thread::add_thread_capability(pid, granted)
                    .map_err(|_| InitError::CapabilityError)?;
                log_info!(
                    LOG_ORIGIN,
                    "Granted init manifest capability {:?}",
                    init_cap.resource
                );
            }
            Err(e) => {
                log_error!(
                    LOG_ORIGIN,
                    "Failed to grant init manifest capability {:?}: {:?}",
                    init_cap.resource,
                    e
                );
                return Err(InitError::CapabilityError);
            }
        }
    }

    Ok(())
}
