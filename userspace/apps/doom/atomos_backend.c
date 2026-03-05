/*
 * atomos_backend.c — AtomOS backend para doomgeneric
 *
 * Implementa as 5 funções da plataforma exigidas pelo doomgeneric:
 *   DG_Init()          — cria janela e contexto TinyGL
 *   DG_DrawFrame()     — converte framebuffer do Doom → superfície do compositor
 *   DG_GetKey()        — retorna eventos de teclado da fila
 *   DG_SetWindowTitle()— armazena título (usado antes de DG_Init)
 *   DG_GetTicksMs()    — retorna milissegundos desde o boot
 *
 * Pipeline de renderização
 * ────────────────────────
 *   Doom software renderer → RGBA8888 (DG_ScreenBuffer, 320×200)
 *     → escala 2× nearest-neighbor → RGB565 (scratch buffer TinyGL, 640×400)
 *       → tgl_blit_to_argb32() → superfície ARGB32 compartilhada do compositor
 *         → wm_commit() → janela visível no desktop
 *
 * Protocolo de pilha (C puro, sem bibliotecas Rust):
 *   namesvc (porta 1) ─NsLookup──► porta compositor.wm
 *   compositor.wm    ─WmRequest─► região de superfície compartilhada
 *   região compartilhada ─blit──► WmCommitFrame ──► janela na tela
 *
 * Teclas Doom suportadas:
 *   Setas, WSAD, Ctrl, Shift, Alt, Espaço, Enter, Esc, F1-F12, Tab
 */

#include <stdint.h>
#include <stdbool.h>
#include <stddef.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>

/* TinyGL — contexto e blit helper do AtomOS */
#include <GL/oscontext.h>
#include "atomos_tgl.h"

/* doomgeneric API pública */
#include "doomgeneric.h"

/* ==========================================================================
 * §1  Wrappers de syscall do kernel
 * ======================================================================== */

#define SYS_THREAD_YIELD    0ULL
#define SYS_THREAD_EXIT     1ULL
#define SYS_THREAD_SLEEP    2ULL
#define SYS_IPC_CREATE_PORT 4ULL
#define SYS_IPC_CLOSE_PORT  5ULL
#define SYS_IPC_RECV        7ULL
#define SYS_GET_TICKS       38ULL
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
    uint64_t r = sc3(SYS_IPC_TRY_RECV, port,
                     (uint64_t)(uintptr_t)buf, (uint64_t)len);
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
 * §2  Protocolo de mensagem IPC
 * ======================================================================== */

#define HDR_SIZE 16u

typedef struct __attribute__((packed)) {
    uint32_t version;
    uint32_t msg_type;
    uint32_t payload_size;
    uint32_t sequence;
} MsgHeader;

/* Tipos de mensagem (mirror de libipc/messages.rs) */
#define MSG_KEY_DOWN            1u
#define MSG_KEY_UP              2u
#define MSG_KEY_PRESS           3u
#define MSG_NS_LOOKUP           602u
#define MSG_NS_RESPONSE         610u
#define MSG_WM_REQUEST          800u
#define MSG_WM_RESPONSE         801u
#define MSG_WM_COMMIT           803u
#define MSG_QUIT                402u
#define MSG_TERMINATE_REQUEST   510u
#define WM_CREATE_WINDOW        1u
#define NAME_SERVICE_PORT       1ULL

static void hdr_init(MsgHeader *h, uint32_t type, uint32_t payload_size)
{
    h->version      = 1;
    h->msg_type     = type;
    h->payload_size = payload_size;
    h->sequence     = 0;
}

/* ==========================================================================
 * §3  Lookup no serviço de nomes
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
 * §4  Protocolo de janela do WM
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
 * §5  Dimensões da janela e do Doom
 *
 * Doom renderiza internamente em 320×200 (resolução clássica).
 * A janela do compositor é 640×400 (escala 2× com relação de aspecto 16:10).
 * A largura do scratch buffer deve ser múltiplo de 4 (TinyGL).
 * ======================================================================== */

#define WIN_W  640u
#define WIN_H  400u

/* Resolução interna do Doom — deve coincidir com DOOMGENERIC_RESX/RESY */
#define DOOM_W DOOMGENERIC_RESX
#define DOOM_H DOOMGENERIC_RESY

