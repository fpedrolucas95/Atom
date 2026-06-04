#![allow(dead_code)]

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::{Mutex, Once};

use crate::interrupts::apic;
use crate::thread::{self, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::util::without_interrupts;
use crate::{log_debug, log_error, log_info};

const LOG_ORIGIN: &str = "sched";
const PRIORITY_LEVELS: usize = 4;
const NO_CPU_OWNER: usize = usize::MAX;

static LAST_LOGGED_MOUSE_IRQ: AtomicU64 = AtomicU64::new(0);
static LAST_LOGGED_RESCHEDULE_IRQ: AtomicU64 = AtomicU64::new(0);
static SCHED_SWITCH_LOG_COUNT: AtomicU64 = AtomicU64::new(0);
static THREAD_STATE_LOG_COUNT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn should_log_count(count: u64) -> bool {
    count <= 64 || count.is_power_of_two()
}

#[inline]
fn priority_index(priority: ThreadPriority) -> usize {
    (priority as usize).min(PRIORITY_LEVELS - 1)
}

fn thread_state_name(state: Option<ThreadState>) -> &'static str {
    match state {
        Some(ThreadState::Ready) => "Ready",
        Some(ThreadState::Running) => "Running",
        Some(ThreadState::Blocked) => "Blocked",
        Some(ThreadState::WaitingIpc) => "WaitingIpc",
        Some(ThreadState::Exited) => "Exited",
        None => "none",
    }
}

#[derive(Debug)]
struct ReadyQueues {
    queues: [VecDeque<ThreadId>; PRIORITY_LEVELS],
}

impl ReadyQueues {
    fn new() -> Self {
        Self {
            queues: [(); PRIORITY_LEVELS].map(|_| VecDeque::new()),
        }
    }

    fn push_back(&mut self, id: ThreadId, priority: ThreadPriority) {
        let idx = priority_index(priority);
        if !self.queues[idx].iter().any(|existing| *existing == id) {
            self.queues[idx].push_back(id);
        }
    }

    fn remove(&mut self, id: ThreadId) {
        for queue in self.queues.iter_mut() {
            queue.retain(|existing| *existing != id);
        }
    }

    fn contains(&self, id: ThreadId) -> bool {
        self.queues
            .iter()
            .any(|queue| queue.iter().any(|existing| *existing == id))
    }

    fn len(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }

    fn pop_front_raw(&mut self) -> Option<ThreadId> {
        for idx in (0..PRIORITY_LEVELS).rev() {
            if let Some(id) = self.queues[idx].pop_front() {
                return Some(id);
            }
        }
        None
    }

    fn pop_back_raw(&mut self) -> Option<ThreadId> {
        for idx in (0..PRIORITY_LEVELS).rev() {
            if let Some(id) = self.queues[idx].pop_back() {
                return Some(id);
            }
        }
        None
    }
}

#[derive(Debug)]
struct CpuSchedulerState {
    ready: ReadyQueues,
    current: Option<ThreadId>,
    idle: Option<ThreadId>,
    local_ticks: u64,
    resched_pending: bool,
    context_switches: u64,
    steals: u64,
    /// Thread whose saved context is currently being overwritten by the low-level
    /// switch routine on this CPU.  The marker is set before publishing the
    /// outgoing thread as runnable and is cleared by the newly-landed context via
    /// `clear_pending_switch_from()`.
    pending_switch_from: Option<ThreadId>,
}

impl CpuSchedulerState {
    fn new() -> Self {
        Self {
            ready: ReadyQueues::new(),
            current: None,
            idle: None,
            local_ticks: 0,
            resched_pending: false,
            context_switches: 0,
            steals: 0,
            pending_switch_from: None,
        }
    }
}

#[derive(Debug)]
struct SchedulerInner {
    cpus: Vec<CpuSchedulerState>,
    base_priorities: BTreeMap<ThreadId, ThreadPriority>,
    effective_priorities: BTreeMap<ThreadId, ThreadPriority>,
    affinity_masks: BTreeMap<ThreadId, u64>,
    ownership: BTreeMap<ThreadId, usize>,
    sleep_queue: Vec<(ThreadId, u64)>,
}

