/*
 * stdio.c — AtomOS libc standard I/O.
 *
 * Implements FILE streams, formatted output (printf family), formatted input
 * (sscanf family), and character I/O.
 *
 * stdout/stderr mapping:
 *   Writing to fd 1 or 2 first tries SYS_FS_WRITE.  If the kernel returns
 *   EBADF (fd not open as a real file), the data falls back to SYS_DEBUG_LOG
 *   so that printf() always produces visible output via the serial port.
 *
 * fopen/fclose/fread/fwrite use the AtomOS filesystem syscalls directly.
 */

#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>
#include <errno.h>
#include <stdarg.h>
#include <fcntl.h>
#include "atom_syscall.h"

/* =========================================================================
 * Internal FILE structure
 * ========================================================================= */

#define FILE_FLAG_READ   (1 << 0)
#define FILE_FLAG_WRITE  (1 << 1)
#define FILE_FLAG_EOF    (1 << 2)
#define FILE_FLAG_ERR    (1 << 3)
#define FILE_FLAG_APPEND (1 << 4)

struct _FILE {
    int      fd;
    int      flags;
    int      unget_char;   /* -1 = none */

    /* Simple write buffer */
    unsigned char wbuf[BUFSIZ];
    int      wbuf_pos;

    /* Simple read buffer */
    unsigned char rbuf[BUFSIZ];
    int      rbuf_pos;
    int      rbuf_len;
};

/* =========================================================================
 * Standard streams
 * ========================================================================= */

static struct _FILE _stdin_obj  = { .fd = 0, .flags = FILE_FLAG_READ,  .unget_char = -1 };
static struct _FILE _stdout_obj = { .fd = 1, .flags = FILE_FLAG_WRITE, .unget_char = -1 };
static struct _FILE _stderr_obj = { .fd = 2, .flags = FILE_FLAG_WRITE, .unget_char = -1 };

FILE *stdin  = &_stdin_obj;
FILE *stdout = &_stdout_obj;
FILE *stderr = &_stderr_obj;

/* =========================================================================
 * Low-level write helpers
 * ========================================================================= */

/*
 * Write to the kernel debug log (serial port).  Used as fallback for
 * fd 1/2 when the real FS fd is not set up.
 */
static void debug_write(const char *buf, size_t len)
{
    atom_syscall2(SYS_DEBUG_LOG, (uint64_t)(uintptr_t)buf, (uint64_t)len);
}

/*
 * Flush the write buffer of `stream` to the kernel.
 * Returns 0 on success, EOF on error.
 */
static int flush_wbuf(FILE *stream)
{
    if (stream->wbuf_pos == 0) return 0;

    const unsigned char *buf = stream->wbuf;
    int  rem  = stream->wbuf_pos;
    int  done = 0;

    while (rem > 0) {
        uint64_t ret = atom_syscall3(SYS_FS_WRITE,
                                     (uint64_t)stream->fd,
                                     (uint64_t)(uintptr_t)(buf + done),
                                     (uint64_t)(unsigned)rem);
        if (atom_is_error(ret)) {
            /* For stdout/stderr fall back to debug log */
            if (stream->fd == 1 || stream->fd == 2) {
                debug_write((const char *)(buf + done), (size_t)rem);
                done += rem;
                rem   = 0;
            } else {
                stream->flags |= FILE_FLAG_ERR;
                atom_set_errno(ret);
                stream->wbuf_pos = 0;
                return EOF;
            }
        } else {
            int n = (int)(uint32_t)ret;
            done += n;
            rem  -= n;
        }
    }

    stream->wbuf_pos = 0;
    return 0;
}

/* =========================================================================
 * fflush
 * ========================================================================= */

int fflush(FILE *stream)
{
    if (!stream) {
        /* Flush all streams — for now just stdout */
        return flush_wbuf(stdout);
    }
    if (!(stream->flags & FILE_FLAG_WRITE)) return 0;
    return flush_wbuf(stream);
}

