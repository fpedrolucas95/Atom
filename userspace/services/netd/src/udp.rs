pub const UDP_HEADER_LEN: usize = 8;

/// UDP header (8 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UdpHeader {
    pub src_port: u16,  // big-endian
    pub dst_port: u16,  // big-endian
    pub length: u16,    // big-endian: header + payload
    pub checksum: u16,  // big-endian (0 = disabled)
}

/// Compute UDP checksum using IPv4 pseudo-header.
pub fn udp_checksum(src_ip: u32, dst_ip: u32, udp_segment: &[u8]) -> u16 {
    let udp_len = udp_segment.len() as u16;
    let mut sum: u32 = 0;
    // Pseudo-header: src_ip, dst_ip, 0x00, proto=17, udp_length
    sum += (src_ip >> 16) as u32;
    sum += (src_ip & 0xFFFF) as u32;
    sum += (dst_ip >> 16) as u32;
    sum += (dst_ip & 0xFFFF) as u32;
    sum += 17u32; // protocol UDP
    sum += udp_len as u32;
    // UDP segment
    let mut i = 0;
    while i + 1 < udp_segment.len() {
        let word = u16::from_be_bytes([udp_segment[i], udp_segment[i + 1]]) as u32;
        sum += word;
        i += 2;
    }
    if i < udp_segment.len() {
        sum += (udp_segment[i] as u32) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    let result = !(sum as u16);
    // RFC 768: if computed checksum is 0, transmit as 0xFFFF
    if result == 0 { 0xFFFF } else { result }
}

/// Build a UDP datagram into `out` with proper checksum.
/// src_ip and dst_ip are needed for the pseudo-header checksum.
/// Returns total length written (8 + payload.len()).
pub fn build_udp_with_checksum(
    src_ip: u32,
    dst_ip: u32,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
    out: &mut [u8],
) -> usize {
    let total = UDP_HEADER_LEN + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..2].copy_from_slice(&src_port.to_be_bytes());
    out[2..4].copy_from_slice(&dst_port.to_be_bytes());
    out[4..6].copy_from_slice(&(total as u16).to_be_bytes());
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out[8..total].copy_from_slice(payload);
    let csum = udp_checksum(src_ip, dst_ip, &out[..total]);
    out[6..8].copy_from_slice(&csum.to_be_bytes());
    total
}

/// Build a UDP datagram into `out`. Checksum is set to 0 (valid per RFC 768).
/// Returns total length written (8 + payload.len()).
pub fn build_udp(src_port: u16, dst_port: u16, payload: &[u8], out: &mut [u8]) -> usize {
    let total = UDP_HEADER_LEN + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..2].copy_from_slice(&src_port.to_be_bytes());
    out[2..4].copy_from_slice(&dst_port.to_be_bytes());
    out[4..6].copy_from_slice(&(total as u16).to_be_bytes());
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // checksum=0
    out[8..total].copy_from_slice(payload);
    total
}

/// Parse a UDP datagram. Returns (header, payload) or None if data < 8 bytes.
pub fn parse_udp(data: &[u8]) -> Option<(UdpHeader, &[u8])> {
    if data.len() < UDP_HEADER_LEN {
        return None;
    }
    let length = u16::from_be_bytes([data[4], data[5]]) as usize;
    let payload_end = length.min(data.len());
    let hdr = UdpHeader {
        src_port: u16::from_be_bytes([data[0], data[1]]),
        dst_port: u16::from_be_bytes([data[2], data[3]]),
        length: length as u16,
        checksum: u16::from_be_bytes([data[6], data[7]]),
    };
    Some((hdr, &data[UDP_HEADER_LEN..payload_end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_udp() {
        let payload = b"hello";
        let mut buf = [0u8; 64];
        let len = build_udp(12345, 53, payload, &mut buf);
        assert_eq!(len, 8 + 5);
        let (hdr, pld) = parse_udp(&buf[..len]).unwrap();
        assert_eq!(hdr.src_port, 12345);
        assert_eq!(hdr.dst_port, 53);
        assert_eq!(hdr.length, 13);
        assert_eq!(hdr.checksum, 0);
        assert_eq!(pld, payload);
    }

    #[test]
    fn parse_too_short_returns_none() {
        let data = [0u8; 4];
        assert!(parse_udp(&data).is_none());
    }
}