/* ==========================================================================
 * §6  Fila de eventos de teclado
 * ======================================================================== */

#define KEY_QUEUE_SIZE 128

typedef struct {
    int           pressed;
    unsigned char doom_key;
} key_evt_t;

static key_evt_t g_key_queue[KEY_QUEUE_SIZE];
static int       g_key_head = 0;
static int       g_key_tail = 0;

static void key_enqueue(int pressed, unsigned char doom_key)
{
    int next = (g_key_tail + 1) % KEY_QUEUE_SIZE;
    if (next != g_key_head) {
        g_key_queue[g_key_tail].pressed  = pressed;
        g_key_queue[g_key_tail].doom_key = doom_key;
        g_key_tail = next;
    }
}

/* ==========================================================================
 * §7  Mapeamento de scancode PS/2 Set 1 → DOOM_KEY_*
 *
 * O AtomOS envia eventos de teclado com:
 *   payload[0] = scancode (Set 1, código make sem prefixo 0xE0)
 *   payload[1] = caractere ASCII (0 para teclas não-ASCII)
 *
 * Para teclas ASCII imprimíveis [32..126], o código Doom é o próprio
 * caractere (em minúsculo). Teclas especiais são mapeadas pelo scancode.
 * ======================================================================== */

static int scancode_to_doom_key(uint8_t sc, uint8_t ch)
{
    switch (sc) {
    /* Teclas de movimento */
    case 0x48: return DOOM_KEY_UP;
    case 0x50: return DOOM_KEY_DOWN;
    case 0x4B: return DOOM_KEY_LEFT;
    case 0x4D: return DOOM_KEY_RIGHT;

    /* Controle */
    case 0x01: return DOOM_KEY_ESCAPE;
    case 0x1C: return DOOM_KEY_ENTER;
    case 0x0F: return DOOM_KEY_TAB;
    case 0x0E: return DOOM_KEY_BACKSPACE;

    /* Modificadores */
    case 0x1D: return DOOM_KEY_RCTRL;    /* Ctrl esquerdo */
    case 0x2A: return DOOM_KEY_LSHIFT;
    case 0x36: return DOOM_KEY_RSHIFT;
    case 0x38: return DOOM_KEY_LALT;
    case 0x3A: return DOOM_KEY_CAPSLOCK;

    /* Teclas de função */
    case 0x3B: return DOOM_KEY_F1;
    case 0x3C: return DOOM_KEY_F2;
    case 0x3D: return DOOM_KEY_F3;
    case 0x3E: return DOOM_KEY_F4;
    case 0x3F: return DOOM_KEY_F5;
    case 0x40: return DOOM_KEY_F6;
    case 0x41: return DOOM_KEY_F7;
    case 0x42: return DOOM_KEY_F8;
    case 0x43: return DOOM_KEY_F9;
    case 0x44: return DOOM_KEY_F10;
    case 0x57: return DOOM_KEY_F11;
    case 0x58: return DOOM_KEY_F12;

    /* Espaço */
    case 0x39: return ' ';
    }

    /* ASCII imprimível [32..126]: usa o caractere em minúsculo */
    if (ch >= 32 && ch <= 126)
        return (ch >= 'A' && ch <= 'Z') ? (ch + 32) : (int)ch;

    return DOOM_KEY_UNKNOWN;
}

/* ==========================================================================
 * §8  Estado global do backend
 * ======================================================================== */

static uint64_t       g_wm_port    = 0;
static uint64_t       g_event_port = 0;
static WmWindowInfo   g_win;
static uint32_t      *g_surface    = NULL;  /* superfície ARGB32 do compositor */
static uint16_t      *g_scratch    = NULL;  /* buffer RGB565 para o TinyGL */
static ostgl_context *g_tgl_ctx    = NULL;
static char           g_title[128] = "DOOM";
static bool           g_init_done  = false;

/* ==========================================================================
 * §9  Conversão de framebuffer
 *
 * Doom gera pixels RGBA8888 (byte order: R=byte0, G=byte1, B=byte2, A=byte3).
 * Fazemos escala 2× para WIN_W×WIN_H e convertemos para RGB565 no scratch
 * buffer. A seguir, tgl_blit_to_argb32() converte RGB565 → ARGB32 e escreve
 * na superfície compartilhada do compositor.
 * ======================================================================== */

