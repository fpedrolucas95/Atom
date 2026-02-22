#ifndef _SYS_STAT_H
#define _SYS_STAT_H

#include <sys/types.h>

/* File type bits for st_mode */
#define S_IFMT   0170000
#define S_IFREG  0100000
#define S_IFDIR  0040000
#define S_IFLNK  0120000
#define S_IFBLK  0060000
#define S_IFCHR  0020000
#define S_IFIFO  0010000
#define S_IFSOCK 0140000

/* Macros to test file type */
#define S_ISREG(m)   (((m) & S_IFMT) == S_IFREG)
#define S_ISDIR(m)   (((m) & S_IFMT) == S_IFDIR)
#define S_ISLNK(m)   (((m) & S_IFMT) == S_IFLNK)
#define S_ISBLK(m)   (((m) & S_IFMT) == S_IFBLK)
#define S_ISCHR(m)   (((m) & S_IFMT) == S_IFCHR)
#define S_ISFIFO(m)  (((m) & S_IFMT) == S_IFIFO)
#define S_ISSOCK(m)  (((m) & S_IFMT) == S_IFSOCK)

/* Permission bits */
#define S_IRUSR  0400
#define S_IWUSR  0200
#define S_IXUSR  0100
#define S_IRWXU  (S_IRUSR | S_IWUSR | S_IXUSR)

#define S_IRGRP  0040
#define S_IWGRP  0020
#define S_IXGRP  0010
#define S_IRWXG  (S_IRGRP | S_IWGRP | S_IXGRP)

#define S_IROTH  0004
#define S_IWOTH  0002
#define S_IXOTH  0001
#define S_IRWXO  (S_IROTH | S_IWOTH | S_IXOTH)

struct stat {
    dev_t     st_dev;
    ino_t     st_ino;
    mode_t    st_mode;
    nlink_t   st_nlink;
    uid_t     st_uid;
    gid_t     st_gid;
    dev_t     st_rdev;
    off_t     st_size;
    blksize_t st_blksize;
    blkcnt_t  st_blocks;
    time_t    st_atime;
    time_t    st_mtime;
    time_t    st_ctime;
};

int stat(const char *path, struct stat *buf);
int fstat(int fd, struct stat *buf);
int lstat(const char *path, struct stat *buf);
int mkdir(const char *path, mode_t mode);
int chmod(const char *path, mode_t mode);

#endif /* _SYS_STAT_H */
