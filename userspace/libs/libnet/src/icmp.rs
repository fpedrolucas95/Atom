use alloc::vec::Vec;
use atom_syscall::ipc::PortId;
use libipc::messages::{
    MessageType,
    NetIcmpEchoRequestMsg, NetIcmpEchoReplyMsg,
};
use libipc::protocol::{send_message, get_payload};
use crate::{NetError, IpAddr};

#[derive(Debug, Clone)]
pub struct IcmpEchoReply {
    pub src_ip: IpAddr,
    pub sequence: u16,
    pub ttl: u8,
    pub rtt_ms: u32,
    pub payload: Vec<u8>,
}

/// Send an ICMP Echo Request and wait for the reply (blocking).
pub fn net_icmp_echo(
    netd_port: PortId,
    dest_ip: IpAddr,
    sequence: u16,
    payload: &[u8],
    timeout_ms: u32,
) -> Result<IcmpEchoReply, NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(|_| NetError::IpcError)?;

    let mut msg = NetIcmpEchoRequestMsg {
        reply_port: reply_port as u64,
        dest_ip: dest_ip.to_net_ip_addr(),
        sequence,
        timeout_ms,
        payload_len: payload.len() as u32,
        payload: [0u8; 64],
    };
    let copy_len = payload.len().min(64);
    msg.payload[..copy_len].copy_from_slice(&payload[..copy_len]);

    if let Err(_) = send_message(netd_port, MessageType::NetIcmpEchoRequest, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(NetError::IpcError);
    }

    // Blocking wait using kernel-level timeout
    let mut response_buf = [0u8; 256];
    let ports = [reply_port];

    match atom_syscall::ipc::wait_any(&ports, timeout_ms as u64) {
        Ok(_) => {
            if let Ok(len) = atom_syscall::ipc::recv(reply_port, &mut response_buf) {
                if let Some(header) = libipc::messages::MessageHeader::from_bytes(&response_buf) {
                    if header.msg_type == MessageType::NetIcmpEchoReply {
                        let payload_bytes = get_payload(&response_buf, len);
                        if let Some(reply_msg) = NetIcmpEchoReplyMsg::from_bytes(payload_bytes) {
                            if reply_msg.error == 0 {
                                let mut reply_payload = Vec::with_capacity(reply_msg.payload_len as usize);
                                reply_payload.extend_from_slice(&reply_msg.payload[..reply_msg.payload_len as usize]);

                                let result = IcmpEchoReply {
                                    src_ip: IpAddr::from_net_ip_addr(reply_msg.src_ip),
                                    sequence: reply_msg.sequence,
                                    ttl: reply_msg.ttl,
                                    rtt_ms: reply_msg.rtt_ms,
                                    payload: reply_payload,
                                };
                                let _ = atom_syscall::ipc::close_port(reply_port);
                                return Ok(result);
                            } else if reply_msg.error == 1 {
                                let _ = atom_syscall::ipc::close_port(reply_port);
                                return Err(NetError::Timeout);
                            }
                        }
                    }
                }
            }
            let _ = atom_syscall::ipc::close_port(reply_port);
            Err(NetError::IpcError)
        }
        Err(atom_syscall::SyscallError::TimedOut) => {
            let _ = atom_syscall::ipc::close_port(reply_port);
            Err(NetError::Timeout)
        }
        Err(_) => {
            let _ = atom_syscall::ipc::close_port(reply_port);
            Err(NetError::IpcError)
        }
    }
}
