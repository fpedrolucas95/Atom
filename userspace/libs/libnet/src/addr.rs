use libipc::messages::NetIpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpAddr {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl IpAddr {
    pub fn from_u32(ip: u32) -> Self {
        Self::V4(ip.to_be_bytes())
    }

    pub fn to_u32(&self) -> Option<u32> {
        match self {
            Self::V4(bytes) => Some(u32::from_be_bytes(*bytes)),
            Self::V6(_) => None,
        }
    }

    pub fn from_net_ip_addr(net_ip: NetIpAddr) -> Self {
        if net_ip.family == 6 {
            Self::V6(net_ip.data)
        } else {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&net_ip.data[0..4]);
            Self::V4(bytes)
        }
    }

    pub fn to_net_ip_addr(&self) -> NetIpAddr {
        match self {
            Self::V4(bytes) => NetIpAddr::ipv4(*bytes),
            Self::V6(bytes) => NetIpAddr::ipv6(*bytes),
        }
    }
}

impl core::fmt::Display for IpAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::V4(b) => write!(f, "{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
            Self::V6(b) => {
                for i in 0..8 {
                    let word = u16::from_be_bytes([b[i * 2], b[i * 2 + 1]]);
                    write!(f, "{:x}", word)?;
                    if i < 7 {
                        write!(f, ":")?;
                    }
                }
                Ok(())
            }
        }
    }
}
