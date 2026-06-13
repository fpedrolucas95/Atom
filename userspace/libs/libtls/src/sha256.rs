// SHA-256 — FIPS 180-4
// no_std + alloc compatible; no external dependencies.

/// Round constants K[0..64]: first 32 bits of the fractional parts
/// of the cube roots of the first 64 prime numbers.
#[rustfmt::skip]
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
    0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
    0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
    0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Initial hash values H[0..8]: first 32 bits of the fractional parts
/// of the square roots of the first 8 prime numbers.
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

// --- Bitwise functions (FIPS 180-4 §4.1.2) ---

#[inline(always)]
fn ch(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (!x & z)
}

#[inline(always)]
fn maj(x: u32, y: u32, z: u32) -> u32 {
    (x & y) ^ (x & z) ^ (y & z)
}

/// Σ0 — big sigma 0
#[inline(always)]
fn big_sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

/// Σ1 — big sigma 1
#[inline(always)]
fn big_sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

/// σ0 — small sigma 0
#[inline(always)]
fn small_sigma0(x: u32) -> u32 {
    x.rotate_right(7) ^ x.rotate_right(18) ^ (x >> 3)
}

/// σ1 — small sigma 1
#[inline(always)]
fn small_sigma1(x: u32) -> u32 {
    x.rotate_right(17) ^ x.rotate_right(19) ^ (x >> 10)
}

// --- Block compression ---

/// Process one 64-byte block and update the state.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    // Prepare the message schedule W[0..64].
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[4 * i],
            block[4 * i + 1],
            block[4 * i + 2],
            block[4 * i + 3],
        ]);
    }
    for i in 16..64 {
        w[i] = small_sigma1(w[i - 2])
            .wrapping_add(w[i - 7])
            .wrapping_add(small_sigma0(w[i - 15]))
            .wrapping_add(w[i - 16]);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;

    for i in 0..64 {
        let t1 = h
            .wrapping_add(big_sigma1(e))
            .wrapping_add(ch(e, f, g))
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let t2 = big_sigma0(a).wrapping_add(maj(a, b, c));
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

// --- Public API ---

/// Incremental SHA-256 hasher.
#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    /// Partial block buffer.
    buf: [u8; 64],
    /// Number of bytes currently in `buf`.
    buf_len: usize,
    /// Total number of bytes processed so far (before the current partial block).
    total_len: u64,
}

impl Sha256 {
    /// Create a new hasher in the initial state.
    pub fn new() -> Self {
        Sha256 {
            state: H0,
            buf: [0u8; 64],
            buf_len: 0,
            total_len: 0,
        }
    }

    /// Feed data into the hasher.
    pub fn update(&mut self, mut data: &[u8]) {
        // If there is already a partial block, try to fill it first.
        if self.buf_len > 0 {
            let need = 64 - self.buf_len;
            let take = need.min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];

            if self.buf_len == 64 {
                let block = self.buf; // copy to satisfy borrow checker
                compress(&mut self.state, &block);
                self.total_len += 64;
                self.buf_len = 0;
            }
        }

        // Process all full blocks directly from `data`.
        while data.len() >= 64 {
            let block: [u8; 64] = data[..64].try_into().unwrap();
            compress(&mut self.state, &block);
            self.total_len += 64;
            data = &data[64..];
        }

        // Buffer any remaining bytes.
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    /// Consume the hasher and produce the digest.
    pub fn finalize(mut self) -> [u8; 32] {
        // Total message length in bits.
        let bit_len: u64 = (self.total_len + self.buf_len as u64) * 8;

        // Append the mandatory 0x80 byte.
        self.buf[self.buf_len] = 0x80;
        self.buf_len += 1;

        // If there is not enough room for the 8-byte length field, pad with
        // zeros and compress this block first.
        if self.buf_len > 56 {
            // Zero out the rest of the current block.
            for b in &mut self.buf[self.buf_len..] {
                *b = 0;
            }
            let block = self.buf;
            compress(&mut self.state, &block);
            // Reset buffer for the final block.
            self.buf = [0u8; 64];
            self.buf_len = 0;
        }

        // Zero out bytes between buf_len and byte 56.
        for b in &mut self.buf[self.buf_len..56] {
            *b = 0;
        }

        // Append the 64-bit big-endian bit length at bytes 56..64.
        self.buf[56..64].copy_from_slice(&bit_len.to_be_bytes());

        let block = self.buf;
        compress(&mut self.state, &block);

        // Produce the big-endian digest.
        let mut out = [0u8; 32];
        for (i, &word) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a single contiguous slice.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize()
}

/// Hash two slices as if they were concatenated.
pub fn sha256_two(a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(a);
    h.update(b);
    h.finalize()
}
