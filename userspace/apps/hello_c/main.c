/*
 * hello_c — AtomOS libc Test Suite (graphical, desktop-integrated)
 *
 * Opens a proper composer window via the WM IPC protocol, maps the
 * shared surface region, renders test results directly into it, then
 * commits the frame so the compositor composites it on screen.
 *
 * Protocol stack (pure C, no Rust libs):
 *   namesvc (port 1) ─NsLookup──► compositor.wm port
 *   compositor.wm    ─WmRequest─► shared surface region
 *   shared region    ─draw──────► WmCommitFrame ──► visible window
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>

/* =========================================================================
 * 1. Syscall wrappers
 * ========================================================================= */

#define SYS_THREAD_YIELD        0ULL
#define SYS_THREAD_EXIT         1ULL
#define SYS_IPC_CREATE_PORT     4ULL
#define SYS_IPC_CLOSE_PORT      5ULL
#define SYS_IPC_RECV            7ULL
#define SYS_SHARED_REGION_MAP  18ULL
#define SYS_IPC_SEND_ASYNC     23ULL
#define SYS_IPC_TRY_RECV       24ULL

/* Return value ≥ this threshold means error (kernel convention: MAX-256) */
#define KERN_ERR_THRESHOLD 0xFFFFFFFFFFFFFF00ULL

static inline bool is_err(uint64_t v) { return v >= KERN_ERR_THRESHOLD; }

static inline uint64_t sc0(uint64_t n) {
    uint64_t r;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n)
        : "rcx","r11","rdi","rsi","rdx","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc1(uint64_t n, uint64_t a) {
    uint64_t r;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n),"D"(a)
        : "rcx","r11","rsi","rdx","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc3(uint64_t n, uint64_t a, uint64_t b, uint64_t c) {
    uint64_t r;
    register uint64_t _b __asm__("rsi") = b;
    register uint64_t _c __asm__("rdx") = c;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n),"D"(a),"r"(_b),"r"(_c)
        : "rcx","r11","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc4(uint64_t n, uint64_t a, uint64_t b, uint64_t c, uint64_t d) {
    uint64_t r;
    register uint64_t _b __asm__("rsi") = b;
    register uint64_t _c __asm__("rdx") = c;
    register uint64_t _d __asm__("r10") = d;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n),"D"(a),"r"(_b),"r"(_c),"r"(_d)
        : "rcx","r11","r8","r9","memory");
    return r;
}

static uint64_t ipc_create_port(void) {
    uint64_t r = sc0(SYS_IPC_CREATE_PORT);
    return is_err(r) ? 0 : r;
}
static void ipc_close_port(uint64_t port) { sc1(SYS_IPC_CLOSE_PORT, port); }

/* send: SYS_IPC_SEND_ASYNC(port, 0, ptr, len) */
static bool ipc_send(uint64_t port, const void *data, uint32_t len) {
    uint64_t r = sc4(SYS_IPC_SEND_ASYNC, port, 0,
                     (uint64_t)(uintptr_t)data, (uint64_t)len);
    return !is_err(r);
}
/* blocking recv: SYS_IPC_RECV(port, buf, len, timeout=0=block) */
static int ipc_recv(uint64_t port, void *buf, uint32_t len) {
    uint64_t r = sc4(SYS_IPC_RECV, port,
                     (uint64_t)(uintptr_t)buf, (uint64_t)len, 0ULL);
    return is_err(r) ? -1 : (int)r;
}
/* non-blocking recv: timeout=1ms */
static int ipc_try_recv(uint64_t port, void *buf, uint32_t len) {
    uint64_t r = sc4(SYS_IPC_RECV, port,
                     (uint64_t)(uintptr_t)buf, (uint64_t)len, 1ULL);
    return is_err(r) ? -1 : (int)r;
}
/* map shared region: SYS_SHARED_REGION_MAP(region_id, 0=auto_va, flags=3=rw) */
static uintptr_t shared_map(uint64_t region_id) {
    uint64_t r = sc3(SYS_SHARED_REGION_MAP, region_id, 0ULL, 3ULL);
    return is_err(r) ? 0 : (uintptr_t)r;
}

