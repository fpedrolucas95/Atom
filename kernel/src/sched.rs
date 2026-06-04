#![allow(dead_code)]

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::{Mutex, Once};

use crate::interrupts::apic;
use crate::thread::{self, Thread, ThreadId, ThreadPriority, ThreadState};
use crate::{log_debug, log_error, log_info};

const PRIORITY_LEVELS: usize = 4;
const LOG_ORIGIN: &str = "sched";
const NO_CPU_OWNER: usize = usize::MAX;
static LAST_LOGGED_MOUSE_IRQ: AtomicU64 = AtomicU64::new(0);
static LAST_LOGGED_RESCHEDULE_IRQ: AtomicU64 = AtomicU64::new(0);
static SCHED_SWITCH_LOG_COUNT: AtomicU64 = AtomicU64::new(0);
static THREAD_STATE_LOG_COUNT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn should_log_irq_count(count: u64) -> bool {
    count <= 64 || count.is_power_of_two()
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

struct ReadyQueues {
    queues: [VecDeque<ThreadId>; PRIORITY_LEVELS],
}

impl ReadyQueues {
    fn new() -> Self {
        Self { queues: [(); PRIORITY_LEVELS].map(|_| VecDeque::new()) }
    }

    fn push(&mut self, id: ThreadId, priority: ThreadPriority) {
        let idx = priority as usize;
        if idx < PRIORITY_LEVELS && !self.queues[idx].iter().any(|it| *it == id) {
            self.queues[idx].push_back(id);
        }
    }

    fn remove(&mut self, id: ThreadId) {
        for q in self.queues.iter_mut() {
            q.retain(|it| *it != id);
        }
    }

    fn pop_next(&mut self) -> Option<ThreadId> {
        for idx in (0..PRIORITY_LEVELS).rev() {
            while let Some(id) = self.queues[idx].pop_front() {
                if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
                    return Some(id);
                }
            }
        }
        None
    }

    fn steal_next(&mut self) -> Option<ThreadId> {
        for idx in (0..PRIORITY_LEVELS).rev() {
            while let Some(id) = self.queues[idx].pop_back() {
                if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
                    return Some(id);
                }
            }
        }
        None
    }

    fn len(&self) -> usize {
        self.queues.iter().map(VecDeque::len).sum()
    }
}

struct CpuSchedulerState {
    ready: ReadyQueues,
    current: Option<ThreadId>,
    idle: Option<ThreadId>,
    local_ticks: u64,
    resched_pending: bool,
    context_switches: u64,
    steals: u64,
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
        }
    }
}

struct Scheduler {
    cpus: Vec<Mutex<CpuSchedulerState>>,
    base_priorities: Mutex<BTreeMap<ThreadId, ThreadPriority>>,
    effective_priorities: Mutex<BTreeMap<ThreadId, ThreadPriority>>,
    affinity_masks: Mutex<BTreeMap<ThreadId, u64>>,
    ownership: Mutex<BTreeMap<ThreadId, usize>>,
    sleep_queue: Mutex<Vec<(ThreadId, u64)>>,
    initialized: AtomicBool,
}

impl Scheduler {
    fn new(cpu_count: usize) -> Self {
        let mut cpus = Vec::with_capacity(cpu_count.max(1));
        for _ in 0..cpu_count.max(1) {
            cpus.push(Mutex::new(CpuSchedulerState::new()));
        }

        Self {
            cpus,
            base_priorities: Mutex::new(BTreeMap::new()),
            effective_priorities: Mutex::new(BTreeMap::new()),
            affinity_masks: Mutex::new(BTreeMap::new()),
            ownership: Mutex::new(BTreeMap::new()),
            sleep_queue: Mutex::new(Vec::new()),
            initialized: AtomicBool::new(false),
        }
    }

    fn cpu_count(&self) -> usize { self.cpus.len() }

