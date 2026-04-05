/// A parsed DNS response with the first A record
pub struct DnsResponse {
    pub id: u16,
    pub ip: u32,   // first A record, big-endian
    pub ttl: u32,
}

/// DNS cache entry: (name_hash, ip, expire_ticks)
pub struct DnsCache {
    entries: [(u64, u32, u64); 16],
    count: usize,
}

impl DnsCache {
    pub fn new() -> Self {
        Self {
            entries: [(0, 0, 0); 16],
            count: 0,
        }
    }

    pub fn lookup(&self, name: &str, now_ticks: u64) -> Option<u32> {
        let hash = Self::hash_name(name);
        for i in 0..self.count {
            let (h, ip, expire) = self.entries[i];
            if h == hash && now_ticks < expire {
                return Some(ip);
            }
        }
        None
    }

    pub fn insert(&mut self, name: &str, ip: u32, ttl_secs: u32, now_ticks: u64) {
        let hash = Self::hash_name(name);
        // Ticks at 100Hz: ttl_secs * 100
        let expire = now_ticks + (ttl_secs as u64) * 100;
        // Update existing
        for i in 0..self.count {
            if self.entries[i].0 == hash {
                self.entries[i].1 = ip;
                self.entries[i].2 = expire;
                return;
            }
        }
        if self.count < 16 {
            self.entries[self.count] = (hash, ip, expire);
            self.count += 1;
        } else {
            // Evict oldest (smallest expire)
            let mut oldest = 0;
            for i in 1..16 {
                if self.entries[i].2 < self.entries[oldest].2 {
                    oldest = i;
                }
            }
            self.entries[oldest] = (hash, ip, expire);
        }
    }

    /// FNV-1a 64-bit hash
    fn hash_name(name: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in name.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }
}

/// Build a DNS query for an A record. Returns bytes written to `out`.
pub fn build_dns_query(id: u16, name: &str, out: &mut [u8]) -> usize {
    if out.len() < 12 {
        return 0;
    }
    // DNS Header
    out[0..2].copy_from_slice(&id.to_be_bytes());
    out[2] = 0x01; // QR=0, OPCODE=0, RD=1
    out[3] = 0x00; // RA=0, Z=0, RCODE=0
    out[4..6].copy_from_slice(&1u16.to_be_bytes()); // QDCOUNT=1
    out[6..8].copy_from_slice(&0u16.to_be_bytes()); // ANCOUNT=0
    out[8..10].copy_from_slice(&0u16.to_be_bytes()); // NSCOUNT=0
    out[10..12].copy_from_slice(&0u16.to_be_bytes()); // ARCOUNT=0

    let mut pos = 12;
    // Encode name as DNS labels
    for label in name.split('.') {
        let label_bytes = label.as_bytes();
        let label_len = label_bytes.len();
        if label_len == 0 || label_len > 63 {
            continue;
        }
        if pos + 1 + label_len > out.len() {
            return 0;
        }
        out[pos] = label_len as u8;
        pos += 1;
        out[pos..pos + label_len].copy_from_slice(label_bytes);
        pos += label_len;
    }
    // Terminating zero
    if pos >= out.len() {
        return 0;
    }
    out[pos] = 0x00;
    pos += 1;

    // QTYPE=A (1), QCLASS=IN (1)
    if pos + 4 > out.len() {
        return 0;
    }
    out[pos..pos + 2].copy_from_slice(&1u16.to_be_bytes()); // QTYPE=A
    pos += 2;
    out[pos..pos + 2].copy_from_slice(&1u16.to_be_bytes()); // QCLASS=IN
    pos += 2;

    pos
}

/// Skip a DNS name (handles compression pointers). Returns new offset or None.
fn skip_dns_name(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        if pos >= data.len() {
            return None;
        }
        let b = data[pos];
        if b == 0 {
            return Some(pos + 1);
        } else if b & 0xC0 == 0xC0 {
            // Compression pointer: 2 bytes
            if pos + 1 >= data.len() {
                return None;
            }
            return Some(pos + 2);
        } else {
            // Label: skip length + label bytes
            let label_len = (b & 0x3F) as usize;
            pos += 1 + label_len;
        }
    }
}

/// Parse a DNS response. Returns the first A record or None.
pub fn parse_dns_response(data: &[u8]) -> Option<DnsResponse> {
    if data.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([data[0], data[1]]);
    let flags = u16::from_be_bytes([data[2], data[3]]);
    // QR must be 1 (response), RCODE must be 0
    if flags & 0x8000 == 0 {
        return None; // Not a response
    }
    if flags & 0x000F != 0 {
        return None; // RCODE != 0
    }
    let qdcount = u16::from_be_bytes([data[4], data[5]]) as usize;
    let ancount = u16::from_be_bytes([data[6], data[7]]) as usize;

    let mut pos = 12;

    // Skip question section
    for _ in 0..qdcount {
        pos = skip_dns_name(data, pos)?;
        pos += 4; // QTYPE + QCLASS
        if pos > data.len() {
            return None;
        }
    }

    // Parse answer section
    for _ in 0..ancount {
        // Skip name
        pos = skip_dns_name(data, pos)?;
        if pos + 10 > data.len() {
            return None;
        }
        let rtype = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let rclass = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let ttl = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]);
        let rdlength = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlength > data.len() {
            return None;
        }
        // QTYPE=A (1), QCLASS=IN (1), RDLENGTH=4
        if rtype == 1 && rclass == 1 && rdlength == 4 {
            let ip = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            return Some(DnsResponse { id, ip, ttl });
        }
        pos += rdlength;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_dns_query_basic() {
        let mut buf = [0u8; 64];
        let len = build_dns_query(0x1234, "example.com", &mut buf);
        assert!(len > 12);
        // Check header
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 0x1234);
        assert_eq!(buf[2], 0x01); // RD=1
        assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 1); // QDCOUNT=1
    }

    #[test]
    fn dns_cache_insert_lookup() {
        let mut cache = DnsCache::new();
        let ip = u32::from_be_bytes([1, 2, 3, 4]);
        cache.insert("example.com", ip, 300, 0);
        assert_eq!(cache.lookup("example.com", 0), Some(ip));
        assert_eq!(cache.lookup("example.com", 30001), None); // expired
        assert_eq!(cache.lookup("other.com", 0), None);
    }
}
