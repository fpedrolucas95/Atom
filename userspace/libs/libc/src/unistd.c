/*
 * unistd.c — AtomOS libc POSIX-ish syscall wrappers.
 *
 * Maps standard POSIX functions to AtomOS kernel syscalls, translating kernel
 * error codes (u64::MAX - N) into conventional POSIX errno values.
 */

#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <string.h>
#include <stdlib.h>
#include <errno.h>
#include <stdint.h>
#include <stdarg.h>
#include "atom_syscall.h"

/* =========================================================================
 * Basic I/O
 * ========================================================================= */

ssize_t read(int fd, void *buf, size_t count)
{
    if (!buf || count == 0) return 0;
    uint64_t ret = atom_syscall3(SYS_FS_READ,
                                 (uint64_t)fd,
                                 (uint64_t)(uintptr_t)buf,
                                 (uint64_t)count);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return (ssize_t)ret;
}

ssize_t write(int fd, const void *buf, size_t count)
{
    if (!buf || count == 0) return 0;
    uint64_t ret = atom_syscall3(SYS_FS_WRITE,
                                 (uint64_t)fd,
                                 (uint64_t)(uintptr_t)buf,
                                 (uint64_t)count);
    if (atom_is_error(ret)) {
        /* Fallback to debug log for stdout / stderr */
        if (fd == 1 || fd == 2) {
            atom_syscall2(SYS_DEBUG_LOG, (uint64_t)(uintptr_t)buf, (uint64_t)count);
            return (ssize_t)count;
        }
        return atom_set_errno(ret);
    }
    return (ssize_t)ret;
}