    fn local_cpu_id(&self) -> usize {
        let cpu = crate::smp::current_cpu_id();
        if cpu < self.cpus.len() { cpu } else { 0 }
    }

    fn all_cpu_mask(&self) -> u64 {
        let n = self.cpu_count().min(64);
        if n == 64 { u64::MAX } else { (1u64 << n) - 1 }
    }

    fn affinity_of(&self, id: ThreadId) -> u64 {
        let all = self.all_cpu_mask();
        self.affinity_masks.lock().get(&id).copied().unwrap_or(all) & all
    }

    fn get_priority(&self, id: ThreadId) -> ThreadPriority {
        self.effective_priorities.lock().get(&id).copied().unwrap_or(ThreadPriority::Normal)
    }

    fn get_base_priority(&self, id: ThreadId) -> ThreadPriority {
        self.base_priorities.lock().get(&id).copied().unwrap_or(ThreadPriority::Normal)
    }

    fn thread_name(&self, id: ThreadId) -> &'static str {
        thread::get_thread_name(id).unwrap_or("?")
    }

    fn is_runnable(&self, id: ThreadId) -> bool {
        thread::get_thread_state(id)
            .map(|s| matches!(s, ThreadState::Ready | ThreadState::Running))
            .unwrap_or(false)
    }

    fn log_thread_state(
        &self,
        id: ThreadId,
        old_state: Option<ThreadState>,
        new_state: ThreadState,
        owner_cpu: Option<usize>,
    ) {
        let count = THREAD_STATE_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if new_state != ThreadState::Running && !should_log_irq_count(count) {
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
            && should_log_irq_count(reschedule_count)
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
            && should_log_irq_count(mouse_count)
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

    fn remove_from_all_ready_queues(&self, id: ThreadId) {
        for cpu in self.cpus.iter() {
            cpu.lock().ready.remove(id);
        }
    }

    #[cfg(debug_assertions)]
    fn is_in_any_ready_queue(&self, id: ThreadId) -> bool {
        for cpu in self.cpus.iter() {
            let cpu = cpu.lock();
            for queue in cpu.ready.queues.iter() {
                if queue.iter().any(|it| *it == id) {
                    return true;
                }
            }
        }
        false
    }


    fn affinity_allows_cpu(&self, id: ThreadId, cpu_id: usize) -> bool {
        ((self.affinity_of(id) >> cpu_id) & 1) != 0
    }

    fn select_target_cpu(&self, id: ThreadId, preferred_cpu: Option<usize>) -> usize {
        let affinity = self.affinity_of(id);

        if let Some(cpu) = preferred_cpu {
            if cpu < self.cpu_count() && ((affinity >> cpu) & 1) != 0 && crate::smp::is_cpu_online(cpu) {
                return cpu;
            }
        }

        let mut best = 0usize;
        let mut best_len = usize::MAX;

        for cpu_id in 0..self.cpu_count() {
            if ((affinity >> cpu_id) & 1) == 0 || !crate::smp::is_cpu_online(cpu_id) {
                continue;
            }
            let len = self.cpus[cpu_id].lock().ready.len();
            if len < best_len {
                best = cpu_id;
                best_len = len;
            }
        }

        best
    }

    fn maybe_send_reschedule_ipi(&self, target_cpu: usize, incoming_priority: ThreadPriority) {
        let local_cpu = self.local_cpu_id();
        if target_cpu == local_cpu {
            return;
        }

        let target_current = self.cpus[target_cpu].lock().current;
        let should_ipi = match target_current {
            Some(cur) => incoming_priority > self.get_priority(cur),
            None => true,
        };

        if should_ipi {
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
    }

    fn enqueue_ready(&self, id: ThreadId, preferred_cpu: Option<usize>) {
        let target = self.select_target_cpu(id, preferred_cpu);
        let prio = self.get_priority(id);

        self.remove_from_all_ready_queues(id);
        let previous_owner = self.ownership.lock().insert(id, NO_CPU_OWNER);
        thread::set_thread_state(id, ThreadState::Ready);

        if let Some(owner) = previous_owner {
            if owner != NO_CPU_OWNER && owner != target {
                log_debug!(
                    LOG_ORIGIN,
                    "migration: tid={} from_cpu={} to_cpu={} prio={:?}",
                    id,
                    owner,
                    target,
                    prio
                );
            }
        }

        self.cpus[target].lock().ready.push(id, prio);
        if target != self.local_cpu_id() {
            log_debug!(
                LOG_ORIGIN,
                "remote enqueue: tid={} target_cpu={} prio={:?}",
                id,
                target,
                prio
            );
        }
        self.maybe_send_reschedule_ipi(target, prio);
    }

    fn init_cpu(&self, cpu_id: usize, idle_thread: Thread) -> ThreadId {
        let idle_id = idle_thread.id();
        thread::add_thread(idle_thread);

        self.base_priorities.lock().insert(idle_id, ThreadPriority::Idle);
        self.effective_priorities.lock().insert(idle_id, ThreadPriority::Idle);
        self.affinity_masks.lock().insert(idle_id, 1u64 << cpu_id.min(63));
        self.ownership.lock().insert(idle_id, cpu_id);

        let mut cpu = self.cpus[cpu_id].lock();
        cpu.idle = Some(idle_id);
        cpu.current = Some(idle_id);

        thread::set_thread_state(idle_id, ThreadState::Running);
        self.initialized.store(true, Ordering::SeqCst);
        idle_id
    }

    fn init_bsp(&self, idle_thread: Thread) -> ThreadId {
        self.init_cpu(self.local_cpu_id(), idle_thread)
    }

    fn add_thread(&self, thread: Thread) -> ThreadId {
        let id = thread.id();
        let prio = thread.priority;
        let state = thread.state;

        thread::add_thread(thread);
        self.base_priorities.lock().insert(id, prio);
        self.effective_priorities.lock().insert(id, prio);
        self.affinity_masks.lock().insert(id, self.all_cpu_mask());
        self.ownership.lock().insert(id, NO_CPU_OWNER);

        if matches!(state, ThreadState::Ready) {
            self.enqueue_ready(id, None);
        }

        id
    }

    fn try_steal(&self, thief_cpu: usize) -> Option<ThreadId> {
        for cpu_id in 0..self.cpu_count() {
            if cpu_id == thief_cpu || !crate::smp::is_cpu_online(cpu_id) {
                continue;
            }

            let candidate = { self.cpus[cpu_id].lock().ready.steal_next() };
            if let Some(id) = candidate {
                if self.affinity_allows_cpu(id, thief_cpu) {
                    log_debug!(
                        LOG_ORIGIN,
                        "work steal: tid={} from_cpu={} to_cpu={}",
                        id,
                        cpu_id,
                        thief_cpu
                    );
                    return Some(id);
                }
                self.enqueue_ready(id, Some(cpu_id));
            }
        }

        None
    }

    fn schedule_local(&self, requeue_current: bool) -> (Option<ThreadId>, Option<ThreadId>) {
        if !self.initialized.load(Ordering::SeqCst) {
            return (None, None);
        }

        let cpu_id = self.local_cpu_id();
        let previous;
        let mut chosen;
        let mut stole_thread = false;
        let had_resched_pending;

        {
            let mut cpu = self.cpus[cpu_id].lock();
            cpu.local_ticks = cpu.local_ticks.saturating_add(1);
            previous = cpu.current;
            had_resched_pending = cpu.resched_pending;

            if requeue_current {
                if let Some(cur) = previous {
                    if Some(cur) != cpu.idle && self.is_runnable(cur) && self.affinity_allows_cpu(cur, cpu_id) {
                        cpu.ready.push(cur, self.get_priority(cur));
                        thread::set_thread_state(cur, ThreadState::Ready);
                        self.ownership.lock().insert(cur, NO_CPU_OWNER);
                    }
                }
            }

            chosen = cpu.ready.pop_next();
            cpu.resched_pending = false;
        }

        // Do not attempt cross-CPU stealing while holding the local CPU lock:
        // simultaneous idle/timer paths can otherwise deadlock lock(A)->lock(B)
        // against lock(B)->lock(A) on 4+ CPU systems.
        if chosen.is_none() {
            chosen = self.try_steal(cpu_id);
            stole_thread = chosen.is_some();
        }

        {
            let mut cpu = self.cpus[cpu_id].lock();
            if stole_thread {
                cpu.steals = cpu.steals.saturating_add(1);
            }

            if chosen.is_none() {
                chosen = cpu.ready.pop_next();
            }
            if chosen.is_none() {
                chosen = previous.filter(|id| self.is_runnable(*id) && self.affinity_allows_cpu(*id, cpu_id));
            }
            if chosen.is_none() {
                chosen = cpu.idle;
            }

            cpu.current = chosen;
            if previous != chosen {
                cpu.context_switches = cpu.context_switches.saturating_add(1);
            }
        }

        if let Some(prev) = previous {
            if matches!(thread::get_thread_state(prev), Some(ThreadState::Running)) {
                thread::set_thread_state(prev, ThreadState::Ready);
                self.ownership.lock().insert(prev, NO_CPU_OWNER);
            }

            // INVARIANT: A thread transitioning to Running must not be in any run queue.
            #[cfg(debug_assertions)]
            if self.is_in_any_ready_queue(prev) {
                panic!("SMP INVARIANT VIOLATION: thread {:?} Running but still in ready queue", prev);
            }
        }

        if let Some(next) = chosen {
            thread::set_thread_state(next, ThreadState::Running);
            let previous_owner = self.ownership.lock().insert(next, cpu_id);

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

            let log_count = SCHED_SWITCH_LOG_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
            if (previous != Some(next) || had_resched_pending)
                && (had_resched_pending || should_log_irq_count(log_count))
            {
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

            self.maybe_log_irq_snapshot(cpu_id);

            // INVARIANT: A thread cannot be both Running and in a ready queue.
            #[cfg(debug_assertions)]
            if self.is_in_any_ready_queue(next) {
                panic!("SMP INVARIANT VIOLATION: thread {:?} Ready but already in queue", next);
            }
        }

        (previous, chosen)
    }

    fn schedule(&self) -> Option<ThreadId> { self.schedule_local(false).1 }
    fn on_timer_tick(&self) -> (Option<ThreadId>, Option<ThreadId>) { self.schedule_local(true) }

    fn sleep_thread(&self, id: ThreadId, wake_tick: u64) {
        self.remove_from_all_ready_queues(id);
        self.ownership.lock().insert(id, NO_CPU_OWNER);
        let old_state = thread::get_thread_state(id);
        thread::set_thread_state(id, ThreadState::Blocked);
        self.log_thread_state(id, old_state, ThreadState::Blocked, None);
        self.sleep_queue.lock().push((id, wake_tick));
    }

    fn restore_current_after_block(&self, id: ThreadId) {
        let cpu_id = self.local_cpu_id();
        self.remove_from_all_ready_queues(id);
        self.sleep_queue.lock().retain(|(thread_id, _)| *thread_id != id);
        {
            let mut cpu = self.cpus[cpu_id].lock();
            cpu.current = Some(id);
        }
        self.ownership.lock().insert(id, cpu_id);
        let old_state = thread::get_thread_state(id);
        thread::set_thread_state(id, ThreadState::Running);
        self.log_thread_state(id, old_state, ThreadState::Running, Some(cpu_id));
    }

    fn wake_sleeping_threads(&self) {
        let current_tick = crate::interrupts::get_ticks();

        loop {
            let due_thread = {
                let mut q = self.sleep_queue.lock();
                if let Some(pos) = q.iter().position(|&(_, wake_tick)| current_tick >= wake_tick) {
                    Some(q.swap_remove(pos).0)
                } else {
                    None
                }
            };

            let Some(id) = due_thread else {
                break;
            };
            self.enqueue_ready(id, None);
        }
    }

    fn mark_ready(&self, id: ThreadId) {
        match thread::get_thread_state(id) {
            Some(ThreadState::Blocked) | Some(ThreadState::Ready) => self.enqueue_ready(id, None),
            Some(ThreadState::Running) => {
                // Thread registered as a receiver (via block_recv) but has not yet
                // transitioned to Blocked.  We cannot enqueue a Running thread — that
                // would let two CPUs execute the same thread simultaneously.
                // Instead flag a reschedule on the owning CPU so the thread returns
                // promptly to try_receive_message and finds the waiting message.
                if let Some(owner_cpu) = self.ownership.lock().get(&id).copied() {
                    if owner_cpu != NO_CPU_OWNER && owner_cpu < self.cpus.len() {
                        self.cpus[owner_cpu].lock().resched_pending = true;
                        log_info!(
                            LOG_ORIGIN,
                            "mark_ready: tid={} Running on cpu={}, set resched_pending \
                             (block_recv TOCTOU — message in queue, TOCTOU guard will catch it)",
                            id,
                            owner_cpu
                        );
                        if owner_cpu != self.local_cpu_id() {
                            if let Some(apic_id) = crate::smp::cpu_apic_id(owner_cpu) {
                                apic::send_reschedule_ipi(apic_id);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn deschedule_thread(&self, id: ThreadId) {
        self.remove_from_all_ready_queues(id);
        self.sleep_queue.lock().retain(|(thread_id, _)| *thread_id != id);
        self.ownership.lock().remove(&id);

        for cpu in self.cpus.iter() {
            let mut cpu = cpu.lock();
            if cpu.current == Some(id) {
                cpu.current = cpu.idle;
            }
        }

        self.base_priorities.lock().remove(&id);
        self.effective_priorities.lock().remove(&id);
        self.affinity_masks.lock().remove(&id);
    }

    fn current_thread(&self) -> Option<ThreadId> {
        self.cpus[self.local_cpu_id()].lock().current
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

    fn set_affinity(&self, id: ThreadId, mask: u64) -> bool {
        let valid = mask & self.all_cpu_mask();
        if valid == 0 {
            return false;
        }
        self.affinity_masks.lock().insert(id, valid);

        if matches!(thread::get_thread_state(id), Some(ThreadState::Ready)) {
            self.enqueue_ready(id, None);
        }

        if let Some(owner_cpu) = self.ownership.lock().get(&id).copied() {
            if owner_cpu != NO_CPU_OWNER && !self.affinity_allows_cpu(id, owner_cpu) {
                if let Some(apic_id) = crate::smp::cpu_apic_id(owner_cpu) {
                    apic::send_reschedule_ipi(apic_id);
                }
            }
        }

        true
    }

    fn get_affinity(&self, id: ThreadId) -> u64 {
        self.affinity_of(id)
    }

    fn flag_reschedule_local(&self) {
        self.cpus[self.local_cpu_id()].lock().resched_pending = true;
    }

    fn owner_cpu_for_logging(&self, id: ThreadId) -> Option<usize> {
        match self.ownership.lock().get(&id).copied() {
            Some(owner) if owner != NO_CPU_OWNER => Some(owner),
            _ => None,
        }
    }
}

static SCHEDULER: Once<Scheduler> = Once::new();

fn scheduler_opt() -> Option<&'static Scheduler> { SCHEDULER.get() }
fn scheduler() -> &'static Scheduler { SCHEDULER.get().expect("scheduler not initialized") }

pub fn init(idle_thread: Thread) -> ThreadId {
    let sched = SCHEDULER.call_once(|| Scheduler::new(crate::smp::cpu_count().max(1)));
    let idle = sched.init_bsp(idle_thread);
    log_info!(LOG_ORIGIN, "scheduler initialized for {} CPUs", sched.cpu_count());
    idle
}

pub fn init_secondary_cpu(cpu_id: usize, idle_thread: Thread) -> ThreadId {
    scheduler().init_cpu(cpu_id, idle_thread)
}

pub fn add_thread(thread: Thread) -> ThreadId { scheduler().add_thread(thread) }
pub fn schedule() -> Option<ThreadId> { scheduler_opt().and_then(|s| s.schedule()) }
pub fn on_timer_tick() -> (Option<ThreadId>, Option<ThreadId>) { scheduler_opt().map(|s| s.on_timer_tick()).unwrap_or((None, None)) }

pub fn drive_cooperative_tick() {
    let (prev, next) = on_timer_tick();
    if let (Some(prev_id), Some(next_id)) = (prev, next) {
        if prev_id != next_id {
            perform_context_switch(prev_id, next_id);
        }
    }
}

pub fn mark_thread_ready(id: ThreadId) { if let Some(s) = scheduler_opt() { s.mark_ready(id); } }
pub fn deschedule_thread(id: ThreadId) { if let Some(s) = scheduler_opt() { s.deschedule_thread(id); } }
pub fn sleep_thread(id: ThreadId, wake_tick: u64) { if let Some(s) = scheduler_opt() { s.sleep_thread(id, wake_tick); } }
pub fn cancel_sleep(id: ThreadId) { if let Some(s) = scheduler_opt() { s.sleep_queue.lock().retain(|(tid, _)| *tid != id); } }
pub fn wake_sleeping_threads() { if let Some(s) = scheduler_opt() { s.wake_sleeping_threads(); } }
pub fn current_thread() -> Option<ThreadId> { scheduler_opt().and_then(|s| s.current_thread()) }

pub fn restore_current_after_block(id: ThreadId) {
    if let Some(s) = scheduler_opt() {
        s.restore_current_after_block(id);
    }
}

pub fn owner_cpu_for_logging(id: ThreadId) -> Option<usize> {
    scheduler_opt().and_then(|s| s.owner_cpu_for_logging(id))
}

pub fn boost_thread_priority(id: ThreadId, new_priority: ThreadPriority) -> bool {
    scheduler_opt().map(|s| s.boost_priority(id, new_priority)).unwrap_or(false)
}

pub fn restore_original_priority(id: ThreadId) { if let Some(s) = scheduler_opt() { s.restore_original_priority(id); } }
pub fn get_thread_priority(id: ThreadId) -> ThreadPriority { scheduler_opt().map(|s| s.get_priority(id)).unwrap_or(ThreadPriority::Normal) }
pub fn get_base_priority(id: ThreadId) -> ThreadPriority { scheduler_opt().map(|s| s.get_base_priority(id)).unwrap_or(ThreadPriority::Normal) }

pub fn yield_current() {
    let current = match current_thread() { Some(id) => id, None => return };
    let (_, next) = on_timer_tick();
    if let Some(next_id) = next {
        if next_id != current {
            perform_context_switch(current, next_id);
        }
    }
}

pub fn set_thread_affinity(id: ThreadId, mask: u64) -> bool {
    scheduler_opt().map(|s| s.set_affinity(id, mask)).unwrap_or(false)
}

pub fn get_thread_affinity(id: ThreadId) -> u64 {
    scheduler_opt().map(|s| s.get_affinity(id)).unwrap_or(1)
}

pub fn perform_context_switch(from_id: ThreadId, to_id: ThreadId) {
    thread::perform_context_switch(from_id, to_id);
}

pub fn on_reschedule_interrupt() {
    if let Some(s) = scheduler_opt() {
        s.flag_reschedule_local();
    }
}