/* =========================================================================
 * fopen / fdopen / fclose
 * ========================================================================= */

FILE *fopen(const char *path, const char *mode)
{
    if (!path || !mode) { errno = EINVAL; return NULL; }

    int flags = 0;
    int atom_flags = 0;

    /* Parse mode string */
    if (mode[0] == 'r') {
        flags = FILE_FLAG_READ;
        atom_flags = O_RDONLY;
        if (mode[1] == '+') { flags |= FILE_FLAG_WRITE; atom_flags = O_RDWR; }
    } else if (mode[0] == 'w') {
        flags = FILE_FLAG_WRITE;
        atom_flags = O_WRONLY | O_CREAT | O_TRUNC;
        if (mode[1] == '+') { flags |= FILE_FLAG_READ; atom_flags = O_RDWR | O_CREAT | O_TRUNC; }
    } else if (mode[0] == 'a') {
        flags = FILE_FLAG_WRITE | FILE_FLAG_APPEND;
        atom_flags = O_WRONLY | O_CREAT | O_APPEND;
        if (mode[1] == '+') { flags |= FILE_FLAG_READ; atom_flags = O_RDWR | O_CREAT | O_APPEND; }
    } else {
        errno = EINVAL;
        return NULL;
    }

    const unsigned char *pb = (const unsigned char *)path;
    uint64_t ret = atom_syscall4(SYS_FS_OPEN,
                                 (uint64_t)(uintptr_t)pb,
                                 (uint64_t)strlen(path),
                                 (uint64_t)(unsigned)atom_flags,
                                 0644ULL);
    if (atom_is_error(ret)) { atom_set_errno(ret); return NULL; }

    FILE *f = (FILE *)malloc(sizeof(struct _FILE));
    if (!f) { atom_syscall1(SYS_FS_CLOSE, ret); errno = ENOMEM; return NULL; }

    f->fd         = (int)(uint32_t)ret;
    f->flags      = flags;
    f->unget_char = -1;
    f->wbuf_pos   = 0;
    f->rbuf_pos   = 0;
    f->rbuf_len   = 0;
    return f;
}

/* Open a FILE wrapper around an existing file descriptor */
FILE *fdopen(int fd, const char *mode)
{
    if (!mode) { errno = EINVAL; return NULL; }

    int flags = 0;
    if (mode[0] == 'r')      flags = FILE_FLAG_READ;
    else if (mode[0] == 'w') flags = FILE_FLAG_WRITE;
    else if (mode[0] == 'a') flags = FILE_FLAG_WRITE | FILE_FLAG_APPEND;
    if (mode[1] == '+')      flags |= FILE_FLAG_READ | FILE_FLAG_WRITE;

    FILE *f = (FILE *)malloc(sizeof(struct _FILE));
    if (!f) { errno = ENOMEM; return NULL; }

    f->fd         = fd;
    f->flags      = flags;
    f->unget_char = -1;
    f->wbuf_pos   = 0;
    f->rbuf_pos   = 0;
    f->rbuf_len   = 0;
    return f;
}

int fclose(FILE *stream)
{
    if (!stream) { errno = EINVAL; return EOF; }
    if (stream == stdin || stream == stdout || stream == stderr) {
        fflush(stream);
        return 0;
    }
    fflush(stream);
    uint64_t ret = atom_syscall1(SYS_FS_CLOSE, (uint64_t)stream->fd);
    free(stream);
    if (atom_is_error(ret)) { atom_set_errno(ret); return EOF; }
    return 0;
}