/* =========================================================================
 * 2. IPC message protocol (mirrors libipc wire format)
 * ========================================================================= */

/* MessageHeader: 16 bytes, all LE */
typedef struct __attribute__((packed)) {
    uint32_t version;       /* always 1 */
    uint32_t msg_type;
    uint32_t payload_size;
    uint32_t sequence;
} MsgHeader;

#define HDR_SIZE  ((uint32_t)sizeof(MsgHeader))
#define HDR_VER   1

static uint32_t g_seq = 0;

static void hdr_init(MsgHeader *h, uint32_t type, uint32_t psz) {
    h->version      = HDR_VER;
    h->msg_type     = type;
    h->payload_size = psz;
    h->sequence     = g_seq++;
}

/* Message types */
#define MSG_NS_LOOKUP   602u
#define MSG_NS_RESPONSE 610u
#define MSG_WM_REQUEST  800u
#define MSG_WM_RESPONSE 801u
#define MSG_WM_COMMIT   803u
#define MSG_QUIT        402u
#define MSG_KEY_PRESS     3u

/* WmRequestType::CreateWindow = 1 */
#define WM_CREATE_WINDOW 1u

/* NAME_SERVICE is always port 1 */
#define NAME_SERVICE_PORT 1ULL

/* -------------------------------------------------------------------------
 * NsLookup: look up a service name → port
 * Payload format: [reply_port: u64][name: [u8; 32]]   = 40 bytes
 * Response payload: [port: u64]                        =  8 bytes
 * ------------------------------------------------------------------------- */
static uint64_t ns_lookup(const char *name) {
    uint64_t reply = ipc_create_port();
    if (!reply) return 0;

    uint8_t buf[HDR_SIZE + 40];
    MsgHeader *h = (MsgHeader *)buf;
    hdr_init(h, MSG_NS_LOOKUP, 40);

    uint8_t *payload = buf + HDR_SIZE;
    memcpy(payload, &reply, 8);                /* reply_port */
    memset(payload + 8, 0, 32);               /* name[32] */
    size_t nlen = strlen(name);
    if (nlen > 31) nlen = 31;
    memcpy(payload + 8, name, nlen);

    ipc_send(NAME_SERVICE_PORT, buf, (uint32_t)sizeof(buf));

    /* Wait up to 10000 yields for response */
    uint8_t resp[HDR_SIZE + 8];
    uint64_t found = 0;
    for (int i = 0; i < 10000 && !found; i++) {
        int n = ipc_try_recv(reply, resp, (uint32_t)sizeof(resp));
        if (n >= (int)(HDR_SIZE + 8)) {
            MsgHeader *rh = (MsgHeader *)resp;
            if (rh->msg_type == MSG_NS_RESPONSE) {
                memcpy(&found, resp + HDR_SIZE, 8);
            }
        }
        sc0(SYS_THREAD_YIELD);
    }

    ipc_close_port(reply);
    return found;
}

/* -------------------------------------------------------------------------
 * WmCreateWindow: create a compositor window
 * Request payload: [req_type: u32][reply_port: u64][w: u32][h: u32]
 *                  [title_len: u32][title: bytes]
 * Response payload: [window_id: u32][region_id: u64][w: u32][h: u32][stride: u32]
 * ------------------------------------------------------------------------- */
typedef struct {
    uint32_t window_id;
    uint64_t region_id;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
} WmWindowInfo;

