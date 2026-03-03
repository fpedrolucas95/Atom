/*
 * atomos_zb_blit.c — RGB565 → ARGB32 blit for the AtomOS compositor.
 *
 * TinyGL renders internally to a 16-bit RGB565 (5R 6G 5B) pixel buffer.
 * The AtomOS compositor expects 32-bit little-endian pixels with the layout
 *
 *   bits [31:24] = 0x00  (unused / alpha)
 *   bits [23:16] = R8
 *   bits [15: 8] = G8
 *   bits [ 7: 0] = B8
 *
 * i.e. the same as the CSS/Win32 COLORREF 0x00RRGGBB convention.
 *
 * The conversion expands each component to 8 bits using the standard
 * bit-replication trick (e.g. R5 → R8 = (r5 << 3) | (r5 >> 2)) which keeps
 * 0x00 at 0 and 0xFF at 0xFF.
 */

#include "atomos_tgl.h"
#include <stdint.h>

void tgl_blit_to_argb32(const uint16_t *src, uint32_t w, uint32_t h,
                         uint32_t src_stride_px,
                         uint32_t *dst, uint32_t dst_stride_px)
{
    for (uint32_t y = 0; y < h; y++) {
        const uint16_t *srow = src + y * src_stride_px;
        uint32_t       *drow = dst + y * dst_stride_px;

        for (uint32_t x = 0; x < w; x++) {
            uint32_t p = srow[x];

            /* Extract 5-/6-/5-bit components */
            uint32_t r = (p >> 11u) & 0x1Fu;
            uint32_t g = (p >>  5u) & 0x3Fu;
            uint32_t b =  p         & 0x1Fu;

            /* Expand to 8 bits via bit-replication */
            r = (r << 3u) | (r >> 2u);   /* 5 → 8 */
            g = (g << 2u) | (g >> 4u);   /* 6 → 8 */
            b = (b << 3u) | (b >> 2u);   /* 5 → 8 */

            drow[x] = (r << 16u) | (g << 8u) | b;
        }
    }
}
