#ifndef _ATOMOS_ZLIB_H
#define _ATOMOS_ZLIB_H

typedef void *voidpf;
typedef unsigned char Bytef;
typedef unsigned int uInt;
typedef unsigned long uLong;

typedef struct z_stream_s {
    Bytef    *next_in;
    uInt      avail_in;
    uLong     total_in;

    Bytef    *next_out;
    uInt      avail_out;
    uLong     total_out;

    const char *msg;
    voidpf    state;

    voidpf    zalloc;
    voidpf    zfree;
    voidpf    opaque;

    int       data_type;
    uLong     adler;
    uLong     reserved;
} z_stream;

typedef z_stream *z_streamp;

#define Z_OK           0
#define Z_STREAM_END   1
#define Z_NO_FLUSH     0
#define Z_MEM_ERROR   -4

#define Z_NULL         ((voidpf)0)

typedef voidpf gzFile;

int inflateInit2(z_streamp strm, int windowBits);
int inflate(z_streamp strm, int flush);
int inflateEnd(z_streamp strm);

gzFile gzdopen(int fd, const char *mode);
int    gzeof(gzFile file);
int    gzread(gzFile file, void *buf, unsigned int len);
int    gzclose(gzFile file);

#endif /* _ATOMOS_ZLIB_H */