static bool wm_create_window(uint64_t wm_port, uint64_t event_port,
                              uint32_t width, uint32_t height,
                              const char *title, WmWindowInfo *out) {
    uint32_t tlen = (uint32_t)strlen(title);
    uint32_t psz  = 4 + 8 + 4 + 4 + 4 + tlen;   /* req_type+reply_port+w+h+tlen+title */
    uint32_t msz  = HDR_SIZE + psz;
    uint8_t  msg[256];
    if (msz > sizeof(msg)) return false;

    MsgHeader *h = (MsgHeader *)msg;
    hdr_init(h, MSG_WM_REQUEST, psz);

    uint8_t *p = msg + HDR_SIZE;
    uint32_t req_type = WM_CREATE_WINDOW;
    memcpy(p,      &req_type,   4); p += 4;
    memcpy(p,      &event_port, 8); p += 8;
    memcpy(p,      &width,      4); p += 4;
    memcpy(p,      &height,     4); p += 4;
    memcpy(p,      &tlen,       4); p += 4;
    memcpy(p,      title,       tlen);

    ipc_send(wm_port, msg, msz);

    /* Wait for WmResponse (msg_type=801) */
    uint8_t resp[HDR_SIZE + 24];
    for (int i = 0; i < 10000; i++) {
        int n = ipc_try_recv(event_port, resp, (uint32_t)sizeof(resp));
        if (n >= (int)(HDR_SIZE + 24)) {
            MsgHeader *rh = (MsgHeader *)resp;
            if (rh->msg_type == MSG_WM_RESPONSE) {
                uint8_t *rp = resp + HDR_SIZE;
                memcpy(&out->window_id, rp,      4);
                memcpy(&out->region_id, rp + 4,  8);
                memcpy(&out->width,     rp + 12, 4);
                memcpy(&out->height,    rp + 16, 4);
                memcpy(&out->stride,    rp + 20, 4);
                return true;
            }
        }
        sc0(SYS_THREAD_YIELD);
    }
    return false;
}

/* -------------------------------------------------------------------------
 * WmCommitFrame: tell compositor to composite the window
 * Payload: [window_id: u32]
 * ------------------------------------------------------------------------- */
static void wm_commit(uint64_t wm_port, uint32_t window_id) {
    uint8_t msg[HDR_SIZE + 4];
    MsgHeader *h = (MsgHeader *)msg;
    hdr_init(h, MSG_WM_COMMIT, 4);
    memcpy(msg + HDR_SIZE, &window_id, 4);
    ipc_send(wm_port, msg, (uint32_t)sizeof(msg));
}

/* =========================================================================
 * 3. Drawing primitives (into WM shared surface)
 * ========================================================================= */

/* Pixel format matches compositor: 0x00RRGGBB stored as LE u32 */
#define RGB(r,g,b) ((uint32_t)(((uint32_t)(r)<<16)|((uint32_t)(g)<<8)|(b)))

/* Nord-inspired palette */
#define COL_BG        RGB(0x1E,0x20,0x30)
#define COL_PANEL     RGB(0x2A,0x2D,0x42)
#define COL_TITLE_BG  RGB(0x36,0x3A,0x54)
#define COL_TEXT      RGB(0xEC,0xEF,0xF4)
#define COL_DIM       RGB(0x8A,0x8F,0xA8)
#define COL_PASS_BG   RGB(0x26,0x3C,0x28)
#define COL_PASS_FG   RGB(0xA3,0xBE,0x8C)
#define COL_FAIL_BG   RGB(0x3C,0x22,0x22)
#define COL_FAIL_FG   RGB(0xBF,0x61,0x6A)
#define COL_ACCENT    RGB(0x88,0xC0,0xD0)
#define COL_BORDER    RGB(0x44,0x49,0x66)

typedef struct {
    uint32_t *pixels; /* base pointer to shared surface */
    uint32_t  width;
    uint32_t  height;
    uint32_t  stride; /* pixels per row */
} Canvas;

static void canvas_init(Canvas *c, uintptr_t addr,
                         uint32_t w, uint32_t h, uint32_t stride) {
    c->pixels = (uint32_t *)(uintptr_t)addr;
    c->width  = w;
    c->height = h;
    c->stride = stride;
}

static inline void put_px(const Canvas *c, int x, int y, uint32_t col) {
    if ((unsigned)x >= c->width || (unsigned)y >= c->height) return;
    volatile uint32_t *p = c->pixels + (uint32_t)y * c->stride + (uint32_t)x;
    *p = col;
}

static void fill(const Canvas *c, int x, int y, int w, int h, uint32_t col) {
    for (int dy = 0; dy < h; dy++)
        for (int dx = 0; dx < w; dx++)
            put_px(c, x+dx, y+dy, col);
}