/* =========================================================================
 * fread / fwrite
 * ========================================================================= */

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    if (!ptr || !stream || size == 0 || nmemb == 0) return 0;
    if (stream->flags & FILE_FLAG_EOF) return 0;

    size_t total = size * nmemb;
    unsigned char *dst = (unsigned char *)ptr;
    size_t done = 0;

    /* Drain unget buffer first */
    if (stream->unget_char >= 0 && done < total) {
        dst[done++] = (unsigned char)stream->unget_char;
        stream->unget_char = -1;
    }

    /* Drain read buffer */
    while (stream->rbuf_pos < stream->rbuf_len && done < total) {
        dst[done++] = stream->rbuf[stream->rbuf_pos++];
    }

    /* Remaining bytes from kernel */
    while (done < total) {
        uint64_t ret = atom_syscall3(SYS_FS_READ,
                                     (uint64_t)stream->fd,
                                     (uint64_t)(uintptr_t)(dst + done),
                                     (uint64_t)(total - done));
        if (atom_is_error(ret)) { stream->flags |= FILE_FLAG_ERR; break; }
        if (ret == 0)           { stream->flags |= FILE_FLAG_EOF; break; }
        done += (size_t)ret;
    }

    return done / size;
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream)
{
    if (!ptr || !stream || size == 0 || nmemb == 0) return 0;
    if (!(stream->flags & FILE_FLAG_WRITE)) { errno = EBADF; return 0; }

    const unsigned char *src   = (const unsigned char *)ptr;
    size_t               total = size * nmemb;
    size_t               done  = 0;

    /* Buffer into wbuf, flushing when full */
    while (done < total) {
        int space = BUFSIZ - stream->wbuf_pos;
        size_t chunk = total - done;
        if ((int)chunk > space) chunk = (size_t)space;

        memcpy(stream->wbuf + stream->wbuf_pos, src + done, chunk);
        stream->wbuf_pos += (int)chunk;
        done             += chunk;

        if (stream->wbuf_pos == BUFSIZ) {
            if (flush_wbuf(stream) == EOF) return done / size;
        }
    }

    /* Auto-flush on newline for stdout/stderr (line buffering) */
    if (stream->fd == 1 || stream->fd == 2) {
        flush_wbuf(stream);
    }

    return nmemb;
}

/* =========================================================================
 * fseek / ftell / rewind
 * ========================================================================= */

int fseek(FILE *stream, long offset, int whence)
{
    if (!stream) { errno = EINVAL; return -1; }
    fflush(stream);
    stream->flags &= ~FILE_FLAG_EOF;
    stream->rbuf_pos = stream->rbuf_len = 0;

    uint64_t ret = atom_syscall3(SYS_FS_SEEK,
                                 (uint64_t)stream->fd,
                                 (uint64_t)(long long)offset,
                                 (uint64_t)(unsigned)whence);
    if (atom_is_error(ret)) { atom_set_errno(ret); return -1; }
    return 0;
}

long ftell(FILE *stream)
{
    if (!stream) { errno = EINVAL; return -1L; }
    uint64_t ret = atom_syscall3(SYS_FS_SEEK,
                                 (uint64_t)stream->fd,
                                 0,
                                 1 /* SEEK_CUR */);
    if (atom_is_error(ret)) { atom_set_errno(ret); return -1L; }
    return (long)ret;
}

void rewind(FILE *stream)
{
    if (stream) { clearerr(stream); fseek(stream, 0, 0); }
}

/* =========================================================================
 * Status queries
 * ========================================================================= */

int feof(FILE *stream)   { return stream && (stream->flags & FILE_FLAG_EOF); }
int ferror(FILE *stream) { return stream && (stream->flags & FILE_FLAG_ERR); }
void clearerr(FILE *stream)
{
    if (stream) stream->flags &= ~(FILE_FLAG_EOF | FILE_FLAG_ERR);
}
int fileno(FILE *stream) { return stream ? stream->fd : -1; }

/* =========================================================================
 * Character I/O
 * ========================================================================= */

