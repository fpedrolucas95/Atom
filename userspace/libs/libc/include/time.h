#ifndef _TIME_H
#define _TIME_H

#include <sys/types.h>

struct tm {
    int tm_sec;    /* seconds [0,60] */
    int tm_min;    /* minutes [0,59] */
    int tm_hour;   /* hours   [0,23] */
    int tm_mday;   /* day of month [1,31] */
    int tm_mon;    /* months since January [0,11] */
    int tm_year;   /* years since 1900 */
    int tm_wday;   /* days since Sunday [0,6] */
    int tm_yday;   /* days since January 1 [0,365] */
    int tm_isdst;  /* daylight saving time flag */
};

time_t time(time_t *t);
double difftime(time_t time1, time_t time0);
struct tm *gmtime(const time_t *timep);
struct tm *localtime(const time_t *timep);
time_t mktime(struct tm *tm);
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm);
char *asctime(const struct tm *tm);
char *ctime(const time_t *timep);

/* AtomOS: ticks since boot (10 ms per tick at 100 Hz) */
time_t atom_get_ticks(void);

#endif /* _TIME_H */