static void border(const Canvas *c, int x, int y, int w, int h, uint32_t col) {
    for (int i = 0; i < w; i++) { put_px(c,x+i,y,col);     put_px(c,x+i,y+h-1,col); }
    for (int i = 0; i < h; i++) { put_px(c,x,y+i,col);     put_px(c,x+w-1,y+i,col); }
}

/* 8×8 bitmap font, ×2 scale, same glyph table as libgui */
static const uint8_t FONT[95][8] = {
    {0x00,0x00,0x00,0x00,0x00,0x00,0x00,0x00}, /* ' ' 0x20 */
    {0x18,0x18,0x18,0x18,0x18,0x00,0x18,0x00}, /* '!' */
    {0x6C,0x6C,0x24,0x00,0x00,0x00,0x00,0x00},
    {0x6C,0x6C,0xFE,0x6C,0xFE,0x6C,0x6C,0x00},
    {0x18,0x3E,0x60,0x3C,0x06,0x7C,0x18,0x00},
    {0x00,0xC6,0xCC,0x18,0x30,0x66,0xC6,0x00},
    {0x38,0x6C,0x38,0x76,0xDC,0xCC,0x76,0x00},
    {0x18,0x18,0x30,0x00,0x00,0x00,0x00,0x00},
    {0x0C,0x18,0x30,0x30,0x30,0x18,0x0C,0x00},
    {0x30,0x18,0x0C,0x0C,0x0C,0x18,0x30,0x00},
    {0x00,0x66,0x3C,0xFF,0x3C,0x66,0x00,0x00},
    {0x00,0x18,0x18,0x7E,0x18,0x18,0x00,0x00},
    {0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x30},
    {0x00,0x00,0x00,0x7E,0x00,0x00,0x00,0x00},
    {0x00,0x00,0x00,0x00,0x00,0x18,0x18,0x00},
    {0x06,0x0C,0x18,0x30,0x60,0xC0,0x80,0x00},
    /* 0-9 */
    {0x3C,0x66,0x6E,0x76,0x66,0x66,0x3C,0x00},
    {0x18,0x38,0x18,0x18,0x18,0x18,0x7E,0x00},
    {0x3C,0x66,0x06,0x0C,0x18,0x30,0x7E,0x00},
    {0x3C,0x66,0x06,0x1C,0x06,0x66,0x3C,0x00},
    {0x0C,0x1C,0x3C,0x6C,0x7E,0x0C,0x0C,0x00},
    {0x7E,0x60,0x7C,0x06,0x06,0x66,0x3C,0x00},
    {0x1C,0x30,0x60,0x7C,0x66,0x66,0x3C,0x00},
    {0x7E,0x06,0x0C,0x18,0x30,0x30,0x30,0x00},
    {0x3C,0x66,0x66,0x3C,0x66,0x66,0x3C,0x00},
    {0x3C,0x66,0x66,0x3E,0x06,0x0C,0x38,0x00},
    /* :;<=>?@ */
    {0x00,0x18,0x18,0x00,0x18,0x18,0x00,0x00},
    {0x00,0x18,0x18,0x00,0x18,0x18,0x30,0x00},
    {0x0C,0x18,0x30,0x60,0x30,0x18,0x0C,0x00},
    {0x00,0x00,0x7E,0x00,0x7E,0x00,0x00,0x00},
    {0x30,0x18,0x0C,0x06,0x0C,0x18,0x30,0x00},
    {0x3C,0x66,0x0C,0x18,0x18,0x00,0x18,0x00},
    {0x3C,0x66,0x6E,0x6A,0x6E,0x60,0x3C,0x00},
    /* A-Z */
    {0x18,0x3C,0x66,0x66,0x7E,0x66,0x66,0x00},
    {0x7C,0x66,0x66,0x7C,0x66,0x66,0x7C,0x00},
    {0x3C,0x66,0x60,0x60,0x60,0x66,0x3C,0x00},
    {0x78,0x6C,0x66,0x66,0x66,0x6C,0x78,0x00},
    {0x7E,0x60,0x60,0x7C,0x60,0x60,0x7E,0x00},
    {0x7E,0x60,0x60,0x7C,0x60,0x60,0x60,0x00},
    {0x3C,0x66,0x60,0x6E,0x66,0x66,0x3E,0x00},
    {0x66,0x66,0x66,0x7E,0x66,0x66,0x66,0x00},
    {0x3C,0x18,0x18,0x18,0x18,0x18,0x3C,0x00},
    {0x06,0x06,0x06,0x06,0x66,0x66,0x3C,0x00},
    {0x66,0x6C,0x78,0x70,0x78,0x6C,0x66,0x00},
    {0x60,0x60,0x60,0x60,0x60,0x60,0x7E,0x00},
    {0xC6,0xEE,0xFE,0xD6,0xC6,0xC6,0xC6,0x00},
    {0x66,0x76,0x7E,0x7E,0x6E,0x66,0x66,0x00},
    {0x3C,0x66,0x66,0x66,0x66,0x66,0x3C,0x00},
    {0x7C,0x66,0x66,0x7C,0x60,0x60,0x60,0x00},
    {0x3C,0x66,0x66,0x66,0x6A,0x6C,0x36,0x00},
    {0x7C,0x66,0x66,0x7C,0x6C,0x66,0x66,0x00},
    {0x3C,0x66,0x60,0x3C,0x06,0x66,0x3C,0x00},
    {0x7E,0x18,0x18,0x18,0x18,0x18,0x18,0x00},
    {0x66,0x66,0x66,0x66,0x66,0x66,0x3C,0x00},
    {0x66,0x66,0x66,0x66,0x66,0x3C,0x18,0x00},
    {0xC6,0xC6,0xC6,0xD6,0xFE,0xEE,0xC6,0x00},
    {0x66,0x66,0x3C,0x18,0x3C,0x66,0x66,0x00},
    {0x66,0x66,0x66,0x3C,0x18,0x18,0x18,0x00},
    {0x7E,0x06,0x0C,0x18,0x30,0x60,0x7E,0x00},
    /* [ \ ] ^ _ ` */
    {0x3C,0x30,0x30,0x30,0x30,0x30,0x3C,0x00},
    {0xC0,0x60,0x30,0x18,0x0C,0x06,0x02,0x00},
    {0x3C,0x0C,0x0C,0x0C,0x0C,0x0C,0x3C,0x00},
    {0x10,0x38,0x6C,0xC6,0x00,0x00,0x00,0x00},
    {0x00,0x00,0x00,0x00,0x00,0x00,0xFE,0x00},
    {0x30,0x18,0x0C,0x00,0x00,0x00,0x00,0x00},
    /* a-z */
    {0x00,0x00,0x3C,0x06,0x3E,0x66,0x3E,0x00},
    {0x60,0x60,0x7C,0x66,0x66,0x66,0x7C,0x00},
    {0x00,0x00,0x3C,0x60,0x60,0x60,0x3C,0x00},
    {0x06,0x06,0x3E,0x66,0x66,0x66,0x3E,0x00},
    {0x00,0x00,0x3C,0x66,0x7E,0x60,0x3C,0x00},
    {0x1C,0x30,0x7C,0x30,0x30,0x30,0x30,0x00},
    {0x00,0x00,0x3E,0x66,0x66,0x3E,0x06,0x3C},
    {0x60,0x60,0x7C,0x66,0x66,0x66,0x66,0x00},
    {0x18,0x00,0x38,0x18,0x18,0x18,0x3C,0x00},
    {0x0C,0x00,0x1C,0x0C,0x0C,0x0C,0x6C,0x38},
    {0x60,0x60,0x66,0x6C,0x78,0x6C,0x66,0x00},
    {0x38,0x18,0x18,0x18,0x18,0x18,0x3C,0x00},
    {0x00,0x00,0xEC,0xFE,0xD6,0xC6,0xC6,0x00},
    {0x00,0x00,0x7C,0x66,0x66,0x66,0x66,0x00},
    {0x00,0x00,0x3C,0x66,0x66,0x66,0x3C,0x00},
    {0x00,0x00,0x7C,0x66,0x66,0x7C,0x60,0x60},
    {0x00,0x00,0x3E,0x66,0x66,0x3E,0x06,0x06},
    {0x00,0x00,0x6E,0x70,0x60,0x60,0x60,0x00},
    {0x00,0x00,0x3E,0x60,0x3C,0x06,0x7C,0x00},
    {0x30,0x30,0x7C,0x30,0x30,0x30,0x1C,0x00},
    {0x00,0x00,0x66,0x66,0x66,0x66,0x3E,0x00},
    {0x00,0x00,0x66,0x66,0x66,0x3C,0x18,0x00},
    {0x00,0x00,0xC6,0xC6,0xD6,0xFE,0x6C,0x00},
    {0x00,0x00,0x66,0x3C,0x18,0x3C,0x66,0x00},
    {0x00,0x00,0x66,0x66,0x66,0x3E,0x06,0x3C},
    {0x00,0x00,0x7E,0x0C,0x18,0x30,0x7E,0x00},
    /* { | } ~ */
    {0x0E,0x18,0x18,0x70,0x18,0x18,0x0E,0x00},
    {0x18,0x18,0x18,0x00,0x18,0x18,0x18,0x00},
    {0x70,0x18,0x18,0x0E,0x18,0x18,0x70,0x00},
    {0x76,0xDC,0x00,0x00,0x00,0x00,0x00,0x00},
};

