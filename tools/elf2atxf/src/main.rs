//! ELF to ATXF converter
//!
//! Converts an ELF binary to ATXF (Atom eXecutable Format) for loading
//! by the Atom kernel.

use object::{Object, ObjectSection, ObjectSymbol};
use std::env;
use std::fs::File;
use std::io::{Read, Write};

const ATXF_MAGIC: u32 = 0x41545846; // "ATXF"
const ATXF_VERSION: u16 = 1;
const HEADER_SIZE: u16 = 32;
const PAGE_SIZE: usize = 4096;

#[repr(C, packed)]
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

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!("Usage: {} <input.elf> <output.atxf>", args[0]);
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    // Read ELF file
    let mut file = File::open(input_path).expect("Failed to open input file");
    let mut data = Vec::new();
    file.read_to_end(&mut data).expect("Failed to read input file");

    let obj = object::File::parse(&*data).expect("Failed to parse ELF file");

    // Get entry point
    let entry = obj.entry();

    // Collect section data and find text base address
    // We need to preserve inter-section padding based on virtual addresses
    let mut text_data = Vec::new();
    let mut text_vaddr = 0u64;
    let mut data_data = Vec::new();
    let mut data_base_vaddr = 0u64;  // Virtual address of first data section
    let mut bss_size = 0usize;

    // First pass: find the base virtual address of the data region
    for section in obj.sections() {
        let name = section.name().unwrap_or("");
        match name {
            ".rodata" | ".got" | ".data" => {
                if data_base_vaddr == 0 {
                    data_base_vaddr = section.address();
                }
            }
            _ => {}
        }
    }

    // Second pass: collect sections with proper padding
    for section in obj.sections() {
        let name = section.name().unwrap_or("");
        let size = section.size() as usize;
        let vaddr = section.address();

        match name {
            ".text" => {
                text_vaddr = vaddr;
                if let Ok(section_data) = section.data() {
                    text_data.extend_from_slice(section_data);
                    println!(".text: {} bytes at vaddr 0x{:x}", section_data.len(), text_vaddr);
                }
            }
            ".rodata" | ".got" | ".data" => {
                if let Ok(section_data) = section.data() {
                    // Calculate expected offset from data base
                    let expected_offset = (vaddr - data_base_vaddr) as usize;
                    let current_offset = data_data.len();

                    // Add padding if needed to match virtual address alignment
                    if expected_offset > current_offset {
                        let padding = expected_offset - current_offset;
                        data_data.extend(std::iter::repeat(0u8).take(padding));
                        println!("{}: {} bytes at vaddr 0x{:x} (added {} padding bytes)",
                                 name, section_data.len(), vaddr, padding);
                    } else {
                        println!("{}: {} bytes at vaddr 0x{:x}", name, section_data.len(), vaddr);
                    }

                    data_data.extend_from_slice(section_data);
                }
            }
            ".bss" => {
                bss_size = size;
                println!(".bss: {} bytes", size);
            }
            _ => {}
        }
    }

    // Calculate entry offset from actual text base
    let entry_offset = (entry - text_vaddr) as u32;

    println!("Entry point: 0x{:x}", entry);
    println!("Text base: 0x{:x}", text_vaddr);
    println!("Entry offset: 0x{:x}", entry_offset);

    // Calculate offsets (page-aligned)
    let text_offset = PAGE_SIZE; // Header takes first page
    let text_size_aligned = align_up(text_data.len(), PAGE_SIZE);
    let data_offset = text_offset + text_size_aligned;
    let data_size_aligned = if data_data.is_empty() { 0 } else { align_up(data_data.len(), PAGE_SIZE) };

    // Build header
    let header = AtxfHeader {
        magic: ATXF_MAGIC,
        version: ATXF_VERSION,
        header_size: HEADER_SIZE,
        entry_offset,
        text_offset: text_offset as u32,
        text_size: text_data.len() as u32,
        data_offset: data_offset as u32,
        data_size: data_data.len() as u32,
        bss_size: bss_size as u32,
    };

    // Write output
    let mut output = File::create(output_path).expect("Failed to create output file");

    // Write header
    let header_bytes: [u8; 32] = unsafe { std::mem::transmute(header) };
    output.write_all(&header_bytes).expect("Failed to write header");

    // Pad header to page boundary
    let padding = PAGE_SIZE - header_bytes.len();
    output.write_all(&vec![0u8; padding]).expect("Failed to write header padding");

    // Write text section
    output.write_all(&text_data).expect("Failed to write text section");
    let text_padding = text_size_aligned - text_data.len();
    if text_padding > 0 {
        output.write_all(&vec![0u8; text_padding]).expect("Failed to write text padding");
    }

    // Write data section
    if !data_data.is_empty() {
        output.write_all(&data_data).expect("Failed to write data section");
        let data_padding = data_size_aligned - data_data.len();
        if data_padding > 0 {
            output.write_all(&vec![0u8; data_padding]).expect("Failed to write data padding");
        }
    }

    let total_size = PAGE_SIZE + text_size_aligned + data_size_aligned;

    println!("\nATXF binary created: {}", output_path);
    println!("  Entry offset: 0x{:x}", entry_offset);
    println!("  Text: {} bytes at offset 0x{:x}", text_data.len(), text_offset);
    println!("  Data: {} bytes at offset 0x{:x}", data_data.len(), data_offset);
    println!("  BSS: {} bytes", bss_size);
    println!("  Total: {} bytes", total_size);
}
