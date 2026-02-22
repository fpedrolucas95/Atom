/*
 * stdlib.c — AtomOS libc general utilities.
 *
 * Implements: malloc/free/realloc, exit/abort, atoi family, strtol family,
 * abs/div, rand/srand, bsearch, qsort, and getenv.
 *
 * Memory allocation strategy:
 *   - Each allocation uses SYS_MMAP to obtain one or more anonymous pages.
 *   - A header prepended to the allocation records the total mapped size.
 *   - free() calls SYS_MUNMAP to return pages to the kernel.
 *   - This is simple and correct; production use would want a pooling allocator.
 */

#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>
#include "atom_syscall.h"

/* =========================================================================
 * Internal atexit registry
 * ========================================================================= */

#define ATEXIT_MAX 32

static void (*atexit_funcs[ATEXIT_MAX])(void);
static int   atexit_count = 0;

int atexit(void (*func)(void))
{
    if (atexit_count >= ATEXIT_MAX) { errno = ENOMEM; return -1; }
    atexit_funcs[atexit_count++] = func;
    return 0;
}

/* =========================================================================
 * Process termination
 * ========================================================================= */

void _exit(int status)
{
    atom_syscall1(SYS_THREAD_EXIT, (uint64_t)(unsigned)status);
    /* Should never return */
    for (;;) __asm__ volatile ("hlt");
}

void exit(int status)
{
    /* Call registered atexit handlers in LIFO order */
    int i = atexit_count;
    while (i-- > 0) atexit_funcs[i]();
    _exit(status);
}

void abort(void)
{
    _exit(134);
}

/* =========================================================================
 * Memory allocation
 *
 * Layout per block:
 *   [ malloc_hdr (16 bytes) | user data ... ]
 *   hdr->mapped_size = total mmap'd size (rounded up to PAGE_SIZE)
 * ========================================================================= */

#define PAGE_SIZE       4096UL
#define HDR_MAGIC       UINT64_C(0xA70CDEADBEEF0000)
#define HDR_SIZE        16

typedef struct {
    uint64_t mapped_size;   /* total size passed to mmap/munmap       */
    uint64_t magic;         /* HDR_MAGIC — detects corruption          */
} malloc_hdr_t;

static inline uint64_t page_align(uint64_t n)
{
    return (n + PAGE_SIZE - 1) & ~(PAGE_SIZE - 1);
}

void *malloc(size_t size)
{
    if (size == 0) size = 1;

    /* Align user size to 16 bytes, then add header */
    size_t user_size  = (size + 15) & ~(size_t)15;
    size_t total      = user_size + HDR_SIZE;
    size_t map_size   = page_align(total);

    uint64_t ret = atom_syscall4(
        SYS_MMAP,
        0,
        (uint64_t)map_size,
        ATOM_PROT_READ | ATOM_PROT_WRITE,
        ATOM_MAP_PRIVATE | ATOM_MAP_ANONYMOUS
    );

    if (atom_is_error(ret)) {
        errno = ENOMEM;
        return NULL;
    }

    malloc_hdr_t *hdr = (malloc_hdr_t *)(uintptr_t)ret;
    hdr->mapped_size = map_size;
    hdr->magic       = HDR_MAGIC;

    return (void *)(hdr + 1);
}

void free(void *ptr)
{
    if (!ptr) return;

    malloc_hdr_t *hdr = (malloc_hdr_t *)ptr - 1;
    if (hdr->magic != HDR_MAGIC) {
        /* Corrupted or double-free; bail silently to avoid further damage */
        return;
    }
    uint64_t sz = hdr->mapped_size;
    hdr->magic = 0;  /* poison to catch use-after-free */
    atom_syscall2(SYS_MUNMAP, (uint64_t)(uintptr_t)hdr, sz);
}

void *calloc(size_t nmemb, size_t size)
{
    /* Check for overflow */
    if (nmemb != 0 && size > SIZE_MAX / nmemb) {
        errno = ENOMEM;
        return NULL;
    }
    size_t total = nmemb * size;
    /* mmap always returns zeroed pages, so memset is not required */
    return malloc(total);
}

