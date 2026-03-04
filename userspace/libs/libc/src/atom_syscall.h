/*
 * atom_syscall.h — AtomOS raw syscall interface for libc internal use.
 *
 * This header is NOT part of the public API; it is used only by libc source
 * files.  It provides:
 *   - Syscall number constants (matching kernel/src/syscall/mod.rs)
 *   - Kernel error-code constants (u64::MAX - N)
 *   - Inline assembly syscall helpers
 *   - A translation function: kernel error code → POSIX errno value
 */

#ifndef _ATOM_SYSCALL_H
#define _ATOM_SYSCALL_H

#include <stdint.h>
#include <errno.h>

/* -----------------------------------------------------------------------
 * Syscall numbers — must match kernel/src/syscall/mod.rs
 * ----------------------------------------------------------------------- */

/* Thread management */
#define SYS_THREAD_YIELD      0
#define SYS_THREAD_EXIT       1
#define SYS_THREAD_SLEEP      2
#define SYS_THREAD_CREATE     3

/* Misc */
#define SYS_GET_TICKS        38
#define SYS_DEBUG_LOG        39

/* Graphics */
#define SYS_GET_FRAMEBUFFER  37
#define SYS_MAP_FRAMEBUFFER  41

/* Spawn */
#define SYS_SPAWN_PROCESS    45

/* Filesystem (53–76) */
#define SYS_FS_OPEN          53
#define SYS_FS_CLOSE         54
#define SYS_FS_READ          55
#define SYS_FS_WRITE         56
#define SYS_FS_SEEK          57
#define SYS_FS_STAT          58
#define SYS_FS_FSTAT         59
#define SYS_FS_MKDIR         60
#define SYS_FS_RMDIR         61
#define SYS_FS_UNLINK        62
#define SYS_FS_RENAME        63
#define SYS_FS_READDIR       64
#define SYS_FS_TRUNCATE      65
#define SYS_FS_FSYNC         66
#define SYS_FS_CHMOD         69
#define SYS_FS_DUP           70
#define SYS_FS_DUP2          71
#define SYS_FS_LINK          72
#define SYS_FS_SYMLINK       73
#define SYS_FS_READLINK      74
#define SYS_FS_STATVFS       76

/* Virtual memory (100–103) */
#define SYS_MMAP            100
#define SYS_MUNMAP          101
#define SYS_MPROTECT        102
#define SYS_BRK             103

/* -----------------------------------------------------------------------
 * mmap prot / flag constants (atom_abi values)
 * ----------------------------------------------------------------------- */
#define ATOM_PROT_NONE       0
#define ATOM_PROT_READ       1
#define ATOM_PROT_WRITE      2
#define ATOM_PROT_EXEC       4

#define ATOM_MAP_PRIVATE     0x02
#define ATOM_MAP_ANONYMOUS   0x20
#define ATOM_MAP_FIXED       0x10

/* -----------------------------------------------------------------------
 * Kernel error codes — u64::MAX - N (from shared/abi/src/lib.rs)
 * ----------------------------------------------------------------------- */
