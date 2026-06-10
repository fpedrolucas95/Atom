#![no_std]

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const ATXF_MAGIC: u32 = 0x4154_5846;
pub const ATXF_VERSION: u16 = 2;
pub const ATXF_FLAG_PIE: u32 = 1 << 0;
pub const ATXF_KNOWN_FLAGS: u32 = ATXF_FLAG_PIE;
pub const ATXF_SIGNATURE_SIZE: usize = 32;
pub const ATXF_PAGE_SIZE: u64 = 4096;

// Development product root. Production builds must inject a protected product
// key instead of keeping symmetric signing material in the source tree.
const PRODUCT_HMAC_KEY: &[u8; 32] = b"AtomOS-ATXF-v2-product-key-0001!";

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

type HmacSha256 = Hmac<Sha256>;

pub fn compute_image_mac(
    image: &[u8],
    signature_offset: usize,
    signature_size: usize,
) -> Option<[u8; ATXF_SIGNATURE_SIZE]> {
    let signature_end = signature_offset.checked_add(signature_size)?;
    if signature_size != ATXF_SIGNATURE_SIZE || signature_end > image.len() {
        return None;
    }

    let mut mac = HmacSha256::new_from_slice(PRODUCT_HMAC_KEY).ok()?;
    mac.update(&image[..signature_offset]);
    mac.update(&[0u8; ATXF_SIGNATURE_SIZE]);
    mac.update(&image[signature_end..]);
    let bytes = mac.finalize().into_bytes();
    let mut result = [0u8; ATXF_SIGNATURE_SIZE];
    result.copy_from_slice(&bytes);
    Some(result)
}

pub fn verify_image_mac(image: &[u8], signature_offset: usize, signature_size: usize) -> bool {
    let signature_end = match signature_offset.checked_add(signature_size) {
        Some(end) if signature_size == ATXF_SIGNATURE_SIZE && end <= image.len() => end,
        _ => return false,
    };
    let expected = match compute_image_mac(image, signature_offset, signature_size) {
        Some(mac) => mac,
        None => return false,
    };

    let mut diff = 0u8;
    for (actual, expected) in image[signature_offset..signature_end]
        .iter()
        .zip(expected.iter())
    {
        diff |= actual ^ expected;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_image() -> ([u8; 128], usize) {
        let mut image = [0x5au8; 128];
        let signature_offset = 48;
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].fill(0);
        let signature = compute_image_mac(&image, signature_offset, ATXF_SIGNATURE_SIZE).unwrap();
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].copy_from_slice(&signature);
        (image, signature_offset)
    }

    #[test]
    fn valid_mac_is_accepted() {
        let (image, signature_offset) = signed_image();
        assert!(verify_image_mac(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE
        ));
    }

    #[test]
    fn unsigned_image_is_rejected() {
        let (mut image, signature_offset) = signed_image();
        image[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].fill(0);
        assert!(!verify_image_mac(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE
        ));
    }

    #[test]
    fn one_byte_tamper_is_rejected() {
        let (mut image, signature_offset) = signed_image();
        image[12] ^= 1;
        assert!(!verify_image_mac(
            &image,
            signature_offset,
            ATXF_SIGNATURE_SIZE
        ));
    }

    #[test]
    fn invalid_signature_bounds_are_rejected() {
        let (image, _) = signed_image();
        assert!(!verify_image_mac(&image, 120, ATXF_SIGNATURE_SIZE));
        assert!(!verify_image_mac(&image, 48, ATXF_SIGNATURE_SIZE - 1));
    }
}
