/*
 * tinygl_demo — AtomOS TinyGL 0.4.1 Gears demo
 *
 * Renders the classic OpenGL gears ("glgears") scene using TinyGL's
 * software rasteriser and presents each frame to the AtomOS compositor
 * via the WM IPC shared-surface protocol.
 *
 * Rendering pipeline
 * ──────────────────
 *   TinyGL (software GL) → 16-bit RGB565 scratch buffer
 *     → tgl_blit_to_argb32()
 *       → compositor 32-bit ARGB shared surface
 *         → wm_commit() → visible window
 *
 * Protocol stack (pure C, no Rust libraries):
 *   namesvc  (port 1) ─NsLookup──► compositor.wm port
 *   compositor.wm    ─WmRequest─► shared surface region
 *   shared region    ─blit──────► WmCommitFrame ──► window on screen
 *
 * Gears logic adapted from TinyGL examples/gears.c
 *   (original: 3-D gear wheels by Brian Paul, public domain)
 */

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>
#include <math.h>

/* TinyGL public headers */
#include <GL/gl.h>
#include <GL/oscontext.h>

/* AtomOS compositor blit helper */
#include "atomos_tgl.h"

/* ==========================================================================
 * §1  Kernel syscall wrappers
 * ======================================================================== */

#define SYS_THREAD_YIELD    0ULL
#define SYS_THREAD_EXIT     1ULL
#define SYS_THREAD_SLEEP    2ULL
#define SYS_IPC_CREATE_PORT 4ULL
#define SYS_IPC_CLOSE_PORT  5ULL
#define SYS_IPC_RECV        7ULL
#define SYS_SHARED_REGION_MAP 18ULL
#define SYS_IPC_SEND_ASYNC  23ULL
#define SYS_IPC_TRY_RECV    24ULL

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
        : "=a"(r) : "0"(n),"D"(a)
        : "rcx","r11","rsi","rdx","r8","r9","r10","memory");
    return r;
}
static inline uint64_t sc3(uint64_t n, uint64_t a, uint64_t b, uint64_t c)
{
    uint64_t r;
    register uint64_t rb __asm__("rsi") = b;
    register uint64_t rc __asm__("rdx") = c;
    __asm__ volatile("syscall"
        : "=a"(r) : "0"(n),"D"(a),"r"(rb),"r"(rc)
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
        : "=a"(r) : "0"(n),"D"(a),"r"(rb),"r"(rc),"r"(rd)
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
static int ipc_recv_timeout(uint64_t port, void *buf, uint32_t len, uint64_t timeout_ms)
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
    uint32_t version;      /* always 1 */
    uint32_t msg_type;
    uint32_t payload_size;
    uint32_t sequence;
} MsgHeader;

#define MSG_NS_LOOKUP    602u
#define MSG_NS_RESPONSE  610u
#define MSG_WM_REQUEST   800u
#define MSG_WM_RESPONSE  801u
#define MSG_WM_COMMIT    803u
#define MSG_QUIT                402u
#define MSG_TERMINATE_REQUEST  510u
#define MSG_KEY_PRESS          3u
#define WM_CREATE_WINDOW 1u
#define NAME_SERVICE_PORT 1ULL

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
    uint64_t reply = ipc_create_port();
    if (!reply) return 0;

    uint8_t buf[HDR_SIZE + 40];
    MsgHeader *h = (MsgHeader *)buf;
    hdr_init(h, MSG_NS_LOOKUP, 40);

    uint8_t *payload = buf + HDR_SIZE;
    memcpy(payload, &reply, 8);
    memset(payload + 8, 0, 32);
    size_t nlen = strlen(name);
    if (nlen > 31) nlen = 31;
    memcpy(payload + 8, name, nlen);

    ipc_send(NAME_SERVICE_PORT, buf, (uint32_t)sizeof(buf));

    uint8_t resp[HDR_SIZE + 8];
    uint64_t found = 0;
    for (int i = 0; i < 10000 && !found; i++) {
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
    uint32_t stride;  /* pixels per row */
} WmWindowInfo;

static bool wm_create_window(uint64_t wm_port, uint64_t event_port,
                              uint32_t width, uint32_t height,
                              const char *title, WmWindowInfo *out)
{
    uint32_t tlen = (uint32_t)strlen(title);
    uint32_t psz  = 4 + 8 + 4 + 4 + 4 + tlen;
    uint32_t msz  = HDR_SIZE + psz;
    uint8_t  msg[256];
    if (msz > sizeof(msg)) return false;

    MsgHeader *h = (MsgHeader *)msg;
    hdr_init(h, MSG_WM_REQUEST, psz);

    uint8_t *p = msg + HDR_SIZE;
    uint32_t req_type = WM_CREATE_WINDOW;
    memcpy(p,  &req_type,   4); p += 4;
    memcpy(p,  &event_port, 8); p += 8;
    memcpy(p,  &width,      4); p += 4;
    memcpy(p,  &height,     4); p += 4;
    memcpy(p,  &tlen,       4); p += 4;
    memcpy(p,  title,       tlen);

    ipc_send(wm_port, msg, msz);

    uint8_t resp[HDR_SIZE + 28];
    for (int i = 0; i < 10000; i++) {
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
 * §5  Gears rendering  (adapted from TinyGL examples/gears.c)
 *     Original: Brian Paul, public domain.
 * ======================================================================== */

#ifndef M_PI
#  define M_PI 3.14159265358979323846
#endif

static GLfloat view_rotx = 20.0f;
static GLfloat view_roty = 30.0f;
static GLfloat view_rotz =  0.0f;
static GLfloat angle = 0.0f;

/* Gear material colours — file-scope so both gears_init and gears_draw can use them */
static GLfloat gear_red[4]   = { 0.8f,  0.1f,  0.0f, 1.0f };
static GLfloat gear_green[4] = { 0.0f,  0.8f,  0.2f, 1.0f };
static GLfloat gear_blue[4]  = { 0.2f,  0.2f,  1.0f, 1.0f };

/*
 * gear() — draw a single gear wheel.
 *
 *   inner_radius — radius of hole at centre
 *   outer_radius — radius at centre of teeth
 *   width        — width of gear along Z axis
 *   teeth        — number of teeth
 *   tooth_depth  — radial depth of each tooth
 */
static void gear(GLfloat inner_radius, GLfloat outer_radius, GLfloat width,
                 GLint teeth, GLfloat tooth_depth)
{
    GLint   i;
    GLfloat r0, r1, r2;
    GLfloat a, da;
    GLfloat u, v, len;

    r0 = inner_radius;
    r1 = outer_radius - tooth_depth * 0.5f;
    r2 = outer_radius + tooth_depth * 0.5f;

    da = 2.0f * (GLfloat)M_PI / (GLfloat)teeth / 4.0f;

    glShadeModel(GL_FLAT);
    glNormal3f(0.0f, 0.0f, 1.0f);

    /* Front face */
    glBegin(GL_QUAD_STRIP);
    for (i = 0; i <= teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;
        glVertex3f(r0 * (GLfloat)cos(a), r0 * (GLfloat)sin(a),  width * 0.5f);
        glVertex3f(r1 * (GLfloat)cos(a), r1 * (GLfloat)sin(a),  width * 0.5f);
        glVertex3f(r0 * (GLfloat)cos(a), r0 * (GLfloat)sin(a),  width * 0.5f);
        glVertex3f(r1 * (GLfloat)cos(a + 3.0f*da),
                   r1 * (GLfloat)sin(a + 3.0f*da), width * 0.5f);
    }
    glEnd();

    /* Front sides of teeth */
    glBegin(GL_QUADS);
    da = 2.0f * (GLfloat)M_PI / (GLfloat)teeth / 4.0f;
    for (i = 0; i < teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;
        glVertex3f(r1*(GLfloat)cos(a),       r1*(GLfloat)sin(a),       width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+da),    r2*(GLfloat)sin(a+da),    width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+2.0f*da),r2*(GLfloat)sin(a+2.0f*da),width*0.5f);
        glVertex3f(r1*(GLfloat)cos(a+3.0f*da),r1*(GLfloat)sin(a+3.0f*da),width*0.5f);
    }
    glEnd();

    glNormal3f(0.0f, 0.0f, -1.0f);

    /* Back face */
    glBegin(GL_QUAD_STRIP);
    for (i = 0; i <= teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;
        glVertex3f(r1*(GLfloat)cos(a),        r1*(GLfloat)sin(a),        -width*0.5f);
        glVertex3f(r0*(GLfloat)cos(a),        r0*(GLfloat)sin(a),        -width*0.5f);
        glVertex3f(r1*(GLfloat)cos(a+3.0f*da),r1*(GLfloat)sin(a+3.0f*da),-width*0.5f);
        glVertex3f(r0*(GLfloat)cos(a),        r0*(GLfloat)sin(a),        -width*0.5f);
    }
    glEnd();

    /* Back sides of teeth */
    glBegin(GL_QUADS);
    da = 2.0f * (GLfloat)M_PI / (GLfloat)teeth / 4.0f;
    for (i = 0; i < teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;
        glVertex3f(r1*(GLfloat)cos(a+3.0f*da), r1*(GLfloat)sin(a+3.0f*da), -width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+2.0f*da), r2*(GLfloat)sin(a+2.0f*da), -width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+da),      r2*(GLfloat)sin(a+da),       -width*0.5f);
        glVertex3f(r1*(GLfloat)cos(a),         r1*(GLfloat)sin(a),          -width*0.5f);
    }
    glEnd();

    /* Outward faces of teeth */
    glBegin(GL_QUAD_STRIP);
    for (i = 0; i < teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;

        glNormal3f((GLfloat)cos(a), (GLfloat)sin(a), 0.0f);
        glVertex3f(r1*(GLfloat)cos(a), r1*(GLfloat)sin(a),  width*0.5f);
        glVertex3f(r1*(GLfloat)cos(a), r1*(GLfloat)sin(a), -width*0.5f);

        u   = r2*(GLfloat)cos(a+da) - r1*(GLfloat)cos(a);
        v   = r2*(GLfloat)sin(a+da) - r1*(GLfloat)sin(a);
        len = (GLfloat)sqrt((double)(u*u + v*v));
        u  /= len;  v /= len;
        glNormal3f(v, -u, 0.0f);
        glVertex3f(r2*(GLfloat)cos(a+da),    r2*(GLfloat)sin(a+da),     width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+da),    r2*(GLfloat)sin(a+da),    -width*0.5f);

        glNormal3f((GLfloat)cos(a+2.0f*da), (GLfloat)sin(a+2.0f*da), 0.0f);
        glVertex3f(r2*(GLfloat)cos(a+2.0f*da),r2*(GLfloat)sin(a+2.0f*da),  width*0.5f);
        glVertex3f(r2*(GLfloat)cos(a+2.0f*da),r2*(GLfloat)sin(a+2.0f*da), -width*0.5f);

        u   = r1*(GLfloat)cos(a+3.0f*da) - r2*(GLfloat)cos(a+2.0f*da);
        v   = r1*(GLfloat)sin(a+3.0f*da) - r2*(GLfloat)sin(a+2.0f*da);
        glNormal3f(v, -u, 0.0f);
        glVertex3f(r1*(GLfloat)cos(a+3.0f*da),r1*(GLfloat)sin(a+3.0f*da),  width*0.5f);
        glVertex3f(r1*(GLfloat)cos(a+3.0f*da),r1*(GLfloat)sin(a+3.0f*da), -width*0.5f);

        glNormal3f((GLfloat)cos(a), (GLfloat)sin(a), 0.0f);
    }
    glVertex3f(r1*(GLfloat)cos(0.0f), r1*(GLfloat)sin(0.0f),  width*0.5f);
    glVertex3f(r1*(GLfloat)cos(0.0f), r1*(GLfloat)sin(0.0f), -width*0.5f);
    glEnd();

    glShadeModel(GL_SMOOTH);

    /* Inside radius cylinder */
    glBegin(GL_QUAD_STRIP);
    for (i = 0; i <= teeth; i++) {
        a = (GLfloat)i * 2.0f * (GLfloat)M_PI / (GLfloat)teeth;
        glNormal3f(-(GLfloat)cos(a), -(GLfloat)sin(a), 0.0f);
        glVertex3f(r0*(GLfloat)cos(a), r0*(GLfloat)sin(a), -width*0.5f);
        glVertex3f(r0*(GLfloat)cos(a), r0*(GLfloat)sin(a),  width*0.5f);
    }
    glEnd();
}

