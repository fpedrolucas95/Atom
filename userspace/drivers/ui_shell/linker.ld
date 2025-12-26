/* Linker script for Atom userspace binaries */
/* Entry point and load address for userspace executables */

ENTRY(_start)

SECTIONS
{
    /* Load at userspace base address */
    . = 0x400000;

    .text : ALIGN(4096)
    {
        *(.text .text.*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4096)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    /* Heap starts after BSS */
    . = ALIGN(4096);
    __heap_start = .;

    /DISCARD/ :
    {
        *(.comment)
        *(.eh_frame)
        *(.note.*)
    }
}
