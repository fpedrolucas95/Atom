// use crate::ip::ipv4_checksum; // used in tests only

pub const TCP_FIN: u8 = 0x01;
pub const TCP_SYN: u8 = 0x02;
pub const TCP_RST: u8 = 0x04;
pub const TCP_PSH: u8 = 0x08;
pub const TCP_ACK: u8 = 0x10;

pub const TCP_HEADER_LEN: usize = 20;
pub const TCP_RECV_BUF_LEN: usize = 64 * 1024;
pub const TCP_MAX_SOCKETS: usize = 4;

/// TCP connection state machine states
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TcpState {
    Closed,
    SynSent,
    Established,
    FinWait1,
    FinWait2,
    TimeWait,
    CloseWait,
    LastAck,
}

/// TCP header (20 bytes, no options)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TcpHeader {
    pub src_port: u16,
    pub dst_port: u16,
    pub seq: u32,
    pub ack_num: u32,
    pub data_offset: u8, // high nibble = offset in 32-bit words (5 for no options)
    pub flags: u8,
    pub window: u16,
    pub checksum: u16,
    pub urgent: u16,
}

/// A TCP socket
pub struct TcpSocket {
    pub id: u32,
    pub state: TcpState,
    pub local_port: u16,
    pub remote_ip: u32,
    pub remote_port: u16,
    pub seq: u32,
    pub ack: u32,
    pub send_buf: [u8; 4096],
    pub send_len: usize,
    pub recv_buf: [u8; TCP_RECV_BUF_LEN],
    pub recv_len: usize,
    pub recv_ready: bool,
    pub owner_port: u64,
    pub reply_seq: u32,
    pub rto_deadline: u64,
    pub retries: u8,
    pub in_use: bool,
}

impl TcpSocket {
    const fn new() -> Self {
        Self {
            id: 0,
            state: TcpState::Closed,
            local_port: 0,
            remote_ip: 0,
            remote_port: 0,
            seq: 0,
            ack: 0,
            send_buf: [0u8; 4096],
            send_len: 0,
            recv_buf: [0u8; TCP_RECV_BUF_LEN],
            recv_len: 0,
            recv_ready: false,
            owner_port: 0,
            reply_seq: 0,
            rto_deadline: 0,
            retries: 0,
            in_use: false,
        }
    }
}

/// Events emitted by TcpManager for the main loop to act on
pub enum TcpEvent {
    Connected(u32),      // socket_id — SYN-ACK received, ACK sent
    DataReceived(u32),   // socket_id — data in recv_buf
    AckOnly(u32),        // socket_id — ACK emitted without new data for userspace
    Closed(u32),         // socket_id — FIN received
    ConnectTimeout(u32), // socket_id — SYN timeout
}

/// Manages TCP sockets.
pub struct TcpManager {
    pub sockets: [TcpSocket; TCP_MAX_SOCKETS],
    next_local_port: u16,
    next_id: u32,
}

fn recv_window(recv_len: usize) -> u16 {
    TCP_RECV_BUF_LEN
        .saturating_sub(recv_len)
        .min(u16::MAX as usize) as u16
}

