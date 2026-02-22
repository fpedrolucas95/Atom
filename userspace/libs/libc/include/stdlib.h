#ifndef _STDLIB_H
#define _STDLIB_H

#include <stddef.h>

#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1

#define RAND_MAX     0x7fffffff

/* Memory allocation */
void *malloc(size_t size);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void  free(void *ptr);
void *aligned_alloc(size_t alignment, size_t size);

/* Process control */
void exit(int status);
void _exit(int status);
void abort(void);
int  atexit(void (*func)(void));

/* Numeric conversion */
int          atoi(const char *nptr);
long         atol(const char *nptr);
long long    atoll(const char *nptr);
double       atof(const char *nptr);

long               strtol(const char *nptr, char **endptr, int base);
unsigned long      strtoul(const char *nptr, char **endptr, int base);
long long          strtoll(const char *nptr, char **endptr, int base);
unsigned long long strtoull(const char *nptr, char **endptr, int base);
double             strtod(const char *nptr, char **endptr);

/* Integer absolute value */
int       abs(int j);
long      labs(long j);
long long llabs(long long j);

/* Integer division */
typedef struct { int quot; int rem; }             div_t;
typedef struct { long quot; long rem; }           ldiv_t;
typedef struct { long long quot; long long rem; } lldiv_t;

div_t   div(int numer, int denom);
ldiv_t  ldiv(long numer, long denom);
lldiv_t lldiv(long long numer, long long denom);

/* Random numbers */
int  rand(void);
void srand(unsigned int seed);

/* Searching and sorting */
void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*compar)(const void *, const void *));
void  qsort(void *base, size_t nmemb, size_t size,
            int (*compar)(const void *, const void *));

/* Environment */
char *getenv(const char *name);

/* Multibyte characters (stub) */
int  mblen(const char *s, size_t n);
int  mbtowc(wchar_t *pwc, const char *s, size_t n);
int  wctomb(char *s, wchar_t wc);

#endif /* _STDLIB_H */
