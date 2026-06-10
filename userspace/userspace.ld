/* Atom userspace PIE layout.
 *
 * Every loadable region starts at a page boundary and receives a dedicated
 * PT_LOAD with final permissions. elf2atxf preserves these permissions and
 * converts .rela.dyn into ATXF v2 relocations.
 */

OUTPUT_FORMAT("elf64-x86-64")
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    text    PT_LOAD FLAGS(5); /* R X */
    rodata  PT_LOAD FLAGS(4); /* R   */
    data    PT_LOAD FLAGS(6); /* R W */
    dynamic PT_DYNAMIC FLAGS(6);
}

SECTIONS
{
    . = 0;

    . = ALIGN(4K);
    .text :
    {
        *(.text .text.*)
    } :text

    . = ALIGN(4K);
    .rodata :
    {
        *(.rodata .rodata.*)
        *(.eh_frame_hdr)
        *(.eh_frame)
    } :rodata

    . = ALIGN(4K);
    .data :
    {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
    } :data

    .dynamic :
    {
        *(.dynamic)
    } :data :dynamic

    .rela.dyn :
    {
        *(.rela.dyn)
        *(.rela.plt)
    } :data

    . = ALIGN(4K);
    .bss (NOLOAD) :
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.comment)
        *(.note*)
        *(.debug*)
        *(.dynsym)
        *(.gnu.hash)
        *(.hash)
        *(.dynstr)
        *(.plt)
        *(.plt.got)
        *(.relro_padding)
        *(.interp)
    }
}
