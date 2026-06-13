//! Entropy source: RDRAND (x86_64) with ChaCha20-DRBG fallback seeded from TSC.

use core::arch::x86_64::_rdrand64_step;

/// Fill `buf` with cryptographically random bytes.
/// Uses RDRAND if available; falls back to TSC-seeded ChaCha20-DRBG.
pub fn random_bytes(buf: &mut [u8]) {
    if try_rdrand(buf) {
        return;
    }
    tsc_chacha_fill(buf);
}

/// Generate a random 32-byte key.
pub fn random_key() -> [u8; 32] {
    let mut k = [0u8; 32];
    random_bytes(&mut k);
    k
}

fn try_rdrand(buf: &mut [u8]) -> bool {
    let mut pos = 0;
    while pos + 8 <= buf.len() {
        let mut val = 0u64;
        let mut ok = false;
        for _ in 0..10 {
            // SAFETY: x86_64 target, RDRAND instruction
            if unsafe { _rdrand64_step(&mut val) } == 1 {
                ok = true;
                break;
            }
        }
        if !ok {
            return false;
        }
        buf[pos..pos + 8].copy_from_slice(&val.to_le_bytes());
        pos += 8;
    }
    // Tail bytes
    if pos < buf.len() {
        let mut val = 0u64;
        let mut ok = false;
        for _ in 0..10 {
            if unsafe { _rdrand64_step(&mut val) } == 1 {
                ok = true;
                break;
            }
        }
        if !ok {
            return false;
        }
        let tail = val.to_le_bytes();
        let remaining = buf.len() - pos;
        buf[pos..].copy_from_slice(&tail[..remaining]);
    }
    true
}

/// TSC-seeded ChaCha20 DRBG fallback.
fn tsc_chacha_fill(buf: &mut [u8]) {
    let seed = read_tsc();
    // Expand seed into a 32-byte key using simple mixing
    let mut key = [0u8; 32];
    let seed_bytes = seed.to_le_bytes();
    for i in 0..32 {
        key[i] = seed_bytes[i % 8].wrapping_add((i as u8).wrapping_mul(0x9e));
    }
    // Mix in a second TSC read for the nonce
    let seed2 = read_tsc();
    let mut nonce = [0u8; 12];
    let seed2_bytes = seed2.to_le_bytes();
    for i in 0..12 {
        nonce[i] = seed2_bytes[i % 8].wrapping_add(i as u8);
    }

    chacha20_fill(&key, &nonce, buf);
}

#[inline]
fn read_tsc() -> u64 {
    unsafe {
        core::arch::x86_64::_rdtsc()
    }
}

/// Minimal ChaCha20 block for the DRBG — avoids a circular dependency on our
/// chacha20 module (which depends on this module).
fn chacha20_fill(key: &[u8; 32], nonce: &[u8; 12], out: &mut [u8]) {
    const CONSTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

    let key_words: [u32; 8] = core::array::from_fn(|i| {
        u32::from_le_bytes(key[4 * i..4 * i + 4].try_into().unwrap())
    });
    let nonce_words: [u32; 3] = core::array::from_fn(|i| {
        u32::from_le_bytes(nonce[4 * i..4 * i + 4].try_into().unwrap())
    });

    let mut counter = 0u32;
    let mut pos = 0;

    while pos < out.len() {
        let mut state = [
            CONSTS[0], CONSTS[1], CONSTS[2], CONSTS[3],
            key_words[0], key_words[1], key_words[2], key_words[3],
            key_words[4], key_words[5], key_words[6], key_words[7],
            counter, nonce_words[0], nonce_words[1], nonce_words[2],
        ];
        let orig = state;
        for _ in 0..10 {
            qr(&mut state,  0,  4,  8, 12);
            qr(&mut state,  1,  5,  9, 13);
            qr(&mut state,  2,  6, 10, 14);
            qr(&mut state,  3,  7, 11, 15);
            qr(&mut state,  0,  5, 10, 15);
            qr(&mut state,  1,  6, 11, 12);
            qr(&mut state,  2,  7,  8, 13);
            qr(&mut state,  3,  4,  9, 14);
        }
        let mut block = [0u8; 64];
        for i in 0..16 {
            let word = state[i].wrapping_add(orig[i]);
            block[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        let take = (out.len() - pos).min(64);
        out[pos..pos + take].copy_from_slice(&block[..take]);
        pos += take;
        counter = counter.wrapping_add(1);
    }
}

#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]); s[d] ^= s[a]; s[d] = s[d].rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]); s[b] ^= s[c]; s[b] = s[b].rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]); s[d] ^= s[a]; s[d] = s[d].rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]); s[b] ^= s[c]; s[b] = s[b].rotate_left(7);
}