#define FS 2              /* font scale */
#define CW (8*FS)         /* char width in px */
#define CH (8*FS)         /* char height in px */

static void draw_char(const Canvas *c, int x, int y,
                      char ch, uint32_t fg, uint32_t bg) {
    uint8_t idx = (uint8_t)ch;
    if (idx < 32 || idx > 126) idx = 32;
    const uint8_t *g = FONT[idx - 32];
    for (int r = 0; r < 8; r++) {
        for (int col = 0; col < 8; col++) {
            uint32_t color = (g[r] & (0x80>>col)) ? fg : bg;
            put_px(c, x+col*FS,   y+r*FS,   color);
            put_px(c, x+col*FS+1, y+r*FS,   color);
            put_px(c, x+col*FS,   y+r*FS+1, color);
            put_px(c, x+col*FS+1, y+r*FS+1, color);
        }
    }
}

static void draw_str(const Canvas *c, int x, int y,
                     const char *s, uint32_t fg, uint32_t bg) {
    while (*s) { draw_char(c,x,y,*s++,fg,bg); x+=CW; }
}
static void draw_str_fg(const Canvas *c, int x, int y,
                        const char *s, uint32_t fg) {
    while (*s) { draw_char(c,x,y,*s++,fg,0); x+=CW; }  /* 0 bg = transparent */
}

