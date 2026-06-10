//! ATXF v3 image parsing and conformance validation.
//!
//! This is the security-critical half of the executable loader: signature
//! authentication, header/table bounds, W^X segment permissions, overlap
//! detection, entry-point containment and relocation targeting. It is pure
//! (no paging, no allocator beyond `alloc::Vec`) so it lives here, in a
//! host-testable crate, instead of inside the kernel where its test suite
//! could never execute (`kernel` builds with `test = false`). The kernel
//! wraps `parse_image` with the embedded product verifying key and keeps only
//! the memory-mapping half (`load_into_process`).

use alloc::vec::Vec;

use crate::{
    verify_image_signature, AtxfV2Header, AtxfV2Relocation, AtxfV2Segment, ATXF_FLAG_PIE,
    ATXF_KNOWN_FLAGS, ATXF_MAGIC, ATXF_PAGE_SIZE, ATXF_SIGNATURE_SIZE, ATXF_VERIFYING_KEY_SIZE,
    ATXF_VERSION, HEADER_SIZE, KNOWN_PERMISSIONS, PERM_EXECUTE, PERM_READ, PERM_WRITE,
    RELOCATION_RELATIVE64, RELOCATION_SIZE, SEGMENT_BSS, SEGMENT_DATA, SEGMENT_RODATA,
    SEGMENT_SIZE, SEGMENT_TEXT, SEGMENT_TLS,
};

pub const MAX_SEGMENTS: usize = 32;
pub const MAX_RELOCATIONS: usize = 1_000_000;

const PAGE_SIZE: usize = ATXF_PAGE_SIZE as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    InvalidMagic,
    UnsupportedVersion(u16),
    Truncated,
    InvalidHeader,
    InvalidFlags,
    InvalidSignature,
    MissingSignature,
    InvalidSegment,
    MisalignedSegment,
    OverlappingSegment,
    InvalidPermissions,
    EntryOutOfBounds,
    InvalidRelocation,
    ArithmeticOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Text,
    Rodata,
    Data,
    Bss,
    Tls,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutableSegment<'a> {
    pub kind: SegmentKind,
    pub permissions: u32,
    pub file_data: &'a [u8],
    pub mem_size: usize,
    pub virtual_offset: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutableRelocation {
    pub offset: usize,
    pub addend: i64,
}

#[derive(Debug)]
pub struct ExecutableImageV2<'a> {
    pub entry_offset: usize,
    pub segments: Vec<ExecutableSegment<'a>>,
    pub relocations: Vec<ExecutableRelocation>,
    pub image_span: usize,
}

