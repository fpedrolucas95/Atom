// elf2atxf - Convert ELF binaries to Atom ATXF executable format
//
// ATXF (Atom Executable Format) is a simple executable format for Atom OS:
// - Magic: 0x41545846 ("ATXF" in little-endian)
// - Version: 1
// - Fixed header with section offsets and sizes
// - .text section (code, read-only)
// - .data section (initialized data, writable)
// - .bss size (zero-initialized data, writable)
//
// Usage: elf2atxf <input.elf> <output.atxf>

use std::env;
use std::fs::{self, File};
use std::io::{self, Write};

const ATXF_MAGIC: u32 = 0x4154_5846; // "ATXF" in ASCII, little-endian
const ATXF_VERSION: u16 = 1;
const PAGE_SIZE: usize = 4096;

// ELF constants
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;
const PF_X: u32 = 0x1;
const PF_W: u32 = 0x2;

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct AtxfHeader {
    magic: u32,           // 0x41545846 = "ATXF"
    version: u16,         // 1
    header_size: u16,     // Size of this header
    entry_offset: u32,    // Entry point relative to text base
    text_offset: u32,     // Offset of .text in file (page-aligned)
    text_size: u32,       // Size of .text section
    data_offset: u32,     // Offset of .data in file (page-aligned)
    data_size: u32,       // Size of .data section
    bss_size: u32,        // Size of .bss (zeroed, not in file)
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Elf64Header {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

fn read_u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn read_u32_le(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        eprintln!("Usage: {} <input.elf> <output.atxf>", args[0]);
        eprintln!();
        eprintln!("Convert ELF binary to Atom ATXF executable format.");
        eprintln!();
        eprintln!("The input ELF should be a statically linked executable");
        eprintln!("compiled with:");
        eprintln!("  cargo build --target x86_64-unknown-none --release");
        std::process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    println!("elf2atxf: Converting {} -> {}", input_path, output_path);

    // Read the ELF file
    let elf_data = fs::read(input_path)?;

    // Verify ELF magic
    if elf_data.len() < 64 {
        eprintln!("Error: File too small to be a valid ELF");
        std::process::exit(1);
    }

    if elf_data[0..4] != ELF_MAGIC {
        eprintln!("Error: Not a valid ELF file (bad magic)");
        std::process::exit(1);
    }

    // Verify 64-bit ELF
    if elf_data[4] != ELFCLASS64 {
        eprintln!("Error: Not a 64-bit ELF");
        std::process::exit(1);
    }

    // Parse ELF header manually
    let e_machine = read_u16_le(&elf_data, 18);
    if e_machine != EM_X86_64 {
        eprintln!("Error: ELF is not x86_64 (machine type: {})", e_machine);
        std::process::exit(1);
    }

    let e_entry = read_u64_le(&elf_data, 24);
    let e_phoff = read_u64_le(&elf_data, 32);
    let e_phentsize = read_u16_le(&elf_data, 54);
    let e_phnum = read_u16_le(&elf_data, 56);

    println!("  Entry point: 0x{:X}", e_entry);
    println!("  Program headers: {} at offset 0x{:X}", e_phnum, e_phoff);

    // Extract sections from program headers
    let mut text_data: Vec<u8> = Vec::new();
    let mut data_data: Vec<u8> = Vec::new();
    let mut bss_size: usize = 0;
    let mut text_vaddr: u64 = 0;
    let mut base_addr: u64 = u64::MAX;

    // First pass: find base address
    for i in 0..e_phnum as usize {
        let phdr_offset = e_phoff as usize + i * e_phentsize as usize;
        if phdr_offset + 56 > elf_data.len() {
            break;
        }

        let p_type = read_u32_le(&elf_data, phdr_offset);
        let p_vaddr = read_u64_le(&elf_data, phdr_offset + 16);

        if p_type == PT_LOAD && p_vaddr < base_addr {
            base_addr = p_vaddr;
        }
    }

    if base_addr == u64::MAX {
        base_addr = 0;
    }
    println!("  Base address: 0x{:X}", base_addr);

    // Second pass: extract segments
    for i in 0..e_phnum as usize {
        let phdr_offset = e_phoff as usize + i * e_phentsize as usize;
        if phdr_offset + 56 > elf_data.len() {
            break;
        }

        let p_type = read_u32_le(&elf_data, phdr_offset);
        let p_flags = read_u32_le(&elf_data, phdr_offset + 4);
        let p_offset = read_u64_le(&elf_data, phdr_offset + 8);
        let p_vaddr = read_u64_le(&elf_data, phdr_offset + 16);
        let p_filesz = read_u64_le(&elf_data, phdr_offset + 32);
        let p_memsz = read_u64_le(&elf_data, phdr_offset + 40);

        if p_type != PT_LOAD {
            continue;
        }

        let start = p_offset as usize;
        let file_size = p_filesz as usize;
        let mem_size = p_memsz as usize;
        let end = start + file_size;

        // Check if executable (text) or writable (data)
        let is_executable = p_flags & PF_X != 0;
        let is_writable = p_flags & PF_W != 0;

        if is_executable {
            // Text segment
            if end <= elf_data.len() {
                text_data.extend_from_slice(&elf_data[start..end]);
                text_vaddr = p_vaddr;
                println!("  Text segment: {} bytes at 0x{:X}", file_size, p_vaddr);
            }
        } else if is_writable || file_size > 0 {
            // Data segment
            if end <= elf_data.len() && file_size > 0 {
                data_data.extend_from_slice(&elf_data[start..end]);
                println!("  Data segment: {} bytes at 0x{:X}", file_size, p_vaddr);
            }
            // BSS is the difference between memory size and file size
            if mem_size > file_size {
                bss_size += mem_size - file_size;
                println!("  BSS: {} bytes", mem_size - file_size);
            }
        }
    }

    // If text is empty, we have a problem
    if text_data.is_empty() {
        eprintln!("Error: No text/code section found in ELF");
        std::process::exit(1);
    }

    // Calculate entry offset relative to text base
    let entry_offset = if text_vaddr > 0 && e_entry >= text_vaddr {
        (e_entry - text_vaddr) as u32
    } else if e_entry >= base_addr {
        (e_entry - base_addr) as u32
    } else {
        e_entry as u32
    };

    println!("  Entry offset: 0x{:X}", entry_offset);

    // Build ATXF file
    let header_size = std::mem::size_of::<AtxfHeader>();
    let text_offset = align_up(header_size, PAGE_SIZE);
    let text_size = text_data.len();
    let data_offset = align_up(text_offset + text_size, PAGE_SIZE);
    let data_size = data_data.len();

    let header = AtxfHeader {
        magic: ATXF_MAGIC,
        version: ATXF_VERSION,
        header_size: header_size as u16,
        entry_offset,
        text_offset: text_offset as u32,
        text_size: text_size as u32,
        data_offset: data_offset as u32,
        data_size: data_size as u32,
        bss_size: bss_size as u32,
    };

    // Write ATXF file
    let mut output = File::create(output_path)?;

    // Write header as bytes
    unsafe {
        let header_bytes = std::slice::from_raw_parts(
            &header as *const AtxfHeader as *const u8,
            header_size,
        );
        output.write_all(header_bytes)?;
    }

    // Pad to text offset
    let padding_to_text = text_offset - header_size;
    output.write_all(&vec![0u8; padding_to_text])?;

    // Write text section
    output.write_all(&text_data)?;

    // Pad to data offset
    let current_pos = text_offset + text_size;
    if data_size > 0 {
        let padding_to_data = data_offset - current_pos;
        output.write_all(&vec![0u8; padding_to_data])?;

        // Write data section
        output.write_all(&data_data)?;
    }

    let total_size = if data_size > 0 {
        data_offset + data_size
    } else {
        text_offset + text_size
    };

    println!();
    println!("ATXF created successfully:");
    println!("  Magic:        0x{:08X} (\"ATXF\")", ATXF_MAGIC);
    println!("  Version:      {}", ATXF_VERSION);
    println!("  Header size:  {} bytes", header_size);
    println!("  Entry offset: 0x{:X}", entry_offset);
    println!("  Text:         offset=0x{:X}, size={} bytes", text_offset, text_size);
    println!("  Data:         offset=0x{:X}, size={} bytes", data_offset, data_size);
    println!("  BSS:          {} bytes", bss_size);
    println!("  Total size:   {} bytes", total_size);

    Ok(())
}