impl SchedulerInner {
    fn new(cpu_count: usize) -> Self {
        let cpu_count = cpu_count.max(1);
        let mut cpus = Vec::with_capacity(cpu_count);
        for _ in 0..cpu_count {
            cpus.push(CpuSchedulerState::new());
        }

        Self {
            cpus,
            base_priorities: BTreeMap::new(),
            effective_priorities: BTreeMap::new(),
            affinity_masks: BTreeMap::new(),
            ownership: BTreeMap::new(),
            sleep_queue: Vec::new(),
        }
    }

    fn cpu_count(&self) -> usize {
        self.cpus.len()
    }

    fn all_cpu_mask(&self) -> u64 {
        let n = self.cpu_count().min(64);
        if n == 64 {
            u64::MAX
        } else {
            (1u64 << n) - 1
        }
    }

    fn affinity_of(&self, id: ThreadId) -> u64 {
        self.affinity_masks
            .get(&id)
            .copied()
            .unwrap_or_else(|| self.all_cpu_mask())
            & self.all_cpu_mask()
    }

    fn affinity_allows_cpu(&self, id: ThreadId, cpu_id: usize) -> bool {
        cpu_id < 64 && ((self.affinity_of(id) >> cpu_id) & 1) != 0
    }

    fn effective_priority(&self, id: ThreadId) -> ThreadPriority {
        self.effective_priorities
            .get(&id)
            .copied()
            .unwrap_or(ThreadPriority::Normal)
    }

    fn base_priority(&self, id: ThreadId) -> ThreadPriority {
        self.base_priorities
            .get(&id)
            .copied()
            .unwrap_or(ThreadPriority::Normal)
    }

    fn thread_is_current(&self, id: ThreadId) -> bool {
        self.cpus.iter().any(|cpu| cpu.current == Some(id))
    }

    fn thread_is_pending_switch_from(&self, id: ThreadId) -> bool {
        self.cpus
            .iter()
            .any(|cpu| cpu.pending_switch_from == Some(id))
    }

    fn current_cpu_for(&self, id: ThreadId) -> Option<usize> {
        self.cpus
            .iter()
            .enumerate()
            .find_map(|(cpu_id, cpu)| (cpu.current == Some(id)).then_some(cpu_id))
    }

    fn remove_from_all_ready_queues(&mut self, id: ThreadId) {
        for cpu in self.cpus.iter_mut() {
            cpu.ready.remove(id);
        }
    }

    #[cfg(debug_assertions)]
    fn is_in_any_ready_queue(&self, id: ThreadId) -> bool {
        self.cpus.iter().any(|cpu| cpu.ready.contains(id))
    }

    fn is_valid_ready_candidate(&self, id: ThreadId, target_cpu: usize) -> bool {
        if !self.affinity_allows_cpu(id, target_cpu) {
            return false;
        }

        if !matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
            return false;
        }

        if self.thread_is_current(id) || self.thread_is_pending_switch_from(id) {
            return false;
        }

