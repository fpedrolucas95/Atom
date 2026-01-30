// Thread Management Subsystem
//
// Implements the core thread abstraction for the Atom kernel, including
// thread creation, lifecycle management, CPU context handling, and
// capability association. This module forms the foundation on which
// scheduling, IPC, and syscall execution are built.
//
// Key responsibilities:
// - Define thread identity, state, and priority
// - Maintain the Thread Control Block (TCB) and CPU execution context
// - Manage a global list of all threads in the system
// - Integrate per-thread capability tables for security enforcement
// - Provide low-level context switching primitives
//
// Thread model:
// - Each thread has a unique `ThreadId`, state, priority, and name
// - Threads transition through: Ready → Running → Blocked / Exited
// - An explicit idle thread is supported by the scheduler
// - Threads are kernel-managed (no user-level threading yet)
//
// CPU context handling:
// - `CpuContext` mirrors the full architectural register set (x86_64)
// - Context includes general-purpose registers, segment selectors, flags,
//   instruction pointer, stack pointer, and CR3 (address space)
// - Context switch is performed by architecture-specific assembly stubs
// - `capture_current_context` snapshots the live CPU state for preemption
//
// Scheduling integration:
// - Thread state is manipulated by the scheduler (`sched` module)
// - Runnable threads are discovered via state inspection
// - Priority information is consumed by the scheduler’s ready queues
//
// Capability integration:
// - Every thread owns a `CapabilityTable`
// - Capability validation is enforced at the thread level
// - Helpers exist to add, remove, query, and validate capabilities
// - Enables capability-based security to be thread-centric
//
// Global thread management:
// - All threads are stored in a global `ThreadList` protected by a spinlock
// - Provides thread lookup, removal, statistics, and state updates
// - Designed for simplicity and correctness over scalability
//
// Correctness and safety notes:
// - Global mutable state relies on spinlocks and careful sequencing
// - Context switching and register capture use `unsafe` inline assembly
// - Assumes uniprocessor (single-core) execution for now
// - No stack guards or user/kernel stack separation yet
//
// Design intent and future evolution:
// - Intended as a minimal kernel thread layer
// - Will later support:
//   - User-space threads/processes
//   - Separate user and kernel stacks
//   - Stronger isolation and per-process address spaces
//   - SMP-aware scheduling and per-CPU data
//
// This module is a critical low-level component and is intentionally
// explicit, conservative, and closely aligned with the underlying hardware.

#![allow(dead_code)]

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use crate::arch::gdt;
use crate::{log_debug, log_error, log_info, log_panic, log_warn};

use crate::cap::CapabilityTable;

/// Global resource counters for tracking and validating cleanup
pub struct ResourceCounters {
    pub threads_created: AtomicUsize,
    pub threads_terminated: AtomicUsize,
    pub kernel_stacks_allocated: AtomicUsize,
    pub kernel_stacks_freed: AtomicUsize,
    pub physical_pages_freed_on_termination: AtomicUsize,
}

impl ResourceCounters {
    pub const fn new() -> Self {
        Self {
            threads_created: AtomicUsize::new(0),
            threads_terminated: AtomicUsize::new(0),
            kernel_stacks_allocated: AtomicUsize::new(0),
            kernel_stacks_freed: AtomicUsize::new(0),
            physical_pages_freed_on_termination: AtomicUsize::new(0),
        }
    }

    pub fn log_stats(&self) {
        log_info!(
            LOG_ORIGIN,
            "Resource stats: threads={}/{}, stacks={}/{}, pages_freed={}",
            self.threads_created.load(Ordering::Relaxed),
            self.threads_terminated.load(Ordering::Relaxed),
            self.kernel_stacks_allocated.load(Ordering::Relaxed),
            self.kernel_stacks_freed.load(Ordering::Relaxed),
            self.physical_pages_freed_on_termination.load(Ordering::Relaxed)
        );
    }
}

pub static RESOURCE_COUNTERS: ResourceCounters = ResourceCounters::new();

/// Reason for thread/process termination
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    /// Normal exit via SYS_THREAD_EXIT or SYS_PROCESS_EXIT
    NormalExit { exit_code: u64 },
    /// Killed due to page fault
    PageFault { address: u64, error_code: u64, rip: u64 },
    /// Killed due to general protection fault
    GeneralProtectionFault { error_code: u64, rip: u64 },
    /// Killed due to other exception
    Exception { vector: u64, error_code: u64, rip: u64 },
    /// Killed by kernel (resource exhaustion, policy violation, etc.)
    KilledByKernel { reason_code: u64 },
    /// Killed by watchdog (unresponsive, timeout, etc.)
    Watchdog { timeout_ms: u64 },
}

impl TerminationReason {
    pub fn exit_code(&self) -> u64 {
        match self {
            TerminationReason::NormalExit { exit_code } => *exit_code,
            TerminationReason::PageFault { .. } => 0xFFFF_FFFF_0000_000E,
            TerminationReason::GeneralProtectionFault { .. } => 0xFFFF_FFFF_0000_000D,
            TerminationReason::Exception { vector, .. } => 0xFFFF_FFFF_0000_0000 | vector,
            TerminationReason::KilledByKernel { reason_code } => 0xFFFF_FFFE_0000_0000 | reason_code,
            TerminationReason::Watchdog { .. } => 0xFFFF_FFFD_0000_0000,
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            TerminationReason::NormalExit { .. } => "normal exit",
            TerminationReason::PageFault { .. } => "page fault",
            TerminationReason::GeneralProtectionFault { .. } => "general protection fault",
            TerminationReason::Exception { .. } => "exception",
            TerminationReason::KilledByKernel { .. } => "killed by kernel",
            TerminationReason::Watchdog { .. } => "watchdog timeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Running,
    Ready,
    Blocked,
    WaitingIpc,
    Exited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThreadPriority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
}

impl Default for ThreadPriority {
    fn default() -> Self {
        ThreadPriority::Normal
    }
}

const LOG_ORIGIN: &str = "thread";
const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CpuContext {
    pub rax: u64,
    pub rbx: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub rbp: u64,
    pub rsp: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rip: u64,
    pub rflags: u64,
    pub cs: u16,
    pub ss: u16,
    pub ds: u16,
    pub es: u16,
    pub fs: u16,
    pub gs: u16,
    pub cr3: u64,
}

impl CpuContext {
    pub const fn zero() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: 0,
            rflags: 0,
            cs: 0,
            ss: 0,
            ds: 0,
            es: 0,
            fs: 0,
            gs: 0,
            cr3: 0,
        }
    }