int fgetc(FILE *stream)
{
    if (!stream) return EOF;
    if (stream->flags & FILE_FLAG_EOF) return EOF;

    if (stream->unget_char >= 0) {
        int c = stream->unget_char;
        stream->unget_char = -1;
        return c;
    }

    if (stream->rbuf_pos < stream->rbuf_len)
        return (unsigned char)stream->rbuf[stream->rbuf_pos++];

    /* Refill */
    uint64_t ret = atom_syscall3(SYS_FS_READ,
                                 (uint64_t)stream->fd,
                                 (uint64_t)(uintptr_t)stream->rbuf,
                                 (uint64_t)BUFSIZ);
    if (atom_is_error(ret)) { stream->flags |= FILE_FLAG_ERR; return EOF; }
    if (ret == 0)           { stream->flags |= FILE_FLAG_EOF; return EOF; }

    stream->rbuf_pos = 1;
    stream->rbuf_len = (int)ret;
    return (unsigned char)stream->rbuf[0];
}

int fputc(int c, FILE *stream)
{
    if (!stream) return EOF;
    if (!(stream->flags & FILE_FLAG_WRITE)) { errno = EBADF; return EOF; }

    unsigned char b = (unsigned char)c;
    stream->wbuf[stream->wbuf_pos++] = b;
    if (stream->wbuf_pos == BUFSIZ || b == '\n') {
        if (flush_wbuf(stream) == EOF) return EOF;
    }
    return (unsigned char)c;
}

int getchar(void) { return fgetc(stdin); }
int putchar(int c) { return fputc(c, stdout); }

char *fgets(char *s, int size, FILE *stream)
{
    if (!s || size <= 0 || !stream) return NULL;
    int i = 0;
    while (i < size - 1) {
        int c = fgetc(stream);
        if (c == EOF) { if (i == 0) return NULL; break; }
        s[i++] = (char)c;
        if (c == '\n') break;
    }
    s[i] = '\0';
    return s;
}

int fputs(const char *s, FILE *stream)
{
    if (!s || !stream) return EOF;
    size_t len = strlen(s);
    return (int)fwrite(s, 1, len, stream) == (int)len ? (int)len : EOF;
}

int puts(const char *s)
{
    if (fputs(s, stdout) == EOF) return EOF;
    return fputc('\n', stdout) == EOF ? EOF : 0;
}

int ungetc(int c, FILE *stream)
{
    if (!stream || c == EOF) return EOF;
    stream->unget_char = (unsigned char)c;
    stream->flags &= ~FILE_FLAG_EOF;
    return (unsigned char)c;
}

/* =========================================================================
 * vsnprintf — the core formatting engine
 * ========================================================================= */

/* Append a single character to the output buffer */
#define OUT(c) do { \
    if (out_pos < (int)buf_size - 1) buf[out_pos] = (char)(c); \
    out_pos++; \
} while (0)

/* Write a string of known length */
static int fmt_str(char *buf, int buf_size, int pos, const char *s, int len,
                   int width, int left_align, char pad)
{
    int padding = width > len ? width - len : 0;
    if (!left_align) {
        for (int i = 0; i < padding; i++) {
            if (pos < buf_size - 1) buf[pos] = pad;
            pos++;
        }
    }
    for (int i = 0; i < len; i++) {
        if (pos < buf_size - 1) buf[pos] = s[i];
        pos++;
    }
    if (left_align) {
        for (int i = 0; i < padding; i++) {
            if (pos < buf_size - 1) buf[pos] = ' ';
            pos++;
        }
    }
    return pos;
}

