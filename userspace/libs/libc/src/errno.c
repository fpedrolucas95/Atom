/*
 * errno.c — AtomOS libc errno storage.
 *
 * For now errno is a single global integer.  When AtomOS gains TLS support
 * this can be upgraded to a per-thread variable without changing the public
 * API (errno.h already indirects through __errno_location()).
 */

#include <errno.h>

/* Global errno storage */
static int _errno_val = 0;

int *__errno_location(void)
{
    return &_errno_val;
}
