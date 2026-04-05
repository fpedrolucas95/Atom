/// Network configuration for netd
pub struct NetConfig {
    pub own_ip: u32,
    pub netmask: u32,
    pub gateway: u32,
    pub dns_server: u32,
    pub own_mac: [u8; 6],
}

impl NetConfig {
    /// Default QEMU user-mode network config: 10.0.2.15/24, gw 10.0.2.2, dns 10.0.2.3
    pub fn qemu_default() -> Self {
        Self {
            own_ip:     u32::from_be_bytes([10, 0, 2, 15]),
            netmask:    u32::from_be_bytes([255, 255, 255, 0]),
            gateway:    u32::from_be_bytes([10, 0, 2, 2]),
            dns_server: u32::from_be_bytes([10, 0, 2, 3]),  // QEMU slirp DNS proxy
            own_mac:    [0u8; 6],
        }
    }
}
