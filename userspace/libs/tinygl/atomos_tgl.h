/*
 * atomos_tgl.h — AtomOS-specific TinyGL integration helpers.
 *
 * Provides the compositor blit function that converts TinyGL's internal
 * 16-bit RGB565 framebuffer to the AtomOS compositor's 32-bit ARGB surface
 * (pixel format: 0x00RRGGBB stored as little-endian uint32_t).
 *
 * Usage pattern:
 *
 *   // 1. Allocate a 16-bit scratch buffer
 *   uint16_t *scratch = malloc(width * height * sizeof(uint16_t));
 *
 *   // 2. Create a TinyGL context pointing at the scratch buffer
 *   void *fbs[1] = { scratch };
 *   ostgl_context *ctx = ostgl_create_context(width, height, 16, fbs, 1);
 *   // glInit() is called inside ostgl_create_context automatically.
 *
 *   // 3. Render your scene with standard GL calls ...
 *   glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
 *   // ... draw ...
 *
 *   // 4. Blit to the compositor's 32-bit ARGB surface and commit
 *   tgl_blit_to_argb32(ctx, 0, compositor_surface, stride_in_pixels);
 *   wm_commit(...);
 */

#ifndef ATOMOS_TGL_H
#define ATOMOS_TGL_H

#include <stdint.h>
#include <GL/oscontext.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * tgl_blit_to_argb32 — blit TinyGL framebuffer to AtomOS compositor surface.
 *
 *   ctx           — context created with ostgl_create_context(w, h, 16, ...)
 *   buf_idx       — framebuffer index (0 for single-buffered use)
 *   dst           — pointer to the compositor's shared-surface memory
 *   dst_stride_px — surface stride in *pixels* (WmWindowInfo.stride)
 *
 * The function converts every pixel from RGB565 (TinyGL internal 16-bit) to
 * ARGB32 (0x00RRGGBB).  The destination width and height are taken from ctx.
 */
void tgl_blit_to_argb32(ostgl_context *ctx, int buf_idx,
                         uint32_t *dst, uint32_t dst_stride_px);

#ifdef __cplusplus
}
#endif

#endif /* ATOMOS_TGL_H */