/* Set up lighting and GL state.  The gear geometry is rendered directly each
 * frame (no display lists) to avoid TinyGL's list-corruption bug that
 * manifests as "list N: corrupt first_op_list" when glCallList is used. */
static void gears_init(void)
{
    static GLfloat pos[4] = { 5.0f, 5.0f, 10.0f, 0.0f };

    glLightfv(GL_LIGHT0, GL_POSITION, pos);
    glEnable(GL_CULL_FACE);
    glEnable(GL_LIGHTING);
    glEnable(GL_LIGHT0);
    glEnable(GL_DEPTH_TEST);
    glEnable(GL_NORMALIZE);
}

/* Set up projection and viewport for the given window dimensions. */
static void gears_reshape(int w, int h)
{
    GLfloat hf = (GLfloat)h / (GLfloat)w;
    glViewport(0, 0, (GLint)w, (GLint)h);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glFrustum(-1.0, 1.0, (double)(-hf), (double)hf, 5.0, 60.0);
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
    glTranslatef(0.0f, 0.0f, -40.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
}

/* Render one frame (no swap — caller handles blit/commit).
 * Geometry is submitted directly each frame; display lists are not used. */
static void gears_draw(void)
{
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);

    glPushMatrix();
    glRotatef(view_rotx, 1.0f, 0.0f, 0.0f);
    glRotatef(view_roty, 0.0f, 1.0f, 0.0f);
    glRotatef(view_rotz, 0.0f, 0.0f, 1.0f);

    glPushMatrix();
    glTranslatef(-3.0f, -2.0f, 0.0f);
    glRotatef(angle, 0.0f, 0.0f, 1.0f);
    glMaterialfv(GL_FRONT, GL_AMBIENT_AND_DIFFUSE, gear_red);
    gear(1.0f, 4.0f, 1.0f, 20, 0.7f);
    glPopMatrix();

    glPushMatrix();
    glTranslatef(3.1f, -2.0f, 0.0f);
    glRotatef(-2.0f * angle - 9.0f, 0.0f, 0.0f, 1.0f);
    glMaterialfv(GL_FRONT, GL_AMBIENT_AND_DIFFUSE, gear_green);
    gear(0.5f, 2.0f, 2.0f, 10, 0.7f);
    glPopMatrix();

    glPushMatrix();
    glTranslatef(-3.1f, 4.2f, 0.0f);
    glRotatef(-2.0f * angle - 25.0f, 0.0f, 0.0f, 1.0f);
    glMaterialfv(GL_FRONT, GL_AMBIENT_AND_DIFFUSE, gear_blue);
    gear(1.3f, 2.0f, 0.5f, 10, 0.7f);
    glPopMatrix();

    glPopMatrix();
}

