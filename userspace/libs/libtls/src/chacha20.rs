// ChaCha20 stream cipher — RFC 8439 IETF variant
// 96-bit nonce, 32-bit counter, 256-bit key.

/// ChaCha20 stream cipher state.
pub struct ChaCha20 {
    state: [u32; 16],
    keystream: [u8; 64],
    pos: usize,
}

// Constants: "expand 32-byte k"
const C0: u32 = 0x61707865;
const C1: u32 = 0x3320646e;
const C2: u32 = 0x79622d32;
const C3: u32 = 0x6b206574;

/// ChaCha20 quarter-round on indices `a,b,c,d` of `w`.
#[inline(always)]
fn quarter_round(w: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    w[a] = w[a].wrapping_add(w[b]);
    w[d] ^= w[a];
    w[d] = w[d].rotate_left(16);
    w[c] = w[c].wrapping_add(w[d]);
    w[b] ^= w[c];
    w[b] = w[b].rotate_left(12);
    w[a] = w[a].wrapping_add(w[b]);
    w[d] ^= w[a];
    w[d] = w[d].rotate_left(8);
    w[c] = w[c].wrapping_add(w[d]);
    w[b] ^= w[c];
    w[b] = w[b].rotate_left(7);
}

/// Produce one 64-byte ChaCha20 block from `state` into `out`.
fn chacha20_block(state: &[u32; 16], out: &mut [u8; 64]) {
    let mut w = *state;

    // 20 rounds = 10 double-rounds
    for _ in 0..10 {
        // Column rounds
        quarter_round(&mut w, 0, 4, 8, 12);
        quarter_round(&mut w, 1, 5, 9, 13);
        quarter_round(&mut w, 2, 6, 10, 14);
        quarter_round(&mut w, 3, 7, 11, 15);
        // Diagonal rounds
        quarter_round(&mut w, 0, 5, 10, 15);
        quarter_round(&mut w, 1, 6, 11, 12);
        quarter_round(&mut w, 2, 7, 8, 13);
        quarter_round(&mut w, 3, 4, 9, 14);
    }

    // Add original state
    for i in 0..16 {
        w[i] = w[i].wrapping_add(state[i]);
    }

    // Serialize little-endian
    for i in 0..16 {
        let bytes = w[i].to_le_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&bytes);
    }
}

impl ChaCha20 {
    /// Create a new ChaCha20 instance.
    ///
    /// * `key`     – 256-bit key
    /// * `nonce`   – 96-bit nonce
    /// * `counter` – initial 32-bit block counter (typically 0 or 1)
    pub fn new(key: &[u8; 32], nonce: &[u8; 12], counter: u32) -> Self {
        let mut state = [0u32; 16];

        // Constants
        state[0] = C0;
        state[1] = C1;
        state[2] = C2;
        state[3] = C3;

        // Key (words 4-11, little-endian)
        for i in 0..8 {
            state[4 + i] = u32::from_le_bytes(key[i * 4..i * 4 + 4].try_into().unwrap());
        }

        // Counter (word 12)
        state[12] = counter;

        // Nonce (words 13-15, little-endian)
        state[13] = u32::from_le_bytes(nonce[0..4].try_into().unwrap());
        state[14] = u32::from_le_bytes(nonce[4..8].try_into().unwrap());
        state[15] = u32::from_le_bytes(nonce[8..12].try_into().unwrap());

        // Pre-generate the first block
        let mut keystream = [0u8; 64];
        chacha20_block(&state, &mut keystream);

        ChaCha20 {
            state,
            keystream,
            pos: 0,
        }
    }

    /// XOR `data` with the keystream in-place, generating new blocks as needed.
    pub fn apply_keystream(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            if self.pos == 64 {
                // Advance counter and generate next block
                self.state[12] = self.state[12].wrapping_add(1);
                chacha20_block(&self.state, &mut self.keystream);
                self.pos = 0;
            }
            *byte ^= self.keystream[self.pos];
            self.pos += 1;
        }
    }
}

/// One-shot ChaCha20: encrypt/decrypt `data` in-place.
///
/// Uses the specified `counter` as the starting block counter.
pub fn chacha20_xor(key: &[u8; 32], nonce: &[u8; 12], counter: u32, data: &mut [u8]) {
    let mut cipher = ChaCha20::new(key, nonce, counter);
    cipher.apply_keystream(data);
}