    pub fn new(entry_point: u64, stack_pointer: u64, page_table: u64) -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: 0,
            rsp: stack_pointer,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: entry_point,
            rflags: 0x200,
            cs: 0x08,
            ss: 0x10,
            ds: 0x10,
            es: 0x10,
            fs: 0x10,
            gs: 0x10,
            cr3: page_table,
        }
    }

    /// Create a kernel-mode (Ring 0) context
    pub fn new_kernel(entry_point: u64, kernel_stack: u64) -> Self {
        use crate::arch::read_cr3;
        Self::new(entry_point, kernel_stack, read_cr3())
    }

    pub fn new_user(entry_point: u64, user_stack: u64, page_table: u64) -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rbp: user_stack - 16,  // Initialize RBP to stack base for frame pointer compatibility
            rsp: user_stack - 16,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rip: entry_point,
            rflags: 0x202,
            cs: gdt::USER_CODE_SELECTOR,
            ss: gdt::USER_DATA_SELECTOR,
            ds: gdt::USER_DATA_SELECTOR,
            es: gdt::USER_DATA_SELECTOR,
            fs: gdt::USER_DATA_SELECTOR,
            gs: gdt::USER_DATA_SELECTOR,
            cr3: page_table,
        }
    }
}

#[derive(Debug)]
pub struct Thread {
    pub id: ThreadId,
    pub state: ThreadState,
    pub context: CpuContext,
    pub kernel_stack: u64,
    pub kernel_stack_size: usize,
    pub address_space: u64,
    pub priority: ThreadPriority,
    pub name: &'static str,
    pub capability_table: CapabilityTable,
    /// Whether this thread executes in Ring 3 (userspace).
    /// This flag is set at creation time and never changes.
    /// It determines whether context switches use enter_user* (IRET to Ring 3)
    /// or switch_context (kernel-to-kernel). This avoids fragile heuristics
    /// based on address_space comparison or context.cs inspection (which
    /// reflects kernel CS after a syscall save, not the thread's true CPL).
    pub is_userspace: bool,
}

impl Thread {
    pub fn new(
        entry_point: u64,
        kernel_stack: u64,
        kernel_stack_size: usize,
        address_space: u64,
        priority: ThreadPriority,
        name: &'static str,
    ) -> Self {
        let id = ThreadId::new();
        let context = CpuContext::new(entry_point, kernel_stack, address_space);
        let capability_table = crate::cap::create_capability_table(id);
        
        // Write canary and immediately verify it
        let _canary_addr = unsafe {
            let bottom = kernel_stack
                .wrapping_sub(kernel_stack_size as u64);
            let canary_addr = bottom as *mut u64;
            core::ptr::write_volatile(canary_addr, STACK_CANARY);
            
            // Read back to verify write succeeded
            let readback = core::ptr::read_volatile(canary_addr);
            
            if readback != STACK_CANARY {
                log_error!(
                    LOG_ORIGIN,
                    "tid={} name={} Canary read-back mismatch! Got {:#X} expected {:#X}",
                    id, name, readback, STACK_CANARY
                );
            }
            
            canary_addr as u64
        };

        Self {
            id,
            state: ThreadState::Ready,
            context,
            kernel_stack,
            kernel_stack_size,
            address_space,
            priority,
            name,
            capability_table,
            is_userspace: false,
        }
    }

    pub fn new_idle(entry_point: u64, kernel_stack: u64, kernel_stack_size: usize) -> Self {
        Self::new(
            entry_point,
            kernel_stack,
            kernel_stack_size,
            0,
            ThreadPriority::Idle,
            "idle",
        )
    }
    
    pub fn validate_stack(&self) -> bool {
        unsafe {
            let bottom = self.kernel_stack
                .wrapping_sub(self.kernel_stack_size as u64);
            let canary_addr = bottom as *const u64;
            core::ptr::read_volatile(canary_addr) == STACK_CANARY
        }
    }

    pub fn id(&self) -> ThreadId {
        self.id
    }

    pub fn state(&self) -> ThreadState {
        self.state
    }

    pub fn set_state(&mut self, state: ThreadState) {
        self.state = state;
    }

    pub fn is_runnable(&self) -> bool {
        matches!(self.state, ThreadState::Ready | ThreadState::Running)
    }

    pub fn capability_table(&self) -> &CapabilityTable {
        &self.capability_table
    }

    pub fn capability_table_mut(&mut self) -> &mut CapabilityTable {
        &mut self.capability_table
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(u64);

impl ThreadId {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        ThreadId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(&self) -> u64 {
        self.0
    }

    pub fn from_raw(value: u64) -> Self {
        ThreadId(value)
    }
}

impl Default for ThreadId {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for ThreadId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

pub struct ThreadList {
    threads: Mutex<Vec<Thread>>,
}

impl ThreadList {
    pub const fn new() -> Self {
        Self {
            threads: Mutex::new(Vec::new()),
        }
    }

    pub fn add(&self, thread: Thread) {
        let mut threads = self.threads.lock();
        threads.push(thread);
    }

    pub fn remove(&self, id: ThreadId) -> Option<Thread> {
        let mut threads = self.threads.lock();
        if let Some(pos) = threads.iter().position(|t| t.id == id) {
            Some(threads.remove(pos))
        } else {
            None
        }
    }

    pub fn find(&self, id: ThreadId) -> Option<ThreadId> {
        let threads = self.threads.lock();
        threads.iter().find(|t| t.id == id).map(|t| t.id)
    }

    pub fn count(&self) -> usize {
        let threads = self.threads.lock();
        threads.len()
    }

    pub fn get_runnable(&self) -> Vec<ThreadId> {
        let threads = self.threads.lock();
        threads
            .iter()
            .filter(|t| t.is_runnable())
            .map(|t| t.id)
            .collect()
    }

    pub fn set_state(&self, id: ThreadId, state: ThreadState) -> bool {
        let mut threads = self.threads.lock();
        if let Some(thread) = threads.iter_mut().find(|t| t.id == id) {
            thread.set_state(state);
            true
        } else {
            false
        }
    }

    fn get_state(&self, id: ThreadId) -> Option<ThreadState> {
        let threads = self.threads.lock();
        threads.iter().find(|t| t.id == id).map(|t| t.state)
    }

    pub fn get_stats(&self) -> ThreadStats {
        let threads = self.threads.lock();
        let mut stats = ThreadStats::default();

        stats.total = threads.len();
        for thread in threads.iter() {
            match thread.state {
                ThreadState::Running => stats.running += 1,
                ThreadState::Ready => stats.ready += 1,
                ThreadState::Blocked => stats.blocked += 1,
                ThreadState::WaitingIpc => stats.blocked += 1,
                ThreadState::Exited => stats.exited += 1,
            }
        }

        stats
    }

    pub fn with_contexts<F, R>(&self, from_id: ThreadId, to_id: ThreadId, f: F) -> Option<R>
    where
        F: FnOnce(&mut CpuContext, &CpuContext) -> R,
    {
        let mut threads = self.threads.lock();

        let from_idx = threads.iter().position(|t| t.id == from_id)?;
        let to_idx = threads.iter().position(|t| t.id == to_id)?;

        if from_idx < to_idx {
            let (left, right) = threads.split_at_mut(to_idx);
            Some(f(&mut left[from_idx].context, &right[0].context))
        } else if from_idx > to_idx {
            let (left, right) = threads.split_at_mut(from_idx);
            Some(f(&mut right[0].context, &left[to_idx].context))
        } else {
            let ctx_copy = threads[from_idx].context;
            Some(f(&mut threads[from_idx].context, &ctx_copy))
        }
    }
    
    fn snapshot_thread(&self, id: ThreadId) -> Option<Thread> {
        let threads = self.threads.lock();
        threads.iter().find(|t| t.id == id).map(|t| Thread {
            id: t.id,
            state: t.state,
            context: t.context,
            kernel_stack: t.kernel_stack,
            kernel_stack_size: t.kernel_stack_size,
            address_space: t.address_space,
            priority: t.priority,
            name: t.name,
            capability_table: crate::cap::create_capability_table(t.id),
            is_userspace: t.is_userspace,
        })
    }

    pub fn validate_capability(
        &self,
        thread_id: ThreadId,
        cap_handle: crate::cap::CapHandle,
        required_permission: crate::cap::CapPermissions,
    ) -> Result<(), crate::cap::CapError> {
        let threads = self.threads.lock();
        let thread = threads
            .iter()
            .find(|t| t.id == thread_id)
            .ok_or(crate::cap::CapError::NotFound)?;

        thread.capability_table.validate(cap_handle, required_permission)?;
        Ok(())
    }

    pub fn add_capability(
        &self,
        thread_id: ThreadId,
        capability: crate::cap::Capability,
    ) -> Result<crate::cap::CapHandle, crate::cap::CapError> {
        let mut threads = self.threads.lock();
        let thread = threads
            .iter_mut()
            .find(|t| t.id == thread_id)
            .ok_or(crate::cap::CapError::NotFound)?;

        thread.capability_table.insert(capability)
    }

    pub fn remove_capability(
        &self,
        thread_id: ThreadId,
        cap_handle: crate::cap::CapHandle,
    ) -> Option<crate::cap::Capability> {
        let mut threads = self.threads.lock();
        let thread = threads.iter_mut().find(|t| t.id == thread_id)?;

        thread.capability_table.remove(cap_handle)
    }

    pub fn has_capability(
        &self,
        thread_id: ThreadId,
        cap_handle: crate::cap::CapHandle,
    ) -> bool {
        let threads = self.threads.lock();
        if let Some(thread) = threads.iter().find(|t| t.id == thread_id) {
            thread.capability_table.contains(cap_handle)
        } else {
            false
        }
    }

    pub fn validate_capability_by_type<F>(
        &self,
        thread_id: ThreadId,
        required_permission: crate::cap::CapPermissions,
        resource_filter: F,
    ) -> bool
    where
        F: Fn(&crate::cap::ResourceType) -> bool,
    {
        let threads = self.threads.lock();
        if let Some(thread) = threads.iter().find(|t| t.id == thread_id) {
            for handle in thread.capability_table.list() {
                if let Some(cap) = thread.capability_table.get(handle) {
                    if resource_filter(&cap.resource) && cap.has_permission(required_permission) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ThreadStats {
    pub total: usize,
    pub running: usize,
    pub ready: usize,
    pub blocked: usize,
    pub exited: usize,
}

static THREAD_LIST: ThreadList = ThreadList::new();
static USERMODE_ENTRIES: Mutex<BTreeSet<ThreadId>> = Mutex::new(BTreeSet::new());

pub fn init() {
    log_info!(
        LOG_ORIGIN,
        "Threading subsystem initialized"
    );
}

pub fn add_thread(thread: Thread) {
    THREAD_LIST.add(thread);
}

pub fn remove_thread(id: ThreadId) -> Option<Thread> {
    THREAD_LIST.remove(id)
}

pub fn find_thread(id: ThreadId) -> Option<ThreadId> {
    THREAD_LIST.find(id)
}

pub fn thread_count() -> usize {
    THREAD_LIST.count()
}

pub fn log_user_entry_once(thread_id: ThreadId, ctx: &CpuContext) {
    let mut entries = USERMODE_ENTRIES.lock();
    if entries.insert(thread_id) {
        log_debug!(
            LOG_ORIGIN,
            "Thread {} entering user mode: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X}",
            thread_id,
            ctx.rip,
            ctx.rsp,
            ctx.cs,
            ctx.ss
        );
        
        // DEBUG: Log the actual IRET frame that was built in switch.asm
        extern "C" {
            static DEBUG_IRET_RIP: u64;
            static DEBUG_IRET_CS: u64;
            static DEBUG_IRET_RFLAGS: u64;
            static DEBUG_IRET_RSP: u64;
            static DEBUG_IRET_SS: u64;
            static DEBUG_IRET_KERNEL_RSP: u64;
        }
        
        unsafe {
            log_debug!(
                "DEBUG_IRET",
                "IRET frame @ kernel RSP={:#016X}:",
                DEBUG_IRET_KERNEL_RSP
            );
            log_debug!(
                "DEBUG_IRET",
                "  [rsp+0]  RIP    = {:#016X} (expected: {:#016X})",
                DEBUG_IRET_RIP,
                ctx.rip
            );
            log_debug!(
                "DEBUG_IRET",
                "  [rsp+8]  CS     = {:#016X} (expected: 0x000000000000001B)",
                DEBUG_IRET_CS
            );
            log_debug!(
                "DEBUG_IRET",
                "  [rsp+16] RFLAGS = {:#016X}",
                DEBUG_IRET_RFLAGS
            );
            log_debug!(
                "DEBUG_IRET",
                "  [rsp+24] RSP    = {:#016X} (expected: {:#016X})",
                DEBUG_IRET_RSP,
                ctx.rsp
            );
            log_debug!(
                "DEBUG_IRET",
                "  [rsp+32] SS     = {:#016X} (expected: 0x0000000000000023)",
                DEBUG_IRET_SS
            );
            
            // Check if values match expectations
            // Skip 0x0 values which occur during initial user transition before variables are set
            if DEBUG_IRET_CS != 0 && DEBUG_IRET_CS != 0x1B {
                log_error!(
                    "DEBUG_IRET",
                    "!!! CS is NOT 0x1B - actual value is {:#X} !!!",
                    DEBUG_IRET_CS
                );
            }
            if DEBUG_IRET_SS != 0 && DEBUG_IRET_SS != 0x23 {
                log_error!(
                    "DEBUG_IRET",
                    "!!! SS is NOT 0x23 - actual value is {:#X} !!!",
                    DEBUG_IRET_SS
                );
            }
        }
    }
}

pub fn get_runnable_threads() -> Vec<ThreadId> {
    THREAD_LIST.get_runnable()
}

pub fn set_thread_state(id: ThreadId, state: ThreadState) -> bool {
    THREAD_LIST.set_state(id, state)
}

pub fn get_thread_state(id: ThreadId) -> Option<ThreadState> {
    THREAD_LIST.get_state(id)
}

pub fn get_thread_stats() -> ThreadStats {
    THREAD_LIST.get_stats()
}

/// Get the address_space (PML4) of a thread
pub fn get_thread_address_space(thread_id: ThreadId) -> Option<u64> {
    let threads = THREAD_LIST.threads.lock();
    threads.iter().find(|t| t.id == thread_id).map(|t| t.address_space)
}

pub fn validate_thread_capability(
    thread_id: ThreadId,
    cap_handle: crate::cap::CapHandle,
    required_permission: crate::cap::CapPermissions,
) -> Result<(), crate::cap::CapError> {
    THREAD_LIST.validate_capability(thread_id, cap_handle, required_permission)
}

pub fn add_thread_capability(
    thread_id: ThreadId,
    capability: crate::cap::Capability,
) -> Result<crate::cap::CapHandle, crate::cap::CapError> {
    THREAD_LIST.add_capability(thread_id, capability)
}

pub fn remove_thread_capability(
    thread_id: ThreadId,
    cap_handle: crate::cap::CapHandle,
) -> Option<crate::cap::Capability> {
    THREAD_LIST.remove_capability(thread_id, cap_handle)
}

pub fn thread_has_capability(
    thread_id: ThreadId,
    cap_handle: crate::cap::CapHandle,
) -> bool {
    THREAD_LIST.has_capability(thread_id, cap_handle)
}

pub fn validate_thread_capability_by_type<F>(
    thread_id: ThreadId,
    required_permission: crate::cap::CapPermissions,
    resource_filter: F,
) -> bool
where
    F: Fn(&crate::cap::ResourceType) -> bool,
{
    THREAD_LIST.validate_capability_by_type(thread_id, required_permission, resource_filter)
}

extern "C" {
    fn switch_context(old_context: *mut CpuContext, new_context: *const CpuContext);
    pub(crate) fn switch_to_context(new_context: *const CpuContext) -> !;
    pub(crate) fn enter_user(rip: u64, rsp: u64, cr3: u64) -> !;
    pub(crate) fn enter_user_first_time(rip: u64, rsp: u64, cr3: u64) -> !;
    pub(crate) fn enter_user_resume(rip: u64, rsp: u64, cr3: u64, regs_ptr: *const crate::syscall::UserspaceGprs) -> !;
}

fn validate_context_for_iret(target: &CpuContext) -> Result<(), &'static str> {
    let rip_canonical = is_canonical(target.rip);
    let rsp_canonical = is_canonical(target.rsp);

    if !rip_canonical || !rsp_canonical {
        return Err("non-canonical RIP or RSP");
    }

    let user_mode = (target.cs & 0x3) == 0x3;

    if user_mode {
        if target.cs != gdt::USER_CODE_SELECTOR {
            return Err("user CS selector invalid");
        }

        if target.ss != gdt::USER_DATA_SELECTOR {
            return Err("user SS selector invalid");
        }
    } else {
        if target.cs != gdt::KERNEL_CODE_SELECTOR {
            return Err("kernel CS selector invalid");
        }

        if target.ss != gdt::KERNEL_DATA_SELECTOR {
            return Err("kernel SS selector invalid");
        }
    }

    Ok(())
}

fn guard_context_or_halt(target: &CpuContext, label: &str) {
    if let Err(reason) = validate_context_for_iret(target) {
        log_panic!(
            LOG_ORIGIN,
            "Refusing to iret to {} context: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X} reason={}",
            label,
            target.rip,
            target.rsp,
            target.cs,
            target.ss,
            reason
        );
    }
}

pub unsafe fn switch_thread_context(current: &mut CpuContext, next: &CpuContext) {
    guard_context_or_halt(next, "scheduled");
    switch_context(current as *mut CpuContext, next as *const CpuContext);
}

pub unsafe fn jump_to_context(context: &CpuContext) -> ! {
    guard_context_or_halt(context, "initial");
    
    // For first-time user entry, use dedicated enter_user to avoid complex switch logic
    let is_user_mode = (context.cs & 0x3) == 0x3;
    if is_user_mode {
        log_debug!(
            LOG_ORIGIN,
            "Using enter_user for first user jump: RIP={:#016X} RSP={:#016X} CR3={:#016X}",
            context.rip,
            context.rsp,
            context.cr3
        );
        enter_user(context.rip, context.rsp, context.cr3)
    } else {
        switch_to_context(context as *const CpuContext)
    }
}

pub fn jump_to_thread(thread_id: ThreadId) -> ! {
    let (ctx_copy, kernel_stack) = {
        let threads = THREAD_LIST.threads.lock();
        let thread = threads
            .iter()
            .find(|t| t.id == thread_id)
            .expect("Thread not found");

        (thread.context, thread.kernel_stack)
    };

    gdt::set_rsp0(kernel_stack);

    if (ctx_copy.cs & 0x3) == 0x3 {
        log_user_entry_once(thread_id, &ctx_copy);
    }

    unsafe {
        jump_to_context(&ctx_copy)
    }
}

pub fn kernel_stack_top(thread_id: ThreadId) -> Option<u64> {
    let threads = THREAD_LIST.threads.lock();
    threads
        .iter()
        .find(|t| t.id == thread_id)
        .map(|t| t.kernel_stack)
}

/// Process/thread information for userspace queries
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProcessInfo {
    pub pid: u64,
    pub state: u8,  // 0=Running, 1=Ready, 2=Blocked, 3=Exited
    pub name: [u8; 32],
}

impl ProcessInfo {
    fn from_thread(thread: &Thread) -> Self {
        let mut name = [0u8; 32];
        let name_bytes = thread.name.as_bytes();
        let copy_len = name_bytes.len().min(31);
        name[..copy_len].copy_from_slice(&name_bytes[..copy_len]);

        ProcessInfo {
            pid: thread.id.raw(),
            state: match thread.state {
                ThreadState::Running => 0,
                ThreadState::Ready => 1,
                ThreadState::Blocked => 2,
                ThreadState::Exited => 3,
                ThreadState::WaitingIpc => 4,
            },
            name,
        }
    }
}

/// Get list of all processes/threads for userspace
/// Returns the number of processes written to the buffer
pub fn list_processes(buffer: &mut [ProcessInfo]) -> usize {
    let threads = THREAD_LIST.threads.lock();
    let count = threads.len().min(buffer.len());

    for (i, thread) in threads.iter().take(count).enumerate() {
        buffer[i] = ProcessInfo::from_thread(thread);
    }

    count
}

/// Get total number of processes/threads
pub fn process_count() -> usize {
    THREAD_LIST.count()
}

pub fn snapshot_context(thread_id: ThreadId) -> Option<CpuContext> {
    let threads = THREAD_LIST.threads.lock();
    threads
        .iter()
        .find(|t| t.id == thread_id)
        .map(|t| t.context)
}

/// Returns true if the thread executes in Ring 3 (userspace).
/// This is the authoritative check - it uses the immutable flag set at creation,
/// not fragile heuristics based on address_space or saved context.cs.
pub fn is_userspace_thread(thread_id: ThreadId) -> bool {
    let threads = THREAD_LIST.threads.lock();
    threads
        .iter()
        .find(|t| t.id == thread_id)
        .map(|t| t.is_userspace)
        .unwrap_or(false)
}

/// Force update a thread's userspace context (RIP, RSP, CS, SS, segments)
/// This is critical before switching to a userspace thread because switch_context
/// saves the KERNEL RSP into CpuContext.rsp, which would then be used as the
/// userspace RSP during iret - causing page faults.
pub fn force_set_userspace_ctx(thread_id: ThreadId, user_rip: u64, user_rsp: u64) {
    let mut threads = THREAD_LIST.threads.lock();
    if let Some(t) = threads.iter_mut().find(|t| t.id == thread_id) {
        t.context.rip = user_rip;
        t.context.rsp = user_rsp;
        t.context.cs = gdt::USER_CODE_SELECTOR;
        t.context.ss = gdt::USER_DATA_SELECTOR;
        t.context.ds = gdt::USER_DATA_SELECTOR;
        t.context.es = gdt::USER_DATA_SELECTOR;
        t.context.fs = gdt::USER_DATA_SELECTOR;
        t.context.gs = gdt::USER_DATA_SELECTOR;
    }
}

pub fn with_thread_contexts<F, R>(from_id: ThreadId, to_id: ThreadId, f: F) -> Option<R>
where
    F: FnOnce(&mut CpuContext, &CpuContext) -> R,
{
    THREAD_LIST.with_contexts(from_id, to_id, f)
}

/// Update the userspace portion of a thread's saved context
/// This is called at syscall entry to ensure the saved context has the correct
/// userspace RIP (return address) and RSP, not the kernel's RIP/RSP.
/// This is critical for syscalls that may cause context switches (e.g., yield).
pub fn update_thread_userspace_context(thread_id: ThreadId, user_rip: u64, user_rsp: u64) {
    let mut threads = THREAD_LIST.threads.lock();

    if let Some(thread) = threads.iter_mut().find(|t| t.id == thread_id) {
        // Only update if this is a userspace thread (CPL=3)
        if (thread.context.cs & 0x3) == 0x3 {
            thread.context.rip = user_rip;
            thread.context.rsp = user_rsp;
        }
    }
}

pub fn capture_current_context() -> CpuContext {
    let mut ctx = CpuContext::zero();

    unsafe {
        core::arch::asm!(
        "mov [{ctx} + 0], rax",
        "mov [{ctx} + 8], rbx",
        "mov [{ctx} + 16], rcx",
        "mov [{ctx} + 24], rdx",
        "mov [{ctx} + 32], rsi",
        "mov [{ctx} + 40], rdi",
        "mov [{ctx} + 48], rbp",
        "mov [{ctx} + 56], rsp",
        "mov [{ctx} + 64], r8",
        "mov [{ctx} + 72], r9",
        "mov [{ctx} + 80], r10",
        "mov [{ctx} + 88], r11",
        "mov [{ctx} + 96], r12",
        "mov [{ctx} + 104], r13",
        "mov [{ctx} + 112], r14",
        "mov [{ctx} + 120], r15",
        "lea rax, [rip]",
        "mov [{ctx} + 128], rax",
        "pushfq",
        "pop rax",
        "mov [{ctx} + 136], rax",
        "mov ax, cs",
        "mov [{ctx} + 144], ax",
        "mov ax, ss",
        "mov [{ctx} + 146], ax",
        "mov ax, ds",
        "mov [{ctx} + 148], ax",
        "mov ax, es",
        "mov [{ctx} + 150], ax",
        "mov ax, fs",
        "mov [{ctx} + 152], ax",
        "mov ax, gs",
        "mov [{ctx} + 154], ax",
        "mov rax, cr3",
        "mov [{ctx} + 160], rax",

        ctx = in(reg) &mut ctx as *mut CpuContext,
        out("rax") _,
        out("rcx") _,
        );
    }

    ctx
}

fn is_canonical(addr: u64) -> bool {
    let sign_extension = addr >> 48;
    sign_extension == 0 || sign_extension == 0xFFFF
}

pub fn perform_context_switch(from_id: ThreadId, to_id: ThreadId) {
    // Disable interrupts during the critical section
    crate::interrupts::disable();

    // Get userspace return address BEFORE acquiring thread list lock
    let userspace_return = crate::syscall::get_userspace_return_addr(from_id);

    // Acquire lock, validate, update contexts, get pointers, then RELEASE lock before switch
    let switch_info = {
        let mut threads = THREAD_LIST.threads.lock();

        let from_idx = threads.iter().position(|t| t.id == from_id)
            .expect("from thread not found in context switch");
        let to_idx = threads.iter().position(|t| t.id == to_id)
            .expect("to thread not found in context switch");

        // Validate stack canary for outgoing thread
        let from_thread = &threads[from_idx];
        let bottom = from_thread.kernel_stack.wrapping_sub(from_thread.kernel_stack_size as u64);
        unsafe {
            let canary_addr = bottom as *const u64;
            let actual = core::ptr::read_volatile(canary_addr);
            if actual != STACK_CANARY {
                log_error!(
                    LOG_ORIGIN,
                    "[CANARY_CORRUPT] tid={} name={} canary_addr={:#X} expected={:#X} actual={:#X}",
                    from_id, from_thread.name, canary_addr as u64, STACK_CANARY, actual
                );
            }
        };

        // Update FROM thread's context with userspace return address if it's a userspace thread
        if let Some((user_rip, user_rsp)) = userspace_return {
            if threads[from_idx].is_userspace {
                threads[from_idx].context.rip = user_rip;
                threads[from_idx].context.rsp = user_rsp;
            }
        }

        // Force CR3 to match address_space
        let from_addr_space = threads[from_idx].address_space;
        let to_addr_space = threads[to_idx].address_space;
        let current_cr3 = unsafe {
            let cr3: u64;
            core::arch::asm!("mov {}, cr3", out(reg) cr3);
            cr3
        };
        let from_cr3 = if from_addr_space == 0 { current_cr3 } else { from_addr_space };
        let to_cr3 = if to_addr_space == 0 { current_cr3 } else { to_addr_space };
        threads[from_idx].context.cr3 = from_cr3;
        threads[to_idx].context.cr3 = to_cr3;

        // Use the authoritative is_userspace flag - NOT address space heuristics
        let to_is_userspace = threads[to_idx].is_userspace;
        let has_userspace_return = crate::syscall::get_userspace_return_addr(to_id).is_some();

        let from_ptr = &mut threads[from_idx].context as *mut CpuContext;
        let to_ptr = &threads[to_idx].context as *const CpuContext;
        let kstack = threads[to_idx].kernel_stack;
        let to_entry_rip = threads[to_idx].context.rip;
        let to_entry_rsp = threads[to_idx].context.rsp;

        if to_is_userspace {
            log_user_entry_once(to_id, &threads[to_idx].context);
        }

        (from_ptr, to_ptr, kstack, to_is_userspace, has_userspace_return, to_entry_rip, to_entry_rsp, to_cr3)
    }; // Lock released

    let (from_ctx_ptr, to_ctx_ptr, to_kernel_stack, to_is_userspace, has_userspace_return, to_entry_rip, to_entry_rsp, to_cr3) = switch_info;

    // Update TSS.RSP0 for the new thread BEFORE entering userspace
    // This ensures interrupt/syscall frames from usermode go to the correct kernel stack
    gdt::set_rsp0(to_kernel_stack);

    // CRITICAL PATH DECISION:
    // - Userspace threads (is_userspace=true): Always use enter_user* functions
    //   which build a proper 5-qword IRET frame for Ring 3 transition.
    //   * First-time entry: enter_user_first_time (zeros all GPRs)
    //   * Syscall resume: enter_user_resume (restores callee-saved GPRs) or enter_user
    // - Kernel threads (is_userspace=false): Use switch_context for
    //   kernel-to-kernel context switch (saves/restores via assembly).
    //
    // The is_userspace flag is set at thread creation and never changes.
    // This avoids the previous fragile heuristic of comparing address_space to
    // kernel CR3, which failed for the init process (shares kernel CR3 but runs
    // in Ring 3), causing IRET with CS=0x0 and SS=0x0.

    if to_is_userspace {
        // Determine target RIP/RSP: prefer saved userspace return address (from syscall entry),
        // fall back to initial context values (first-time entry)
        let (target_rip, target_rsp) = if has_userspace_return {
            crate::syscall::get_userspace_return_addr(to_id)
                .unwrap_or((to_entry_rip, to_entry_rsp))
        } else {
            (to_entry_rip, to_entry_rsp)
        };

        log_debug!(
            LOG_ORIGIN,
            "Enter userspace: TID={} RIP={:#016X} RSP={:#016X} CR3={:#016X} resume={}",
            to_id, target_rip, target_rsp, to_cr3, has_userspace_return
        );

        // Re-enable interrupts before entering userspace
        crate::interrupts::enable();

        if has_userspace_return {
            // Syscall resume - restore callee-saved registers if available
            let gprs_map = crate::syscall::USERSPACE_GPRS.lock();
            if let Some(gprs_ref) = gprs_map.get(&to_id) {
                let gprs_ptr = gprs_ref as *const crate::syscall::UserspaceGprs;
                drop(gprs_map);
                unsafe {
                    enter_user_resume(target_rip, target_rsp, to_cr3, gprs_ptr);
                }
            } else {
                drop(gprs_map);
                unsafe {
                    enter_user(target_rip, target_rsp, to_cr3);
                }
            }
        } else {
            // First-time entry - zero all registers for clean initial state
            unsafe {
                enter_user_first_time(target_rip, target_rsp, to_cr3);
            }
        }
        // enter_user* functions are divergent (-> !) so we never reach here
    }

    // Kernel-to-kernel context switch path
    unsafe {
        guard_context_or_halt(&*to_ctx_ptr, "scheduled");
    }

    // Perform the actual context switch with no locks held.
    // switch_context saves callee-saved registers to FROM, restores from TO,
    // then enters switch_to_context_internal which builds a proper 5-qword
    // IRET frame (including SS and RSP) and does iretq.
    // When THIS thread is scheduled again, it will "return" from switch_context.
    unsafe {
        switch_context(from_ctx_ptr, to_ctx_ptr);
    }

    // Re-enable interrupts when we resume
    crate::interrupts::enable();
}

/// Walk through page tables and free all mapped physical frames
/// This is used during termination to ensure complete cleanup of user-space memory
///
/// Returns the number of physical pages freed
fn free_user_space_pages(pml4_phys: usize) -> usize {
    let mut pages_freed = 0;

    // Walk PML4 entries (only user-space half: entries 0-255)
    for pml4_idx in 0..256 {
        if let Ok(pdpt_entry) = get_pml4_entry(pml4_phys, pml4_idx) {
            if pdpt_entry == 0 || (pdpt_entry & 0x1) == 0 {
                continue; // Entry not present
            }

            let pdpt_phys = pdpt_entry & 0x000F_FFFF_FFFF_F000;

            // Walk PDPT entries
            for pdpt_idx in 0..512 {
                if let Ok(pd_entry) = get_pdpt_entry(pdpt_phys as usize, pdpt_idx) {
                    if pd_entry == 0 || (pd_entry & 0x1) == 0 {
                        continue;
                    }

                    let pd_phys = pd_entry & 0x000F_FFFF_FFFF_F000;

                    // Walk PD entries
                    for pd_idx in 0..512 {
                        if let Ok(pt_entry) = get_pd_entry(pd_phys as usize, pd_idx) {
                            if pt_entry == 0 || (pt_entry & 0x1) == 0 {
                                continue;
                            }

                            let pt_phys = pt_entry & 0x000F_FFFF_FFFF_F000;

                            // Walk PT entries and free physical pages
                            for pt_idx in 0..512 {
                                if let Ok(page_entry) = get_pt_entry(pt_phys as usize, pt_idx) {
                                    if page_entry != 0 && (page_entry & 0x1) != 0 {
                                        let phys_frame = (page_entry & 0x000F_FFFF_FFFF_F000) as usize;
                                        crate::mm::pmm::free_page(phys_frame);
                                        pages_freed += 1;
                                    }
                                }
                            }

                            // Free the PT itself
                            crate::mm::pmm::free_page(pt_phys as usize);
                        }
                    }

                    // Free the PD
                    crate::mm::pmm::free_page(pd_phys as usize);
                }
            }

            // Free the PDPT
            crate::mm::pmm::free_page(pdpt_phys as usize);
        }
    }

    pages_freed
}

/// Helper function to convert physical address to virtual (higher half mapping)
#[inline]
fn phys_to_virt(phys: usize) -> usize {
    crate::mm::vm::HIGHER_HALF_BASE + phys
}

/// Helper function to read a PML4 entry
fn get_pml4_entry(pml4_phys: usize, index: usize) -> Result<u64, ()> {
    if index >= 512 {
        return Err(());
    }

    let pml4_virt = phys_to_virt(pml4_phys);
    let entry_ptr = (pml4_virt + index * 8) as *const u64;

    unsafe { Ok(*entry_ptr) }
}

/// Helper function to read a PDPT entry
fn get_pdpt_entry(pdpt_phys: usize, index: usize) -> Result<u64, ()> {
    if index >= 512 {
        return Err(());
    }

    let pdpt_virt = phys_to_virt(pdpt_phys);
    let entry_ptr = (pdpt_virt + index * 8) as *const u64;

    unsafe { Ok(*entry_ptr) }
}

/// Helper function to read a PD entry
fn get_pd_entry(pd_phys: usize, index: usize) -> Result<u64, ()> {
    if index >= 512 {
        return Err(());
    }

    let pd_virt = phys_to_virt(pd_phys);
    let entry_ptr = (pd_virt + index * 8) as *const u64;

    unsafe { Ok(*entry_ptr) }
}

/// Helper function to read a PT entry
fn get_pt_entry(pt_phys: usize, index: usize) -> Result<u64, ()> {
    if index >= 512 {
        return Err(());
    }

    let pt_virt = phys_to_virt(pt_phys);
    let entry_ptr = (pt_virt + index * 8) as *const u64;

    unsafe { Ok(*entry_ptr) }
}

/// Terminate an entity (thread/process) and clean up ALL its resources
///
/// This is the UNIFIED termination function that handles all cleanup for ANY
/// termination scenario: normal exit, faults, exceptions, kernel kill, watchdog.
///
/// Cleanup operations performed (deterministic and exhaustive):
/// 1. Mark thread as Exited to prevent scheduler from selecting it
/// 2. Close all IPC ports and wake waiting threads with errors
/// 3. Clean up shared memory regions (unmap + destroy owned regions)
/// 4. Revoke all capabilities (recursive parent→children if needed)
/// 5. Walk page tables and free ALL user-space physical frames
/// 6. Free page table structures (PT/PD/PDPT)
/// 7. Destroy address space (free PML4)
/// 8. Free kernel stack pages
/// 9. Remove from scheduler queues
/// 10. Remove from global thread list
/// 11. Update resource counters
/// 12. Log comprehensive termination report
///
/// After this function completes, the entity is completely gone from the system.
/// All memory is reclaimed, all handles are invalid, and all waiters are unblocked.
pub fn terminate_entity(thread_id: ThreadId, reason: TerminationReason) {
    log_info!(
        LOG_ORIGIN,
        "=== TERMINATING ENTITY {} - Reason: {} (exit_code=0x{:X}) ===",
        thread_id,
        reason.description(),
        reason.exit_code()
    );

    // Get thread info before we start cleanup
    let thread_info = THREAD_LIST.threads.lock()
        .iter()
        .find(|t| t.id == thread_id)
        .map(|t| (t.kernel_stack, t.kernel_stack_size, t.address_space, t.name));

    let (kernel_stack, kernel_stack_size, address_space_cr3, thread_name) = match thread_info {
        Some((ks, kss, as_cr3, name)) => (ks, kss, as_cr3, name),
        None => {
            log_warn!(LOG_ORIGIN, "Thread {} not found in thread list during termination", thread_id);
            RESOURCE_COUNTERS.threads_terminated.fetch_add(1, Ordering::Relaxed);
            return;
        }
    };

    log_info!(LOG_ORIGIN, "Terminating '{}' (TID={}, CR3=0x{:X})", thread_name, thread_id, address_space_cr3);

    // Step 1: Mark as Exited to prevent scheduling
    set_thread_state(thread_id, ThreadState::Exited);
    log_debug!(LOG_ORIGIN, "Step 1/10: Thread marked as Exited");

    // Step 2: Close all IPC ports and wake waiters
    log_debug!(LOG_ORIGIN, "Step 2/10: Closing IPC ports");
    crate::ipc::close_all_thread_ports(thread_id);

    // Step 3: Clean up shared memory regions
    log_debug!(LOG_ORIGIN, "Step 3/10: Cleaning up shared memory");
    crate::shared_mem::cleanup_thread_shared_memory(thread_id);

    // Step 4: Revoke all capabilities
    log_debug!(LOG_ORIGIN, "Step 4/10: Revoking capabilities");
    crate::cap::revoke_all_thread_capabilities(thread_id);

    // Step 5-7: Complete page table walk and physical memory cleanup
    log_debug!(LOG_ORIGIN, "Step 5/10: Walking page tables and freeing physical frames");
    let pages_freed = if address_space_cr3 != crate::arch::read_cr3() {
        // Thread has its own PML4 (userspace thread)
        let user_pages = free_user_space_pages(address_space_cr3 as usize);
        log_info!(LOG_ORIGIN, "Freed {} user-space physical pages from PML4 0x{:X}", user_pages, address_space_cr3);

        // Free the PML4 itself
        crate::mm::pmm::free_page(address_space_cr3 as usize);
        log_debug!(LOG_ORIGIN, "Freed PML4 page at 0x{:X}", address_space_cr3);

        user_pages
    } else {
        log_debug!(LOG_ORIGIN, "Thread using kernel PML4, skipping user page table cleanup");
        0
    };

    RESOURCE_COUNTERS.physical_pages_freed_on_termination.fetch_add(pages_freed, Ordering::Relaxed);

    // Step 6: Clean up address spaces from manager
    log_debug!(LOG_ORIGIN, "Step 6/10: Removing address spaces from manager");
    crate::mm::addrspace::cleanup_thread_address_spaces(thread_id);

    // Step 7: Free kernel stack
    log_debug!(LOG_ORIGIN, "Step 7/10: Freeing kernel stack");
    let kernel_stack_bottom = kernel_stack.wrapping_sub(kernel_stack_size as u64);
    let num_stack_pages = kernel_stack_size / crate::mm::pmm::PAGE_SIZE;

    log_debug!(
        LOG_ORIGIN,
        "Freeing {} kernel stack pages starting at 0x{:X}",
        num_stack_pages,
        kernel_stack_bottom
    );

    let current_pml4 = crate::arch::read_cr3() as usize;
    let mut stack_pages_freed = 0;

    for i in 0..num_stack_pages {
        let page_addr = kernel_stack_bottom + (i * crate::mm::pmm::PAGE_SIZE) as u64;

        if let Ok((phys_addr, _)) = crate::mm::vm::query_mapping_in_pml4(current_pml4, page_addr as usize) {
            crate::mm::pmm::free_page(phys_addr);
            stack_pages_freed += 1;
        }
    }

    log_debug!(LOG_ORIGIN, "Freed {} kernel stack pages", stack_pages_freed);
    RESOURCE_COUNTERS.kernel_stacks_freed.fetch_add(1, Ordering::Relaxed);

    // Step 8: Remove from scheduler queues, priority maps, and current-thread slot
    log_debug!(LOG_ORIGIN, "Step 8/10: Removing from scheduler");
    crate::sched::remove_thread(thread_id);

    // Step 9: Remove from global thread list
    log_debug!(LOG_ORIGIN, "Step 9/10: Removing from thread list");
    if let Some(_removed) = THREAD_LIST.remove(thread_id) {
        log_debug!(LOG_ORIGIN, "Thread removed from global list");
    }

    // Step 10: Update counters and log final report
    log_debug!(LOG_ORIGIN, "Step 10/10: Updating resource counters");
    RESOURCE_COUNTERS.threads_terminated.fetch_add(1, Ordering::Relaxed);

    log_info!(
        LOG_ORIGIN,
        "=== ENTITY {} ('{}') TERMINATED SUCCESSFULLY ===",
        thread_id,
        thread_name
    );
    log_info!(
        LOG_ORIGIN,
        "Resource cleanup: {} user pages + {} stack pages freed",
        pages_freed,
        stack_pages_freed
    );

    RESOURCE_COUNTERS.log_stats();
}

/// Legacy function - redirects to terminate_entity
#[allow(dead_code)]
pub fn terminate_thread(thread_id: ThreadId) {
    terminate_entity(thread_id, TerminationReason::NormalExit { exit_code: 0 });
}
