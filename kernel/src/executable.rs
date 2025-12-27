// User executable loading and format handling
//
// This module implements support for user-space executables in the kernel,
// including format validation, section layout, and safe loading into a
// user address space.
//
// Key responsibilities:
// - Define and validate the ATXF executable format
// - Parse executable headers and sections (.text, .data, .bss)
// - Load executables into user address spaces
// - Allocate and map physical memory for code and data
// - Provide automatic rollback on partial failure
//
// MICROKERNEL ARCHITECTURE:
// - The bootloader MUST provide a valid ui_shell.atxf payload
// - There is NO embedded fallback image
// - If the payload is missing or invalid, the system MUST fail
// - This enforces proper separation of kernel and userspace
//
// Design and implementation:
// - Simple format with a fixed header and explicit offsets
// - Sections aligned to page boundaries (4 KiB)
// - Fixed load base for user executables
// - Explicit use of PMM and VMM for allocation and mapping
// - RollbackGuard ensures consistent cleanup on failures
//
// Safety and correctness notes:
// - Executables are validated before any mapping occurs
// - Layout is checked against canonical user address limits
// - Mapping failures release all previously allocated memory
// - Raw pointers are used only for controlled data copying
//
// Limitations and future considerations:
// - No relocation or ASLR support
// - Format supports only a single text and data segment
// - No explicit executable permission enforcement
// - Loading assumes a trusted executable from boot/init
//
// Public interface:
// - `load_boot_payload` to load init provided at boot
// - `load_into_address_space` to load generic executables
// - `ExecError` for detailed failure diagnostics

use alloc::vec::Vec;
use core::mem::size_of;
use core::ptr;

use crate::boot::ExecutableImage;
use crate::mm::{addrspace, pmm};
use crate::mm::addrspace::{AddressSpaceId, USER_CANONICAL_MAX};
use crate::mm::vm::PageFlags;
use crate::thread::ThreadId;
use crate::{log_error, log_info, log_warn};

#[allow(dead_code)]
const LOG_ORIGIN: &str = "exec";

/// ATXF executable format magic number: "ATXF" in little-endian
pub const ATXF_MAGIC: u32 = 0x4154_5846;

/// Current ATXF format version
pub const ATXF_VERSION: u16 = 1;

/// Base address where user executables are loaded
pub const USER_EXEC_LOAD_BASE: usize = 0x0040_0000;

#[allow(dead_code)]
#[derive(Debug)]
pub enum ExecError {
    MissingImage,
    InvalidMagic,
    UnsupportedVersion(u16),
    Truncated,
    MisalignedSection,
    OverlappingSection,
    EntryOutOfBounds,
    OutOfMemory,
    AddressSpace(addrspace::AddressSpaceError),
    NonCanonicalLayout,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct AtxfHeader {
    magic: u32,
    version: u16,
    header_size: u16,
    entry_offset: u32,
    text_offset: u32,
    text_size: u32,
    data_offset: u32,
    data_size: u32,
    bss_size: u32,
}

#[derive(Clone, Copy)]
pub struct ExecutableSections<'a> {
    pub entry_offset: usize,
    pub text: &'a [u8],
    pub data: &'a [u8],
    pub bss_size: usize,
}

#[allow(dead_code)]
pub struct LoadedExecutable {
    pub entry_point: usize,
    pub text_base: usize,
    pub data_base: usize,
    pub bss_base: usize,
}

#[allow(dead_code)]
pub fn log_format_overview() {
    log_info!(
        LOG_ORIGIN,
        "Executable format active: magic=0x{:X}, version={}, sections=.text/.data/.bss",
        ATXF_MAGIC,
        ATXF_VERSION
    );
    log_info!(
        LOG_ORIGIN,
        "Load base: 0x{:X}, page size: {} bytes, entry offset relative to base",
        USER_EXEC_LOAD_BASE,
        pmm::PAGE_SIZE
    );
}

#[allow(dead_code)]
pub fn summarize_boot_payload(payload: &ExecutableImage) {
    if !payload.is_present() {
        log_warn!(LOG_ORIGIN, "Bootloader did not provide a user executable payload");
        return;
    }

    log_info!(
        LOG_ORIGIN,
        "Boot payload located at 0x{:X} ({} bytes)",
        payload.ptr as usize,
        payload.size
    );

    match parse_boot_image(payload) {
        Ok(sections) => {
            log_info!(
                LOG_ORIGIN,
                "Payload validated: text={} bytes, data={} bytes, bss={} bytes, entry offset=0x{:X}",
                sections.text.len(),
                sections.data.len(),
                sections.bss_size,
                sections.entry_offset
            );
        }
        Err(err) => {
            log_error!(LOG_ORIGIN, "Payload validation failed: {:?}", err);
        }
    }
}

