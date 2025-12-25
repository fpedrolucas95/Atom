// Kernel Scheduler
//
// Implements a fixed-priority, round-robin scheduler with timer-based
// preemption and an explicit idle thread fallback. This scheduler is
// designed to be simple, predictable, and sufficient for early
// microkernel-style execution.
//
// Key responsibilities:
// - Maintain per-priority ready queues
// - Select the next runnable thread on demand or timer tick
// - Enforce fixed priority ordering with round-robin fairness
// - Manage thread state transitions (Running ↔ Ready)
// - Provide an idle thread when no runnable work exists
//
// Scheduling model:
// - Threads are assigned one of a small, fixed set of priorities
// - Higher-priority threads always run before lower-priority ones
// - Threads at the same priority level are scheduled round-robin
// - Timer interrupts drive preemptive scheduling
//
// Priority management:
// - Each thread has a base priority and an effective priority
// - Effective priority may be temporarily boosted (e.g. IPC inheritance)
// - Original priority is restored explicitly after the critical section
//
// Implementation details:
// - Ready queues are stored as `VecDeque`s indexed by priority
// - Scheduler state is protected by spinlocks for simplicity
// - Global singleton (`SCHEDULER`) centralizes all scheduling decisions
// - Thread metadata and context are managed by the `thread` subsystem
//
// Correctness and safety notes:
// - Scheduling is disabled until `init()` installs an idle thread
// - Idle thread ensures the CPU always has something safe to run
// - Preemption occurs only on timer ticks, keeping behavior predictable
// - No dynamic priority recalculation or load balancing is performed
//
// Design trade-offs and future work:
// - No support for SMP or per-CPU run queues
// - No time-slice accounting beyond timer ticks
// - No real-time guarantees or deadline scheduling
// - Intended to evolve alongside user-space services and IPC policies
//
// This scheduler prioritizes clarity and correctness over sophistication,
// making it well-suited for an early-stage microkernel architecture.

#![allow(dead_code)]

use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use crate::arch::gdt;
use crate::thread::{self, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::util::without_interrupts;
use crate::{log_debug, log_info};

const PRIORITY_LEVELS: usize = 4;

struct ReadyQueues {
    queues: [VecDeque<ThreadId>; PRIORITY_LEVELS],
}

impl ReadyQueues {
    fn new() -> Self {
        Self {
            queues: [(); PRIORITY_LEVELS].map(|_| VecDeque::new()),
        }
    }

    fn push(&mut self, id: ThreadId, priority: ThreadPriority) {
        let idx = priority as usize;
        if idx < PRIORITY_LEVELS {
            self.queues[idx].push_back(id);
        }
    }

    fn pop_next(&mut self) -> Option<ThreadId> {
        for idx in (0..PRIORITY_LEVELS).rev() {
            if let Some(id) = self.queues[idx].pop_front() {
                self.queues[idx].push_back(id);
                return Some(id);
            }
        }
        None
    }

    fn is_empty(&self) -> bool {
        self.queues.iter().all(|q| q.is_empty())
    }
}

struct Scheduler {
    ready: Mutex<ReadyQueues>,
    base_priorities: Mutex<BTreeMap<ThreadId, ThreadPriority>>,
    effective_priorities: Mutex<BTreeMap<ThreadId, ThreadPriority>>,
    current: Mutex<Option<ThreadId>>,
    idle: Mutex<Option<ThreadId>>,
    initialized: AtomicBool,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            ready: Mutex::new(ReadyQueues {
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new(), VecDeque::new()],
            }),
            base_priorities: Mutex::new(BTreeMap::new()),
            effective_priorities: Mutex::new(BTreeMap::new()),
            current: Mutex::new(None),
            idle: Mutex::new(None),
            initialized: AtomicBool::new(false),
        }
    }

    fn init(&self, idle_thread: Thread) -> ThreadId {
        let idle_id = idle_thread.id();
        thread::add_thread(idle_thread);
        *self.idle.lock() = Some(idle_id);
        *self.current.lock() = Some(idle_id);
        self.base_priorities
            .lock()
            .insert(idle_id, ThreadPriority::Idle);
        self.effective_priorities
            .lock()
            .insert(idle_id, ThreadPriority::Idle);
        thread::set_thread_state(idle_id, ThreadState::Running);
        self.initialized.store(true, Ordering::SeqCst);
        idle_id
    }

    fn add_thread(&self, thread: Thread) -> ThreadId {
        let id = thread.id();
        let priority = thread.priority;
        let state = thread.state;

        thread::add_thread(thread);
        self.base_priorities.lock().insert(id, priority);
        self.effective_priorities.lock().insert(id, priority);

        if matches!(state, ThreadState::Ready) {
            self.ready.lock().push(id, priority);
        }

        id
    }

    fn schedule(&self) -> Option<ThreadId> {
        if !self.initialized.load(Ordering::SeqCst) {
            return None;
        }

        let next = {
            let mut ready = self.ready.lock();
            ready.pop_next()
        };

        self.apply_switch(next)
    }

    fn on_timer_tick(&self) -> (Option<ThreadId>, Option<ThreadId>) {
        if !self.initialized.load(Ordering::SeqCst) {
            return (None, None);
        }

        let mut next: Option<ThreadId> = None;
        let mut previous: Option<ThreadId> = None;

        {
            let mut ready = self.ready.lock();
            let mut current = self.current.lock();

            if let Some(cur) = *current {
                previous = Some(cur);

                if !ready.is_empty() {
                    let priority = self.get_priority(cur);
                    ready.push(cur, priority);

                    log_debug!("sched", "Thread {} requeued (priority={:?})", cur, priority);

                    *current = None;
                }
            }

            if current.is_none() {
                next = ready.pop_next();

                if let Some(n) = next {
                    log_debug!("sched", "Next thread selected: {}", n);
                }
            }
        }

        let chosen = self.apply_switch_with_previous(previous, next);
        (previous, chosen)
    }
    
    fn apply_switch(&self, next: Option<ThreadId>) -> Option<ThreadId> {
        let previous = self.current_thread();
        self.apply_switch_with_previous(previous, next)
    }

    fn apply_switch_with_previous(
        &self,
        previous: Option<ThreadId>,
        next: Option<ThreadId>,
    ) -> Option<ThreadId> {
        let chosen = next.or(previous).or_else(|| self.idle_id());

        if let Some(prev) = previous {
            if Some(prev) != chosen {
                thread::set_thread_state(prev, ThreadState::Ready);
            }
        }

        if let Some(id) = chosen {
            thread::set_thread_state(id, ThreadState::Running);
            *self.current.lock() = Some(id);
            return Some(id);
        }

        None
    }

    fn idle_id(&self) -> Option<ThreadId> {
        *self.idle.lock()
    }

    fn get_priority(&self, id: ThreadId) -> ThreadPriority {
        self.effective_priorities
            .lock()
            .get(&id)
            .copied()
            .unwrap_or(ThreadPriority::Normal)
    }

    fn get_base_priority(&self, id: ThreadId) -> ThreadPriority {
        self.base_priorities
            .lock()
            .get(&id)
            .copied()
            .unwrap_or(ThreadPriority::Normal)
    }
    
    fn boost_priority(&self, id: ThreadId, new_priority: ThreadPriority) -> bool {
        let mut effective = self.effective_priorities.lock();
        let current = effective.get(&id).copied().unwrap_or(ThreadPriority::Normal);

        if new_priority > current {
            effective.insert(id, new_priority);
            true
        } else {
            false
        }
    }

    fn restore_original_priority(&self, id: ThreadId) {
        let base = self.get_base_priority(id);
        self.effective_priorities.lock().insert(id, base);
    }

    fn mark_ready(&self, id: ThreadId) {
        let priority = self.get_priority(id);
        thread::set_thread_state(id, ThreadState::Ready);
        self.ready.lock().push(id, priority);
    }

    fn current_thread(&self) -> Option<ThreadId> {
        *self.current.lock()
    }
}

