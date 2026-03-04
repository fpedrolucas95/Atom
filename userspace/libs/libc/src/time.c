/*
 * time.c — AtomOS libc time functions.
 *
 * time()    — wall-clock seconds since epoch (approximated via kernel ticks)
 * difftime  — difference between two time_t values
 * gmtime    — broken-down UTC time (static buffer)
 * localtime — same as gmtime (no timezone support)
 * mktime    — struct tm → time_t
 * strftime  — format time string (minimal subset)
 * asctime   — ctime-style string from struct tm
 * ctime     — ctime-style string from time_t
 *
 * Timing note:
 *   SYS_GET_TICKS (syscall 38) returns milliseconds since boot.
 *   We treat boot as a fixed epoch offset so that time() returns a
 *   plausible unix timestamp (seconds since 1970-01-01 00:00:00 UTC).
 *   The offset is chosen as 2024-01-01 00:00:00 UTC = 1704067200.
 */

#include <time.h>
#include <stdint.h>
#include <string.h>
#include "atom_syscall.h"

/* Unix timestamp of 2024-01-01 00:00:00 UTC, used as boot epoch */
#define BOOT_EPOCH_OFFSET UINT64_C(1704067200)

/* -----------------------------------------------------------------------
 * atom_get_ticks — raw kernel tick counter (milliseconds since boot)
 * ----------------------------------------------------------------------- */
time_t atom_get_ticks(void)
{
    return (time_t)atom_syscall0(SYS_GET_TICKS);
}

/* -----------------------------------------------------------------------
 * time — seconds since Unix epoch (approximated)
 * ----------------------------------------------------------------------- */
time_t time(time_t *tloc)
{
    uint64_t ms   = atom_syscall0(SYS_GET_TICKS);
    time_t   secs = (time_t)(BOOT_EPOCH_OFFSET + ms / 1000u);
    if (tloc)
        *tloc = secs;
    return secs;
}

/* -----------------------------------------------------------------------
 * difftime
 * ----------------------------------------------------------------------- */
double difftime(time_t time1, time_t time0)
{
    return (double)(time1 - time0);
}

/* -----------------------------------------------------------------------
 * Gregorian calendar helpers
 * ----------------------------------------------------------------------- */

