/*
 * doomgeneric_atom.c — AtomOS platform layer for doomgeneric
 *
 * Implements the six doomgeneric callbacks using:
 *  - AtomOS compositor IPC for window creation and frame presentation
 *  - SYS_KEYBOARD_POLL for raw Scancode Set 1 input
 *  - SYS_GET_TICKS (100 Hz) × 10 for millisecond timer
 *  - SYS_THREAD_SLEEP for DG_SleepMs
 *
 * Build dependency: libc (for malloc/memcpy/printf), no other libs needed.
 *
 * Entry point:
 *   main() calls doomgeneric_Create(argc, argv) with a hard-coded
 *   -iwad path pointing at the WAD on the AtomOS FAT32 volume.
 */

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

#include "doomgeneric.h"
#include "doomkeys.h"

/* ==========================================================================
 * §1  Kernel syscall wrappers  (mirrored verbatim from tinygl_demo)
 * ======================================================================== */

#define SYS_THREAD_YIELD      0ULL
#define SYS_THREAD_EXIT       1ULL
#define SYS_THREAD_SLEEP      2ULL
#define SYS_IPC_CREATE_PORT   4ULL
#define SYS_IPC_CLOSE_PORT    5ULL
#define SYS_IPC_RECV          7ULL
#define SYS_SHARED_REGION_MAP 18ULL
#define SYS_IPC_SEND_ASYNC    23ULL
#define SYS_IPC_TRY_RECV      24ULL
#define SYS_KEYBOARD_POLL     36ULL
#define SYS_GET_TICKS         38ULL

/* Error values are in the top 256 of the 64-bit address space */
#define KERN_ERR_THRESHOLD 0xFFFFFFFFFFFFFF00ULL
static inline bool is_err(uint64_t v) { return v >= KERN_ERR_THRESHOLD; }

static inline uint64_t sc0(uint64_t n)
{
    uint64_t r;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n)
        : "rcx","r11","rdi","rsi","rdx","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc1(uint64_t n, uint64_t a)
{
    uint64_t r;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n), "D"(a)
        : "rcx","r11","rsi","rdx","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc3(uint64_t n, uint64_t a, uint64_t b, uint64_t c)
{
    uint64_t r;
    register uint64_t rb __asm__("rsi") = b;
    register uint64_t rc __asm__("rdx") = c;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n), "D"(a), "r"(rb), "r"(rc)
        : "rcx","r11","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc4(uint64_t n, uint64_t a, uint64_t b,
                            uint64_t c, uint64_t d)
{
    uint64_t r;
    register uint64_t rb __asm__("rsi") = b;
    register uint64_t rc __asm__("rdx") = c;
    register uint64_t rd __asm__("r10") = d;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n), "D"(a), "r"(rb), "r"(rc), "r"(rd)
        : "rcx","r11","r8","r9","memory");
    return r;
}

static uint64_t ipc_create_port(void)
{
    uint64_t r = sc0(SYS_IPC_CREATE_PORT);
    return is_err(r) ? 0 : r;
}
static void ipc_close_port(uint64_t port) { sc1(SYS_IPC_CLOSE_PORT, port); }

static bool ipc_send(uint64_t port, const void *data, uint32_t len)
{
    uint64_t r = sc4(SYS_IPC_SEND_ASYNC, port, 0,
                     (uint64_t)(uintptr_t)data, (uint64_t)len);
    return !is_err(r);
}
static int ipc_try_recv(uint64_t port, void *buf, uint32_t len)
{
    uint64_t r = sc4(SYS_IPC_RECV, port,
                     (uint64_t)(uintptr_t)buf, (uint64_t)len, 1ULL);
    return is_err(r) ? -1 : (int)r;
}
static int ipc_recv_timeout(uint64_t port, void *buf, uint32_t len,
                             uint64_t timeout_ms)
{
    uint64_t r = sc4(SYS_IPC_RECV, port,
                     (uint64_t)(uintptr_t)buf, (uint64_t)len, timeout_ms);
    return is_err(r) ? -1 : (int)r;
}
static uintptr_t shared_map(uint64_t region_id)
{
    uint64_t r = sc3(SYS_SHARED_REGION_MAP, region_id, 0ULL, 3ULL);
    return is_err(r) ? 0 : (uintptr_t)r;
}

/* ==========================================================================
 * §2  IPC message protocol
 * ======================================================================== */

#define HDR_SIZE 16u