        match self.ownership.get(&id).copied() {
            Some(owner) if owner != NO_CPU_OWNER => false,
            _ => true,
        }
    }

    fn pop_local_candidate(&mut self, cpu_id: usize) -> Option<ThreadId> {
        loop {
            let id = self.cpus[cpu_id].ready.pop_front_raw()?;
            if self.is_valid_ready_candidate(id, cpu_id) {
                return Some(id);
            }
            self.ownership.insert(id, NO_CPU_OWNER);
        }
    }

    fn steal_candidate(&mut self, thief_cpu: usize) -> Option<ThreadId> {
        for victim_cpu in 0..self.cpu_count() {
            if victim_cpu == thief_cpu || !crate::smp::is_cpu_online(victim_cpu) {
                continue;
            }

            loop {
                let Some(id) = self.cpus[victim_cpu].ready.pop_back_raw() else {
                    break;
                };

                if self.is_valid_ready_candidate(id, thief_cpu) {
                    self.cpus[thief_cpu].steals = self.cpus[thief_cpu].steals.saturating_add(1);
                    log_debug!(
                        LOG_ORIGIN,
                        "work steal: tid={} from_cpu={} to_cpu={}",
                        id,
                        victim_cpu,
                        thief_cpu
                    );
                    return Some(id);
                }

                // Stale or ineligible entry.  Do not push it back blindly: if it
                // is Running/in-flight, requeueing can resurrect a duplicate CPU
                // owner.  A future mark_ready/enqueue will publish it again if
                // appropriate.
                self.ownership.insert(id, NO_CPU_OWNER);
            }
        }

        None
    }

    fn select_target_cpu(&self, id: ThreadId, preferred_cpu: Option<usize>) -> usize {
        let affinity = self.affinity_of(id);

        if let Some(cpu_id) = preferred_cpu {
            if cpu_id < self.cpu_count()
                && ((affinity >> cpu_id) & 1) != 0
                && crate::smp::is_cpu_online(cpu_id)
            {
                return cpu_id;
            }
        }

        let mut best_cpu = 0usize;
        let mut best_len = usize::MAX;

        for cpu_id in 0..self.cpu_count() {
            if ((affinity >> cpu_id) & 1) == 0 || !crate::smp::is_cpu_online(cpu_id) {
                continue;
            }

            let len = self.cpus[cpu_id].ready.len();
            if len < best_len {
                best_cpu = cpu_id;
                best_len = len;
            }
        }

        best_cpu
    }

    fn enqueue_ready_locked(&mut self, id: ThreadId, preferred_cpu: Option<usize>) -> usize {
        self.remove_from_all_ready_queues(id);

        if matches!(thread::get_thread_state(id), Some(ThreadState::Exited) | None) {
            self.ownership.remove(&id);
            return self.select_target_cpu(id, preferred_cpu);
        }

        let target_cpu = self.select_target_cpu(id, preferred_cpu);
        let priority = self.effective_priority(id);
        self.ownership.insert(id, NO_CPU_OWNER);
        thread::set_thread_state(id, ThreadState::Ready);
        self.cpus[target_cpu].ready.push_back(id, priority);
        target_cpu
    }

    fn unregister_thread_locked(&mut self, id: ThreadId) {
        self.remove_from_all_ready_queues(id);
        self.sleep_queue.retain(|(tid, _)| *tid != id);
        self.base_priorities.remove(&id);
        self.effective_priorities.remove(&id);
        self.affinity_masks.remove(&id);
        self.ownership.remove(&id);

        for cpu in self.cpus.iter_mut() {
            if cpu.current == Some(id) {
                cpu.current = cpu.idle;
            }
            if cpu.pending_switch_from == Some(id) {
                cpu.pending_switch_from = None;
            }
        }
    }
}

struct Scheduler {
    inner: Mutex<SchedulerInner>,
    initialized: AtomicBool,
}

impl Scheduler {
    fn new(cpu_count: usize) -> Self {
        Self {
            inner: Mutex::new(SchedulerInner::new(cpu_count)),
            initialized: AtomicBool::new(false),
        }
    }

    fn cpu_count(&self) -> usize {
        self.inner.lock().cpu_count()
    }

    fn local_cpu_id(&self, inner: &SchedulerInner) -> usize {
        let cpu_id = crate::smp::current_cpu_id();
        if cpu_id < inner.cpu_count() {
            cpu_id
        } else {
            0
        }
    }