void *realloc(void *ptr, size_t size)
{
    if (!ptr)   return malloc(size);
    if (!size)  { free(ptr); return NULL; }

    malloc_hdr_t *hdr = (malloc_hdr_t *)ptr - 1;
    if (hdr->magic != HDR_MAGIC) { errno = EINVAL; return NULL; }

    /* Compute old usable size */
    size_t old_usable = (size_t)hdr->mapped_size - HDR_SIZE;

    void *new_ptr = malloc(size);
    if (!new_ptr) return NULL;

    size_t copy_len = old_usable < size ? old_usable : size;
    memcpy(new_ptr, ptr, copy_len);
    free(ptr);
    return new_ptr;
}

void *aligned_alloc(size_t alignment, size_t size)
{
    /* For simplicity, mmap guarantees page alignment (>= any reasonable value) */
    (void)alignment;
    return malloc(size);
}

/* =========================================================================
 * Numeric string conversions
 * ========================================================================= */

static int is_digit(char c)  { return c >= '0' && c <= '9'; }
static int is_space(char c)  { return c == ' ' || c == '\t' || c == '\n' ||
                                       c == '\r' || c == '\v' || c == '\f'; }
static int to_digit(char c, int base)
{
    int v;
    if (c >= '0' && c <= '9') v = c - '0';
    else if (c >= 'a' && c <= 'z') v = c - 'a' + 10;
    else if (c >= 'A' && c <= 'Z') v = c - 'A' + 10;
    else return -1;
    return v < base ? v : -1;
}

long strtol(const char *nptr, char **endptr, int base)
{
    const char *s = nptr;
    while (is_space(*s)) s++;

    int neg = 0;
    if (*s == '-') { neg = 1; s++; }
    else if (*s == '+') { s++; }

    if (base == 0) {
        if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
        else if (s[0] == '0') { base = 8; s++; }
        else base = 10;
    } else if (base == 16 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }

    long val = 0;
    int any = 0;
    int d;
    while ((d = to_digit(*s, base)) >= 0) {
        val = val * base + d;
        any = 1; s++;
    }

    if (endptr) *endptr = (char *)(any ? s : nptr);
    return neg ? -val : val;
}

unsigned long strtoul(const char *nptr, char **endptr, int base)
{
    const char *s = nptr;
    while (is_space(*s)) s++;
    if (*s == '+') s++;

    if (base == 0) {
        if (s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
        else if (s[0] == '0') { base = 8; s++; }
        else base = 10;
    } else if (base == 16 && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }

    unsigned long val = 0;
    int any = 0, d;
    while ((d = to_digit(*s, base)) >= 0) {
        val = val * (unsigned)base + (unsigned)d;
        any = 1; s++;
    }
    if (endptr) *endptr = (char *)(any ? s : nptr);
    return val;
}

long long strtoll(const char *nptr, char **endptr, int base)
{
    return (long long)strtol(nptr, endptr, base);
}

unsigned long long strtoull(const char *nptr, char **endptr, int base)
{
    return (unsigned long long)strtoul(nptr, endptr, base);
}

int atoi(const char *nptr)
{
    return (int)strtol(nptr, NULL, 10);
}

long atol(const char *nptr)
{
    return strtol(nptr, NULL, 10);
}

long long atoll(const char *nptr)
{
    return strtoll(nptr, NULL, 10);
}

double atof(const char *nptr)
{
    return strtod(nptr, NULL);
}

double strtod(const char *nptr, char **endptr)
{
    const char *s = nptr;
    while (is_space(*s)) s++;

    double sign = 1.0;
    if (*s == '-') { sign = -1.0; s++; }
    else if (*s == '+') { s++; }

    double val = 0.0, frac = 0.0, div = 1.0;
    int any = 0;

    while (is_digit(*s)) { val = val * 10.0 + (*s - '0'); any = 1; s++; }
    if (*s == '.') {
        s++;
        while (is_digit(*s)) {
            frac += (*s - '0') / (div *= 10.0);
            any = 1; s++;
        }
    }

    if (endptr) *endptr = (char *)(any ? s : nptr);
    return sign * (val + frac);
}

/* =========================================================================
 * Integer absolute value / division
 * ========================================================================= */

int abs(int j)       { return j < 0 ? -j : j; }
long labs(long j)    { return j < 0 ? -j : j; }
long long llabs(long long j) { return j < 0 ? -j : j; }

div_t div(int numer, int denom)
{
    div_t r = { numer / denom, numer % denom };
    return r;
}

ldiv_t ldiv(long numer, long denom)
{
    ldiv_t r = { numer / denom, numer % denom };
    return r;
}

lldiv_t lldiv(long long numer, long long denom)
{
    lldiv_t r = { numer / denom, numer % denom };
    return r;
}

/* =========================================================================
 * Pseudo-random number generation (LCG — good enough for non-crypto use)
 * ========================================================================= */

static unsigned long rand_seed = 1;

void srand(unsigned int seed)
{
    rand_seed = seed;
}

int rand(void)
{
    rand_seed = rand_seed * 6364136223846793005ULL + 1442695040888963407ULL;
    return (int)((rand_seed >> 33) & (unsigned long)RAND_MAX);
}

/* =========================================================================
 * Binary search
 * ========================================================================= */

void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*compar)(const void *, const void *))
{
    size_t lo = 0, hi = nmemb;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        const void *mid_ptr = (const unsigned char *)base + mid * size;
        int cmp = compar(key, mid_ptr);
        if      (cmp < 0) hi = mid;
        else if (cmp > 0) lo = mid + 1;
        else              return (void *)mid_ptr;
    }
    return NULL;
}