int vsnprintf(char *buf, size_t buf_size, const char *fmt, va_list ap)
{
    if (!buf) buf_size = 0;

    int out_pos = 0;

    for (const char *p = fmt; *p; p++) {
        if (*p != '%') { OUT(*p); continue; }

        p++;  /* skip '%' */

        /* Flags */
        int  left_align = 0, force_sign = 0, alt = 0;
        char pad = ' ';
        for (;;) {
            if      (*p == '-') { left_align = 1; p++; }
            else if (*p == '+') { force_sign = 1; p++; }
            else if (*p == '0') { pad = '0';       p++; }
            else if (*p == '#') { alt = 1;          p++; }
            else if (*p == ' ') { p++; }
            else break;
        }
        (void)alt;  /* used for 0x prefix below */

        /* Width */
        int width = 0;
        if (*p == '*') { width = va_arg(ap, int); p++; }
        else { while (*p >= '0' && *p <= '9') width = width * 10 + (*p++ - '0'); }

        /* Precision */
        int precision = -1;
        if (*p == '.') {
            p++;
            precision = 0;
            if (*p == '*') { precision = va_arg(ap, int); p++; }
            else { while (*p >= '0' && *p <= '9') precision = precision * 10 + (*p++ - '0'); }
        }

        /* Length modifier */
        int long_flag = 0, long_long_flag = 0;
        if (*p == 'l') {
            p++;
            if (*p == 'l') { long_long_flag = 1; p++; }
            else long_flag = 1;
        } else if (*p == 'h') {
            p++;
            if (*p == 'h') p++; /* hh ignored — promote to int anyway */
        } else if (*p == 'z') {
            long_flag = 1; p++;
        }

        char tmp[64];
        int  tmp_len;

        switch (*p) {

        /* ---- Character ---- */
        case 'c': {
            char c = (char)va_arg(ap, int);
            tmp[0] = c; tmp_len = 1;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, 1, width, left_align, ' ');
            break;
        }

        /* ---- String ---- */
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            int slen = (int)strlen(s);
            if (precision >= 0 && slen > precision) slen = precision;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, s, slen, width, left_align, ' ');
            break;
        }

        /* ---- Signed decimal ---- */
        case 'd': case 'i': {
            long long val;
            if (long_long_flag)  val = va_arg(ap, long long);
            else if (long_flag)  val = va_arg(ap, long);
            else                 val = (long long)(int)va_arg(ap, int);

            int negative = val < 0;
            unsigned long long uval = negative ? (unsigned long long)-val
                                               : (unsigned long long)val;

            /* Build digits in reverse */
            int start = 0;
            if (uval == 0) { tmp[start++] = '0'; }
            else { while (uval) { tmp[start++] = '0' + (int)(uval % 10); uval /= 10; } }

            /* Sign */
            if (negative)          tmp[start++] = '-';
            else if (force_sign)   tmp[start++] = '+';

            /* Reverse */
            for (int i = 0, j = start - 1; i < j; i++, j--) {
                char t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t;
            }
            tmp_len = start;

            /* Zero-pad to precision */
            if (precision > tmp_len) {
                /* Shift right and insert zeros after sign */
                int extra = precision - tmp_len;
                int sign_off = (tmp[0] == '-' || tmp[0] == '+') ? 1 : 0;
                memmove(tmp + sign_off + extra, tmp + sign_off, (size_t)(tmp_len - sign_off));
                memset(tmp + sign_off, '0', (size_t)extra);
                tmp_len += extra;
            }

            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, tmp_len, width,
                              left_align, pad);
            break;
        }

        /* ---- Unsigned decimal ---- */
        case 'u': {
            unsigned long long uval;
            if (long_long_flag)  uval = va_arg(ap, unsigned long long);
            else if (long_flag)  uval = va_arg(ap, unsigned long);
            else                 uval = (unsigned long long)(unsigned)va_arg(ap, unsigned);

            int start = 0;
            if (uval == 0) { tmp[start++] = '0'; }
            else { while (uval) { tmp[start++] = '0' + (int)(uval % 10); uval /= 10; } }
            for (int i = 0, j = start - 1; i < j; i++, j--) {
                char t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t;
            }
            tmp_len = start;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, tmp_len, width,
                              left_align, pad);
            break;
        }

        /* ---- Hex (lower/upper) ---- */
        case 'x': case 'X': {
            const char *hex = (*p == 'x') ? "0123456789abcdef"
                                          : "0123456789ABCDEF";
            unsigned long long uval;
            if (long_long_flag)  uval = va_arg(ap, unsigned long long);
            else if (long_flag)  uval = va_arg(ap, unsigned long);
            else                 uval = (unsigned long long)(unsigned)va_arg(ap, unsigned);

            int start = 0;
            if (uval == 0) { tmp[start++] = '0'; }
            else { while (uval) { tmp[start++] = hex[uval & 0xF]; uval >>= 4; } }

            /* Alternate form: 0x / 0X prefix */
            if (alt && !(start == 1 && tmp[0] == '0')) {
                tmp[start++] = (*p == 'x') ? 'x' : 'X';
                tmp[start++] = '0';
            }

            for (int i = 0, j = start - 1; i < j; i++, j--) {
                char t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t;
            }
            tmp_len = start;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, tmp_len, width,
                              left_align, pad);
            break;
        }

        /* ---- Octal ---- */
        case 'o': {
            unsigned long long uval;
            if (long_long_flag)  uval = va_arg(ap, unsigned long long);
            else if (long_flag)  uval = va_arg(ap, unsigned long);
            else                 uval = (unsigned long long)(unsigned)va_arg(ap, unsigned);

            int start = 0;
            if (uval == 0) { tmp[start++] = '0'; }
            else { while (uval) { tmp[start++] = '0' + (int)(uval & 7); uval >>= 3; } }
            for (int i = 0, j = start - 1; i < j; i++, j--) {
                char t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t;
            }
            tmp_len = start;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, tmp_len, width,
                              left_align, pad);
            break;
        }

        /* ---- Pointer ---- */
        case 'p': {
            uintptr_t uval = (uintptr_t)va_arg(ap, void *);
            const char *hex = "0123456789abcdef";
            int start = 0;
            if (uval == 0) { tmp[start++] = '0'; }
            else { while (uval) { tmp[start++] = hex[uval & 0xF]; uval >>= 4; } }
            tmp[start++] = 'x'; tmp[start++] = '0';
            for (int i = 0, j = start - 1; i < j; i++, j--) {
                char t = tmp[i]; tmp[i] = tmp[j]; tmp[j] = t;
            }
            tmp_len = start;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, tmp, tmp_len, width,
                              left_align, ' ');
            break;
        }

        /* ---- Floating point (basic) ---- */
        case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
            double val = va_arg(ap, double);
            int    prec = (precision >= 0) ? precision : 6;
            char   ftmp[64];

            /* Simple fixed-point renderer for reasonable values */
            int   neg  = (val < 0.0);
            if (neg) val = -val;

            long long ipart = (long long)val;
            double    fpart = val - (double)ipart;

            /* Build fractional part */
            int fidx = 0;
            char fbuf[32];
            if (prec > 31) prec = 31; /* clamp to buffer size */
            for (int i = 0; i < prec; i++) {
                fpart *= 10.0;
                fbuf[fidx++] = '0' + (int)fpart % 10;
            }

            /* Build integer part */
            int iidx = 0;
            char ibuf[32];
            if (ipart == 0) { ibuf[iidx++] = '0'; }
            else {
                long long ip2 = ipart;
                while (ip2) { ibuf[iidx++] = '0' + (int)(ip2 % 10); ip2 /= 10; }
                for (int i = 0, j = iidx - 1; i < j; i++, j--) {
                    char t = ibuf[i]; ibuf[i] = ibuf[j]; ibuf[j] = t;
                }
            }

            int fi = 0;
            if (neg)           ftmp[fi++] = '-';
            else if (force_sign) ftmp[fi++] = '+';
            memcpy(ftmp + fi, ibuf, (size_t)iidx); fi += iidx;
            if (prec > 0) {
                ftmp[fi++] = '.';
                memcpy(ftmp + fi, fbuf, (size_t)prec); fi += prec;
            }
            ftmp[fi] = '\0';
            tmp_len  = fi;
            out_pos = fmt_str(buf, (int)buf_size, out_pos, ftmp, tmp_len, width,
                              left_align, pad);
            break;
        }

        /* ---- Count of chars written so far ---- */
        case 'n': {
            int *n = va_arg(ap, int *);
            if (n) *n = out_pos;
            break;
        }

        /* ---- Literal percent ---- */
        case '%':
            OUT('%');
            break;

        default:
            OUT('%'); OUT(*p);
            break;
        }
    }

    /* NUL-terminate */
    if ((int)buf_size > 0) {
        if (out_pos < (int)buf_size) buf[out_pos] = '\0';
        else buf[buf_size - 1] = '\0';
    }
    return out_pos;
}

