#ifndef _STDDEF_H
#define _STDDEF_H

/* Null pointer constant */
#ifndef NULL
#define NULL ((void *)0)
#endif

/* Unsigned size type */
typedef unsigned long size_t;

/* Signed difference type */
typedef long ptrdiff_t;

/* Wide character type */
typedef unsigned int wchar_t;

/* max_align_t — alignment of any scalar type */
typedef long double max_align_t;

/* Offset of a member within a struct */
#ifndef offsetof
#define offsetof(type, member) __builtin_offsetof(type, member)
#endif

#endif /* _STDDEF_H */