/* =========================================================================
 * 4. Test implementations
 * ========================================================================= */

typedef struct {
    const char *name;
    const char *detail;
    bool        passed;
} TestResult;

#define MAX_TESTS 16
static TestResult g_res[MAX_TESTS];
static int g_count = 0, g_pass = 0;

static void record(const char *name, const char *detail, bool ok) {
    if (g_count >= MAX_TESTS) return;
    g_res[g_count].name   = name;
    g_res[g_count].detail = detail;
    g_res[g_count].passed = ok;
    if (ok) g_pass++;
    g_count++;
}

#define RUN(nm,dt,expr) record(nm, dt, (expr))

static int int_cmp(const void *a, const void *b) {
    int x = *(const int*)a, y = *(const int*)b;
    return (x>y)-(x<y);
}

static void run_all_tests(void) {
    /* puts / printf */
    RUN("puts / printf", NULL, (puts("libc test running"),true));

    /* strlen / strcpy */
    {
        const char *s = "AtomOS";
        char buf[16]; strcpy(buf,s);
        RUN("strlen / strcpy", "len(\"AtomOS\")==6",
            strlen(s)==6 && strcmp(buf,s)==0);
    }

    /* snprintf */
    {
        char buf[64];
        snprintf(buf,sizeof(buf),"val=%d str=%s",99,"hi");
        RUN("snprintf","val=99 str=hi", strcmp(buf,"val=99 str=hi")==0);
    }

    /* malloc / free */
    {
        int *p = malloc(16*sizeof(int));
        bool ok = p != NULL;
        if (p) { for(int i=0;i<16;i++) p[i]=i*2; ok = p[15]==30; free(p); }
        RUN("malloc / free","16 ints",ok);
    }

    /* memcpy / memset */
    {
        uint8_t src[8]={1,2,3,4,5,6,7,8}, dst[8]={0};
        memcpy(dst,src,8);
        bool ok=true; for(int i=0;i<8;i++) if(dst[i]!=src[i]){ ok=false; break; }
        memset(dst,0xAB,8);
        for(int i=0;i<8;i++) if(dst[i]!=0xAB){ ok=false; break; }
        RUN("memcpy / memset","8 bytes",ok);
    }

    /* memmove overlap */
    {
        char buf[16]="Hello, World!";
        memmove(buf+7,buf,5);
        RUN("memmove (overlap)",NULL, strncmp(buf+7,"Hello",5)==0);
    }

    /* qsort */
    {
        int arr[]={9,3,7,1,5,2,8,4,6,0};
        qsort(arr,10,sizeof(int),int_cmp);
        bool ok=true; for(int i=0;i<10;i++) if(arr[i]!=i){ ok=false; break; }
        RUN("qsort","0..9 sorted",ok);
    }

    /* atoi */
    {
        RUN("atoi","42 -7 0",
            atoi("42")==42 && atoi("-7")==-7 && atoi("0")==0);
    }

    /* strdup / strndup */
    {
        char *a=strdup("hello");
        char *b=strndup("world!!!",5);
        bool ok = a && b && strcmp(a,"hello")==0 && strcmp(b,"world")==0;
        free(a); free(b);
        RUN("strdup / strndup",NULL,ok);
    }

    /* strstr / strchr */
    {
        const char *h="The quick brown fox";
        RUN("strstr / strchr",NULL,
            strstr(h,"quick")!=NULL && strstr(h,"lazy")==NULL
            && strchr(h,'q')!=NULL && strchr(h,'z')==NULL);
    }

    /* strtok */
    {
        char buf[]="one,two,three";
        char *t=strtok(buf,",");
        bool ok = t && strcmp(t,"one")==0;
        t=strtok(NULL,","); ok = ok && t && strcmp(t,"two")==0;
        t=strtok(NULL,","); ok = ok && t && strcmp(t,"three")==0;
        t=strtok(NULL,","); ok = ok && t==NULL;
        RUN("strtok","one,two,three",ok);
    }

    /* abs / labs */
    RUN("abs / labs",NULL,
        abs(-42)==42 && abs(7)==7 && labs(-1000000L)==1000000L);
}