pub fn parse_boot_image(payload: &ExecutableImage) -> Result<ExecutableSections<'_>, ExecError> {
    if !payload.is_present() {
        return Err(ExecError::MissingImage);
    }

    let bytes = unsafe { core::slice::from_raw_parts(payload.ptr, payload.size) };
    parse_image(bytes)
}

pub fn parse_image<'a>(image: &'a [u8]) -> Result<ExecutableSections<'a>, ExecError> {
    if image.len() < size_of::<AtxfHeader>() {
        return Err(ExecError::Truncated);
    }

    let mut raw = AtxfHeader {
        magic: 0,
        version: 0,
        header_size: 0,
        entry_offset: 0,
        text_offset: 0,
        text_size: 0,
        data_offset: 0,
        data_size: 0,
        bss_size: 0,
    };

    unsafe {
        let header_bytes = core::slice::from_raw_parts_mut(
            &mut raw as *mut AtxfHeader as *mut u8,
            size_of::<AtxfHeader>(),
        );
        header_bytes.copy_from_slice(&image[..size_of::<AtxfHeader>()]);
    }

    if raw.magic != ATXF_MAGIC {
        return Err(ExecError::InvalidMagic);
    }

    if raw.version != ATXF_VERSION {
        return Err(ExecError::UnsupportedVersion(raw.version));
    }

    let header_size = raw.header_size as usize;
    if header_size < size_of::<AtxfHeader>() {
        return Err(ExecError::Truncated);
    }

    if raw.text_offset as usize % pmm::PAGE_SIZE != 0 || raw.data_offset as usize % pmm::PAGE_SIZE != 0 {
        return Err(ExecError::MisalignedSection);
    }

    if (raw.text_offset as usize) < header_size || (raw.data_offset as usize) < header_size {
        return Err(ExecError::OverlappingSection);
    }

    if raw.text_offset as usize + raw.text_size as usize > image.len() {
        return Err(ExecError::Truncated);
    }

    let data_end = raw.data_offset as usize + raw.data_size as usize;
    if data_end > image.len() {
        return Err(ExecError::Truncated);
    }

    if (raw.text_offset <= raw.data_offset && raw.text_offset + raw.text_size > raw.data_offset)
        || (raw.data_offset <= raw.text_offset && raw.data_offset + raw.data_size > raw.text_offset)
    {
        return Err(ExecError::OverlappingSection);
    }

    if raw.entry_offset as usize >= raw.text_size as usize {
        return Err(ExecError::EntryOutOfBounds);
    }

    if image.len() < header_size {
        return Err(ExecError::Truncated);
    }

    let text = &image[raw.text_offset as usize..raw.text_offset as usize + raw.text_size as usize];
    let data = &image[raw.data_offset as usize..raw.data_offset as usize + raw.data_size as usize];

    Ok(ExecutableSections {
        entry_offset: raw.entry_offset as usize,
        text,
        data,
        bss_size: raw.bss_size as usize,
    })
}

// NOTE: No embedded fallback image is provided.
// The bootloader MUST supply a valid ui_shell.atxf payload.
// If the payload is missing or invalid, the system will halt.
// This enforces proper microkernel architecture where all UI runs in userspace.

#[allow(dead_code)]
pub fn load_into_address_space(
    image: &[u8],
    address_space: AddressSpaceId,
    owner: ThreadId,
) -> Result<LoadedExecutable, ExecError> {
    let sections = parse_image(image)?;
    do_load(sections, address_space, owner)
}

#[allow(dead_code)]
pub fn load_boot_payload(
    payload: &ExecutableImage,
    address_space: AddressSpaceId,
    owner: ThreadId,
) -> Result<LoadedExecutable, ExecError> {
    let sections = parse_boot_image(payload)?;
    do_load(sections, address_space, owner)
}

