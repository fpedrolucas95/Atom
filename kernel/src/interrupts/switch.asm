; kernel/src/interrupts/switch.asm
; Context switching for x86_64 (MS x64 ABI)
;
; IMPORTANT: All functions that load CR3 first jump to a higher-half
; trampoline.  The higher-half PML4 entries (256-511) are SHARED across
; all address spaces, so kernel code at 0xFFFF_8000_… is always reachable
; regardless of which PML4 is in CR3.  This avoids triple-faults when
; the target PML4 has incomplete lower-half identity mappings.

[BITS 64]
default rel

; Higher-half base: virtual mirror of physical memory
%define HIGHER_HALF_BASE  0xFFFF800000000000

; Macro: JUMP_TO_HIGHER_HALF label
; Idempotently jumps to the higher-half mirror of the provided local label.
;
; Architectural Note: x86_64 canonical addresses require bits 48-63 to be
; copies of bit 47. Our higher-half mirror starts at 0xFFFF800000000000,
; where bit 47 is set. By testing bit 47, we can determine if the current
; code is already executing in the higher-half mirror. If so, we skip
; adding HIGHER_HALF_BASE to avoid an overflow that would result in a
; non-canonical or wrapped address.
%macro JUMP_TO_HIGHER_HALF 1
    lea  rax, [rel %1]
    bt   rax, 47                ; Check if bit 47 is set (already in higher half)
    jc   %%skip_hh              ; If so, skip adding mirror base
    mov  r10, HIGHER_HALF_BASE
    add  rax, r10
%%skip_hh:
    jmp  rax
%endmacro

section .text

; =================================================
; enter_user(rip, rsp, cr3) - Entry to userspace
; MS x64 ABI: rcx=rip, rdx=rsp, r8=cr3
; =================================================
global enter_user
enter_user:
    cli
    JUMP_TO_HIGHER_HALF .hh_enter
.hh_enter:
    ; Load CR3 if non-zero
    test r8, r8
    jz .skip_cr3
    mov cr3, r8
.skip_cr3:

    ; Set user data segments (USER_DATA_SELECTOR = 0x1B = GDT[0x18]|3)
    mov ax, 0x1B
    mov ds, ax
    mov es, ax

    ; Build IRET frame on current kernel stack
    sub rsp, 40
    mov [rsp + 0], rcx          ; RIP
    mov qword [rsp + 8], 0x23   ; CS = USER_CODE_SELECTOR (GDT[0x20]|3)

    ; RFLAGS: enable interrupts (IF=1)
    pushfq
    pop rax
    and rax, 0x3C7FD7
    or rax, 0x202
    mov [rsp + 16], rax         ; RFLAGS

    mov [rsp + 24], rdx         ; RSP
    mov qword [rsp + 32], 0x1B  ; SS = USER_DATA_SELECTOR (GDT[0x18]|3)

    iretq

; =================================================
; enter_user_first_time(rip, rsp, cr3) - First-time clean entry
; MS x64 ABI: rcx=rip, rdx=rsp, r8=cr3
; =================================================
global enter_user_first_time
enter_user_first_time:
    cli
    JUMP_TO_HIGHER_HALF .hh_first
.hh_first:
    test r8, r8
    jz .skip_cr3_first
    mov cr3, r8
.skip_cr3_first:

    mov ax, 0x1B
    mov ds, ax
    mov es, ax

    sub rsp, 40
    mov [rsp + 0], rcx
    mov qword [rsp + 8], 0x23
    pushfq
    pop rax
    and rax, 0x3C7FD7
    or rax, 0x202
    mov [rsp + 16], rax
    mov [rsp + 24], rdx
    mov qword [rsp + 32], 0x1B

    ; Zero all GPRs for clean state
    xor rax, rax
    xor rbx, rbx
    xor rcx, rcx
    xor rdx, rdx
    xor rsi, rsi
    xor rdi, rdi
    xor rbp, rbp
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15

    iretq

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
%define OFF_CR3     160

; =================================================
; switch_context(old, new) - MS x64: rcx=old, rdx=new
; =================================================
global switch_context
switch_context:
    ; Save current GPRs directly into the old context structure.
    ; This avoids clunky stack-based capture and is safer for pointer management.
    mov [rcx + OFF_RAX], rax
    mov [rcx + OFF_RBX], rbx
    mov [rcx + OFF_RCX], rcx
    mov [rcx + OFF_RDX], rdx
    mov [rcx + OFF_RSI], rsi
    mov [rcx + OFF_RDI], rdi
    mov [rcx + OFF_RBP], rbp
    mov [rcx + OFF_R8],  r8
    mov [rcx + OFF_R9],  r9
    mov [rcx + OFF_R10], r10
    mov [rcx + OFF_R11], r11
    mov [rcx + OFF_R12], r12
    mov [rcx + OFF_R13], r13
    mov [rcx + OFF_R14], r14
    mov [rcx + OFF_R15], r15

    ; Save the return address (RIP) and the stack pointer (RSP).
    ; [rsp] contains the RIP pushed by the 'call' instruction.
    mov rax, [rsp]
    mov [rcx + OFF_RIP], rax
    ; RSP before the call was [rsp + 8].
    lea rax, [rsp + 8]
    mov [rcx + OFF_RSP], rax

    ; Save segments and flags.
    mov ax, cs
    mov [rcx + OFF_CS], ax
    mov ax, ss
    mov [rcx + OFF_SS], ax

    pushfq
    pop rax
    or  rax, 0x200  ; Ensure the saved state has IF=1 for resuming.
    mov [rcx + OFF_RFLAGS], rax

    ; Prepare the NEW context pointer in R15 for the jump.
    ; RDX contains the 'new' parameter from the Rust call.
    mov r15, rdx

    ; Unified restoration path
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
    JUMP_TO_HIGHER_HALF .hh_switch
.hh_switch:
    ; Load CR3
    mov rax, [r15 + OFF_CR3]
    test rax, rax
    jz .skip_cr3_sw
    mov rcx, cr3
    cmp rax, rcx
    je .skip_cr3_sw
    mov cr3, rax
.skip_cr3_sw:

    ; Check target CS
    movzx eax, word [r15 + OFF_CS]
    cmp ax, 0x08
    je .iret_to_kernel

    ; ========== IRET to USER ==========
.iret_to_user:
    mov cx, 0x1B
    mov ds, cx
    mov es, cx

    sub rsp, 40
    mov rbx, [r15 + OFF_RIP]
    mov [rsp + 0], rbx
    mov qword [rsp + 8], 0x23   ; CS = USER_CODE_SELECTOR
    mov rax, [r15 + OFF_RFLAGS]
    and rax, 0x3C7FD7
    or  rax, 0x202
    mov [rsp + 16], rax
    mov rbx, [r15 + OFF_RSP]
    mov [rsp + 24], rbx
    mov qword [rsp + 32], 0x1B  ; SS = USER_DATA_SELECTOR

    ; Restore registers
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
    mov r15, [r15 + OFF_R15]
    iretq

    ; ========== IRET to KERNEL ==========
.iret_to_kernel:
    sub rsp, 40
    mov rax, [r15 + OFF_RIP]
    mov [rsp + 0], rax
    movzx eax, word [r15 + OFF_CS]
    mov [rsp + 8], rax
    mov rax, [r15 + OFF_RFLAGS]
    mov [rsp + 16], rax
    mov rax, [r15 + OFF_RSP]
    mov [rsp + 24], rax
    movzx eax, word [r15 + OFF_SS]
    mov [rsp + 32], rax

    ; Restore registers
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
    mov r15, [r15 + OFF_R15]
    iretq
