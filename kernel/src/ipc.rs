// Inter-Process Communication (IPC) Subsystem
//
// Implements message-based IPC for the Atom kernel, providing synchronous
// and asynchronous communication channels with priority inheritance,
// capability delegation, and rich observability.
//
// Key responsibilities:
// - Create and manage IPC ports owned by threads
// - Send and receive messages with bounded queues
// - Support blocking, non-blocking, and batched receive/send operations
// - Integrate shared memory for zero-copy message transfer
// - Enforce capability-based security during IPC
// - Detect and prevent common IPC deadlocks
//
// Message model:
// - Messages carry a sender, type, payload, optional capability, and timestamp
// - Payloads are size-limited; larger transfers require shared memory regions
// - Capabilities can be delegated via IPC using GRANT or MOVE semantics
//
// Design principles:
// - Deterministic bounds: queue depth, batch size, and message size are capped
// - Priority-aware IPC: blocked receivers trigger priority inheritance
// - Safety first: invalid ports, payload conflicts, and oversized messages
//   are rejected early
// - Observability: tracing, metrics, and statistics are first-class features
//
// Scheduling and blocking:
// - Threads may block waiting for messages with optional deadlines
// - Deadlock detection prevents circular wait across ports
// - Timer-driven wakeups handle IPC timeouts cleanly
// - Blocked threads are resumed with original priorities restored
//
// Performance optimizations:
// - Zero-copy threshold encourages shared memory for large messages
// - Batched send/receive reduces lock contention and syscall overhead
// - Next-message fast paths avoid unnecessary blocking
//
// Diagnostics and metrics:
// - Per-port statistics track throughput and latency
// - Ring-buffer tracing records recent send/receive events
// - Global IPC stats summarize system-wide activity
//
// Correctness and safety notes:
// - All shared IPC state is protected by spinlocks
// - Queue and waiter limits prevent resource exhaustion
// - Capability checks are enforced before message delivery
// - Time is derived from kernel ticks; coarse but deterministic
//
// This subsystem is a core building block for user-space services,
// enabling structured, secure, and observable communication.

#![allow(dead_code)]

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use crate::shared_mem;
use crate::shared_mem::RegionId;
use crate::thread::{ThreadId, ThreadPriority};
use crate::log_debug;
use crate::log_info;
use crate::log_warn;

pub const MAX_MESSAGE_SIZE: usize = 256;
pub const ZERO_COPY_THRESHOLD: usize = 128;
pub const MAX_BATCH_SIZE: usize = 32;
pub const MAX_QUEUE_DEPTH: usize = 64;

const LOG_ORIGIN: &str = "ipc";

const CONFIG_DEADLOCK_DETECT: bool = true;
const CONFIG_IPC_TRACE: bool = true;
const IPC_TRACE_RING_SIZE: usize = 1000;

#[inline(always)]
fn current_time_ms() -> u64 {
    crate::interrupts::get_ticks() * 10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortId(u64);

impl PortId {
    pub fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        PortId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
    
    pub fn from_raw(raw: u64) -> Self {
        PortId(raw)
    }

    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl Default for PortId {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Display for PortId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Port({})", self.0)
    }
}

#[derive(Debug, Clone)]
pub struct Message {
    pub sender: ThreadId,
    pub message_type: u32,
    pub payload: Vec<u8>,
    pub capability: Option<IpcCapability>,
    pub shared_region: Option<RegionId>,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone)]
pub enum IpcCapability {
    Grant {
        cap_handle: crate::cap::CapHandle,
        permissions: crate::cap::CapPermissions,
    },
    
    Move {
        cap_handle: crate::cap::CapHandle,
    },
}

impl Message {
    pub fn new(sender: ThreadId, message_type: u32, payload: Vec<u8>) -> Self {
        Self {
            sender,
            message_type,
            payload,
            capability: None,
            shared_region: None,
            timestamp_ms: current_time_ms(),
        }
    }

    pub fn new_with_shared_region(sender: ThreadId, message_type: u32, region_id: RegionId) -> Self {
        Self {
            sender,
            message_type,
            payload: Vec::new(),
            capability: None,
            shared_region: Some(region_id),
            timestamp_ms: current_time_ms(),
        }
    }
    
    pub fn new_with_grant(
        sender: ThreadId,
        message_type: u32,
        payload: Vec<u8>,
        cap_handle: crate::cap::CapHandle,
        permissions: crate::cap::CapPermissions,
    ) -> Self {
        Self {
            sender,
            message_type,
            payload,
            capability: Some(IpcCapability::Grant {
                cap_handle,
                permissions,
            }),
            shared_region: None,
            timestamp_ms: current_time_ms(),
        }
    }
    