/// Compute TCP checksum using pseudo-header
pub fn tcp_checksum(src_ip: u32, dst_ip: u32, tcp_segment: &[u8]) -> u16 {
    let tcp_len = tcp_segment.len() as u16;
    let mut sum: u32 = 0;
    // Pseudo-header: src_ip, dst_ip, 0x00, proto=6, tcp_length
    sum += (src_ip >> 16) as u32;
    sum += (src_ip & 0xFFFF) as u32;
    sum += (dst_ip >> 16) as u32;
    sum += (dst_ip & 0xFFFF) as u32;
    sum += 6u32; // protocol TCP
    sum += tcp_len as u32;
    // TCP segment
    let mut i = 0;
    while i + 1 < tcp_segment.len() {
        let word = u16::from_be_bytes([tcp_segment[i], tcp_segment[i + 1]]) as u32;
        sum += word;
        i += 2;
    }
    if i < tcp_segment.len() {
        sum += (tcp_segment[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build a TCP segment into `out`. Returns total length.
pub fn build_tcp_segment(
    src_ip: u32,
    dst_ip: u32,
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    payload: &[u8],
    out: &mut [u8],
) -> usize {
    let total = TCP_HEADER_LEN + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..2].copy_from_slice(&src_port.to_be_bytes());
    out[2..4].copy_from_slice(&dst_port.to_be_bytes());
    out[4..8].copy_from_slice(&seq.to_be_bytes());
    out[8..12].copy_from_slice(&ack.to_be_bytes());
    out[12] = 0x50; // data_offset=5 (20 bytes), reserved=0
    out[13] = flags;
    out[14..16].copy_from_slice(&window.to_be_bytes());
    out[16..18].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out[18..20].copy_from_slice(&0u16.to_be_bytes()); // urgent
    out[TCP_HEADER_LEN..total].copy_from_slice(payload);
    // Compute checksum
    let csum = tcp_checksum(src_ip, dst_ip, &out[0..total]);
    out[16..18].copy_from_slice(&csum.to_be_bytes());
    total
}

/// Parse a TCP segment. Returns (header, payload) or None if data < 20 bytes.
pub fn parse_tcp(data: &[u8]) -> Option<(TcpHeader, &[u8])> {
    if data.len() < TCP_HEADER_LEN {
        return None;
    }
    let data_offset = data[12];
    let header_len = ((data_offset >> 4) as usize) * 4;
    if header_len < TCP_HEADER_LEN || header_len > data.len() {
        return None;
    }
    let hdr = TcpHeader {
        src_port: u16::from_be_bytes([data[0], data[1]]),
        dst_port: u16::from_be_bytes([data[2], data[3]]),
        seq: u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
        ack_num: u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        data_offset,
        flags: data[13],
        window: u16::from_be_bytes([data[14], data[15]]),
        checksum: u16::from_be_bytes([data[16], data[17]]),
        urgent: u16::from_be_bytes([data[18], data[19]]),
    };
    Some((hdr, &data[header_len..]))
}

impl TcpManager {
    pub fn new() -> Self {
        Self {
            sockets: [
                TcpSocket::new(),
                TcpSocket::new(),
                TcpSocket::new(),
                TcpSocket::new(),
            ],
            next_local_port: 49152,
            next_id: 1,
        }
    }

    /// Allocate a new socket for the given owner IPC port. Returns socket_id.
    pub fn alloc_socket(&mut self, owner_port: u64) -> Option<u32> {
        for sock in self.sockets.iter_mut() {
            if !sock.in_use {
                let id = self.next_id;
                self.next_id += 1;
                sock.id = id;
                sock.state = TcpState::Closed;
                sock.owner_port = owner_port;
                sock.in_use = true;
                sock.send_len = 0;
                sock.recv_len = 0;
                sock.recv_ready = false;
                sock.retries = 0;
                return Some(id);
            }
        }
        None
    }

    /// Initiate a TCP connection. Builds SYN packet, sets state=SynSent.
    /// Returns packet length or None on error.
    pub fn connect(
        &mut self,
        socket_id: u32,
        remote_ip: u32,
        remote_port: u16,
        now_ticks: u64,
        out_buf: &mut [u8],
    ) -> Option<usize> {
        let idx = self.find_by_id_mut(socket_id)?;
        let local_port = self.next_local_port;
        self.next_local_port = if self.next_local_port >= 65535 {
            49152
        } else {
            self.next_local_port + 1
        };

        let sock = &mut self.sockets[idx];
        sock.local_port = local_port;
        sock.remote_ip = remote_ip;
        sock.remote_port = remote_port;
        sock.seq = now_ticks as u32; // ISN based on ticks
        sock.ack = 0;
        sock.state = TcpState::SynSent;
        sock.rto_deadline = now_ticks + 500; // 5 seconds at 100Hz
        sock.retries = 0;

        let own_ip = 0u32; // Will be filled by caller from config
        let len = build_tcp_segment(
            own_ip,
            remote_ip,
            local_port,
            remote_port,
            sock.seq,
            0,
            TCP_SYN,
            65535,
            &[],
            out_buf,
        );
        sock.seq = sock.seq.wrapping_add(1); // SYN consumes one seq
        Some(len)
    }

    /// Connect with explicit source IP (used by main.rs)
    pub fn connect_with_ip(
        &mut self,
        socket_id: u32,
        src_ip: u32,
        remote_ip: u32,
        remote_port: u16,
        now_ticks: u64,
        out_buf: &mut [u8],
    ) -> Option<usize> {
        let idx = self.find_by_id_mut(socket_id)?;

        let local_port = self.next_local_port;
        self.next_local_port = if self.next_local_port >= 65535 {
            49152
        } else {
            self.next_local_port + 1
        };

        let sock = &mut self.sockets[idx];
        sock.local_port = local_port;
        sock.remote_ip = remote_ip;
        sock.remote_port = remote_port;
        sock.seq = now_ticks as u32;
        sock.ack = 0;
        sock.state = TcpState::SynSent;
        sock.rto_deadline = now_ticks + 500;
        sock.retries = 0;

        let isn = sock.seq;
        let len = build_tcp_segment(
            src_ip,
            remote_ip,
            local_port,
            remote_port,
            isn,
            0,
            TCP_SYN,
            65535,
            &[],
            out_buf,
        );
        sock.seq = isn.wrapping_add(1);
        Some(len)
    }

    /// Dispatch a received TCP segment to the correct socket.
    pub fn on_rx(
        &mut self,
        src_ip: u32,
        dst_ip: u32,
        tcp_hdr: &TcpHeader,
        payload: &[u8],
        now_ticks: u64,
        out_buf: &mut [u8],
    ) -> Option<TcpEvent> {
        let idx = self.find_socket_mut(src_ip, tcp_hdr.src_port, tcp_hdr.dst_port)?;
        let sock = &mut self.sockets[idx];

        match sock.state {
            TcpState::SynSent => {
                // Expect SYN-ACK
                if tcp_hdr.flags & (TCP_SYN | TCP_ACK) == (TCP_SYN | TCP_ACK) {
                    sock.ack = tcp_hdr.seq.wrapping_add(1);
                    sock.state = TcpState::Established;
                    sock.rto_deadline = 0;
                    // Send ACK
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    return Some(TcpEvent::Connected(socket_id));
                }
                // RST
                if tcp_hdr.flags & TCP_RST != 0 {
                    let socket_id = sock.id;
                    sock.state = TcpState::Closed;
                    sock.in_use = false;
                    return Some(TcpEvent::Closed(socket_id));
                }
            }
            TcpState::Established => {
                // Update ACK
                if tcp_hdr.flags & TCP_ACK != 0 {
                    // Acknowledged
                }
                // Data received
                if !payload.is_empty() {
                    if tcp_hdr.seq != sock.ack {
                        let local_port = sock.local_port;
                        let remote_port = sock.remote_port;
                        let seq = sock.seq;
                        let ack = sock.ack;
                        let socket_id = sock.id;
                        build_tcp_segment(
                            dst_ip,
                            src_ip,
                            local_port,
                            remote_port,
                            seq,
                            ack,
                            TCP_ACK,
                            recv_window(sock.recv_len),
                            &[],
                            out_buf,
                        );
                        return Some(TcpEvent::AckOnly(socket_id));
                    }

                    let copy_len = payload
                        .len()
                        .min(TCP_RECV_BUF_LEN.saturating_sub(sock.recv_len));
                    if copy_len > 0 {
                        let start = sock.recv_len;
                        sock.recv_buf[start..start + copy_len]
                            .copy_from_slice(&payload[..copy_len]);
                        sock.recv_len += copy_len;
                        sock.recv_ready = true;
                    }
                    sock.ack = sock.ack.wrapping_add(copy_len as u32);

                    if copy_len == payload.len() && tcp_hdr.flags & TCP_FIN != 0 {
                        sock.ack = sock.ack.wrapping_add(1);
                        sock.state = TcpState::CloseWait;
                    }

                    // Send ACK
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    if copy_len > 0 {
                        return Some(TcpEvent::DataReceived(socket_id));
                    }
                    return Some(TcpEvent::AckOnly(socket_id));
                }
                // FIN received
                if tcp_hdr.flags & TCP_FIN != 0 {
                    if tcp_hdr.seq != sock.ack {
                        let local_port = sock.local_port;
                        let remote_port = sock.remote_port;
                        let seq = sock.seq;
                        let ack = sock.ack;
                        let socket_id = sock.id;
                        build_tcp_segment(
                            dst_ip,
                            src_ip,
                            local_port,
                            remote_port,
                            seq,
                            ack,
                            TCP_ACK,
                            recv_window(sock.recv_len),
                            &[],
                            out_buf,
                        );
                        return Some(TcpEvent::AckOnly(socket_id));
                    }

                    sock.ack = sock.ack.wrapping_add(1);
                    sock.state = TcpState::CloseWait;
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    return Some(TcpEvent::Closed(socket_id));
                }
            }
            TcpState::CloseWait => {
                if !payload.is_empty() || tcp_hdr.flags & TCP_FIN != 0 {
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    return Some(TcpEvent::AckOnly(socket_id));
                }
            }
            TcpState::FinWait1 => {
                if tcp_hdr.flags & TCP_ACK != 0 {
                    sock.state = TcpState::FinWait2;
                }
                if tcp_hdr.flags & TCP_FIN != 0 {
                    sock.ack = sock.ack.wrapping_add(1);
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    sock.state = TcpState::TimeWait;
                    sock.rto_deadline = now_ticks + 400; // 4s TimeWait
                    return Some(TcpEvent::Closed(socket_id));
                }
            }
            TcpState::FinWait2 => {
                if tcp_hdr.flags & TCP_FIN != 0 {
                    sock.ack = sock.ack.wrapping_add(1);
                    let local_port = sock.local_port;
                    let remote_port = sock.remote_port;
                    let seq = sock.seq;
                    let ack = sock.ack;
                    let socket_id = sock.id;
                    build_tcp_segment(
                        dst_ip,
                        src_ip,
                        local_port,
                        remote_port,
                        seq,
                        ack,
                        TCP_ACK,
                        recv_window(sock.recv_len),
                        &[],
                        out_buf,
                    );
                    sock.state = TcpState::TimeWait;
                    sock.rto_deadline = now_ticks + 400;
                    return Some(TcpEvent::Closed(socket_id));
                }
            }
            TcpState::LastAck => {
                if tcp_hdr.flags & TCP_ACK != 0 {
                    let socket_id = sock.id;
                    sock.state = TcpState::Closed;
                    sock.in_use = false;
                    return Some(TcpEvent::Closed(socket_id));
                }
            }
            _ => {}
        }
        None
    }

    /// Send data on an established socket. Returns packet length or error code.
    pub fn send_data(
        &mut self,
        socket_id: u32,
        src_ip: u32,
        data: &[u8],
        out_buf: &mut [u8],
    ) -> Result<usize, u32> {
        let idx = self.find_by_id_mut(socket_id).ok_or(1u32)?;
        let sock = &mut self.sockets[idx];
        if sock.state != TcpState::Established {
            return Err(2);
        }
        let send_len = data.len().min(1024); // cap at 1024 for MVP
        let remote_ip = sock.remote_ip;
        let remote_port = sock.remote_port;
        let local_port = sock.local_port;
        let seq = sock.seq;
        let ack = sock.ack;
        let len = build_tcp_segment(
            src_ip,
            remote_ip,
            local_port,
            remote_port,
            seq,
            ack,
            TCP_PSH | TCP_ACK,
            recv_window(sock.recv_len),
            &data[..send_len],
            out_buf,
        );
        sock.seq = sock.seq.wrapping_add(send_len as u32);
        Ok(len)
    }

    /// Read received data from a socket's recv buffer.
    pub fn recv_data(&mut self, socket_id: u32, buf: &mut [u8]) -> Result<usize, u32> {
        let idx = self.find_by_id_mut(socket_id).ok_or(1u32)?;
        let sock = &mut self.sockets[idx];
        if sock.recv_len == 0 {
            return Ok(0);
        }
        let copy_len = sock.recv_len.min(buf.len());
        buf[..copy_len].copy_from_slice(&sock.recv_buf[..copy_len]);
        // Shift remaining data
        let remaining = sock.recv_len - copy_len;
        if remaining > 0 {
            sock.recv_buf.copy_within(copy_len..sock.recv_len, 0);
        }
        sock.recv_len = remaining;
        if remaining == 0 {
            sock.recv_ready = false;
        }
        Ok(copy_len)
    }

    pub fn recv_eof(&self, socket_id: u32) -> bool {
        self.get_socket(socket_id)
            .map(|sock| sock.recv_len == 0 && sock.state == TcpState::CloseWait)
            .unwrap_or(false)
    }

    /// Send FIN to close a socket. Returns packet length or None.
    pub fn close(&mut self, socket_id: u32, src_ip: u32, out_buf: &mut [u8]) -> Option<usize> {
        let idx = self.find_by_id_mut(socket_id)?;
        let sock = &mut self.sockets[idx];
        if sock.state != TcpState::Established && sock.state != TcpState::CloseWait {
            sock.state = TcpState::Closed;
            sock.in_use = false;
            return None;
        }
        let passive_close = sock.state == TcpState::CloseWait;
        let remote_ip = sock.remote_ip;
        let remote_port = sock.remote_port;
        let local_port = sock.local_port;
        let seq = sock.seq;
        let ack = sock.ack;
        let len = build_tcp_segment(
            src_ip,
            remote_ip,
            local_port,
            remote_port,
            seq,
            ack,
            TCP_FIN | TCP_ACK,
            recv_window(sock.recv_len),
            &[],
            out_buf,
        );
        sock.seq = sock.seq.wrapping_add(1);
        sock.state = if passive_close {
            TcpState::LastAck
        } else {
            TcpState::FinWait1
        };
        Some(len)
    }

    /// Timer tick: retransmit SYN if RTO expired, clean up TimeWait sockets.
    /// Returns a packet to send if retransmitting, or None.
    pub fn tick(
        &mut self,
        src_ip: u32,
        now_ticks: u64,
        out_buf: &mut [u8],
    ) -> Option<(usize, TcpEvent)> {
        for i in 0..8 {
            let sock = &self.sockets[i];
            if !sock.in_use {
                continue;
            }

            match sock.state {
                TcpState::SynSent => {
                    if sock.rto_deadline > 0 && now_ticks >= sock.rto_deadline {
                        if sock.retries >= 3 {
                            let socket_id = sock.id;
                            self.sockets[i].state = TcpState::Closed;
                            self.sockets[i].in_use = false;
                            return Some((0, TcpEvent::ConnectTimeout(socket_id)));
                        }
                        // Retransmit SYN
                        let sock = &mut self.sockets[i];
                        let isn = sock.seq.wrapping_sub(1); // original ISN
                        let remote_ip = sock.remote_ip;
                        let remote_port = sock.remote_port;
                        let local_port = sock.local_port;
                        let len = build_tcp_segment(
                            src_ip,
                            remote_ip,
                            local_port,
                            remote_port,
                            isn,
                            0,
                            TCP_SYN,
                            recv_window(sock.recv_len),
                            &[],
                            out_buf,
                        );
                        sock.retries += 1;
                        sock.rto_deadline = now_ticks + 500;
                        let socket_id = sock.id;
                        return Some((len, TcpEvent::ConnectTimeout(socket_id)));
                    }
                }
                TcpState::TimeWait => {
                    if sock.rto_deadline > 0 && now_ticks >= sock.rto_deadline {
                        self.sockets[i].state = TcpState::Closed;
                        self.sockets[i].in_use = false;
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn find_socket_mut(
        &mut self,
        remote_ip: u32,
        remote_port: u16,
        local_port: u16,
    ) -> Option<usize> {
        for (i, sock) in self.sockets.iter().enumerate() {
            if sock.in_use
                && sock.remote_ip == remote_ip
                && sock.remote_port == remote_port
                && sock.local_port == local_port
            {
                return Some(i);
            }
        }
        None
    }

    fn find_by_id_mut(&mut self, id: u32) -> Option<usize> {
        for (i, sock) in self.sockets.iter().enumerate() {
            if sock.in_use && sock.id == id {
                return Some(i);
            }
        }
        None
    }

    /// Get socket by id (immutable)
    pub fn get_socket(&self, id: u32) -> Option<&TcpSocket> {
        for sock in self.sockets.iter() {
            if sock.in_use && sock.id == id {
                return Some(sock);
            }
        }
        None
    }
}
