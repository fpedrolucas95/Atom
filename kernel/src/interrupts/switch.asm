; kernel/src/interrupts/switch.asm
; Context switching for x86_64 (MS x64 ABI)

[BITS 64]
section .text

; Debug variables for IRET frame inspection
section .bss
align 8
global DEBUG_IRET_RIP
global DEBUG_IRET_CS
global DEBUG_IRET_RFLAGS
global DEBUG_IRET_RSP
global DEBUG_IRET_SS
global DEBUG_IRET_KERNEL_RSP
DEBUG_IRET_RIP:         resq 1
DEBUG_IRET_CS:          resq 1
DEBUG_IRET_RFLAGS:      resq 1
DEBUG_IRET_RSP:         resq 1
DEBUG_IRET_SS:          resq 1
DEBUG_IRET_KERNEL_RSP:  resq 1

section .text

; =================================================
; enter_user_resume(rip, rsp, cr3, regs_ptr) - Resume from syscall
; MS x64 ABI: rcx=rip, rdx=rsp, r8=cr3, r9=regs_ptr
; Restores callee-saved registers from struct pointed by regs_ptr
; Struct layout (8 bytes each): rbx, rbp, r12, r13, r14, r15
; =================================================
global enter_user_resume
enter_user_resume:
    cli
    
    ; Load CR3 if non-zero
    test r8, r8
    jz .skip_cr3_resume
    mov cr3, r8
.skip_cr3_resume:
    
    ; Set user data segments
    mov ax, 0x23
    mov ds, ax
    mov es, ax
    
    ; Restore callee-saved registers from *regs_ptr (r9)
    ; Struct layout: [0]=rbx, [8]=rbp, [16]=r12, [24]=r13, [32]=r14, [40]=r15
    mov rbx, [r9 + 0]
    mov rbp, [r9 + 8]
    mov r12, [r9 + 16]
    mov r13, [r9 + 24]
    mov r14, [r9 + 32]
    mov r15, [r9 + 40]
    
    ; Build IRET frame on current kernel stack
    ; Frame: RIP, CS, RFLAGS, RSP, SS
    sub rsp, 40
    
    ; RIP from rcx
    mov [rsp + 0], rcx
    
    ; CS = 0x1B (USER_CODE_SELECTOR)
    mov qword [rsp + 8], 0x1B
    
    ; RFLAGS: get current and sanitize
    pushfq
    pop rax
    and rax, 0x3C7FD7
    or rax, 0x202        ; IF + reserved bit
    mov [rsp + 16], rax
    
    ; User RSP from rdx
    mov [rsp + 24], rdx
    
    ; SS = 0x23 (USER_DATA_SELECTOR)
    mov qword [rsp + 32], 0x23
    
    ; Zero volatile registers for security (caller-saved, not preserved by ABI)
    xor rax, rax
    xor rdi, rdi
    xor rsi, rsi
    xor rcx, rcx
    xor rdx, rdx
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    
    iretq

; =================================================
; enter_user(rip, rsp, cr3) - Entry/resume to userspace
; MS x64 ABI: rcx=rip, rdx=rsp, r8=cr3
; This version does NOT zero registers - preserves ABI for syscall return
; =================================================
global enter_user
enter_user:
    cli
    
    ; Load CR3 if non-zero
    test r8, r8
    jz .skip_cr3
    mov cr3, r8
.skip_cr3:
    
    ; Set user data segments
    mov ax, 0x23
    mov ds, ax
    mov es, ax
    
    ; Build IRET frame on current kernel stack
    ; Frame: RIP, CS, RFLAGS, RSP, SS
    sub rsp, 40
    
    ; RIP from rcx
    mov [rsp + 0], rcx
    
    ; CS = 0x1B (USER_CODE_SELECTOR)
    mov qword [rsp + 8], 0x1B
    
    ; RFLAGS: get current and sanitize
    pushfq
    pop rax
    and rax, 0x3C7FD7
    or rax, 0x202        ; IF + reserved bit
    mov [rsp + 16], rax
    
    ; User RSP from rdx
    mov [rsp + 24], rdx
    
    ; SS = 0x23 (USER_DATA_SELECTOR)
    mov qword [rsp + 32], 0x23
    
    ; DO NOT zero registers - preserve ABI for syscall return
    ; RBX, RBP, R12-R15 must be preserved across syscalls
    ; Other registers (RAX, RDI, RSI, etc.) contain syscall return values
    
    iretq

; =================================================
; enter_user_first_time(rip, rsp, cr3) - First-time clean entry
; MS x64 ABI: rcx=rip, rdx=rsp, r8=cr3
; This version zeros all registers for a clean initial state
; =================================================
global enter_user_first_time
enter_user_first_time:
    cli
    
    ; Load CR3 if non-zero
    test r8, r8
    jz .skip_cr3_first
    mov cr3, r8
