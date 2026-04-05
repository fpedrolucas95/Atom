use crate::tcp::TcpManager;
use crate::dns::DnsCache;
use libipc::messages::{
    NetSocketMsg, NetSocketReplyMsg,
    NetConnectMsg, NetConnectReplyMsg,
    NetSendMsg, NetSendReplyMsg,
    NetRecvMsg, NetRecvReplyMsg,
    NetCloseMsg, NetCloseReplyMsg,
    NetResolveMsg, NetResolveReplyMsg,
};

/// A pending deferred recv: socket waiting for data
pub struct PendingRecv {
    pub socket_id: u32,
    pub reply_port: u64,
    pub max_len: u32,
    pub in_use: bool,
}

impl PendingRecv {
    const fn new() -> Self {
        Self {
            socket_id: 0,
            reply_port: 0,
            max_len: 0,
            in_use: false,
        }
    }
}

/// A pending DNS resolve: waiting for DNS response to arrive
pub struct PendingResolve {
    pub name: [u8; 256],
    pub name_len: usize,
    pub reply_port: u64,
    pub in_use: bool,
    pub needs_resend: bool,  // true = re-send DNS query on next tick (after ARP reply)
    pub gateway_mac: [u8; 6], // MAC to use for resend
}

impl PendingResolve {
    const fn new() -> Self {
        Self {
            name: [0u8; 256],
            name_len: 0,
            reply_port: 0,
            in_use: false,
            needs_resend: false,
            gateway_mac: [0u8; 6],
        }
    }
}

/// A pending TCP connect: waiting for SYN-ACK before replying to app
pub struct PendingConnect {
    pub socket_id: u32,
    pub reply_port: u64,
    pub in_use: bool,
}

impl PendingConnect {
    const fn new() -> Self {
        Self {
            socket_id: 0,
            reply_port: 0,
            in_use: false,
        }
    }
}

pub struct SocketManager {
    pub pending_recvs: [PendingRecv; 8],
    pub pending_resolves: [PendingResolve; 4],
    pub pending_connects: [PendingConnect; 8],
}

impl SocketManager {
    pub fn new() -> Self {
        Self {
            pending_recvs: [
                PendingRecv::new(), PendingRecv::new(), PendingRecv::new(), PendingRecv::new(),
                PendingRecv::new(), PendingRecv::new(), PendingRecv::new(), PendingRecv::new(),
            ],
            pending_resolves: [
                PendingResolve::new(), PendingResolve::new(),
                PendingResolve::new(), PendingResolve::new(),
            ],
            pending_connects: [
                PendingConnect::new(), PendingConnect::new(), PendingConnect::new(), PendingConnect::new(),
                PendingConnect::new(), PendingConnect::new(), PendingConnect::new(), PendingConnect::new(),
            ],
        }
    }

    /// Handle NetSocket: allocate a TCP socket, return reply.
    pub fn handle_net_socket(
        &mut self,
        payload: &[u8],
        tcp: &mut TcpManager,
    ) -> Option<(u64, [u8; NetSocketReplyMsg::SIZE])> {
        let msg = NetSocketMsg::from_bytes(payload)?;
        let socket_id = tcp.alloc_socket(msg.reply_port).unwrap_or(0);
        let error = if socket_id == 0 { 1 } else { 0 };
        let reply = NetSocketReplyMsg { socket_id, error };
        Some((msg.reply_port, reply.to_bytes()))
    }

    /// Handle NetConnect: send SYN and store pending connect — reply only after SYN-ACK.
    pub fn handle_net_connect(
        &mut self,
        payload: &[u8],
        src_ip: u32,
        tcp: &mut TcpManager,
        out_pkt: &mut [u8],
    ) -> Option<(u64, [u8; NetConnectReplyMsg::SIZE], usize)> {
        let msg = NetConnectMsg::from_bytes(payload)?;
        let now = atom_syscall::thread::get_ticks();
        let pkt_len = tcp.connect_with_ip(
            msg.socket_id,
            src_ip,
            msg.remote_ip,
            msg.remote_port,
            now,
            out_pkt,
        ).unwrap_or(0);

        if pkt_len == 0 {
            // Failed to build SYN — reply with error immediately
            let reply = NetConnectReplyMsg { socket_id: msg.socket_id, error: 1 };
            return Some((msg.reply_port, reply.to_bytes(), 0));
        }

        // Store pending connect — will reply when TcpEvent::Connected fires
        for pc in self.pending_connects.iter_mut() {
            if !pc.in_use {
                pc.socket_id = msg.socket_id;
                pc.reply_port = msg.reply_port;
                pc.in_use = true;
                break;
            }
        }

        // Return the SYN packet but no IPC reply yet (reply_port=0 signals caller to skip send)
        Some((0, [0u8; NetConnectReplyMsg::SIZE], pkt_len))
    }

    /// Called when TcpEvent::Connected fires — send the deferred NetConnectReply.
    pub fn notify_connected(&mut self, socket_id: u32) -> Option<(u64, [u8; NetConnectReplyMsg::SIZE])> {
        for pc in self.pending_connects.iter_mut() {
            if pc.in_use && pc.socket_id == socket_id {
                let reply_port = pc.reply_port;
                pc.in_use = false;
                let reply = NetConnectReplyMsg { socket_id, error: 0 };
                return Some((reply_port, reply.to_bytes()));
            }
        }
        None
    }

