use alloc::vec::Vec;

use atom_atxf::{
    verify_image_mac, AtxfV2Header, AtxfV2Relocation, AtxfV2Segment, ATXF_FLAG_PIE,
    ATXF_KNOWN_FLAGS, ATXF_MAGIC, ATXF_PAGE_SIZE, ATXF_SIGNATURE_SIZE, ATXF_VERSION, HEADER_SIZE,
    KNOWN_PERMISSIONS, PERM_EXECUTE, PERM_READ, PERM_WRITE, RELOCATION_RELATIVE64, RELOCATION_SIZE,
    SEGMENT_BSS, SEGMENT_DATA, SEGMENT_RODATA, SEGMENT_SIZE, SEGMENT_TEXT, SEGMENT_TLS,
};

use crate::log_info;
use crate::mm::pmm::{self, PAGE_SIZE};
use crate::mm::vm::{self, PageFlags};
use crate::mm::vma::{self, PageSource, Vma, VmaBacking, VmaPermissions};
use crate::process::ProcessId;

const LOG_ORIGIN: &str = "exec";
const MAX_SEGMENTS: usize = 32;
const MAX_RELOCATIONS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecError {
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
    OutOfMemory,
    MappingFailed,
    VmaFailed,
    NonCanonicalLayout,
    EntropyUnavailable,
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

#[derive(Debug, Clone, Copy)]
pub struct LoadedExecutable {
    pub entry_point: usize,
    pub image_base: usize,
    pub image_end: usize,
}

pub fn parse_image(image: &[u8]) -> Result<ExecutableImageV2<'_>, ExecError> {
    let header = read_header(image)?;
    if header.magic != ATXF_MAGIC {
        return Err(ExecError::InvalidMagic);
    }
    if header.version != ATXF_VERSION {
        return Err(ExecError::UnsupportedVersion(header.version));
    }
    if header.header_size as usize != HEADER_SIZE
        || header.reserved != 0
        || header.image_size as usize != image.len()
    {
        return Err(ExecError::InvalidHeader);
    }
    if header.flags != ATXF_FLAG_PIE || header.flags & !ATXF_KNOWN_FLAGS != 0 {
        return Err(ExecError::InvalidFlags);
    }

    let signature_offset =
        usize::try_from(header.signature_offset).map_err(|_| ExecError::ArithmeticOverflow)?;
    let signature_size = header.signature_size as usize;
    if signature_size == 0 {
        return Err(ExecError::MissingSignature);
    }
    if signature_size != ATXF_SIGNATURE_SIZE
        || !verify_image_mac(image, signature_offset, signature_size)
    {
        return Err(ExecError::InvalidSignature);
    }

    let segment_count = header.segment_count as usize;
    let relocation_count = header.relocation_count as usize;
    if segment_count == 0 || segment_count > MAX_SEGMENTS || relocation_count > MAX_RELOCATIONS {
        return Err(ExecError::InvalidHeader);
    }
    let segment_table_offset =
        usize::try_from(header.segment_table_offset).map_err(|_| ExecError::ArithmeticOverflow)?;
    let relocation_table_offset = usize::try_from(header.relocation_table_offset)
        .map_err(|_| ExecError::ArithmeticOverflow)?;
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
                    .ok_or(ExecError::ArithmeticOverflow)?,
            )
            .ok_or(ExecError::ArithmeticOverflow)?;
        let raw = read_segment(image, offset)?;
        let kind = parse_kind(raw.kind)?;
        validate_segment_permissions(kind, raw.permissions)?;
        if raw.permissions & !KNOWN_PERMISSIONS != 0
            || raw.align != ATXF_PAGE_SIZE
            || raw.virtual_offset % ATXF_PAGE_SIZE != 0
            || raw.mem_size == 0
            || raw.mem_size < raw.file_size
        {
            return Err(ExecError::InvalidSegment);
        }

        let virtual_offset =
            usize::try_from(raw.virtual_offset).map_err(|_| ExecError::ArithmeticOverflow)?;
        let mem_size = usize::try_from(raw.mem_size).map_err(|_| ExecError::ArithmeticOverflow)?;
        let file_size =
            usize::try_from(raw.file_size).map_err(|_| ExecError::ArithmeticOverflow)?;
        if kind == SegmentKind::Bss && file_size != 0 {
            return Err(ExecError::InvalidSegment);
        }
        let file_data = if file_size == 0 {
            &image[0..0]
        } else {
            let file_offset =
                usize::try_from(raw.file_offset).map_err(|_| ExecError::ArithmeticOverflow)?;
            if file_offset % PAGE_SIZE != 0 {
                return Err(ExecError::MisalignedSegment);
            }
            checked_slice(image, file_offset, file_size)?
        };
        let segment_end = virtual_offset
            .checked_add(align_up(mem_size)?)
            .ok_or(ExecError::ArithmeticOverflow)?;
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
            .ok_or(ExecError::ArithmeticOverflow)?;
        if left_end > pair[1].virtual_offset {
            return Err(ExecError::OverlappingSegment);
        }
    }

    let entry_offset =
        usize::try_from(header.entry_offset).map_err(|_| ExecError::ArithmeticOverflow)?;
    if !segments.iter().any(|segment| {
        segment.permissions & PERM_EXECUTE != 0
            && entry_offset >= segment.virtual_offset
            && entry_offset < segment.virtual_offset.saturating_add(segment.mem_size)
    }) {
        return Err(ExecError::EntryOutOfBounds);
    }

    let mut relocations = Vec::with_capacity(relocation_count);
    for index in 0..relocation_count {
        let offset = relocation_table_offset
            .checked_add(
                index
                    .checked_mul(RELOCATION_SIZE)
                    .ok_or(ExecError::ArithmeticOverflow)?,
            )
            .ok_or(ExecError::ArithmeticOverflow)?;
        let raw = read_relocation(image, offset)?;
        if raw.kind != RELOCATION_RELATIVE64 || raw.reserved != 0 {
            return Err(ExecError::InvalidRelocation);
        }
        let target = usize::try_from(raw.offset).map_err(|_| ExecError::ArithmeticOverflow)?;
        let valid_target = segments.iter().any(|segment| {
            segment.permissions & PERM_WRITE != 0
                && target >= segment.virtual_offset
                && target
                    .checked_add(8)
                    .is_some_and(|end| end <= segment.virtual_offset + segment.mem_size)
        });
        if !valid_target {
            return Err(ExecError::InvalidRelocation);
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

pub fn load_into_process(
    image: &ExecutableImageV2<'_>,
    pml4_phys: usize,
    process_id: ProcessId,
) -> Result<LoadedExecutable, ExecError> {
    let image_base =
        crate::random::random_user_base(image.image_span).ok_or(ExecError::EntropyUnavailable)?;
    let image_end = image_base
        .checked_add(image.image_span)
        .ok_or(ExecError::ArithmeticOverflow)?;
    if image_end >= atom_abi::USER_HEAP_START as usize
        || atom_abi::validate_user_range(image_base, image.image_span).is_err()
    {
        return Err(ExecError::NonCanonicalLayout);
    }

    let mut allocated = Vec::with_capacity(image.segments.len());
    for segment in &image.segments {
        let size = align_up(segment.mem_size)?;
        let pages = size / PAGE_SIZE;
        let virt = image_base
            .checked_add(segment.virtual_offset)
            .ok_or(ExecError::ArithmeticOverflow)?;
        let phys = pmm::alloc_pages_zeroed(pages).ok_or_else(|| {
            release_allocations(&allocated);
            ExecError::OutOfMemory
        })?;
        if !segment.file_data.is_empty() {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    segment.file_data.as_ptr(),
                    vm::phys_to_virt_ptr(phys) as *mut u8,
                    segment.file_data.len(),
                );
            }
        }
        allocated.push(AllocatedSegment {
            segment: *segment,
            virt,
            phys,
            pages,
            mapped_pages: 0,
        });
    }

    if let Err(error) = apply_relocations(image, image_base, &allocated) {
        release_allocations(&allocated);
        return Err(error);
    }

    for index in 0..allocated.len() {
        let virt = allocated[index].virt;
        let phys = allocated[index].phys;
        let pages = allocated[index].pages;
        let segment = allocated[index].segment;
        let flags = segment_page_flags(segment.permissions)?;
        for page in 0..pages {
            if vm::map_page_in_pml4(
                pml4_phys,
                virt + page * PAGE_SIZE,
                phys + page * PAGE_SIZE,
                flags,
            )
            .is_err()
            {
                rollback_mappings(pml4_phys, &allocated);
                return Err(ExecError::MappingFailed);
            }
            allocated[index].mapped_pages += 1;
        }

        let end = virt
            .checked_add(pages * PAGE_SIZE)
            .ok_or(ExecError::ArithmeticOverflow)?;
        if vma::insert_bootstrap_process_vma(
            process_id,
            pml4_phys,
            Vma {
                start: virt,
                end,
                perms: segment_vma_permissions(segment.permissions),
                backing: VmaBacking::Anonymous,
                label: segment_label(segment.kind),
            },
        )
        .is_err()
        {
            rollback_mappings(pml4_phys, &allocated);
            return Err(ExecError::VmaFailed);
        }
        if vma::account_pre_mapped_range(process_id, pml4_phys, virt, end, PageSource::Anonymous)
            .is_err()
        {
            rollback_mappings(pml4_phys, &allocated);
            return Err(ExecError::VmaFailed);
        }
    }

    if verify_segment_mappings(pml4_phys, &allocated).is_err() {
        rollback_mappings(pml4_phys, &allocated);
        return Err(ExecError::MappingFailed);
    }

    let entry_point = image_base
        .checked_add(image.entry_offset)
        .ok_or(ExecError::ArithmeticOverflow)?;
    log_info!(
        LOG_ORIGIN,
        "ATXF v2 loaded: base=0x{:X} entry=0x{:X} segments={} relocations={} W^X=enabled",
        image_base,
        entry_point,
        image.segments.len(),
        image.relocations.len()
    );
    Ok(LoadedExecutable {
        entry_point,
        image_base,
        image_end,
    })
}