/* =========================================================================
 * Remaining printf family
 * ========================================================================= */

int vsprintf(char *str, const char *fmt, va_list ap)
{
    return vsnprintf(str, (size_t)-1, fmt, ap);
}

int sprintf(char *str, const char *fmt, ...)
{
    va_list ap; va_start(ap, fmt);
    int r = vsprintf(str, fmt, ap);
    va_end(ap);
    return r;
}

int snprintf(char *str, size_t size, const char *fmt, ...)
{
    va_list ap; va_start(ap, fmt);
    int r = vsnprintf(str, size, fmt, ap);
    va_end(ap);
    return r;
}

int vfprintf(FILE *stream, const char *fmt, va_list ap)
{
    /* Format into a temporary heap buffer, then fwrite */
    char   tmp[1024];
    char  *dyn  = NULL;
    char  *buf  = tmp;
    int    size = (int)sizeof(tmp);

    va_list ap2;
    va_copy(ap2, ap);
    int need = vsnprintf(buf, (size_t)size, fmt, ap2);
    va_end(ap2);

    if (need >= size) {
        dyn = (char *)malloc((size_t)(need + 1));
        if (dyn) {
            buf  = dyn;
            size = need + 1;
            vsnprintf(buf, (size_t)size, fmt, ap);
        }
    }

    int written = (int)fwrite(buf, 1, (size_t)need, stream);
    if (dyn) free(dyn);
    return written;
}

