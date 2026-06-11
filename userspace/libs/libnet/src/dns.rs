use crate::NetError;
use alloc::string::String;
use atom_syscall::ipc::PortId;
use libipc::messages::{MessageType, NetResolveMsg, NetResolveReplyMsg};
use libipc::protocol::{get_payload, send_message, try_recv_message};

fn ipc_err(_e: atom_syscall::SyscallError) -> NetError {
    NetError::IpcError
}

use crate::IpAddr;

/// Try to parse a dotted-decimal IPv4 string. Returns the 4 octets on success.
fn try_parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0;
    let mut cur: u16 = 0;
    let mut has_digits = false;

    for b in s.bytes() {
        if b == b'.' {
            if !has_digits || idx >= 3 || cur > 255 {
                return None;
            }
            octets[idx] = cur as u8;
            idx += 1;
            cur = 0;
            has_digits = false;
        } else if b.is_ascii_digit() {
            cur = cur * 10 + (b - b'0') as u16;
            if cur > 255 {
                return None;
            }
            has_digits = true;
        } else {
            return None;
        }
    }

    if idx != 3 || !has_digits {
        return None;
    }
    octets[3] = cur as u8;
    Some(octets)
}

/// Resolve a hostname to an IP address.
/// If hostname is already a dotted-decimal IPv4 address, it is returned directly
/// without a DNS query. hostname must be <= 255 bytes.
pub fn net_resolve(netd_port: PortId, hostname: &str) -> Result<IpAddr, NetError> {
    // Short-circuit: if the input is already an IPv4 address, return it directly.
    if let Some(octets) = try_parse_ipv4(hostname) {
        return Ok(IpAddr::V4(octets));
    }

    let mut normalized = String::new();
    let query_name = if hostname.bytes().any(|b| b == b'.') {
        hostname
    } else {
        normalized.push_str(hostname);
        normalized.push_str(".com");
        &normalized
    };

    let hostname_bytes = query_name.as_bytes();
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

    // DNS may take a couple of seconds while ARP and the first query settle.
    // Counting scheduler yields made this timeout CPU-speed dependent and
    // caused the reply port to close before otherwise valid responses arrived.
    let deadline = atom_syscall::thread::get_ticks().saturating_add(500);
    while atom_syscall::thread::get_ticks() < deadline {
        if let Ok(Some((_header, len))) = try_recv_message(reply_port, &mut buf) {
            let payload = get_payload(&buf, len);
            if let Some(reply) = NetResolveReplyMsg::from_bytes(payload) {
                if reply.error == 0 && reply.ip != 0 {
                    result = Ok(IpAddr::from_u32(reply.ip));
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