#[derive(Clone, Copy)]
struct AllocatedSegment<'a> {
    segment: ExecutableSegment<'a>,
    virt: usize,
    phys: usize,
    pages: usize,
    mapped_pages: usize,
}

fn apply_relocations(
    image: &ExecutableImageV2<'_>,
    image_base: usize,
    allocated: &[AllocatedSegment<'_>],
) -> Result<(), ExecError> {
    for relocation in &image.relocations {
        let allocation = allocated
            .iter()
            .find(|allocation| {
                relocation.offset >= allocation.segment.virtual_offset
                    && relocation.offset.checked_add(8).is_some_and(|end| {
                        end <= allocation.segment.virtual_offset + allocation.segment.mem_size
                    })
            })
            .ok_or(ExecError::InvalidRelocation)?;
        if allocation.segment.permissions & PERM_WRITE == 0 {
            return Err(ExecError::InvalidRelocation);
        }
        let within = relocation.offset - allocation.segment.virtual_offset;
        let value = (image_base as i128)
            .checked_add(relocation.addend as i128)
            .filter(|value| *value >= 0 && *value <= u64::MAX as i128)
            .ok_or(ExecError::ArithmeticOverflow)? as u64;
        unsafe {
            let target = vm::phys_to_virt_ptr(allocation.phys + within) as *mut u64;
            target.write_unaligned(value);
        }
    }
    Ok(())
}

fn segment_page_flags(permissions: u32) -> Result<PageFlags, ExecError> {
    if permissions & PERM_WRITE != 0 && permissions & PERM_EXECUTE != 0 {
        return Err(ExecError::InvalidPermissions);
    }
    let mut flags = PageFlags::PRESENT | PageFlags::USER;
    if permissions & PERM_WRITE != 0 {
        flags |= PageFlags::WRITABLE;
    }
    if permissions & PERM_EXECUTE == 0 {
        flags |= PageFlags::NO_EXECUTE;
    }
    vm::validate_user_page_flags(flags).map_err(|_| ExecError::InvalidPermissions)?;
    Ok(flags)
}

fn segment_vma_permissions(permissions: u32) -> VmaPermissions {
    let mut result = VmaPermissions::NONE;
    if permissions & PERM_READ != 0 {
        result = result.union(VmaPermissions::READ);
    }
    if permissions & PERM_WRITE != 0 {
        result = result.union(VmaPermissions::WRITE);
    }
    if permissions & PERM_EXECUTE != 0 {
        result = result.union(VmaPermissions::EXEC);
    }
    result
}

fn segment_label(kind: SegmentKind) -> &'static str {
    match kind {
        SegmentKind::Text => "text",
        SegmentKind::Rodata => "rodata",
        SegmentKind::Data => "data",
        SegmentKind::Bss => "bss",
        SegmentKind::Tls => "tls",
    }
}

