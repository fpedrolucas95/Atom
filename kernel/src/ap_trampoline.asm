BITS 16
ORG 0x8000

start:
    cli
    cld

    xor ax, ax
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov sp, 0x7C00

    lgdt [gdt_descriptor]

    mov eax, cr0
    or eax, 0x1
    mov cr0, eax
    jmp 0x08:protected_mode_entry

BITS 32
protected_mode_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov fs, ax
    mov gs, ax
    mov ss, ax

    mov eax, [pml4_ptr]
    mov cr3, eax

    mov eax, cr4
    ; PAE is required for long mode. OSFXSR/OSXMMEXCPT make SSE/FPU
    ; state legal before entering compiler-generated x86_64 Rust code.
    or eax, (1 << 5) | (1 << 9) | (1 << 10)
    mov cr4, eax

    mov ecx, 0xC0000080
    rdmsr
    ; Enable Long Mode (LME) and NX support (NXE). The kernel page
    ; tables use NX on data/stack pages, and without NXE those bits are
    ; reserved, causing an immediate #PF(RSVD) on the AP's first stack push.
    or eax, (1 << 8) | (1 << 11)
    wrmsr

    mov eax, cr0
    ; APs start with CD/NW set after INIT. Clear those cache-disable bits
    ; and enable paging with the same core protection bits expected by Rust.
    and eax, 0x9FFFFFFF
    or eax, 0x80010023
    mov cr0, eax

    jmp 0x18:long_mode_entry

BITS 64
long_mode_entry:
    mov ax, 0x20
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax

    mov rsp, [stack_top_ptr]
    mov rbp, rsp
    mov rax, [entry_ptr]
    xor rdi, rdi
    jmp rax

hang:
    hlt
    jmp hang

ALIGN 8
pml4_ptr:
    dd 0xA1B2C3D4
    dd 0x0

stack_top_ptr:
    dq 0x1122334455667788

entry_ptr:
    dq 0x8877665544332211

ALIGN 8
gdt_start:
    dq 0x0000000000000000
    dq 0x00CF9A000000FFFF ; 0x08 32-bit code
    dq 0x00CF92000000FFFF ; 0x10 data
    dq 0x00AF9A000000FFFF ; 0x18 64-bit code
    dq 0x00AF92000000FFFF ; 0x20 64-bit data
gdt_end:

gdt_descriptor:
    dw gdt_end - gdt_start - 1
    dd gdt_start
