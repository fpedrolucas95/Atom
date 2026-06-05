/// Ethernet frame header (14 bytes)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EthHeader {
    pub dst_mac: [u8; 6],
    pub src_mac: [u8; 6],
    pub ethertype: u16, // stored as big-endian bytes in memory
}

/// EtherType for IPv4 (native u16, written big-endian by build_eth_frame)
pub const ETHERTYPE_IPV4: u16 = 0x0800u16;
/// EtherType for ARP (native u16, written big-endian by build_eth_frame)
pub const ETHERTYPE_ARP: u16 = 0x0806u16;

pub const ETH_HEADER_LEN: usize = 14;

/// Build an Ethernet frame into `out`. Returns total frame length.
/// `ethertype` is passed as native u16 (e.g. 0x0800) and written big-endian.
pub fn build_eth_frame(
    dst: [u8; 6],
    src: [u8; 6],
    ethertype: u16,
    payload: &[u8],
    out: &mut [u8],
) -> usize {
    let total = ETH_HEADER_LEN + payload.len();
    if out.len() < total {
        return 0;
    }
    out[0..6].copy_from_slice(&dst);
    out[6..12].copy_from_slice(&src);
    out[12..14].copy_from_slice(&ethertype.to_be_bytes());
    out[14..total].copy_from_slice(payload);
    total
}

/// Parse an Ethernet frame. Returns (header, payload) or None if too short.
pub fn parse_eth_frame(data: &[u8]) -> Option<(EthHeader, &[u8])> {
    if data.len() < ETH_HEADER_LEN {
        return None;
    }
    let mut dst_mac = [0u8; 6];
    let mut src_mac = [0u8; 6];
    dst_mac.copy_from_slice(&data[0..6]);
    src_mac.copy_from_slice(&data[6..12]);
    let ethertype = u16::from_be_bytes([data[12], data[13]]);
    let hdr = EthHeader {
        dst_mac,
        src_mac,
        ethertype,
    };
    Some((hdr, &data[ETH_HEADER_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_parse_arp_frame() {
        let dst = [0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        let src = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let payload = [0u8; 28]; // ARP packet size
        let mut out = [0u8; 64];
        let len = build_eth_frame(dst, src, 0x0806, &payload, &mut out);
        assert_eq!(len, 14 + 28);
        let (hdr, pld) = parse_eth_frame(&out[..len]).unwrap();
        assert_eq!(hdr.dst_mac, dst);
        assert_eq!(hdr.src_mac, src);
        assert_eq!(hdr.ethertype, 0x0806);
        assert_eq!(pld.len(), 28);
    }

    #[test]
    fn build_parse_ipv4_frame() {
        let dst = [0x52, 0x54, 0x00, 0xAA, 0xBB, 0xCC];
        let src = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let payload = [0xABu8; 20];
        let mut out = [0u8; 64];
        let len = build_eth_frame(dst, src, 0x0800, &payload, &mut out);
        assert_eq!(len, 14 + 20);
        let (hdr, pld) = parse_eth_frame(&out[..len]).unwrap();
        assert_eq!(hdr.ethertype, 0x0800);
        assert_eq!(pld, &payload[..]);
    }

    #[test]
    fn parse_too_short_returns_none() {
        let data = [0u8; 10];
        assert!(parse_eth_frame(&data).is_none());
    }
}
