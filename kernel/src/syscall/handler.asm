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

section .text
global syscall_entry
syscall_entry:
    ; SYSCALL enters Ring 0 with interrupts masked by SFMASK.
    ; swapgs switches to per-CPU kernel GS base configured by smp::init_cpu_local_syscall_state.
    swapgs
    mov     [gs:8], rsp              ; temp_user_rsp
    mov     rsp, [gs:0]              ; current kernel stack top
    and     rsp, -16                 ; ABI alignment

    ; Build IRET frame (SS, RSP, RFLAGS, CS, RIP)
    push    qword 0x23               ; SS
    push    qword [gs:8]             ; RSP
    push    r11                      ; RFLAGS
    push    qword 0x1B               ; CS
    push    rcx                      ; RIP

    ; Save ALL user GPRs so we can restore them on return.
    ; The kernel must not leak its internal register values back to userspace.
    push    rbx
    push    rbp
    push    r12
    push    r13
    push    r14
    push    r15
    push    rdi  ; user arg0
    push    rsi  ; user arg1
    push    rdx  ; user arg2
    push    r10  ; user arg3
    push    r8   ; user arg4
    push    r9   ; user arg5

    ; Bridge to Rust dispatcher using explicit Win64 ABI:
    ;   RCX = syscall number
    ;   RDX = pointer to saved syscall frame at current RSP
    ;
    ; This avoids brittle 15-argument stack marshalling and keeps
    ; the saved IRET frame/callee-saved GPRs as a single authoritative
    ; structure for dispatch + optional debug verification.
    mov     rcx, rax
    mov     rdx, rsp

    ; Win64 call-site requirements:
    ; - 32 bytes shadow space
    ; - RSP 16-byte aligned before CALL
    ; After 17 pushes above, RSP is 8 mod 16, so reserve 40 bytes.
    sub     rsp, 40

    call    rust_syscall_dispatcher

    ; Restore stack and ALL user GPRs
    add     rsp, 40
    pop     r9
    pop     r8
    pop     r10
    pop     rdx
    pop     rsi
    pop     rdi
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbp
    pop     rbx

    ; The stack now points to the IRET frame (RIP, CS, RFLAGS, RSP, SS)
    ; Ensure IF=1 in restored RFLAGS and restore user GS base.
    or      qword [rsp + 16], 0x200
    swapgs
    iretq
