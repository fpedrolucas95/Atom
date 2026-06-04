; kernel/src/interrupts/switch.asm
; x86_64 context switching and userspace entry glue.
;
; ABI:
;   - NASM syntax, 64-bit mode.
;   - Rust target ABI is MS x64:
;       enter_user(rip, rsp, cr3)             rcx=rip, rdx=rsp, r8=cr3
;       enter_user_first_time(rip, rsp, cr3)  rcx=rip, rdx=rsp, r8=cr3
;       switch_context(old, new)              rcx=old, rdx=new
;       switch_to_context(new)                rcx=new
;
; Design:
;   - Context switching is cooperative / scheduler-driven. It saves a normal
;     call-return continuation, not an interrupt frame.
;   - User transitions use IRETQ because privilege changes require the CPU to
;     load RIP, CS, RFLAGS, RSP and SS atomically.
;   - Kernel-to-kernel resumes use RET, not IRETQ. In long mode, IRETQ only
;     pops RSP/SS when the privilege level changes. A ring0->ring0 IRETQ would
;     ignore the saved kernel RSP and resume on the outgoing stack.
;   - CR3 changes happen after an idempotent jump to the higher-half mirror.
;     This assumes all address spaces share the kernel higher-half mappings.
;
; Required Rust-side Context layout:
;   #[repr(C)]
;   struct Context {
;       rax: u64, rbx: u64, rcx: u64, rdx: u64,
;       rsi: u64, rdi: u64, rbp: u64, rsp: u64,
;       r8:  u64, r9:  u64, r10: u64, r11: u64,
;       r12: u64, r13: u64, r14: u64, r15: u64,
;       rip: u64, rflags: u64,
;       cs:  u16, ss: u16,
;       _pad: [u8; 12],
;       cr3: u64,
;   }
;
; The offsets below are an ABI. If the Rust Context struct changes, update both
; sides together and add compile-time offset assertions in Rust.

[BITS 64]
default rel

section .text

; ---------------------------------------------------------------------------
; Constants
; ---------------------------------------------------------------------------

%define HIGHER_HALF_BASE      0xFFFF800000000000

%define KERNEL_CODE_SELECTOR  0x08
%define USER_DATA_SELECTOR    0x1B    ; GDT[0x18] | RPL=3
%define USER_CODE_SELECTOR    0x23    ; GDT[0x20] | RPL=3

%define RFLAGS_RESERVED_ONE   0x0000000000000002
%define RFLAGS_INTERRUPT      0x0000000000000200

; User-visible status/control flags we allow to survive into CPL=3.
; Deliberately excludes IOPL, NT, VM, RF and privileged/system bits.
%define RFLAGS_USER_ALLOWED   0x0000000000240DD5

; Context offsets.
%define OFF_RAX       0
%define OFF_RBX       8
%define OFF_RCX       16
%define OFF_RDX       24
%define OFF_RSI       32
%define OFF_RDI       40
%define OFF_RBP       48
%define OFF_RSP       56
%define OFF_R8        64
%define OFF_R9        72
%define OFF_R10       80
%define OFF_R11       88
%define OFF_R12       96
%define OFF_R13       104
%define OFF_R14       112
%define OFF_R15       120
%define OFF_RIP       128
%define OFF_RFLAGS    136
%define OFF_CS        144
%define OFF_SS        146
%define OFF_CR3       160

; ---------------------------------------------------------------------------
; Higher-half trampoline
; ---------------------------------------------------------------------------
; Jump to the higher-half alias of a local label if currently executing from
; the lower-half identity mapping. This makes the subsequent CR3 load safe when
; the target address space does not identity-map early kernel text.
;
; x86_64 canonicality rule: bit 47 selects lower/upper canonical half. If bit
; 47 is already set, the address is already higher-half and must not be offset.
%macro ENTER_HIGHER_HALF 1
    lea rax, [rel %1]
    bt  rax, 47
    jc  %%already_higher_half
    mov r11, HIGHER_HALF_BASE
    add rax, r11
%%already_higher_half:
    jmp rax
%endmacro

; ---------------------------------------------------------------------------
; RFLAGS sanitizers
; ---------------------------------------------------------------------------