    fn thread_name(&self, id: ThreadId) -> &'static str {
        thread::get_thread_name(id).unwrap_or("?")
    }

    fn log_thread_state(
        &self,
        id: ThreadId,
        old_state: Option<ThreadState>,
        new_state: ThreadState,
        owner_cpu: Option<usize>,
    ) {
        let count = THREAD_STATE_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if new_state != ThreadState::Running && !should_log_count(count) {
            return;
        }

        if let Some(owner_cpu) = owner_cpu {
            log_info!(
                "thread",
                "[thread] tid={} proc={} old_state={} new_state={:?} owner_cpu={}",
                id.raw(),
                self.thread_name(id),
                thread_state_name(old_state),
                new_state,
                owner_cpu
            );
        } else {
            log_info!(
                "thread",
                "[thread] tid={} proc={} old_state={} new_state={:?} owner_cpu=none",
                id.raw(),
                self.thread_name(id),
                thread_state_name(old_state),
                new_state
            );
        }
    }

    fn maybe_log_irq_snapshot(&self, cpu_id: usize) {
        let snap = crate::interrupts::handlers::irq_snapshot();

        let reschedule_count = snap.reschedule;
        let last_reschedule = LAST_LOGGED_RESCHEDULE_IRQ.load(Ordering::Relaxed);
        if reschedule_count != last_reschedule
            && snap.reschedule_eoi >= reschedule_count
            && should_log_count(reschedule_count)
        {
            LAST_LOGGED_RESCHEDULE_IRQ.store(reschedule_count, Ordering::Relaxed);
            log_info!(
                "irq",
                "[irq] cpu={} vector=0x2d irq=ipi handler=reschedule eoi=yes count={}",
                cpu_id,
                reschedule_count
            );
        }

        let mouse_count = snap.mouse;
        let last_mouse = LAST_LOGGED_MOUSE_IRQ.load(Ordering::Relaxed);
        if mouse_count != last_mouse
            && snap.mouse_eoi >= mouse_count
            && should_log_count(mouse_count)
        {
            LAST_LOGGED_MOUSE_IRQ.store(mouse_count, Ordering::Relaxed);
            log_info!(
                "irq",
                "[irq] cpu={} vector=0x2c irq=12 handler=mouse eoi=yes count={}",
                cpu_id,
                mouse_count
            );
        }
    }

    fn maybe_send_reschedule_ipi_locked(
        &self,
        inner: &mut SchedulerInner,
        target_cpu: usize,
        incoming_priority: ThreadPriority,
    ) {
        let local_cpu = self.local_cpu_id(inner);
        if target_cpu == local_cpu || target_cpu >= inner.cpu_count() {
            return;
        }

        let should_ipi = match inner.cpus[target_cpu].current {
            Some(current) => incoming_priority > inner.effective_priority(current),
            None => true,
        };

        if !should_ipi || inner.cpus[target_cpu].resched_pending {
            return;
        }

        inner.cpus[target_cpu].resched_pending = true;
        if let Some(apic_id) = crate::smp::cpu_apic_id(target_cpu) {
            log_debug!(
                LOG_ORIGIN,
                "send resched IPI: target_cpu={} apic_id={} incoming_prio={:?}",
                target_cpu,
                apic_id,
                incoming_priority
            );
            apic::send_reschedule_ipi(apic_id);
        }
    }

    fn init_cpu(&self, cpu_id: usize, idle_thread: Thread) -> ThreadId {
        let idle_id = idle_thread.id();
        thread::add_thread(idle_thread);

        let mut inner = self.inner.lock();
        let cpu_id = cpu_id.min(inner.cpu_count().saturating_sub(1));

        inner.base_priorities.insert(idle_id, ThreadPriority::Idle);
        inner.effective_priorities.insert(idle_id, ThreadPriority::Idle);
        inner.affinity_masks.insert(idle_id, 1u64 << cpu_id.min(63));
        inner.ownership.insert(idle_id, cpu_id);
        inner.remove_from_all_ready_queues(idle_id);

        inner.cpus[cpu_id].idle = Some(idle_id);
        inner.cpus[cpu_id].current = Some(idle_id);
        inner.cpus[cpu_id].pending_switch_from = None;
        thread::set_thread_state(idle_id, ThreadState::Running);

        self.initialized.store(true, Ordering::SeqCst);
        idle_id
    }

    fn add_thread(&self, thread: Thread) -> ThreadId {
        let id = thread.id();
        let priority = thread.priority;
        let initial_state = thread.state;

        thread::add_thread(thread);

        let mut inner = self.inner.lock();
        inner.base_priorities.insert(id, priority);
        inner.effective_priorities.insert(id, priority);
        let all_cpu_mask = inner.all_cpu_mask();
        inner.affinity_masks.insert(id, all_cpu_mask);
        inner.ownership.insert(id, NO_CPU_OWNER);

        if matches!(initial_state, ThreadState::Ready) {
            let target_cpu = inner.enqueue_ready_locked(id, None);
            self.maybe_send_reschedule_ipi_locked(&mut inner, target_cpu, priority);
        }

        id
    }

    fn schedule_local(&self, requeue_current: bool) -> (Option<ThreadId>, Option<ThreadId>) {
        if !self.initialized.load(Ordering::SeqCst) {
            return (None, None);
        }

        let mut inner = self.inner.lock();
        let cpu_id = self.local_cpu_id(&inner);
        let previous = inner.cpus[cpu_id].current;
        let had_resched_pending = inner.cpus[cpu_id].resched_pending;

        inner.cpus[cpu_id].local_ticks = inner.cpus[cpu_id].local_ticks.saturating_add(1);
        inner.cpus[cpu_id].resched_pending = false;

        let mut chosen = inner.pop_local_candidate(cpu_id);
        let mut stole = false;

        if chosen.is_none() {
            chosen = inner.steal_candidate(cpu_id);
            stole = chosen.is_some();
        }

        if chosen.is_none() {
            if let Some(prev) = previous {
                if matches!(thread::get_thread_state(prev), Some(ThreadState::Running) | Some(ThreadState::Ready))
                    && inner.affinity_allows_cpu(prev, cpu_id)
                {
                    chosen = Some(prev);
                }
            }
        }

        if chosen.is_none() {
            chosen = inner.cpus[cpu_id].idle;
        }

        let switching = previous != chosen;

        if switching {
            inner.cpus[cpu_id].context_switches = inner.cpus[cpu_id].context_switches.saturating_add(1);
            inner.cpus[cpu_id].pending_switch_from = previous;
            if stole {
                inner.cpus[cpu_id].steals = inner.cpus[cpu_id].steals.saturating_add(1);
            }
        } else {
            inner.cpus[cpu_id].pending_switch_from = None;
        }

        if switching && requeue_current {
            if let Some(prev) = previous {
                let prev_state = thread::get_thread_state(prev);
                let prev_is_idle = inner.cpus[cpu_id].idle == Some(prev);
                if !prev_is_idle
                    && matches!(prev_state, Some(ThreadState::Running) | Some(ThreadState::Ready))
                    && inner.affinity_allows_cpu(prev, cpu_id)
                {
                    inner.remove_from_all_ready_queues(prev);
                    thread::set_thread_state(prev, ThreadState::Ready);
                    inner.ownership.insert(prev, NO_CPU_OWNER);
                    let prio = inner.effective_priority(prev);
                    inner.cpus[cpu_id].ready.push_back(prev, prio);
                }
            }
        }

        inner.cpus[cpu_id].current = chosen;

        if let Some(next) = chosen {
            inner.remove_from_all_ready_queues(next);
            thread::set_thread_state(next, ThreadState::Running);
            let previous_owner = inner.ownership.insert(next, cpu_id);

            if let Some(owner) = previous_owner {
                if owner != NO_CPU_OWNER && owner != cpu_id {
                    log_error!(
                        LOG_ORIGIN,
                        "[sched] duplicate-running-thread tid={}/{} prev_owner={} new_owner={}",
                        next,
                        self.thread_name(next),
                        owner,
                        cpu_id
                    );
                }
            }

            #[cfg(debug_assertions)]
            if inner.is_in_any_ready_queue(next) {
                panic!(
                    "SMP INVARIANT VIOLATION: thread {:?} is Running but still in a ready queue",
                    next
                );
            }
        }

        let log_count = SCHED_SWITCH_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(next) = chosen {
            if (switching || had_resched_pending) && should_log_count(log_count) {
                log_info!(
                    LOG_ORIGIN,
                    "[sched] cpu={} prev={}/{} next={}/{} state={:?} resched_pending={}",
                    cpu_id,
                    previous.map(|id| id.raw()).unwrap_or(0),
                    previous.map(|id| self.thread_name(id)).unwrap_or("none"),
                    next.raw(),
                    self.thread_name(next),
                    thread::get_thread_state(next).unwrap_or(ThreadState::Exited),
                    if had_resched_pending { "yes" } else { "no" }
                );
            }
        }

        drop(inner);
        self.maybe_log_irq_snapshot(cpu_id);

        (previous, chosen)
    }

    fn mark_ready(&self, id: ThreadId) {
        let mut inner = self.inner.lock();

        if let Some(owner_cpu) = inner.current_cpu_for(id) {
            // The thread is still executing.  Never enqueue a Running thread;
            // just request that its CPU re-enters the scheduler soon.
            if owner_cpu < inner.cpu_count() {
                inner.cpus[owner_cpu].resched_pending = true;
                if owner_cpu != self.local_cpu_id(&inner) {
                    if let Some(apic_id) = crate::smp::cpu_apic_id(owner_cpu) {
                        apic::send_reschedule_ipi(apic_id);
                    }
                }
            }
            return;
        }

        if inner.thread_is_pending_switch_from(id) {
            // The low-level switch is still saving this context.  Do not publish
            // it twice.  The existing ready queue entry, if any, will become
            // valid after `clear_pending_switch_from`.
            return;
        }

        match thread::get_thread_state(id) {
            Some(ThreadState::Ready) | Some(ThreadState::Blocked) | Some(ThreadState::WaitingIpc) => {
                let old_state = thread::get_thread_state(id);
                let target = inner.enqueue_ready_locked(id, None);
                let priority = inner.effective_priority(id);
                self.log_thread_state(id, old_state, ThreadState::Ready, None);
                self.maybe_send_reschedule_ipi_locked(&mut inner, target, priority);
            }
            Some(ThreadState::Running) => {
                if let Some(owner_cpu) = inner.ownership.get(&id).copied() {
                    if owner_cpu != NO_CPU_OWNER && owner_cpu < inner.cpu_count() {
                        inner.cpus[owner_cpu].resched_pending = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn sleep_thread(&self, id: ThreadId, wake_tick: u64) {
        let mut inner = self.inner.lock();
        inner.remove_from_all_ready_queues(id);
        inner.ownership.insert(id, NO_CPU_OWNER);
        inner.sleep_queue.retain(|(tid, _)| *tid != id);
        inner.sleep_queue.push((id, wake_tick));
        let old_state = thread::get_thread_state(id);
        thread::set_thread_state(id, ThreadState::Blocked);
        self.log_thread_state(id, old_state, ThreadState::Blocked, None);
    }

    fn cancel_sleep(&self, id: ThreadId) {
        self.inner.lock().sleep_queue.retain(|(tid, _)| *tid != id);
    }

    fn wake_sleeping_threads(&self) {
        let current_tick = crate::interrupts::get_ticks();
        let mut to_wake = Vec::new();

        {
            let mut inner = self.inner.lock();
            let mut index = 0;
            while index < inner.sleep_queue.len() {
                if current_tick >= inner.sleep_queue[index].1 {
                    to_wake.push(inner.sleep_queue.swap_remove(index).0);
                } else {
                    index += 1;
                }
            }

            for id in to_wake.iter().copied() {
                let target = inner.enqueue_ready_locked(id, None);
                let priority = inner.effective_priority(id);
                self.maybe_send_reschedule_ipi_locked(&mut inner, target, priority);
            }
        }
    }

    fn restore_current_after_block(&self, id: ThreadId) {
        let mut inner = self.inner.lock();
        let cpu_id = self.local_cpu_id(&inner);
        inner.remove_from_all_ready_queues(id);
        inner.sleep_queue.retain(|(tid, _)| *tid != id);
        inner.cpus[cpu_id].current = Some(id);
        inner.ownership.insert(id, cpu_id);
        let old_state = thread::get_thread_state(id);
        thread::set_thread_state(id, ThreadState::Running);
        self.log_thread_state(id, old_state, ThreadState::Running, Some(cpu_id));
    }

    fn deschedule_thread(&self, id: ThreadId) {
        let mut inner = self.inner.lock();
        inner.unregister_thread_locked(id);
    }

    fn current_thread(&self) -> Option<ThreadId> {
        let inner = self.inner.lock();
        let cpu_id = self.local_cpu_id(&inner);
        inner.cpus[cpu_id].current
    }

    fn clear_pending_switch_from(&self) {
        let mut inner = self.inner.lock();
        let cpu_id = self.local_cpu_id(&inner);
        inner.cpus[cpu_id].pending_switch_from = None;
    }

    fn is_pending_switch_from(&self, id: ThreadId) -> bool {
        self.inner.lock().thread_is_pending_switch_from(id)
    }

    fn boost_priority(&self, id: ThreadId, new_priority: ThreadPriority) -> bool {
        let mut inner = self.inner.lock();
        let current = inner.effective_priority(id);
        if new_priority <= current {
            return false;
        }
        inner.effective_priorities.insert(id, new_priority);
        if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
            let target = inner.enqueue_ready_locked(id, None);
            self.maybe_send_reschedule_ipi_locked(&mut inner, target, new_priority);
        }
        true
    }

    fn restore_original_priority(&self, id: ThreadId) {
        let mut inner = self.inner.lock();
        let base = inner.base_priority(id);
        inner.effective_priorities.insert(id, base);
        if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
            let target = inner.enqueue_ready_locked(id, None);
            self.maybe_send_reschedule_ipi_locked(&mut inner, target, base);
        }
    }

    fn get_priority(&self, id: ThreadId) -> ThreadPriority {
        self.inner.lock().effective_priority(id)
    }

    fn get_base_priority(&self, id: ThreadId) -> ThreadPriority {
        self.inner.lock().base_priority(id)
    }

    fn set_affinity(&self, id: ThreadId, mask: u64) -> bool {
        let mut inner = self.inner.lock();
        let valid = mask & inner.all_cpu_mask();
        if valid == 0 {
            return false;
        }

        inner.affinity_masks.insert(id, valid);

        if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
            let target = inner.enqueue_ready_locked(id, None);
            let priority = inner.effective_priority(id);
            self.maybe_send_reschedule_ipi_locked(&mut inner, target, priority);
        }

        if let Some(owner_cpu) = inner.ownership.get(&id).copied() {
            if owner_cpu != NO_CPU_OWNER && owner_cpu < inner.cpu_count() && !inner.affinity_allows_cpu(id, owner_cpu) {
                inner.cpus[owner_cpu].resched_pending = true;
                if owner_cpu != self.local_cpu_id(&inner) {
                    if let Some(apic_id) = crate::smp::cpu_apic_id(owner_cpu) {
                        apic::send_reschedule_ipi(apic_id);
                    }
                }
            }
        }

        true
    }

    fn get_affinity(&self, id: ThreadId) -> u64 {
        self.inner.lock().affinity_of(id)
    }

    fn flag_reschedule_local(&self) {
        let mut inner = self.inner.lock();
        let cpu_id = self.local_cpu_id(&inner);
        inner.cpus[cpu_id].resched_pending = true;
    }

    fn owner_cpu_for_logging(&self, id: ThreadId) -> Option<usize> {
        let inner = self.inner.lock();
        match inner.ownership.get(&id).copied() {
            Some(owner) if owner != NO_CPU_OWNER => Some(owner),
            _ => None,
        }
    }
}

static SCHEDULER: Once<Scheduler> = Once::new();

fn scheduler_opt() -> Option<&'static Scheduler> {
    SCHEDULER.get()
}

fn scheduler() -> &'static Scheduler {
    SCHEDULER.get().expect("scheduler not initialized")
}

pub fn init(idle_thread: Thread) -> ThreadId {
    let scheduler = SCHEDULER.call_once(|| Scheduler::new(crate::smp::cpu_count().max(1)));
    let idle = scheduler.init_cpu(crate::smp::current_cpu_id(), idle_thread);
    log_info!(LOG_ORIGIN, "scheduler initialized for {} CPUs", scheduler.cpu_count());
    idle
}

pub fn init_secondary_cpu(cpu_id: usize, idle_thread: Thread) -> ThreadId {
    scheduler().init_cpu(cpu_id, idle_thread)
}

pub fn add_thread(thread: Thread) -> ThreadId {
    without_interrupts(|| scheduler().add_thread(thread))
}

pub fn schedule() -> Option<ThreadId> {
    without_interrupts(|| scheduler_opt().and_then(|sched| sched.schedule_local(false).1))
}

pub fn on_timer_tick() -> (Option<ThreadId>, Option<ThreadId>) {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.schedule_local(true))
            .unwrap_or((None, None))
    })
}

pub fn drive_cooperative_tick() {
    let (previous, next) = on_timer_tick();
    if let (Some(previous), Some(next)) = (previous, next) {
        if previous != next {
            perform_context_switch(previous, next);
        }
    }
}

