//! Network Daemon (netd) — Phase C rewrite
//!
//! Implements a full TCP/IP stack in userspace:
//! Ethernet → ARP → IPv4 → ICMP/UDP/TCP → DNS → Socket IPC API

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use atom_syscall::debug::log;
use libring::{RingHeader, RingEntry, RingProducer, RingConsumer};
use libipc::protocol::{register_service, lookup_service, send_message, try_recv_message, get_payload};
use libipc::messages::{
    MessageType, NetAssignRingsMsg, NetDriverReadyMsg,
};

mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 512 * 1024;

    #[repr(align(4096))]
    struct Heap {
        data: UnsafeCell<[u8; HEAP_SIZE]>,
        next: UnsafeCell<usize>,
    }
    unsafe impl Sync for Heap {}

    static HEAP: Heap = Heap {
        data: UnsafeCell::new([0; HEAP_SIZE]),
        next: UnsafeCell::new(0),
    };

    pub struct BumpAllocator;
    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let next = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;
            let current = *next;
            let aligned = (current + layout.align() - 1) & !(layout.align() - 1);
            if aligned + layout.size() > HEAP_SIZE { return null_mut(); }
            *next = aligned + layout.size();
            heap_start.add(aligned)
        }
        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

mod config;
mod buf;
mod eth;
mod arp;
mod ip;
mod icmp;
mod udp;
mod tcp;
mod dns;
mod socket;

use config::NetConfig;
use buf::BufPool;
use arp::{ArpTable, parse_arp, build_arp_reply, build_arp_request, ARP_REQUEST, ARP_REPLY};
use eth::{build_eth_frame, parse_eth_frame};
use ip::{build_ipv4, parse_ipv4, next_hop, IP_PROTO_ICMP, IP_PROTO_TCP, IP_PROTO_UDP};
use icmp::{build_icmp_echo_request, parse_icmp, ICMP_ECHO_REPLY};
use udp::{build_udp, build_udp_with_checksum, parse_udp};
use tcp::{TcpManager, TcpEvent, parse_tcp};
use dns::{DnsCache, build_dns_query, parse_dns_response};
use socket::SocketManager;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn push_to_tx(tx_producer: &mut RingProducer, data: &[u8]) {
    if data.is_empty() || !tx_producer.can_push() {
        return;
    }
    let mut entry = RingEntry {
        len: data.len() as u16,
        flags: 0,
        offset: 0,
        data: [0u8; 1528],
    };
    let copy_len = data.len().min(1528);
    entry.data[..copy_len].copy_from_slice(&data[..copy_len]);
    tx_producer.push(entry).ok();
}

