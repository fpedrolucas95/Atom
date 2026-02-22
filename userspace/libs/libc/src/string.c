/*
 * string.c — AtomOS libc string and memory functions.
 *
 * Implements all functions declared in <string.h>.  No kernel interaction is
 * needed here; all work is pure userspace computation.
 */

#include <string.h>
#include <stdint.h>
#include <stdlib.h>   /* for malloc/free used by strdup/strndup */
#include <ctype.h>
#include <errno.h>

/* =========================================================================
 * Memory operations
 * ========================================================================= */

void *memcpy(void *dest, const void *src, size_t n)
{
    unsigned char       *d = dest;
    const unsigned char *s = src;

    /* Copy 8 bytes at a time when possible */
    while (n >= 8) {
        uint64_t v;
        __builtin_memcpy(&v, s, 8);
        __builtin_memcpy(d, &v, 8);
        d += 8; s += 8; n -= 8;
    }
    while (n--) *d++ = *s++;
    return dest;
}

void *memmove(void *dest, const void *src, size_t n)
{
    unsigned char       *d = dest;
    const unsigned char *s = src;

    if (d == s || n == 0)
        return dest;

    if (d < s || (uintptr_t)d >= (uintptr_t)s + n) {
        /* Non-overlapping or dest < src: forward copy */
        return memcpy(dest, src, n);
    } else {
        /* Overlapping and dest > src: backward copy */
        d += n;
        s += n;
        while (n--) *--d = *--s;
    }
    return dest;
}

void *memset(void *s, int c, size_t n)
{
    unsigned char *p = s;
    unsigned char  v = (unsigned char)c;

    /* Fill 8 bytes at a time */
    if (n >= 8) {
        uint64_t fill = (uint64_t)v;
        fill |= fill << 8;
        fill |= fill << 16;
        fill |= fill << 32;
        while (n >= 8) {
            __builtin_memcpy(p, &fill, 8);
            p += 8; n -= 8;
        }
    }
    while (n--) *p++ = v;
    return s;
}

int memcmp(const void *s1, const void *s2, size_t n)
{
    const unsigned char *a = s1;
    const unsigned char *b = s2;
    while (n--) {
        if (*a != *b)
            return (int)*a - (int)*b;
        a++; b++;
    }
    return 0;
}

void *memchr(const void *s, int c, size_t n)
{
    const unsigned char *p = s;
    unsigned char        v = (unsigned char)c;
    while (n--) {
        if (*p == v) return (void *)p;
        p++;
    }
    return NULL;
}

/* =========================================================================
 * String copy
 * ========================================================================= */

char *strcpy(char *dest, const char *src)
{
    char *d = dest;
    while ((*d++ = *src++) != '\0') ;
    return dest;
}

char *strncpy(char *dest, const char *src, size_t n)
{
    char *d = dest;
    while (n > 0 && *src != '\0') {
        *d++ = *src++;
        n--;
    }
    /* Pad remaining bytes with NUL (POSIX requirement) */
    while (n-- > 0) *d++ = '\0';
    return dest;
}

/* =========================================================================
 * String concatenation
 * ========================================================================= */

char *strcat(char *dest, const char *src)
{
    char *d = dest + strlen(dest);
    while ((*d++ = *src++) != '\0') ;
    return dest;
}

char *strncat(char *dest, const char *src, size_t n)
{
    char *d = dest + strlen(dest);
    while (n > 0 && *src != '\0') {
        *d++ = *src++;
        n--;
    }
    *d = '\0';
    return dest;
}

/* =========================================================================
 * String length
 * ========================================================================= */

size_t strlen(const char *s)
{
    const char *p = s;
    while (*p) p++;
    return (size_t)(p - s);
}

size_t strnlen(const char *s, size_t maxlen)
{
    size_t n = 0;
    while (n < maxlen && s[n]) n++;
    return n;
}

/* =========================================================================
 * String comparison
 * ========================================================================= */

int strcmp(const char *s1, const char *s2)
{
    const unsigned char *a = (const unsigned char *)s1;
    const unsigned char *b = (const unsigned char *)s2;
    while (*a && *a == *b) { a++; b++; }
    return (int)*a - (int)*b;
}

int strncmp(const char *s1, const char *s2, size_t n)
{
    const unsigned char *a = (const unsigned char *)s1;
    const unsigned char *b = (const unsigned char *)s2;
    while (n > 0 && *a && *a == *b) { a++; b++; n--; }
    if (n == 0) return 0;
    return (int)*a - (int)*b;
}

int strcasecmp(const char *s1, const char *s2)
{
    const unsigned char *a = (const unsigned char *)s1;
    const unsigned char *b = (const unsigned char *)s2;
    while (*a && tolower(*a) == tolower(*b)) { a++; b++; }
    return tolower(*a) - tolower(*b);
}

