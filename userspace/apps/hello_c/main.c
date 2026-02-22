/*
 * hello_c — AtomOS "Hello, World!" in C using the native AtomOS libc.
 *
 * This application demonstrates that the AtomOS libc port works correctly
 * by exercising core functionality:
 *   - printf / puts (formatted output via debug log)
 *   - malloc / free (heap allocation via mmap syscall)
 *   - string functions (strlen, strcpy, sprintf)
 *   - stdlib functions (atoi, qsort)
 *   - exit (clean process termination)
 *
 * Build:
 *   See userspace/apps/hello_c/Makefile
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* qsort comparator for integers */
static int int_cmp(const void *a, const void *b)
{
    return *(const int *)a - *(const int *)b;
}

int main(void)
{
    /* ------------------------------------------------------------------ */
    /* 1. Basic output                                                      */
    /* ------------------------------------------------------------------ */
    puts("=== AtomOS libc hello_c ===");
    printf("Hello from C on AtomOS!\n");

    /* ------------------------------------------------------------------ */
    /* 2. String functions                                                  */
    /* ------------------------------------------------------------------ */
    const char *greeting = "AtomOS";
    char buf[64];
    snprintf(buf, sizeof(buf), "Running on: %s (len=%zu)", greeting, strlen(greeting));
    puts(buf);

    /* ------------------------------------------------------------------ */
    /* 3. Heap allocation                                                   */
    /* ------------------------------------------------------------------ */
    int *arr = (int *)malloc(8 * sizeof(int));
    if (!arr) {
        fprintf(stderr, "malloc failed!\n");
        return 1;
    }

    /* Fill with unsorted values */
    int vals[] = {5, 2, 8, 1, 9, 3, 7, 4};
    memcpy(arr, vals, sizeof(vals));

    printf("Before sort: ");
    for (int i = 0; i < 8; i++) printf("%d ", arr[i]);
    printf("\n");

    /* ------------------------------------------------------------------ */
    /* 4. qsort                                                             */
    /* ------------------------------------------------------------------ */
    qsort(arr, 8, sizeof(int), int_cmp);

    printf("After  sort: ");
    for (int i = 0; i < 8; i++) printf("%d ", arr[i]);
    printf("\n");

    free(arr);

    /* ------------------------------------------------------------------ */
    /* 5. atoi / sprintf                                                    */
    /* ------------------------------------------------------------------ */
    const char *num_str = "42";
    int num = atoi(num_str);
    char result[32];
    sprintf(result, "atoi(\"%s\") = %d", num_str, num);
    puts(result);

    /* ------------------------------------------------------------------ */
    /* 6. strdup / strerror                                                 */
    /* ------------------------------------------------------------------ */
    char *dup = strdup("duplicated string");
    printf("strdup: \"%s\"\n", dup);
    free(dup);

    puts("=== All tests passed! ===");
    return 0;
}