static void blit_doom_to_scratch(void)
{
    const uint32_t *src = (const uint32_t *)DG_ScreenBuffer;
    uint16_t       *dst = g_scratch;

    /* Escala nearest-neighbour: WIN_W×WIN_H ← DOOM_W×DOOM_H */
    for (uint32_t wy = 0; wy < WIN_H; wy++) {
        uint32_t sy = wy * (uint32_t)DOOM_H / WIN_H;
        const uint32_t *srow = src + sy * (uint32_t)DOOM_W;
        uint16_t       *drow = dst + wy * WIN_W;

        for (uint32_t wx = 0; wx < WIN_W; wx++) {
            uint32_t sx = wx * (uint32_t)DOOM_W / WIN_W;
            uint32_t p  = srow[sx];

            /*
             * doomgeneric RGBA8888 (byte order R,G,B,A):
             *   como uint32_t little-endian: bits 7:0 = R, 15:8 = G, 23:16 = B
             */
            uint32_t r = (p >>  0) & 0xFFu;
            uint32_t g = (p >>  8) & 0xFFu;
            uint32_t b = (p >> 16) & 0xFFu;

            /* RGB565: RRRRRGGGGGGBBBBB */
            drow[wx] = (uint16_t)(((r >> 3) << 11) | ((g >> 2) << 5) | (b >> 3));
        }
    }
}

/* ==========================================================================
 * §10  Processamento de eventos de IPC (não-bloqueante)
 * ======================================================================== */

static void process_events(void)
{
    uint8_t evbuf[256];

    for (;;) {
        int n = ipc_try_recv(g_event_port, evbuf, (uint32_t)sizeof(evbuf));
        if (n < (int)HDR_SIZE) break;

        MsgHeader *h = (MsgHeader *)evbuf;
        int plen = n - (int)HDR_SIZE;

        switch (h->msg_type) {
        case MSG_KEY_DOWN:
        case MSG_KEY_UP: {
            if (plen >= 2) {
                uint8_t sc  = evbuf[HDR_SIZE + 0];
                uint8_t chr = evbuf[HDR_SIZE + 1];
                int dk = scancode_to_doom_key(sc, chr);
                if (dk != DOOM_KEY_UNKNOWN)
                    key_enqueue(h->msg_type == MSG_KEY_DOWN ? 1 : 0,
                                (unsigned char)dk);
            }
            break;
        }
        case MSG_KEY_PRESS: {
            /* MSG_KEY_PRESS carrega apenas o caractere: trata como key-down */
            if (plen >= 2) {
                uint8_t sc  = evbuf[HDR_SIZE + 0];
                uint8_t chr = evbuf[HDR_SIZE + 1];
                int dk = scancode_to_doom_key(sc, chr);
                if (dk != DOOM_KEY_UNKNOWN) {
                    key_enqueue(1, (unsigned char)dk);
                    key_enqueue(0, (unsigned char)dk);
                }
            }
            break;
        }
        case MSG_QUIT:
        case MSG_TERMINATE_REQUEST:
            exit(0);
            break;
        default:
            break;
        }
    }
}

/* ==========================================================================
 * §11  Implementações das funções doomgeneric
 * ======================================================================== */

/*
 * DG_Init — inicializa janela do compositor e contexto TinyGL.
 *
 * Chamado por doomgeneric_create() depois de carregar o WAD e antes do
 * primeiro frame. Se o compositor não for encontrado (modo headless) o
 * Doom ainda roda mas não exibe nada.
 */