pub fn mark_thread_ready(id: ThreadId) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.mark_ready(id);
        }
    })
}

pub fn deschedule_thread(id: ThreadId) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.deschedule_thread(id);
        }
    })
}

pub fn sleep_thread(id: ThreadId, wake_tick: u64) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.sleep_thread(id, wake_tick);
        }
    })
}

pub fn cancel_sleep(id: ThreadId) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.cancel_sleep(id);
        }
    })
}

pub fn wake_sleeping_threads() {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.wake_sleeping_threads();
        }
    })
}

pub fn current_thread() -> Option<ThreadId> {
    without_interrupts(|| scheduler_opt().and_then(|sched| sched.current_thread()))
}

pub fn clear_pending_switch_from() {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.clear_pending_switch_from();
        }
    })
}

pub fn is_pending_switch_from(id: ThreadId) -> bool {
    scheduler_opt()
        .map(|sched| sched.is_pending_switch_from(id))
        .unwrap_or(false)
}

pub fn restore_current_after_block(id: ThreadId) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.restore_current_after_block(id);
        }
    })
}

pub fn owner_cpu_for_logging(id: ThreadId) -> Option<usize> {
    scheduler_opt().and_then(|sched| sched.owner_cpu_for_logging(id))
}

pub fn boost_thread_priority(id: ThreadId, new_priority: ThreadPriority) -> bool {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.boost_priority(id, new_priority))
            .unwrap_or(false)
    })
}

