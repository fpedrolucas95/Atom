; kernel/src/interrupts/switch.asm
; Context switching for x86_64 (MS x64 ABI)

[BITS 64]
section .text

%define OFF_RAX     0
%define OFF_RBX     8
%define OFF_RCX     16
%define OFF_RDX     24
%define OFF_RSI     32
%define OFF_RDI     40
%define OFF_RBP     48
%define OFF_RSP     56
%define OFF_R8      64
%define OFF_R9      72
%define OFF_R10     80
%define OFF_R11     88
%define OFF_R12     96
%define OFF_R13     104
%define OFF_R14     112
%define OFF_R15     120
%define OFF_RIP     128
%define OFF_RFLAGS  136
%define OFF_CS      144
%define OFF_SS      146
%define OFF_DS      148
%define OFF_ES      150
%define OFF_FS      152
%define OFF_GS      154
%define OFF_CR3     160

; =================================================
; switch_context(old, new) - MS x64: rcx=old, rdx=new
; =================================================
global switch_context
switch_context:
    push rbp
    mov rbp, rsp
    push r12
    push r13
    push r14
    push r15

    mov r12, rcx
    mov r15, rdx

    ; Save current context to r12
    mov [r12 + OFF_RAX], rax
    mov [r12 + OFF_RBX], rbx
    mov [r12 + OFF_RCX], rcx
    mov [r12 + OFF_RDX], rdx
    mov [r12 + OFF_RSI], rsi
    mov [r12 + OFF_RDI], rdi
    mov [r12 + OFF_RBP], rbp
    lea rax, [rbp + 8]
    mov [r12 + OFF_RSP], rax
    mov [r12 + OFF_R8],  r8
    mov [r12 + OFF_R9],  r9
    mov [r12 + OFF_R10], r10
    mov [r12 + OFF_R11], r11
    mov [r12 + OFF_R12], r12
    mov [r12 + OFF_R13], r13
    mov [r12 + OFF_R14], r14
    mov [r12 + OFF_R15], r15
    mov rax, [rbp + 8]
    mov [r12 + OFF_RIP], rax
    pushfq
    pop rax
    mov [r12 + OFF_RFLAGS], rax
    mov ax, cs
    mov [r12 + OFF_CS], ax
    mov ax, ss
    mov [r12 + OFF_SS], ax
    mov rax, cr3
    mov [r12 + OFF_CR3], rax

    mov rsp, rbp
    pop rbp
    jmp switch_to_context_internal

; =================================================
; switch_to_context(new) - MS x64: rcx=new
; =================================================
global switch_to_context
switch_to_context:
    mov r15, rcx
    ; fall through

switch_to_context_internal:
    cli

    ; Load CR3 if needed
    mov rax, [r15 + OFF_CR3]
    test rax, rax
    jz .skip_cr3
    mov rcx, cr3
    cmp rax, rcx
    je .skip_cr3
    mov cr3, rax
.skip_cr3:

    ; Check target CPL
    movzx eax, word [r15 + OFF_CS]
    test ax, 0x3
    jnz .iret_to_user

    ; ========== IRET to KERNEL (CPL 0) ==========
.iret_to_kernel:
    ; Restore GP registers (except r15)
    mov rax, [r15 + OFF_RAX]
    mov rbx, [r15 + OFF_RBX]
    mov rcx, [r15 + OFF_RCX]
    mov rdx, [r15 + OFF_RDX]
    mov rsi, [r15 + OFF_RSI]
    mov rdi, [r15 + OFF_RDI]
    mov rbp, [r15 + OFF_RBP]
    mov r8,  [r15 + OFF_R8]
    mov r9,  [r15 + OFF_R9]
    mov r10, [r15 + OFF_R10]
    mov r11, [r15 + OFF_R11]
    mov r12, [r15 + OFF_R12]
    mov r13, [r15 + OFF_R13]
    mov r14, [r15 + OFF_R14]

    ; Build 5-slot IRET frame (64-bit ALWAYS needs all 5!)
    push qword [r15 + OFF_SS]
    push qword [r15 + OFF_RSP]
    push qword [r15 + OFF_RFLAGS]
    push qword [r15 + OFF_CS]
    push qword [r15 + OFF_RIP]

    mov r15, [r15 + OFF_R15]
    iretq

    ; ========== IRET to USER (CPL 3) ==========
.iret_to_user:
    ; Sanitizar RFLAGS preservando IF (bit 9)
    mov rax, [r15 + OFF_RFLAGS]
    and rax, 0x3C7FD7      ; Remove bits perigosos
    or  rax, 0x202         ; ← Força bit 1 (reservado) + bit 9 (IF)
    mov [r15 + OFF_RFLAGS], rax

    ; Restore segment registers
    mov ax, [r15 + OFF_DS]
    mov ds, ax
    mov ax, [r15 + OFF_ES]
    mov es, ax

    ; Restore GP registers (except r15)
    mov rax, [r15 + OFF_RAX]
    mov rbx, [r15 + OFF_RBX]
    mov rcx, [r15 + OFF_RCX]
    mov rdx, [r15 + OFF_RDX]
    mov rsi, [r15 + OFF_RSI]
    mov rdi, [r15 + OFF_RDI]
    mov rbp, [r15 + OFF_RBP]
    mov r8,  [r15 + OFF_R8]
    mov r9,  [r15 + OFF_R9]
    mov r10, [r15 + OFF_R10]
    mov r11, [r15 + OFF_R11]
    mov r12, [r15 + OFF_R12]
    mov r13, [r15 + OFF_R13]
    mov r14, [r15 + OFF_R14]

    ; Build 5-slot IRET frame using push (simpler, correct)
    push qword [r15 + OFF_SS]
    push qword [r15 + OFF_RSP]
    push qword [r15 + OFF_RFLAGS]
    push qword [r15 + OFF_CS]
    push qword [r15 + OFF_RIP]

    mov r15, [r15 + OFF_R15]
    iretq