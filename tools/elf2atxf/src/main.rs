// EFI/PE to ATXF converter tool
// Converts PE/EFI binaries to Atom's ATXF executable format

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

const ATXF_MAGIC: u32 = 0x46585441; // 'ATXF'
const ATXF_VERSION: u16 = 1;
const PAGE_SIZE: usize = 0x1000;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
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
    _padding: u32,
}

fn read_u16_le(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[0], data[1]])
}

fn read_u32_le(data: &[u8]) -> u32 {
    u32::from_le_bytes([data[0], data[1], data[2], data[3]])
}

fn parse_pe_binary(data: &[u8]) -> io::Result<(Vec<u8>, u32)> {
    // Check MZ header
    if data.len() < 64 || &data[0..2] != b"MZ" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Not a valid PE file"));
    }

    // Get PE header offset
    let pe_offset = read_u32_le(&data[0x3c..0x40]) as usize;

    if data.len() < pe_offset + 4 || &data[pe_offset..pe_offset + 4] != b"PE\0\0" {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid PE signature"));
    }

    // Read COFF header
    let num_sections = read_u16_le(&data[pe_offset + 6..pe_offset + 8]);
    let optional_header_size = read_u16_le(&data[pe_offset + 20..pe_offset + 22]);

    // Read optional header to get entry point
    let entry_point_rva = read_u32_le(&data[pe_offset + 24 + 16..pe_offset + 24 + 20]);

    // Section headers start after optional header
    let section_header_offset = pe_offset + 24 + optional_header_size as usize;

    // Find .text section
    let mut text_data = Vec::new();
    let mut text_rva = 0u32;

    for i in 0..num_sections {
        let offset = section_header_offset + i as usize * 40;
        if offset + 40 > data.len() {
            break;
        }

        let name_bytes = &data[offset..offset + 8];
        let name = String::from_utf8_lossy(name_bytes).trim_end_matches('\0').to_string();

        let virtual_address = read_u32_le(&data[offset + 12..offset + 16]);
        let raw_size = read_u32_le(&data[offset + 16..offset + 20]) as usize;
        let raw_offset = read_u32_le(&data[offset + 20..offset + 24]) as usize;

        if name.contains(".text") {
            if raw_offset + raw_size > data.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Text section extends beyond file",
                ));
            }
            text_data = data[raw_offset..raw_offset + raw_size].to_vec();
            text_rva = virtual_address;
            eprintln!("Found .text section: RVA=0x{:x}, size={}", virtual_address, raw_size);
            break;
        }
    }

    if text_data.is_empty() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "No .text section found"));
    }

    // Calculate entry offset relative to text section
    let entry_offset = entry_point_rva.saturating_sub(text_rva);

    Ok((text_data, entry_offset))
}

fn create_atxf(text_data: Vec<u8>, entry_offset: u32, bss_size: u32) -> io::Result<Vec<u8>> {
    let header_size = std::mem::size_of::<AtxfHeader>();
    let text_offset = PAGE_SIZE;
    let text_size = text_data.len() as u32;

    // Align to page boundary
    let text_end = text_offset + text_data.len();
    let data_offset = ((text_end + PAGE_SIZE - 1) / PAGE_SIZE) * PAGE_SIZE;

    let header = AtxfHeader {
        magic: ATXF_MAGIC,
        version: ATXF_VERSION,
        header_size: header_size as u16,
        entry_offset,
        text_offset: text_offset as u32,
        text_size,
        data_offset: data_offset as u32,
        data_size: 0,
        bss_size,
        _padding: 0,
    };

    let mut output = Vec::new();

    // Write header
    unsafe {
        let header_bytes = std::slice::from_raw_parts(
            &header as *const AtxfHeader as *const u8,
            header_size,
        );
        output.extend_from_slice(header_bytes);
    }

    // Pad to text offset
    output.resize(text_offset, 0);

    // Write text section
    output.extend_from_slice(&text_data);

    // Pad to next page boundary
    let final_size = ((output.len() + PAGE_SIZE - 1) / PAGE_SIZE) * PAGE_SIZE;
    output.resize(final_size, 0);

    eprintln!(
        "Created ATXF: text={} bytes, entry=0x{:x}, bss={} bytes",
        text_size, entry_offset, bss_size
    );

    Ok(output)
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <input.efi> <output.atxf> [bss_size]", args[0]);
        eprintln!("  bss_size: BSS section size in bytes (default: 4096)");
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];
    let bss_size = if args.len() >= 4 {
        args[3].parse().unwrap_or(4096)
    } else {
        4096
    };

    eprintln!("Converting {} to ATXF format", input_path);

    // Read input file
    let input_data = fs::read(input_path)?;
    eprintln!("Input file size: {} bytes", input_data.len());

    // Parse PE binary
    let (text_data, entry_offset) = parse_pe_binary(&input_data)?;

    // Create ATXF
    let atxf_data = create_atxf(text_data, entry_offset, bss_size)?;

    // Write output file
    fs::write(output_path, &atxf_data)?;
    eprintln!("Written {} bytes to {}", atxf_data.len(), output_path);

    Ok(())
}
