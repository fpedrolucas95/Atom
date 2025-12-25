; syscall/handler.asm
;
; Kernel System Call Handler
;
; Responsável por gerenciar a transição do modo usuário para o kernel durante
; uma syscall. Este módulo salva o estado do usuário, troca para a stack do
; kernel, prepara os argumentos para o dispatcher Rust, e retorna ao usuário
; de forma segura.
;
; Seções:
; - .bss: reserva memória para temporários e stack do kernel
; - .text: código da syscall

[BITS 64]
default rel

section .text
extern rust_syscall_dispatcher

section .bss align=16
temp_user_rsp:       resq 1
temp_user_rcx:       resq 1
temp_user_r11:       resq 1
temp_arg4:           resq 1
temp_arg5:           resq 1
temp_kernel_stack:   resb 16384

section .text
global syscall_entry
syscall_entry:
    mov     [rel temp_user_rsp], rsp
    mov     [rel temp_user_rcx], rcx
    mov     [rel temp_user_r11], r11
    mov     [rel temp_arg4], r8
    mov     [rel temp_arg5], r9

    lea     rsp, [rel temp_kernel_stack + 16384]
    and     rsp, -16

    push    rbx
    push    rbp
    push    r12
    push    r13
    push    r14
    push    r15

    mov     rcx, rax
    mov     rdx, rdi
    mov     r8,  rsi
    mov     r9,  rdx

    sub     rsp, 56

    mov     rax, r10
    mov     [rsp + 32], rax

    mov     rax, [rel temp_arg4]
    mov     [rsp + 40], rax

    mov     rax, [rel temp_arg5]
    mov     [rsp + 48], rax

    call    rust_syscall_dispatcher

    add     rsp, 56

    pop     r15
    pop     r14
    pop     r13
    pop     r12
    pop     rbp
    pop     rbx

    mov     rcx, [rel temp_user_rcx]
    mov     r11, [rel temp_user_r11]

    and     r11, 0x3C7FD7
    or      r11, 0x200

    shl     rcx, 16
    sar     rcx, 16

    mov     r10, [rel temp_user_rsp]
    mov     rsp, r10

    push    qword 0x23       ; SS = User Data Selector (0x20 | RPL=3)
    push    r10              ; RSP
    push    r11              ; RFLAGS
    push    qword 0x1B       ; CS = User Code Selector (0x18 | RPL=3)
    push    rcx              ; RIP

    iretq