pub fn parse_image<'a>(
    image: &'a [u8],
    verifying_key: &[u8; ATXF_VERIFYING_KEY_SIZE],
) -> Result<ExecutableImageV2<'a>, ParseError> {
    let header = read_header(image)?;
    if header.magic != ATXF_MAGIC {
        return Err(ParseError::InvalidMagic);
    }
    if header.version != ATXF_VERSION {
        return Err(ParseError::UnsupportedVersion(header.version));
    }
    if header.header_size as usize != HEADER_SIZE
        || header.reserved != 0
        || header.image_size as usize != image.len()
    {
        return Err(ParseError::InvalidHeader);
    }
    if header.flags != ATXF_FLAG_PIE || header.flags & !ATXF_KNOWN_FLAGS != 0 {
        return Err(ParseError::InvalidFlags);
    }

    let signature_offset =
        usize::try_from(header.signature_offset).map_err(|_| ParseError::ArithmeticOverflow)?;
    let signature_size = header.signature_size as usize;
    if signature_size == 0 {
        return Err(ParseError::MissingSignature);
    }
    if signature_size != ATXF_SIGNATURE_SIZE
        || !verify_image_signature(image, signature_offset, signature_size, verifying_key)
    {
        return Err(ParseError::InvalidSignature);
    }

    let segment_count = header.segment_count as usize;
    let relocation_count = header.relocation_count as usize;
    if segment_count == 0 || segment_count > MAX_SEGMENTS || relocation_count > MAX_RELOCATIONS {
        return Err(ParseError::InvalidHeader);
    }
    let segment_table_offset =
        usize::try_from(header.segment_table_offset).map_err(|_| ParseError::ArithmeticOverflow)?;
    let relocation_table_offset = usize::try_from(header.relocation_table_offset)
        .map_err(|_| ParseError::ArithmeticOverflow)?;
    validate_table(
        image,
        segment_table_offset,
        segment_count,
        SEGMENT_SIZE,
        signature_offset,
    )?;
    validate_table(
        image,
        relocation_table_offset,
        relocation_count,
        RELOCATION_SIZE,
        signature_offset,
    )?;

    let mut segments = Vec::with_capacity(segment_count);
    let mut image_span = 0usize;
    for index in 0..segment_count {
        let offset = segment_table_offset
            .checked_add(
                index
                    .checked_mul(SEGMENT_SIZE)
                    .ok_or(ParseError::ArithmeticOverflow)?,
            )
            .ok_or(ParseError::ArithmeticOverflow)?;
        let raw = read_segment(image, offset)?;
        let kind = parse_kind(raw.kind)?;
        validate_segment_permissions(kind, raw.permissions)?;
        if raw.permissions & !KNOWN_PERMISSIONS != 0
            || raw.align != ATXF_PAGE_SIZE
            || raw.virtual_offset % ATXF_PAGE_SIZE != 0
            || raw.mem_size == 0
            || raw.mem_size < raw.file_size
        {
            return Err(ParseError::InvalidSegment);
        }

        let virtual_offset =
            usize::try_from(raw.virtual_offset).map_err(|_| ParseError::ArithmeticOverflow)?;
        let mem_size = usize::try_from(raw.mem_size).map_err(|_| ParseError::ArithmeticOverflow)?;
        let file_size =
            usize::try_from(raw.file_size).map_err(|_| ParseError::ArithmeticOverflow)?;
        if kind == SegmentKind::Bss && file_size != 0 {
            return Err(ParseError::InvalidSegment);
        }
        let file_data = if file_size == 0 {
            &image[0..0]
        } else {
            let file_offset =
                usize::try_from(raw.file_offset).map_err(|_| ParseError::ArithmeticOverflow)?;
            if file_offset % PAGE_SIZE != 0 {
                return Err(ParseError::MisalignedSegment);
            }
            checked_slice(image, file_offset, file_size)?
        };
        let segment_end = virtual_offset
            .checked_add(align_up(mem_size)?)
            .ok_or(ParseError::ArithmeticOverflow)?;
        image_span = image_span.max(segment_end);
        segments.push(ExecutableSegment {
            kind,
            permissions: raw.permissions,
            file_data,
            mem_size,
            virtual_offset,
        });
    }
    segments.sort_by_key(|segment| segment.virtual_offset);
    for pair in segments.windows(2) {
        let left_end = pair[0]
            .virtual_offset
            .checked_add(align_up(pair[0].mem_size)?)
            .ok_or(ParseError::ArithmeticOverflow)?;
        if left_end > pair[1].virtual_offset {
            return Err(ParseError::OverlappingSegment);
        }
    }

    let entry_offset =
        usize::try_from(header.entry_offset).map_err(|_| ParseError::ArithmeticOverflow)?;
    if !segments.iter().any(|segment| {
        segment.permissions & PERM_EXECUTE != 0
            && entry_offset >= segment.virtual_offset
            && entry_offset < segment.virtual_offset.saturating_add(segment.mem_size)
    }) {
        return Err(ParseError::EntryOutOfBounds);
    }

    let mut relocations = Vec::with_capacity(relocation_count);
    for index in 0..relocation_count {
        let offset = relocation_table_offset
            .checked_add(
                index
                    .checked_mul(RELOCATION_SIZE)
                    .ok_or(ParseError::ArithmeticOverflow)?,
            )
            .ok_or(ParseError::ArithmeticOverflow)?;
        let raw = read_relocation(image, offset)?;
        if raw.kind != RELOCATION_RELATIVE64 || raw.reserved != 0 {
            return Err(ParseError::InvalidRelocation);
        }
        let target = usize::try_from(raw.offset).map_err(|_| ParseError::ArithmeticOverflow)?;
        let valid_target = segments.iter().any(|segment| {
            segment.permissions & PERM_WRITE != 0
                && target >= segment.virtual_offset
                && target
                    .checked_add(8)
                    .is_some_and(|end| end <= segment.virtual_offset + segment.mem_size)
        });
        if !valid_target {
            return Err(ParseError::InvalidRelocation);
        }
        relocations.push(ExecutableRelocation {
            offset: target,
            addend: raw.addend,
        });
    }

    Ok(ExecutableImageV2 {
        entry_offset,
        segments,
        relocations,
        image_span,
    })
}