fn do_load(
    sections: ExecutableSections,
    address_space: AddressSpaceId,
    owner: ThreadId,
) -> Result<LoadedExecutable, ExecError> {
    let text_base = USER_EXEC_LOAD_BASE;
    let text_size = pmm::align_up(sections.text.len());
    let data_base = pmm::align_up(text_base + text_size);
    let data_size = pmm::align_up(sections.data.len());
    let bss_base = pmm::align_up(data_base + data_size);
    let bss_size = pmm::align_up(sections.bss_size);

    let highest_virt = bss_base.saturating_add(bss_size);
    if highest_virt > USER_CANONICAL_MAX {
        log_error!(
            LOG_ORIGIN,
            "Executable layout exceeds canonical user range: top=0x{:X} limit=0x{:X}",
            highest_virt,
            USER_CANONICAL_MAX
        );
        return Err(ExecError::NonCanonicalLayout);
    }

    let mut rollback = RollbackGuard::new(address_space, owner);

    if let Some(mapping) = map_segment(
        address_space,
        owner,
        text_base,
        sections.text,
        PageFlags::PRESENT | PageFlags::USER,
    )? {
        rollback.track(mapping);
    }

    if let Some(mapping) = map_segment(
        address_space,
        owner,
        data_base,
        sections.data,
        PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
    )? {
        rollback.track(mapping);
    }

    if sections.bss_size > 0 {
        if let Some(mapping) = map_zeroed_segment(
            address_space,
            owner,
            bss_base,
            bss_size,
            PageFlags::PRESENT | PageFlags::USER | PageFlags::WRITABLE,
        )? {
            rollback.track(mapping);
        }
    }

    let entry_point = USER_EXEC_LOAD_BASE + sections.entry_offset;

    log_info!(
        LOG_ORIGIN,
        "Executable loaded: text=0x{:X}-0x{:X}, data=0x{:X}-0x{:X}, bss=0x{:X}-0x{:X}",
        text_base,
        text_base + text_size,
        data_base,
        data_base + data_size,
        bss_base,
        bss_base + bss_size
    );

    log_info!(LOG_ORIGIN, "Entry point set to 0x{:X}", entry_point);

    rollback.disarm();

    Ok(LoadedExecutable {
        entry_point,
        text_base,
        data_base,
        bss_base,
    })
}

fn map_segment(
    address_space: AddressSpaceId,
    owner: ThreadId,
    virt_start: usize,
    data: &[u8],
    flags: PageFlags,
) -> Result<Option<(usize, usize, usize)>, ExecError> {
    if data.is_empty() {
        return Ok(None);
    }

    let size = pmm::align_up(data.len());
    let pages = size / pmm::PAGE_SIZE;
    let phys_base = pmm::alloc_pages_zeroed(pages).ok_or(ExecError::OutOfMemory)?;

    unsafe {
        ptr::copy_nonoverlapping(data.as_ptr(), phys_base as *mut u8, data.len());
    }

    match addrspace::map_region(address_space, owner, virt_start, phys_base, size, flags) {
        Ok(()) => {
            Ok(Some((virt_start, phys_base, size)))
        }
        Err(err) => {
            pmm::free_pages(phys_base, pages);
            Err(ExecError::AddressSpace(err))
        }
    }
}

fn map_zeroed_segment(
    address_space: AddressSpaceId,
    owner: ThreadId,
    virt_start: usize,
    size: usize,
    flags: PageFlags,
) -> Result<Option<(usize, usize, usize)>, ExecError> {
    if size == 0 {
        return Ok(None);
    }

    let aligned_size = pmm::align_up(size);
    let pages = aligned_size / pmm::PAGE_SIZE;
    let phys_base = pmm::alloc_pages_zeroed(pages).ok_or(ExecError::OutOfMemory)?;

    match addrspace::map_region(address_space, owner, virt_start, phys_base, aligned_size, flags) {
        Ok(()) => {
            Ok(Some((virt_start, phys_base, aligned_size)))
        }
        Err(err) => {
            pmm::free_pages(phys_base, pages);
            Err(ExecError::AddressSpace(err))
        }
    }
}

impl Drop for RollbackGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }

        log_warn!(LOG_ORIGIN, "Rolling back partially mapped executable");

        for &(virt, phys, size) in self.mapped.iter().rev() {
            let pages = size / pmm::PAGE_SIZE;
            let _ = addrspace::unmap_region(self.address_space, self.owner, virt, size);
            pmm::free_pages(phys, pages);
        }
    }
}

struct RollbackGuard {
    mapped: Vec<(usize, usize, usize)>,
    address_space: AddressSpaceId,
    owner: ThreadId,
    active: bool,
}

impl RollbackGuard {
    fn new(address_space: AddressSpaceId, owner: ThreadId) -> Self {
        Self {
            mapped: Vec::new(),
            address_space,
            owner,
            active: true,
        }
    }

    fn track(&mut self, mapping: (usize, usize, usize)) {
        self.mapped.push(mapping);
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}