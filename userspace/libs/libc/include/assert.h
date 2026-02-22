#ifndef _ASSERT_H
#define _ASSERT_H

void __assert_fail(const char *expr, const char *file, unsigned int line,
                   const char *func);

#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
#define assert(expr) \
    ((expr) ? (void)0 \
            : __assert_fail(#expr, __FILE__, __LINE__, __func__))
#endif

#define static_assert _Static_assert

#endif /* _ASSERT_H */
