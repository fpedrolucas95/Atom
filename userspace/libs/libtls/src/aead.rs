// ChaCha20-Poly1305 AEAD — RFC 8439
//
// Depends on `chacha20` and `poly1305` modules in the same crate.

use alloc::vec::Vec;
use crate::chacha20::chacha20_xor;
use crate::poly1305::poly1305_mac;

/// Errors returned by AEAD operations.
#[derive(Debug)]
pub enum AeadError {
    /// Authentication tag verification failed.
    AuthFailed,
}

// -----------------------------------------------------------------------
// Internal helpers
// -----------------------------------------------------------------------

/// Generate the 32-byte Poly1305 one-time key using ChaCha20 with counter=0.
///
/// Per RFC 8439 §2.6: encrypt 64 zero bytes and take the first 32.
fn poly1305_key_gen(key: &[u8; 32], nonce: &[u8; 12]) -> [u8; 32] {
    let mut block = [0u8; 64];
    chacha20_xor(key, nonce, 0, &mut block);
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&block[0..32]);
    otk
}

/// Number of padding bytes needed to bring `len` up to the next 16-byte boundary.
/// Returns 0 if `len` is already a multiple of 16.
#[inline]
fn pad16_len(len: usize) -> usize {
    let rem = len % 16;
    if rem == 0 { 0 } else { 16 - rem }
}

/// Build the Poly1305 authentication data per RFC 8439 §2.8:
///
///   aad || pad16(aad) || ciphertext || pad16(ciphertext)
///   || LE64(aad.len()) || LE64(ciphertext.len())
fn build_mac_input(aad: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let aad_pad = pad16_len(aad.len());
    let ct_pad  = pad16_len(ciphertext.len());
    let total   = aad.len() + aad_pad + ciphertext.len() + ct_pad + 8 + 8;

    let mut buf = Vec::with_capacity(total);

    buf.extend_from_slice(aad);
    for _ in 0..aad_pad { buf.push(0); }

    buf.extend_from_slice(ciphertext);
    for _ in 0..ct_pad { buf.push(0); }

    buf.extend_from_slice(&(aad.len()        as u64).to_le_bytes());
    buf.extend_from_slice(&(ciphertext.len() as u64).to_le_bytes());

    buf
}

/// Constant-time equality for two equal-length byte slices.
#[inline]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// -----------------------------------------------------------------------
// Public API
// -----------------------------------------------------------------------

/// Encrypt `plaintext` with ChaCha20-Poly1305, authenticating `aad`.
///
/// Returns `ciphertext || tag` where `tag` is 16 bytes.
///
/// * The Poly1305 one-time key is derived from ChaCha20 with counter = 0.
/// * Encryption uses ChaCha20 with counter = 1.
/// * The MAC covers: aad || pad16(aad) || ciphertext || pad16(ciphertext)
///   || LE64(len(aad)) || LE64(len(ciphertext)).
pub fn chacha20_poly1305_seal(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    plaintext: &[u8],
) -> Vec<u8> {
    // 1. Derive Poly1305 one-time key (ChaCha20 block 0)
    let otk = poly1305_key_gen(key, nonce);

    // 2. Encrypt plaintext with ChaCha20 counter = 1
    let mut ciphertext = Vec::with_capacity(plaintext.len() + 16);
    ciphertext.extend_from_slice(plaintext);
    chacha20_xor(key, nonce, 1, &mut ciphertext);

    // 3. Compute Poly1305 tag over aad + padded ciphertext + lengths
    let mac_data = build_mac_input(aad, &ciphertext);
    let tag = poly1305_mac(&otk, &mac_data);

    // 4. Append tag and return
    ciphertext.extend_from_slice(&tag);
    ciphertext
}

/// Decrypt and authenticate a ChaCha20-Poly1305 ciphertext.
///
/// `ciphertext_with_tag` must be at least 16 bytes (the tag alone).
/// Returns the plaintext on success, or `AeadError::AuthFailed` if the
/// Poly1305 tag does not match (or the input is too short).
///
/// Authentication is verified **before** decryption to avoid releasing
/// unauthenticated plaintext.
pub fn chacha20_poly1305_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    ciphertext_with_tag: &[u8],
) -> Result<Vec<u8>, AeadError> {
    if ciphertext_with_tag.len() < 16 {
        return Err(AeadError::AuthFailed);
    }

    let (ciphertext, tag) = ciphertext_with_tag.split_at(ciphertext_with_tag.len() - 16);

    // 1. Derive Poly1305 one-time key (ChaCha20 block 0)
    let otk = poly1305_key_gen(key, nonce);

    // 2. Verify tag BEFORE decrypting (authenticate-then-decrypt)
    let mac_data     = build_mac_input(aad, ciphertext);
    let expected_tag = poly1305_mac(&otk, &mac_data);

    if !constant_time_eq(&expected_tag, tag) {
        return Err(AeadError::AuthFailed);
    }

    // 3. Decrypt with ChaCha20 counter = 1
    let mut plaintext = Vec::with_capacity(ciphertext.len());
    plaintext.extend_from_slice(ciphertext);
    chacha20_xor(key, nonce, 1, &mut plaintext);

    Ok(plaintext)
}