fn main() -> ! {
    log("netd: starting");

    // ── 1. Create shared memory rings ────────────────────────────────────────
    let ring_capacity = 32;
    let header_size = core::mem::size_of::<RingHeader>();
    let entries_size = ring_capacity * core::mem::size_of::<RingEntry>();
    let region_size = (header_size * 2 + entries_size * 2 + 4095) & !4095;

    let region_id = match atom_syscall::shared_mem::create_region(region_size) {
        Ok(id) => id,
        Err(_) => {
            log("netd: FAILED to create shared region");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    let va = match atom_syscall::shared_mem::map_region(
        region_id, 0,
        atom_syscall::shared_mem::RegionFlags::READ | atom_syscall::shared_mem::RegionFlags::WRITE,
    ) {
        Ok(v) => v,
        Err(_) => {
            log("netd: FAILED to map shared region");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    let tx_header_ptr = va as *mut RingHeader;
    let rx_header_ptr = (va + header_size as u64) as *mut RingHeader;
    let tx_entries_ptr = (va + (header_size * 2) as u64) as *mut RingEntry;
    let rx_entries_ptr = (va + (header_size * 2 + entries_size) as u64) as *mut RingEntry;

    unsafe {
        core::ptr::write_bytes(va as *mut u8, 0, region_size);
        (*tx_header_ptr).capacity = ring_capacity;
        (*rx_header_ptr).capacity = ring_capacity;
    }

    let mut tx_producer = RingProducer::new(
        unsafe { &*tx_header_ptr },
        unsafe { core::slice::from_raw_parts_mut(tx_entries_ptr, ring_capacity) },
    );
    let mut rx_consumer = RingConsumer::new(
        unsafe { &*rx_header_ptr },
        unsafe { core::slice::from_raw_parts(rx_entries_ptr, ring_capacity) },
    );

    // ── 2. Register IPC port ──────────────────────────────────────────────────
    let port = match atom_syscall::ipc::create_port() {
        Ok(p) => p,
        Err(_) => {
            log("netd: FAILED to create port");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };
    if register_service("netd", port).is_err() {
        log("netd: FAILED to register with namesvc");
    }

    log("netd: registered, waiting for nic_driver");

    // ── 3. Wait for nic_driver and send ring assignment ───────────────────────
    let mut driver_port = 0u64;
    while driver_port == 0 {
        if let Ok(p) = lookup_service("nic_driver") {
            driver_port = p;
        } else {
            atom_syscall::thread::sleep_ms(100);
        }
    }

    let assign_msg = NetAssignRingsMsg {
        region_id,
        ring_capacity: ring_capacity as u32,
    };
    send_message(driver_port, MessageType::NetAssignRings, &assign_msg.to_bytes()).ok();
    log("netd: rings assigned to nic_driver");

    // ── 4. Wait for NetDriverReady with MAC ───────────────────────────────────
    let mut ipc_buf = [0u8; 1600];
    let mut cfg = NetConfig::qemu_default();

    loop {
        let ports = [port];
        if atom_syscall::ipc::wait_any(&ports, 5000).is_ok() {
            if let Ok(Some((header, len))) = try_recv_message(port, &mut ipc_buf) {
                if header.msg_type == MessageType::NetDriverReady {
                    let payload = get_payload(&ipc_buf, len);
                    if let Some(msg) = NetDriverReadyMsg::from_bytes(payload) {
                        cfg.own_mac = msg.mac;
                        log("netd: MAC configured");
                        break;
                    }
                }
            }
        }
    }

    // ── 5. Initialize protocol state ─────────────────────────────────────────
    let mut _buf_pool = BufPool::new();
    let mut arp_table = ArpTable::new();
    let mut tcp_mgr = TcpManager::new();
    let mut dns_cache = DnsCache::new();
    let mut sock_mgr = SocketManager::new();

    // ── 6. Send ICMP ping to gateway as smoke test ────────────────────────────
    {
        let mut pkt_buf = [0u8; 1600];
        let mut icmp_buf = [0u8; 8];
        let icmp_len = build_icmp_echo_request(1, 1, &mut icmp_buf);
        let mut ip_buf = [0u8; 64];
        let ip_len = build_ipv4(cfg.own_ip, cfg.gateway, IP_PROTO_ICMP, 64, &icmp_buf[..icmp_len], &mut ip_buf);
        let broadcast = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let eth_len = build_eth_frame(broadcast, cfg.own_mac, 0x0800, &ip_buf[..ip_len], &mut pkt_buf);
        push_to_tx(&mut tx_producer, &pkt_buf[..eth_len]);
        log("netd: sent ICMP ping to gateway");
    }

    // Also send ARP request for gateway
    {
        let mut arp_buf = [0u8; 64];
        let arp_len = build_arp_request(cfg.own_mac, cfg.own_ip, cfg.gateway, &mut arp_buf);
        push_to_tx(&mut tx_producer, &arp_buf[..arp_len]);
    }

    log("netd: ready");

    // ── 7. Main event loop ────────────────────────────────────────────────────
    let mut idle_count = 0u32;

    loop {
        let mut did_work = false;

        // ── 7a. Process RX ring ───────────────────────────────────────────────
        while rx_consumer.can_pop() {
            if let Some(entry) = rx_consumer.pop() {
                let frame_data = &entry.data[..entry.len as usize];
                if let Some((eth_hdr, eth_payload)) = parse_eth_frame(frame_data) {
                    dispatch_rx(
                        eth_hdr,
                        eth_payload,
                        &mut cfg,
                        &mut arp_table,
                        &mut tcp_mgr,
                        &mut dns_cache,
                        &mut sock_mgr,
                        &mut tx_producer,
                        port,
                    );
                }
                did_work = true;
            }
        }

        // ── 7b. Flush pending DNS resends (deferred from ARP handler) ─────────
        for pr in sock_mgr.pending_resolves.iter_mut() {
            if pr.in_use && pr.needs_resend {
                pr.needs_resend = false;
                let name_len = pr.name_len;
                let gateway_mac = pr.gateway_mac;
                if let Ok(name) = core::str::from_utf8(&pr.name[..name_len]) {
                    let query_id = (atom_syscall::thread::get_ticks() & 0xFFFF) as u16;
                    let mut dns_buf = [0u8; 512];
                    let dns_len = build_dns_query(query_id, name, &mut dns_buf);
                    if dns_len > 0 {
                        let mut udp_buf = [0u8; 600];
                        let udp_len = build_udp_with_checksum(
                            cfg.own_ip, cfg.dns_server,
                            49000, 53,
                            &dns_buf[..dns_len],
                            &mut udp_buf,
                        );
                        let mut ip_buf = [0u8; 700];
                        let ip_len = build_ipv4(
                            cfg.own_ip, cfg.dns_server,
                            IP_PROTO_UDP, 64,
                            &udp_buf[..udp_len],
                            &mut ip_buf,
                        );
                        let mut eth_buf = [0u8; 800];
                        let eth_len = build_eth_frame(
                            gateway_mac, cfg.own_mac, 0x0800,
                            &ip_buf[..ip_len],
                            &mut eth_buf,
                        );
                        push_to_tx(&mut tx_producer, &eth_buf[..eth_len]);
                        log("netd: DNS query resent with unicast MAC");
                        did_work = true;
                    }
                }
            }
        }

        // ── 7c. Process IPC messages ──────────────────────────────────────────
        while let Ok(Some((header, len))) = try_recv_message(port, &mut ipc_buf) {
            log("netd: processing IPC message");
            let payload = get_payload(&ipc_buf, len);
            handle_ipc(
                header.msg_type,
                payload,
                &cfg,
                &mut tcp_mgr,
                &mut dns_cache,
                &mut sock_mgr,
                &mut tx_producer,
            );
            did_work = true;
        }

        // ── 7c. Tick timers ───────────────────────────────────────────────────
        let now = atom_syscall::thread::get_ticks();
        // ARP eviction: 60s = 6000 ticks at 100Hz
        arp_table.evict_expired(now, 6000);

        // TCP tick
        let mut tcp_out = [0u8; 1600];
        if let Some((pkt_len, event)) = tcp_mgr.tick(cfg.own_ip, now, &mut tcp_out) {
            if pkt_len > 0 {
                // Push the raw TCP segment (tick already built it with IP context)
                push_to_tx(&mut tx_producer, &tcp_out[..pkt_len]);
            }
            match event {
                TcpEvent::ConnectTimeout(sid) => {
                    let _ = sid;
                }
                _ => {}
            }
        }

        // ── 7d. Sleep if idle ─────────────────────────────────────────────────
        if !did_work {
            idle_count += 1;
            if idle_count >= 10 {
                atom_syscall::thread::sleep_ms(1);
                idle_count = 0;
            } else {
                atom_syscall::thread::yield_now();
            }
        } else {
            idle_count = 0;
        }
    }
}

/// Dispatch a received Ethernet frame to the appropriate protocol handler.
#[allow(clippy::too_many_arguments)]
fn dispatch_rx(
    eth_hdr: eth::EthHeader,
    payload: &[u8],
    cfg: &mut NetConfig,
    arp_table: &mut ArpTable,
    tcp_mgr: &mut TcpManager,
    dns_cache: &mut DnsCache,
    sock_mgr: &mut SocketManager,
    tx_producer: &mut RingProducer,
    _ipc_port: u64,
) {
    match eth_hdr.ethertype {
        0x0806 => {
            // ARP
            log("netd: RX ARP packet");
            if let Some(arp_pkt) = parse_arp(payload) {
                let now = atom_syscall::thread::get_ticks();
                if arp_pkt.oper == ARP_REPLY {
                    log("netd: ARP reply received, updating table");
                    arp_table.insert(arp_pkt.spa, arp_pkt.sha, now);
                } else if arp_pkt.oper == ARP_REQUEST && arp_pkt.tpa == cfg.own_ip {
                    log("netd: ARP request for our IP, sending reply");
                    // Reply to ARP request for our IP
                    let mut out = [0u8; 64];
                    let len = build_arp_reply(cfg.own_mac, cfg.own_ip, arp_pkt.sha, arp_pkt.spa, &mut out);
                    push_to_tx(tx_producer, &out[..len]);

                    // After sending ARP reply, slirp now knows our MAC.
                    // Mark pending DNS resolves for resend on next event loop tick
                    // (not immediately — slirp needs to process the ARP reply first).
                    for pr in sock_mgr.pending_resolves.iter_mut() {
                        if pr.in_use && !pr.needs_resend {
                            pr.needs_resend = true;
                            pr.gateway_mac = arp_pkt.sha;
                            log("netd: marked DNS query for resend after ARP");
                        }
                    }
                }
            } else {
                log("netd: ARP parse failed");
            }
        }
        0x0800 => {
            // IPv4
            log("netd: RX IPv4 packet");
            if let Some((ip_hdr, ip_payload)) = parse_ipv4(payload) {
                match ip_hdr.protocol {
                    IP_PROTO_ICMP => {
                        log("netd: RX ICMP");
                        if let Some(icmp_pkt) = parse_icmp(ip_payload) {
                            if icmp_pkt.icmp_type == ICMP_ECHO_REPLY {
                                log("netd: gateway reachable (ping ok)");
                            }
                        }
                    }
                    IP_PROTO_UDP => {
                        if let Some((udp_hdr, udp_payload)) = parse_udp(ip_payload) {
                            // DNS response from port 53
                            if udp_hdr.src_port == 53 {
                                log("netd: received UDP from port 53 (DNS response)");
                                if let Some(dns_resp) = parse_dns_response(udp_payload) {
                                    let now = atom_syscall::thread::get_ticks();
                                    let ttl = dns_resp.ttl.max(30);

                                    // Find the pending resolve name that matches this response
                                    // by checking all pending resolves and trying to match
                                    let mut resolved_name_buf = [0u8; 256];
                                    let mut resolved_name_len = 0usize;
                                    for pr in sock_mgr.pending_resolves.iter() {
                                        if pr.in_use {
                                            resolved_name_buf[..pr.name_len].copy_from_slice(&pr.name[..pr.name_len]);
                                            resolved_name_len = pr.name_len;
                                            break; // Take the first pending resolve (FIFO)
                                        }
                                    }

                                    if resolved_name_len > 0 {
                                        if let Ok(name) = core::str::from_utf8(&resolved_name_buf[..resolved_name_len]) {
                                            // Insert into DNS cache
                                            dns_cache.insert(name, dns_resp.ip, ttl, now);
                                            log("netd: DNS response matched, notifying pending resolve");

                                            // Notify pending resolve
                                            if let Some((reply_port, reply_bytes)) =
                                                sock_mgr.notify_dns_resolved(name, dns_resp.ip)
                                            {
                                                send_message(
                                                    reply_port,
                                                    MessageType::NetResolveReply,
                                                    &reply_bytes,
                                                ).ok();
                                                log("netd: NetResolveReply sent");
                                            }
                                        }
                                    } else {
                                        log("netd: DNS response arrived but no pending resolve found");
                                    }
                                }
                            }
                        }
                    }
                    IP_PROTO_TCP => {
                        if let Some((tcp_hdr, tcp_payload)) = parse_tcp(ip_payload) {
                            let mut out_buf = [0u8; 1600];
                            let now = atom_syscall::thread::get_ticks();
                            if let Some(event) = tcp_mgr.on_rx(
                                ip_hdr.src,
                                ip_hdr.dst,
                                &tcp_hdr,
                                tcp_payload,
                                now,
                                &mut out_buf,
                            ) {
                                // Wrap the TCP response in IP+Eth and send
                                let tcp_resp_len = {
                                    // Find the length by checking if out_buf was written
                                    // The build_tcp_segment returns the length but on_rx doesn't
                                    // return it. We check if the first bytes look like a TCP header.
                                    if out_buf[12] == 0x50 {
                                        // data_offset=5 means 20-byte header
                                        let payload_in_resp = match &event {
                                            TcpEvent::Connected(_) => 0,
                                            TcpEvent::DataReceived(_) => 0,
                                            TcpEvent::Closed(_) => 0,
                                            TcpEvent::ConnectTimeout(_) => 0,
                                        };
                                        20 + payload_in_resp
                                    } else {
                                        0
                                    }
                                };

                                if tcp_resp_len > 0 {
                                    // Wrap in IPv4
                                    let mut ip_buf = [0u8; 1600];
                                    let ip_len = build_ipv4(
                                        ip_hdr.dst, ip_hdr.src,
                                        IP_PROTO_TCP, 64,
                                        &out_buf[..tcp_resp_len],
                                        &mut ip_buf,
                                    );
                                    // Wrap in Ethernet
                                    let dst_mac = arp_table.lookup(ip_hdr.src)
                                        .unwrap_or(eth_hdr.src_mac);
                                    let mut eth_buf = [0u8; 1600];
                                    let eth_len = build_eth_frame(
                                        dst_mac, cfg.own_mac, 0x0800,
                                        &ip_buf[..ip_len],
                                        &mut eth_buf,
                                    );
                                    push_to_tx(tx_producer, &eth_buf[..eth_len]);
                                }

                                // Handle events
                                match event {
                                    TcpEvent::Connected(sid) => {
                                        log("netd: TCP SYN-ACK received, sending ACK and notifying app");
                                        // Send the ACK that on_rx built in out_buf
                                        if tcp_resp_len > 0 {
                                            let mut ip_buf = [0u8; 1600];
                                            let ip_len = build_ipv4(
                                                ip_hdr.dst, ip_hdr.src,
                                                IP_PROTO_TCP, 64,
                                                &out_buf[..tcp_resp_len],
                                                &mut ip_buf,
                                            );
                                            let dst_mac = arp_table.lookup(ip_hdr.src)
                                                .unwrap_or(eth_hdr.src_mac);
                                            let mut eth_buf = [0u8; 1600];
                                            let eth_len = build_eth_frame(
                                                dst_mac, cfg.own_mac, 0x0800,
                                                &ip_buf[..ip_len],
                                                &mut eth_buf,
                                            );
                                            push_to_tx(tx_producer, &eth_buf[..eth_len]);
                                        }
                                        // Notify the app that was waiting for connect
                                        if let Some((reply_port, reply_bytes)) =
                                            sock_mgr.notify_connected(sid)
                                        {
                                            send_message(
                                                reply_port,
                                                MessageType::NetConnectReply,
                                                &reply_bytes,
                                            ).ok();
                                        }
                                    }
                                    TcpEvent::DataReceived(sid) => {
                                        if let Some((reply_port, reply_msg)) =
                                            sock_mgr.notify_data_received(sid, tcp_mgr)
                                        {
                                            send_message(
                                                reply_port,
                                                MessageType::NetRecvReply,
                                                &reply_msg.to_bytes(),
                                            ).ok();
                                        }
                                    }
                                    TcpEvent::Closed(sid) => {
                                        let _ = sid;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                log("netd: IPv4 parse failed (bad checksum or version)");
            }
        }
        _ => {
            // Unknown ethertype — ignore
        }
    }
}

/// Handle an IPC message from an app (socket API).
fn handle_ipc(
    msg_type: MessageType,
    payload: &[u8],
    cfg: &NetConfig,
    tcp_mgr: &mut TcpManager,
    dns_cache: &mut DnsCache,
    sock_mgr: &mut SocketManager,
    tx_producer: &mut RingProducer,
) {
    match msg_type {
        MessageType::NetSocket => {
            if let Some((reply_port, reply_bytes)) = sock_mgr.handle_net_socket(payload, tcp_mgr) {
                send_message(reply_port, MessageType::NetSocketReply, &reply_bytes).ok();
            }
        }
        MessageType::NetConnect => {
            let mut out_pkt = [0u8; 1600];
            if let Some((reply_port, reply_bytes, pkt_len)) =
                sock_mgr.handle_net_connect(payload, cfg.own_ip, tcp_mgr, &mut out_pkt)
            {
                if pkt_len > 0 {
                    // Wrap TCP SYN in IPv4 + Ethernet
                    if let Some(msg) = libipc::messages::NetConnectMsg::from_bytes(payload) {
                        let hop = next_hop(msg.remote_ip, cfg.own_ip, cfg.netmask, cfg.gateway);
                        let dst_mac = get_or_broadcast_mac(hop, tx_producer, cfg);
                        let mut ip_buf = [0u8; 1600];
                        let ip_len = build_ipv4(
                            cfg.own_ip, msg.remote_ip,
                            IP_PROTO_TCP, 64,
                            &out_pkt[..pkt_len],
                            &mut ip_buf,
                        );
                        let mut eth_buf = [0u8; 1600];
                        let eth_len = build_eth_frame(
                            dst_mac, cfg.own_mac, 0x0800,
                            &ip_buf[..ip_len],
                            &mut eth_buf,
                        );
                        push_to_tx(tx_producer, &eth_buf[..eth_len]);
                    }
                }
                // reply_port=0 means "SYN sent, waiting for SYN-ACK" — don't reply yet
                if reply_port != 0 {
                    send_message(reply_port, MessageType::NetConnectReply, &reply_bytes).ok();
                }
            }
        }
        MessageType::NetSend => {
            let mut out_pkt = [0u8; 1600];
            if let Some((reply_port, reply_bytes, pkt_len)) =
                sock_mgr.handle_net_send(payload, cfg.own_ip, tcp_mgr, &mut out_pkt)
            {
                if pkt_len > 0 {
                    if let Some(msg) = libipc::messages::NetSendMsg::from_bytes(payload) {
                        if let Some(sock) = tcp_mgr.get_socket(msg.socket_id) {
                            let remote_ip = sock.remote_ip;
                            let hop = next_hop(remote_ip, cfg.own_ip, cfg.netmask, cfg.gateway);
                            let dst_mac = get_or_broadcast_mac(hop, tx_producer, cfg);
                            let mut ip_buf = [0u8; 1600];
                            let ip_len = build_ipv4(
                                cfg.own_ip, remote_ip,
                                IP_PROTO_TCP, 64,
                                &out_pkt[..pkt_len],
                                &mut ip_buf,
                            );
                            let mut eth_buf = [0u8; 1600];
                            let eth_len = build_eth_frame(
                                dst_mac, cfg.own_mac, 0x0800,
                                &ip_buf[..ip_len],
                                &mut eth_buf,
                            );
                            push_to_tx(tx_producer, &eth_buf[..eth_len]);
                        }
                    }
                }
                send_message(reply_port, MessageType::NetSendReply, &reply_bytes).ok();
            }
        }
        MessageType::NetRecv => {
            if let Some((reply_port, reply_msg)) = sock_mgr.handle_net_recv(payload, tcp_mgr) {
                send_message(reply_port, MessageType::NetRecvReply, &reply_msg.to_bytes()).ok();
            }
        }
        MessageType::NetClose => {
            let mut out_pkt = [0u8; 1600];
            if let Some((reply_port, reply_bytes, pkt_len)) =
                sock_mgr.handle_net_close(payload, cfg.own_ip, tcp_mgr, &mut out_pkt)
            {
                if pkt_len > 0 {
                    if let Some(msg) = libipc::messages::NetCloseMsg::from_bytes(payload) {
                        if let Some(sock) = tcp_mgr.get_socket(msg.socket_id) {
                            let remote_ip = sock.remote_ip;
                            let hop = next_hop(remote_ip, cfg.own_ip, cfg.netmask, cfg.gateway);
                            let dst_mac = get_or_broadcast_mac(hop, tx_producer, cfg);
                            let mut ip_buf = [0u8; 1600];
                            let ip_len = build_ipv4(
                                cfg.own_ip, remote_ip,
                                IP_PROTO_TCP, 64,
                                &out_pkt[..pkt_len],
                                &mut ip_buf,
                            );
                            let mut eth_buf = [0u8; 1600];
                            let eth_len = build_eth_frame(
                                dst_mac, cfg.own_mac, 0x0800,
                                &ip_buf[..ip_len],
                                &mut eth_buf,
                            );
                            push_to_tx(tx_producer, &eth_buf[..eth_len]);
                        }
                    }
                }
                send_message(reply_port, MessageType::NetCloseReply, &reply_bytes).ok();
            }
        }
        MessageType::NetResolve => {
            let now = atom_syscall::thread::get_ticks();
            log("netd: received NetResolve request");
            // handle_net_resolve returns Some on cache hit, None on miss (pending stored)
            let cache_reply = sock_mgr.handle_net_resolve(payload, dns_cache, now);

            if let Some((reply_port, reply_bytes)) = cache_reply {
                // Cache hit — reply immediately
                log("netd: DNS cache hit, replying immediately");
                send_message(reply_port, MessageType::NetResolveReply, &reply_bytes).ok();
            } else {
                // Cache miss — send DNS query; reply will come when DNS response arrives
                log("netd: DNS cache miss, sending query");
                if let Some(msg) = libipc::messages::NetResolveMsg::from_bytes(payload) {
                    let name_len = msg.name_len as usize;
                    let name_bytes = &msg.name[..name_len.min(256)];
                    if let Ok(name) = core::str::from_utf8(name_bytes) {
                        let query_id = (atom_syscall::thread::get_ticks() & 0xFFFF) as u16;
                        let mut dns_buf = [0u8; 512];
                        let dns_len = build_dns_query(query_id, name, &mut dns_buf);
                        if dns_len > 0 {
                            let mut udp_buf = [0u8; 600];
                            let udp_len = build_udp_with_checksum(
                                cfg.own_ip, cfg.dns_server,
                                49000, 53,
                                &dns_buf[..dns_len],
                                &mut udp_buf,
                            );
                            let mut ip_buf = [0u8; 700];
                            let ip_len = build_ipv4(
                                cfg.own_ip, cfg.dns_server,
                                IP_PROTO_UDP, 64,
                                &udp_buf[..udp_len],
                                &mut ip_buf,
                            );
                            let broadcast = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
                            let mut eth_buf = [0u8; 800];
                            let eth_len = build_eth_frame(
                                broadcast, cfg.own_mac, 0x0800,
                                &ip_buf[..ip_len],
                                &mut eth_buf,
                            );
                            push_to_tx(tx_producer, &eth_buf[..eth_len]);
                            log("netd: DNS query pushed to TX");
                        } else {
                            log("netd: ERROR build_dns_query returned 0");
                        }
                    }
                }
            }
        }
        MessageType::NetConfigure => {
            // Configuration update from init/service_manager
            if let Some(msg) = libipc::messages::NetConfigureMsg::from_bytes(payload) {
                let _ = msg; // Would update cfg, but cfg is not mut here
                // In a real implementation, we'd update the config
            }
        }
        _ => {}
    }
}

/// Get MAC for a given IP from ARP table, or return broadcast and send ARP request.
fn get_or_broadcast_mac(
    ip: u32,
    tx_producer: &mut RingProducer,
    cfg: &NetConfig,
) -> [u8; 6] {
    // We don't have access to arp_table here, so always use broadcast for now.
    // In a full implementation, we'd pass arp_table as a parameter.
    let broadcast = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    // Send ARP request for the IP
    let mut arp_buf = [0u8; 64];
    let arp_len = build_arp_request(cfg.own_mac, cfg.own_ip, ip, &mut arp_buf);
    push_to_tx(tx_producer, &arp_buf[..arp_len]);
    broadcast
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log("netd: PANIC");
    if let Some(loc) = info.location() {
        log(loc.file());
    }
    loop {
        atom_syscall::thread::sleep_ms(1000);
    }
}
