; syscall/handler.asm
;
; Kernel System Call Handler
;
; Responsável por gerenciar a transição do modo usuário para o kernel durante
; uma syscall. Este módulo salva o estado do usuário, troca para a stack do
; kernel, prepara os argumentos para o dispatcher Rust, e retorna ao usuário
; de forma segura.

[BITS 64]
default rel

section .text
extern rust_syscall_dispatcher
extern CURRENT_THREAD_KSTACK

section .bss align=16
; Temporary storage used ONLY during initial transition (interrupts disabled)
temp_user_rsp:       resq 1
temp_user_rcx:       resq 1
temp_user_r11:       resq 1
temp_arg4:           resq 1
temp_arg5:           resq 1

section .text
global syscall_entry
syscall_entry:
    ; SYSCALL has already disabled interrupts (via SFMASK).
    ; We save initial state to globals while interrupts are disabled.
    mov     [rel temp_user_rsp], rsp
    mov     [rel temp_user_rcx], rcx ; User RIP
    mov     [rel temp_user_r11], r11 ; User RFLAGS
    mov     [rel temp_arg4], r8      ; arg4 (original r8)
    mov     [rel temp_arg5], r9      ; arg5 (original r9)

    ; Switch to the per-thread kernel stack immediately.
    ; This makes the syscall handler reentrant.
    mov     rsp, [rel CURRENT_THREAD_KSTACK]
    and     rsp, -16 ; Alignment

    ; Build an IRET-compatible frame on the per-thread kernel stack.
    ; This ensures we return to the CORRECT user context even after switches.
    push    qword 0x23          ; SS
    push    qword [rel temp_user_rsp]
    push    qword [rel temp_user_r11]
    push    qword 0x1B          ; CS
    push    qword [rel temp_user_rcx] ; RIP

    ; Save callee-saved registers as required by MS x64 ABI
    push    rbx
    push    rbp
    push    r12
    push    r13
    push    r14
    push    r15

    ; Pass arguments to rust_syscall_dispatcher (Windows x64 ABI)
    ; User:  rax=num, rdi=arg0, rsi=arg1, rdx=arg2, r10=arg3, r8=arg4, r9=arg5
    ; Windows: rcx=num, rdx=arg0, r8=arg1, r9=arg2, stack=[arg3, arg4, arg5, ...]

    mov     rcx, rax        ; rcx = syscall number
    mov     rax, rdx        ; save user rdx (arg2)
    mov     rdx, rdi        ; rdx = arg0
    mov     r8,  rsi        ; r8 = arg1
    mov     r9,  rax        ; r9 = arg2 (from saved)

    ; Reserve shadow space (32) + 8 extra arg slots (64) = 96. Use 128 for alignment.
    sub     rsp, 128

    ; arg3 (user r10)
    mov     rax, r10
    mov     [rsp + 32], rax

    ; arg4 (original r8)
    mov     rax, [rel temp_arg4]
    mov     [rsp + 40], rax

    ; arg5 (original r9)
    mov     rax, [rel temp_arg5]
    mov     [rsp + 48], rax

    ; Pass user RIP and RSP to the dispatcher for logging/context updates
    mov     rax, [rel temp_user_rcx]
    mov     [rsp + 56], rax
    mov     rax, [rel temp_user_rsp]
    mov     [rsp + 64], rax

    ; Pass callee-saved registers (currently on stack)
    ; RBX is at [rsp + 128 + 40] = [rsp + 168]
    mov     rax, [rsp + 168]
    mov     [rsp + 72], rax
    mov     rax, [rsp + 160] ; RBP
    mov     [rsp + 80], rax
    mov     rax, [rsp + 152] ; R12
    mov     [rsp + 88], rax
    mov     rax, [rsp + 144] ; R13
    mov     [rsp + 96], rax
    mov     rax, [rsp + 136] ; R14
    mov     [rsp + 104], rax
    mov     rax, [rsp + 128] ; R15
    mov     [rsp + 112], rax

    call    rust_syscall_dispatcher

    add     rsp, 128

    ; Restore registers
    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbp
    pop     rbx

    ; Reconstruct the return frame from the per-thread stack.
    ; This avoids race conditions on globals during return.
    pop     rcx              ; RIP
    add     rsp, 8           ; skip CS
    pop     r11              ; RFLAGS
    pop     r10              ; RSP
    add     rsp, 8           ; skip SS

    ; Sanitize RFLAGS for return (ensure IF=1)
    and     r11, 0x3C7FD7
    or      r11, 0x200

    ; iretq expects: RIP, CS, RFLAGS, RSP, SS
    ; We manually restore the frame to the stack for IRETQ
    sub     rsp, 40
    mov     [rsp + 0],  rcx  ; RIP
    mov     qword [rsp + 8], 0x1B ; CS
    mov     [rsp + 16], r11  ; RFLAGS
    mov     [rsp + 24], r10  ; RSP
    mov     qword [rsp + 32], 0x23 ; SS

    iretq
