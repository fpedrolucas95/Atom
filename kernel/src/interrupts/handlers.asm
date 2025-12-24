; kernel/src/interrupts/handlers.asm
;
; Interrupt and exception handler stubs for x86_64

[BITS 64]

section .text

; External Rust handler functions
extern rust_exception_handler
extern rust_unexpected_interrupt_handler

; ---------------------------------------------------------------------------
; Catch-all interrupt stubs
; ---------------------------------------------------------------------------

global unexpected_interrupt_table

unexpected_interrupt_table:
%assign vec 0
%rep 256
    dq unexpected_interrupt_handler_%[vec]
%assign vec vec + 1
%endrep

; ---------------------------------------------------------------------------
; Common handler for unexpected interrupts (vectors without a dedicated stub)
; ---------------------------------------------------------------------------
unexpected_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    ; Windows x64 ABI: first arg in RCX, second in RDX
    mov rcx, [rsp + 15*8]          ; vector
    lea rdx, [rsp + 15*8 + 16]     ; &InterruptStackFrame

    ; Shadow space required by Windows x64 ABI
    sub rsp, 32
    call rust_unexpected_interrupt_handler
    add rsp, 32

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16                    ; drop vector + error_code
    iretq

; ---------------------------------------------------------------------------
; Exception handler macros
; ---------------------------------------------------------------------------

%macro EXCEPTION_HANDLER_NO_ERR 1
global exception_handler_%1
exception_handler_%1:
    push qword 0          ; dummy error code
    push qword %1         ; vector
    jmp exception_common
%endmacro

%macro EXCEPTION_HANDLER_ERR 1
global exception_handler_%1
exception_handler_%1:
    ; CPU pushed: error_code at [rsp], then RIP, CS, RFLAGS, RSP, SS
    ; Just push vector - no swap needed!
    ; Result: vector, error_code, RIP (correct order for InterruptFrame)
    push qword %1
    jmp exception_common
%endmacro

; ---------------------------------------------------------------------------
; Unexpected interrupt handlers (no error code)
; ---------------------------------------------------------------------------

%macro UNEXPECTED_INTERRUPT_HANDLER 1
global unexpected_interrupt_handler_%1
unexpected_interrupt_handler_%1:
    push qword 0
    push qword %1
    jmp unexpected_common
%endmacro

%assign vec 0
%rep 256
UNEXPECTED_INTERRUPT_HANDLER vec
%assign vec vec + 1
%endrep

; ---------------------------------------------------------------------------
; Exception handlers (0–31)
; ---------------------------------------------------------------------------

EXCEPTION_HANDLER_NO_ERR 0    ; #DE Divide Error
EXCEPTION_HANDLER_NO_ERR 1    ; #DB Debug
EXCEPTION_HANDLER_NO_ERR 2    ; NMI
EXCEPTION_HANDLER_NO_ERR 3    ; #BP Breakpoint
EXCEPTION_HANDLER_NO_ERR 4    ; #OF Overflow
EXCEPTION_HANDLER_NO_ERR 5    ; #BR Bound Range Exceeded
EXCEPTION_HANDLER_NO_ERR 6    ; #UD Invalid Opcode
EXCEPTION_HANDLER_NO_ERR 7    ; #NM Device Not Available
EXCEPTION_HANDLER_ERR    8    ; #DF Double Fault
EXCEPTION_HANDLER_NO_ERR 9
EXCEPTION_HANDLER_ERR    10   ; #TS Invalid TSS
EXCEPTION_HANDLER_ERR    11   ; #NP Segment Not Present
EXCEPTION_HANDLER_ERR    12   ; #SS Stack-Segment Fault
EXCEPTION_HANDLER_ERR    13   ; #GP General Protection
EXCEPTION_HANDLER_ERR    14   ; #PF Page Fault
EXCEPTION_HANDLER_NO_ERR 16   ; #MF x87 FPU
EXCEPTION_HANDLER_ERR    17   ; #AC Alignment Check
EXCEPTION_HANDLER_NO_ERR 18   ; #MC Machine Check
EXCEPTION_HANDLER_NO_ERR 19   ; #XM SIMD FP
EXCEPTION_HANDLER_NO_ERR 20   ; #VE Virtualization
EXCEPTION_HANDLER_ERR    21   ; #CP Control Protection

; ---------------------------------------------------------------------------
; Common exception handler
; ---------------------------------------------------------------------------

exception_common:
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15

    mov rcx, rsp           ; ✅ First arg in rcx (was rdi)

    sub rsp, 32            ; Shadow space required by MS x64
    call rust_exception_handler
    add rsp, 32

    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax

    add rsp, 16                    ; drop vector + error_code
    iretq