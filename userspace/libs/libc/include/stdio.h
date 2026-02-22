#ifndef _STDIO_H
#define _STDIO_H

#include <stddef.h>
#include <stdarg.h>
#include <sys/types.h>

#define EOF         (-1)
#define BUFSIZ      4096
#define FOPEN_MAX   64

/* Seek whence constants (also in unistd.h) */
#define SEEK_SET    0
#define SEEK_CUR    1
#define SEEK_END    2

/* FILE type — opaque to callers */
typedef struct _FILE FILE;

/* Standard streams */
extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

/* Convenience macros */
#define stdin  stdin
#define stdout stdout
#define stderr stderr

/* File opening and closing */
FILE *fopen(const char *path, const char *mode);
FILE *fdopen(int fd, const char *mode);
int   fclose(FILE *stream);
int   fflush(FILE *stream);

/* Binary I/O */
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream);

/* Positioned I/O */
int  fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
void rewind(FILE *stream);

/* File status */
int feof(FILE *stream);
int ferror(FILE *stream);
int fileno(FILE *stream);
void clearerr(FILE *stream);

/* Character I/O */
int   fgetc(FILE *stream);
int   fputc(int c, FILE *stream);
int   getchar(void);
int   putchar(int c);
char *fgets(char *s, int size, FILE *stream);
int   fputs(const char *s, FILE *stream);
int   puts(const char *s);
int   ungetc(int c, FILE *stream);

/* Formatted output */
int fprintf(FILE *stream, const char *format, ...);
int vfprintf(FILE *stream, const char *format, va_list ap);
int printf(const char *format, ...);
int vprintf(const char *format, va_list ap);
int sprintf(char *str, const char *format, ...);
int vsprintf(char *str, const char *format, va_list ap);
int snprintf(char *str, size_t size, const char *format, ...);
int vsnprintf(char *str, size_t size, const char *format, va_list ap);

/* Formatted input */
int sscanf(const char *str, const char *format, ...);
int vsscanf(const char *str, const char *format, va_list ap);
int fscanf(FILE *stream, const char *format, ...);
int scanf(const char *format, ...);

/* Error reporting */
void perror(const char *s);

#endif /* _STDIO_H */
