/* Userspace Linker Script for Atom OS
 *
 * This script ensures userspace binaries are linked at the correct base
 * address (0x400000) to match where the kernel loads them.
 *
 * The kernel's USER_EXEC_LOAD_BASE is 0x0040_0000 (4 MiB).
 */

OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

/* Base address must match kernel's USER_EXEC_LOAD_BASE */
USER_BASE = 0x400000;

SECTIONS
{
    /* Start at USER_BASE */
    . = USER_BASE;

    /* Code section - includes .rodata so it's part of executable segment */
    .text : ALIGN(4K)
    {
        *(.text .text.*)
        /* Include rodata in text segment so it's captured by elf2atxf */
        *(.rodata .rodata.*)
    }

    /* Initialized data - include relocation-related sections */
    .data : ALIGN(4K)
    {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
    }

    /* Zero-initialized data */
    .bss : ALIGN(4K)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /* Discard unneeded sections - especially dynamic linking infrastructure */
    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.eh_frame_hdr*)
        *(.debug*)
        /* Discard dynamic linking sections - we're fully static */
        *(.dynsym)
        *(.gnu.hash)
        *(.hash)
        *(.dynstr)
        *(.dynamic)
        *(.rela.dyn)
        *(.rela.plt)
        *(.plt)
        *(.plt.got)
        *(.relro_padding)
        *(.interp)
    }
}
