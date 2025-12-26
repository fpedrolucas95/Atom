//! IPC Protocol Helpers
//!
//! This module provides high-level functions for sending and receiving
//! typed messages over IPC ports.

extern crate alloc;

use alloc::vec::Vec;
use atom_syscall::ipc::{PortId, send, recv, send_async, try_recv};
use atom_syscall::SyscallResult;
use crate::messages::{MessageHeader, MessageType};

/// Send a typed message with header
pub fn send_message(port: PortId, msg_type: MessageType, payload: &[u8]) -> SyscallResult<()> {
    let header = MessageHeader::new(msg_type, payload.len() as u32);
    let header_bytes = header.to_bytes();

    let mut message = Vec::with_capacity(MessageHeader::SIZE + payload.len());
    message.extend_from_slice(&header_bytes);
    message.extend_from_slice(payload);

    send(port, &message)
}

/// Send a typed message asynchronously
pub fn send_message_async(port: PortId, msg_type: MessageType, payload: &[u8]) -> SyscallResult<()> {
    let header = MessageHeader::new(msg_type, payload.len() as u32);
    let header_bytes = header.to_bytes();

    let mut message = Vec::with_capacity(MessageHeader::SIZE + payload.len());
    message.extend_from_slice(&header_bytes);
    message.extend_from_slice(payload);

    send_async(port, &message)
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
