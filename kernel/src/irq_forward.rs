// IRQ Forwarding to User Space
//
// This module implements IRQ forwarding from kernel space to user space drivers.
// When an IRQ occurs, instead of handling it in the kernel, we forward it to a
// registered user space driver via IPC.
//
// Key responsibilities:
// - Maintain a mapping of IRQ number -> IPC Port
// - Forward IRQ notifications to registered user space handlers
// - Provide registration/unregistration interface for user space drivers
//
// Design:
// - Global table protected by spinlock
// - Fast lookup on IRQ (array indexed by IRQ number)
// - Send simple notification messages via IPC
//
// Safety:
// - Only one handler per IRQ
// - IRQ handlers call this from interrupt context
// - IPC send must be non-blocking

use spin::Mutex;
use crate::ipc::{self, PortId, Message};
use crate::thread::ThreadId;
use crate::{log_debug, log_warn};

const LOG_ORIGIN: &str = "irq_forward";
const MAX_IRQS: usize = 256;

#[derive(Clone, Copy)]
struct IrqMapping {
    port_id_raw: u64,
    registered: bool,
}

impl IrqMapping {
    const fn empty() -> Self {
        Self {
            port_id_raw: 0,
            registered: false,
        }
    }
}

static IRQ_TABLE: Mutex<[IrqMapping; MAX_IRQS]> = Mutex::new([IrqMapping::empty(); MAX_IRQS]);

/// Register an IRQ handler for user space
pub fn register_handler(irq_num: u8, port_id: PortId) -> Result<(), ()> {
    let mut table = IRQ_TABLE.lock();

    if table[irq_num as usize].registered {
        log_warn!(
            LOG_ORIGIN,
            "IRQ {} already registered, overwriting",
            irq_num
        );
    }

    table[irq_num as usize] = IrqMapping {
        port_id_raw: port_id.raw(),
        registered: true,
    };

    log_debug!(
        LOG_ORIGIN,
        "Registered IRQ {} -> Port {}",
        irq_num,
        port_id
    );

    Ok(())
}

/// Unregister an IRQ handler
pub fn unregister_handler(irq_num: u8) -> Result<(), ()> {
    let mut table = IRQ_TABLE.lock();

    if !table[irq_num as usize].registered {
        return Err(());
    }

    table[irq_num as usize] = IrqMapping::empty();

    log_debug!(LOG_ORIGIN, "Unregistered IRQ {}", irq_num);

    Ok(())
}

/// Forward an IRQ to user space
/// Called from interrupt context - must be fast and non-blocking
pub fn forward_irq(irq_num: u8) {
    let table = IRQ_TABLE.lock();
    let mapping = table[irq_num as usize];
    drop(table); // Release lock ASAP

    if !mapping.registered {
        // No handler registered, just return
        return;
    }

    // Create a simple notification message
    // We use the kernel's "IRQ forwarding" thread as sender
    let sender = ThreadId::from_raw(0); // Special ID for kernel
    let msg_type = irq_num as u32; // Message type = IRQ number
    let payload = alloc::vec![irq_num]; // Payload contains IRQ number

    let message = Message::new(sender, msg_type, payload);
    let port_id = PortId::from_raw(mapping.port_id_raw);

    // Try to send (non-blocking)
    match ipc::send_message_async(port_id, message) {
        Ok(_) => {
            // Success - IRQ forwarded
        }
        Err(ipc::IpcError::QueueFull) | Err(ipc::IpcError::WouldBlock) => {
            // Queue full - drop the IRQ notification
            // This is acceptable for input devices
            log_warn!(
                LOG_ORIGIN,
                "Dropped IRQ {} notification (queue full)",
                irq_num
            );
        }
        Err(e) => {
            log_warn!(
                LOG_ORIGIN,
                "Failed to forward IRQ {}: {:?}",
                irq_num,
                e
            );
        }
    }
}

/// Check if an IRQ has a registered handler
pub fn has_handler(irq_num: u8) -> bool {
    let table = IRQ_TABLE.lock();
    table[irq_num as usize].registered
}