#define KERN_ERR_THRESHOLD   (UINT64_C(0xFFFFFFFFFFFFFF00))  /* MAX - 256 */
#define KERN_EINVAL          (UINT64_MAX - UINT64_C(1))
#define KERN_ENOSYS          (UINT64_MAX - UINT64_C(2))
#define KERN_ENOMEM          (UINT64_MAX - UINT64_C(3))
#define KERN_EPERM           (UINT64_MAX - UINT64_C(4))
#define KERN_EBUSY           (UINT64_MAX - UINT64_C(5))
#define KERN_EMSGSIZE        (UINT64_MAX - UINT64_C(6))
#define KERN_ETIMEDOUT       (UINT64_MAX - UINT64_C(7))
#define KERN_EWOULDBLOCK     (UINT64_MAX - UINT64_C(8))
#define KERN_EDEADLK         (UINT64_MAX - UINT64_C(9))
#define KERN_ENOTFOUND       (UINT64_MAX - UINT64_C(10))
#define KERN_ENOENT          (UINT64_MAX - UINT64_C(11))
#define KERN_EEXIST          (UINT64_MAX - UINT64_C(12))
#define KERN_EISDIR          (UINT64_MAX - UINT64_C(13))
#define KERN_ENOTDIR         (UINT64_MAX - UINT64_C(14))
#define KERN_ENOTEMPTY       (UINT64_MAX - UINT64_C(15))
#define KERN_EBADF           (UINT64_MAX - UINT64_C(16))
#define KERN_EFBIG           (UINT64_MAX - UINT64_C(17))
#define KERN_ENOSPC          (UINT64_MAX - UINT64_C(18))
#define KERN_EROFS           (UINT64_MAX - UINT64_C(19))
#define KERN_ENAMETOOLONG    (UINT64_MAX - UINT64_C(20))
#define KERN_EIO             (UINT64_MAX - UINT64_C(21))
#define KERN_EACCES          (UINT64_MAX - UINT64_C(22))
#define KERN_EMFILE          (UINT64_MAX - UINT64_C(23))
#define KERN_EOVERFLOW       (UINT64_MAX - UINT64_C(24))
#define KERN_ECORRUPTED      (UINT64_MAX - UINT64_C(25))
#define KERN_EMLINK          (UINT64_MAX - UINT64_C(26))
#define KERN_EXDEV           (UINT64_MAX - UINT64_C(27))
#define KERN_EPIPE           (UINT64_MAX - UINT64_C(28))
#define KERN_ENOTSUP         (UINT64_MAX - UINT64_C(29))
#define KERN_EAGAIN          (UINT64_MAX - UINT64_C(30))
#define KERN_EINTR           (UINT64_MAX - UINT64_C(31))
#define KERN_E2BIG           (UINT64_MAX - UINT64_C(32))

/* -----------------------------------------------------------------------
 * Raw syscall wrappers — x86_64 System V AMD64 ABI
 *
 * syscall instruction:
 *   rax = syscall number / return value
 *   rdi, rsi, rdx, r10, r8, r9 = arguments (r10 instead of rcx!)
 *   rcx, r11 clobbered by CPU
 * ----------------------------------------------------------------------- */

/*
 * CRITICAL: The kernel's syscall handler (handler.asm) does NOT preserve the
 * caller-saved registers rdi, rsi, rdx, r10, r8, r9 across the syscall.
 * Only the callee-saved registers (rbx, rbp, r12-r15) are saved and restored.
 * The CPU itself clobbers rcx (saved RIP) and r11 (saved RFLAGS).
 *
 * Every register that is NOT used as an input operand in a given wrapper and
 * is NOT preserved by the kernel MUST appear in the clobber list.  Failure to
 * do so lets the compiler keep a live value in that register across the
 * syscall, which the kernel will silently overwrite — causing downstream
 * corruption (e.g. free(0x19) page faults).
 */

static inline uint64_t atom_syscall0(uint64_t num)
{
    uint64_t ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num)
        : "rcx", "r11", "rdi", "rsi", "rdx", "r8", "r9", "r10", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall1(uint64_t num, uint64_t a1)
{
    uint64_t ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1)
        : "rcx", "r11", "rsi", "rdx", "r8", "r9", "r10", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall2(uint64_t num, uint64_t a1, uint64_t a2)
{
    uint64_t ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1), "S"(a2)
        : "rcx", "r11", "rdx", "r8", "r9", "r10", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall3(uint64_t num, uint64_t a1, uint64_t a2,
                                     uint64_t a3)
{
    uint64_t ret;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1), "S"(a2), "d"(a3)
        : "rcx", "r11", "r8", "r9", "r10", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall4(uint64_t num, uint64_t a1, uint64_t a2,
                                     uint64_t a3, uint64_t a4)
{
    uint64_t ret;
    register uint64_t r10 __asm__("r10") = a4;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1), "S"(a2), "d"(a3), "r"(r10)
        : "rcx", "r11", "r8", "r9", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall5(uint64_t num, uint64_t a1, uint64_t a2,
                                     uint64_t a3, uint64_t a4, uint64_t a5)
{
    uint64_t ret;
    register uint64_t r10 __asm__("r10") = a4;
    register uint64_t r8  __asm__("r8")  = a5;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8)
        : "rcx", "r11", "r9", "memory"
    );
    return ret;
}