/* ==========================================================================
 * §6  Key event helpers
 * ======================================================================== */

/* Scancode Set 1 scancodes for arrow keys (after 0xE0 prefix is consumed) */
#define KEY_ESC      0x01u
#define KEY_UP_SC    0x48u
#define KEY_DOWN_SC  0x50u
#define KEY_LEFT_SC  0x4Bu
#define KEY_RIGHT_SC 0x4Du
#define ASCII_ESC    27u

static bool handle_key(const uint8_t *payload, int plen)
{
    if (plen < 2) return false;          /* need at least scancode + char */
    uint8_t sc   = payload[0];
    uint8_t chr  = payload[1];

    if (chr == ASCII_ESC || sc == KEY_ESC) return true;  /* quit */

    if (sc == KEY_UP_SC)    view_rotx += 5.0f;
    if (sc == KEY_DOWN_SC)  view_rotx -= 5.0f;
    if (sc == KEY_LEFT_SC)  view_roty += 5.0f;
    if (sc == KEY_RIGHT_SC) view_roty -= 5.0f;

    return false;
}

/* ==========================================================================
 * §7  Entry point
 * ======================================================================== */

#define WIN_W 640u
#define WIN_H 480u
/* Target ~30 frames per second */
#define FRAME_INTERVAL_MS 33u
/* Angle increment per frame (2°/frame ≈ 60 rpm at 30 fps) */
#define ANGLE_STEP 2.0f

