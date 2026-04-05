use crate::ip::ipv4_checksum;

pub mod v4 {
    use crate::ip::ipv4_checksum;

    pub const ICMPV4_ECHO_REQUEST: u8 = 8;
    pub const ICMPV4_ECHO_REPLY: u8 = 0;
    pub const ICMPV4_HEADER_LEN: usize = 8;

    /// ICMPv4 packet header
    #[derive(Debug, Clone, Copy)]
    pub struct Icmpv4Packet {
        pub icmp_type: u8,
        pub code: u8,
        pub checksum: u16,
        pub id: u16,
        pub seq: u16,
    }

    /// Build an ICMPv4 Echo Request into `out`. Returns length written.
    pub fn build_echo_request(id: u16, seq: u16, payload: &[u8], out: &mut [u8]) -> usize {
        let total_len = ICMPV4_HEADER_LEN + payload.len();
        if out.len() < total_len {
            return 0;
        }
        out[0] = ICMPV4_ECHO_REQUEST;
        out[1] = 0; // code
        out[2..4].copy_from_slice(&0u16.to_be_bytes()); // checksum placeholder
        out[4..6].copy_from_slice(&id.to_be_bytes());
        out[6..8].copy_from_slice(&seq.to_be_bytes());

        if !payload.is_empty() {
            out[ICMPV4_HEADER_LEN..total_len].copy_from_slice(payload);
        }

        // Compute checksum over the whole ICMP packet
        let csum = ipv4_checksum(&out[0..total_len]);
        out[2..4].copy_from_slice(&csum.to_be_bytes());
        total_len
    }

    /// Parse an ICMPv4 packet. Returns (header, payload) or None if data < 8 bytes.
    pub fn parse_icmp(data: &[u8]) -> Option<(Icmpv4Packet, &[u8])> {
        if data.len() < ICMPV4_HEADER_LEN {
            return None;
        }
        // Validate checksum
        if ipv4_checksum(data) != 0 {
            return None;
        }

        let pkt = Icmpv4Packet {
            icmp_type: data[0],
            code: data[1],
            checksum: u16::from_be_bytes([data[2], data[3]]),
            id: u16::from_be_bytes([data[4], data[5]]),
            seq: u16::from_be_bytes([data[6], data[7]]),
        };
        Some((pkt, &data[ICMPV4_HEADER_LEN..]))
    }
}

pub mod v6 {
    // Reserved for future ICMPv6 implementation
}

// Re-export v4 types as default for now to minimize breakage
pub use v4::{
    ICMPV4_ECHO_REQUEST as ICMP_ECHO_REQUEST,
    ICMPV4_ECHO_REPLY as ICMP_ECHO_REPLY,
    ICMPV4_HEADER_LEN as ICMP_HEADER_LEN,
    Icmpv4Packet as IcmpPacket,
    build_echo_request as build_icmp_echo_request,
    parse_icmp,
};


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_echo_request() {
        let mut buf = [0u8; 12];
        let payload = [0xAA, 0xBB, 0xCC, 0xDD];
        let len = build_icmp_echo_request(0x1234, 1, &payload, &mut buf);
        assert_eq!(len, 12);
        let (pkt, pld) = parse_icmp(&buf[..len]).unwrap();
        assert_eq!(pkt.icmp_type, ICMP_ECHO_REQUEST);
        assert_eq!(pkt.code, 0);
        assert_eq!(pkt.id, 0x1234);
        assert_eq!(pkt.seq, 1);
        assert_eq!(pld, &payload);
        // Verify checksum: re-computing over the packet with checksum included should give 0
        let csum = ipv4_checksum(&buf[..len]);
        assert_eq!(csum, 0);
    }

    #[test]
    fn parse_too_short_returns_none() {
        let data = [0u8; 4];
        assert!(parse_icmp(&data).is_none());
    }

    #[test]
    fn parse_bad_checksum_returns_none() {
        let mut buf = [0u8; 8];
        build_icmp_echo_request(0x1234, 1, &[], &mut buf);
        buf[2] ^= 0xFF; // Corrupt checksum
        assert!(parse_icmp(&buf).is_none());
    }
}