/* =========================================================================
 * 5. UI render
 * ========================================================================= */

#define PX  10       /* panel x */
#define PY  10       /* panel y */
#define PW  620      /* panel width */
#define TH  36       /* title bar height */
#define RH  28       /* row height */
#define RPX 12       /* row padding x */
#define RPY  6       /* row padding y */
#define BW  56       /* badge width */
#define BH  18       /* badge height */

static void render(const Canvas *c) {
    int ph = TH + g_count*RH + 46;

    /* Background */
    fill(c, 0, 0, (int)c->width, (int)c->height, COL_BG);

    /* Panel */
    fill(c, PX, PY, PW, ph, COL_PANEL);
    border(c, PX, PY, PW, ph, COL_BORDER);

    /* Title bar */
    fill(c, PX, PY, PW, TH, COL_TITLE_BG);
    draw_str(c, PX+12, PY+9, "AtomOS libc Test Suite", COL_ACCENT, COL_TITLE_BG);
    for (int i=0;i<PW;i++) put_px(c, PX+i, PY+TH-1, COL_BORDER);

    /* Test rows */
    for (int i=0; i<g_count; i++) {
        int ry = PY + TH + i*RH;
        uint32_t rbg = (i%2==0) ? COL_PANEL : RGB(0x2E,0x31,0x48);
        fill(c, PX+1, ry, PW-2, RH, rbg);

        /* index */
        char idx[4]; snprintf(idx,sizeof(idx),"%2d",i+1);
        draw_str_fg(c, PX+RPX, ry+RPY, idx, COL_DIM);

        /* name */
        draw_str_fg(c, PX+RPX+32, ry+RPY, g_res[i].name, COL_TEXT);

        /* detail */
        if (g_res[i].detail) {
            int dx = PX+RPX+32 + (int)(strlen(g_res[i].name)*CW) + 8;
            draw_str_fg(c, dx, ry+RPY, g_res[i].detail, COL_DIM);
        }

        /* badge */
        bool ok = g_res[i].passed;
        int bx = PX+PW-BW-10, by = ry+(RH-BH)/2;
        fill(c, bx, by, BW, BH, ok?COL_PASS_BG:COL_FAIL_BG);
        border(c, bx, by, BW, BH, ok?COL_PASS_FG:COL_FAIL_FG);
        draw_str_fg(c, bx+4, by+1, ok?" PASS ":" FAIL ",
                    ok?COL_PASS_FG:COL_FAIL_FG);
    }

    /* Summary */
    int sy = PY+TH+g_count*RH+6;
    char sum[64];
    snprintf(sum,sizeof(sum),"Results: %d / %d passed", g_pass, g_count);
    draw_str_fg(c, PX+RPX, sy, sum,
                (g_pass==g_count)?COL_PASS_FG:COL_FAIL_FG);

    /* Progress bar */
    int bary = sy+CH+4, barw=PW-RPX*2;
    fill(c,   PX+RPX, bary, barw,    10, COL_BORDER);
    int fw = g_count>0 ? barw*g_pass/g_count : 0;
    if (fw>0) fill(c, PX+RPX, bary, fw, 10, COL_PASS_FG);
    border(c, PX+RPX, bary, barw,    10, COL_DIM);
}