int main(void)
{
    printf("[tinygl_demo] TinyGL 0.4.1 Gears — starting\n");

    /* ------------------------------------------------------------------ */
    /* 1. Locate compositor WM port                                        */
    /* ------------------------------------------------------------------ */
    uint64_t wm_port = ns_lookup("compositor.wm");
    if (!wm_port) {
        printf("[tinygl_demo] compositor.wm not found — running headless\n");

        /* Headless mode: allocate a scratch buffer, render a few frames,
         * then exit so CI / boot testing still exercises the code path.  */
        const uint32_t hw = WIN_W, hh = WIN_H;
        uint16_t *scratch = (uint16_t *)malloc((size_t)hw * hh * 2u);
        if (!scratch) { printf("[tinygl_demo] malloc failed\n"); return 1; }
        memset(scratch, 0, (size_t)hw * hh * 2u);

        void     *fbs[1] = { scratch };
        ostgl_context *ctx = ostgl_create_context((int)hw, (int)hh, 16, fbs, 1);
        if (!ctx) { printf("[tinygl_demo] ostgl_create_context failed\n"); return 1; }

        gears_reshape((int)hw, (int)hh);
        gears_init();

        for (int f = 0; f < 60; f++) {
            angle += ANGLE_STEP;
            gears_draw();
        }
        printf("[tinygl_demo] headless: rendered 60 frames OK\n");

        ostgl_delete_context(ctx);
        free(scratch);
        return 0;
    }

    /* ------------------------------------------------------------------ */
    /* 2. Create IPC event port + compositor window                        */
    /* ------------------------------------------------------------------ */
    uint64_t event_port = ipc_create_port();
    if (!event_port) {
        printf("[tinygl_demo] failed to create event port\n");
        return 1;
    }

    WmWindowInfo win;
    if (!wm_create_window(wm_port, event_port,
                          WIN_W, WIN_H, "TinyGL Gears", &win)) {
        printf("[tinygl_demo] wm_create_window failed\n");
        ipc_close_port(event_port);
        return 1;
    }
    printf("[tinygl_demo] window %u  surface %llu  %ux%u stride %u\n",
           win.window_id,
           (unsigned long long)win.region_id,
           win.width, win.height, win.stride);

    /* ------------------------------------------------------------------ */
    /* 3. Map the compositor's shared surface (32-bit ARGB)                */
    /* ------------------------------------------------------------------ */
    uintptr_t surface_addr = shared_map(win.region_id);
    if (!surface_addr) {
        printf("[tinygl_demo] shared_map failed\n");
        ipc_close_port(event_port);
        return 1;
    }
    uint32_t *surface = (uint32_t *)surface_addr;

    /* ------------------------------------------------------------------ */
    /* 4. Allocate 16-bit RGB565 scratch buffer for TinyGL                 */
    /* ------------------------------------------------------------------ */
    uint32_t tw = win.width  & ~3u;   /* TinyGL requires multiple of 4  */
    uint32_t th = win.height;
    uint16_t *scratch = (uint16_t *)malloc((size_t)tw * th * 2u);
    if (!scratch) {
        printf("[tinygl_demo] malloc scratch failed\n");
        ipc_close_port(event_port);
        return 1;
    }
    /* Zero the pixel buffer before handing it to TinyGL.  Some TinyGL
     * versions read from pbuf during ZBuffer initialisation; an
     * uninitialised buffer can leave internal state in an undefined
     * condition. */
    memset(scratch, 0, (size_t)tw * th * 2u);

    /* ------------------------------------------------------------------ */
    /* 5. Initialise TinyGL context                                        */
    /* ------------------------------------------------------------------ */
    void *fbs[1] = { scratch };
    ostgl_context *ctx = ostgl_create_context((int)tw, (int)th, 16, fbs, 1);
    if (!ctx) {
        printf("[tinygl_demo] ostgl_create_context failed\n");
        free(scratch);
        ipc_close_port(event_port);
        return 1;
    }

    gears_reshape((int)tw, (int)th);
    gears_init();

    printf("[tinygl_demo] GL ready — entering render loop\n");

    /* ------------------------------------------------------------------ */
    /* 6. Render / event loop                                              */
    /* ------------------------------------------------------------------ */
    uint8_t evbuf[256];
    bool quit = false;

    while (!quit) {
        /* Advance animation angle */
        angle += ANGLE_STEP;
        if (angle >= 360.0f) angle -= 360.0f;

        /* Render the scene into TinyGL's internal 16-bit buffer */
        gears_draw();

        /* Blit RGB565 → ARGB32 into the compositor shared surface */
        tgl_blit_to_argb32(scratch, tw, th, tw, surface, win.stride);

        /* Tell compositor to composite and display the frame */
        wm_commit(wm_port, win.window_id);

        /* Block up to FRAME_INTERVAL_MS for an event — this serves as both
         * the frame-rate limiter and keeps event_port always drained so the
         * compositor never hits an IPC send timeout. */
        {
            int n = ipc_recv_timeout(event_port, evbuf,
                                     (uint32_t)sizeof(evbuf),
                                     (uint64_t)FRAME_INTERVAL_MS);
            if (n >= (int)HDR_SIZE) {
                MsgHeader *h = (MsgHeader *)evbuf;
                switch (h->msg_type) {
                case MSG_QUIT:
                case MSG_TERMINATE_REQUEST:
                    quit = true;
                    break;
                case MSG_KEY_PRESS: {
                    int plen = n - (int)HDR_SIZE;
                    if (plen > 0 && handle_key(evbuf + HDR_SIZE, plen))
                        quit = true;
                    break;
                }
                default:
                    break;
                }
            }
        }

        /* Drain any additional events that arrived during rendering */
        if (!quit) {
            for (;;) {
                int n = ipc_try_recv(event_port, evbuf, (uint32_t)sizeof(evbuf));
                if (n < (int)HDR_SIZE) break;

                MsgHeader *h = (MsgHeader *)evbuf;
                switch (h->msg_type) {
                case MSG_QUIT:
                case MSG_TERMINATE_REQUEST:
                    quit = true;
                    break;
                case MSG_KEY_PRESS: {
                    int plen = n - (int)HDR_SIZE;
                    if (plen > 0 && handle_key(evbuf + HDR_SIZE, plen))
                        quit = true;
                    break;
                }
                default:
                    break;
                }
                if (quit) break;
            }
        }
    }

    printf("[tinygl_demo] exiting\n");

    /* ------------------------------------------------------------------ */
    /* 7. Cleanup                                                          */
    /* ------------------------------------------------------------------ */
    ostgl_delete_context(ctx);
    free(scratch);
    ipc_close_port(event_port);
    return 0;
}
