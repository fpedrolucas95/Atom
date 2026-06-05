pub const IP_PROTO_ICMP: u8 = 1;
pub const IP_PROTO_TCP: u8 = 6;
pub const IP_PROTO_UDP: u8 = 17;

pub const IPV4_HEADER_LEN: usize = 20;

/// IPv4 header (20 bytes, no options)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Ipv4Header {
    pub ver_ihl: u8, // version=4, ihl=5 → 0x45
    pub dscp_ecn: u8,
    pub total_len: u16,  // big-endian
    pub id: u16,         // big-endian
    pub flags_frag: u16, // big-endian
    pub ttl: u8,
    pub protocol: u8,
    pub checksum: u16, // big-endian
    pub src: u32,      // big-endian
    pub dst: u32,      // big-endian
}

/// Compute IPv4 one's complement checksum over header bytes.
pub fn ipv4_checksum(header_bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < header_bytes.len() {
        let word = u16::from_be_bytes([header_bytes[i], header_bytes[i + 1]]) as u32;
        sum += word;
        i += 2;
    }
    // Handle odd byte
    if i < header_bytes.len() {
        sum += (header_bytes[i] as u32) << 8;
    }
    // Fold carry
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build an IPv4 packet into `out`. Returns total length (header + payload).
pub fn build_ipv4(src: u32, dst: u32, proto: u8, ttl: u8, payload: &[u8], out: &mut [u8]) -> usize {
    let total_len = IPV4_HEADER_LEN + payload.len();
    if out.len() < total_len {
        return 0;
    }
    out[0] = 0x45; // version=4, ihl=5
    out[1] = 0; // DSCP/ECN
    out[2..4].copy_from_slice(&(total_len as u16).to_be_bytes());
    out[4..6].copy_from_slice(&0u16.to_be_bytes()); // id=0
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // flags/frag=0 (DF could be set)
    out[8] = ttl;
    out[9] = proto;
    out[10..12].copy_from_slice(&0u16.to_be_bytes()); // checksum=0 initially
    out[12..16].copy_from_slice(&src.to_be_bytes());
    out[16..20].copy_from_slice(&dst.to_be_bytes());
    // Compute checksum over header
    let csum = ipv4_checksum(&out[0..IPV4_HEADER_LEN]);
    out[10..12].copy_from_slice(&csum.to_be_bytes());
    // Copy payload
    out[IPV4_HEADER_LEN..total_len].copy_from_slice(payload);
    total_len
}

/// Parse an IPv4 packet. Validates version, IHL, and checksum.
/// Returns (header, payload) or None on invalid.
pub fn parse_ipv4(data: &[u8]) -> Option<(Ipv4Header, &[u8])> {
    if data.len() < IPV4_HEADER_LEN {
        return None;
    }
    let ver_ihl = data[0];
    if ver_ihl != 0x45 {
        // Only support version=4, ihl=5 (no options)
        return None;
    }
    // Validate checksum
    let csum = ipv4_checksum(&data[0..IPV4_HEADER_LEN]);
    if csum != 0 {
        return None;
    }
    let total_len = u16::from_be_bytes([data[2], data[3]]) as usize;
    if total_len < IPV4_HEADER_LEN || total_len > data.len() {
        return None;
    }
    let hdr = Ipv4Header {
        ver_ihl,
        dscp_ecn: data[1],
        total_len: u16::from_be_bytes([data[2], data[3]]),
        id: u16::from_be_bytes([data[4], data[5]]),
        flags_frag: u16::from_be_bytes([data[6], data[7]]),
        ttl: data[8],
        protocol: data[9],
        checksum: u16::from_be_bytes([data[10], data[11]]),
        src: u32::from_be_bytes([data[12], data[13], data[14], data[15]]),
        dst: u32::from_be_bytes([data[16], data[17], data[18], data[19]]),
    };
    Some((hdr, &data[IPV4_HEADER_LEN..total_len]))
}

/// Determine next hop IP for routing.
/// If dst is on the same subnet → return dst directly.
/// Otherwise → return gateway.
pub fn next_hop(dst_ip: u32, own_ip: u32, mask: u32, gateway: u32) -> u32 {
    if (dst_ip & mask) == (own_ip & mask) {
        dst_ip
    } else {
        gateway
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_known_header() {
        // Build a header and verify checksum round-trips to 0
        let mut buf = [0u8; 64];
        let src = u32::from_be_bytes([10, 0, 2, 15]);
        let dst = u32::from_be_bytes([10, 0, 2, 2]);
        let len = build_ipv4(src, dst, IP_PROTO_ICMP, 64, &[0u8; 8], &mut buf);
        assert!(len > 0);
        // Checksum of the header should be 0 when re-computed
        let csum = ipv4_checksum(&buf[0..20]);
        assert_eq!(csum, 0);
    }

    #[test]
    fn parse_roundtrip() {
        let mut buf = [0u8; 64];
        let src = u32::from_be_bytes([192, 168, 1, 1]);
        let dst = u32::from_be_bytes([8, 8, 8, 8]);
        let payload = [0xABu8; 8];
        let len = build_ipv4(src, dst, IP_PROTO_UDP, 64, &payload, &mut buf);
        let (hdr, pld) = parse_ipv4(&buf[..len]).unwrap();
        assert_eq!(hdr.src, src);
        assert_eq!(hdr.dst, dst);
        assert_eq!(hdr.protocol, IP_PROTO_UDP);
        assert_eq!(pld, &payload[..]);
    }

    #[test]
    fn next_hop_same_subnet() {
        let own = u32::from_be_bytes([10, 0, 2, 15]);
        let mask = u32::from_be_bytes([255, 255, 255, 0]);
        let gw = u32::from_be_bytes([10, 0, 2, 2]);
        let dst = u32::from_be_bytes([10, 0, 2, 100]);
        assert_eq!(next_hop(dst, own, mask, gw), dst);
    }

    #[test]
    fn next_hop_different_subnet() {
        let own = u32::from_be_bytes([10, 0, 2, 15]);
        let mask = u32::from_be_bytes([255, 255, 255, 0]);
        let gw = u32::from_be_bytes([10, 0, 2, 2]);
        let dst = u32::from_be_bytes([8, 8, 8, 8]);
        assert_eq!(next_hop(dst, own, mask, gw), gw);
    }
}
