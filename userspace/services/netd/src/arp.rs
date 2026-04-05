use crate::eth::{build_eth_frame, ETHERTYPE_ARP};

/// ARP packet (28 bytes for Ethernet/IPv4)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArpPacket {
    pub htype: u16,   // big-endian: 1 = Ethernet
    pub ptype: u16,   // big-endian: 0x0800 = IPv4
    pub hlen: u8,     // 6
    pub plen: u8,     // 4
    pub oper: u16,    // big-endian: 1=request, 2=reply
    pub sha: [u8; 6], // sender hardware addr
    pub spa: u32,     // sender protocol addr (big-endian)
    pub tha: [u8; 6], // target hardware addr
    pub tpa: u32,     // target protocol addr (big-endian)
}

pub const ARP_REQUEST: u16 = 1;
pub const ARP_REPLY: u16 = 2;
pub const ARP_PACKET_LEN: usize = 28;

/// ARP table with up to 16 entries: (ip, mac, timestamp_ticks)
pub struct ArpTable {
    entries: [(u32, [u8; 6], u64); 16],
    count: usize,
}

impl ArpTable {
    pub fn new() -> Self {
        Self {
            entries: [(0, [0u8; 6], 0); 16],
            count: 0,
        }
    }

    pub fn lookup(&self, ip: u32) -> Option<[u8; 6]> {
        for i in 0..self.count {
            if self.entries[i].0 == ip {
                return Some(self.entries[i].1);
            }
        }
        None
    }

    pub fn insert(&mut self, ip: u32, mac: [u8; 6], now_ticks: u64) {
        // Update existing entry if present
        for i in 0..self.count {
            if self.entries[i].0 == ip {
                self.entries[i].1 = mac;
                self.entries[i].2 = now_ticks;
                return;
            }
        }
        // Add new entry, evict oldest if full
        if self.count < 16 {
            self.entries[self.count] = (ip, mac, now_ticks);
            self.count += 1;
        } else {
            // Find oldest entry and replace
            let mut oldest_idx = 0;
            let mut oldest_ticks = self.entries[0].2;
            for i in 1..16 {
                if self.entries[i].2 < oldest_ticks {
                    oldest_ticks = self.entries[i].2;
                    oldest_idx = i;
                }
            }
            self.entries[oldest_idx] = (ip, mac, now_ticks);
        }
    }

    pub fn evict_expired(&mut self, now_ticks: u64, timeout_ticks: u64) {
        let mut i = 0;
        while i < self.count {
            if now_ticks.saturating_sub(self.entries[i].2) > timeout_ticks {
                // Remove by swapping with last
                self.count -= 1;
                self.entries[i] = self.entries[self.count];
                self.entries[self.count] = (0, [0u8; 6], 0);
            } else {
                i += 1;
            }
        }
    }
}

fn write_arp_packet(pkt: &ArpPacket, out: &mut [u8]) -> usize {
    if out.len() < ARP_PACKET_LEN {
        return 0;
    }
    out[0..2].copy_from_slice(&pkt.htype.to_be_bytes());
    out[2..4].copy_from_slice(&pkt.ptype.to_be_bytes());
    out[4] = pkt.hlen;
    out[5] = pkt.plen;
    out[6..8].copy_from_slice(&pkt.oper.to_be_bytes());
    out[8..14].copy_from_slice(&pkt.sha);
    out[14..18].copy_from_slice(&pkt.spa.to_be_bytes());
    out[18..24].copy_from_slice(&pkt.tha);
    out[24..28].copy_from_slice(&pkt.tpa.to_be_bytes());
    ARP_PACKET_LEN
}

/// Build an ARP request frame (Ethernet + ARP). Returns total length written to `out`.
pub fn build_arp_request(
    src_mac: [u8; 6],
    src_ip: u32,
    target_ip: u32,
    out: &mut [u8],
) -> usize {
    let pkt = ArpPacket {
        htype: 1,
        ptype: 0x0800,
        hlen: 6,
        plen: 4,
        oper: ARP_REQUEST,
        sha: src_mac,
        spa: src_ip,
        tha: [0u8; 6],
        tpa: target_ip,
    };
    let mut arp_buf = [0u8; ARP_PACKET_LEN];
    write_arp_packet(&pkt, &mut arp_buf);
    let broadcast = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    build_eth_frame(broadcast, src_mac, ETHERTYPE_ARP, &arp_buf, out)
}

/// Build an ARP reply frame (Ethernet + ARP). Returns total length written to `out`.
pub fn build_arp_reply(
    src_mac: [u8; 6],
    src_ip: u32,
    dst_mac: [u8; 6],
    dst_ip: u32,
    out: &mut [u8],
) -> usize {
    let pkt = ArpPacket {
        htype: 1,
        ptype: 0x0800,
        hlen: 6,
        plen: 4,
        oper: ARP_REPLY,
        sha: src_mac,
        spa: src_ip,
        tha: dst_mac,
        tpa: dst_ip,
    };
    let mut arp_buf = [0u8; ARP_PACKET_LEN];
    write_arp_packet(&pkt, &mut arp_buf);
    build_eth_frame(dst_mac, src_mac, ETHERTYPE_ARP, &arp_buf, out)
}

/// Parse an ARP packet from raw bytes (not including Ethernet header).
pub fn parse_arp(data: &[u8]) -> Option<ArpPacket> {
    if data.len() < ARP_PACKET_LEN {
        return None;
    }
    let htype = u16::from_be_bytes([data[0], data[1]]);
    let ptype = u16::from_be_bytes([data[2], data[3]]);
    let hlen = data[4];
    let plen = data[5];
    let oper = u16::from_be_bytes([data[6], data[7]]);
    let mut sha = [0u8; 6];
    sha.copy_from_slice(&data[8..14]);
    let spa = u32::from_be_bytes([data[14], data[15], data[16], data[17]]);
    let mut tha = [0u8; 6];
    tha.copy_from_slice(&data[18..24]);
    let tpa = u32::from_be_bytes([data[24], data[25], data[26], data[27]]);
    Some(ArpPacket { htype, ptype, hlen, plen, oper, sha, spa, tha, tpa })
}