static int is_leap(int y)
{
    return (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
}

static const int days_per_month[2][12] = {
    { 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 },
    { 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 }
};

static struct tm g_tm_buf;

/* -----------------------------------------------------------------------
 * gmtime / localtime — broken-down time (UTC; no timezone support)
 * ----------------------------------------------------------------------- */
struct tm *gmtime(const time_t *timep)
{
    time_t t = timep ? *timep : 0;

    /* Days and seconds within day */
    int64_t days = (int64_t)t / 86400;
    int secs     = (int)((int64_t)t % 86400);
    if (secs < 0) { secs += 86400; days--; }

    /* Time of day */
    g_tm_buf.tm_sec  = secs % 60;
    g_tm_buf.tm_min  = (secs / 60) % 60;
    g_tm_buf.tm_hour = secs / 3600;

    /* Day of week (1970-01-01 was a Thursday = 4) */
    g_tm_buf.tm_wday = (int)((days + 4) % 7);
    if (g_tm_buf.tm_wday < 0) g_tm_buf.tm_wday += 7;

    /* Year */
    int year = 1970;
    if (days >= 0) {
        while (1) {
            int ydays = is_leap(year) ? 366 : 365;
            if (days < ydays) break;
            days -= ydays;
            year++;
        }
    } else {
        while (days < 0) {
            year--;
            days += is_leap(year) ? 366 : 365;
        }
    }
    g_tm_buf.tm_year = year - 1900;
    g_tm_buf.tm_yday = (int)days;

    /* Month */
    int *mdays = (int *)days_per_month[is_leap(year)];
    int mon = 0;
    while (mon < 11 && days >= mdays[mon]) {
        days -= mdays[mon];
        mon++;
    }
    g_tm_buf.tm_mon  = mon;
    g_tm_buf.tm_mday = (int)days + 1;
    g_tm_buf.tm_isdst = 0;

    return &g_tm_buf;
}

struct tm *localtime(const time_t *timep)
{
    return gmtime(timep);
}

/* -----------------------------------------------------------------------
 * mktime — struct tm → time_t
 * ----------------------------------------------------------------------- */
time_t mktime(struct tm *tm)
{
    int year = tm->tm_year + 1900;
    int mon  = tm->tm_mon;          /* 0–11 */

    /* Normalise months */
    while (mon >= 12) { year++; mon -= 12; }
    while (mon <   0) { year--; mon += 12; }

    /* Days since epoch */
    int64_t days = 0;
    int y;
    if (year >= 1970) {
        for (y = 1970; y < year; y++)
            days += is_leap(y) ? 366 : 365;
    } else {
        for (y = year; y < 1970; y++)
            days -= is_leap(y) ? 366 : 365;
    }

    int *mdays = (int *)days_per_month[is_leap(year)];
    for (int m = 0; m < mon; m++)
        days += mdays[m];

    days += tm->tm_mday - 1;

    time_t t = (time_t)(days * 86400)
             + (time_t)(tm->tm_hour * 3600)
             + (time_t)(tm->tm_min  *   60)
             + (time_t) tm->tm_sec;

    /* Reflect normalised values back */
    struct tm *norm = gmtime(&t);
    *tm = *norm;
    return t;
}

/* -----------------------------------------------------------------------
 * asctime
 * ----------------------------------------------------------------------- */
static const char * const day_names[] = {
    "Sun","Mon","Tue","Wed","Thu","Fri","Sat"
};
static const char * const mon_names[] = {
    "Jan","Feb","Mar","Apr","May","Jun",
    "Jul","Aug","Sep","Oct","Nov","Dec"
};

static char g_asctime_buf[32];

char *asctime(const struct tm *tm)
{
    /* Format: "Www Mmm DD HH:MM:SS YYYY\n" */
    const char *wd = (tm->tm_wday >= 0 && tm->tm_wday <= 6)
                     ? day_names[tm->tm_wday] : "???";
    const char *mn = (tm->tm_mon  >= 0 && tm->tm_mon  <= 11)
                     ? mon_names[tm->tm_mon]  : "???";

    /* Build string manually (no sprintf dependency on format strings) */
    char *p = g_asctime_buf;
    /* Day-of-week */
    p[0]=wd[0]; p[1]=wd[1]; p[2]=wd[2]; p[3]=' '; p+=4;
    /* Month */
    p[0]=mn[0]; p[1]=mn[1]; p[2]=mn[2]; p[3]=' '; p+=4;
    /* Day (space-padded) */
    if (tm->tm_mday < 10) { p[0]=' '; p[1]=(char)('0'+tm->tm_mday); }
    else { p[0]=(char)('0'+tm->tm_mday/10); p[1]=(char)('0'+tm->tm_mday%10); }
    p[2]=' '; p+=3;
    /* HH:MM:SS */
    p[0]=(char)('0'+tm->tm_hour/10); p[1]=(char)('0'+tm->tm_hour%10); p[2]=':'; p+=3;
    p[0]=(char)('0'+tm->tm_min/10);  p[1]=(char)('0'+tm->tm_min%10);  p[2]=':'; p+=3;
    p[0]=(char)('0'+tm->tm_sec/10);  p[1]=(char)('0'+tm->tm_sec%10);  p[2]=' '; p+=3;
    /* Year */
    int yr = tm->tm_year + 1900;
    p[0]=(char)('0'+(yr/1000)%10); p[1]=(char)('0'+(yr/100)%10);
    p[2]=(char)('0'+(yr/10)%10);   p[3]=(char)('0'+yr%10);
    p[4]='\n'; p[5]='\0';

    return g_asctime_buf;
}

char *ctime(const time_t *timep)
{
    return asctime(localtime(timep));
}

/* -----------------------------------------------------------------------
 * strftime — minimal subset (%%  %Y %m %d %H %M %S %A %B %a %b %p)
 * ----------------------------------------------------------------------- */
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm)
{
    size_t n = 0;

#define PUTC(c) do { if (n + 1 >= max) goto done; s[n++] = (c); } while (0)
#define PUTS2(a,b) do { PUTC(a); PUTC(b); } while (0)
#define PUTS2D(v) PUTS2((char)('0'+(v)/10), (char)('0'+(v)%10))

    for (const char *f = format; *f; f++) {
        if (*f != '%') { PUTC(*f); continue; }
        f++;
        switch (*f) {
        case '%': PUTC('%'); break;
        case 'Y': {
            int yr = tm->tm_year + 1900;
            PUTS2((char)('0'+(yr/1000)%10), (char)('0'+(yr/100)%10));
            PUTS2D(yr % 100);
            break;
        }
        case 'y': PUTS2D((tm->tm_year + 1900) % 100); break;
        case 'm': PUTS2D(tm->tm_mon + 1); break;
        case 'd': PUTS2D(tm->tm_mday); break;
        case 'H': PUTS2D(tm->tm_hour); break;
        case 'M': PUTS2D(tm->tm_min);  break;
        case 'S': PUTS2D(tm->tm_sec);  break;
        case 'A': {
            const char *wd = (tm->tm_wday >= 0 && tm->tm_wday <= 6)
                ? (const char *[]){"Sunday","Monday","Tuesday","Wednesday",
                                   "Thursday","Friday","Saturday"}[tm->tm_wday]
                : "?";
            while (*wd) PUTC(*wd++);
            break;
        }
        case 'a': {
            const char *wd = (tm->tm_wday >= 0 && tm->tm_wday <= 6)
                ? day_names[tm->tm_wday] : "?";
            PUTC(wd[0]); PUTC(wd[1]); PUTC(wd[2]);
            break;
        }
        case 'B': {
            const char *mn = (tm->tm_mon >= 0 && tm->tm_mon <= 11)
                ? (const char *[]){"January","February","March","April","May",
                                   "June","July","August","September","October",
                                   "November","December"}[tm->tm_mon]
                : "?";
            while (*mn) PUTC(*mn++);
            break;
        }
        case 'b': case 'h': {
            const char *mn = (tm->tm_mon >= 0 && tm->tm_mon <= 11)
                ? mon_names[tm->tm_mon] : "?";
            PUTC(mn[0]); PUTC(mn[1]); PUTC(mn[2]);
            break;
        }
        case 'p': PUTC(tm->tm_hour < 12 ? 'A' : 'P'); PUTC('M'); break;
        default:  PUTC('%'); PUTC(*f); break;
        }
    }
done:
    if (n < max) s[n] = '\0';
    return n;

#undef PUTC
#undef PUTS2
#undef PUTS2D
}