fn validate_segment_permissions(kind: SegmentKind, permissions: u32) -> Result<(), ExecError> {
    let expected = match kind {
        SegmentKind::Text => PERM_READ | PERM_EXECUTE,
        SegmentKind::Rodata => PERM_READ,
        SegmentKind::Data | SegmentKind::Bss | SegmentKind::Tls => PERM_READ | PERM_WRITE,
    };
    if permissions != expected || permissions & PERM_WRITE != 0 && permissions & PERM_EXECUTE != 0 {
        return Err(ExecError::InvalidPermissions);
    }
    Ok(())
}

fn parse_kind(kind: u32) -> Result<SegmentKind, ExecError> {
    match kind {
        SEGMENT_TEXT => Ok(SegmentKind::Text),
        SEGMENT_RODATA => Ok(SegmentKind::Rodata),
        SEGMENT_DATA => Ok(SegmentKind::Data),
        SEGMENT_BSS => Ok(SegmentKind::Bss),
        SEGMENT_TLS => Ok(SegmentKind::Tls),
        _ => Err(ExecError::InvalidSegment),
    }
}

fn validate_table(
    image: &[u8],
    offset: usize,
    count: usize,
    entry_size: usize,
    signature_offset: usize,
) -> Result<(), ExecError> {
    if offset < HEADER_SIZE {
        return Err(ExecError::InvalidHeader);
    }
    let end = offset
        .checked_add(
            count
                .checked_mul(entry_size)
                .ok_or(ExecError::ArithmeticOverflow)?,
        )
        .ok_or(ExecError::ArithmeticOverflow)?;
    if end > image.len() || end > signature_offset {
        return Err(ExecError::Truncated);
    }
    Ok(())
}