%macro SANITIZE_USER_RFLAGS_IN_RAX 0
    and rax, RFLAGS_USER_ALLOWED
    or  rax, RFLAGS_RESERVED_ONE | RFLAGS_INTERRUPT
%endmacro

%macro SANITIZE_KERNEL_RFLAGS_IN_RAX 0
    ; Keep the saved kernel flags mostly intact, but ensure architectural bit 1
    ; stays set. Clear NT to avoid task-switch semantics leaking across resumes.
    or  rax, RFLAGS_RESERVED_ONE
    and rax, ~0x4000
%endmacro

; ---------------------------------------------------------------------------
; Context save / restore helpers
; ---------------------------------------------------------------------------

%macro SAVE_CONTEXT 1
    ; %1 = pointer to Context
    mov [%1 + OFF_RAX], rax
    mov [%1 + OFF_RBX], rbx
    mov [%1 + OFF_RCX], rcx
    mov [%1 + OFF_RDX], rdx
    mov [%1 + OFF_RSI], rsi
    mov [%1 + OFF_RDI], rdi
    mov [%1 + OFF_RBP], rbp
    mov [%1 + OFF_R8],  r8
    mov [%1 + OFF_R9],  r9
    mov [%1 + OFF_R10], r10
    mov [%1 + OFF_R11], r11
    mov [%1 + OFF_R12], r12
    mov [%1 + OFF_R13], r13
    mov [%1 + OFF_R14], r14
    mov [%1 + OFF_R15], r15

    ; switch_context was called normally, so [rsp] is the return RIP and
    ; [rsp + 8] is the stack pointer that must be restored before returning.
    mov rax, [rsp]
    mov [%1 + OFF_RIP], rax
    lea rax, [rsp + 8]
    mov [%1 + OFF_RSP], rax

    pushfq
    pop rax
    SANITIZE_KERNEL_RFLAGS_IN_RAX
    mov [%1 + OFF_RFLAGS], rax

    mov ax, cs
    mov [%1 + OFF_CS], ax
    mov ax, ss
    mov [%1 + OFF_SS], ax

    mov rax, cr3
    mov [%1 + OFF_CR3], rax
%endmacro

%macro RESTORE_GPRS_FROM_R15 0
    ; R15 holds Context*. Restore it last.
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
%endmacro

; ---------------------------------------------------------------------------
; enter_user(rip, rsp, cr3)
; ---------------------------------------------------------------------------
; Enters CPL=3 using the provided user RIP/RSP and optional CR3. This is for
; entering an already-initialized user context whose general registers are not
; specified by the caller.
global enter_user
enter_user:
    cli
    ENTER_HIGHER_HALF .hh_enter_user

.hh_enter_user:
    test r8, r8
    jz .enter_user_keep_cr3
    mov cr3, r8
.enter_user_keep_cr3:

    mov ax, USER_DATA_SELECTOR
    mov ds, ax
    mov es, ax

    ; Build an IRET frame:
    ;   [rsp+00] RIP
    ;   [rsp+08] CS
    ;   [rsp+16] RFLAGS
    ;   [rsp+24] RSP
    ;   [rsp+32] SS
    sub rsp, 40
    mov [rsp + 0], rcx
    mov qword [rsp + 8], USER_CODE_SELECTOR

    pushfq
    pop rax
    SANITIZE_USER_RFLAGS_IN_RAX
    mov [rsp + 16], rax

    mov [rsp + 24], rdx
    mov qword [rsp + 32], USER_DATA_SELECTOR

    ; The interrupt entry path uses swapgs when arriving from CPL=3. Mirror that
    ; convention when leaving the kernel for userspace.
    swapgs
    iretq

; ---------------------------------------------------------------------------
; enter_user_first_time(rip, rsp, cr3)
; ---------------------------------------------------------------------------
; Same as enter_user, but zeros all GPRs before IRET so first userspace entry
; starts from a deterministic register state.
global enter_user_first_time
enter_user_first_time:
    cli
    ENTER_HIGHER_HALF .hh_enter_user_first_time

.hh_enter_user_first_time:
    test r8, r8
    jz .first_keep_cr3
    mov cr3, r8
