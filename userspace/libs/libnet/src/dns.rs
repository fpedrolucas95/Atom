use atom_syscall::ipc::PortId;
use libipc::messages::{MessageType, NetResolveMsg, NetResolveReplyMsg};
use libipc::protocol::{send_message, try_recv_message, get_payload};
use crate::NetError;

fn ipc_err(_e: atom_syscall::SyscallError) -> NetError {
    NetError::IpcError
}

/// Resolve a hostname to an IPv4 address (host byte order u32).
/// hostname must be <= 255 bytes.
pub fn net_resolve(netd_port: PortId, hostname: &str) -> Result<u32, NetError> {
    let hostname_bytes = hostname.as_bytes();
    if hostname_bytes.len() > 255 {
        return Err(NetError::InvalidArgument);
    }

    let reply_port = atom_syscall::ipc::create_port().map_err(ipc_err)?;

    let mut name = [0u8; 256];
    name[..hostname_bytes.len()].copy_from_slice(hostname_bytes);

    let msg = NetResolveMsg {
        reply_port: reply_port as u64,
        name_len: hostname_bytes.len() as u32,
        name,
    };

    if let Err(e) = send_message(netd_port, MessageType::NetResolve, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(ipc_err(e));
    }

    let mut buf = [0u8; 64];
    let mut result = Err(NetError::Timeout);

    for _ in 0..100000u32 {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
            let payload = get_payload(&buf, len);
            if let Some(reply) = NetResolveReplyMsg::from_bytes(payload) {
                if reply.error == 0 && reply.ip != 0 {
                    result = Ok(reply.ip);
                } else {
                    result = Err(NetError::DnsResolutionFailed);
                }
                break;
            }
        }
        atom_syscall::thread::yield_now();
    }

    let _ = atom_syscall::ipc::close_port(reply_port);
    result
}
