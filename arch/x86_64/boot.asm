; arch/x86_64/boot.asm
;
; Atom Kernel — x86_64 UEFI entry (QEMU + máquinas reais)
;
; This file is a *UEFI application* entry point in NASM syntax.
; UEFI already runs in 64-bit long mode, using the Microsoft x64 ABI.
;
; What this file does (and ONLY does, by design):
; - Provides the PE/COFF entry symbol: efi_entry
; - Sets up a known-good stack (16-byte aligned)
; - Bridges into Rust: efi_main(ImageHandle, SystemTable)
;
; Why we keep UEFI logic out of assembly:
; - Calling UEFI Boot Services involves deep struct offsets, variable layouts,
;   and careful error handling. It's much safer and more maintainable in Rust.
;
; Requirements to be truly "functional":
; - You must implement `efi_main` in Rust (no_std) and build a PE64 EFI app.
; - That Rust function should:
;   1) display boot screen using UEFI console services,
;   2) get memory map,
;   3) ExitBootServices,
;   4) set up page tables if desired (optional at MVP),
;   5) jump to `kmain()`.
;
; This is the smallest boot shim that works reliably on real UEFI and QEMU.
;
; Assemble with: nasm -f win64 arch/x86_64/boot.asm -o boot.obj
; Link as PE/COFF EFI app with lld-link or rust-lld (subsystem:efi_application).

BITS 64
DEFAULT REL

GLOBAL efi_entry
EXTERN efi_main

SECTION .text

; UEFI entry point:
; EFI_STATUS efi_main(EFI_HANDLE ImageHandle, EFI_SYSTEM_TABLE* SystemTable)
; - ImageHandle in RCX
; - SystemTable  in RDX
; MS x64 ABI requirements:
; - 32 bytes of shadow space reserved by caller
; - Stack aligned so that (RSP + 8) % 16 == 0 at the CALL instruction
efi_entry:
    ; Establish a clean stack.
    ; stack_top is 16-byte aligned by construction.
    lea     rsp, [stack_top - 8]     ; make RSP % 16 == 8 before CALL
    sub     rsp, 0x20                ; 32-byte shadow space

    ; Call into Rust, preserving the UEFI arguments in RCX/RDX.
    ; Rust function signature:
    ;   extern "win64" fn efi_main(image: EfiHandle,
    ;                              system_table: *mut c_void) -> EfiStatus;
    call    efi_main

    ; If Rust returns, we halt.
.hang:
    hlt
    jmp     .hang


SECTION .bss
ALIGN 16

; A small fixed stack for the UEFI phase.
; Keep it conservative; Rust UEFI code should avoid deep recursion.
stack_bottom:
    resb    262144                   ; 256 KiB
ALIGN 16
stack_top:
