/* Linker script for Atom userspace binaries */
/* Entry point and load address for userspace executables */

ENTRY(_start)

SECTIONS
{
    /* Load at userspace base address (separate from init at 0x400000) */
    . = 0x500000;

    .text : ALIGN(4096)
    {
        *(.text .text.*)
    }

    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    }

    /* Global Offset Table - needs to be at a known location */
    .got : ALIGN(8)
    {
        *(.got)
        *(.got.plt)
    }

    /* Data must follow .got contiguously (no page alignment)
       because elf2atxf concatenates rodata+got+data into one blob */
    .data : ALIGN(8)
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