typedef struct __attribute__((packed)) {
    uint32_t version;
    uint32_t msg_type;
    uint32_t payload_size;
    uint32_t sequence;
} MsgHeader;

#define MSG_KEY_DOWN              1u
#define MSG_KEY_UP                2u
#define MSG_KEY_PRESS             3u
#define MSG_NS_LOOKUP           602u
#define MSG_NS_RESPONSE         610u
#define MSG_WM_REQUEST          800u
#define MSG_WM_RESPONSE         801u
#define MSG_WM_COMMIT           803u
#define MSG_QUIT                402u
#define MSG_TERMINATE_REQUEST   510u
#define WM_CREATE_WINDOW          1u
#define NAME_SERVICE_PORT         1ULL

static void hdr_init(MsgHeader *h, uint32_t type, uint32_t payload_size)
{
    h->version      = 1;
    h->msg_type     = type;
    h->payload_size = payload_size;
    h->sequence     = 0;
}

/* ==========================================================================
 * §3  Name service lookup
 * ======================================================================== */

static uint64_t ns_lookup(const char *name)
{
    int i;
    uint64_t reply = ipc_create_port();
    if (!reply) return 0;

    uint8_t buf[HDR_SIZE + 40];
    MsgHeader *h = (MsgHeader *)buf;
    hdr_init(h, MSG_NS_LOOKUP, 40);

    uint8_t *payload = buf + HDR_SIZE;
    memcpy(payload, &reply, 8);
    memset(payload + 8, 0, 32);
    {
        size_t nlen = strlen(name);
        if (nlen > 31) nlen = 31;
        memcpy(payload + 8, name, nlen);
    }

    ipc_send(NAME_SERVICE_PORT, buf, (uint32_t)sizeof(buf));

    uint8_t resp[HDR_SIZE + 8];
    uint64_t found = 0;
    for (i = 0; i < 10000 && !found; i++) {
        int n = ipc_try_recv(reply, resp, (uint32_t)sizeof(resp));
        if (n >= (int)(HDR_SIZE + 8)) {
            MsgHeader *rh = (MsgHeader *)resp;
            if (rh->msg_type == MSG_NS_RESPONSE)
                memcpy(&found, resp + HDR_SIZE, 8);
        }
        sc0(SYS_THREAD_YIELD);
    }
    ipc_close_port(reply);
    return found;
}

/* ==========================================================================
 * §4  WM window protocol
 * ======================================================================== */

typedef struct {
    uint32_t window_id;
    uint64_t region_id;
    uint32_t width;
    uint32_t height;
    uint32_t stride;
} WmWindowInfo;

static bool wm_create_window(uint64_t wm_port, uint64_t event_port,
                              uint32_t width, uint32_t height,
                              const char *title, WmWindowInfo *out)
{
    int i;
    uint32_t tlen = (uint32_t)strlen(title);
    uint32_t psz  = 4 + 8 + 4 + 4 + 4 + tlen;
    uint32_t msz  = HDR_SIZE + psz;
    uint8_t  msg[256];
    if (msz > sizeof(msg)) return false;

    {
        MsgHeader *h = (MsgHeader *)msg;
        hdr_init(h, MSG_WM_REQUEST, psz);
    }

    {
        uint8_t *p = msg + HDR_SIZE;
        uint32_t req_type = WM_CREATE_WINDOW;
        memcpy(p,  &req_type,   4); p += 4;
        memcpy(p,  &event_port, 8); p += 8;
        memcpy(p,  &width,      4); p += 4;
        memcpy(p,  &height,     4); p += 4;
        memcpy(p,  &tlen,       4); p += 4;
        memcpy(p,  title,       tlen);
    }

    ipc_send(wm_port, msg, msz);

    {
        uint8_t resp[HDR_SIZE + 28];
        for (i = 0; i < 10000; i++) {
            int n = ipc_try_recv(event_port, resp, (uint32_t)sizeof(resp));
            if (n >= (int)(HDR_SIZE + 24)) {
                MsgHeader *rh = (MsgHeader *)resp;
                if (rh->msg_type == MSG_WM_RESPONSE) {
                    uint8_t *rp = resp + HDR_SIZE;
                    memcpy(&out->window_id, rp,      4);
                    memcpy(&out->region_id, rp +  4, 8);
                    memcpy(&out->width,     rp + 12, 4);
                    memcpy(&out->height,    rp + 16, 4);
                    memcpy(&out->stride,    rp + 20, 4);
                    return true;
                }
            }
            sc0(SYS_THREAD_YIELD);
        }
    }
    return false;
}

