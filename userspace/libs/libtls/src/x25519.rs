//! X25519 Diffie-Hellman (RFC 7748) over Curve25519.
//!
//! Field: GF(2^255 - 19).  Arithmetic uses 5 × 51-bit limbs in u64; products
//! use u128 so every intermediate fits without overflow.

/// X25519 base point (u-coordinate = 9, LE).
pub const BASE_POINT: [u8; 32] = {
    let mut p = [0u8; 32];
    p[0] = 9;
    p
};

/// Compute X25519(scalar, point).
/// Returns the u-coordinate of `scalar * point`.
pub fn x25519(scalar: &[u8; 32], point: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    // RFC 7748 §5 clamping
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;

    let x1 = fe_from_bytes(point);
    let mut x2 = FE_ONE;
    let mut z2 = FE_ZERO;
    let mut x3 = x1;
    let mut z3 = FE_ONE;
    let mut swap: u64 = 0;

    // 255-bit ladder (bit 255 is zero after clamping)
    for bit in (0..255usize).rev() {
        let k_t = ((k[bit / 8] >> (bit % 8)) & 1) as u64;
        swap ^= k_t;
        fe_cswap(&mut x2, &mut x3, swap);
        fe_cswap(&mut z2, &mut z3, swap);
        swap = k_t;

        let a = fe_add(x2, z2); // A  = x2 + z2
        let aa = fe_sq(a); // AA = A^2
        let b = fe_sub(x2, z2); // B  = x2 - z2
        let bb = fe_sq(b); // BB = B^2
        let e = fe_sub(aa, bb); // E  = AA - BB
        let c = fe_add(x3, z3); // C  = x3 + z3
        let d = fe_sub(x3, z3); // D  = x3 - z3
        let da = fe_mul(d, a); // DA = D * A
        let cb = fe_mul(c, b); // CB = C * B
        x3 = fe_sq(fe_add(da, cb)); // x3 = (DA+CB)^2
        z3 = fe_mul(x1, fe_sq(fe_sub(da, cb))); // z3 = x1*(DA-CB)^2
        x2 = fe_mul(aa, bb); // x2 = AA * BB
                             // z2 = E*(AA + a24*E),  a24 = 121665
        z2 = fe_mul(e, fe_add(aa, fe_mul_small(e, 121665)));
    }
    fe_cswap(&mut x2, &mut x3, swap);
    fe_cswap(&mut z2, &mut z3, swap);

    fe_to_bytes(fe_mul(x2, fe_invert(z2)))
}

/// Generate an x25519 public key from a 32-byte private scalar.
pub fn x25519_public(private_key: &[u8; 32]) -> [u8; 32] {
    x25519(private_key, &BASE_POINT)
}

// ── Field arithmetic ─────────────────────────────────────────────────────────

/// Field element: value = f[0] + f[1]·2^51 + f[2]·2^102 + f[3]·2^153 + f[4]·2^204
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

const MASK51: u64 = (1u64 << 51) - 1;
const FE_ZERO: Fe = Fe([0, 0, 0, 0, 0]);
const FE_ONE: Fe = Fe([1, 0, 0, 0, 0]);

fn fe_add(a: Fe, b: Fe) -> Fe {
    Fe([
        a.0[0] + b.0[0],
        a.0[1] + b.0[1],
        a.0[2] + b.0[2],
        a.0[3] + b.0[3],
        a.0[4] + b.0[4],
    ])
}

fn fe_sub(a: Fe, b: Fe) -> Fe {
    // Add 4·p before subtracting to stay positive.
    // 4p limbs: f[0] = 4*(2^51-19) = 2^53 - 76; f[1..4] = 4*(2^51-1) = 2^53 - 4.
    // These are < 2^53 so they fit in u64.
    const P0: u64 = 4 * ((1 << 51) - 19);
    const PN: u64 = 4 * ((1u64 << 51) - 1);
    Fe([
        a.0[0].wrapping_add(P0).wrapping_sub(b.0[0]),
        a.0[1].wrapping_add(PN).wrapping_sub(b.0[1]),
        a.0[2].wrapping_add(PN).wrapping_sub(b.0[2]),
        a.0[3].wrapping_add(PN).wrapping_sub(b.0[3]),
        a.0[4].wrapping_add(PN).wrapping_sub(b.0[4]),
    ])
}

fn fe_mul(a: Fe, b: Fe) -> Fe {
    // Each limb ≤ 2^52 in practice (after reductions); products fit in u128.
    let a = a.0;
    let b = b.0;
    let mut h = [0u128; 5];
    for i in 0..5 {
        for j in 0..5 {
            let k = i + j;
            let p = a[i] as u128 * b[j] as u128;
            if k < 5 {
                h[k] += p;
            } else {
                h[k - 5] += p * 19;
            }
        }
    }
    fe_carry(h)
}

fn fe_sq(a: Fe) -> Fe {
    fe_mul(a, a)
}

fn fe_mul_small(a: Fe, s: u64) -> Fe {
    let mut h = [0u128; 5];
    for i in 0..5 {
        h[i] = a.0[i] as u128 * s as u128;
    }
    fe_carry(h)
}

