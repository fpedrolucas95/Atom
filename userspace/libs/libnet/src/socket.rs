use crate::NetError;
use alloc::vec::Vec;
use atom_syscall::ipc::PortId;
use libipc::messages::{
    MessageType, NetCloseMsg, NetCloseReplyMsg, NetConnectMsg, NetConnectReplyMsg, NetRecvMsg,
    NetRecvReplyMsg, NetSendMsg, NetSendReplyMsg, NetSocketMsg, NetSocketReplyMsg,
};
use libipc::protocol::{get_payload, send_message, try_recv_message};

fn ipc_err(_e: atom_syscall::SyscallError) -> NetError {
    NetError::IpcError
}

/// Create a TCP socket. Returns socket_id.
pub fn net_socket(netd_port: PortId) -> Result<u32, NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

    let msg = NetSocketMsg {
        reply_port: reply_port as u64,
        proto: 0, // TCP
        _pad: [0u8; 7],
    };

    if let Err(e) = send_message(netd_port, MessageType::NetSocket, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(ipc_err(e));
    }

    let mut buf = [0u8; 64];
    let mut result = Err(NetError::Timeout);

    for _ in 0..50000u32 {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
            let payload = get_payload(&buf, len);
            if let Some(reply) = NetSocketReplyMsg::from_bytes(payload) {
                if reply.error == 0 {
                    result = Ok(reply.socket_id);
                } else {
                    result = Err(NetError::IpcError);
                }
                break;
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = atom_syscall::ipc::close_port(reply_port);
    result
}

use crate::IpAddr;

/// Connect a TCP socket to remote_ip:remote_port.
pub fn net_connect(
    netd_port: PortId,
    socket_id: u32,
    remote_ip: IpAddr,
    remote_port: u16,
) -> Result<(), NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

    let ipv4 = remote_ip.to_u32().ok_or(NetError::InvalidArgument)?;

    let msg = NetConnectMsg {
        reply_port: reply_port as u64,
        socket_id,
        remote_ip: ipv4,
        remote_port,
        _pad: [0u8; 2],
    };

    if let Err(e) = send_message(netd_port, MessageType::NetConnect, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(ipc_err(e));
    }

    let mut buf = [0u8; 64];
    let mut result = Err(NetError::Timeout);

    // Connect can take longer — allow up to ~100000 yields
    for _ in 0..100000u32 {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
            let payload = get_payload(&buf, len);
            if let Some(reply) = NetConnectReplyMsg::from_bytes(payload) {
                if reply.error == 0 {
                    result = Ok(());
                } else {
                    result = Err(NetError::ConnectionRefused);
                }
                break;
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = atom_syscall::ipc::close_port(reply_port);
    result
}

/// Send data over a socket. Chunks data into 1024-byte pieces if needed.
pub fn net_send(netd_port: PortId, socket_id: u32, data: &[u8]) -> Result<usize, NetError> {
    let mut total_sent = 0usize;
    let mut offset = 0usize;

    while offset < data.len() {
        let chunk_len = (data.len() - offset).min(1024);
        let chunk = &data[offset..offset + chunk_len];

        let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

        let mut msg = NetSendMsg {
            reply_port: reply_port as u64,
            socket_id,
            len: chunk_len as u32,
            data: [0u8; 1024],
        };
        msg.data[..chunk_len].copy_from_slice(chunk);

        if let Err(e) = send_message(netd_port, MessageType::NetSend, &msg.to_bytes()) {
            let _ = atom_syscall::ipc::close_port(reply_port);
            return Err(ipc_err(e));
        }

        let mut buf = [0u8; 64];
        let mut chunk_result = Err(NetError::Timeout);

        for _ in 0..50000u32 {
            if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
                let payload = get_payload(&buf, len);
                if let Some(reply) = NetSendReplyMsg::from_bytes(payload) {
                    if reply.error == 0 {
                        chunk_result = Ok(reply.sent as usize);
                    } else {
                        chunk_result = Err(NetError::TcpError);
                    }
                    break;
                }
            }
            atom_syscall::thread::yield_now();
        }

        let _ = atom_syscall::ipc::close_port(reply_port);

        let sent = chunk_result?;
        total_sent += sent;
        offset += chunk_len;
    }

    Ok(total_sent)
}

/// Receive data from a socket into buf. timeout_ms = 0 means use default (10000ms).
pub fn net_recv(
    netd_port: PortId,
    socket_id: u32,
    buf: &mut [u8],
    timeout_ms: u32,
) -> Result<usize, NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

    let effective_timeout = if timeout_ms == 0 { 10000 } else { timeout_ms };

    let msg = NetRecvMsg {
        reply_port: reply_port as u64,
        socket_id,
        max_len: buf.len() as u32,
        timeout_ms: effective_timeout,
    };

    if let Err(e) = send_message(netd_port, MessageType::NetRecv, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(ipc_err(e));
    }

    // Use a heap-allocated buffer for the large reply (NetRecvReplyMsg is 1036 bytes)
    let mut recv_buf: Vec<u8> = alloc::vec![0; 1100];

    let mut result = Err(NetError::Timeout);

    let deadline = atom_syscall::thread::get_ticks()
        .saturating_add((effective_timeout as u64 + 9) / 10);
    while atom_syscall::thread::get_ticks() < deadline {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut recv_buf) {
            let payload = get_payload(&recv_buf, len);
            if let Some(reply) = NetRecvReplyMsg::from_bytes(payload) {
                if reply.error == 0 {
                    let data_len = (reply.len as usize).min(buf.len()).min(1024);
                    buf[..data_len].copy_from_slice(&reply.data[..data_len]);
                    result = Ok(data_len);
                } else {
                    result = Err(NetError::NotConnected);
                }
                break;
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = atom_syscall::ipc::close_port(reply_port);
    result
}

/// Close a socket.
pub fn net_close(netd_port: PortId, socket_id: u32) -> Result<(), NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

    let msg = NetCloseMsg {
        reply_port: reply_port as u64,
        socket_id,
        _pad: [0u8; 4],
    };

    if let Err(e) = send_message(netd_port, MessageType::NetClose, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(ipc_err(e));
    }

    let mut buf = [0u8; 64];
    let mut result = Err(NetError::Timeout);

    for _ in 0..50000u32 {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
            let payload = get_payload(&buf, len);
            if let Some(reply) = NetCloseReplyMsg::from_bytes(payload) {
                if reply.error == 0 {
                    result = Ok(());
                } else {
                    result = Err(NetError::IpcError);
                }
                break;
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = atom_syscall::ipc::close_port(reply_port);
    result
}