int fprintf(FILE *stream, const char *fmt, ...)
{
    va_list ap; va_start(ap, fmt);
    int r = vfprintf(stream, fmt, ap);
    va_end(ap);
    return r;
}

int vprintf(const char *fmt, va_list ap)
{
    return vfprintf(stdout, fmt, ap);
}

int printf(const char *fmt, ...)
{
    va_list ap; va_start(ap, fmt);
    int r = vprintf(fmt, ap);
    va_end(ap);
    return r;
}

/* =========================================================================
 * perror
 * ========================================================================= */

void perror(const char *s)
{
    if (s && *s) {
        fputs(s, stderr);
        fputs(": ", stderr);
    }
    fputs(strerror(errno), stderr);
    fputc('\n', stderr);
}

/* =========================================================================
 * sscanf / vsscanf — basic implementation
 * ========================================================================= */

static int vsscanf_impl(const char *str, const char *fmt, va_list ap)
{
    int assigned = 0;
    const char *s = str;

    for (const char *f = fmt; *f && *s; f++) {
        if (*f == '%') {
            f++;
            int suppress = (*f == '*');
            if (suppress) f++;

            /* Width field (ignored for simplicity) */
            while (*f >= '0' && *f <= '9') f++;

            /* Length */
            int lng = 0;
            if (*f == 'l') { lng = 1; f++; }

            switch (*f) {
            case 'd': case 'i': {
                while (*s == ' ' || *s == '\t' || *s == '\n') s++;
                long long val = 0; int neg = 0;
                if (*s == '-') { neg = 1; s++; }
                else if (*s == '+') s++;
                while (*s >= '0' && *s <= '9') val = val * 10 + (*s++ - '0');
                if (!suppress) {
                    if (lng) *va_arg(ap, long *) = (long)(neg ? -val : val);
                    else     *va_arg(ap, int *)  = (int)(neg ? -val : val);
                    assigned++;
                }
                break;
            }
            case 'u': {
                while (*s == ' ' || *s == '\t' || *s == '\n') s++;
                unsigned long long uval = 0;
                while (*s >= '0' && *s <= '9') uval = uval * 10 + (unsigned)(*s++ - '0');
                if (!suppress) {
                    if (lng) *va_arg(ap, unsigned long *) = (unsigned long)uval;
                    else     *va_arg(ap, unsigned *)      = (unsigned)uval;
                    assigned++;
                }
                break;
            }
            case 'x': case 'X': {
                while (*s == ' ' || *s == '\t') s++;
                unsigned long long uval = 0;
                const char *digits = "0123456789abcdef";
                while (*s) {
                    const char *d = strchr(digits, (*s >= 'A' && *s <= 'F')
                                                   ? (*s + 32) : *s);
                    if (!d) break;
                    uval = uval * 16 + (unsigned long long)(d - digits);
                    s++;
                }
                if (!suppress) {
                    *va_arg(ap, unsigned *) = (unsigned)uval;
                    assigned++;
                }
                break;
            }
            case 's': {
                while (*s == ' ' || *s == '\t' || *s == '\n') s++;
                if (!suppress) {
                    char *dst = va_arg(ap, char *);
                    while (*s && *s != ' ' && *s != '\t' && *s != '\n')
                        *dst++ = *s++;
                    *dst = '\0';
                    assigned++;
                } else {
                    while (*s && *s != ' ' && *s != '\t' && *s != '\n') s++;
                }
                break;
            }
            case 'c': {
                if (!suppress) {
                    *va_arg(ap, char *) = *s;
                    assigned++;
                }
                s++;
                break;
            }
            case 'n': {
                if (!suppress) *va_arg(ap, int *) = (int)(s - str);
                break;
            }
            case '%':
                if (*s == '%') s++; else return assigned;
                break;
            default:
                return assigned;
            }
        } else if (*f == ' ') {
            /* Match zero or more whitespace */
            while (*s == ' ' || *s == '\t' || *s == '\n' || *s == '\r') s++;
        } else {
            /* Literal match */
            if (*s != *f) return assigned;
            s++;
        }
    }
    return assigned;
}