static void wm_commit(uint64_t wm_port, uint32_t window_id)
{
    uint8_t msg[HDR_SIZE + 4];
    MsgHeader *h = (MsgHeader *)msg;
    hdr_init(h, MSG_WM_COMMIT, 4);
    memcpy(msg + HDR_SIZE, &window_id, 4);
    ipc_send(wm_port, msg, (uint32_t)sizeof(msg));
}

/* ==========================================================================
 * §5  Scancode Set 1 → doomkeys translation
 * ======================================================================== */

/*
 * Scancode Set 1 key codes (base set, no 0xE0 extended prefix handling).
 * Bit 7 of the raw byte from SYS_KEYBOARD_POLL signals key release;
 * bits 6:0 are the make code used here.
 */
static unsigned char scancode_to_doomkey(uint8_t sc)
{
    /* Number row and symbols above it: 0x02 = '1' .. 0x0B = '0', 0x0C = '-', 0x0D = '=' */
    static const unsigned char row_num[12] = {
        '1','2','3','4','5','6','7','8','9','0','-','='
    };
    /* QWERTY top row: 0x10 = 'q' .. 0x1B = ']' */
    static const unsigned char row_top[12] = {
        'q','w','e','r','t','y','u','i','o','p','[',']'
    };
    /* QWERTY home row: 0x1E = 'a' .. 0x26 = 'l' */
    static const unsigned char row_home[9] = {
        'a','s','d','f','g','h','j','k','l'
    };
    /* QWERTY bottom row: 0x2C = 'z' .. 0x35 = '/' */
    static const unsigned char row_bot[10] = {
        'z','x','c','v','b','n','m',',','.','/'
    };

    if (sc >= 0x02u && sc <= 0x0Du) return row_num[sc - 0x02u];
    if (sc >= 0x10u && sc <= 0x1Bu) return row_top[sc - 0x10u];
    if (sc >= 0x1Eu && sc <= 0x26u) return row_home[sc - 0x1Eu];
    if (sc >= 0x2Cu && sc <= 0x35u) return row_bot[sc - 0x2Cu];

    switch (sc) {
    /* Control */
    case 0x01: return KEY_ESCAPE;
    case 0x0E: return KEY_BACKSPACE;
    case 0x0F: return KEY_TAB;
    case 0x1C: return KEY_ENTER;
    case 0x1D: return KEY_FIRE;     /* fire */
    case 0x2A: return KEY_RSHIFT;   /* left shift */
    case 0x36: return KEY_RSHIFT;   /* right shift */
    case 0x38: return KEY_RALT;
    case 0x39: return KEY_USE;      /* spacebar (use) */
    case 0x3A: return KEY_CAPSLOCK;
    /* Function keys */
    case 0x3B: return KEY_F1;
    case 0x3C: return KEY_F2;
    case 0x3D: return KEY_F3;
    case 0x3E: return KEY_F4;
    case 0x3F: return KEY_F5;
    case 0x40: return KEY_F6;
    case 0x41: return KEY_F7;
    case 0x42: return KEY_F8;
    case 0x43: return KEY_F9;
    case 0x44: return KEY_F10;
    case 0x57: return KEY_F11;
    case 0x58: return KEY_F12;
    /* Lock keys */
    case 0x45: return KEY_NUMLOCK;
    case 0x46: return KEY_SCRLCK;
    /* Navigation cluster */
    case 0x47: return KEY_HOME;
    case 0x48: return KEY_UPARROW;
    case 0x49: return KEY_PGUP;
    case 0x4B: return KEY_LEFTARROW;
    case 0x4D: return KEY_RIGHTARROW;
    case 0x4F: return KEY_END;
    case 0x50: return KEY_DOWNARROW;
    case 0x51: return KEY_PGDN;
    case 0x52: return KEY_INS;
    case 0x53: return KEY_DEL;
    default:   return 0;
    }
}

/* ==========================================================================
 * §6  Module-level state
 * ======================================================================== */

static uint64_t  g_wm_port   = 0;
static uint64_t  g_event_port = 0;
static WmWindowInfo g_win;
static uint32_t *g_surface   = NULL;
static bool      g_should_quit = false;