    /// Handle NetSend: send data over TCP socket.
    pub fn handle_net_send(
        &mut self,
        payload: &[u8],
        src_ip: u32,
        tcp: &mut TcpManager,
        out_pkt: &mut [u8],
    ) -> Option<(u64, [u8; NetSendReplyMsg::SIZE], usize)> {
        let msg = NetSendMsg::from_bytes(payload)?;
        let data_len = msg.len as usize;
        let data = &msg.data[..data_len.min(1024)];
        let result = tcp.send_data(msg.socket_id, src_ip, data, out_pkt);
        let (sent, error, pkt_len) = match result {
            Ok(len) => (data_len as u32, 0u32, len),
            Err(e) => (0, e, 0),
        };
        let reply = NetSendReplyMsg { socket_id: msg.socket_id, sent, error };
        Some((msg.reply_port, reply.to_bytes(), pkt_len))
    }

    /// Handle NetRecv: if data available return immediately, else store as pending.
    pub fn handle_net_recv(
        &mut self,
        payload: &[u8],
        tcp: &mut TcpManager,
    ) -> Option<(u64, NetRecvReplyMsg)> {
        let msg = NetRecvMsg::from_bytes(payload)?;
        let mut buf = [0u8; 1024];
        let max = msg.max_len as usize;
        let result = tcp.recv_data(msg.socket_id, &mut buf[..max.min(1024)]);
        match result {
            Ok(n) if n > 0 => {
                let mut data = [0u8; 1024];
                data[..n].copy_from_slice(&buf[..n]);
                let reply = NetRecvReplyMsg {
                    socket_id: msg.socket_id,
                    len: n as u32,
                    error: 0,
                    data,
                };
                Some((msg.reply_port, reply))
            }
            _ => {
                // Store as pending recv
                for pr in self.pending_recvs.iter_mut() {
                    if !pr.in_use {
                        pr.socket_id = msg.socket_id;
                        pr.reply_port = msg.reply_port;
                        pr.max_len = msg.max_len;
                        pr.in_use = true;
                        break;
                    }
                }
                None
            }
        }
    }

    /// Handle NetClose: send FIN, return reply.
    pub fn handle_net_close(
        &mut self,
        payload: &[u8],
        src_ip: u32,
        tcp: &mut TcpManager,
        out_pkt: &mut [u8],
    ) -> Option<(u64, [u8; NetCloseReplyMsg::SIZE], usize)> {
        let msg = NetCloseMsg::from_bytes(payload)?;
        let pkt_len = tcp.close(msg.socket_id, src_ip, out_pkt).unwrap_or(0);
        let reply = NetCloseReplyMsg { socket_id: msg.socket_id, error: 0 };
        Some((msg.reply_port, reply.to_bytes(), pkt_len))
    }

    /// Handle NetResolve: check DNS cache; if miss, store pending resolve and return None.
    /// Returns Some only on cache hit.
    pub fn handle_net_resolve(
        &mut self,
        payload: &[u8],
        dns: &mut DnsCache,
        now_ticks: u64,
    ) -> Option<(u64, [u8; NetResolveReplyMsg::SIZE])> {
        let msg = NetResolveMsg::from_bytes(payload)?;
        let name_len = (msg.name_len as usize).min(256);
        let name_bytes = &msg.name[..name_len];
        let name = core::str::from_utf8(name_bytes).unwrap_or("");

        if let Some(ip) = dns.lookup(name, now_ticks) {
            // Cache hit — reply immediately
            let reply = NetResolveReplyMsg { ip, error: 0 };
            return Some((msg.reply_port, reply.to_bytes()));
        }

        // Cache miss — store pending resolve, caller will send DNS query
        for pr in self.pending_resolves.iter_mut() {
            if !pr.in_use {
                pr.name[..name_len].copy_from_slice(name_bytes);
                pr.name_len = name_len;
                pr.reply_port = msg.reply_port;
                pr.in_use = true;
                break;
            }
        }

        None // No reply yet; will be sent when DNS response arrives
    }

    /// Called when a DNS response arrives — fulfill any pending resolve for this name.
    pub fn notify_dns_resolved(
        &mut self,
        name: &str,
        ip: u32,
    ) -> Option<(u64, [u8; NetResolveReplyMsg::SIZE])> {
        let name_bytes = name.as_bytes();
        for pr in self.pending_resolves.iter_mut() {
            if pr.in_use && &pr.name[..pr.name_len] == name_bytes {
                let reply_port = pr.reply_port;
                pr.in_use = false;
                let reply = NetResolveReplyMsg { ip, error: 0 };
                return Some((reply_port, reply.to_bytes()));
            }
        }
        None
    }

    /// Check if there's a pending recv for this socket; if so, fulfill it.
    pub fn notify_data_received(
        &mut self,
        socket_id: u32,
        tcp: &mut TcpManager,
    ) -> Option<(u64, NetRecvReplyMsg)> {
        for pr in self.pending_recvs.iter_mut() {
            if pr.in_use && pr.socket_id == socket_id {
                let mut buf = [0u8; 1024];
                let max = pr.max_len as usize;
                let result = tcp.recv_data(socket_id, &mut buf[..max.min(1024)]);
                if let Ok(n) = result {
                    if n > 0 {
                        let mut data = [0u8; 1024];
                        data[..n].copy_from_slice(&buf[..n]);
                        let reply_port = pr.reply_port;
                        pr.in_use = false;
                        let reply = NetRecvReplyMsg {
                            socket_id,
                            len: n as u32,
                            error: 0,
                            data,
                        };
                        return Some((reply_port, reply));
                    }
                }
            }
        }
        None
    }
}
