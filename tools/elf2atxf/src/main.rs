use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::env;
use std::fs;
use std::io;

const ATXF_MAGIC: u32 = 0x4154_5846;
const ATXF_VERSION: u16 = 2;
const ATXF_FLAG_PIE: u32 = 1;
const ATXF_SIGNATURE_SIZE: usize = 32;
const ATXF_PAGE_SIZE: u64 = 4096;
const PRODUCT_HMAC_KEY: &[u8; 32] = b"AtomOS-ATXF-v2-product-key-0001!";
const SEGMENT_TEXT: u32 = 1;
const SEGMENT_RODATA: u32 = 2;
const SEGMENT_DATA: u32 = 3;
const SEGMENT_BSS: u32 = 4;
const PERM_READ: u32 = 1;
const PERM_WRITE: u32 = 2;
const PERM_EXECUTE: u32 = 4;
const KNOWN_PERMISSIONS: u32 = PERM_READ | PERM_WRITE | PERM_EXECUTE;
const RELOCATION_RELATIVE64: u32 = 1;

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct AtxfV2Header {
    magic: u32,
    version: u16,
    header_size: u16,
    flags: u32,
    entry_offset: u64,
    segment_count: u32,
    relocation_count: u32,
    segment_table_offset: u64,
    relocation_table_offset: u64,
    signature_offset: u64,
    signature_size: u32,
    reserved: u32,
    image_size: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct AtxfV2Segment {
    kind: u32,
    permissions: u32,
    file_offset: u64,
    file_size: u64,
    mem_size: u64,
    virtual_offset: u64,
    align: u64,
}

#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
struct AtxfV2Relocation {
    offset: u64,
    kind: u32,
    reserved: u32,
    addend: i64,
}

const HEADER_SIZE: usize = std::mem::size_of::<AtxfV2Header>();
const SEGMENT_SIZE: usize = std::mem::size_of::<AtxfV2Segment>();
const RELOCATION_SIZE: usize = std::mem::size_of::<AtxfV2Relocation>();

fn compute_image_mac(
    image: &[u8],
    signature_offset: usize,
    signature_size: usize,
) -> Option<[u8; ATXF_SIGNATURE_SIZE]> {
    let signature_end = signature_offset.checked_add(signature_size)?;
    if signature_size != ATXF_SIGNATURE_SIZE || signature_end > image.len() {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(PRODUCT_HMAC_KEY).ok()?;
    mac.update(&image[..signature_offset]);
    mac.update(&[0u8; ATXF_SIGNATURE_SIZE]);
    mac.update(&image[signature_end..]);
    let bytes = mac.finalize().into_bytes();
    let mut result = [0u8; ATXF_SIGNATURE_SIZE];
    result.copy_from_slice(&bytes);
    Some(result)
}

const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;
const SHT_RELA: u32 = 4;
const R_X86_64_RELATIVE: u32 = 8;

#[derive(Clone)]
struct OutputSegment {
    descriptor: AtxfV2Segment,
    bytes: Vec<u8>,
}

fn fail(message: impl AsRef<str>) -> ! {
    eprintln!("elf2atxf: error: {}", message.as_ref());
    std::process::exit(1);
}

fn checked_slice(data: &[u8], offset: u64, size: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(size).ok()?;
    let end = start.checked_add(len)?;
    data.get(start..end)
}

fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64_at(data: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn i64_at(data: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_le_bytes(
        data.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn align_up(value: usize, alignment: usize) -> usize {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .unwrap_or_else(|| fail("layout overflow"))
}

fn append_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_i64(out: &mut Vec<u8>, value: i64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn append_header(out: &mut Vec<u8>, header: AtxfV2Header) {
    append_u32(out, header.magic);
    append_u16(out, header.version);
    append_u16(out, header.header_size);
    append_u32(out, header.flags);
    append_u64(out, header.entry_offset);
    append_u32(out, header.segment_count);
    append_u32(out, header.relocation_count);
    append_u64(out, header.segment_table_offset);
    append_u64(out, header.relocation_table_offset);
    append_u64(out, header.signature_offset);
    append_u32(out, header.signature_size);
    append_u32(out, header.reserved);
    append_u64(out, header.image_size);
}

fn append_segment(out: &mut Vec<u8>, segment: AtxfV2Segment) {
    append_u32(out, segment.kind);
    append_u32(out, segment.permissions);
    append_u64(out, segment.file_offset);
    append_u64(out, segment.file_size);
    append_u64(out, segment.mem_size);
    append_u64(out, segment.virtual_offset);
    append_u64(out, segment.align);
}

fn append_relocation(out: &mut Vec<u8>, relocation: AtxfV2Relocation) {
    append_u64(out, relocation.offset);
    append_u32(out, relocation.kind);
    append_u32(out, relocation.reserved);
    append_i64(out, relocation.addend);
}

fn writable_target(segments: &[OutputSegment], offset: u64) -> bool {
    segments.iter().any(|segment| {
        let permissions = segment.descriptor.permissions;
        let start = segment.descriptor.virtual_offset;
        let end = start.saturating_add(segment.descriptor.mem_size);
        permissions & PERM_WRITE != 0
            && offset >= start
            && offset
                .checked_add(8)
                .is_some_and(|target_end| target_end <= end)
    })
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <input-pie.elf> <output.atxf>", args[0]);
        std::process::exit(2);
    }

    let elf = fs::read(&args[1])?;
    if elf.len() < 64 || elf[..4] != ELF_MAGIC {
        fail("input is not an ELF image");
    }
    if elf[4] != ELFCLASS64 || elf[5] != ELFDATA2LSB {
        fail("only little-endian ELF64 images are supported");
    }
    if u16_at(&elf, 16) != Some(ET_DYN) {
        fail("ELF is not PIE (expected ET_DYN)");
    }
    if u16_at(&elf, 18) != Some(EM_X86_64) {
        fail("ELF architecture is not x86_64");
    }

    let entry = u64_at(&elf, 24).unwrap_or_else(|| fail("truncated ELF header"));
    let phoff = u64_at(&elf, 32).unwrap_or_else(|| fail("truncated ELF header"));
    let shoff = u64_at(&elf, 40).unwrap_or_else(|| fail("truncated ELF header"));
    let phentsize = u16_at(&elf, 54).unwrap_or(0) as usize;
    let phnum = u16_at(&elf, 56).unwrap_or(0) as usize;
    let shentsize = u16_at(&elf, 58).unwrap_or(0) as usize;
    let shnum = u16_at(&elf, 60).unwrap_or(0) as usize;
    if phentsize < 56 || phnum == 0 {
        fail("ELF has no usable program header table");
    }

    let mut segments = Vec::new();
    for index in 0..phnum {
        let offset = usize::try_from(phoff)
            .ok()
            .and_then(|base| base.checked_add(index.checked_mul(phentsize)?))
            .unwrap_or_else(|| fail("program header table overflow"));
        if offset.checked_add(56).is_none_or(|end| end > elf.len()) {
            fail("truncated program header table");
        }
        if u32_at(&elf, offset) != Some(PT_LOAD) {
            continue;
        }

        let flags = u32_at(&elf, offset + 4).unwrap();
        let file_offset = u64_at(&elf, offset + 8).unwrap();
        let virtual_offset = u64_at(&elf, offset + 16).unwrap();
        let file_size = u64_at(&elf, offset + 32).unwrap();
        let mem_size = u64_at(&elf, offset + 40).unwrap();
        let align = u64_at(&elf, offset + 48).unwrap();

        if mem_size == 0 {
            continue;
        }
        if flags & PF_R == 0 {
            fail("PT_LOAD segment is not readable");
        }
        if flags & PF_W != 0 && flags & PF_X != 0 {
            fail("ELF contains a writable and executable PT_LOAD segment");
        }
        if virtual_offset % ATXF_PAGE_SIZE != 0 || align < ATXF_PAGE_SIZE {
            fail("PT_LOAD virtual address/alignment is not page aligned");
        }
        if mem_size < file_size {
            fail("PT_LOAD mem size is smaller than file size");
        }

        let permissions = PERM_READ
            | if flags & PF_W != 0 { PERM_WRITE } else { 0 }
            | if flags & PF_X != 0 { PERM_EXECUTE } else { 0 };
        if permissions & !KNOWN_PERMISSIONS != 0 {
            fail("unsupported segment permissions");
        }
        let kind = if flags & PF_X != 0 {
            SEGMENT_TEXT
        } else if flags & PF_W != 0 {
            SEGMENT_DATA
        } else {
            SEGMENT_RODATA
        };
        let bytes = checked_slice(&elf, file_offset, file_size)
            .unwrap_or_else(|| fail("PT_LOAD file range is outside ELF"))
            .to_vec();

        if mem_size > file_size && flags & PF_W == 0 {
            fail("non-writable PT_LOAD contains an unsupported zero-fill tail");
        }

        let data_mem_size = if file_size == 0 {
            0
        } else {
            mem_size.min(align_up(file_size as usize, ATXF_PAGE_SIZE as usize) as u64)
        };
        if data_mem_size != 0 {
            segments.push(OutputSegment {
                descriptor: AtxfV2Segment {
                    kind,
                    permissions,
                    file_offset: 0,
                    file_size,
                    mem_size: data_mem_size,
                    virtual_offset,
                    align: ATXF_PAGE_SIZE,
                },
                bytes,
            });
        }

        if mem_size > data_mem_size {
            let bss_offset = virtual_offset
                .checked_add(data_mem_size)
                .unwrap_or_else(|| fail("BSS virtual range overflow"));
            if bss_offset % ATXF_PAGE_SIZE != 0 {
                fail("BSS split is not page aligned");
            }
            segments.push(OutputSegment {
                descriptor: AtxfV2Segment {
                    kind: SEGMENT_BSS,
                    permissions: PERM_READ | PERM_WRITE,
                    file_offset: 0,
                    file_size: 0,
                    mem_size: mem_size - data_mem_size,
                    virtual_offset: bss_offset,
                    align: ATXF_PAGE_SIZE,
                },
                bytes: Vec::new(),
            });
        }
    }

    if segments.is_empty() {
        fail("ELF has no loadable segments");
    }
    segments.sort_by_key(|segment| segment.descriptor.virtual_offset);
    for pair in segments.windows(2) {
        let left_end = pair[0]
            .descriptor
            .virtual_offset
            .checked_add(pair[0].descriptor.mem_size)
            .unwrap_or_else(|| fail("segment virtual range overflow"));
        if left_end > pair[1].descriptor.virtual_offset {
            fail("ELF PT_LOAD segments overlap");
        }
    }
    if !segments.iter().any(|segment| {
        segment.descriptor.permissions & PERM_EXECUTE != 0
            && entry >= segment.descriptor.virtual_offset
            && entry
                < segment
                    .descriptor
                    .virtual_offset
                    .saturating_add(segment.descriptor.mem_size)
    }) {
        fail("entry point is outside executable memory");
    }

    let mut relocations = Vec::new();
    if shoff != 0 && shnum != 0 {
        if shentsize < 64 {
            fail("invalid ELF section header size");
        }
        for index in 0..shnum {
            let offset = usize::try_from(shoff)
                .ok()
                .and_then(|base| base.checked_add(index.checked_mul(shentsize)?))
                .unwrap_or_else(|| fail("section header table overflow"));
            if offset.checked_add(64).is_none_or(|end| end > elf.len()) {
                fail("truncated section header table");
            }
            if u32_at(&elf, offset + 4) != Some(SHT_RELA) {
                continue;
            }
            let section_offset = u64_at(&elf, offset + 24).unwrap();
            let section_size = u64_at(&elf, offset + 32).unwrap();
            let entry_size = u64_at(&elf, offset + 56).unwrap();
            if entry_size != 24 || section_size % entry_size != 0 {
                fail("unsupported ELF RELA table layout");
            }
            let rela = checked_slice(&elf, section_offset, section_size)
                .unwrap_or_else(|| fail("RELA section is outside ELF"));
            for record in rela.chunks_exact(24) {
                let target = u64_at(record, 0).unwrap();
                let info = u64_at(record, 8).unwrap();
                let addend = i64_at(record, 16).unwrap();
                let kind = info as u32;
                let symbol = info >> 32;
                if kind != R_X86_64_RELATIVE || symbol != 0 {
                    fail(format!(
                        "unsupported relocation type {} (symbol {})",
                        kind, symbol
                    ));
                }
                if !writable_target(&segments, target) {
                    fail("relocation target is not inside a writable segment");
                }
                relocations.push(AtxfV2Relocation {
                    offset: target,
                    kind: RELOCATION_RELATIVE64,
                    reserved: 0,
                    addend,
                });
            }
        }
    }

    let segment_table_offset = HEADER_SIZE;
    let relocation_table_offset = segment_table_offset + segments.len() * SEGMENT_SIZE;
    let metadata_end = relocation_table_offset + relocations.len() * RELOCATION_SIZE;
    let mut cursor = align_up(metadata_end, ATXF_PAGE_SIZE as usize);
    for segment in &mut segments {
        if !segment.bytes.is_empty() {
            segment.descriptor.file_offset = cursor as u64;
            cursor = align_up(cursor + segment.bytes.len(), ATXF_PAGE_SIZE as usize);
        }
    }
    let signature_offset = cursor;
    let image_size = signature_offset + ATXF_SIGNATURE_SIZE;

    let header = AtxfV2Header {
        magic: ATXF_MAGIC,
        version: ATXF_VERSION,
        header_size: HEADER_SIZE as u16,
        flags: ATXF_FLAG_PIE,
        entry_offset: entry,
        segment_count: segments.len() as u32,
        relocation_count: relocations.len() as u32,
        segment_table_offset: segment_table_offset as u64,
        relocation_table_offset: relocation_table_offset as u64,
        signature_offset: signature_offset as u64,
        signature_size: ATXF_SIGNATURE_SIZE as u32,
        reserved: 0,
        image_size: image_size as u64,
    };

    let mut output = Vec::with_capacity(image_size);
    append_header(&mut output, header);
    for segment in &segments {
        append_segment(&mut output, segment.descriptor);
    }
    for relocation in relocations.iter().copied() {
        append_relocation(&mut output, relocation);
    }
    output.resize(align_up(output.len(), ATXF_PAGE_SIZE as usize), 0);
    for segment in &segments {
        if segment.bytes.is_empty() {
            continue;
        }
        output.resize(segment.descriptor.file_offset as usize, 0);
        output.extend_from_slice(&segment.bytes);
        output.resize(align_up(output.len(), ATXF_PAGE_SIZE as usize), 0);
    }
    output.resize(signature_offset, 0);
    output.extend_from_slice(&[0u8; ATXF_SIGNATURE_SIZE]);
    let mac = compute_image_mac(&output, signature_offset, ATXF_SIGNATURE_SIZE)
        .unwrap_or_else(|| fail("failed to authenticate output image"));
    output[signature_offset..signature_offset + ATXF_SIGNATURE_SIZE].copy_from_slice(&mac);

    fs::write(&args[2], &output)?;
    println!(
        "elf2atxf: ATXF v2 PIE: {} segments, {} relocations, entry=0x{:x}, {} bytes",
        segments.len(),
        relocations.len(),
        entry,
        output.len()
    );
    Ok(())
}