/* Key event ring buffer */
#define KEY_BUF_SIZE 128
typedef struct {
    int           pressed;
    unsigned char doomKey;
} KeyEvent;

static KeyEvent  g_key_buf[KEY_BUF_SIZE];
static int       g_key_head = 0;   /* next read  */
static int       g_key_tail = 0;   /* next write */

static void key_push(int pressed, unsigned char dk)
{
    if (!dk) return;    /* ignore unmapped keys */
    int next = (g_key_tail + 1) % KEY_BUF_SIZE;
    if (next == g_key_head) return; /* buffer full — drop oldest entry */
    g_key_buf[g_key_tail].pressed = pressed;
    g_key_buf[g_key_tail].doomKey = dk;
    g_key_tail = next;
}

static bool key_pop(int *pressed, unsigned char *dk)
{
    if (g_key_head == g_key_tail) return false;
    *pressed = g_key_buf[g_key_head].pressed;
    *dk      = g_key_buf[g_key_head].doomKey;
    g_key_head = (g_key_head + 1) % KEY_BUF_SIZE;
    return true;
}

/* Drain all pending keyboard scancodes from the kernel into the ring buffer */
static void poll_keyboard(void)
{
    int i;
    for (i = 0; i < 32; i++) {
        uint8_t raw = 0;
        uint64_t r = sc1(SYS_KEYBOARD_POLL, (uint64_t)(uintptr_t)&raw);
        if (is_err(r)) break;                     /* EWOULDBLOCK → no more keys */
        {
            int pressed      = (raw & 0x80u) ? 0 : 1;
            uint8_t make     = raw & 0x7Fu;
            unsigned char dk = scancode_to_doomkey(make);
            key_push(pressed, dk);
        }
    }
}

/* Process one compositor IPC event; returns true if quit requested */
static bool handle_event(const uint8_t *buf, int len)
{
    if (len < (int)HDR_SIZE) return false;
    const MsgHeader *h = (const MsgHeader *)buf;
    switch (h->msg_type) {
    case MSG_QUIT:
    case MSG_TERMINATE_REQUEST:
        return true;
    case MSG_KEY_DOWN:
    case MSG_KEY_UP: {
        /*
         * Compositor forwards every key press and release via these messages.
         * payload[0] = scancode (PS/2 Set 1 make code, no 0x80 release bit)
         * payload[1] = ASCII character (0 for non-printable keys)
         * payload[2] = modifier flags
         *
         * We translate the scancode to a doomkey and push it into the ring
         * buffer. This is the only reliable keyboard path because the
         * compositor has already drained SYS_KEYBOARD_POLL.
         */
        int plen = len - (int)HDR_SIZE;
        if (plen >= 1) {
            uint8_t sc = buf[HDR_SIZE];
            int pressed = (h->msg_type == MSG_KEY_DOWN) ? 1 : 0;
            unsigned char dk = scancode_to_doomkey(sc);
            if (dk) key_push(pressed, dk);
        }
        break;
    }
    case MSG_KEY_PRESS: {
        /* Backward-compat: some paths send KeyPress only (no release).
         * ESC via this path triggers quit; other keys are ignored here
         * because they also arrive as MSG_KEY_DOWN above. */
        int plen = len - (int)HDR_SIZE;
        if (plen >= 1) {
            uint8_t sc = buf[HDR_SIZE];
            if (sc == 0x01u) return true; /* ESC */
        }
        break;
    }
    default:
        break;
    }
    return false;
}

/* ==========================================================================
 * §7  doomgeneric callbacks
 * ======================================================================== */

void DG_Init(void)
{
    printf("[doom] DG_Init: locating compositor...\n");

    g_wm_port = ns_lookup("compositor.wm");
    if (!g_wm_port) {
        printf("[doom] DG_Init: compositor.wm not found — running headless\n");
        return;
    }

    g_event_port = ipc_create_port();
    if (!g_event_port) {
        printf("[doom] DG_Init: failed to create event port\n");
        return;
    }

    if (!wm_create_window(g_wm_port, g_event_port,
                          DOOMGENERIC_RESX, DOOMGENERIC_RESY,
                          "DOOM", &g_win)) {
        printf("[doom] DG_Init: wm_create_window failed\n");
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }

    uintptr_t addr = shared_map(g_win.region_id);
    if (!addr) {
        printf("[doom] DG_Init: shared_map failed\n");
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }
    g_surface = (uint32_t *)addr;

    printf("[doom] DG_Init: window %u  surface %llu  %ux%u stride %u\n",
           g_win.window_id, (unsigned long long)g_win.region_id,
           g_win.width, g_win.height, g_win.stride);
}

