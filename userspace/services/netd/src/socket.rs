use alloc::vec::Vec;
use alloc::format;
use atom_syscall::debug::log;
use crate::tcp::TcpManager;
use crate::dns::DnsCache;
use libipc::messages::{
    NetSocketMsg, NetSocketReplyMsg,
    NetConnectMsg, NetConnectReplyMsg,
    NetSendMsg, NetSendReplyMsg,
    NetRecvMsg, NetRecvReplyMsg,
    NetCloseMsg, NetCloseReplyMsg,
    NetResolveMsg, NetResolveReplyMsg,
    NetIcmpEchoRequestMsg, NetIcmpEchoReplyMsg,
    NetIpAddr,
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
    pub transaction_id: u16,
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
            transaction_id: 0,
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

/// A pending ICMP echo request: waiting for Echo Reply
pub struct PendingIcmp {
    pub reply_port: u64,
    pub dest_ip: u32,
    pub identifier: u16,
    pub sequence: u16,
    pub start_tick: u64,
    pub timeout_ticks: u64,
    pub in_use: bool,
}

impl PendingIcmp {
    const fn new() -> Self {
        Self {
            reply_port: 0,
            dest_ip: 0,
            identifier: 0,
            sequence: 0,
            start_tick: 0,
            timeout_ticks: 0,
            in_use: false,
        }
    }
}

pub struct SocketManager {
    pub pending_recvs: [PendingRecv; 8],
    pub pending_resolves: [PendingResolve; 4],
    pub pending_connects: [PendingConnect; 8],
    next_dns_id: u16,
    pub pending_icmps: [PendingIcmp; 8],
    next_icmp_id: u16,
}

fn log_netd_icmp_registered(id: u16, seq: u16, dest: u32, client: u64) {
    let dest_b = dest.to_be_bytes();
    log(&format!(
        "[netd] ICMP request registered: id=0x{:04x} seq={} dest={}.{}.{}.{} client=Port({})",
        id, seq, dest_b[0], dest_b[1], dest_b[2], dest_b[3], client
    ));
}

fn log_netd_icmp_match(id: u16, seq: u16, src: u32) {
    let src_b = src.to_be_bytes();
    log(&format!(
        "[netd] ICMP match found: id=0x{:04x} seq={} src={}.{}.{}.{} -> delivering to client",
        id, seq, src_b[0], src_b[1], src_b[2], src_b[3]
    ));
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
            pending_icmps: [
                PendingIcmp::new(), PendingIcmp::new(), PendingIcmp::new(), PendingIcmp::new(),
                PendingIcmp::new(), PendingIcmp::new(), PendingIcmp::new(), PendingIcmp::new(),
            ],
            next_icmp_id: 1000,
            next_dns_id: 0xABCD,
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
    ) -> Option<Result<(u64, [u8; NetResolveReplyMsg::SIZE]), u16>> {
        let msg = NetResolveMsg::from_bytes(payload)?;
        let name_len = (msg.name_len as usize).min(256);
        let name_bytes = &msg.name[..name_len];
        let name = core::str::from_utf8(name_bytes).unwrap_or("");

        if let Some(ip) = dns.lookup(name, now_ticks) {
            // Cache hit — reply immediately
            let reply = NetResolveReplyMsg { ip, error: 0 };
            return Some(Ok((msg.reply_port, reply.to_bytes())));
        }

        // Cache miss — store pending resolve, caller will send DNS query
        for pr in self.pending_resolves.iter_mut() {
            if !pr.in_use {
                let tid = self.next_dns_id;
                self.next_dns_id = self.next_dns_id.wrapping_add(1);

                pr.transaction_id = tid;
                pr.name[..name_len].copy_from_slice(name_bytes);
                pr.name_len = name_len;
                pr.reply_port = msg.reply_port;
                pr.in_use = true;

                // Return transaction ID to caller so they can build the packet
                return Some(Err(tid));
            }
        }

        None // No free slots
    }

    /// Called when a DNS response arrives — fulfill any pending resolve for this ID.
    pub fn notify_dns_resolved(
        &mut self,
        transaction_id: u16,
        ip: u32,
    ) -> Option<(u64, [u8; NetResolveReplyMsg::SIZE])> {
        for pr in self.pending_resolves.iter_mut() {
            if pr.in_use && pr.transaction_id == transaction_id {
                let reply_port = pr.reply_port;
                pr.in_use = false;
                let reply = NetResolveReplyMsg { ip, error: 0 };
                return Some((reply_port, reply.to_bytes()));
            }
        }
        None
    }

    /// Handle NetIcmpEchoRequest: register pending and return ICMP info for sending.
    pub fn handle_net_icmp_echo(
        &mut self,
        payload: &[u8],
        now_ticks: u64,
    ) -> Option<(u32, u16, u16, &[u8])> {
        let msg = NetIcmpEchoRequestMsg::from_bytes(payload)?;

        // IPv4 only for now
        if msg.dest_ip.family != 4 {
            return None;
        }
        let dest_ip = u32::from_be_bytes([msg.dest_ip.data[0], msg.dest_ip.data[1], msg.dest_ip.data[2], msg.dest_ip.data[3]]);

        // Find free slot
        for pi in self.pending_icmps.iter_mut() {
            if !pi.in_use {
                let id = self.next_icmp_id;
                self.next_icmp_id = self.next_icmp_id.wrapping_add(1);
                if self.next_icmp_id < 1000 { self.next_icmp_id = 1000; }

                pi.reply_port = msg.reply_port;
                pi.dest_ip = dest_ip;
                pi.identifier = id;
                pi.sequence = msg.sequence;
                pi.start_tick = now_ticks;

                log_netd_icmp_registered(id, msg.sequence, dest_ip, msg.reply_port);
                pi.timeout_ticks = msg.timeout_ms as u64 / 10;
                pi.in_use = true;

                // We need to return a reference to the payload, but msg is local.
                // Actually, the payload is part of the msg which is part of payload buffer.
                // We can't return it easily if we want to return a slice.
                // Let's change the signature to return the full msg or just the needed parts.
                // For now, let's just return what we need.
                return Some((dest_ip, pi.identifier, pi.sequence, &[]));
            }
        }
        None
    }

    /// Called when an ICMP Echo Reply arrives — fulfill any pending ICMP request.
    pub fn notify_icmp_reply(
        &mut self,
        src_ip: u32,
        id: u16,
        seq: u16,
        ttl: u8,
        now_ticks: u64,
        icmp_payload: &[u8],
    ) -> Option<(u64, Vec<u8>)> {
        for pi in self.pending_icmps.iter_mut() {
            if pi.in_use && pi.identifier == id && pi.sequence == seq && pi.dest_ip == src_ip {
                let reply_port = pi.reply_port;
                pi.in_use = false;

                log_netd_icmp_match(id, seq, src_ip);

                let rtt_ticks = now_ticks.saturating_sub(pi.start_tick);
                let rtt_ms = (rtt_ticks * 10) as u32;

                let mut reply = NetIcmpEchoReplyMsg {
                    src_ip: NetIpAddr::ipv4(src_ip.to_be_bytes()),
                    sequence: seq,
                    ttl,
                    rtt_ms,
                    error: 0,
                    payload_len: icmp_payload.len() as u32,
                    payload: [0u8; 64],
                };
                let copy_len = icmp_payload.len().min(64);
                reply.payload[..copy_len].copy_from_slice(&icmp_payload[..copy_len]);

                return Some((reply_port, reply.to_bytes()));
            }
        }
        None
    }

    /// Periodic cleanup of timed out ICMP requests.
    pub fn check_icmp_timeouts(&mut self, now_ticks: u64) -> alloc::vec::Vec<(u64, Vec<u8>)> {
        let mut timed_out = alloc::vec::Vec::new();
        for pi in self.pending_icmps.iter_mut() {
            if pi.in_use && now_ticks.saturating_sub(pi.start_tick) >= pi.timeout_ticks {
                pi.in_use = false;
                let reply = NetIcmpEchoReplyMsg {
                    src_ip: NetIpAddr::ipv4(pi.dest_ip.to_be_bytes()),
                    sequence: pi.sequence,
                    ttl: 0,
                    rtt_ms: 0,
                    error: 1, // Timeout
                    payload_len: 0,
                    payload: [0u8; 64],
                };
                timed_out.push((pi.reply_port, reply.to_bytes()));
            }
        }
        timed_out
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