.first_keep_cr3:

    mov ax, USER_DATA_SELECTOR
    mov ds, ax
    mov es, ax

    sub rsp, 40
    mov [rsp + 0], rcx
    mov qword [rsp + 8], USER_CODE_SELECTOR

    pushfq
    pop rax
    SANITIZE_USER_RFLAGS_IN_RAX
    mov [rsp + 16], rax

    mov [rsp + 24], rdx
    mov qword [rsp + 32], USER_DATA_SELECTOR

    swapgs

    xor rax, rax
    xor rbx, rbx
    xor rcx, rcx
    xor rdx, rdx
    xor rsi, rsi
    xor rdi, rdi
    xor rbp, rbp
    xor r8,  r8
    xor r9,  r9
    xor r10, r10
    xor r11, r11
    xor r12, r12
    xor r13, r13
    xor r14, r14
    xor r15, r15

    iretq

; ---------------------------------------------------------------------------
; switch_context(old, new)
; ---------------------------------------------------------------------------
; Saves the current cooperative continuation into *old and resumes *new.
;
; Important ABI note:
;   RCX and RDX already contain the function arguments under MS x64, so the
;   saved RCX/RDX fields reflect the call ABI state, not arbitrary pre-call
;   caller-saved values. That is correct for a cooperative switch routine:
;   callers cannot rely on caller-saved registers surviving a function call.
global switch_context
switch_context:
    ; Save the caller-provided `old` pointer via RCX. Avoid clobbering
    ; other caller-saved registers before they are saved.
    SAVE_CONTEXT rcx

    ; Resume new context.
    mov r15, rdx
    jmp switch_to_context_internal

; ---------------------------------------------------------------------------
; switch_to_context(new)
; ---------------------------------------------------------------------------
; Resumes a previously prepared Context without saving the current one.
global switch_to_context
switch_to_context:
    mov r15, rcx
    jmp switch_to_context_internal

; ---------------------------------------------------------------------------
; Shared restore path
; ---------------------------------------------------------------------------
switch_to_context_internal:
    cli
    ENTER_HIGHER_HALF .hh_switch_to_context

.hh_switch_to_context:
    ; Switch address spaces if requested and different.
    mov rax, [r15 + OFF_CR3]
    test rax, rax
    jz .cr3_ready

    mov rbx, cr3
    cmp rax, rbx
    je .cr3_ready

    mov cr3, rax

.cr3_ready:
    ; Decide by saved CS.RPL, not by a single kernel selector value. This keeps
    ; the path correct if more kernel/user code selectors are added later.
    movzx eax, word [r15 + OFF_CS]
    test ax, 0x3
    jnz .restore_user

.restore_kernel:
    ; Ring0 -> ring0: IRETQ would not pop RSP/SS, so use RET after restoring
    ; the target kernel stack and pushing its continuation RIP.
    mov rsp, [r15 + OFF_RSP]

    mov rax, [r15 + OFF_RIP]
    push rax

    mov rax, [r15 + OFF_RFLAGS]
    SANITIZE_KERNEL_RFLAGS_IN_RAX
    push rax
    popfq

    RESTORE_GPRS_FROM_R15
    ret

.restore_user:
    ; Ring0 -> ring3: use IRETQ so the CPU atomically loads RIP, CS, RFLAGS,
    ; user RSP and SS.
    mov ax, USER_DATA_SELECTOR
    mov ds, ax
    mov es, ax

    sub rsp, 40

    mov rax, [r15 + OFF_RIP]
    mov [rsp + 0], rax

    ; Enforce the known userspace selectors instead of trusting stale Context
    ; values. The RPL=3 bit in CS chose this path.
    mov qword [rsp + 8], USER_CODE_SELECTOR

    mov rax, [r15 + OFF_RFLAGS]
    SANITIZE_USER_RFLAGS_IN_RAX
    mov [rsp + 16], rax

    mov rax, [r15 + OFF_RSP]
    mov [rsp + 24], rax
    mov qword [rsp + 32], USER_DATA_SELECTOR

    swapgs

    RESTORE_GPRS_FROM_R15
    iretq