void DG_DrawFrame(void)
{
    if (!g_surface) return;

    /* Blit the doomgeneric screen buffer into the compositor shared surface.
     *
     * doomgeneric rgba8888 format: B=byte0, G=byte1, R=byte2, A=byte3
     *   integer 0x00RRGGBB stored little-endian as [B,G,R,0]
     * AtomOS compositor surface: same 32-bit ARGB layout.
     *
     * The compositor may return a surface smaller than requested (due to
     * window decorations), so we clamp the copy to the actual surface
     * dimensions to avoid writing past the shared region.
     */
    {
        uint32_t copy_w = (uint32_t)DOOMGENERIC_RESX;
        uint32_t copy_h = (uint32_t)DOOMGENERIC_RESY;
        if (copy_w > g_win.width)  copy_w = g_win.width;
        if (copy_h > g_win.height) copy_h = g_win.height;

        const uint32_t *src = DG_ScreenBuffer;
        uint32_t       *dst = g_surface;
        uint32_t y;
        for (y = 0; y < copy_h; y++) {
            memcpy(dst, src, (size_t)copy_w * 4u);
            src += DOOMGENERIC_RESX;
            dst += g_win.stride;
        }
    }

    /* Present the frame */
    wm_commit(g_wm_port, g_win.window_id);

    /*
     * Drain all pending compositor events (key down/up, quit, etc.).
     *
     * NOTE: SYS_KEYBOARD_POLL is intentionally NOT called here. The
     * compositor registers itself as the IRQ 1 handler and drains the
     * kernel PS/2 ring buffer before Doom would see it. All keyboard
     * input arrives via MSG_KEY_DOWN / MSG_KEY_UP on the event port.
     */
    {
        uint8_t evbuf[256];
        for (;;) {
            int n = ipc_try_recv(g_event_port, evbuf, (uint32_t)sizeof(evbuf));
            if (n < (int)HDR_SIZE) break;
            if (handle_event(evbuf, n)) {
                g_should_quit = true;
                /* Inject a synthetic ESC key-press so doomgeneric's loop exits */
                key_push(1, KEY_ESCAPE);
                key_push(0, KEY_ESCAPE);
                break;
            }
        }
    }
}

uint32_t DG_GetTicksMs(void)
{
    /* SYS_GET_TICKS returns a 100 Hz counter → multiply by 10 for ms */
    return (uint32_t)(sc0(SYS_GET_TICKS) * 10ULL);
}

void DG_SleepMs(uint32_t ms)
{
    if (ms > 0)
        sc1(SYS_THREAD_SLEEP, (uint64_t)ms);
    else
        sc0(SYS_THREAD_YIELD);
}

int DG_GetKey(int *pressed, unsigned char *doomKey)
{
    return key_pop(pressed, doomKey) ? 1 : 0;
}

void DG_SetWindowTitle(const char *title)
{
    /* No dynamic window-title API in the current WM protocol — ignore. */
    (void)title;
}

/* ==========================================================================
 * §9  Stubs for libc functions not implemented in AtomOS libc
 * ======================================================================== */

/*
 * system() — run a shell command.
 * AtomOS has no shell; this is used by i_system.c only for the optional
 * Zenity error-box path, which is unreachable on our platform.
 */
int system(const char *cmd)
{
    (void)cmd;
    return -1;
}

/*
 * remove() — delete a file.
 * Used by g_game.c to clean up savegame temp files.
 * A stub returning failure is acceptable — it just means old saves
 * won't be deleted.
 */
int remove(const char *path)
{
    (void)path;
    return -1;
}

/* ==========================================================================
 * §8  main() entry point
 * ======================================================================== */

int main(void)
{
    /*
     * The WAD is placed at /apps/user/doom1.wad on the AtomOS FAT32 volume
     * by build.sh (copied from efi/apps/user/doom1.wad via mcopy).
     */
    const char *argv[] = {
        "doom",
        "-nogui",                         /* suppress ZenityErrorBox on error */
        "-iwad", "/apps/user/doom1.wad",
        NULL
    };
    doomgeneric_Create(4, (char **)argv);

    for (;;) {
        doomgeneric_Tick();
    }

    return 0;
}
