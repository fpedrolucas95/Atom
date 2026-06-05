use crate::{IpAddr, NetError};
use atom_syscall::ipc::PortId;
use libipc::messages::{MessageType, NetGetConfigMsg, NetGetConfigReplyMsg};
use libipc::protocol::{get_payload, send_message};

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub ip: IpAddr,
    pub netmask: IpAddr,
    pub gateway: IpAddr,
    pub dns: IpAddr,
    pub mac: [u8; 6],
}

pub fn net_get_config(netd_port: PortId) -> Result<NetworkConfig, NetError> {
    let reply_port = atom_syscall::ipc::create_port().map_err(|_| NetError::IpcError)?;

    let msg = NetGetConfigMsg {
        reply_port: reply_port as u64,
    };

    if let Err(_) = send_message(netd_port, MessageType::NetGetConfig, &msg.to_bytes()) {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return Err(NetError::IpcError);
    }

    let mut buf = [0u8; 64];
    let ports = [reply_port];

    match atom_syscall::ipc::wait_any(&ports, 5000) {
        Ok(_) => {
            if let Ok(len) = atom_syscall::ipc::recv(reply_port, &mut buf) {
                if let Some(header) = libipc::messages::MessageHeader::from_bytes(&buf) {
                    if header.msg_type == MessageType::NetGetConfigReply {
                        let payload = get_payload(&buf, len);
                        if let Some(reply) = NetGetConfigReplyMsg::from_bytes(payload) {
                            let config = NetworkConfig {
                                ip: IpAddr::from_u32(reply.own_ip),
                                netmask: IpAddr::from_u32(reply.netmask),
                                gateway: IpAddr::from_u32(reply.gateway),
                                dns: IpAddr::from_u32(reply.dns_server),
                                mac: reply.mac,
                            };
                            let _ = atom_syscall::ipc::close_port(reply_port);
                            return Ok(config);
                        }
                    }
                }
            }
            let _ = atom_syscall::ipc::close_port(reply_port);
            Err(NetError::IpcError)
        }
        _ => {
            let _ = atom_syscall::ipc::close_port(reply_port);
            Err(NetError::Timeout)
        }
    }
}
