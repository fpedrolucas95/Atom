//! Kernel half of the ATXF v3 executable loader.
//!
//! Parsing, signature authentication and conformance validation live in
//! `atom_atxf::loader` (host-testable; its test suite runs in CI). This module
//! binds that parser to the embedded product verifying key and implements the
//! memory side: physical allocation, relocation application, W^X page-table
//! mapping and VMA registration.

use alloc::vec::Vec;

use atom_atxf::loader::{self, ParseError};
pub use atom_atxf::loader::{ExecutableImageV2, ExecutableSegment, SegmentKind};
use atom_atxf::{ATXF_DEV_VERIFYING_KEY, PERM_EXECUTE, PERM_READ, PERM_WRITE};

use crate::log_info;
use crate::mm::pmm::{self, PAGE_SIZE};
use crate::mm::vm::{self, PageFlags};
use crate::mm::vma::{self, PageSource, Vma, VmaBacking, VmaPermissions};
use crate::process::ProcessId;

const LOG_ORIGIN: &str = "exec";

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

impl From<ParseError> for ExecError {
    fn from(error: ParseError) -> Self {
        match error {
            ParseError::InvalidMagic => Self::InvalidMagic,
            ParseError::UnsupportedVersion(v) => Self::UnsupportedVersion(v),
            ParseError::Truncated => Self::Truncated,
            ParseError::InvalidHeader => Self::InvalidHeader,
            ParseError::InvalidFlags => Self::InvalidFlags,
            ParseError::InvalidSignature => Self::InvalidSignature,
            ParseError::MissingSignature => Self::MissingSignature,
            ParseError::InvalidSegment => Self::InvalidSegment,
            ParseError::MisalignedSegment => Self::MisalignedSegment,
            ParseError::OverlappingSegment => Self::OverlappingSegment,
            ParseError::InvalidPermissions => Self::InvalidPermissions,
            ParseError::EntryOutOfBounds => Self::EntryOutOfBounds,
            ParseError::InvalidRelocation => Self::InvalidRelocation,
            ParseError::ArithmeticOverflow => Self::ArithmeticOverflow,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LoadedExecutable {
    pub entry_point: usize,
    pub image_base: usize,
    pub image_end: usize,
}

/// Authenticate and parse an ATXF v3 image against the embedded product
/// verifying key (Ed25519; the kernel holds no signing capability).
pub fn parse_image(image: &[u8]) -> Result<ExecutableImageV2<'_>, ExecError> {
    loader::parse_image(image, &ATXF_DEV_VERIFYING_KEY).map_err(ExecError::from)
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
        "ATXF v3 loaded: base=0x{:X} entry=0x{:X} segments={} relocations={} W^X=enabled",
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