static SCHEDULER: Scheduler = Scheduler::new();

pub fn init(idle_thread: Thread) -> ThreadId {
    SCHEDULER.init(idle_thread)
}

pub fn add_thread(thread: Thread) -> ThreadId {
    SCHEDULER.add_thread(thread)
}

pub fn schedule() -> Option<ThreadId> {
    SCHEDULER.schedule()
}

pub fn on_timer_tick() -> (Option<ThreadId>, Option<ThreadId>) {
    SCHEDULER.on_timer_tick()
}

pub fn drive_cooperative_tick() {
    let (prev, next) = on_timer_tick();

    if let (Some(prev_id), Some(next_id)) = (prev, next) {
        if prev_id != next_id {
            perform_context_switch(prev_id, next_id);
        }
    }
}

pub fn mark_thread_ready(id: ThreadId) {
    SCHEDULER.mark_ready(id);
}

pub fn current_thread() -> Option<ThreadId> {
    SCHEDULER.current_thread()
}

pub fn boost_thread_priority(id: ThreadId, new_priority: ThreadPriority) -> bool {
    SCHEDULER.boost_priority(id, new_priority)
}

pub fn restore_original_priority(id: ThreadId) {
    SCHEDULER.restore_original_priority(id)
}

pub fn get_thread_priority(id: ThreadId) -> ThreadPriority {
    SCHEDULER.get_priority(id)
}

pub fn get_base_priority(id: ThreadId) -> ThreadPriority {
    SCHEDULER.get_base_priority(id)
}

/// Yield the current thread, allowing other threads to run
pub fn yield_current() {
    // Get current thread
    let current = match current_thread() {
        Some(id) => id,
        None => return, // No current thread, nothing to yield
    };
    
    // Schedule next thread
    if let Some(next) = schedule() {
        if next != current {
            perform_context_switch(current, next);
        }
    }
}

pub fn perform_context_switch(from_id: ThreadId, to_id: ThreadId) {
    without_interrupts(|| {
        let target_kernel_stack = thread::kernel_stack_top(to_id);

        thread::with_thread_contexts(from_id, to_id, |from_ctx, to_ctx| unsafe {
            if let Some(stack) = target_kernel_stack {
                gdt::set_rsp0(stack);
            }

            let target_cpl = (to_ctx.cs & 0x3) as u8;
            if target_cpl == 3 {
                thread::log_user_entry_once(to_id, to_ctx);
                log_info!(
                    "sched",
                    "Switching to user context: RIP={:#016X} CS={:#04X} SS={:#04X} CPL={} CR3={:#016X}",
                    to_ctx.rip,
                    to_ctx.cs,
                    to_ctx.ss,
                    target_cpl,
                    to_ctx.cr3
                );
            }

            thread::switch_thread_context(from_ctx, to_ctx);
        });
    });
}
