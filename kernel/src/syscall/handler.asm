; syscall/handler.asm
;
; Kernel System Call Handler - Reentrant and Thread-Safe
;
; Este módulo gerencia a transição de Ring 3 para Ring 0 usando a instrução
; SYSCALL. Ele utiliza a stack per-thread do kernel para garantir que
; syscalls possam ser interrompidas e que múltiplos threads possam estar
; em syscalls simultâneas sem corrupção de estado.

[BITS 64]
default rel

section .text
extern rust_syscall_dispatcher
extern CURRENT_THREAD_KSTACK

section .bss align=16
; Temporary storage used ONLY during the RSP swap while interrupts are disabled.
; Since interrupts are disabled by the SYSCALL instruction itself (via SFMASK),
; this is safe on a uniprocessor system. For SMP, this must be per-CPU (GS).
temp_user_rsp: resq 1

section .text
global syscall_entry
syscall_entry:
    ; SYSCALL disables interrupts.
    ; We must not enable them until we are on a safe stack.
    mov     [rel temp_user_rsp], rsp
    mov     rsp, [rel CURRENT_THREAD_KSTACK]
    and     rsp, -16 ; ABI alignment

    ; Build IRET frame (SS, RSP, RFLAGS, CS, RIP)
    push    qword 0x23               ; SS
    push    qword [rel temp_user_rsp] ; RSP
    push    r11                      ; RFLAGS
    push    qword 0x1B               ; CS
    push    rcx                      ; RIP

    ; Save all other GPRs
    push    rbx
    push    rbp
    push    r12
    push    r13
    push    r14
    push    r15
    push    r8  ; arg4
    push    r9  ; arg5

    ; Prepare args for Rust (Windows x64 ABI)
    ; RCX=num, RDX=arg0, R8=arg1, R9=arg2
    mov     rcx, rax        ; arg0 (num)
    mov     rax, rdx        ; temp
    mov     rdx, rdi        ; arg1 (user RDI)
    mov     r8,  rsi        ; arg2 (user RSI)
    mov     r9,  rax        ; arg3 (user RDX)

    sub     rsp, 120        ; shadow space + arguments 3-14

    ; Fill stack arguments for dispatcher
    mov     rax, r10        ; User R10 -> arg3
    mov     [rsp + 32], rax
    mov     rax, [rsp + 128] ; User R8  -> arg4
    mov     [rsp + 40], rax
    mov     rax, [rsp + 120] ; User R9  -> arg5
    mov     [rsp + 48], rax

    ; Original user RIP and RSP (for debug)
    mov     rax, [rsp + 184]
    mov     [rsp + 56], rax
    mov     rax, [rsp + 208]
    mov     [rsp + 64], rax

    ; Original callee-saved registers (for debug/trace)
    mov     rax, [rsp + 176] ; RBX
    mov     [rsp + 72], rax
    mov     rax, [rsp + 168] ; RBP
    mov     [rsp + 80], rax
    mov     rax, [rsp + 160] ; R12
    mov     [rsp + 88], rax
    mov     rax, [rsp + 152] ; R13
    mov     [rsp + 96], rax
    mov     rax, [rsp + 144] ; R14
    mov     [rsp + 104], rax
    mov     rax, [rsp + 136] ; R15
    mov     [rsp + 112], rax

    call    rust_syscall_dispatcher

    ; Restore stack and return
    add     rsp, 120
    add     rsp, 16 ; skip saved R8, R9
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbp
    pop     rbx

    ; The stack now points to the IRET frame (RIP, CS, RFLAGS, RSP, SS)
    ; Ensure IF=1 in restored RFLAGS
    or      qword [rsp + 16], 0x200
    iretq
