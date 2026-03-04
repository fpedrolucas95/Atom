/*
 * inttypes.h — C99 integer format specifier macros
 *
 * AtomOS libc shim: provides PRI-prefix and SCN-prefix macros for printf/scanf.
 * Assumes 64-bit LP64 ABI (x86-64).
 */
#ifndef _INTTYPES_H
#define _INTTYPES_H

#include <stdint.h>

/* Decimal */
#define PRId8   "d"
#define PRId16  "d"
#define PRId32  "d"
#define PRId64  "ld"

#define PRIi8   "i"
#define PRIi16  "i"
#define PRIi32  "i"
#define PRIi64  "li"

/* Unsigned decimal */
#define PRIu8   "u"
#define PRIu16  "u"
#define PRIu32  "u"
#define PRIu64  "lu"

/* Octal */
#define PRIo8   "o"
#define PRIo16  "o"
#define PRIo32  "o"
#define PRIo64  "lo"

/* Hex (lower) */
#define PRIx8   "x"
#define PRIx16  "x"
#define PRIx32  "x"
#define PRIx64  "lx"

/* Hex (upper) */
#define PRIX8   "X"
#define PRIX16  "X"
#define PRIX32  "X"
#define PRIX64  "lX"

/* Least-width types */
#define PRIdLEAST8   PRId8
#define PRIdLEAST16  PRId16
#define PRIdLEAST32  PRId32
#define PRIdLEAST64  PRId64

#define PRIuLEAST8   PRIu8
#define PRIuLEAST16  PRIu16
#define PRIuLEAST32  PRIu32
#define PRIuLEAST64  PRIu64

#define PRIxLEAST8   PRIx8
#define PRIxLEAST16  PRIx16
#define PRIxLEAST32  PRIx32
#define PRIxLEAST64  PRIx64

/* Fast types */
#define PRIdFAST8    PRId8
#define PRIdFAST16   PRId16
#define PRIdFAST32   PRId32
#define PRIdFAST64   PRId64

#define PRIuFAST8    PRIu8
#define PRIuFAST16   PRIu16
#define PRIuFAST32   PRIu32
#define PRIuFAST64   PRIu64

/* Pointer */
#define PRIuPTR  "lu"
#define PRIxPTR  "lx"
#define PRIdPTR  "ld"

/* Scanf variants (mirrors of PRI*) */
#define SCNd8    "hhd"
#define SCNd16   "hd"
#define SCNd32   "d"
#define SCNd64   "ld"

#define SCNu8    "hhu"
#define SCNu16   "hu"
#define SCNu32   "u"
#define SCNu64   "lu"

#define SCNx8    "hhx"
#define SCNx16   "hx"
#define SCNx32   "x"
#define SCNx64   "lx"

#endif /* _INTTYPES_H */