pub fn restore_original_priority(id: ThreadId) {
    without_interrupts(|| {
        if let Some(sched) = scheduler_opt() {
            sched.restore_original_priority(id);
        }
    })
}

pub fn get_thread_priority(id: ThreadId) -> ThreadPriority {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.get_priority(id))
            .unwrap_or(ThreadPriority::Normal)
    })
}

pub fn get_base_priority(id: ThreadId) -> ThreadPriority {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.get_base_priority(id))
            .unwrap_or(ThreadPriority::Normal)
    })
}

pub fn yield_current() {
    let Some(current) = current_thread() else {
        return;
    };

    let (_, next) = on_timer_tick();
    if let Some(next) = next {
        if next != current {
            perform_context_switch(current, next);
        }
    }
}

pub fn set_thread_affinity(id: ThreadId, mask: u64) -> bool {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.set_affinity(id, mask))
            .unwrap_or(false)
    })
}

pub fn get_thread_affinity(id: ThreadId) -> u64 {
    without_interrupts(|| {
        scheduler_opt()
            .map(|sched| sched.get_affinity(id))
            .unwrap_or(1)
    })
}

pub fn perform_context_switch(from_id: ThreadId, to_id: ThreadId) {
    thread::perform_context_switch(from_id, to_id);
}

pub fn on_reschedule_interrupt() {
    if let Some(sched) = scheduler_opt() {
        sched.flag_reschedule_local();
    }
}
