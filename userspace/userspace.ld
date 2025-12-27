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
    /* Code section - includes .rodata so it's part of executable segment */
    . = USER_BASE;

    .text : ALIGN(4K)
    {
        *(.text .text.*)
        /* Include rodata in text segment so it's captured by elf2atxf */
        *(.rodata .rodata.*)
    }

    /* Initialized data */
    .data : ALIGN(4K)
    {
        *(.data .data.*)
        *(.got .got.*)
    }

    /* Zero-initialized data */
    .bss : ALIGN(4K)
    {
        *(.bss .bss.*)
        *(COMMON)
    }

    /* Discard unneeded sections */
    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.debug*)
    }
}