/* =========================================================================
 * 6. Entry point
 * ========================================================================= */

int main(void) {
    /* -- Run tests first (no UI needed) -- */
    run_all_tests();

    /* -- Serial summary -- */
    printf("\n=== AtomOS libc Test Results ===\n");
    for (int i=0; i<g_count; i++)
        printf("  [%s] %s\n", g_res[i].passed?"PASS":"FAIL", g_res[i].name);
    printf("Total: %d/%d\n", g_pass, g_count);

    /* -- Look up compositor WM port -- */
    uint64_t wm_port = ns_lookup("compositor.wm");
    if (!wm_port) {
        printf("[hello_c] compositor.wm not found, running headless\n");
        return 0;
    }

    /* -- Create IPC event port & window -- */
    uint64_t event_port = ipc_create_port();
    if (!event_port) return 1;

    WmWindowInfo win;
    if (!wm_create_window(wm_port, event_port, 660, 400,
                          "libc Test Suite", &win)) {
        printf("[hello_c] failed to create window\n");
        ipc_close_port(event_port);
        return 1;
    }

    /* -- Map shared surface -- */
    uintptr_t surface_addr = shared_map(win.region_id);
    if (!surface_addr) {
        printf("[hello_c] failed to map surface region %llu\n",
               (unsigned long long)win.region_id);
        ipc_close_port(event_port);
        return 1;
    }

    /* -- Draw & present -- */
    Canvas canvas;
    canvas_init(&canvas, surface_addr, win.width, win.height, win.stride);
    render(&canvas);
    wm_commit(wm_port, win.window_id);

    /* -- Event loop: redraw on events, quit on MSG_QUIT -- */
    uint8_t evbuf[256];
    for (;;) {
        int n = ipc_recv(event_port, evbuf, (uint32_t)sizeof(evbuf));
        if (n < (int)HDR_SIZE) { sc0(SYS_THREAD_YIELD); continue; }
        MsgHeader *h = (MsgHeader *)evbuf;
        if (h->msg_type == MSG_QUIT) break;
        /* Redraw on any window event */
        render(&canvas);
        wm_commit(wm_port, win.window_id);
    }

    ipc_close_port(event_port);
    return 0;
}