int vsscanf(const char *str, const char *fmt, va_list ap)
{
    return vsscanf_impl(str, fmt, ap);
}

int sscanf(const char *str, const char *fmt, ...)
{
    va_list ap; va_start(ap, fmt);
    int r = vsscanf(str, fmt, ap);
    va_end(ap);
    return r;
}

int fscanf(FILE *stream, const char *fmt, ...)
{
    /* Simple line-based: read a line then sscanf it */
    char buf[1024];
    if (!fgets(buf, (int)sizeof(buf), stream)) return EOF;
    va_list ap; va_start(ap, fmt);
    int r = vsscanf(buf, fmt, ap);
    va_end(ap);
    return r;
}

int scanf(const char *fmt, ...)
{
    char buf[1024];
    if (!fgets(buf, (int)sizeof(buf), stdin)) return EOF;
    va_list ap; va_start(ap, fmt);
    int r = vsscanf(buf, fmt, ap);
    va_end(ap);
    return r;
}

/* =========================================================================
 * __libc_init — called from crt0.S before main()
 * ========================================================================= */

void __libc_init(void)
{
    /* Reset standard streams */
    _stdin_obj.fd  = 0; _stdin_obj.flags  = FILE_FLAG_READ;  _stdin_obj.unget_char  = -1;
    _stdout_obj.fd = 1; _stdout_obj.flags = FILE_FLAG_WRITE; _stdout_obj.unget_char = -1;
    _stderr_obj.fd = 2; _stderr_obj.flags = FILE_FLAG_WRITE; _stderr_obj.unget_char = -1;

    stdin  = &_stdin_obj;
    stdout = &_stdout_obj;
    stderr = &_stderr_obj;
}

/* Required by string.c include at the bottom — nothing to add here */