fn validate_segment_permissions(kind: SegmentKind, permissions: u32) -> Result<(), ParseError> {
    let expected = match kind {
        SegmentKind::Text => PERM_READ | PERM_EXECUTE,
        SegmentKind::Rodata => PERM_READ,
        SegmentKind::Data | SegmentKind::Bss | SegmentKind::Tls => PERM_READ | PERM_WRITE,
    };
    if permissions != expected || permissions & PERM_WRITE != 0 && permissions & PERM_EXECUTE != 0 {
        return Err(ParseError::InvalidPermissions);
    }
    Ok(())
}

fn parse_kind(kind: u32) -> Result<SegmentKind, ParseError> {
    match kind {
        SEGMENT_TEXT => Ok(SegmentKind::Text),
        SEGMENT_RODATA => Ok(SegmentKind::Rodata),
        SEGMENT_DATA => Ok(SegmentKind::Data),
        SEGMENT_BSS => Ok(SegmentKind::Bss),
        SEGMENT_TLS => Ok(SegmentKind::Tls),
        _ => Err(ParseError::InvalidSegment),
    }
}

fn validate_table(
    image: &[u8],
    offset: usize,
    count: usize,
    entry_size: usize,
    signature_offset: usize,
) -> Result<(), ParseError> {
    if offset < HEADER_SIZE {
        return Err(ParseError::InvalidHeader);
    }
    let end = offset
        .checked_add(
            count
                .checked_mul(entry_size)
                .ok_or(ParseError::ArithmeticOverflow)?,
        )
        .ok_or(ParseError::ArithmeticOverflow)?;
    if end > image.len() || end > signature_offset {
        return Err(ParseError::Truncated);
    }
    Ok(())
}

fn checked_slice(image: &[u8], offset: usize, size: usize) -> Result<&[u8], ParseError> {
    let end = offset
        .checked_add(size)
        .ok_or(ParseError::ArithmeticOverflow)?;
    image.get(offset..end).ok_or(ParseError::Truncated)
}

fn align_up(value: usize) -> Result<usize, ParseError> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(ParseError::ArithmeticOverflow)
}

fn read_header(image: &[u8]) -> Result<AtxfV2Header, ParseError> {
    if image.len() < HEADER_SIZE {
        return Err(ParseError::Truncated);
    }
    Ok(AtxfV2Header {
        magic: read_u32(image, 0)?,
        version: read_u16(image, 4)?,
        header_size: read_u16(image, 6)?,
        flags: read_u32(image, 8)?,
        entry_offset: read_u64(image, 12)?,
        segment_count: read_u32(image, 20)?,
        relocation_count: read_u32(image, 24)?,
        segment_table_offset: read_u64(image, 28)?,
        relocation_table_offset: read_u64(image, 36)?,
        signature_offset: read_u64(image, 44)?,
        signature_size: read_u32(image, 52)?,
        reserved: read_u32(image, 56)?,
        image_size: read_u64(image, 60)?,
    })
}

fn read_segment(image: &[u8], offset: usize) -> Result<AtxfV2Segment, ParseError> {
    Ok(AtxfV2Segment {
        kind: read_u32(image, offset)?,
        permissions: read_u32(image, offset + 4)?,
        file_offset: read_u64(image, offset + 8)?,
        file_size: read_u64(image, offset + 16)?,
        mem_size: read_u64(image, offset + 24)?,
        virtual_offset: read_u64(image, offset + 32)?,
        align: read_u64(image, offset + 40)?,
    })
}