int strncasecmp(const char *s1, const char *s2, size_t n)
{
    const unsigned char *a = (const unsigned char *)s1;
    const unsigned char *b = (const unsigned char *)s2;
    while (n > 0 && *a && tolower(*a) == tolower(*b)) { a++; b++; n--; }
    if (n == 0) return 0;
    return tolower(*a) - tolower(*b);
}

/* =========================================================================
 * String search
 * ========================================================================= */

char *strchr(const char *s, int c)
{
    unsigned char v = (unsigned char)c;
    for (; *s; s++) {
        if ((unsigned char)*s == v) return (char *)s;
    }
    return v == '\0' ? (char *)s : NULL;
}

char *strrchr(const char *s, int c)
{
    unsigned char v = (unsigned char)c;
    const char *last = NULL;
    for (; *s; s++) {
        if ((unsigned char)*s == v) last = s;
    }
    if (v == '\0') return (char *)s;
    return (char *)last;
}

char *strstr(const char *haystack, const char *needle)
{
    if (*needle == '\0') return (char *)haystack;
    size_t nlen = strlen(needle);
    for (; *haystack; haystack++) {
        if (*haystack == *needle && strncmp(haystack, needle, nlen) == 0)
            return (char *)haystack;
    }
    return NULL;
}

char *strpbrk(const char *s, const char *accept)
{
    for (; *s; s++) {
        const char *a = accept;
        while (*a) {
            if (*s == *a) return (char *)s;
            a++;
        }
    }
    return NULL;
}

/* =========================================================================
 * Span functions
 * ========================================================================= */

size_t strspn(const char *s, const char *accept)
{
    size_t n = 0;
    while (s[n]) {
        const char *a = accept;
        int found = 0;
        while (*a) { if (s[n] == *a++) { found = 1; break; } }
        if (!found) break;
        n++;
    }
    return n;
}

size_t strcspn(const char *s, const char *reject)
{
    size_t n = 0;
    while (s[n]) {
        const char *r = reject;
        while (*r) { if (s[n] == *r) return n; r++; }
        n++;
    }
    return n;
}

/* =========================================================================
 * Tokenisation
 * ========================================================================= */

char *strtok_r(char *str, const char *delim, char **saveptr)
{
    char *s = str ? str : *saveptr;

    /* Skip leading delimiters */
    s += strspn(s, delim);
    if (*s == '\0') {
        *saveptr = s;
        return NULL;
    }

    /* Find end of token */
    char *end = s + strcspn(s, delim);
    if (*end != '\0') {
        *end = '\0';
        *saveptr = end + 1;
    } else {
        *saveptr = end;
    }
    return s;
}

char *strtok(char *str, const char *delim)
{
    static char *saved = NULL;
    return strtok_r(str, delim, &saved);
}

/* =========================================================================
 * Duplication (uses malloc from stdlib.c)
 * ========================================================================= */

char *strdup(const char *s)
{
    size_t len = strlen(s) + 1;
    char *p = malloc(len);
    if (p) memcpy(p, s, len);
    return p;
}

char *strndup(const char *s, size_t n)
{
    size_t len = strnlen(s, n);
    char *p = malloc(len + 1);
    if (p) {
        memcpy(p, s, len);
        p[len] = '\0';
    }
    return p;
}

/* =========================================================================
 * Error message
 * ========================================================================= */

char *strerror(int errnum)
{
    switch (errnum) {
    case 0:           return "Success";
    case EPERM:       return "Operation not permitted";
    case ENOENT:      return "No such file or directory";
    case EINTR:       return "Interrupted system call";
    case EIO:         return "I/O error";
    case E2BIG:       return "Argument list too long";
    case EBADF:       return "Bad file descriptor";
    case EAGAIN:      return "Resource temporarily unavailable";
    case ENOMEM:      return "Out of memory";
    case EACCES:      return "Permission denied";
    case EEXIST:      return "File exists";
    case EXDEV:       return "Cross-device link";
    case ENODEV:      return "No such device";
    case ENOTDIR:     return "Not a directory";
    case EISDIR:      return "Is a directory";
    case EINVAL:      return "Invalid argument";
    case EMFILE:      return "Too many open files";
    case EFBIG:       return "File too large";
    case ENOSPC:      return "No space left on device";
    case EROFS:       return "Read-only file system";
    case EMLINK:      return "Too many links";
    case EPIPE:       return "Broken pipe";
    case ERANGE:      return "Math result not representable";
    case ENOSYS:      return "Function not implemented";
    case ENOTEMPTY:   return "Directory not empty";
    case EOVERFLOW:   return "Value too large";
    case ENAMETOOLONG:return "File name too long";
    case ENOTSUP:     return "Operation not supported";
    case ETIMEDOUT:   return "Connection timed out";
    case EDEADLK:     return "Resource deadlock would occur";
    case ECORRUPTED:  return "Filesystem corrupted";
    default:          return "Unknown error";
    }
}

