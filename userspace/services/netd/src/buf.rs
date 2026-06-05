/// A single packet buffer (Ethernet MTU + header)
pub struct PacketBuf {
    pub data: [u8; 1528],
    pub len: usize,
    pub in_use: bool,
}

impl PacketBuf {
    const fn new() -> Self {
        Self {
            data: [0u8; 1528],
            len: 0,
            in_use: false,
        }
    }
}

/// Pool of 32 packet buffers for zero-allocation hot path
pub struct BufPool {
    bufs: [PacketBuf; 32],
}

impl BufPool {
    pub const fn new() -> Self {
        // Can't use array init with non-Copy in const, use manual init
        Self {
            bufs: [
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
                PacketBuf::new(),
            ],
        }
    }

    /// Allocate a buffer from the pool. Returns the index, or None if full.
    pub fn alloc(&mut self) -> Option<usize> {
        for (i, buf) in self.bufs.iter_mut().enumerate() {
            if !buf.in_use {
                buf.in_use = true;
                buf.len = 0;
                return Some(i);
            }
        }
        None
    }

    /// Free a buffer back to the pool.
    pub fn free(&mut self, idx: usize) {
        if idx < 32 {
            self.bufs[idx].in_use = false;
            self.bufs[idx].len = 0;
        }
    }

    /// Get a reference to a buffer by index.
    pub fn get(&self, idx: usize) -> Option<&PacketBuf> {
        if idx < 32 {
            Some(&self.bufs[idx])
        } else {
            None
        }
    }

    /// Get a mutable reference to a buffer by index.
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut PacketBuf> {
        if idx < 32 {
            Some(&mut self.bufs[idx])
        } else {
            None
        }
    }
}
