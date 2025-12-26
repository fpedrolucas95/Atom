#!/usr/bin/env python3
"""
Build ATXF (Atom eXecutable Format) binary from ELF.

ATXF Header (32 bytes):
  - magic: u32 (0x41545846 = "ATXF")
  - version: u16 (1)
  - header_size: u16 (32)
  - entry_offset: u32 (offset from text base)
  - text_offset: u32 (offset in file)
  - text_size: u32
  - data_offset: u32 (offset in file)
  - data_size: u32
  - bss_size: u32
"""

import struct
import subprocess
import sys
import os

def get_sections(elf_path):
    """Parse ELF sections using objdump"""
    result = subprocess.run(
        ['objdump', '-h', elf_path],
        capture_output=True, text=True
    )

    sections = {}
    for line in result.stdout.split('\n'):
        parts = line.split()
        if len(parts) >= 7 and parts[0].isdigit():
            name = parts[1]
            size = int(parts[2], 16)
            vma = int(parts[3], 16)
            sections[name] = {'size': size, 'vma': vma}

    return sections

def get_entry_point(elf_path):
    """Get entry point from ELF"""
    result = subprocess.run(
        ['readelf', '-h', elf_path],
        capture_output=True, text=True
    )

    for line in result.stdout.split('\n'):
        if 'Entry point' in line:
            return int(line.split(':')[1].strip(), 16)

    return 0

def extract_section(elf_path, output_path, sections):
    """Extract sections to raw binary"""
    args = ['objcopy', '-O', 'binary']
    for sec in sections:
        args.extend(['--only-section=' + sec])
    args.extend([elf_path, output_path])

    subprocess.run(args, check=True)

def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <input.elf> <output.atxf>")
        sys.exit(1)

    elf_path = sys.argv[1]
    output_path = sys.argv[2]

    # Parse ELF
    sections = get_sections(elf_path)
    entry = get_entry_point(elf_path)

    print(f"Sections found: {list(sections.keys())}")
    print(f"Entry point: 0x{entry:x}")

    # Get text base (should be 0x400000)
    text_vma = sections.get('.text', {}).get('vma', 0x400000)
    entry_offset = entry - text_vma

    # Calculate sizes
    text_size = sections.get('.text', {}).get('size', 0)
    rodata_size = sections.get('.rodata', {}).get('size', 0)
    got_size = sections.get('.got', {}).get('size', 0)
    bss_size = sections.get('.bss', {}).get('size', 0)

    # Text + rodata + got combined as "text"
    # For simplicity, extract all code/data sections together
    data_size = rodata_size + got_size

    # Page-align sizes
    PAGE_SIZE = 4096
    def align_up(x):
        return (x + PAGE_SIZE - 1) & ~(PAGE_SIZE - 1)

    text_size_aligned = align_up(text_size)
    data_size_aligned = align_up(data_size) if data_size > 0 else 0

    # ATXF header is 32 bytes
    HEADER_SIZE = 32
    text_offset = PAGE_SIZE  # Start text at page boundary
    data_offset = text_offset + text_size_aligned if data_size > 0 else text_offset + text_size_aligned

    # Extract raw binary for text section
    text_bin_path = elf_path + '.text.bin'
    extract_section(elf_path, text_bin_path, ['.text'])

    with open(text_bin_path, 'rb') as f:
        text_data = f.read()
    os.remove(text_bin_path)

    # Extract rodata + got as data
    data_data = b''
    if rodata_size > 0:
        data_bin_path = elf_path + '.data.bin'
        extract_section(elf_path, data_bin_path, ['.rodata', '.got'])
        with open(data_bin_path, 'rb') as f:
            data_data = f.read()
        os.remove(data_bin_path)

    # Build ATXF header
    # magic, version, header_size, entry_offset, text_offset, text_size, data_offset, data_size, bss_size
    header = struct.pack('<IHHIIIIII',
        0x41545846,  # magic "ATXF"
        1,           # version
        HEADER_SIZE, # header_size
        entry_offset,
        text_offset,
        len(text_data),
        data_offset,
        len(data_data),
        bss_size
    )

    # Pad header to page boundary
    header_padded = header + b'\x00' * (PAGE_SIZE - len(header))

    # Pad text to page boundary
    text_padded = text_data + b'\x00' * (text_size_aligned - len(text_data))

    # Pad data to page boundary
    data_padded = data_data + b'\x00' * (data_size_aligned - len(data_data)) if data_data else b''

    # Write ATXF file
    with open(output_path, 'wb') as f:
        f.write(header_padded)
        f.write(text_padded)
        f.write(data_padded)

    print(f"\nATXF binary created: {output_path}")
    print(f"  Entry offset: 0x{entry_offset:x}")
    print(f"  Text: {len(text_data)} bytes at offset 0x{text_offset:x}")
    print(f"  Data: {len(data_data)} bytes at offset 0x{data_offset:x}")
    print(f"  BSS: {bss_size} bytes")
    print(f"  Total: {os.path.getsize(output_path)} bytes")

if __name__ == '__main__':
    main()
