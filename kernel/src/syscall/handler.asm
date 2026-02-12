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
; UNIPROCESSOR ONLY: Temporary scratch space used during initial transition while IRQs are OFF.
; NOTE: For SMP support, these must be moved to per-CPU storage (accessed via GS segment).
temp_user_rsp:       resq 1
temp_user_rcx:       resq 1
temp_user_r11:       resq 1

section .text
global syscall_entry
syscall_entry:
    ; SYSCALL disables interrupts via SFMASK.
    ; We save volatile state in scratch variables while IRQs are OFF.
    mov     [rel temp_user_rsp], rsp
    mov     [rel temp_user_rcx], rcx ; User RIP
    mov     [rel temp_user_r11], r11 ; User RFLAGS

    ; Switch to the current thread's kernel stack.
    ; NOTE: For SMP support, CURRENT_THREAD_KSTACK must be a per-CPU variable in GS.
    mov     rsp, [rel CURRENT_THREAD_KSTACK]
    and     rsp, -16 ; 16-byte alignment for ABI compliance

    ; Build an IRET frame on the thread's kernel stack.
    ; This ensures the return state is preserved during context switches.
    ; Stack order (top to bottom): RIP, CS, RFLAGS, RSP, SS
    push    qword 0x23               ; SS
    push    qword [rel temp_user_rsp] ; RSP
    push    qword [rel temp_user_r11] ; RFLAGS
    push    qword 0x1B               ; CS
    push    qword [rel temp_user_rcx] ; RIP

    ; Save callee-saved registers (RBX, RBP, R12-R15) as required by ABI
    push    rbx
    push    rbp
    push    r12
    push    r13
    push    r14
    push    r15

    ; Save arg4 (R8) and arg5 (R9) on the stack as they will be clobbered
    ; by the Windows x64 ABI call parameters.
    push    r8
    push    r9

    ; Prepare arguments for the Rust dispatcher (Windows x64 ABI)
    ; User Convention: RAX=num, RDI=arg0, RSI=arg1, RDX=arg2, R10=arg3, R8=arg4, R9=arg5
    ; Win Convention:  RCX=num, RDX=arg0, R8=arg1, R9=arg2, stack=[arg3, arg4, arg5, rip, rsp...]

    mov     rcx, rax        ; arg0 (num)
    mov     rax, rdx        ; temp for user RDX
    mov     rdx, rdi        ; arg1 (user RDI)
    mov     r8,  rsi        ; arg2 (user RSI)
    mov     r9,  rax        ; arg3 (user RDX)

    ; Reserve shadow space (32) + 8 argument slots (64) = 96.
    ; Use 128 to maintain 16-byte alignment (pushed 13 * 8 = 104; 104+120=224, 224%16=0).
    sub     rsp, 120

    ; arg3 (user R10) -> [rsp + 32]
    mov     rax, r10
    mov     [rsp + 32], rax

    ; arg4 (user R8) -> [rsp + 40]
    mov     rax, [rsp + 128] ; Retrieve from stack (pushed R9 is at +120, R8 at +128)
    mov     [rsp + 40], rax

    ; arg5 (user R9) -> [rsp + 48]
    mov     rax, [rsp + 120] ; Retrieve from stack
    mov     [rsp + 48], rax

    ; Passamos RIP e RSP originais para o dispatcher (para logs/debug)
    mov     rax, [rel temp_user_rcx]
    mov     [rsp + 56], rax
    mov     rax, [rel temp_user_rsp]
    mov     [rsp + 64], rax

    ; Passamos ponteiros para os registradores salvos na stack (opcional, mas útil)
    lea     rax, [rsp + 128]  ; Ponteiro para o bloco de GPRs
    mov     [rsp + 72], rax

    call    rust_syscall_dispatcher

    ; Restore shadow space and arguments (from 'sub rsp, 120')
    add     rsp, 120

    ; Skip the saved R8 and R9
    add     rsp, 16

    ; Restore callee-saved registers
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbp
    pop     rbx

    ; The stack now points to the IRET frame we pushed earlier:
    ; [rsp + 0]:  RIP
    ; [rsp + 8]:  CS
    ; [rsp + 16]: RFLAGS
    ; [rsp + 24]: RSP
    ; [rsp + 32]: SS

    ; Ensure interrupts are enabled in the restored RFLAGS (bit 9 = 0x200)
    or      qword [rsp + 16], 0x200

    ; Return to userspace using the frame on the stack
    iretq
