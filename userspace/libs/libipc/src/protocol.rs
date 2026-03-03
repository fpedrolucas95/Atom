//! IPC Protocol Helpers
//!
//! This module provides high-level functions for sending and receiving
//! typed messages over IPC ports.

extern crate alloc;

use alloc::vec::Vec;
use atom_syscall::ipc::{PortId, send, recv, send_async, try_recv, close_port};
use atom_syscall::SyscallResult;
use crate::messages::{MessageHeader, MessageType, NsRegisterMsg, NsLookupMsg, NsResponseMsg};

/// Maximum payload that fits in the stack-based send buffer.
/// Control messages are well under this limit; variable-length data
/// (e.g. window titles) are only sent during initialization.
const MAX_STACK_MSG: usize = 240;

/// Send a typed message with header.
///
/// Uses a stack buffer for messages up to `MAX_STACK_MSG` bytes to
/// avoid heap allocations in the hot path (critical for userspace
/// apps with bump allocators).
pub fn send_message(port: PortId, msg_type: MessageType, payload: &[u8]) -> SyscallResult<()> {
    let header = MessageHeader::new(msg_type, payload.len() as u32);
    let header_bytes = header.to_bytes();
    let total = MessageHeader::SIZE + payload.len();

    if total <= MAX_STACK_MSG {
        let mut buf = [0u8; MAX_STACK_MSG];
        buf[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
        buf[MessageHeader::SIZE..total].copy_from_slice(payload);
        send(port, &buf[..total])
    } else {
        let mut message = Vec::with_capacity(total);
        message.extend_from_slice(&header_bytes);
        message.extend_from_slice(payload);
        send(port, &message)
    }
}

/// Send a typed message asynchronously.
///
/// Uses a stack buffer for messages up to `MAX_STACK_MSG` bytes.
pub fn send_message_async(port: PortId, msg_type: MessageType, payload: &[u8]) -> SyscallResult<()> {
    let header = MessageHeader::new(msg_type, payload.len() as u32);
    let header_bytes = header.to_bytes();
    let total = MessageHeader::SIZE + payload.len();

    if total <= MAX_STACK_MSG {
        let mut buf = [0u8; MAX_STACK_MSG];
        buf[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
        buf[MessageHeader::SIZE..total].copy_from_slice(payload);
        send_async(port, &buf[..total])
    } else {
        let mut message = Vec::with_capacity(total);
        message.extend_from_slice(&header_bytes);
        message.extend_from_slice(payload);
        send_async(port, &message)
    }
}

/// Receive a message and parse its header
pub fn recv_message(port: PortId, buffer: &mut [u8]) -> SyscallResult<(MessageHeader, usize)> {
    let len = recv(port, buffer)?;

    if len < MessageHeader::SIZE {
        return Err(atom_syscall::SyscallError::InvalidArgument);
    }

    let header = MessageHeader::from_bytes(&buffer[..MessageHeader::SIZE])
        .ok_or(atom_syscall::SyscallError::InvalidArgument)?;

    Ok((header, len))
}

/// Try to receive a message without blocking
pub fn try_recv_message(port: PortId, buffer: &mut [u8]) -> SyscallResult<Option<(MessageHeader, usize)>> {
    match try_recv(port, buffer)? {
        Some(len) if len >= MessageHeader::SIZE => {
            let header = MessageHeader::from_bytes(&buffer[..MessageHeader::SIZE])
                .ok_or(atom_syscall::SyscallError::InvalidArgument)?;
            Ok(Some((header, len)))
        }
        Some(_) => Err(atom_syscall::SyscallError::InvalidArgument),
        None => Ok(None),
    }
}

/// Get the payload portion of a received message
pub fn get_payload(buffer: &[u8], total_len: usize) -> &[u8] {
    if total_len > MessageHeader::SIZE {
        &buffer[MessageHeader::SIZE..total_len]
    } else {
        &[]
    }
}

/// Helper to check if a message matches expected type
pub fn is_message_type(header: &MessageHeader, expected: MessageType) -> bool {
    header.msg_type == expected
}

fn str_to_name(name: &str) -> [u8; 32] {
    let mut buf = [0u8; 32];
    let bytes = name.as_bytes();
    let len = bytes.len().min(32);
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// Register a service with the name service
pub fn register_service(name: &str, port: PortId) -> SyscallResult<()> {
    let msg = NsRegisterMsg {
        port: port,
        name: str_to_name(name),
    };
    send_message(crate::well_known::NAME_SERVICE, MessageType::NsRegister, &msg.to_bytes())
}

/// Look up a service by name via the name service
pub fn lookup_service(name: &str) -> SyscallResult<PortId> {
    let reply_port = atom_syscall::ipc::create_port()?;

    let lookup_msg = NsLookupMsg {
        reply_port: reply_port,
        name: str_to_name(name),
    };

    if let Err(e) = send_message(crate::well_known::NAME_SERVICE, MessageType::NsLookup, &lookup_msg.to_bytes()) {
        let _ = close_port(reply_port);
        return Err(e);
    }

    // Wait for response with reasonable timeout
    // 10000 yields gives enough time for namesvc to process the lookup
    // even if the system is busy initializing other services
    let mut buffer = [0u8; 128];
    let mut result = Err(atom_syscall::SyscallError::TimedOut);

    for _ in 0..10000 {
        if let Ok(Some((header, len))) = try_recv_message(reply_port, &mut buffer) {
            if header.msg_type == MessageType::NsResponse {
                let payload = get_payload(&buffer, len);
                if let Some(resp) = NsResponseMsg::from_bytes(payload) {
                    if resp.port != 0 {
                        result = Ok(resp.port);
                    } else {
                        result = Err(atom_syscall::SyscallError::NotFound);
                    }
                    break;
                }
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = close_port(reply_port);
    result
}
