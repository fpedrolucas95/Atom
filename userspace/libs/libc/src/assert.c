/*
 * assert.c — AtomOS libc assertion handler.
 *
 * When an assertion fires, the failure message is printed to stderr and the
 * process terminates via _exit(134) (SIGABRT conventional exit code).
 */

#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

void __assert_fail(const char *expr, const char *file, unsigned int line,
                   const char *func)
{
    fprintf(stderr, "%s:%u: %s: Assertion `%s' failed.\n",
            file, line, func ? func : "?", expr);
    _exit(134);
}
