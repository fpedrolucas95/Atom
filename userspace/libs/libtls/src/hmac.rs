// HMAC-SHA-256 — RFC 2104
// no_std + alloc compatible; depends only on the sibling sha256 module.

use super::sha256::{sha256, Sha256};

const BLOCK_SIZE: usize = 64; // SHA-256 block size in bytes
const IPAD: u8 = 0x36;
const OPAD: u8 = 0x5C;

/// Derive the 64-byte padded key from a raw key.
///
/// If `key` is longer than the block size it is hashed first (RFC 2104 §2).
fn prepare_key(key: &[u8]) -> [u8; BLOCK_SIZE] {
    let mut k = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let digest = sha256(key);
        k[..32].copy_from_slice(&digest);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    k
}

/// HMAC-SHA-256 over a single message slice.
///
/// Returns the 32-byte MAC.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let k = prepare_key(key);

    // Build ipad-XORed key.
    let mut ikey = [0u8; BLOCK_SIZE];
    let mut okey = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ikey[i] = k[i] ^ IPAD;
        okey[i] = k[i] ^ OPAD;
    }

    // Inner hash: H(K XOR ipad || msg)
    let mut inner = Sha256::new();
    inner.update(&ikey);
    inner.update(msg);
    let inner_digest = inner.finalize();

    // Outer hash: H(K XOR opad || inner_digest)
    let mut outer = Sha256::new();
    outer.update(&okey);
    outer.update(&inner_digest);
    outer.finalize()
}

/// HMAC-SHA-256 over two message slices treated as a single concatenated message.
///
/// This avoids an allocation when the caller has two separate buffers.
pub fn hmac_sha256_two(key: &[u8], a: &[u8], b: &[u8]) -> [u8; 32] {
    let k = prepare_key(key);

    let mut ikey = [0u8; BLOCK_SIZE];
    let mut okey = [0u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ikey[i] = k[i] ^ IPAD;
        okey[i] = k[i] ^ OPAD;
    }

    // Inner hash: H(K XOR ipad || a || b)
    let mut inner = Sha256::new();
    inner.update(&ikey);
    inner.update(a);
    inner.update(b);
    let inner_digest = inner.finalize();

    // Outer hash: H(K XOR opad || inner_digest)
    let mut outer = Sha256::new();
    outer.update(&okey);
    outer.update(&inner_digest);
    outer.finalize()
}
