#ifndef _CTYPE_H
#define _CTYPE_H

int isalpha(int c);
int isdigit(int c);
int isalnum(int c);
int isspace(int c);
int isupper(int c);
int islower(int c);
int isprint(int c);
int ispunct(int c);
int isxdigit(int c);
int iscntrl(int c);
int isgraph(int c);
int isblank(int c);
int isascii(int c);

int toupper(int c);
int tolower(int c);
int toascii(int c);

#endif /* _CTYPE_H */