void DG_Init(void)
{
    printf("[doom] DG_Init — procurando compositor.wm\n");

    g_wm_port = ns_lookup("compositor.wm");
    if (!g_wm_port) {
        printf("[doom] compositor.wm não encontrado — modo headless\n");
        return;
    }

    /* Porta de eventos para receber respostas do WM e input */
    g_event_port = ipc_create_port();
    if (!g_event_port) {
        printf("[doom] falha ao criar porta de eventos\n");
        return;
    }

    /* Criar janela no compositor */
    if (!wm_create_window(g_wm_port, g_event_port,
                          WIN_W, WIN_H, g_title, &g_win)) {
        printf("[doom] wm_create_window falhou\n");
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }
    printf("[doom] janela %u  região %llu  %ux%u stride %u\n",
           g_win.window_id,
           (unsigned long long)g_win.region_id,
           g_win.width, g_win.height, g_win.stride);

    /* Mapear superfície compartilhada ARGB32 do compositor */
    uintptr_t surf_addr = shared_map(g_win.region_id);
    if (!surf_addr) {
        printf("[doom] shared_map falhou\n");
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }
    g_surface = (uint32_t *)surf_addr;

    /* Alocar scratch buffer RGB565 para o TinyGL */
    uint32_t tw = WIN_W & ~3u;   /* TinyGL exige múltiplo de 4 */
    g_scratch = (uint16_t *)malloc((size_t)tw * WIN_H * 2u);
    if (!g_scratch) {
        printf("[doom] malloc scratch falhou\n");
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }
    memset(g_scratch, 0, (size_t)tw * WIN_H * 2u);

    /* Inicializar contexto TinyGL apontando para o scratch buffer */
    void *fbs[1] = { g_scratch };
    g_tgl_ctx = ostgl_create_context((int)tw, (int)WIN_H, 16, fbs, 1);
    if (!g_tgl_ctx) {
        printf("[doom] ostgl_create_context falhou\n");
        free(g_scratch);
        g_scratch = NULL;
        ipc_close_port(g_event_port);
        g_event_port = 0;
        return;
    }

    g_init_done = true;
    printf("[doom] backend AtomOS pronto — janela %dx%d, Doom %dx%d\n",
           WIN_W, WIN_H, DOOM_W, DOOM_H);
}

/*
 * DG_DrawFrame — apresenta um frame do Doom na janela do AtomOS.
 *
 * DG_ScreenBuffer contém WIN_W×WIN_H pixels RGBA8888 após o render do Doom.
 * Convertemos para RGB565 (com escala) no scratch buffer, depois usamos
 * tgl_blit_to_argb32() para escrever na superfície do compositor e
 * chamamos wm_commit() para atualizar a tela.
 */
void DG_DrawFrame(void)
{
    if (!g_init_done) return;

    /* Processar eventos pendentes (teclado, fechar janela) */
    process_events();

    /* Converter + escalar framebuffer do Doom → scratch RGB565 */
    blit_doom_to_scratch();

    /* Converter scratch RGB565 → superfície ARGB32 do compositor */
    tgl_blit_to_argb32(g_scratch, WIN_W, WIN_H, WIN_W,
                       g_surface, g_win.stride);

    /* Sinalizar ao compositor que o frame está pronto */
    wm_commit(g_wm_port, g_win.window_id);
}

/*
 * DG_GetKey — devolve o próximo evento de teclado da fila.
 *
 * Retorna 1 se havia evento, 0 caso contrário.
 * *pressed = 1 para tecla pressionada, 0 para solta.
 * *doomKey = código DOOM_KEY_*.
 */
int DG_GetKey(int *pressed, unsigned char *doomKey)
{
    if (g_key_head == g_key_tail) return 0;
    *pressed = g_key_queue[g_key_head].pressed;
    *doomKey = g_key_queue[g_key_head].doom_key;
    g_key_head = (g_key_head + 1) % KEY_QUEUE_SIZE;
    return 1;
}

/*
 * DG_SetWindowTitle — armazena o título para uso em DG_Init().
 *
 * doomgeneric chama esta função com o título do WAD antes de DG_Init(),
 * então apenas salvamos o valor e o usamos na criação da janela.
 */
void DG_SetWindowTitle(const char *title)
{
    if (!title) return;
    size_t n = strlen(title);
    if (n >= sizeof(g_title)) n = sizeof(g_title) - 1;
    memcpy(g_title, title, n);
    g_title[n] = '\0';
}

/*
 * DG_GetTicksMs — retorna milissegundos desde o boot.
 *
 * Usa o syscall SYS_GET_TICKS (nº 38) que o kernel AtomOS expõe.
 */
uint32_t DG_GetTicksMs(void)
{
    return (uint32_t)sc0(SYS_GET_TICKS);
}

/*
 * DG_SleepMs — dorme o número especificado de milissegundos.
 *
 * Algumas versões do doomgeneric declaram esta função opcionalmente.
 * O Doom usa SYS_THREAD_SLEEP (nº 2) no AtomOS.
 */
void DG_SleepMs(uint32_t ms)
{
    sc1(SYS_THREAD_SLEEP, (uint64_t)ms);
}
