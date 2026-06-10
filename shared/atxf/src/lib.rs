//! ATXF v3 — Atom executable format: layout constants and **asymmetric**
//! image authentication.
//!
//! ## Trust model (v3)
//!
//! Images are signed with **Ed25519** (RFC 8032). The signer (`elf2atxf`,
//! release tooling) holds the private seed; the kernel embeds **only the
//! public verifying key** and can therefore authenticate images but never
//! produce them. This replaces the ATXF v2 HMAC scheme, whose symmetric key
//! necessarily shipped inside the kernel binary and made every kernel image
//! a signing oracle.
//!
//! ## Key material
//!
//! * `ATXF_DEV_VERIFYING_KEY` — the **development** product public key,
//!   embedded below. The matching private seed lives in
//!   `keys/dev/atxf_signing.seed` and is read by host tooling only.
//! * Production builds must replace both: generate a product keypair, keep
//!   the seed in protected release infrastructure, and swap the constant (or
//!   wire it through the build) — the kernel/source tree must never contain
//!   a production private key.

#![no_std]

extern crate alloc;

pub mod loader;

use ed25519_compact::{PublicKey, Signature};

pub const ATXF_MAGIC: u32 = 0x4154_5846;
pub const ATXF_VERSION: u16 = 3;
pub const ATXF_FLAG_PIE: u32 = 1 << 0;
pub const ATXF_KNOWN_FLAGS: u32 = ATXF_FLAG_PIE;
/// Ed25519 signature size.
pub const ATXF_SIGNATURE_SIZE: usize = 64;
/// Ed25519 public (verifying) key size.
pub const ATXF_VERIFYING_KEY_SIZE: usize = 32;
pub const ATXF_PAGE_SIZE: u64 = 4096;

/// Development product **public** key (Ed25519). See the module docs for the
/// production key-rotation contract. Knowing this key only allows verifying
/// images, never signing them.
pub const ATXF_DEV_VERIFYING_KEY: [u8; ATXF_VERIFYING_KEY_SIZE] = [
    0x9B, 0x2F, 0x17, 0x19, 0x41, 0xED, 0xF4, 0xED, 0x18, 0x79, 0x13, 0x86, 0xE2, 0xC5, 0x86, 0x6D,
    0x41, 0x2D, 0x28, 0x64, 0x55, 0x26, 0xD0, 0xB0, 0x10, 0x9F, 0x39, 0xC0, 0x64, 0xF9, 0xFB, 0x46,
];

pub const SEGMENT_TEXT: u32 = 1;
pub const SEGMENT_RODATA: u32 = 2;
pub const SEGMENT_DATA: u32 = 3;
pub const SEGMENT_BSS: u32 = 4;
pub const SEGMENT_TLS: u32 = 5;

pub const PERM_READ: u32 = 1 << 0;
pub const PERM_WRITE: u32 = 1 << 1;
pub const PERM_EXECUTE: u32 = 1 << 2;
pub const KNOWN_PERMISSIONS: u32 = PERM_READ | PERM_WRITE | PERM_EXECUTE;

pub const RELOCATION_RELATIVE64: u32 = 1;
pub const RELOCATION_ABS64: u32 = 2;

// The on-disk table layout is unchanged from v2 (only the signature algorithm
// and size changed), so the struct names keep the V2 suffix.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AtxfV2Header {
    pub magic: u32,
    pub version: u16,
    pub header_size: u16,
    pub flags: u32,
    pub entry_offset: u64,
    pub segment_count: u32,
    pub relocation_count: u32,
    pub segment_table_offset: u64,
    pub relocation_table_offset: u64,
    pub signature_offset: u64,
    pub signature_size: u32,
    pub reserved: u32,
    pub image_size: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AtxfV2Segment {
    pub kind: u32,
    pub permissions: u32,
    pub file_offset: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub virtual_offset: u64,
    pub align: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AtxfV2Relocation {
    pub offset: u64,
    pub kind: u32,
    pub reserved: u32,
    pub addend: i64,
}

pub const HEADER_SIZE: usize = core::mem::size_of::<AtxfV2Header>();
pub const SEGMENT_SIZE: usize = core::mem::size_of::<AtxfV2Segment>();
pub const RELOCATION_SIZE: usize = core::mem::size_of::<AtxfV2Relocation>();

/// Verify the Ed25519 signature embedded at `signature_offset` against
/// `verifying_key`. The signature covers the image bytes with the signature
/// window zeroed (so the embedded signature is not part of the signed data).
/// Fail-closed: any malformed input returns `false`.
pub fn verify_image_signature(
    image: &[u8],
    signature_offset: usize,
    signature_size: usize,
    verifying_key: &[u8; ATXF_VERIFYING_KEY_SIZE],
) -> bool {
    if signature_size != ATXF_SIGNATURE_SIZE {
        return false;
    }
    let Some(signature_end) = signature_offset.checked_add(signature_size) else {
        return false;
    };
    if signature_end > image.len() {
        return false;
    }
    let Ok(pk) = PublicKey::from_slice(verifying_key) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&image[signature_offset..signature_end]) else {
        return false;
    };
    // Build the signed message: full image with the signature window zeroed.
    let mut msg = alloc::vec::Vec::with_capacity(image.len());
    msg.extend_from_slice(&image[..signature_offset]);
    msg.resize(signature_offset + signature_size, 0u8);
    msg.extend_from_slice(&image[signature_end..]);
    pk.verify(&msg, &sig).is_ok()
}

/// Sign an image with an Ed25519 private seed. Host-tooling/test only: the
/// kernel never enables this feature and never holds a seed.
#[cfg(any(test, feature = "signing"))]
pub fn sign_image(
    image: &[u8],
    signature_offset: usize,
    signature_size: usize,
    seed: &[u8; 32],
) -> Option<[u8; ATXF_SIGNATURE_SIZE]> {
    use ed25519_compact::{KeyPair, Seed};

    if signature_size != ATXF_SIGNATURE_SIZE {
        return None;
    }
    let signature_end = signature_offset.checked_add(signature_size)?;
    if signature_end > image.len() {
        return None;
    }
    let kp = KeyPair::from_seed(Seed::new(*seed));
    // Build the signed message: full image with the signature window zeroed.
    let mut msg = alloc::vec::Vec::with_capacity(image.len());
    msg.extend_from_slice(&image[..signature_offset]);
    msg.resize(signature_offset + signature_size, 0u8);
    msg.extend_from_slice(&image[signature_end..]);
    let sig = kp.sk.sign(&msg, None);
    let mut result = [0u8; ATXF_SIGNATURE_SIZE];
    result.copy_from_slice(sig.as_ref());
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_compact::{KeyPair, Seed};

    /// Fixed test-only keypair seed — distinct from the dev product key so the
    /// tests also prove that key identity matters.
    pub(crate) const TEST_SEED: [u8; 32] = [0x42; 32];

    pub(crate) fn test_verifying_key() -> [u8; ATXF_VERIFYING_KEY_SIZE] {
        let kp = KeyPair::from_seed(Seed::new(TEST_SEED));
        let mut result = [0u8; ATXF_VERIFYING_KEY_SIZE];
        result.copy_from_slice(kp.pk.as_ref());
        result
    }

    fn signed_image() -> ([u8; 192], usize) {
        let mut image = [0x5au8; 192];
        let signature_offset = 48;
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].fill(0);
        let signature =
            sign_image(&image, signature_offset, ATXF_SIGNATURE_SIZE, &TEST_SEED).unwrap();
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].copy_from_slice(&signature);
        (image, signature_offset)
    }

    #[test]
    fn valid_signature_is_accepted() {
        let (image, signature_offset) = signed_image();
        assert!(verify_image_signature(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE,
            &test_verifying_key(),
        ));
    }

    #[test]
    fn unsigned_image_is_rejected() {
        let (mut image, signature_offset) = signed_image();
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].fill(0);
        assert!(!verify_image_signature(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE,
            &test_verifying_key(),
        ));
    }

    #[test]
    fn one_byte_tamper_is_rejected() {
        let (mut image, signature_offset) = signed_image();
        image[12] ^= 1;
        assert!(!verify_image_signature(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE,
            &test_verifying_key(),
        ));
    }

    #[test]
    fn wrong_verifying_key_is_rejected() {
        let (image, signature_offset) = signed_image();
        assert!(!verify_image_signature(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE,
            &ATXF_DEV_VERIFYING_KEY,
        ));
    }

    #[test]
    fn invalid_signature_bounds_are_rejected() {
        let (image, _) = signed_image();
        assert!(!verify_image_signature(
            &image,
            180,
            ATXF_SIGNATURE_SIZE,
            &test_verifying_key(),
        ));
        assert!(!verify_image_signature(
            &image,
            48,
            ATXF_SIGNATURE_SIZE - 1,
            &test_verifying_key(),
        ));
    }

    #[test]
    fn dev_key_is_a_valid_ed25519_point() {
        assert!(
            PublicKey::from_slice(&ATXF_DEV_VERIFYING_KEY).is_ok(),
            "ATXF_DEV_VERIFYING_KEY must be a valid Ed25519 public key"
        );
    }
}