    pub fn new_with_move(
        sender: ThreadId,
        message_type: u32,
        payload: Vec<u8>,
        cap_handle: crate::cap::CapHandle,
    ) -> Self {
        Self {
            sender,
            message_type,
            payload,
            capability: Some(IpcCapability::Move { cap_handle }),
            shared_region: None,
            timestamp_ms: current_time_ms(),
        }
    }

    pub fn size(&self) -> usize {
        self.payload.len()
    }

    pub fn has_capability(&self) -> bool {
        self.capability.is_some()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum IpcEventKind {
    Send,
    Receive,
}

impl IpcEventKind {
    pub const fn as_u64(&self) -> u64 {
        match self {
            IpcEventKind::Send => 0,
            IpcEventKind::Receive => 1,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct IpcTraceEvent {
    pub timestamp_ms: u64,
    pub kind: IpcEventKind,
    pub port: PortId,
    pub sender: ThreadId,
    pub receiver: Option<ThreadId>,
    pub size: usize,
}

impl Default for IpcTraceEvent {
    fn default() -> Self {
        Self {
            timestamp_ms: 0,
            kind: IpcEventKind::Send,
            port: PortId(0),
            sender: ThreadId::from_raw(0),
            receiver: None,
            size: 0,
        }
    }
}

struct IpcTraceBuffer {
    events: [Option<IpcTraceEvent>; IPC_TRACE_RING_SIZE],
    head: usize,
    full: bool,
}

impl IpcTraceBuffer {
    const fn new() -> Self {
        Self {
            events: [None; IPC_TRACE_RING_SIZE],
            head: 0,
            full: false,
        }
    }

    fn push(&mut self, event: IpcTraceEvent) {
        self.events[self.head] = Some(event);
        self.head = (self.head + 1) % IPC_TRACE_RING_SIZE;
        if self.head == 0 {
            self.full = true;
        }
    }

    fn snapshot(&self, max_events: usize) -> Vec<IpcTraceEvent> {
        let total = if self.full {
            IPC_TRACE_RING_SIZE
        } else {
            self.head
        };

        let mut output = Vec::new();
        let to_collect = core::cmp::min(total, max_events);

        for i in 0..to_collect {
            let idx = if self.full {
                (self.head + i) % IPC_TRACE_RING_SIZE
            } else {
                i
            };

            if let Some(event) = self.events[idx] {
                output.push(event);
            }
        }

        output
    }
}

#[derive(Debug, Clone)]
struct IpcPortMetrics {
    messages_sent: u64,
    messages_received: u64,
    bytes_sent: u64,
    bytes_received: u64,
    min_latency_ms: Option<u64>,
    max_latency_ms: Option<u64>,
    total_latency_ms: u128,
    first_message_timestamp_ms: Option<u64>,
    last_message_timestamp_ms: Option<u64>,
}

impl Default for IpcPortMetrics {
    fn default() -> Self {
        Self {
            messages_sent: 0,
            messages_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
            min_latency_ms: None,
            max_latency_ms: None,
            total_latency_ms: 0,
            first_message_timestamp_ms: None,
            last_message_timestamp_ms: None,
        }
    }
}

impl IpcPortMetrics {
    fn record_send(&mut self, size: usize, timestamp_ms: u64) {
        self.messages_sent += 1;
        self.bytes_sent += size as u64;
        if self.first_message_timestamp_ms.is_none() {
            self.first_message_timestamp_ms = Some(timestamp_ms);
        }
        self.last_message_timestamp_ms = Some(timestamp_ms);
    }

    fn record_receive(&mut self, size: usize, send_timestamp_ms: u64, receive_timestamp_ms: u64) {
        self.messages_received += 1;
        self.bytes_received += size as u64;

        let latency = receive_timestamp_ms.saturating_sub(send_timestamp_ms);

        self.min_latency_ms = Some(match self.min_latency_ms {
            Some(current_min) => current_min.min(latency),
            None => latency,
        });

        self.max_latency_ms = Some(match self.max_latency_ms {
            Some(current_max) => current_max.max(latency),
            None => latency,
        });

        self.total_latency_ms = self.total_latency_ms.saturating_add(latency as u128);
        if self.first_message_timestamp_ms.is_none() {
            self.first_message_timestamp_ms = Some(send_timestamp_ms);
        }
        self.last_message_timestamp_ms = Some(receive_timestamp_ms);
    }

    fn to_stats(&self) -> IpcPortStats {
        let avg_latency_ms = if self.messages_received > 0 {
            (self.total_latency_ms / self.messages_received as u128) as u64
        } else {
            0
        };

        let min_latency_ms = self.min_latency_ms.unwrap_or(0);
        let max_latency_ms = self.max_latency_ms.unwrap_or(0);

        let messages_per_second = if let (Some(first), Some(last)) = (
            self.first_message_timestamp_ms,
            self.last_message_timestamp_ms,
        ) {
            let duration_ms = last.saturating_sub(first).max(1);
            (self.messages_received.saturating_mul(1000)) / duration_ms
        } else {
            0
        };

        IpcPortStats {
            messages_sent: self.messages_sent,
            messages_received: self.messages_received,
            bytes_sent: self.bytes_sent,
            bytes_received: self.bytes_received,
            min_latency_ms,
            max_latency_ms,
            avg_latency_ms,
            messages_per_second,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IpcPortStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub min_latency_ms: u64,
    pub max_latency_ms: u64,
    pub avg_latency_ms: u64,
    pub messages_per_second: u64,
}

#[derive(Debug)]
struct PortState {
    id: PortId,
    owner: ThreadId,
    messages: VecDeque<Message>,
    receiver_blocked: Option<ThreadId>,
    max_waiter_priority: Option<ThreadPriority>,
    metrics: IpcPortMetrics,
}

impl PortState {
    fn new(id: PortId, owner: ThreadId) -> Self {
        Self {
            id,
            owner,
            messages: VecDeque::new(),
            receiver_blocked: None,
            max_waiter_priority: None,
            metrics: IpcPortMetrics::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WaiterInfo {
    port: PortId,
    deadline: Option<u64>,
}

struct IpcManager {
    ports: Mutex<BTreeMap<PortId, PortState>>,
    waiting_threads: Mutex<BTreeMap<ThreadId, WaiterInfo>>,
    trace: Mutex<IpcTraceBuffer>,
}

impl IpcManager {
    const fn new() -> Self {
        Self {
            ports: Mutex::new(BTreeMap::new()),
            waiting_threads: Mutex::new(BTreeMap::new()),
            trace: Mutex::new(IpcTraceBuffer::new()),
        }
    }

    fn create_port(&self, owner: ThreadId) -> PortId {
        let port_id = PortId::new();
        let port = PortState::new(port_id, owner);

        self.ports.lock().insert(port_id, port);
        port_id
    }

    fn port_owner(&self, port_id: PortId) -> Option<ThreadId> {
        self.ports.lock().get(&port_id).map(|port| port.owner)
    }

    fn close_port(&self, port_id: PortId, caller: ThreadId) -> Result<(), IpcError> {
        let mut ports = self.ports.lock();

        if let Some(port) = ports.get(&port_id) {
            if port.owner != caller {
                return Err(IpcError::PermissionDenied);
            }

            ports.remove(&port_id);
            Ok(())
        } else {
            Err(IpcError::InvalidPort)
        }
    }
    
    fn validate_payload_and_size(&self, message: &Message) -> Result<usize, IpcError> {
        if let Some(region) = message.shared_region {
            if !message.payload.is_empty() {
                return Err(IpcError::SharedMemoryPayloadConflict);
            }

            let info = shared_mem::get_region_info(region).map_err(|_| IpcError::InvalidSharedRegion)?;

            Ok(info.size)
        } else {
            if message.payload.len() > MAX_MESSAGE_SIZE {
                return Err(IpcError::MessageTooLarge);
            }

            if message.payload.len() > ZERO_COPY_THRESHOLD {
                return Err(IpcError::RequiresSharedMemory);
            }

            Ok(message.payload.len())
        }
    }

    fn resolve_message_size(&self, message: &Message) -> Result<usize, IpcError> {
        self.validate_payload_and_size(message)
    }

    fn send(&self, port_id: PortId, mut message: Message) -> Result<(), IpcError> {
        let mut ports = self.ports.lock();

        let port = ports.get_mut(&port_id).ok_or(IpcError::InvalidPort)?;

        let size = self.validate_payload_and_size(&message)?;

        if port.messages.len() >= MAX_QUEUE_DEPTH {
            return Err(IpcError::QueueFull);
        }

        if message.timestamp_ms == 0 {
            message.timestamp_ms = current_time_ms();
        }

        let timestamp_ms = message.timestamp_ms;
        let sender = message.sender;

        port.messages.push_back(message);
        port.metrics.record_send(size, timestamp_ms);
        self.record_trace_event(IpcTraceEvent {
            timestamp_ms,
            kind: IpcEventKind::Send,
            port: port_id,
            sender,
            receiver: None,
            size,
        });

        Ok(())
    }

    fn send_batch(&self, port_id: PortId, messages: Vec<Message>) -> Result<usize, IpcError> {
        if messages.is_empty() {
            return Ok(0);
        }

        if messages.len() > MAX_BATCH_SIZE {
            return Err(IpcError::BatchTooLarge);
        }

        let mut ports = self.ports.lock();
        let port = ports.get_mut(&port_id).ok_or(IpcError::InvalidPort)?;

        let mut prepared = Vec::with_capacity(messages.len());
        for mut msg in messages {
            let size = self.validate_payload_and_size(&msg)?;

            if msg.timestamp_ms == 0 {
                msg.timestamp_ms = current_time_ms();
            }

            prepared.push((msg, size));
        }

        if port.messages.len() + prepared.len() > MAX_QUEUE_DEPTH {
            return Err(IpcError::QueueFull);
        }

        let count = prepared.len();
        for (msg, size) in prepared {
            let timestamp_ms = msg.timestamp_ms;
            let sender = msg.sender;

            port.metrics.record_send(size, timestamp_ms);
            self.record_trace_event(IpcTraceEvent {
                timestamp_ms,
                kind: IpcEventKind::Send,
                port: port_id,
                sender,
                receiver: None,
                size,
            });

            port.messages.push_back(msg);
        }

        if let Some(receiver_id) = port.receiver_blocked.take() {
            port.max_waiter_priority = None;
            drop(ports);

            self.waiting_threads.lock().remove(&receiver_id);
            crate::sched::mark_thread_ready(receiver_id);
            self.restore_priority(receiver_id);
        }

        Ok(count)
    }
    
    fn recv_batch(&self, port_id: PortId, caller: ThreadId, max_count: usize)
        -> Result<Vec<Message>, IpcError>
    {
        let max_count = core::cmp::min(max_count, MAX_BATCH_SIZE);
        let mut ports = self.ports.lock();
        let port = ports.get_mut(&port_id).ok_or(IpcError::InvalidPort)?;

        let mut messages = Vec::new();

        for _ in 0..max_count {
            if let Some(msg) = port.messages.pop_front() {
                let receive_timestamp_ms = current_time_ms();
                let size = self.resolve_message_size(&msg)?;
                port
                    .metrics
                    .record_receive(size, msg.timestamp_ms, receive_timestamp_ms);

                self.record_trace_event(IpcTraceEvent {
                    timestamp_ms: receive_timestamp_ms,
                    kind: IpcEventKind::Receive,
                    port: port_id,
                    sender: msg.sender,
                    receiver: Some(caller),
                    size,
                });

                messages.push(msg);
            } else {
                break;
            }
        }
        Ok(messages)
    }

    fn try_recv(&self, port_id: PortId, caller: ThreadId) -> Result<Option<Message>, IpcError> {
        let mut ports = self.ports.lock();

        let port = ports.get_mut(&port_id).ok_or(IpcError::InvalidPort)?;

        if let Some(msg) = port.messages.pop_front() {
            let receive_timestamp_ms = current_time_ms();
            let size = self.resolve_message_size(&msg)?;

            port
                .metrics
                .record_receive(size, msg.timestamp_ms, receive_timestamp_ms);

            self.record_trace_event(IpcTraceEvent {
                timestamp_ms: receive_timestamp_ms,
                kind: IpcEventKind::Receive,
                port: port_id,
                sender: msg.sender,
                receiver: Some(caller),
                size,
            });

            Ok(Some(msg))
        } else {
            Ok(None)
        }
    }

    fn block_recv(
        &self,
        port_id: PortId,
        caller: ThreadId,
        caller_priority: ThreadPriority,
        deadline: Option<u64>,
    ) -> Result<(), IpcError> {
        {
            let ports = self.ports.lock();

            if !ports.contains_key(&port_id) {
                return Err(IpcError::InvalidPort);
            }
        }

        if CONFIG_DEADLOCK_DETECT && self.detect_deadlock(caller, port_id) {
            log_warn!(
                LOG_ORIGIN,
                "Deadlock detection prevented {} from blocking on {}",
                caller,
                port_id
            );
            return Err(IpcError::DeadlockDetected);
        }

        let mut ports = self.ports.lock();
        let port = ports.get_mut(&port_id).ok_or(IpcError::InvalidPort)?;

        if port.receiver_blocked.is_some() {
            return Err(IpcError::PortBusy);
        }

        port.receiver_blocked = Some(caller);

        port.max_waiter_priority = Some(
            port.max_waiter_priority
                .map(|p| p.max(caller_priority))
                .unwrap_or(caller_priority)
        );

        drop(ports);
        self.waiting_threads
            .lock()
            .insert(caller, WaiterInfo { port: port_id, deadline });

        Ok(())
    }
    
    fn get_max_waiter_priority(&self, port_id: PortId) -> Option<ThreadPriority> {
        self.ports
            .lock()
            .get(&port_id)
            .and_then(|p| p.max_waiter_priority)
    }

    fn detect_deadlock(&self, start: ThreadId, target_port: PortId) -> bool {
        let ports = self.ports.lock();
        let waiting = self.waiting_threads.lock();

        let mut current_port = Some(target_port);
        let mut steps = 0usize;

        while let Some(port_id) = current_port {
            if steps > ports.len() + waiting.len() {
                break;
            }

            let owner = match ports.get(&port_id) {
                Some(port) => port.owner,
                None => break,
            };

            if owner == start {
                return true;
            }

            current_port = waiting.get(&owner).map(|info| info.port);
            steps += 1;
        }

        false
    }

    fn handle_timeouts(&self, current_ticks: u64) {
        let mut expired: Vec<(ThreadId, PortId)> = Vec::new();

        {
            let waiting = self.waiting_threads.lock();
            for (tid, info) in waiting.iter() {
                if let Some(deadline) = info.deadline {
                    if current_ticks >= deadline {
                        expired.push((*tid, info.port));
                    }
                }
            }
        }

        if expired.is_empty() {
            return;
        }

        {
            let mut ports = self.ports.lock();
            for (tid, port_id) in &expired {
                if let Some(port) = ports.get_mut(port_id) {
                    if port.receiver_blocked == Some(*tid) {
                        port.receiver_blocked = None;
                        port.max_waiter_priority = None;
                    }
                }
            }
        }

        {
            let mut waiting = self.waiting_threads.lock();
            for (tid, _) in expired.iter() {
                waiting.remove(tid);
            }
        }

        for (tid, port_id) in expired {
            log_warn!(LOG_ORIGIN, "Timeout waking blocked thread {} on {}", tid, port_id);
            crate::sched::mark_thread_ready(tid);
            self.restore_priority(tid);
        }
    }

    fn record_trace_event(&self, event: IpcTraceEvent) {
        {
            let mut trace = self.trace.lock();
            trace.push(event);
        }

        match event.kind {
            IpcEventKind::Send => log_debug!(
                LOG_ORIGIN,
                "TRACE send: sender={} port={} size={}B ts={}ms",
                event.sender,
                event.port,
                event.size,
                event.timestamp_ms
            ),
            IpcEventKind::Receive => log_debug!(
                LOG_ORIGIN,
                "TRACE recv: receiver={} from={} size={}B ts={}ms",
                event.receiver.unwrap_or(ThreadId::from_raw(0)),
                event.sender,
                event.size,
                event.timestamp_ms
            ),
        }
    }

    fn get_trace_events(&self, max_events: usize) -> Vec<IpcTraceEvent> {
        self.trace.lock().snapshot(max_events)
    }

    fn restore_priority(&self, thread_id: ThreadId) {
        crate::sched::restore_original_priority(thread_id);
    }

    fn port_stats(&self, port_id: PortId) -> Result<IpcPortStats, IpcError> {
        let ports = self.ports.lock();
        let port = ports.get(&port_id).ok_or(IpcError::InvalidPort)?;
        Ok(port.metrics.to_stats())
    }

    fn get_stats(&self) -> IpcStats {
        let ports = self.ports.lock();
        let waiting = self.waiting_threads.lock();

        let total_messages: usize = ports.values().map(|p| p.messages.len()).sum();

        IpcStats {
            total_ports: ports.len(),
            total_messages,
            blocked_threads: waiting.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IpcStats {
    pub total_ports: usize,
    pub total_messages: usize,
    pub blocked_threads: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcError {
    InvalidPort,
    PermissionDenied,
    MessageTooLarge,
    PortBusy,
    WouldBlock,
    BatchTooLarge,
    Timeout,
    QueueFull,
    DeadlockDetected,
    InvalidSharedRegion,
    RequiresSharedMemory,
    SharedMemoryPayloadConflict,
}

impl core::fmt::Display for IpcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IpcError::InvalidPort => write!(f, "Invalid port"),
            IpcError::PermissionDenied => write!(f, "Permission denied"),
            IpcError::MessageTooLarge => write!(f, "Message too large"),
            IpcError::PortBusy => write!(f, "Port busy"),
            IpcError::WouldBlock => write!(f, "Would block"),
            IpcError::BatchTooLarge => write!(f, "Batch too large"),
            IpcError::Timeout => write!(f, "Operation timed out"),
            IpcError::QueueFull => write!(f, "Port queue full"),
            IpcError::DeadlockDetected => write!(f, "Deadlock detected"),
            IpcError::InvalidSharedRegion => write!(f, "Invalid shared region"),
            IpcError::RequiresSharedMemory => write!(f, "Use shared memory for large payload"),
            IpcError::SharedMemoryPayloadConflict => {
                write!(f, "Inline payload is not allowed with shared regions")
            }
        }
    }
}

static IPC_MANAGER: IpcManager = IpcManager::new();

pub fn init() {
    log_info!(
        LOG_ORIGIN,
        "IPC subsystem with priority inheritance initialized (Phase 4.7)"
    );

    log_info!(
        LOG_ORIGIN,
        "Capability-aware IPC enabled: auto-grant on create, permission checks on send/recv"
    );

    log_debug!(
        LOG_ORIGIN,
        "Capability delegation via SYS_IPC_SEND_WITH_CAP (grant/move) is active"
    );

    log_info!(
        LOG_ORIGIN,
        "Phase 4.4 optimizations: zero-copy threshold={}B, batching up to {} messages",
        ZERO_COPY_THRESHOLD,
        MAX_BATCH_SIZE
    );

    log_info!(
        LOG_ORIGIN,
        "Phase 4.6 safeguards: bounded queues ({} messages) and timeout-aware waiters",
        MAX_QUEUE_DEPTH
    );

    log_info!(
        LOG_ORIGIN,
        "Phase 4.7 observability: tracing={}, ring depth={}, per-port metrics enabled",
        CONFIG_IPC_TRACE,
        IPC_TRACE_RING_SIZE
    );
}

pub fn create_port(owner: ThreadId) -> PortId {
    IPC_MANAGER.create_port(owner)
}

pub fn get_port_owner(port_id: PortId) -> Option<ThreadId> {
    IPC_MANAGER.port_owner(port_id)
}

pub fn close_port(port_id: PortId, caller: ThreadId) -> Result<(), IpcError> {
    IPC_MANAGER.close_port(port_id, caller)
}

pub fn send_message(port_id: PortId, message: Message) -> Result<(), IpcError> {
    IPC_MANAGER.send(port_id, message)
}

pub fn send_message_async(port_id: PortId, message: Message) -> Result<(), IpcError> {
    IPC_MANAGER.send(port_id, message)
}

pub fn try_receive_message(port_id: PortId, caller: ThreadId) -> Result<Option<Message>, IpcError> {
    IPC_MANAGER.try_recv(port_id, caller)
}

pub fn block_receive(
    port_id: PortId,
    caller: ThreadId,
    caller_priority: ThreadPriority,
    deadline: Option<u64>,
) -> Result<(), IpcError> {
    IPC_MANAGER.block_recv(port_id, caller, caller_priority, deadline)
}

pub fn get_max_waiter_priority(port_id: PortId) -> Option<ThreadPriority> {
    IPC_MANAGER.get_max_waiter_priority(port_id)
}

pub fn on_timer_tick(current_ticks: u64) {
    IPC_MANAGER.handle_timeouts(current_ticks);
}

pub fn get_stats() -> IpcStats {
    IPC_MANAGER.get_stats()
}

pub fn get_port_stats(port_id: PortId) -> Result<IpcPortStats, IpcError> {
    IPC_MANAGER.port_stats(port_id)
}

pub fn read_trace(max_events: usize) -> Vec<IpcTraceEvent> {
    IPC_MANAGER.get_trace_events(max_events)
}

pub fn send_batch(port_id: PortId, messages: Vec<Message>) -> Result<usize, IpcError> {
    IPC_MANAGER.send_batch(port_id, messages)
}

pub fn receive_batch(port_id: PortId, caller: ThreadId, max_count: usize) -> Result<Vec<Message>, IpcError> {
    IPC_MANAGER.recv_batch(port_id, caller, max_count)
}