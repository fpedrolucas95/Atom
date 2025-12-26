// libipc - Inter-Process Communication Library for Atom OS
//
// This library provides a standardized message-passing protocol for
// communication between all userspace services and applications.
//
// The library builds on top of the kernel's IPC primitives (syscalls)
// and provides:
// - Well-known service ports and discovery
// - Typed message protocol definitions
// - Client/server communication patterns
// - Error handling and result types
//
// Architecture:
// - Services register on well-known ports
// - Clients connect to services by port ID
// - Messages are serialized/deserialized using a simple binary protocol
// - Large data is passed via shared memory regions

#![no_std]
#![allow(dead_code)]

extern crate alloc;

pub mod protocol;
pub mod services;
pub mod client;
pub mod server;
pub mod error;

pub use protocol::{Message, MessageType, MessageHeader};
pub use services::{ServicePort, SERVICE_DESKTOP, SERVICE_INPUT, SERVICE_DISPLAY};
pub use client::Client;
pub use server::Server;
pub use error::{IpcError, IpcResult};
