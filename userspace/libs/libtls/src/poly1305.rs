// Poly1305 one-time MAC — RFC 8439
//
// Uses five 26-bit limbs (stored in u32) for the 130-bit accumulator and r.
// u64 intermediates prevent overflow during multiplication.

/// Compute the Poly1305 MAC of `msg` under `key`.
///
/// `key` must be a 32-byte one-time key:
///   r = key[0..16]  (clamped)
///   s = key[16..32]
///
/// Returns a 16-byte tag.
pub fn poly1305_mac(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    // -----------------------------------------------------------------------
    // 1. Parse and clamp r
    // -----------------------------------------------------------------------
    let mut r_bytes = [0u8; 16];
    r_bytes.copy_from_slice(&key[0..16]);

    // Clamping mask (RFC 8439 §2.5)
    r_bytes[3] &= 0x0f;
    r_bytes[7] &= 0x0f;
    r_bytes[11] &= 0x0f;
    r_bytes[15] &= 0x0f;
    r_bytes[4] &= 0xfc;
    r_bytes[8] &= 0xfc;
    r_bytes[12] &= 0xfc;

    // Decode r into five 26-bit limbs (little-endian bit layout).
    // Each limb overlaps the previous by 6 bits (stride = 26 bits = 3 bytes + 2 bits).
    // We read 4-byte LE windows at byte offsets 0, 3, 6, 9, 12 and shift/mask.
    let r0 = (u32::from_le_bytes(r_bytes[0..4].try_into().unwrap())) & 0x3ff_ffff;
    let r1 = (u32::from_le_bytes(r_bytes[3..7].try_into().unwrap()) >> 2) & 0x3ff_ffff;
    let r2 = (u32::from_le_bytes(r_bytes[6..10].try_into().unwrap()) >> 4) & 0x3ff_ffff;
    let r3 = (u32::from_le_bytes(r_bytes[9..13].try_into().unwrap()) >> 6) & 0x3ff_ffff;
    let r4 = (u32::from_le_bytes(r_bytes[12..16].try_into().unwrap()) >> 8) & 0x3ff_ffff;

    // Pre-compute 5*r[i] for the wrap-around multiplication terms.
    // After clamping, r fits in 124 bits; r4 covers bits [104..130) and can
    // be non-zero (bytes 13-14 are unclamped, byte 15 retains its low nibble).
    let r1_5 = r1 * 5;
    let r2_5 = r2 * 5;
    let r3_5 = r3 * 5;
    let r4_5 = r4 * 5;

    // -----------------------------------------------------------------------
    // 2. Parse s (16 bytes, added mod 2^128 at the end)
    // -----------------------------------------------------------------------
    let s0 = u32::from_le_bytes(key[16..20].try_into().unwrap());
    let s1 = u32::from_le_bytes(key[20..24].try_into().unwrap());
    let s2 = u32::from_le_bytes(key[24..28].try_into().unwrap());
    let s3 = u32::from_le_bytes(key[28..32].try_into().unwrap());

    // -----------------------------------------------------------------------
    // 3. Accumulator h (five 26-bit limbs, initially zero)
    // -----------------------------------------------------------------------
    let mut h0: u32 = 0;
    let mut h1: u32 = 0;
    let mut h2: u32 = 0;
    let mut h3: u32 = 0;
    let mut h4: u32 = 0;

    // -----------------------------------------------------------------------
    // 4. Process 16-byte (or final partial) blocks
    // -----------------------------------------------------------------------
    let mut remaining = msg;
    while !remaining.is_empty() {
        // Take up to 16 bytes
        let chunk_len = remaining.len().min(16);
        let chunk = &remaining[..chunk_len];
        remaining = &remaining[chunk_len..];

        // Build a 17-byte buffer: copy chunk, then set byte at position
        // `chunk_len` to 0x01 (sets the "high bit" marker per RFC 8439 §2.5.1:
        // bit 128 for a full 16-byte block, or the next bit after actual data
        // for a partial block).
        let mut buf = [0u8; 17];
        buf[..chunk_len].copy_from_slice(chunk);
        buf[chunk_len] = 0x01;

        // Decode message block into five 26-bit limbs (same layout as r)
        let m0 = (u32::from_le_bytes(buf[0..4].try_into().unwrap())) & 0x3ff_ffff;
        let m1 = (u32::from_le_bytes(buf[3..7].try_into().unwrap()) >> 2) & 0x3ff_ffff;
        let m2 = (u32::from_le_bytes(buf[6..10].try_into().unwrap()) >> 4) & 0x3ff_ffff;
        let m3 = (u32::from_le_bytes(buf[9..13].try_into().unwrap()) >> 6) & 0x3ff_ffff;
        // m4 holds bits [104..130]: lower bits from the packed window, high bits
        // from buf[16] (0x00 or 0x01).
        let m4 =
            (u32::from_le_bytes(buf[12..16].try_into().unwrap()) >> 8) | ((buf[16] as u32) << 24);

        // h = h + m
        h0 = h0.wrapping_add(m0);
        h1 = h1.wrapping_add(m1);
        h2 = h2.wrapping_add(m2);
        h3 = h3.wrapping_add(m3);
        h4 = h4.wrapping_add(m4);

        // h = h * r  (mod 2^130 - 5)
        //
        // Full 5-limb product reduced mod 2^130-5 using 2^130 ≡ 5 (mod 2^130-5).
        // For any product term h[j]*r[k] where j+k >= 5, we instead add
        // h[j] * (5 * r[k - 5 + 5]) = h[j] * 5 * r[(j+k) mod 5], which
        // corresponds to the pre-multiplied r?_5 values.
        //
        // d[i] = Σ_j  h[j] * r_eff[(i - j) mod 5]
        // where r_eff[k] = r[k] for k < 5 for the "un-wrapped" part, and
        // the "wrapped" part uses 5*r[k] to account for 2^130 ≡ 5.
        let d0 = (h0 as u64) * (r0 as u64)
            + (h1 as u64) * (r4_5 as u64)
            + (h2 as u64) * (r3_5 as u64)
            + (h3 as u64) * (r2_5 as u64)
            + (h4 as u64) * (r1_5 as u64);

        let d1 = (h0 as u64) * (r1 as u64)
            + (h1 as u64) * (r0 as u64)
            + (h2 as u64) * (r4_5 as u64)
            + (h3 as u64) * (r3_5 as u64)
            + (h4 as u64) * (r2_5 as u64);

        let d2 = (h0 as u64) * (r2 as u64)
            + (h1 as u64) * (r1 as u64)
            + (h2 as u64) * (r0 as u64)
            + (h3 as u64) * (r4_5 as u64)
            + (h4 as u64) * (r3_5 as u64);

        let d3 = (h0 as u64) * (r3 as u64)
            + (h1 as u64) * (r2 as u64)
            + (h2 as u64) * (r1 as u64)
            + (h3 as u64) * (r0 as u64)
            + (h4 as u64) * (r4_5 as u64);

        let d4 = (h0 as u64) * (r4 as u64)
            + (h1 as u64) * (r3 as u64)
            + (h2 as u64) * (r2 as u64)
            + (h3 as u64) * (r1 as u64)
            + (h4 as u64) * (r0 as u64);

        // Propagate carries (each limb is 26 bits wide)
        let c0 = d0 >> 26;
        h0 = (d0 as u32) & 0x3ff_ffff;

        let d1 = d1 + c0;
        let c1 = d1 >> 26;
        h1 = (d1 as u32) & 0x3ff_ffff;

        let d2 = d2 + c1;
        let c2 = d2 >> 26;
        h2 = (d2 as u32) & 0x3ff_ffff;

        let d3 = d3 + c2;
        let c3 = d3 >> 26;
        h3 = (d3 as u32) & 0x3ff_ffff;

        let d4 = d4 + c3;
        let c4 = d4 >> 26;
        h4 = (d4 as u32) & 0x3ff_ffff;

        // Wrap the carry out of limb 4: 2^130 ≡ 5 (mod 2^130-5)
        h0 = h0.wrapping_add((c4 as u32) * 5);
        let c = h0 >> 26;
        h0 &= 0x3ff_ffff;
        h1 = h1.wrapping_add(c);
    }

    // -----------------------------------------------------------------------
    // 5. Fully reduce h mod 2^130-5
    // -----------------------------------------------------------------------
    // Propagate any residual carry through all five limbs (two passes guarantee
    // the limbs are fully reduced to [0, 2^26)).
    let c = h1 >> 26;
    h1 &= 0x3ff_ffff;
    h2 = h2.wrapping_add(c);
    let c = h2 >> 26;
    h2 &= 0x3ff_ffff;
    h3 = h3.wrapping_add(c);
    let c = h3 >> 26;
    h3 &= 0x3ff_ffff;
    h4 = h4.wrapping_add(c);
    let c = h4 >> 26;
    h4 &= 0x3ff_ffff;
    h0 = h0.wrapping_add(c * 5);
    let c = h0 >> 26;
    h0 &= 0x3ff_ffff;
    h1 = h1.wrapping_add(c);

    // Conditional subtract: compute g = h + 5.  If g does not overflow the
    // 130-bit field (i.e. g4 >= 2^26, meaning we had a carry out), then
    // h >= 2^130-5 and we should use g = h - (2^130-5) instead of h.
    let mut g0 = h0.wrapping_add(5);
    let c = g0 >> 26;
    g0 &= 0x3ff_ffff;
    let mut g1 = h1.wrapping_add(c);
    let c = g1 >> 26;
    g1 &= 0x3ff_ffff;
    let mut g2 = h2.wrapping_add(c);
    let c = g2 >> 26;
    g2 &= 0x3ff_ffff;
    let mut g3 = h3.wrapping_add(c);
    let c = g3 >> 26;
    g3 &= 0x3ff_ffff;
    let mut g4 = h4.wrapping_add(c);

    // After adding 5 and propagating, a carry out of g4's 26-bit field
    // (g4 >= 2^26) signals that h >= 2^130-5, so we use g.
    // mask = 0x0000_0000 when carry (use g), 0xffff_ffff when no carry (use h).
    let mask = (g4 >> 26).wrapping_sub(1);
    g4 &= 0x3ff_ffff;

    h0 = (h0 & mask) | (g0 & !mask);
    h1 = (h1 & mask) | (g1 & !mask);
    h2 = (h2 & mask) | (g2 & !mask);
    h3 = (h3 & mask) | (g3 & !mask);
    h4 = (h4 & mask) | (g4 & !mask);

    // -----------------------------------------------------------------------
    // 6. Reconstruct 128-bit integer from five 26-bit limbs
    // -----------------------------------------------------------------------
    // Bit layout: h0 covers bits [0..26), h1 [26..52), h2 [52..78),
    //             h3 [78..104), h4 [104..130) — but we only emit bits [0..128).
    //
    // word0 = bits [ 0.. 32) = h0[0..26] | h1[0..6]  shifted
    // word1 = bits [32.. 64) = h1[6..26] | h2[0..20] shifted
    // word2 = bits [64.. 96) = h2[12..26] | h3[0..18] shifted  (h2>>12, h3<<14)
    //   wait: bits 64-78 = h2[12..26], bits 78-96 = h3[0..18]  → (h2>>12)|(h3<<14) ✓
    // word3 = bits [96..128) = h3[18..26] | h4[0..24] shifted  (h3>>18, h4<<8)
    let word0 = h0 | (h1 << 26);
    let word1 = (h1 >> 6) | (h2 << 20);
    let word2 = (h2 >> 12) | (h3 << 14);
    let word3 = (h3 >> 18) | (h4 << 8);

    // -----------------------------------------------------------------------
    // 7. Add s (mod 2^128) and serialize
    // -----------------------------------------------------------------------
    // Use overflowing_add for each word so carries chain correctly even when
    // sN is close to u32::MAX.
    let (w0, c0) = word0.overflowing_add(s0);
    let (w1a, c1a) = word1.overflowing_add(s1);
    let (w1, c1b) = w1a.overflowing_add(c0 as u32);
    let (w2a, c2a) = word2.overflowing_add(s2);
    let (w2, c2b) = w2a.overflowing_add((c1a | c1b) as u32);
    let (w3a, _) = word3.overflowing_add(s3);
    let (w3, _) = w3a.overflowing_add((c2a | c2b) as u32);

    let mut tag = [0u8; 16];
    tag[0..4].copy_from_slice(&w0.to_le_bytes());
    tag[4..8].copy_from_slice(&w1.to_le_bytes());
    tag[8..12].copy_from_slice(&w2.to_le_bytes());
    tag[12..16].copy_from_slice(&w3.to_le_bytes());
    tag
}
