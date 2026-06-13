// HKDF-SHA-256 — RFC 5869
// TLS 1.3 label helpers — RFC 8446 §7.1
// no_std + alloc compatible; depends only on the sibling hmac module.

use alloc::vec::Vec;
use super::hmac::hmac_sha256;

// ---------------------------------------------------------------------------
// HKDF-Extract
// ---------------------------------------------------------------------------

/// HKDF-Extract (RFC 5869 §2.2).
///
/// `PRK = HMAC-SHA-256(salt, IKM)`
///
/// The salt acts as the HMAC key and IKM as the HMAC message.
pub fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    hmac_sha256(salt, ikm)
}

// ---------------------------------------------------------------------------
// HKDF-Expand
// ---------------------------------------------------------------------------

/// HKDF-Expand (RFC 5869 §2.3).
///
/// Produces `len` bytes of output keying material.
///
/// ```text
/// T(0) = ""   (empty)
/// T(1) = HMAC-SHA-256(PRK, T(0) || info || 0x01)
/// T(n) = HMAC-SHA-256(PRK, T(n-1) || info || n)
/// OKM  = T(1) || T(2) || ... (first `len` bytes)
/// ```
///
/// Panics if `len > 255 * 32` (RFC 5869 limit).
pub fn hkdf_expand(prk: &[u8; 32], info: &[u8], len: usize) -> Vec<u8> {
    assert!(
        len <= 255 * 32,
        "hkdf_expand: requested length exceeds RFC 5869 maximum (255 * HashLen)"
    );

    let n = (len + 31) / 32; // number of T blocks needed (ceiling division)
    let mut okm: Vec<u8> = Vec::with_capacity(n * 32);

    let mut t_prev: [u8; 32] = [0u8; 32]; // T(0) = "" represented as zeroed array
    let mut t_prev_len: usize = 0;          // 0 for the first iteration (T(0) is empty)

    for i in 1..=(n as u8) {
        // HMAC input: T(i-1) || info || i
        // We feed the parts incrementally via hmac_sha256_two for the common
        // case, but we need three parts here.  Build into a stack buffer or
        // use the incremental Sha256 + hmac approach.
        //
        // Use the three-part variant via the sha256 module directly, but since
        // hmac only exposes one- and two-part helpers we assemble the input
        // into a temporary Vec for simplicity (the length is bounded and small).

        let input_len = t_prev_len + info.len() + 1;
        let mut input: Vec<u8> = Vec::with_capacity(input_len);
        input.extend_from_slice(&t_prev[..t_prev_len]);
        input.extend_from_slice(info);
        input.push(i);

        let t_i = hmac_sha256(prk, &input);
        okm.extend_from_slice(&t_i);

        t_prev = t_i;
        t_prev_len = 32; // all subsequent T blocks are 32 bytes
    }

    okm.truncate(len);
    okm
}

// ---------------------------------------------------------------------------
// HKDF-Expand-Label  (RFC 8446 §7.1)
// ---------------------------------------------------------------------------

/// HKDF-Expand-Label (RFC 8446 §7.1).
///
/// Constructs the `HkdfLabel` structure and calls `hkdf_expand`:
///
/// ```text
/// struct {
///     uint16 length;
///     opaque label<7..255>;   // "tls13 " + label
///     opaque context<0..255>;
/// } HkdfLabel;
/// ```
pub fn hkdf_expand_label(
    secret: &[u8; 32],
    label: &str,
    context: &[u8],
    len: usize,
) -> Vec<u8> {
    // Build the HkdfLabel byte string.
    // "tls13 " prefix is 6 bytes; label may be at most 249 bytes so the
    // combined label field fits in one byte length prefix (max 255).
    let prefix = b"tls13 ";
    let full_label_len = prefix.len() + label.len();
    debug_assert!(
        full_label_len <= 255,
        "hkdf_expand_label: label too long"
    );
    debug_assert!(context.len() <= 255, "hkdf_expand_label: context too long");

    // Total HkdfLabel capacity:
    //   2 (uint16 length) + 1 (label length byte) + full_label_len
    //   + 1 (context length byte) + context.len()
    let mut hkdf_label: Vec<u8> =
        Vec::with_capacity(2 + 1 + full_label_len + 1 + context.len());

    // uint16 length — big-endian output length
    hkdf_label.push((len >> 8) as u8);
    hkdf_label.push(len as u8);

    // opaque label<7..255>: length-prefixed
    hkdf_label.push(full_label_len as u8);
    hkdf_label.extend_from_slice(prefix);
    hkdf_label.extend_from_slice(label.as_bytes());

    // opaque context<0..255>: length-prefixed
    hkdf_label.push(context.len() as u8);
    hkdf_label.extend_from_slice(context);

    hkdf_expand(secret, &hkdf_label, len)
}

// ---------------------------------------------------------------------------
// Derive-Secret  (RFC 8446 §7.1)
// ---------------------------------------------------------------------------

/// Derive-Secret (RFC 8446 §7.1).
///
/// ```text
/// Derive-Secret(Secret, Label, Messages) =
///     HKDF-Expand-Label(Secret, Label, Transcript-Hash(Messages), Hash.length)
/// ```
///
/// The caller provides the already-computed `transcript_hash` (SHA-256 of the
/// handshake transcript).  Returns exactly 32 bytes.
pub fn derive_secret(
    secret: &[u8; 32],
    label: &str,
    transcript_hash: &[u8; 32],
) -> [u8; 32] {
    let out = hkdf_expand_label(secret, label, transcript_hash, 32);
    // SAFETY: hkdf_expand_label with len=32 always returns exactly 32 bytes.
    let mut result = [0u8; 32];
    result.copy_from_slice(&out);
    result
}