.skip_cr3_first:
    
    ; Set user data segments
    mov ax, 0x23
    mov ds, ax
    mov es, ax
    
    ; Build IRET frame on current kernel stack
    sub rsp, 40
    
    mov [rsp + 0], rcx          ; RIP
    mov qword [rsp + 8], 0x1B   ; CS
    
    pushfq
    pop rax
    and rax, 0x3C7FD7
    or rax, 0x202
    mov [rsp + 16], rax         ; RFLAGS
    
    mov [rsp + 24], rdx         ; RSP
    mov qword [rsp + 32], 0x23  ; SS
    
    ; Zero all GPRs for clean first-time entry
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
    ; Save callee-saved registers on stack
    push rbp
    push r12
    push r13
    push r14
    push r15

    mov r12, rcx   ; r12 = old context ptr
    mov r15, rdx   ; r15 = new context ptr

    ; Save current context to r12 (old)
    mov [r12 + OFF_RAX], rax
    mov [r12 + OFF_RBX], rbx
    mov [r12 + OFF_RCX], rcx
    mov [r12 + OFF_RDX], rdx
    mov [r12 + OFF_RSI], rsi
    mov [r12 + OFF_RDI], rdi
    ; Save RBP from before the push
    mov rax, [rsp + 32]  ; RBP is 5 pushes ago (5*8 = 40, but we saved it first so +32)
    mov [r12 + OFF_RBP], rax
    ; Save RSP as it was before the call (skip 6 pushes: rbp,r12,r13,r14,r15,return_address)
    lea rax, [rsp + 48]
    mov [r12 + OFF_RSP], rax
    mov [r12 + OFF_R8],  r8
    mov [r12 + OFF_R9],  r9
    mov [r12 + OFF_R10], r10
    mov [r12 + OFF_R11], r11
    mov rax, [rsp + 24]  ; r12 saved 4 pushes ago
    mov [r12 + OFF_R12], rax
    mov rax, [rsp + 16]  ; r13 saved 3 pushes ago
    mov [r12 + OFF_R13], rax
    mov rax, [rsp + 8]   ; r14 saved 2 pushes ago
    mov [r12 + OFF_R14], rax
    mov rax, [rsp]       ; r15 saved 1 push ago
    mov [r12 + OFF_R15], rax
    ; Save return address as RIP
    mov rax, [rsp + 40]  ; return address is 6 pushes ago
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

    ; Clean up our stack frame
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp

    ; Jump to common restore path
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

    ; Check target CPL: only go to kernel path if CS is EXACTLY kernel code selector
    ; If CS is garbage (like 0x34EC), we'll go to user path where we force valid selectors
    movzx eax, word [r15 + OFF_CS]
    cmp ax, 0x08            ; KERNEL_CODE_SELECTOR (exact match required)
    je .iret_to_kernel
    ; else: assume usermode (even if CS is garbage - safer default)
    ; fall through to .iret_to_user

    ; ========== IRET to KERNEL (CPL 0) ==========
.iret_to_kernel:
    ; In x86_64 long mode, IRETQ ALWAYS pops 5 qwords:
    ;   RIP, CS, RFLAGS, RSP, SS
    ; This is true even for CPL0->CPL0 transitions (unlike 32-bit protected mode).
    ; We must build a full 5-qword IRET frame.

    ; Build full IRET frame (5 qwords = 40 bytes)
    sub rsp, 40

    ; [rsp+0]  = RIP
    mov rax, [r15 + OFF_RIP]
    mov [rsp + 0], rax

    ; [rsp+8]  = CS (kernel code selector)
    movzx eax, word [r15 + OFF_CS]
    mov [rsp + 8], rax

    ; [rsp+16] = RFLAGS
    mov rax, [r15 + OFF_RFLAGS]
    mov [rsp + 16], rax

    ; [rsp+24] = RSP (restored kernel stack pointer)
    mov rax, [r15 + OFF_RSP]
    mov [rsp + 24], rax

    ; [rsp+32] = SS (kernel data selector)
    movzx eax, word [r15 + OFF_SS]
    mov [rsp + 32], rax

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
    mov r15, [r15 + OFF_R15]

    iretq

    ; ========== IRET to USER (CPL 3) ==========
.iret_to_user:
    ; Sanitizar RFLAGS preservando IF (bit 9)
    mov rax, [r15 + OFF_RFLAGS]
    and rax, 0x3C7FD7      ; Remove bits perigosos
    or  rax, 0x202         ; ← Força bit 1 (reservado) + bit 9 (IF)

    ; Force segment registers to user data selector BEFORE building frame
    mov cx, 0x23           ; USER_DATA_SELECTOR
    mov ds, cx
    mov es, cx

    ; Build IRET frame EXPLICITLY (not with push) to avoid any stack corruption
    ; IRETQ expects (from top to bottom): RIP, CS, RFLAGS, RSP, SS
    ; Allocate 40 bytes (5 qwords) on stack
    sub rsp, 40

    ; [rsp+0] = RIP
    mov rbx, [r15 + OFF_RIP]
    mov [rsp + 0], rbx

    ; [rsp+8] = CS (forced to 0x1B - USER_CODE_SELECTOR)
    mov qword [rsp + 8], 0x1B

    ; [rsp+16] = RFLAGS (sanitized value in rax)
    mov [rsp + 16], rax

    ; [rsp+24] = RSP
    mov rbx, [r15 + OFF_RSP]
    mov [rsp + 24], rbx

    ; [rsp+32] = SS (forced to 0x23 - USER_DATA_SELECTOR)
    mov qword [rsp + 32], 0x23

    ; Restore GP registers (except r15, but including rax/rbx used above)
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

    ; DEBUG: Save frame values AFTER restoring regs (use stack directly)
    ; At this point all GPRs are restored, so we read directly from stack
    ; and write to globals using the last instruction before IRETQ
    push rax
    mov rax, rsp
    add rax, 8                      ; account for push rax
    mov [rel DEBUG_IRET_KERNEL_RSP], rax
    mov rax, [rsp + 8]              ; [original_rsp+0] = RIP
    mov [rel DEBUG_IRET_RIP], rax
    mov rax, [rsp + 16]             ; [original_rsp+8] = CS
    mov [rel DEBUG_IRET_CS], rax
    mov rax, [rsp + 24]             ; [original_rsp+16] = RFLAGS
    mov [rel DEBUG_IRET_RFLAGS], rax
    mov rax, [rsp + 32]             ; [original_rsp+24] = RSP
    mov [rel DEBUG_IRET_RSP], rax
    mov rax, [rsp + 40]             ; [original_rsp+32] = SS
    mov [rel DEBUG_IRET_SS], rax
    pop rax

    iretq