static inline uint64_t atom_syscall6(uint64_t num, uint64_t a1, uint64_t a2,
                                     uint64_t a3, uint64_t a4, uint64_t a5,
                                     uint64_t a6)
{
    uint64_t ret;
    register uint64_t r10 __asm__("r10") = a4;
    register uint64_t r8  __asm__("r8")  = a5;
    register uint64_t r9  __asm__("r9")  = a6;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "0"(num), "D"(a1), "S"(a2), "d"(a3), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory"
    );
    return ret;
}

/* -----------------------------------------------------------------------
 * Error detection helper
 * ----------------------------------------------------------------------- */

static inline int atom_is_error(uint64_t v)
{
    return v >= KERN_ERR_THRESHOLD;
}

/* -----------------------------------------------------------------------
 * Translate a kernel error code to the POSIX errno value and store in
 * the thread-local errno, then return -1.
 * ----------------------------------------------------------------------- */
static inline int atom_set_errno(uint64_t kern_err)
{
    int posix_err;

    if      (kern_err == KERN_EINVAL)       posix_err = EINVAL;
    else if (kern_err == KERN_ENOSYS)       posix_err = ENOSYS;
    else if (kern_err == KERN_ENOMEM)       posix_err = ENOMEM;
    else if (kern_err == KERN_EPERM)        posix_err = EPERM;
    else if (kern_err == KERN_EBUSY)        posix_err = EBUSY;
    else if (kern_err == KERN_EMSGSIZE)     posix_err = EMSGSIZE;
    else if (kern_err == KERN_ETIMEDOUT)    posix_err = ETIMEDOUT;
    else if (kern_err == KERN_EWOULDBLOCK)  posix_err = EAGAIN;
    else if (kern_err == KERN_EDEADLK)     posix_err = EDEADLK;
    else if (kern_err == KERN_ENOENT)       posix_err = ENOENT;
    else if (kern_err == KERN_ENOTFOUND)    posix_err = ENOENT;
    else if (kern_err == KERN_EEXIST)       posix_err = EEXIST;
    else if (kern_err == KERN_EISDIR)       posix_err = EISDIR;
    else if (kern_err == KERN_ENOTDIR)      posix_err = ENOTDIR;
    else if (kern_err == KERN_ENOTEMPTY)    posix_err = ENOTEMPTY;
    else if (kern_err == KERN_EBADF)        posix_err = EBADF;
    else if (kern_err == KERN_EFBIG)        posix_err = EFBIG;
    else if (kern_err == KERN_ENOSPC)       posix_err = ENOSPC;
    else if (kern_err == KERN_EROFS)        posix_err = EROFS;
    else if (kern_err == KERN_ENAMETOOLONG) posix_err = ENAMETOOLONG;
    else if (kern_err == KERN_EIO)          posix_err = EIO;
    else if (kern_err == KERN_EACCES)       posix_err = EACCES;
    else if (kern_err == KERN_EMFILE)       posix_err = EMFILE;
    else if (kern_err == KERN_EOVERFLOW)    posix_err = EOVERFLOW;
    else if (kern_err == KERN_ECORRUPTED)   posix_err = ECORRUPTED;
    else if (kern_err == KERN_EMLINK)       posix_err = EMLINK;
    else if (kern_err == KERN_EXDEV)        posix_err = EXDEV;
    else if (kern_err == KERN_EPIPE)        posix_err = EPIPE;
    else if (kern_err == KERN_ENOTSUP)      posix_err = ENOTSUP;
    else if (kern_err == KERN_EAGAIN)       posix_err = EAGAIN;
    else if (kern_err == KERN_EINTR)        posix_err = EINTR;
    else if (kern_err == KERN_E2BIG)        posix_err = E2BIG;
    else                                    posix_err = EINVAL;

    errno = posix_err;
    return -1;
}

#endif /* _ATOM_SYSCALL_H */