fn read_relocation(image: &[u8], offset: usize) -> Result<AtxfV2Relocation, ParseError> {
    Ok(AtxfV2Relocation {
        offset: read_u64(image, offset)?,
        kind: read_u32(image, offset + 8)?,
        reserved: read_u32(image, offset + 12)?,
        addend: read_i64(image, offset + 16)?,
    })
}

fn read_u16(image: &[u8], offset: usize) -> Result<u16, ParseError> {
    Ok(u16::from_le_bytes(
        checked_slice(image, offset, 2)?.try_into().unwrap(),
    ))
}

fn read_u32(image: &[u8], offset: usize) -> Result<u32, ParseError> {
    Ok(u32::from_le_bytes(
        checked_slice(image, offset, 4)?.try_into().unwrap(),
    ))
}

fn read_u64(image: &[u8], offset: usize) -> Result<u64, ParseError> {
    Ok(u64::from_le_bytes(
        checked_slice(image, offset, 8)?.try_into().unwrap(),
    ))
}

fn read_i64(image: &[u8], offset: usize) -> Result<i64, ParseError> {
    Ok(i64::from_le_bytes(
        checked_slice(image, offset, 8)?.try_into().unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign_image;
    use alloc::vec::Vec;

    /// Test keypair — deliberately not the dev product key.
    const TEST_SEED: [u8; 32] = [0x42; 32];

    fn test_verifying_key() -> [u8; ATXF_VERIFYING_KEY_SIZE] {
        use ed25519_compact::{KeyPair, Seed};
        let kp = KeyPair::from_seed(Seed::new(TEST_SEED));
        let mut result = [0u8; ATXF_VERIFYING_KEY_SIZE];
        result.copy_from_slice(kp.pk.as_ref());
        result
    }

    fn parse(image: &[u8]) -> Result<ExecutableImageV2<'_>, ParseError> {
        parse_image(image, &test_verifying_key())
    }

    const SEGMENTS_OFFSET: usize = HEADER_SIZE;
    const RELOCATIONS_OFFSET: usize = SEGMENTS_OFFSET + 2 * SEGMENT_SIZE;
    const TEXT_FILE_OFFSET: usize = PAGE_SIZE;
    const DATA_FILE_OFFSET: usize = 2 * PAGE_SIZE;
    const SIGNATURE_OFFSET: usize = 3 * PAGE_SIZE;

    fn write_u16(image: &mut [u8], offset: usize, value: u16) {
        image[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_i64(image: &mut [u8], offset: usize, value: i64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_segment(
        image: &mut [u8],
        offset: usize,
        kind: u32,
        permissions: u32,
        file_offset: usize,
        file_size: usize,
        mem_size: usize,
        virtual_offset: usize,
    ) {
        write_u32(image, offset, kind);
        write_u32(image, offset + 4, permissions);
        write_u64(image, offset + 8, file_offset as u64);
        write_u64(image, offset + 16, file_size as u64);
        write_u64(image, offset + 24, mem_size as u64);
        write_u64(image, offset + 32, virtual_offset as u64);
        write_u64(image, offset + 40, PAGE_SIZE as u64);
    }

    fn resign(image: &mut [u8]) {
        image[SIGNATURE_OFFSET..SIGNATURE_OFFSET + ATXF_SIGNATURE_SIZE].fill(0);
        let signature =
            sign_image(image, SIGNATURE_OFFSET, ATXF_SIGNATURE_SIZE, &TEST_SEED).unwrap();
        image[SIGNATURE_OFFSET..SIGNATURE_OFFSET + ATXF_SIGNATURE_SIZE].copy_from_slice(&signature);
    }

    fn valid_image() -> Vec<u8> {
        let mut image = alloc::vec![0u8; SIGNATURE_OFFSET + ATXF_SIGNATURE_SIZE];
        let image_len = image.len();
        write_u32(&mut image, 0, ATXF_MAGIC);
        write_u16(&mut image, 4, ATXF_VERSION);
        write_u16(&mut image, 6, HEADER_SIZE as u16);
        write_u32(&mut image, 8, ATXF_FLAG_PIE);
        write_u64(&mut image, 12, 0);
        write_u32(&mut image, 20, 2);
        write_u32(&mut image, 24, 1);
        write_u64(&mut image, 28, SEGMENTS_OFFSET as u64);
        write_u64(&mut image, 36, RELOCATIONS_OFFSET as u64);
        write_u64(&mut image, 44, SIGNATURE_OFFSET as u64);
        write_u32(&mut image, 52, ATXF_SIGNATURE_SIZE as u32);
        write_u32(&mut image, 56, 0);
        write_u64(&mut image, 60, image_len as u64);

        write_segment(
            &mut image,
            SEGMENTS_OFFSET,
            SEGMENT_TEXT,
            PERM_READ | PERM_EXECUTE,
            TEXT_FILE_OFFSET,
            1,
            PAGE_SIZE,
            0,
        );
        write_segment(
            &mut image,
            SEGMENTS_OFFSET + SEGMENT_SIZE,
            SEGMENT_DATA,
            PERM_READ | PERM_WRITE,
            DATA_FILE_OFFSET,
            8,
            PAGE_SIZE,
            PAGE_SIZE,
        );
        write_u64(&mut image, RELOCATIONS_OFFSET, PAGE_SIZE as u64);
        write_u32(&mut image, RELOCATIONS_OFFSET + 8, RELOCATION_RELATIVE64);
        write_u32(&mut image, RELOCATIONS_OFFSET + 12, 0);
        write_i64(&mut image, RELOCATIONS_OFFSET + 16, 16);
        image[TEXT_FILE_OFFSET] = 0xc3;
        resign(&mut image);
        image
    }

    #[test]
    fn accepts_valid_image() {
        assert!(parse(&valid_image()).is_ok());
    }

    #[test]
    fn rejects_older_versions_without_fallback() {
        for version in [1u16, 2u16] {
            let mut image = valid_image();
            write_u16(&mut image, 4, version);
            assert_eq!(
                parse(&image).unwrap_err(),
                ParseError::UnsupportedVersion(version)
            );
        }
    }

    #[test]
    fn rejects_missing_invalid_and_tampered_signatures() {
        let mut missing = valid_image();
        write_u32(&mut missing, 52, 0);
        assert_eq!(parse(&missing).unwrap_err(), ParseError::MissingSignature);

        let mut invalid = valid_image();
        invalid[SIGNATURE_OFFSET] ^= 1;
        assert_eq!(parse(&invalid).unwrap_err(), ParseError::InvalidSignature);

        let mut tampered = valid_image();
        tampered[TEXT_FILE_OFFSET] ^= 1;
        assert_eq!(parse(&tampered).unwrap_err(), ParseError::InvalidSignature);
    }

    #[test]
    fn rejects_image_signed_with_a_different_key() {
        let image = valid_image();
        assert_eq!(
            parse_image(&image, &crate::ATXF_DEV_VERIFYING_KEY).unwrap_err(),
            ParseError::InvalidSignature
        );
    }

    #[test]
    fn rejects_wx_overlap_and_non_executable_entry() {
        let mut wx = valid_image();
        write_u32(
            &mut wx,
            SEGMENTS_OFFSET + 4,
            PERM_READ | PERM_WRITE | PERM_EXECUTE,
        );
        resign(&mut wx);
        assert_eq!(parse(&wx).unwrap_err(), ParseError::InvalidPermissions);

        let mut overlap = valid_image();
        write_u64(&mut overlap, SEGMENTS_OFFSET + SEGMENT_SIZE + 32, 0);
        resign(&mut overlap);
        assert_eq!(parse(&overlap).unwrap_err(), ParseError::OverlappingSegment);

        let mut bad_entry = valid_image();
        write_u64(&mut bad_entry, 12, PAGE_SIZE as u64);
        resign(&mut bad_entry);
        assert_eq!(parse(&bad_entry).unwrap_err(), ParseError::EntryOutOfBounds);
    }

    #[test]
    fn rejects_relocation_outside_writable_segment() {
        let mut image = valid_image();
        write_u64(&mut image, RELOCATIONS_OFFSET, 0);
        resign(&mut image);
        assert_eq!(parse(&image).unwrap_err(), ParseError::InvalidRelocation);
    }
}
