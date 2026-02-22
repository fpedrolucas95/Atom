/*
 * ctype.c — AtomOS libc character classification and conversion.
 *
 * All functions operate on the basic ASCII subset (values 0–127).
 * Values outside that range are treated as non-classifiable and the
 * classification functions return 0.
 */

#include <ctype.h>

int isascii(int c) { return (unsigned)c < 128; }
int toascii(int c) { return c & 0x7f; }

int iscntrl(int c)  { return (unsigned)c < 32 || c == 127; }
int isprint(int c)  { return (unsigned)c >= 32 && (unsigned)c < 127; }
int isgraph(int c)  { return (unsigned)c > 32  && (unsigned)c < 127; }
int isspace(int c)  { return c == ' ' || c == '\t' || c == '\n' ||
                             c == '\r' || c == '\v' || c == '\f'; }
int isblank(int c)  { return c == ' ' || c == '\t'; }
int isupper(int c)  { return c >= 'A' && c <= 'Z'; }
int islower(int c)  { return c >= 'a' && c <= 'z'; }
int isalpha(int c)  { return isupper(c) || islower(c); }
int isdigit(int c)  { return c >= '0' && c <= '9'; }
int isxdigit(int c) { return isdigit(c) || (c >= 'a' && c <= 'f') ||
                                            (c >= 'A' && c <= 'F'); }
int isalnum(int c)  { return isalpha(c) || isdigit(c); }
int ispunct(int c)  { return isgraph(c) && !isalnum(c); }

int toupper(int c) { return islower(c) ? c - 'a' + 'A' : c; }
int tolower(int c) { return isupper(c) ? c - 'A' + 'a' : c; }
