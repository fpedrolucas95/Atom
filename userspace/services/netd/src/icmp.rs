use crate::ip::ipv4_checksum;

pub const ICMP_ECHO_REQUEST: u8 = 8;
pub const ICMP_ECHO_REPLY: u8 = 0;
pub const ICMP_HEADER_LEN: usize = 8;

/// ICMP packet header
#[derive(Debug, Clone, Copy)]
pub struct IcmpPacket {
    pub icmp_type: u8,
    pub code: u8,
    pub checksum: u16,
    pub id: u16,
    pub seq: u16,
}

/// Build an ICMP Echo Request into `out`. Returns length written.
pub fn build_icmp_echo_request(id: u16, seq: u16, out: &mut [u8]) -> usize {
    if out.len() < ICMP_HEADER_LEN {
        return 0;
    }
    out[0] = ICMP_ECHO_REQUEST;
    out[1] = 0; // code
    out[2..4].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
    out[4..6].copy_from_slice(&id.to_be_bytes());
    out[6..8].copy_from_slice(&seq.to_be_bytes());
    // Compute checksum over the 8-byte ICMP header
    let csum = ipv4_checksum(&out[0..ICMP_HEADER_LEN]);
    out[2..4].copy_from_slice(&csum.to_be_bytes());
    ICMP_HEADER_LEN
}

/// Parse an ICMP packet. Returns None if data < 8 bytes.
pub fn parse_icmp(data: &[u8]) -> Option<IcmpPacket> {
    if data.len() < ICMP_HEADER_LEN {
        return None;
    }
    Some(IcmpPacket {
        icmp_type: data[0],
        code: data[1],
        checksum: u16::from_be_bytes([data[2], data[3]]),
        id: u16::from_be_bytes([data[4], data[5]]),
        seq: u16::from_be_bytes([data[6], data[7]]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_echo_request() {
        let mut buf = [0u8; 8];
        let len = build_icmp_echo_request(0x1234, 1, &mut buf);
        assert_eq!(len, 8);
        let pkt = parse_icmp(&buf).unwrap();
        assert_eq!(pkt.icmp_type, ICMP_ECHO_REQUEST);
        assert_eq!(pkt.code, 0);
        assert_eq!(pkt.id, 0x1234);
        assert_eq!(pkt.seq, 1);
        // Verify checksum: re-computing over the header with checksum included should give 0
        let csum = ipv4_checksum(&buf[0..8]);
        assert_eq!(csum, 0);
    }

    #[test]
    fn parse_too_short_returns_none() {
        let data = [0u8; 4];
        assert!(parse_icmp(&data).is_none());
    }
}