fn fe_carry(h: [u128; 5]) -> Fe {
    let mut carry: u128 = 0;
    let mut f = [0u64; 5];
    for i in 0..5 {
        let v = h[i] + carry;
        f[i] = (v & MASK51 as u128) as u64;
        carry = v >> 51;
    }
    // Wrap-around carry: multiply by 19
    f[0] = f[0].wrapping_add((carry * 19) as u64);
    // One more propagation for f[0] overflow
    let c = f[0] >> 51;
    f[0] &= MASK51;
    f[1] = f[1].wrapping_add(c);
    Fe(f)
}

fn fe_cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
    let mask = 0u64.wrapping_sub(swap & 1);
    for i in 0..5 {
        let x = (a.0[i] ^ b.0[i]) & mask;
        a.0[i] ^= x;
        b.0[i] ^= x;
    }
}

/// Modular inverse via Fermat: a^(p-2) = a^(2^255 - 21).
fn fe_invert(z: Fe) -> Fe {
    // Addition chain from djb's original:
    let z2 = fe_sq(z);
    let z4 = fe_sq(z2);
    let z8 = fe_sq(z4);
    let z9 = fe_mul(z8, z);
    let z11 = fe_mul(z9, z2);
    let z22 = fe_sq(z11);
    let t0 = fe_mul(z22, z9); // z^(2^5 - 1)

    let t1 = {
        let mut x = t0;
        for _ in 0..5 {
            x = fe_sq(x);
        }
        fe_mul(x, t0) // z^(2^10 - 1)
    };
    let t2 = {
        let mut x = t1;
        for _ in 0..10 {
            x = fe_sq(x);
        }
        fe_mul(x, t1) // z^(2^20 - 1)
    };
    let t3 = {
        let mut x = t2;
        for _ in 0..20 {
            x = fe_sq(x);
        }
        fe_mul(x, t2) // z^(2^40 - 1)
    };
    let t4 = {
        let mut x = t3;
        for _ in 0..10 {
            x = fe_sq(x);
        }
        fe_mul(x, t1) // z^(2^50 - 1)
    };
    let t5 = {
        let mut x = t4;
        for _ in 0..50 {
            x = fe_sq(x);
        }
        fe_mul(x, t4) // z^(2^100 - 1)
    };
    let t6 = {
        let mut x = t5;
        for _ in 0..100 {
            x = fe_sq(x);
        }
        fe_mul(x, t5) // z^(2^200 - 1)
    };
    let t7 = {
        let mut x = t6;
        for _ in 0..50 {
            x = fe_sq(x);
        }
        fe_mul(x, t4) // z^(2^250 - 1)
    };
    let t8 = {
        let mut x = t7;
        for _ in 0..5 {
            x = fe_sq(x);
        }
        x // z^(2^255 - 32)
    };
    fe_mul(t8, z11) // z^(2^255 - 21) = z^(p-2)
}

/// Canonical form: fully reduce to [0, p).
fn fe_canon(mut a: Fe) -> Fe {
    // Two rounds of carry propagation
    for _ in 0..2 {
        let mut carry = 0u64;
        for i in 0..4 {
            let v = a.0[i] + carry;
            a.0[i] = v & MASK51;
            carry = v >> 51;
        }
        let v = a.0[4] + carry;
        a.0[4] = v & MASK51;
        a.0[0] += (v >> 51) * 19;
    }
    // Conditional reduce: if a + 19 >= 2^255 (i.e., a >= p), use a - p = (a+19) mod 2^255
    let mut g = a.0;
    g[0] += 19;
    for i in 0..4 {
        let c = g[i] >> 51;
        g[i] &= MASK51;
        g[i + 1] += c;
    }
    let top = g[4] >> 51;
    g[4] &= MASK51;
    if top != 0 {
        Fe(g)
    } else {
        a
    }
}

// ── Encoding/decoding ─────────────────────────────────────────────────────────

/// Decode 32 bytes (LE, high bit cleared per RFC 7748) into a field element.
fn fe_from_bytes(bytes: &[u8; 32]) -> Fe {
    let mut b = *bytes;
    b[31] &= 127; // clear bit 255

    // Bit-accumulation: read 8 bits at a time, extract 51-bit limbs.
    let mut acc: u64 = 0;
    let mut acc_bits: u32 = 0;
    let mut limbs = [0u64; 5];
    let mut li = 0usize;

    for &byte in b.iter() {
        acc |= (byte as u64) << acc_bits;
        acc_bits += 8;
        if acc_bits >= 51 && li < 5 {
            limbs[li] = acc & MASK51;
            li += 1;
            acc >>= 51;
            acc_bits -= 51;
        }
    }
    // Any remaining bits go into the last limb
    if li < 5 {
        limbs[li] = acc & MASK51;
    }
    Fe(limbs)
}

/// Encode a field element into 32 bytes (LE).
fn fe_to_bytes(a: Fe) -> [u8; 32] {
    let f = fe_canon(a);

    // Pack 5 × 51-bit values into 32 bytes LE.
    let mut out = [0u8; 32];
    let mut acc: u64 = 0;
    let mut acc_bits: u32 = 0;
    let mut pos = 0usize;

    for i in 0..5 {
        acc |= f.0[i] << acc_bits;
        acc_bits += 51;
        while acc_bits >= 8 && pos < 32 {
            out[pos] = (acc & 0xFF) as u8;
            acc >>= 8;
            acc_bits -= 8;
            pos += 1;
        }
    }
    if pos < 32 {
        out[pos] = (acc & 0xFF) as u8;
    }
    out
}
