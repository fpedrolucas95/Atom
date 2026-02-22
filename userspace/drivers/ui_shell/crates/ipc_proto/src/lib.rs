//! Desktop IPC communication

#![no_std]

use atom_syscall::ipc::{PortId, create_port};

pub struct DesktopIpc {
    pub event_port: PortId,
    pub register_port: PortId,
}

impl DesktopIpc {
    pub fn new() -> Self {
        Self {
            event_port: create_port().expect("event_port"),
            register_port: create_port().expect("reg_port"),
        }
    }
}