int close(int fd)
{
    uint64_t ret = atom_syscall1(SYS_FS_CLOSE, (uint64_t)fd);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

/* =========================================================================
 * open() — declared in fcntl.h but implemented here with the filesystem
 * ========================================================================= */

int open(const char *path, int flags, ...)
{
    if (!path) { errno = EINVAL; return -1; }

    mode_t mode = 0644;
    if (flags & O_CREAT) {
        va_list ap; va_start(ap, flags);
        mode = (mode_t)va_arg(ap, unsigned int);
        va_end(ap);
    }

    size_t len = strlen(path);
    uint64_t ret = atom_syscall4(SYS_FS_OPEN,
                                 (uint64_t)(uintptr_t)path,
                                 (uint64_t)len,
                                 (uint64_t)(unsigned)flags,
                                 (uint64_t)mode);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return (int)(uint32_t)ret;
}

int creat(const char *path, mode_t mode)
{
    return open(path, O_WRONLY | O_CREAT | O_TRUNC, mode);
}

/* =========================================================================
 * File positioning
 * ========================================================================= */

off_t lseek(int fd, off_t offset, int whence)
{
    uint64_t ret = atom_syscall3(SYS_FS_SEEK,
                                 (uint64_t)fd,
                                 (uint64_t)(int64_t)offset,
                                 (uint64_t)(unsigned)whence);
    if (atom_is_error(ret)) { atom_set_errno(ret); return (off_t)-1; }
    return (off_t)ret;
}

/* =========================================================================
 * File descriptor duplication
 * ========================================================================= */

int dup(int fd)
{
    uint64_t ret = atom_syscall1(SYS_FS_DUP, (uint64_t)fd);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return (int)(uint32_t)ret;
}

int dup2(int oldfd, int newfd)
{
    uint64_t ret = atom_syscall2(SYS_FS_DUP2, (uint64_t)oldfd, (uint64_t)newfd);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return (int)(uint32_t)ret;
}

/* =========================================================================
 * File / directory operations
 * ========================================================================= */

int unlink(const char *path)
{
    if (!path) { errno = EINVAL; return -1; }
    size_t len = strlen(path);
    uint64_t ret = atom_syscall2(SYS_FS_UNLINK,
                                 (uint64_t)(uintptr_t)path, (uint64_t)len);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int rmdir(const char *path)
{
    if (!path) { errno = EINVAL; return -1; }
    size_t len = strlen(path);
    uint64_t ret = atom_syscall2(SYS_FS_RMDIR,
                                 (uint64_t)(uintptr_t)path, (uint64_t)len);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int rename(const char *oldpath, const char *newpath)
{
    if (!oldpath || !newpath) { errno = EINVAL; return -1; }
    size_t olen = strlen(oldpath), nlen = strlen(newpath);
    uint64_t ret = atom_syscall4(SYS_FS_RENAME,
                                 (uint64_t)(uintptr_t)oldpath, (uint64_t)olen,
                                 (uint64_t)(uintptr_t)newpath, (uint64_t)nlen);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int symlink(const char *target, const char *linkpath)
{
    if (!target || !linkpath) { errno = EINVAL; return -1; }
    size_t tlen = strlen(target), llen = strlen(linkpath);
    uint64_t ret = atom_syscall4(SYS_FS_SYMLINK,
                                 (uint64_t)(uintptr_t)target, (uint64_t)tlen,
                                 (uint64_t)(uintptr_t)linkpath, (uint64_t)llen);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

ssize_t readlink(const char *path, char *buf, size_t bufsiz)
{
    if (!path || !buf) { errno = EINVAL; return -1; }
    size_t plen = strlen(path);
    uint64_t ret = atom_syscall4(SYS_FS_READLINK,
                                 (uint64_t)(uintptr_t)path, (uint64_t)plen,
                                 (uint64_t)(uintptr_t)buf,  (uint64_t)bufsiz);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return (ssize_t)ret;
}

int link(const char *oldpath, const char *newpath)
{
    if (!oldpath || !newpath) { errno = EINVAL; return -1; }
    size_t olen = strlen(oldpath), nlen = strlen(newpath);
    uint64_t ret = atom_syscall4(SYS_FS_LINK,
                                 (uint64_t)(uintptr_t)oldpath, (uint64_t)olen,
                                 (uint64_t)(uintptr_t)newpath, (uint64_t)nlen);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int ftruncate(int fd, off_t length)
{
    uint64_t ret = atom_syscall2(SYS_FS_TRUNCATE, (uint64_t)fd, (uint64_t)length);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int truncate(const char *path, off_t length)
{
    /* Open, truncate, close */
    int fd = open(path, O_WRONLY);
    if (fd < 0) return -1;
    int r = ftruncate(fd, length);
    int e = errno;
    close(fd);
    errno = e;
    return r;
}

int fsync(int fd)
{
    uint64_t ret = atom_syscall1(SYS_FS_FSYNC, (uint64_t)fd);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int access(const char *path, int mode)
{
    /* Simple implementation: use stat to check existence */
    if (!path) { errno = EINVAL; return -1; }
    /* Use stat to check existence and permissions */
    uint8_t buf[80];
    size_t plen = strlen(path);
    uint64_t ret = atom_syscall3(SYS_FS_STAT,
                                 (uint64_t)(uintptr_t)path, (uint64_t)plen,
                                 (uint64_t)(uintptr_t)buf);
    if (atom_is_error(ret)) { atom_set_errno(ret); return -1; }
    (void)mode; /* Full permission check not yet implemented */
    return 0;
}

/* =========================================================================
 * stat / fstat (sys/stat.h)
 * ========================================================================= */

/*
 * The kernel serialises stat as an 80-byte little-endian record:
 *   off  0: u64 size
 *   off  8: u64 inode
 *   off 16: u64 mtime_ns
 *   off 24: u64 atime_ns
 *   off 32: u64 ctime_ns
 *   off 40: u32 mode
 *   off 44: u16 uid
 *   off 46: u16 gid
 *   off 48: u32 nlinks
 *   off 52: u32 nblocks
 *   off 56: u32 blksize
 *   off 60: u32 dev
 */
static uint64_t stat_le64(const uint8_t *p)
{
    return (uint64_t)p[0]        | ((uint64_t)p[1] <<  8) |
           ((uint64_t)p[2] << 16) | ((uint64_t)p[3] << 24) |
           ((uint64_t)p[4] << 32) | ((uint64_t)p[5] << 40) |
           ((uint64_t)p[6] << 48) | ((uint64_t)p[7] << 56);
}

static uint32_t stat_le32(const uint8_t *p)
{
    return (uint32_t)p[0]        | ((uint32_t)p[1] <<  8) |
           ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static uint16_t stat_le16(const uint8_t *p)
{
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static void parse_stat_buf(const uint8_t *b, struct stat *st)
{
    st->st_size    = (off_t)   stat_le64(b +  0);
    st->st_ino     = (ino_t)   stat_le64(b +  8);
    st->st_mtime   = (time_t) (stat_le64(b + 16) / 1000000000ULL);
    st->st_atime   = (time_t) (stat_le64(b + 24) / 1000000000ULL);
    st->st_ctime   = (time_t) (stat_le64(b + 32) / 1000000000ULL);
    st->st_mode    = (mode_t)  stat_le32(b + 40);
    st->st_uid     = (uid_t)   stat_le16(b + 44);
    st->st_gid     = (gid_t)   stat_le16(b + 46);
    st->st_nlink   = (nlink_t) stat_le32(b + 48);
    st->st_blocks  = (blkcnt_t)stat_le32(b + 52);
    st->st_blksize = (blksize_t)stat_le32(b + 56);
    st->st_dev     = (dev_t)   stat_le32(b + 60);
}

int stat(const char *path, struct stat *buf)
{
    if (!path || !buf) { errno = EINVAL; return -1; }
    uint8_t raw[80];
    size_t plen = strlen(path);
    uint64_t ret = atom_syscall3(SYS_FS_STAT,
                                 (uint64_t)(uintptr_t)path, (uint64_t)plen,
                                 (uint64_t)(uintptr_t)raw);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    parse_stat_buf(raw, buf);
    return 0;
}

int fstat(int fd, struct stat *buf)
{
    if (!buf) { errno = EINVAL; return -1; }
    uint8_t raw[80];
    uint64_t ret = atom_syscall2(SYS_FS_FSTAT,
                                 (uint64_t)fd, (uint64_t)(uintptr_t)raw);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    parse_stat_buf(raw, buf);
    return 0;
}

int lstat(const char *path, struct stat *buf)
{
    /* AtomOS does not distinguish lstat/stat yet */
    return stat(path, buf);
}

int mkdir(const char *path, mode_t mode)
{
    if (!path) { errno = EINVAL; return -1; }
    size_t plen = strlen(path);
    uint64_t ret = atom_syscall3(SYS_FS_MKDIR,
                                 (uint64_t)(uintptr_t)path, (uint64_t)plen,
                                 (uint64_t)mode);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

int chmod(const char *path, mode_t mode)
{
    if (!path) { errno = EINVAL; return -1; }
    size_t plen = strlen(path);
    uint64_t ret = atom_syscall3(SYS_FS_CHMOD,
                                 (uint64_t)(uintptr_t)path, (uint64_t)plen,
                                 (uint64_t)mode);
    if (atom_is_error(ret)) return atom_set_errno(ret);
    return 0;
}

/* =========================================================================
 * Process
 * ========================================================================= */

pid_t getpid(void)
{
    /* AtomOS does not expose getpid yet; return 1 as a safe placeholder */
    return 1;
}

pid_t getppid(void) { return 0; }

/* =========================================================================
 * Working directory (stubs — AtomOS CWD not yet wired)
 * ========================================================================= */

char *getcwd(char *buf, size_t size)
{
    if (!buf || size == 0) { errno = EINVAL; return NULL; }
    if (size < 2) { errno = ERANGE; return NULL; }
    buf[0] = '/'; buf[1] = '\0';
    return buf;
}

int chdir(const char *path)
{
    (void)path;
    errno = ENOSYS;
    return -1;
}

/* =========================================================================
 * Sleeping
 * ========================================================================= */

unsigned int sleep(unsigned int seconds)
{
    atom_syscall1(SYS_THREAD_SLEEP, (uint64_t)seconds * 1000ULL);
    return 0;
}

int usleep(unsigned long usec)
{
    atom_syscall1(SYS_THREAD_SLEEP, (uint64_t)(usec / 1000ULL));
    return 0;
}
