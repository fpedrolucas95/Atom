#ifndef _STDINT_H
#define _STDINT_H

/* Exact-width integer types */
typedef signed char        int8_t;
typedef short              int16_t;
typedef int                int32_t;
typedef long long          int64_t;

typedef unsigned char      uint8_t;
typedef unsigned short     uint16_t;
typedef unsigned int       uint32_t;
typedef unsigned long long uint64_t;

/* Pointer-sized integer types (64-bit on AtomOS x86_64) */
typedef long               intptr_t;
typedef unsigned long      uintptr_t;

/* Maximum-width integer types */
typedef long long          intmax_t;
typedef unsigned long long uintmax_t;

/* Signed limits */
#define INT8_MIN   (-128)
#define INT8_MAX   (127)
#define INT16_MIN  (-32768)
#define INT16_MAX  (32767)
#define INT32_MIN  (-2147483648)
#define INT32_MAX  (2147483647)
#define INT64_MIN  (-9223372036854775807LL - 1)
#define INT64_MAX  (9223372036854775807LL)

/* Unsigned limits */
#define UINT8_MAX   (255U)
#define UINT16_MAX  (65535U)
#define UINT32_MAX  (4294967295U)
#define UINT64_MAX  (18446744073709551615ULL)

/* Pointer-sized limits */
#define INTPTR_MIN  INT64_MIN
#define INTPTR_MAX  INT64_MAX
#define UINTPTR_MAX UINT64_MAX

/* size_t / ptrdiff_t limits */
#define SIZE_MAX    UINT64_MAX
#define PTRDIFF_MIN INT64_MIN
#define PTRDIFF_MAX INT64_MAX

/* Intmax limits */
#define INTMAX_MIN  INT64_MIN
#define INTMAX_MAX  INT64_MAX
#define UINTMAX_MAX UINT64_MAX

/* Integer constant macros */
#define INT8_C(c)    ((int8_t)(c))
#define INT16_C(c)   ((int16_t)(c))
#define INT32_C(c)   (c)
#define INT64_C(c)   (c ## LL)

#define UINT8_C(c)   ((uint8_t)(c))
#define UINT16_C(c)  ((uint16_t)(c))
#define UINT32_C(c)  (c ## U)
#define UINT64_C(c)  (c ## ULL)

#define INTMAX_C(c)  (c ## LL)
#define UINTMAX_C(c) (c ## ULL)

#endif /* _STDINT_H */
