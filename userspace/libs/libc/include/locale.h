#ifndef _ATOMOS_LOCALE_H
#define _ATOMOS_LOCALE_H

struct lconv {
    char *decimal_point;
};

static inline struct lconv *localeconv(void) {
    static struct lconv lc = { "." };
    return &lc;
}

#endif /* _ATOMOS_LOCALE_H */

