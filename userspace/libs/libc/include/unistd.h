#ifndef _UNISTD_H
#define _UNISTD_H

#include <sys/types.h>

/* Standard file descriptor numbers */
#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

/* Seek whence values */
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

/* I/O */
ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int     close(int fd);

/* File descriptor duplication */
int dup(int fd);
int dup2(int oldfd, int newfd);

/* File positioning */
off_t lseek(int fd, off_t offset, int whence);

/* File/directory operations */
int unlink(const char *path);
int rmdir(const char *path);
int rename(const char *oldpath, const char *newpath);
int symlink(const char *target, const char *linkpath);
ssize_t readlink(const char *path, char *buf, size_t bufsiz);
int link(const char *oldpath, const char *newpath);
int access(const char *path, int mode);
int truncate(const char *path, off_t length);
int ftruncate(int fd, off_t length);
int fsync(int fd);

/* Process */
void _exit(int status);
pid_t getpid(void);
pid_t getppid(void);

/* Working directory */
char *getcwd(char *buf, size_t size);
int chdir(const char *path);

/* Sleeping */
unsigned int sleep(unsigned int seconds);
int usleep(unsigned long usec);

/* Access mode bits */
#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4

#endif /* _UNISTD_H */