/* =========================================================================
 * Quicksort (in-place, median-of-three pivot)
 * ========================================================================= */

static void swap_bytes(unsigned char *a, unsigned char *b, size_t size)
{
    while (size--) {
        unsigned char t = *a;
        *a++ = *b;
        *b++ = t;
    }
}

static void qsort_r(unsigned char *base, size_t lo, size_t hi, size_t size,
                    int (*cmp)(const void *, const void *))
{
    if (lo >= hi) return;

    /* Median-of-three pivot selection */
    size_t mid = lo + (hi - lo) / 2;
    if (cmp(base + lo * size, base + mid * size) > 0)
        swap_bytes(base + lo * size, base + mid * size, size);
    if (cmp(base + lo * size, base + hi * size) > 0)
        swap_bytes(base + lo * size, base + hi * size, size);
    if (cmp(base + mid * size, base + hi * size) > 0)
        swap_bytes(base + mid * size, base + hi * size, size);

    /* Move pivot to hi-1 */
    swap_bytes(base + mid * size, base + (hi - 1) * size, size);
    unsigned char *pivot = base + (hi - 1) * size;

    size_t i = lo, j = hi - 1;
    for (;;) {
        while (cmp(base + ++i * size, pivot) < 0) ;
        while (j > lo && cmp(base + --j * size, pivot) > 0) ;
        if (i >= j) break;
        swap_bytes(base + i * size, base + j * size, size);
    }
    swap_bytes(base + i * size, pivot, size);
    if (i > 0) qsort_r(base, lo, i - 1, size, cmp);
    qsort_r(base, i + 1, hi, size, cmp);
}

void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *))
{
    if (nmemb <= 1) return;
    qsort_r((unsigned char *)base, 0, nmemb - 1, size, compar);
}

/* =========================================================================
 * Environment (stub — AtomOS has no environment variables yet)
 * ========================================================================= */

char *getenv(const char *name)
{
    (void)name;
    return NULL;
}

/* =========================================================================
 * Multibyte character stubs
 * ========================================================================= */

int mblen(const char *s, size_t n)  { (void)s; (void)n; return 0; }
int mbtowc(wchar_t *pwc, const char *s, size_t n)
{
    (void)n;
    if (!s) return 0;
    if (pwc) *pwc = (wchar_t)(unsigned char)*s;
    return *s ? 1 : 0;
}
int wctomb(char *s, wchar_t wc)
{
    if (!s) return 0;
    *s = (char)wc;
    return 1;
}