fn checked_slice(image: &[u8], offset: usize, size: usize) -> Result<&[u8], ExecError> {
    let end = offset
        .checked_add(size)
        .ok_or(ExecError::ArithmeticOverflow)?;
    image.get(offset..end).ok_or(ExecError::Truncated)
}

fn align_up(value: usize) -> Result<usize, ExecError> {
    value
        .checked_add(PAGE_SIZE - 1)
        .map(|value| value & !(PAGE_SIZE - 1))
        .ok_or(ExecError::ArithmeticOverflow)
}

fn release_allocations(allocated: &[AllocatedSegment<'_>]) {
    for allocation in allocated {
        let _ = pmm::free_pages(allocation.phys, allocation.pages);
    }
}

fn verify_segment_mappings(
    pml4_phys: usize,
    allocated: &[AllocatedSegment<'_>],
) -> Result<(), ExecError> {
    for allocation in allocated {
        let expected_writable = allocation.segment.permissions & PERM_WRITE != 0;
        let expected_executable = allocation.segment.permissions & PERM_EXECUTE != 0;
        for page in 0..allocation.pages {
            let (_, flags) =
                vm::query_mapping_in_pml4(pml4_phys, allocation.virt + page * PAGE_SIZE)
                    .map_err(|_| ExecError::MappingFailed)?;
            let writable = flags.contains(PageFlags::WRITABLE);
            let executable = !flags.contains(PageFlags::NO_EXECUTE);
            if !flags.contains(PageFlags::USER)
                || writable != expected_writable
                || executable != expected_executable
                || writable && executable
            {
                return Err(ExecError::InvalidPermissions);
            }
        }
    }
    Ok(())
}

fn rollback_mappings(pml4_phys: usize, allocated: &[AllocatedSegment<'_>]) {
    for allocation in allocated {
        for page in 0..allocation.mapped_pages {
            let virt = allocation.virt + page * PAGE_SIZE;
            let _ = vma::take_materialized_page(pml4_phys, virt);
            let _ = vm::unmap_page_in_pml4(pml4_phys, virt);
        }
        let _ = vma::remove_vma(pml4_phys, allocation.virt);
        let _ = pmm::free_pages(allocation.phys, allocation.pages);
    }
}

fn read_header(image: &[u8]) -> Result<AtxfV2Header, ExecError> {
    if image.len() < HEADER_SIZE {
        return Err(ExecError::Truncated);
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

fn read_segment(image: &[u8], offset: usize) -> Result<AtxfV2Segment, ExecError> {
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

fn read_relocation(image: &[u8], offset: usize) -> Result<AtxfV2Relocation, ExecError> {
    Ok(AtxfV2Relocation {
        offset: read_u64(image, offset)?,
        kind: read_u32(image, offset + 8)?,
        reserved: read_u32(image, offset + 12)?,
        addend: read_i64(image, offset + 16)?,
    })
}

fn read_u16(image: &[u8], offset: usize) -> Result<u16, ExecError> {
    Ok(u16::from_le_bytes(
        checked_slice(image, offset, 2)?.try_into().unwrap(),
    ))
}

fn read_u32(image: &[u8], offset: usize) -> Result<u32, ExecError> {
    Ok(u32::from_le_bytes(
        checked_slice(image, offset, 4)?.try_into().unwrap(),
    ))
}

fn read_u64(image: &[u8], offset: usize) -> Result<u64, ExecError> {
    Ok(u64::from_le_bytes(
        checked_slice(image, offset, 8)?.try_into().unwrap(),
    ))
}

fn read_i64(image: &[u8], offset: usize) -> Result<i64, ExecError> {
    Ok(i64::from_le_bytes(
        checked_slice(image, offset, 8)?.try_into().unwrap(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use atom_atxf::compute_image_mac;

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
        let signature = compute_image_mac(image, SIGNATURE_OFFSET, ATXF_SIGNATURE_SIZE).unwrap();
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
    fn accepts_valid_v2() {
        assert!(parse_image(&valid_image()).is_ok());
    }

    #[test]
    fn rejects_v1_without_fallback() {
        let mut image = valid_image();
        write_u16(&mut image, 4, 1);
        assert_eq!(
            parse_image(&image).unwrap_err(),
            ExecError::UnsupportedVersion(1)
        );
    }

    #[test]
    fn rejects_missing_invalid_and_tampered_signatures() {
        let mut missing = valid_image();
        write_u32(&mut missing, 52, 0);
        assert_eq!(
            parse_image(&missing).unwrap_err(),
            ExecError::MissingSignature
        );

        let mut invalid = valid_image();
        invalid[SIGNATURE_OFFSET] ^= 1;
        assert_eq!(
            parse_image(&invalid).unwrap_err(),
            ExecError::InvalidSignature
        );

        let mut tampered = valid_image();
        tampered[TEXT_FILE_OFFSET] ^= 1;
        assert_eq!(
            parse_image(&tampered).unwrap_err(),
            ExecError::InvalidSignature
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
        assert_eq!(parse_image(&wx).unwrap_err(), ExecError::InvalidPermissions);

        let mut overlap = valid_image();
        write_u64(&mut overlap, SEGMENTS_OFFSET + SEGMENT_SIZE + 32, 0);
        resign(&mut overlap);
        assert_eq!(
            parse_image(&overlap).unwrap_err(),
            ExecError::OverlappingSegment
        );

        let mut bad_entry = valid_image();
        write_u64(&mut bad_entry, 12, PAGE_SIZE as u64);
        resign(&mut bad_entry);
        assert_eq!(
            parse_image(&bad_entry).unwrap_err(),
            ExecError::EntryOutOfBounds
        );
    }

    #[test]
    fn rejects_relocation_outside_writable_segment() {
        let mut image = valid_image();
        write_u64(&mut image, RELOCATIONS_OFFSET, 0);
        resign(&mut image);
        assert_eq!(
            parse_image(&image).unwrap_err(),
            ExecError::InvalidRelocation
        );
    }
}
