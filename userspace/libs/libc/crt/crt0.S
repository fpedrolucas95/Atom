/*
 * crt0.S — AtomOS C Runtime Startup (x86_64, AT&T syntax)
 *
 * This is the first code that executes in a C userspace process on AtomOS.
 * The kernel jumps to `_start` after setting up the initial stack and
 * mapping the ATXF binary.
 *
 * Calling convention:
 *   AtomOS does not currently pass argc/argv/envp from the kernel.
 *   We default to argc=0, argv=NULL, envp=NULL for now.
 *   When the kernel gains process argument passing, update this file.
 *
 * Startup sequence:
 *   1. Clear the frame pointer (stack unwind terminates here)
 *   2. Align the stack to 16 bytes (System V AMD64 ABI requirement)
 *   3. Call __libc_init()  — sets up errno, stdio streams, etc.
 *   4. Call main(argc, argv, envp) with zeroed arguments
 *   5. Pass main's return value to exit()
 *   6. If exit() somehow returns, spin forever
 */

    .section .text
    .globl  _start
    .type   _start, @function

_start:
    /* ------------------------------------------------------------------ */
    /* 1. Clear the frame pointer so that stack traces terminate here.    */
    /* ------------------------------------------------------------------ */
    xorq    %rbp, %rbp

    /* ------------------------------------------------------------------ */
    /* 2. Align the stack pointer to a 16-byte boundary.                  */
    /*    The ABI requires RSP % 16 == 0 before a call instruction.       */
    /*    A call pushes 8 bytes (return address), making RSP % 16 == 8    */
    /*    just before the callee prologue — that is also acceptable.      */
    /* ------------------------------------------------------------------ */
    andq    $-16, %rsp

    /* ------------------------------------------------------------------ */
    /* 3. Initialise the C library (stdio streams, errno, etc.)           */
    /* ------------------------------------------------------------------ */
    call    __libc_init

    /* ------------------------------------------------------------------ */
    /* 4. Call main(argc=0, argv=NULL, envp=NULL)                         */
    /* ------------------------------------------------------------------ */
    xorq    %rdi, %rdi       /* argc = 0  */
    xorq    %rsi, %rsi       /* argv = NULL */
    xorq    %rdx, %rdx       /* envp = NULL */
    call    main

    /* ------------------------------------------------------------------ */
    /* 5. Exit with the value returned by main.                           */
    /* ------------------------------------------------------------------ */
    movq    %rax, %rdi       /* exit status = main() return value */
    call    exit

    /* ------------------------------------------------------------------ */
    /* 6. Should never reach here — spin to satisfy the noreturn path.    */
    /* ------------------------------------------------------------------ */
.Lhalt:
    hlt
    jmp     .Lhalt

    .size   _start, . - _start

/* Mark the stack as non-executable (suppresses GNU-stack linker warning) */
    .section .note.GNU-stack, "", @progbits
