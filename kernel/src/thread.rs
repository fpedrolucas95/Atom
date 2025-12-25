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
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use crate::arch::gdt;
use crate::{log_info, log_panic};

use crate::cap::CapabilityTable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Running,
    Ready,
    Blocked,
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
            rbp: 0,
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
        
        unsafe {
            let bottom = kernel_stack
                .wrapping_sub(kernel_stack_size as u64);
            let canary_addr = bottom as *mut u64;
            core::ptr::write_volatile(canary_addr, STACK_CANARY);
        }

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

    pub fn get_stats(&self) -> ThreadStats {
        let threads = self.threads.lock();
        let mut stats = ThreadStats::default();

        stats.total = threads.len();
        for thread in threads.iter() {
            match thread.state {
                ThreadState::Running => stats.running += 1,
                ThreadState::Ready => stats.ready += 1,
                ThreadState::Blocked => stats.blocked += 1,
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
        log_info!(
            LOG_ORIGIN,
            "Thread {} entering user mode: RIP={:#016X} RSP={:#016X} CS={:#04X} SS={:#04X}",
            thread_id,
            ctx.rip,
            ctx.rsp,
            ctx.cs,
            ctx.ss
        );
    }
}

pub fn get_runnable_threads() -> Vec<ThreadId> {
    THREAD_LIST.get_runnable()
}

pub fn set_thread_state(id: ThreadId, state: ThreadState) -> bool {
    THREAD_LIST.set_state(id, state)
}

pub fn get_thread_stats() -> ThreadStats {
    THREAD_LIST.get_stats()
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
    switch_to_context(context as *const CpuContext)
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

pub fn snapshot_context(thread_id: ThreadId) -> Option<CpuContext> {
    let threads = THREAD_LIST.threads.lock();
    threads
        .iter()
        .find(|t| t.id == thread_id)
        .map(|t| t.context)
}

pub fn with_thread_contexts<F, R>(from_id: ThreadId, to_id: ThreadId, f: F) -> Option<R>
where
    F: FnOnce(&mut CpuContext, &CpuContext) -> R,
{
    THREAD_LIST.with_contexts(from_id, to_id, f)
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
    let from_thread = {
        let threads = THREAD_LIST.threads.lock();
        threads.iter().find(|t| t.id == from_id).map(|t| {
            (t.kernel_stack, t.kernel_stack_size)
        })
    };

    if let Some((kernel_stack, kernel_stack_size)) = from_thread {
        let ok = unsafe {
            let bottom = kernel_stack.wrapping_sub(kernel_stack_size as u64);
            let canary_addr = bottom as *const u64;
            core::ptr::read_volatile(canary_addr) == STACK_CANARY
        };

        debug_assert!(
            ok,
            "Stack overflow detected on thread {}",
            from_id
        );

        if !ok {
            log_panic!(
                LOG_ORIGIN,
                "Kernel stack canary corrupted on thread {} (stack overflow / corruption)",
                from_id
            );
        }
    }

    let _ = with_thread_contexts(from_id, to_id, |from_ctx, to_ctx| unsafe {
        switch_thread_context(from_ctx, to_ctx);
    });
}
