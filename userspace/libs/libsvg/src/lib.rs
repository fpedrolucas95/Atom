#![no_std]
#![allow(clippy::needless_range_loop, clippy::comparison_chain)]

extern crate alloc;
use alloc::vec::Vec;
use atom_syscall::graphics::{Color, Framebuffer, SharedSurface};

// ═══════════════════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════════════════

const FP_ONE: i32 = 1024;
const SSAA_MAX_PIXELS: u32 = 512 * 512;
const MAX_SVG_ELEMENTS: usize = 10_000;
const MAX_PATH_POINTS: usize = 65_536;
const MAX_FILL_SUBPATHS: usize = 4_096;
const MAX_SCAN_EDGES: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_USE_DEPTH: u32 = 4;
const MAX_CSS_RULES: usize = 4_096;
const MAX_GRADIENTS: usize = 256;
const MAX_GRAD_STOPS: usize = 64;
const MAX_DEFS: usize = 2_048;
const MAX_ID_INDEX: usize = 4_096;
const MAX_CLIP_PATHS: usize = 256;
const MAX_PATTERNS: usize = 256;
const MAX_PATTERN_PIXELS: usize = 512 * 512;
const MAX_PATTERN_DEPTH: u32 = 4;

// ═══════════════════════════════════════════════════════════════════════════════
// Core types
// ═══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy)]
struct Tx { a: i32, b: i32, c: i32, d: i32, tx: i32, ty: i32 }

impl Tx {
    const fn identity() -> Self { Self { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx: 0, ty: 0 } }
}

#[derive(Clone)]
enum Paint {
    Solid(u32),
    LinearGrad { x1: i32, y1: i32, x2: i32, y2: i32, stops: Vec<(u8, u32)> },
    RadialGrad { cx: i32, cy: i32, fx: i32, fy: i32, rx: i32, ry: i32, stops: Vec<(u8, u32)> },
    Pattern { pixels: Vec<u32>, width: u32, height: u32, origin_x: i32, origin_y: i32 },
}

#[derive(Clone)]
enum GradientKind {
    Linear { x1: i32, y1: i32, x2: i32, y2: i32 },
    Radial { cx: i32, cy: i32, r: i32, fx: i32, fy: i32 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FillRule { EvenOdd, NonZero }

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineCap { Butt, Round, Square }

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineJoin { Miter, Round, Bevel }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvgError { InvalidDimensions, ComplexityLimitExceeded }

// ═══════════════════════════════════════════════════════════════════════════════
// Internal context
// ═══════════════════════════════════════════════════════════════════════════════

struct CssRule { class_name: Vec<u8>, prop: Vec<u8>, value: Vec<u8> }

struct GradientDef {
    id: Vec<u8>,
    kind: GradientKind,
    stops: Vec<(u8, u32)>,
    object_bbox: bool,
    grad_tx: Tx,
}

struct DefContent { id: Vec<u8>, start: usize, end: usize }

struct ElementDef {
    id: Vec<u8>,
    start: usize,
    content_start: usize,
    content_end: usize,
}

struct RenderContext {
    css_rules: Vec<CssRule>,
    gradients: Vec<GradientDef>,
    defs: Vec<DefContent>,
    id_index: Vec<DefContent>,
    clip_paths: Vec<ElementDef>,
    patterns: Vec<ElementDef>,
    viewport_w: i32,
    viewport_h: i32,
}

struct ScanEdge { y_top: i32, y_bot: i32, x: i64, dx: i64, dir: i32 }

// ═══════════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════════

/// Rendered SVG bitmap in ARGB format (0xAARRGGBB).
pub struct SvgBitmap {
    pub pixels: Vec<u32>,
    pub width: u32,
    pub height: u32,
}

impl SvgBitmap {
    /// Render SVG bytes to a bitmap. Returns error details on failure.
    pub fn render_result(svg: &[u8], out_w: u32, out_h: u32) -> Result<Self, SvgError> {
        if out_w == 0 || out_h == 0 { return Err(SvgError::InvalidDimensions); }
        let n = (out_w * out_h) as usize;
        let mut pixels = alloc::vec![0u32; n];
        let max_dim = out_w.max(out_h);
        let mut ssaa = if max_dim <= 64 { 4u32 } else if max_dim <= 128 { 2 } else { 1 };
        while ssaa > 1 {
            let hi = out_w.saturating_mul(ssaa).saturating_mul(out_h.saturating_mul(ssaa));
            if hi <= SSAA_MAX_PIXELS { break; }
            ssaa /= 2;
        }
        if ssaa <= 1 {
            render_svg(svg, out_w, out_h, &mut pixels)?;
        } else {
            let rw = out_w * ssaa;
            let rh = out_h * ssaa;
            let mut hi = alloc::vec![0u32; (rw * rh) as usize];
            render_svg(svg, rw, rh, &mut hi)?;
            downsample_premul(&hi, rw, rh, &mut pixels, out_w, out_h, ssaa);
        }
        Ok(SvgBitmap { pixels, width: out_w, height: out_h })
    }

    /// Render SVG bytes to a bitmap. Returns None on failure.
    pub fn render(svg: &[u8], out_w: u32, out_h: u32) -> Option<Self> {
        Self::render_result(svg, out_w, out_h).ok()
    }

    /// Alpha-blit onto a raw u32 ARGB slice.
    pub fn blit_to_slice(&self, dst: &mut [u32], dst_w: u32, dx: u32, dy: u32) {
        for py in 0..self.height {
            for px in 0..self.width {
                let p = self.pixels[(py * self.width + px) as usize];
                if (p >> 24) == 0 { continue; }
                let idx = ((dy + py) * dst_w + (dx + px)) as usize;
                if idx < dst.len() { dst[idx] = alpha_blend_over(dst[idx], p); }
            }
        }
    }

    /// Alpha-blit onto a Framebuffer.
    pub fn blit_fb(&self, fb: &Framebuffer, dx: u32, dy: u32) {
        for py in 0..self.height {
            for px in 0..self.width {
                let p = self.pixels[(py * self.width + px) as usize];
                let a = (p >> 24) as u8;
                if a == 0 { continue; }
                let c = Color::new((p >> 16) as u8, (p >> 8) as u8, p as u8);
                if a == 255 { fb.draw_pixel(dx + px, dy + py, c); }
                else { fb.fill_rect_alpha(dx + px, dy + py, 1, 1, c, a); }
            }
        }
    }

    /// Alpha-blit onto a SharedSurface.
    pub fn blit_surface(&self, surface: &SharedSurface, dx: u32, dy: u32) {
        for py in 0..self.height {
            for px in 0..self.width {
                let p = self.pixels[(py * self.width + px) as usize];
                let a = (p >> 24) as u8;
                if a == 0 { continue; }
                let c = Color::new((p >> 16) as u8, (p >> 8) as u8, p as u8);
                if a == 255 { surface.draw_pixel(dx + px, dy + py, c); }
                else { surface.fill_rect_alpha(dx + px, dy + py, 1, 1, c, a); }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// SSAA downsampling
// ═══════════════════════════════════════════════════════════════════════════════

fn downsample_premul(
    src: &[u32], src_w: u32, _src_h: u32,
    dst: &mut [u32], dst_w: u32, dst_h: u32, factor: u32,
) {
    let samples = (factor * factor) as u64;
    for y in 0..dst_h {
        for x in 0..dst_w {
            let (mut sa, mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64, 0u64);
            for oy in 0..factor {
                for ox in 0..factor {
                    let si = ((y * factor + oy) * src_w + (x * factor + ox)) as usize;
                    if si >= src.len() { continue; }
                    let p = src[si];
                    let a = ((p >> 24) & 0xFF) as u64;
                    sa += a;
                    sr += ((p >> 16) & 0xFF) as u64 * a;
                    sg += ((p >> 8) & 0xFF) as u64 * a;
                    sb += (p & 0xFF) as u64 * a;
                }
            }
            let di = (y * dst_w + x) as usize;
            if di >= dst.len() { continue; }
            let aa = (sa + samples / 2) / samples;
            if aa == 0 { dst[di] = 0; continue; }
            let r = ((sr * 255 + sa / 2) / sa).min(255) as u32;
            let g = ((sg * 255 + sa / 2) / sa).min(255) as u32;
            let b = ((sb * 255 + sa / 2) / sa).min(255) as u32;
            dst[di] = (aa.min(255) as u32) << 24 | r << 16 | g << 8 | b;
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Main renderer
// ═══════════════════════════════════════════════════════════════════════════════

fn render_svg(svg: &[u8], out_w: u32, out_h: u32, px: &mut [u32]) -> Result<(), SvgError> {
    let (vbx, vby, raw_vbw, raw_vbh) = parse_viewbox(svg);
    let vbw = if raw_vbw > 0 { raw_vbw } else { out_w as i32 * FP_ONE };
    let vbh = if raw_vbh > 0 { raw_vbh } else { out_h as i32 * FP_ONE };
    let ctx = RenderContext {
        css_rules: parse_css_rules(svg),
        gradients: parse_all_gradients(svg, vbw, vbh),
        defs: collect_defs(svg),
        id_index: collect_id_index(svg),
        clip_paths: collect_element_defs(svg, b"clipPath", MAX_CLIP_PATHS),
        patterns: collect_element_defs(svg, b"pattern", MAX_PATTERNS),
        viewport_w: vbw,
        viewport_h: vbh,
    };
    let par = parse_preserve_aspect_ratio(svg);
    let root_tx = compute_root_tx(vbx, vby, vbw, vbh, out_w as i32, out_h as i32, par);
    render_range(svg, 0, svg.len(), px, out_w as i32, out_h as i32,
                 root_tx, 255, (0, 0, 0, 255), &ctx, 0)
}

fn render_range(
    svg: &[u8], start: usize, end: usize,
    px: &mut [u32], ow: i32, oh: i32,
    init_tx: Tx, init_opacity: u8, init_color: (u8, u8, u8, u8),
    rctx: &RenderContext, use_depth: u32,
) -> Result<(), SvgError> {
    let mut tstk = [Tx::identity(); MAX_DEPTH];
    let mut ostk = [255u8; MAX_DEPTH];
    let mut cstk = [(0u8, 0u8, 0u8, 255u8); MAX_DEPTH];
    tstk[0] = init_tx; ostk[0] = init_opacity; cstk[0] = init_color;
    let mut depth: usize = 1;
    let mut skip_depth: usize = 0;
    let mut elem_count: usize = 0;
    let mut i = start;
    while i < end {
        if svg[i] != b'<' { i += 1; continue; }
        i += 1;
        if i >= end { break; }
        // Comments <!-- ... -->
        if i + 2 < end && svg[i] == b'!' && svg[i + 1] == b'-' && svg[i + 2] == b'-' {
            i += 3;
            while i + 2 < end {
                if svg[i] == b'-' && svg[i + 1] == b'-' && svg[i + 2] == b'>' { i += 3; break; }
                i += 1;
            }
            continue;
        }
        // PIs and DOCTYPE
        if svg[i] == b'?' || svg[i] == b'!' {
            while i < end && svg[i] != b'>' { i += 1; }
            i += 1; continue;
        }
        // Close tag
        if svg[i] == b'/' {
            i += 1;
            let ts = i;
            while i < end && !is_ws(svg[i]) && svg[i] != b'>' { i += 1; }
            let ctag = &svg[ts..i];
            while i < end && svg[i] != b'>' { i += 1; }
            i += 1;
            if skip_depth > 0 { skip_depth -= 1; continue; }
            if (ctag == b"g" || ctag == b"svg" || ctag == b"symbol") && depth > 1 { depth -= 1; }
            continue;
        }
        // Open tag name
        let ts = i;
        while i < end && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        let tag = &svg[ts..i];
        elem_count += 1;
        if elem_count > MAX_SVG_ELEMENTS { return Err(SvgError::ComplexityLimitExceeded); }
        // Attrs slice
        let as0 = i;
        let (tag_end, self_close) = find_tag_end(svg, i, end);
        let attrs = &svg[as0..tag_end];
        i = tag_end.saturating_add(1);
        if skip_depth > 0 { if !self_close { skip_depth += 1; } continue; }
        // Skip defs and invisible tags
        let is_defs = tag == b"defs" || tag == b"filter" || tag == b"clipPath"
            || tag == b"mask" || tag == b"pattern" || tag == b"metadata";
        if is_defs || is_display_none(attrs, rctx) || is_visibility_hidden(attrs, rctx) {
            if !self_close { skip_depth = 1; }
            continue;
        }
        // Skip gradient definitions (already pre-parsed)
        if tag == b"linearGradient" || tag == b"radialGradient" {
            if !self_close { skip_depth = 1; }
            continue;
        }
        let cur = tstk[depth - 1];
        let inh_op = if depth > 0 { ostk[depth - 1] } else { 255 };
        let inh_col = if depth > 0 { cstk[depth - 1] } else { (0, 0, 0, 255) };
        match tag {
            b"svg" if !self_close => {
                if let Some(clip_id) = clip_path_id(attrs, rctx) {
                    let close_end = find_closing_tag_end(svg, i, tag);
                    let content_end = close_end.saturating_sub(tag.len() + 3);
                    render_clipped_subtree(
                        svg,
                        i,
                        content_end.min(close_end),
                        px,
                        ow,
                        oh,
                        cur,
                        inh_op,
                        inh_col,
                        rctx,
                        clip_id,
                        (0, 0, rctx.viewport_w, rctx.viewport_h),
                        use_depth,
                    )?;
                    i = close_end;
                    continue;
                }
                if depth >= MAX_DEPTH { return Err(SvgError::ComplexityLimitExceeded); }
                tstk[depth] = cur;
                ostk[depth] = inh_op;
                cstk[depth] = inh_col;
                depth += 1;
            }
            b"g" if !self_close => {
                let gtx = compose_tx(cur, parse_transform_attr(attrs, rctx));
                let gop = mul_alpha(inh_op, group_opacity(attrs, rctx));
                let gcol = resolve_current_color(attrs, inh_col, rctx);
                if let Some(clip_id) = clip_path_id(attrs, rctx) {
                    let close_end = find_closing_tag_end(svg, i, tag);
                    let content_end = close_end.saturating_sub(tag.len() + 3);
                    render_clipped_subtree(
                        svg,
                        i,
                        content_end.min(close_end),
                        px,
                        ow,
                        oh,
                        gtx,
                        gop,
                        gcol,
                        rctx,
                        clip_id,
                        (0, 0, rctx.viewport_w, rctx.viewport_h),
                        use_depth,
                    )?;
                    i = close_end;
                    continue;
                }
                if depth >= MAX_DEPTH { return Err(SvgError::ComplexityLimitExceeded); }
                tstk[depth] = gtx;
                ostk[depth] = gop;
                cstk[depth] = gcol;
                depth += 1;
            }
            b"symbol" if !self_close => {
                // symbol is not rendered directly
                if !self_close { skip_depth = 1; }
            }
            b"use" => {
                if use_depth < MAX_USE_DEPTH {
                    handle_use(svg, attrs, px, ow, oh, cur, inh_op, inh_col, rctx, use_depth)?;
                }
            }
            b"rect" | b"circle" | b"ellipse" | b"line" | b"path" | b"polygon" | b"polyline" => {
                render_graphic_element(svg, tag, attrs, px, ow, oh, cur, inh_op, inh_col, rctx);
            }
            _ => {}
        }
    }
    Ok(())
}

fn render_graphic_element(
    svg: &[u8], tag: &[u8], attrs: &[u8],
    px: &mut [u32], ow: i32, oh: i32,
    ctx: Tx, inh_op: u8, inh_col: (u8, u8, u8, u8),
    rctx: &RenderContext,
) {
    if let Some(clip_id) = clip_path_id(attrs, rctx) {
        let bbox = match element_bbox_svg(tag, attrs, rctx) {
            Some(b) => b,
            None => {
                render_graphic_element_raw(svg, tag, attrs, px, ow, oh, ctx, inh_op, inh_col, rctx);
                return;
            }
        };
        let n = (ow.max(0) as usize).saturating_mul(oh.max(0) as usize);
        if n == 0 { return; }
        let mut tmp = alloc::vec![0u32; n];
        render_graphic_element_raw(svg, tag, attrs, &mut tmp, ow, oh, ctx, inh_op, inh_col, rctx);
        let mut mask = alloc::vec![0u32; n];
        if render_clip_path_mask(svg, clip_id, &mut mask, ow, oh, ctx, rctx, bbox) {
            blend_masked(px, &tmp, &mask);
        } else {
            blend_masked(px, &tmp, &tmp);
        }
        return;
    }
    render_graphic_element_raw(svg, tag, attrs, px, ow, oh, ctx, inh_op, inh_col, rctx);
}

fn render_clipped_subtree(
    svg: &[u8], start: usize, end: usize,
    px: &mut [u32], ow: i32, oh: i32,
    ctx: Tx, opacity: u8, color: (u8, u8, u8, u8),
    rctx: &RenderContext, clip_id: &[u8], bbox: (i32, i32, i32, i32),
    use_depth: u32,
) -> Result<(), SvgError> {
    let n = (ow.max(0) as usize).saturating_mul(oh.max(0) as usize);
    if n == 0 { return Ok(()); }
    let mut tmp = alloc::vec![0u32; n];
    render_range(svg, start, end, &mut tmp, ow, oh, ctx, opacity, color, rctx, use_depth)?;
    let mut mask = alloc::vec![0u32; n];
    if render_clip_path_mask(svg, clip_id, &mut mask, ow, oh, ctx, rctx, bbox) {
        blend_masked(px, &tmp, &mask);
    } else {
        blend_masked(px, &tmp, &tmp);
    }
    Ok(())
}

fn render_graphic_element_raw(
    svg: &[u8], tag: &[u8], attrs: &[u8],
    px: &mut [u32], ow: i32, oh: i32,
    ctx: Tx, inh_op: u8, inh_col: (u8, u8, u8, u8),
    rctx: &RenderContext,
) {
    match tag {
        b"rect" => elem_rect(svg, px, ow, oh, attrs, ctx, inh_op, inh_col, rctx),
        b"circle" => elem_circle(svg, px, ow, oh, attrs, ctx, inh_op, inh_col, rctx),
        b"ellipse" => elem_ellipse(svg, px, ow, oh, attrs, ctx, inh_op, inh_col, rctx),
        b"line" => elem_line(svg, px, ow, oh, attrs, ctx, inh_op, inh_col, rctx),
        b"path" => elem_path(svg, px, ow, oh, attrs, ctx, inh_op, inh_col, rctx),
        b"polygon" => elem_poly(svg, px, ow, oh, attrs, ctx, true, inh_op, inh_col, rctx),
        b"polyline" => elem_poly(svg, px, ow, oh, attrs, ctx, false, inh_op, inh_col, rctx),
        _ => {}
    }
}

fn handle_use(
    svg: &[u8], attrs: &[u8],
    px: &mut [u32], ow: i32, oh: i32,
    parent_tx: Tx, opacity: u8, color: (u8, u8, u8, u8),
    rctx: &RenderContext, use_depth: u32,
) -> Result<(), SvgError> {
    let href = get_attr(attrs, b"href", rctx).or_else(|| get_attr(attrs, b"xlink:href", rctx));
    let href = match href { Some(v) => v, None => return Ok(()), };
    if href.is_empty() || href[0] != b'#' { return Ok(()); }
    let ref_id = &href[1..];
    for d in &rctx.defs {
        if d.id == ref_id {
            let ux = attr_len(attrs, b"x", LengthAxis::Horizontal, rctx);
            let uy = attr_len(attrs, b"y", LengthAxis::Vertical, rctx);
            let local = parse_transform_attr(attrs, rctx);
            let translate = Tx { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx: ux, ty: uy };
            let tx = compose_tx(parent_tx, compose_tx(local, translate));
            let op = mul_alpha(opacity, group_opacity(attrs, rctx));
            return render_range(svg, d.start, d.end, px, ow, oh, tx, op, color, rctx, use_depth + 1);
        }
    }
    if let Some(d) = rctx.id_index.iter().find(|d| d.id == ref_id) {
        let ux = attr_len(attrs, b"x", LengthAxis::Horizontal, rctx);
        let uy = attr_len(attrs, b"y", LengthAxis::Vertical, rctx);
        let local = parse_transform_attr(attrs, rctx);
        let translate = Tx { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx: ux, ty: uy };
        let tx = compose_tx(parent_tx, compose_tx(local, translate));
        let op = mul_alpha(opacity, group_opacity(attrs, rctx));
        return render_range(svg, d.start, d.end, px, ow, oh, tx, op, color, rctx, use_depth + 1);
    }
    Ok(())
}

fn collect_id_index(svg: &[u8]) -> Vec<DefContent> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < svg.len() && out.len() < MAX_ID_INDEX {
        if svg[i] != b'<' { i += 1; continue; }
        let tag_start = i;
        i += 1;
        if i >= svg.len() || svg[i] == b'/' || svg[i] == b'!' || svg[i] == b'?' {
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1; continue;
        }
        let ns = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        let tag_name = &svg[ns..i];
        let a0 = i;
        let (tag_end, sc) = find_tag_end(svg, i, svg.len());
        let a = &svg[a0..tag_end];
        i = tag_end.saturating_add(1);
        if let Some(id) = get_attr_raw(a, b"id") {
            let end = if sc { i } else { find_closing_tag_end(svg, i, tag_name) };
            out.push(DefContent { id: trim_ascii(id).to_vec(), start: tag_start, end });
        }
    }
    out
}

fn find_closing_tag_end(svg: &[u8], start: usize, tag_name: &[u8]) -> usize {
    let mut depth = 1usize;
    let mut i = start;
    while i < svg.len() && depth > 0 {
        if svg[i] != b'<' { i += 1; continue; }
        i += 1;
        if i >= svg.len() { break; }
        if svg[i] == b'/' {
            i += 1;
            let ts = i;
            while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' { i += 1; }
            if &svg[ts..i] == tag_name { depth -= 1; }
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1;
            continue;
        }
        if svg[i] == b'!' || svg[i] == b'?' {
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1; continue;
        }
        let ns = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        let tn = &svg[ns..i];
        let (tag_end, sc) = find_tag_end(svg, i, svg.len());
        i = tag_end.saturating_add(1);
        if tn == tag_name && !sc { depth += 1; }
    }
    i
}

fn find_tag_end(svg: &[u8], mut i: usize, end: usize) -> (usize, bool) {
    let mut quote = 0u8;
    let mut last_non_ws = 0u8;
    while i < end {
        let b = svg[i];
        if quote != 0 {
            if b == quote { quote = 0; }
            i += 1;
            continue;
        }
        if b == b'\'' || b == b'"' {
            quote = b;
            i += 1;
            continue;
        }
        if b == b'>' { return (i, last_non_ws == b'/'); }
        if !is_ws(b) { last_non_ws = b; }
        i += 1;
    }
    (end, false)
}

// ═══════════════════════════════════════════════════════════════════════════════
// Element renderers
// ═══════════════════════════════════════════════════════════════════════════════

fn elem_rect(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let x = attr_len(attrs, b"x", LengthAxis::Horizontal, rctx);
    let y = attr_len(attrs, b"y", LengthAxis::Vertical, rctx);
    let w = attr_len(attrs, b"width", LengthAxis::Horizontal, rctx);
    let h = attr_len(attrs, b"height", LengthAxis::Vertical, rctx);
    if w <= 0 || h <= 0 { return; }
    let bbox = (x, y, x + w, y + h);
    let (x0, y0) = tx_to_px(ctx, x, y);
    let (x1, y1) = tx_to_px(ctx, x + w, y + h);
    if let Some(paint) = fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, bbox) {
        fill_rect_paint(px, ow as u32, oh as u32, x0, y0, x1, y1, &paint);
    }
    if let Some((paint, sw)) = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx) {
        rect_outline_paint(px, ow as u32, oh as u32, x0, y0, x1, y1, sw, &paint);
    }
}

fn elem_circle(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let cx = attr_len(attrs, b"cx", LengthAxis::Horizontal, rctx);
    let cy = attr_len(attrs, b"cy", LengthAxis::Vertical, rctx);
    let r = attr_len(attrs, b"r", LengthAxis::Normalized, rctx);
    if r <= 0 { return; }
    let bbox = (cx - r, cy - r, cx + r, cy + r);
    let (pcx, pcy) = tx_to_px(ctx, cx, cy);
    let pr = (fp_mul(r, tx_scale_x(ctx)).abs().max(fp_mul(r, tx_scale_y(ctx)).abs())) / FP_ONE;
    if let Some(paint) = fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, bbox) {
        fill_circle_paint(px, ow as u32, oh as u32, pcx, pcy, pr, &paint);
    }
    if let Some((paint, sw)) = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx) {
        circle_outline_paint(px, ow as u32, oh as u32, pcx, pcy, pr, sw, &paint);
    }
}

fn elem_ellipse(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let cx = attr_len(attrs, b"cx", LengthAxis::Horizontal, rctx);
    let cy = attr_len(attrs, b"cy", LengthAxis::Vertical, rctx);
    let rx = attr_len(attrs, b"rx", LengthAxis::Horizontal, rctx);
    let ry = attr_len(attrs, b"ry", LengthAxis::Vertical, rctx);
    if rx <= 0 || ry <= 0 { return; }
    let bbox = (cx - rx, cy - ry, cx + rx, cy + ry);
    let (pcx, pcy) = tx_to_px(ctx, cx, cy);
    let prx = fp_mul(rx, tx_scale_x(ctx)).abs() / FP_ONE;
    let pry = fp_mul(ry, tx_scale_y(ctx)).abs() / FP_ONE;
    if let Some(paint) = fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, bbox) {
        fill_ellipse_paint(px, ow as u32, oh as u32, pcx, pcy, prx, pry, &paint);
    }
    if let Some((paint, sw)) = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx) {
        ellipse_outline_paint(px, ow as u32, oh as u32, pcx, pcy, prx, pry, sw, &paint);
    }
}

fn elem_line(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let x1 = attr_len(attrs, b"x1", LengthAxis::Horizontal, rctx);
    let y1 = attr_len(attrs, b"y1", LengthAxis::Vertical, rctx);
    let x2 = attr_len(attrs, b"x2", LengthAxis::Horizontal, rctx);
    let y2 = attr_len(attrs, b"y2", LengthAxis::Vertical, rctx);
    let (px1, py1) = tx_to_px(ctx, x1, y1);
    let (px2, py2) = tx_to_px(ctx, x2, y2);
    let bbox = (
        x1.min(x2),
        y1.min(y2),
        x1.max(x2),
        y1.max(y2),
    );
    if let Some((paint, sw)) = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx) {
        let _ = bbox;
        let cap = stroke_linecap_attr(attrs, rctx);
        thick_line(px, ow as u32, oh as u32, px1, py1, px2, py2, &paint, sw);
        if cap == LineCap::Round {
            let hw = sw / 2;
            fill_circle_paint(px, ow as u32, oh as u32, px1, py1, hw, &paint);
            fill_circle_paint(px, ow as u32, oh as u32, px2, py2, hw, &paint);
        }
    }
}

fn elem_poly(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    closed: bool, inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let raw = match get_attr(attrs, b"points", rctx) { Some(v) => v, None => return };
    let pts = parse_points_list(raw, ctx);
    if pts.is_empty() { return; }
    let bbox = points_bbox_fp(raw, ctx);
    if let Some(paint) = fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, bbox) {
        if pts.len() >= 3 {
            let subs = [pts.clone()];
            let rule = fill_rule_attr(attrs, rctx);
            scanline_fill(px, ow as u32, oh as u32, &subs, rule, &paint);
        }
    }
    if let Some((paint, sw)) = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx) {
        let cap = stroke_linecap_attr(attrs, rctx);
        let join = stroke_linejoin_attr(attrs, rctx);
        stroke_polyline(px, ow as u32, oh as u32, &pts, closed, &paint, sw, cap, join);
    }
}

fn elem_path(
    svg: &[u8], px: &mut [u32], ow: i32, oh: i32, attrs: &[u8], ctx: Tx,
    inh_op: u8, inh_col: (u8, u8, u8, u8), rctx: &RenderContext,
) {
    let d_val = match get_attr(attrs, b"d", rctx) { Some(v) => v, None => return };
    let d = d_val;
    let fill_p = fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, (0,0,0,0)); // bbox computed later
    let stroke_p = stroke_paint_info(svg, attrs, inh_op, inh_col, rctx, ctx);
    let has_fill = fill_p.is_some();
    let has_stroke = stroke_p.is_some();
    if !has_fill && !has_stroke { return; }

    let rule = fill_rule_attr(attrs, rctx);
    let cap = stroke_linecap_attr(attrs, rctx);
    let join = stroke_linejoin_attr(attrs, rctx);

    // Parse path data into subpaths
    let (fill_subs, stroke_subs, closed_flags) = parse_path_data(d, ctx);

    // Compute actual bbox for gradient from untransformed path geometry.
    let (bbox_subs, _, _) = parse_path_data(d, Tx::identity());
    let bbox = compute_subpaths_bbox_from_points(&bbox_subs);
    let fill_p = if has_fill {
        fill_paint(svg, attrs, inh_op, inh_col, rctx, ctx, bbox)
    } else { None };

    if let Some(ref paint) = fill_p {
        if !fill_subs.is_empty() {
            scanline_fill(px, ow as u32, oh as u32, &fill_subs, rule, paint);
        }
    }
    if let Some((ref paint, sw)) = stroke_p {
        for (si, sub) in stroke_subs.iter().enumerate() {
            let c = si < closed_flags.len() && closed_flags[si];
            stroke_polyline(px, ow as u32, oh as u32, sub, c, paint, sw, cap, join);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Path data parser
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_path_data(d: &[u8], ctx: Tx) -> (Vec<Vec<(i32, i32)>>, Vec<Vec<(i32, i32)>>, Vec<bool>) {
    let mut fill_subs: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut stroke_subs: Vec<Vec<(i32, i32)>> = Vec::new();
    let mut closed_flags: Vec<bool> = Vec::new();
    let mut sub: Vec<(i32, i32)> = Vec::new();
    let mut cx: i32 = 0; let mut cy: i32 = 0;
    let mut sx: i32 = 0; let mut sy: i32 = 0;
    let mut last_c2x: i32 = 0; let mut last_c2y: i32 = 0; let mut has_lc: bool = false;
    let mut last_qx: i32 = 0; let mut last_qy: i32 = 0; let mut has_lq: bool = false;
    let mut pt_count: usize = 0;

    let flush_sub = |sub: &mut Vec<(i32, i32)>, closed: bool,
                     fill_subs: &mut Vec<Vec<(i32, i32)>>,
                     stroke_subs: &mut Vec<Vec<(i32, i32)>>,
                     closed_flags: &mut Vec<bool>| {
        if sub.is_empty() { return; }
        if sub.len() >= 3 && fill_subs.len() < MAX_FILL_SUBPATHS {
            fill_subs.push(sub.clone());
        }
        stroke_subs.push(sub.clone());
        closed_flags.push(closed);
    };

    let mut i = 0usize;
    let mut last_cmd = 0u8;
    while i < d.len() {
        skip_sep(d, &mut i);
        if i >= d.len() { break; }
        if pt_count > MAX_PATH_POINTS || fill_subs.len() > MAX_FILL_SUBPATHS { break; }

        let b = d[i];
        let cmd = if b.is_ascii_alphabetic() { i += 1; b } else { last_cmd };
        if cmd == 0 { i += 1; continue; }
        last_cmd = cmd;

        match cmd {
            b'M' => {
                flush_sub(&mut sub, false, &mut fill_subs, &mut stroke_subs, &mut closed_flags);
                sub.clear();
                let x = pn_fp(d, &mut i); let y = pn_fp(d, &mut i);
                cx = x; cy = y; sx = cx; sy = cy;
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                last_cmd = b'L'; has_lc = false; has_lq = false;
            }
            b'm' => {
                flush_sub(&mut sub, false, &mut fill_subs, &mut stroke_subs, &mut closed_flags);
                sub.clear();
                let dx = pn_fp(d, &mut i); let dy = pn_fp(d, &mut i);
                cx += dx; cy += dy; sx = cx; sy = cy;
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                last_cmd = b'l'; has_lc = false; has_lq = false;
            }
            b'L' => {
                let x = pn_fp(d, &mut i); let y = pn_fp(d, &mut i);
                cx = x; cy = y;
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'l' => {
                let dx = pn_fp(d, &mut i); let dy = pn_fp(d, &mut i);
                cx += dx; cy += dy;
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'H' => {
                cx = pn_fp(d, &mut i);
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'h' => {
                cx += pn_fp(d, &mut i);
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'V' => {
                cy = pn_fp(d, &mut i);
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'v' => {
                cy += pn_fp(d, &mut i);
                sub.push(tx_to_px(ctx, cx, cy)); pt_count += 1;
                has_lc = false; has_lq = false;
            }
            b'C' => {
                let x1 = pn_fp(d, &mut i); let y1 = pn_fp(d, &mut i);
                let x2 = pn_fp(d, &mut i); let y2 = pn_fp(d, &mut i);
                let x3 = pn_fp(d, &mut i); let y3 = pn_fp(d, &mut i);
                subdivide_cubic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), tx_to_px(ctx, x3, y3), 10);
                cx = x3; cy = y3; last_c2x = x2; last_c2y = y2; has_lc = true; has_lq = false;
                pt_count += 8;
            }
            b'c' => {
                let dx1 = pn_fp(d, &mut i); let dy1 = pn_fp(d, &mut i);
                let dx2 = pn_fp(d, &mut i); let dy2 = pn_fp(d, &mut i);
                let dx3 = pn_fp(d, &mut i); let dy3 = pn_fp(d, &mut i);
                let (x1, y1) = (cx + dx1, cy + dy1);
                let (x2, y2) = (cx + dx2, cy + dy2);
                let (x3, y3) = (cx + dx3, cy + dy3);
                subdivide_cubic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), tx_to_px(ctx, x3, y3), 10);
                last_c2x = x2; last_c2y = y2; cx = x3; cy = y3;
                has_lc = true; has_lq = false; pt_count += 8;
            }
            b'S' => {
                let x2 = pn_fp(d, &mut i); let y2 = pn_fp(d, &mut i);
                let x3 = pn_fp(d, &mut i); let y3 = pn_fp(d, &mut i);
                let (x1, y1) = if has_lc { (2 * cx - last_c2x, 2 * cy - last_c2y) } else { (cx, cy) };
                subdivide_cubic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), tx_to_px(ctx, x3, y3), 10);
                last_c2x = x2; last_c2y = y2; cx = x3; cy = y3;
                has_lc = true; has_lq = false; pt_count += 8;
            }
            b's' => {
                let dx2 = pn_fp(d, &mut i); let dy2 = pn_fp(d, &mut i);
                let dx3 = pn_fp(d, &mut i); let dy3 = pn_fp(d, &mut i);
                let (x1, y1) = if has_lc { (2 * cx - last_c2x, 2 * cy - last_c2y) } else { (cx, cy) };
                let (x2, y2) = (cx + dx2, cy + dy2);
                let (x3, y3) = (cx + dx3, cy + dy3);
                subdivide_cubic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), tx_to_px(ctx, x3, y3), 10);
                last_c2x = x2; last_c2y = y2; cx = x3; cy = y3;
                has_lc = true; has_lq = false; pt_count += 8;
            }
            b'Q' => {
                let x1 = pn_fp(d, &mut i); let y1 = pn_fp(d, &mut i);
                let x2 = pn_fp(d, &mut i); let y2 = pn_fp(d, &mut i);
                subdivide_quadratic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), 10);
                last_qx = x1; last_qy = y1; cx = x2; cy = y2;
                has_lq = true; has_lc = false; pt_count += 4;
            }
            b'q' => {
                let dx1 = pn_fp(d, &mut i); let dy1 = pn_fp(d, &mut i);
                let dx2 = pn_fp(d, &mut i); let dy2 = pn_fp(d, &mut i);
                let (x1, y1) = (cx + dx1, cy + dy1);
                let (x2, y2) = (cx + dx2, cy + dy2);
                subdivide_quadratic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), 10);
                last_qx = x1; last_qy = y1; cx = x2; cy = y2;
                has_lq = true; has_lc = false; pt_count += 4;
            }
            b'T' => {
                let x2 = pn_fp(d, &mut i); let y2 = pn_fp(d, &mut i);
                let (x1, y1) = if has_lq { (2 * cx - last_qx, 2 * cy - last_qy) } else { (cx, cy) };
                subdivide_quadratic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), 10);
                last_qx = x1; last_qy = y1; cx = x2; cy = y2;
                has_lq = true; has_lc = false; pt_count += 4;
            }
            b't' => {
                let dx2 = pn_fp(d, &mut i); let dy2 = pn_fp(d, &mut i);
                let (x1, y1) = if has_lq { (2 * cx - last_qx, 2 * cy - last_qy) } else { (cx, cy) };
                let (x2, y2) = (cx + dx2, cy + dy2);
                subdivide_quadratic(&mut sub, tx_to_px(ctx, cx, cy),
                    tx_to_px(ctx, x1, y1), tx_to_px(ctx, x2, y2), 10);
                last_qx = x1; last_qy = y1; cx = x2; cy = y2;
                has_lq = true; has_lc = false; pt_count += 4;
            }
            b'A' => {
                let rx = pn_fp(d, &mut i); let ry = pn_fp(d, &mut i);
                let xa = pn_fp(d, &mut i);
                let la = pn_fp(d, &mut i) != 0; let sw = pn_fp(d, &mut i) != 0;
                let x = pn_fp(d, &mut i); let y = pn_fp(d, &mut i);
                append_arc_points(&mut sub, ctx, cx, cy, rx, ry, xa, la, sw, x, y);
                cx = x; cy = y; has_lc = false; has_lq = false; pt_count += 16;
            }
            b'a' => {
                let rx = pn_fp(d, &mut i); let ry = pn_fp(d, &mut i);
                let xa = pn_fp(d, &mut i);
                let la = pn_fp(d, &mut i) != 0; let sw = pn_fp(d, &mut i) != 0;
                let dx = pn_fp(d, &mut i); let dy = pn_fp(d, &mut i);
                let (x, y) = (cx + dx, cy + dy);
                append_arc_points(&mut sub, ctx, cx, cy, rx, ry, xa, la, sw, x, y);
                cx = x; cy = y; has_lc = false; has_lq = false; pt_count += 16;
            }
            b'Z' | b'z' => {
                cx = sx; cy = sy;
                flush_sub(&mut sub, true, &mut fill_subs, &mut stroke_subs, &mut closed_flags);
                sub.clear();
                has_lc = false; has_lq = false;
            }
            _ => { i += 1; }
        }
    }
    if !sub.is_empty() {
        flush_sub(&mut sub, false, &mut fill_subs, &mut stroke_subs, &mut closed_flags);
    }
    (fill_subs, stroke_subs, closed_flags)
}

fn compute_subpaths_bbox_from_points(subs: &[Vec<(i32, i32)>]) -> (i32, i32, i32, i32) {
    let mut minx = i32::MAX; let mut miny = i32::MAX;
    let mut maxx = i32::MIN; let mut maxy = i32::MIN;
    for sub in subs {
        for &(x, y) in sub {
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x);
            maxy = maxy.max(y);
        }
    }
    if minx > maxx { (0, 0, 0, 0) } else { (minx, miny, maxx, maxy) }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Curve subdivision
// ═══════════════════════════════════════════════════════════════════════════════

fn subdivide_cubic(out: &mut Vec<(i32, i32)>, p0: (i32, i32), p1: (i32, i32), p2: (i32, i32), p3: (i32, i32), depth: u32) {
    if depth == 0 || cubic_flat(p0, p1, p2, p3) { out.push(p3); return; }
    let a = mid_pt(p0, p1); let b = mid_pt(p1, p2); let c = mid_pt(p2, p3);
    let d = mid_pt(a, b); let e = mid_pt(b, c); let f = mid_pt(d, e);
    subdivide_cubic(out, p0, a, d, f, depth - 1);
    subdivide_cubic(out, f, e, c, p3, depth - 1);
}

fn subdivide_quadratic(out: &mut Vec<(i32, i32)>, p0: (i32, i32), p1: (i32, i32), p2: (i32, i32), depth: u32) {
    if depth == 0 || quad_flat(p0, p1, p2) { out.push(p2); return; }
    let a = mid_pt(p0, p1); let b = mid_pt(p1, p2); let c = mid_pt(a, b);
    subdivide_quadratic(out, p0, a, c, depth - 1);
    subdivide_quadratic(out, c, b, p2, depth - 1);
}

#[inline] fn mid_pt(a: (i32, i32), b: (i32, i32)) -> (i32, i32) {
    (((a.0 as i64 + b.0 as i64) / 2) as i32, ((a.1 as i64 + b.1 as i64) / 2) as i32)
}

fn cubic_flat(p0: (i32, i32), p1: (i32, i32), p2: (i32, i32), p3: (i32, i32)) -> bool {
    let l2 = line_len_sq(p0, p3).max(1);
    let tol = FP_ONE as i64; let t2l = tol * tol * l2;
    pt_line_dist_sq(p0, p3, p1) <= t2l && pt_line_dist_sq(p0, p3, p2) <= t2l
}

fn quad_flat(p0: (i32, i32), p1: (i32, i32), p2: (i32, i32)) -> bool {
    let l2 = line_len_sq(p0, p2).max(1);
    let tol = FP_ONE as i64;
    pt_line_dist_sq(p0, p2, p1) <= tol * tol * l2
}

fn pt_line_dist_sq(a: (i32, i32), b: (i32, i32), p: (i32, i32)) -> i64 {
    let dx = (b.0 - a.0) as i64; let dy = (b.1 - a.1) as i64;
    let px = (p.0 - a.0) as i64; let py = (p.1 - a.1) as i64;
    let c = px * dy - py * dx; c * c
}

fn line_len_sq(a: (i32, i32), b: (i32, i32)) -> i64 {
    let dx = (b.0 - a.0) as i64; let dy = (b.1 - a.1) as i64; dx * dx + dy * dy
}

// ═══════════════════════════════════════════════════════════════════════════════
// Arc implementation
// ═══════════════════════════════════════════════════════════════════════════════

fn append_arc_points(
    out: &mut Vec<(i32, i32)>, ctx: Tx,
    x0: i32, y0: i32, rx_in: i32, ry_in: i32, x_axis_deg: i32,
    large_arc: bool, sweep: bool, x1: i32, y1: i32,
) {
    if x0 == x1 && y0 == y1 { return; }
    let mut rx = rx_in.abs(); let mut ry = ry_in.abs();
    if rx == 0 || ry == 0 { out.push(tx_to_px(ctx, x1, y1)); return; }
    let rot_fp = ((x_axis_deg as i64) * (TRIG_LUT_SIZE as i64) / 360) as i32;
    let cos_phi = lut_cos(rot_fp * FP_ONE);
    let sin_phi = lut_sin(rot_fp * FP_ONE);
    let mx = ((x0 as i64 - x1 as i64) / 2) as i32;
    let my = ((y0 as i64 - y1 as i64) / 2) as i32;
    let x0p = fp_mul(cos_phi, mx) + fp_mul(sin_phi, my);
    let y0p = -fp_mul(sin_phi, mx) + fp_mul(cos_phi, my);
    // Scale radii if needed
    let check = ((x0p as i64 * x0p as i64) / (rx as i64 * rx as i64).max(1)
        + (y0p as i64 * y0p as i64) / (ry as i64 * ry as i64).max(1)) as i32;
    if check > FP_ONE {
        let s = isqrt64((check as i64 * FP_ONE as i64) / (FP_ONE as i64)).max(1) as i32;
        rx = fp_mul(rx, s); ry = fp_mul(ry, s);
    }
    let rx2 = (rx as i64) * (rx as i64);
    let ry2 = (ry as i64) * (ry as i64);
    let xp2 = (x0p as i64) * (x0p as i64);
    let yp2 = (y0p as i64) * (y0p as i64);
    let num = (rx2 * ry2 - rx2 * yp2 - ry2 * xp2).max(0);
    let den = (rx2 * yp2 + ry2 * xp2).max(1);
    let sq = isqrt64(num * FP_ONE as i64 * FP_ONE as i64 / den);
    let sign = if large_arc == sweep { -1i64 } else { 1 };
    let cxp = (sign * sq * (rx as i64) * (y0p as i64) / (ry as i64).max(1) / FP_ONE as i64) as i32;
    let cyp = (sign * -sq * (ry as i64) * (x0p as i64) / (rx as i64).max(1) / FP_ONE as i64) as i32;
    let ccx = fp_mul(cos_phi, cxp) - fp_mul(sin_phi, cyp) + ((x0 as i64 + x1 as i64) / 2) as i32;
    let ccy = fp_mul(sin_phi, cxp) + fp_mul(cos_phi, cyp) + ((y0 as i64 + y1 as i64) / 2) as i32;
    let sa = angle_from_vec(
        fp_div(x0p - cxp, rx), fp_div(y0p - cyp, ry));
    let ea = angle_from_vec(
        fp_div(-x0p - cxp, rx), fp_div(-y0p - cyp, ry));
    let mut da = ea - sa;
    let half_period = ANGLE_PERIOD_FP / 2;
    if sweep { while da < 0 { da += ANGLE_PERIOD_FP; } if !large_arc && da > half_period { da -= ANGLE_PERIOD_FP; } }
    else { while da > 0 { da -= ANGLE_PERIOD_FP; } if !large_arc && da < -half_period { da += ANGLE_PERIOD_FP; } }
    let steps = arc_steps(da, rx, ry, ctx);
    for step in 1..=steps {
        let angle = sa + (da as i64 * step as i64 / steps as i64) as i32;
        let cos_t = lut_cos(angle); let sin_t = lut_sin(angle);
        let ex = fp_mul(rx, cos_t); let ey = fp_mul(ry, sin_t);
        let xr = fp_mul(cos_phi, ex) - fp_mul(sin_phi, ey) + ccx;
        let yr = fp_mul(sin_phi, ex) + fp_mul(cos_phi, ey) + ccy;
        let pt = if step == steps { tx_to_px(ctx, x1, y1) } else { tx_to_px(ctx, xr, yr) };
        out.push(pt);
    }
}

fn arc_steps(da: i32, rx: i32, ry: i32, ctx: Tx) -> i32 {
    let abs_da = (da as i64).unsigned_abs();
    if abs_da == 0 { return 1; }
    let sx = tx_scale_x(ctx).abs().max(1);
    let rpx = fp_mul(rx.abs().max(ry.abs()), sx).abs().max(1) as i64;
    let arc = rpx * 6283 * abs_da as i64 / (ANGLE_PERIOD_FP as i64 * FP_ONE as i64);
    let steps = (arc / 750).max(8).min(128) as i32;
    steps
}

fn fp_div(a: i32, b: i32) -> i32 {
    if b == 0 { return 0; }
    ((a as i64 * FP_ONE as i64) / b as i64) as i32
}

// ═══════════════════════════════════════════════════════════════════════════════
// Trig LUT
// ═══════════════════════════════════════════════════════════════════════════════

const TRIG_LUT_SIZE: usize = 1024;
const TRIG_LUT_MASK: usize = TRIG_LUT_SIZE - 1;
const TRIG_QUARTER: usize = TRIG_LUT_SIZE / 4;
const ANGLE_PERIOD_FP: i32 = TRIG_LUT_SIZE as i32 * FP_ONE;

static TRIG_LUT: [i32; TRIG_LUT_SIZE] = [
        0,    6,   13,   19,   25,   31,   38,   44,   50,   57,   63,   69,   75,   82,   88,   94,
      100,  107,  113,  119,  125,  132,  138,  144,  150,  156,  163,  169,  175,  181,  187,  194,
      200,  206,  212,  218,  224,  230,  237,  243,  249,  255,  261,  267,  273,  279,  285,  291,
      297,  303,  309,  315,  321,  327,  333,  339,  345,  351,  357,  363,  369,  374,  380,  386,
      392,  398,  403,  409,  415,  421,  426,  432,  438,  443,  449,  455,  460,  466,  472,  477,
      483,  488,  494,  499,  505,  510,  516,  521,  526,  532,  537,  543,  548,  553,  558,  564,
      569,  574,  579,  584,  590,  595,  600,  605,  610,  615,  620,  625,  630,  635,  640,  645,
      650,  654,  659,  664,  669,  674,  678,  683,  688,  692,  697,  702,  706,  711,  715,  720,
      724,  729,  733,  737,  742,  746,  750,  755,  759,  763,  767,  771,  775,  779,  784,  788,
      792,  796,  799,  803,  807,  811,  815,  819,  822,  826,  830,  834,  837,  841,  844,  848,
      851,  855,  858,  862,  865,  868,  872,  875,  878,  882,  885,  888,  891,  894,  897,  900,
      903,  906,  909,  912,  915,  917,  920,  923,  926,  928,  931,  934,  936,  939,  941,  944,
      946,  948,  951,  953,  955,  958,  960,  962,  964,  966,  968,  970,  972,  974,  976,  978,
      980,  982,  983,  985,  987,  989,  990,  992,  993,  995,  996,  998,  999, 1000, 1002, 1003,
     1004, 1006, 1007, 1008, 1009, 1010, 1011, 1012, 1013, 1014, 1015, 1016, 1016, 1017, 1018, 1018,
     1019, 1020, 1020, 1021, 1021, 1022, 1022, 1022, 1023, 1023, 1023, 1024, 1024, 1024, 1024, 1024,
     1024, 1024, 1024, 1024, 1024, 1024, 1023, 1023, 1023, 1022, 1022, 1022, 1021, 1021, 1020, 1020,
     1019, 1018, 1018, 1017, 1016, 1016, 1015, 1014, 1013, 1012, 1011, 1010, 1009, 1008, 1007, 1006,
     1004, 1003, 1002, 1000,  999,  998,  996,  995,  993,  992,  990,  989,  987,  985,  983,  982,
      980,  978,  976,  974,  972,  970,  968,  966,  964,  962,  960,  958,  955,  953,  951,  948,
      946,  944,  941,  939,  936,  934,  931,  928,  926,  923,  920,  917,  915,  912,  909,  906,
      903,  900,  897,  894,  891,  888,  885,  882,  878,  875,  872,  868,  865,  862,  858,  855,
      851,  848,  844,  841,  837,  834,  830,  826,  822,  819,  815,  811,  807,  803,  799,  796,
      792,  788,  784,  779,  775,  771,  767,  763,  759,  755,  750,  746,  742,  737,  733,  729,
      724,  720,  715,  711,  706,  702,  697,  692,  688,  683,  678,  674,  669,  664,  659,  654,
      650,  645,  640,  635,  630,  625,  620,  615,  610,  605,  600,  595,  590,  584,  579,  574,
      569,  564,  558,  553,  548,  543,  537,  532,  526,  521,  516,  510,  505,  499,  494,  488,
      483,  477,  472,  466,  460,  455,  449,  443,  438,  432,  426,  421,  415,  409,  403,  398,
      392,  386,  380,  374,  369,  363,  357,  351,  345,  339,  333,  327,  321,  315,  309,  303,
      297,  291,  285,  279,  273,  267,  261,  255,  249,  243,  237,  230,  224,  218,  212,  206,
      200,  194,  187,  181,  175,  169,  163,  156,  150,  144,  138,  132,  125,  119,  113,  107,
      100,   94,   88,   82,   75,   69,   63,   57,   50,   44,   38,   31,   25,   19,   13,    6,
        0,   -6,  -13,  -19,  -25,  -31,  -38,  -44,  -50,  -57,  -63,  -69,  -75,  -82,  -88,  -94,
     -100, -107, -113, -119, -125, -132, -138, -144, -150, -156, -163, -169, -175, -181, -187, -194,
     -200, -206, -212, -218, -224, -230, -237, -243, -249, -255, -261, -267, -273, -279, -285, -291,
     -297, -303, -309, -315, -321, -327, -333, -339, -345, -351, -357, -363, -369, -374, -380, -386,
     -392, -398, -403, -409, -415, -421, -426, -432, -438, -443, -449, -455, -460, -466, -472, -477,
     -483, -488, -494, -499, -505, -510, -516, -521, -526, -532, -537, -543, -548, -553, -558, -564,
     -569, -574, -579, -584, -590, -595, -600, -605, -610, -615, -620, -625, -630, -635, -640, -645,
     -650, -654, -659, -664, -669, -674, -678, -683, -688, -692, -697, -702, -706, -711, -715, -720,
     -724, -729, -733, -737, -742, -746, -750, -755, -759, -763, -767, -771, -775, -779, -784, -788,
     -792, -796, -799, -803, -807, -811, -815, -819, -822, -826, -830, -834, -837, -841, -844, -848,
     -851, -855, -858, -862, -865, -868, -872, -875, -878, -882, -885, -888, -891, -894, -897, -900,
     -903, -906, -909, -912, -915, -917, -920, -923, -926, -928, -931, -934, -936, -939, -941, -944,
     -946, -948, -951, -953, -955, -958, -960, -962, -964, -966, -968, -970, -972, -974, -976, -978,
     -980, -982, -983, -985, -987, -989, -990, -992, -993, -995, -996, -998, -999,-1000,-1002,-1003,
    -1004,-1006,-1007,-1008,-1009,-1010,-1011,-1012,-1013,-1014,-1015,-1016,-1016,-1017,-1018,-1018,
    -1019,-1020,-1020,-1021,-1021,-1022,-1022,-1022,-1023,-1023,-1023,-1024,-1024,-1024,-1024,-1024,
    -1024,-1024,-1024,-1024,-1024,-1024,-1023,-1023,-1023,-1022,-1022,-1022,-1021,-1021,-1020,-1020,
    -1019,-1018,-1018,-1017,-1016,-1016,-1015,-1014,-1013,-1012,-1011,-1010,-1009,-1008,-1007,-1006,
    -1004,-1003,-1002,-1000, -999, -998, -996, -995, -993, -992, -990, -989, -987, -985, -983, -982,
     -980, -978, -976, -974, -972, -970, -968, -966, -964, -962, -960, -958, -955, -953, -951, -948,
     -946, -944, -941, -939, -936, -934, -931, -928, -926, -923, -920, -917, -915, -912, -909, -906,
     -903, -900, -897, -894, -891, -888, -885, -882, -878, -875, -872, -868, -865, -862, -858, -855,
     -851, -848, -844, -841, -837, -834, -830, -826, -822, -819, -815, -811, -807, -803, -799, -796,
     -792, -788, -784, -779, -775, -771, -767, -763, -759, -755, -750, -746, -742, -737, -733, -729,
     -724, -720, -715, -711, -706, -702, -697, -692, -688, -683, -678, -674, -669, -664, -659, -654,
     -650, -645, -640, -635, -630, -625, -620, -615, -610, -605, -600, -595, -590, -584, -579, -574,
     -569, -564, -558, -553, -548, -543, -537, -532, -526, -521, -516, -510, -505, -499, -494, -488,
     -483, -477, -472, -466, -460, -455, -449, -443, -438, -432, -426, -421, -415, -409, -403, -398,
     -392, -386, -380, -374, -369, -363, -357, -351, -345, -339, -333, -327, -321, -315, -309, -303,
     -297, -291, -285, -279, -273, -267, -261, -255, -249, -243, -237, -230, -224, -218, -212, -206,
     -200, -194, -187, -181, -175, -169, -163, -156, -150, -144, -138, -132, -125, -119, -113, -107,
     -100,  -94,  -88,  -82,  -75,  -69,  -63,  -57,  -50,  -44,  -38,  -31,  -25,  -19,  -13,   -6,
];

fn wrap_angle(mut a: i32) -> i32 {
    while a < 0 { a += ANGLE_PERIOD_FP; }
    while a >= ANGLE_PERIOD_FP { a -= ANGLE_PERIOD_FP; }
    a
}

fn lut_sin(a: i32) -> i32 {
    let a = wrap_angle(a);
    let idx = ((a / FP_ONE) as usize) & TRIG_LUT_MASK;
    let next = (idx + 1) & TRIG_LUT_MASK;
    let frac = a % FP_ONE;
    TRIG_LUT[idx] + fp_mul(TRIG_LUT[next] - TRIG_LUT[idx], frac)
}

fn lut_cos(a: i32) -> i32 { lut_sin(a + TRIG_QUARTER as i32 * FP_ONE) }

fn angle_from_vec(x: i32, y: i32) -> i32 {
    if x == 0 && y == 0 { return 0; }
    let mut best_i = 0usize; let mut best_dot = i64::MIN;
    for k in (0..TRIG_LUT_SIZE).step_by(4) {
        let cv = TRIG_LUT[(k + TRIG_QUARTER) & TRIG_LUT_MASK] as i64;
        let sv = TRIG_LUT[k] as i64;
        let dot = x as i64 * cv + y as i64 * sv;
        if dot > best_dot { best_dot = dot; best_i = k; }
    }
    let lo = if best_i >= 4 { best_i - 4 } else { TRIG_LUT_SIZE - 4 };
    let hi = (best_i + 4) & TRIG_LUT_MASK;
    for k in lo..=hi {
        let ki = k & TRIG_LUT_MASK;
        let cv = TRIG_LUT[(ki + TRIG_QUARTER) & TRIG_LUT_MASK] as i64;
        let sv = TRIG_LUT[ki] as i64;
        let dot = x as i64 * cv + y as i64 * sv;
        if dot > best_dot { best_dot = dot; best_i = ki; }
    }
    best_i as i32 * FP_ONE
}

// ═══════════════════════════════════════════════════════════════════════════════
// Transform
// ═══════════════════════════════════════════════════════════════════════════════

fn compose_tx(a: Tx, b: Tx) -> Tx {
    Tx {
        a: fp_mul(a.a, b.a) + fp_mul(a.c, b.b),
        b: fp_mul(a.b, b.a) + fp_mul(a.d, b.b),
        c: fp_mul(a.a, b.c) + fp_mul(a.c, b.d),
        d: fp_mul(a.b, b.c) + fp_mul(a.d, b.d),
        tx: fp_mul(a.a, b.tx) + fp_mul(a.c, b.ty) + a.tx,
        ty: fp_mul(a.b, b.tx) + fp_mul(a.d, b.ty) + a.ty,
    }
}

#[inline] fn apply_tx(t: Tx, x: i32, y: i32) -> (i32, i32) {
    (fp_mul(x, t.a) + fp_mul(y, t.c) + t.tx,
     fp_mul(x, t.b) + fp_mul(y, t.d) + t.ty)
}

#[inline] fn tx_to_px(t: Tx, x: i32, y: i32) -> (i32, i32) {
    let (fx, fy) = apply_tx(t, x, y);
    (fx / FP_ONE, fy / FP_ONE)
}

#[inline] fn fp_mul(a: i32, b: i32) -> i32 { ((a as i64 * b as i64) / FP_ONE as i64) as i32 }

fn tx_scale_x(t: Tx) -> i32 { isqrt64((t.a as i64 * t.a as i64 + t.b as i64 * t.b as i64) / FP_ONE as i64) as i32 }
fn tx_scale_y(t: Tx) -> i32 { isqrt64((t.c as i64 * t.c as i64 + t.d as i64 * t.d as i64) / FP_ONE as i64) as i32 }

fn parse_transform_attr(attrs: &[u8], rctx: &RenderContext) -> Tx {
    match get_attr(attrs, b"transform", rctx) {
        Some(v) => parse_transforms(v),
        None => Tx::identity(),
    }
}

fn parse_transforms(t: &[u8]) -> Tx {
    let mut cur = Tx::identity();
    let mut i = 0usize;
    while i < t.len() {
        while i < t.len() && (is_ws(t[i]) || t[i] == b',') { i += 1; }
        if i >= t.len() { break; }
        if i + 10 <= t.len() && t[i..i+10].eq_ignore_ascii_case(b"translate(") {
            i += 10;
            let tx = pn_fp(t, &mut i);
            let ty = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            cur = compose_tx(cur, Tx { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx, ty });
        } else if i + 6 <= t.len() && t[i..i+6].eq_ignore_ascii_case(b"scale(") {
            i += 6;
            let sx = pn_fp(t, &mut i);
            let sy = if peek_n(t, i) { pn_fp(t, &mut i) } else { sx };
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            cur = compose_tx(cur, Tx { a: sx, b: 0, c: 0, d: sy, tx: 0, ty: 0 });
        } else if i + 7 <= t.len() && t[i..i+7].eq_ignore_ascii_case(b"rotate(") {
            i += 7;
            let deg = pn_fp(t, &mut i);
            let ccx = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            let ccy = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            let ang = ((deg as i64) * TRIG_LUT_SIZE as i64 / 360) as i32 * FP_ONE;
            let c = lut_cos(ang); let s = lut_sin(ang);
            if ccx != 0 || ccy != 0 {
                cur = compose_tx(cur, Tx { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx: ccx, ty: ccy });
                cur = compose_tx(cur, Tx { a: c, b: s, c: -s, d: c, tx: 0, ty: 0 });
                cur = compose_tx(cur, Tx { a: FP_ONE, b: 0, c: 0, d: FP_ONE, tx: -ccx, ty: -ccy });
            } else {
                cur = compose_tx(cur, Tx { a: c, b: s, c: -s, d: c, tx: 0, ty: 0 });
            }
        } else if i + 6 <= t.len() && t[i..i+6].eq_ignore_ascii_case(b"skewX(") {
            i += 6;
            let deg = pn_fp(t, &mut i);
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            let ang = ((deg as i64) * TRIG_LUT_SIZE as i64 / 360) as i32 * FP_ONE;
            let tn = fp_div(lut_sin(ang), lut_cos(ang).max(1));
            cur = compose_tx(cur, Tx { a: FP_ONE, b: 0, c: tn, d: FP_ONE, tx: 0, ty: 0 });
        } else if i + 6 <= t.len() && t[i..i+6].eq_ignore_ascii_case(b"skewY(") {
            i += 6;
            let deg = pn_fp(t, &mut i);
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            let ang = ((deg as i64) * TRIG_LUT_SIZE as i64 / 360) as i32 * FP_ONE;
            let tn = fp_div(lut_sin(ang), lut_cos(ang).max(1));
            cur = compose_tx(cur, Tx { a: FP_ONE, b: tn, c: 0, d: FP_ONE, tx: 0, ty: 0 });
        } else if i + 7 <= t.len() && t[i..i+7].eq_ignore_ascii_case(b"matrix(") {
            i += 7;
            let a = pn_fp(t, &mut i);
            let b = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            let c = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            let d = if peek_n(t, i) { pn_fp(t, &mut i) } else { FP_ONE };
            let e = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            let f = if peek_n(t, i) { pn_fp(t, &mut i) } else { 0 };
            while i < t.len() && t[i] != b')' { i += 1; } if i < t.len() { i += 1; }
            cur = compose_tx(cur, Tx { a, b, c, d, tx: e, ty: f });
        } else {
            i += 1;
        }
    }
    cur
}

// ═══════════════════════════════════════════════════════════════════════════════
// ViewBox / preserveAspectRatio
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_viewbox(svg: &[u8]) -> (i32, i32, i32, i32) {
    let attrs = match find_root_svg_attrs(svg) {
        Some(attrs) => attrs,
        None => return (0, 0, 0, 0),
    };
    if let Some(v) = get_attr_raw(attrs, b"viewBox").or_else(|| get_attr_raw(attrs, b"viewbox")) {
        let mut i = 0;
        let x = pn_fp(v, &mut i);
        let y = pn_fp(v, &mut i);
        let w = pn_fp(v, &mut i);
        let h = pn_fp(v, &mut i);
        if w > 0 && h > 0 { return (x, y, w, h); }
    }
    (0, 0, 0, 0)
}

#[derive(Clone, Copy)] struct AspectRatio { align: u8, meet: bool }
const PAR_NONE: u8 = 0;
const PAR_XMID_YMID: u8 = 1;
const PAR_XMIN_YMIN: u8 = 2;
const PAR_XMAX_YMAX: u8 = 3;

fn parse_preserve_aspect_ratio(svg: &[u8]) -> AspectRatio {
    let attrs = match find_root_svg_attrs(svg) {
        Some(attrs) => attrs,
        None => return AspectRatio { align: PAR_XMID_YMID, meet: true },
    };
    if let Some(val) = get_attr_raw(attrs, b"preserveAspectRatio") {
        let val = trim_ascii(val);
        if val.eq_ignore_ascii_case(b"none") { return AspectRatio { align: PAR_NONE, meet: true }; }
        let meet = !bytes_contains(val, b"slice");
        let align = if bytes_contains(val, b"xMinYMin") { PAR_XMIN_YMIN }
            else if bytes_contains(val, b"xMaxYMax") { PAR_XMAX_YMAX }
            else { PAR_XMID_YMID };
        return AspectRatio { align, meet };
    }
    AspectRatio { align: PAR_XMID_YMID, meet: true }
}

fn find_root_svg_attrs(svg: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i < svg.len() {
        if svg[i] != b'<' { i += 1; continue; }
        i += 1;
        if i >= svg.len() { break; }
        if i + 2 < svg.len() && svg[i] == b'!' && svg[i + 1] == b'-' && svg[i + 2] == b'-' {
            i += 3;
            while i + 2 < svg.len() {
                if svg[i] == b'-' && svg[i + 1] == b'-' && svg[i + 2] == b'>' { i += 3; break; }
                i += 1;
            }
            continue;
        }
        if svg[i] == b'!' || svg[i] == b'?' || svg[i] == b'/' {
            let (tag_end, _) = find_tag_end(svg, i, svg.len());
            i = tag_end.saturating_add(1);
            continue;
        }
        let ts = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        if !svg[ts..i].eq_ignore_ascii_case(b"svg") {
            let (tag_end, _) = find_tag_end(svg, i, svg.len());
            i = tag_end.saturating_add(1);
            continue;
        }
        let as0 = i;
        let (tag_end, _) = find_tag_end(svg, i, svg.len());
        return Some(&svg[as0..tag_end]);
    }
    None
}

fn compute_root_tx(vbx: i32, vby: i32, vbw: i32, vbh: i32, ow: i32, oh: i32, par: AspectRatio) -> Tx {
    if vbw <= 0 || vbh <= 0 { return Tx::identity(); }
    let ow_fp = ow * FP_ONE;
    let oh_fp = oh * FP_ONE;
    if par.align == PAR_NONE {
        let sx = ((ow_fp as i64 * FP_ONE as i64) / vbw as i64) as i32;
        let sy = ((oh_fp as i64 * FP_ONE as i64) / vbh as i64) as i32;
        let tx = -fp_mul(vbx, sx);
        let ty = -fp_mul(vby, sy);
        return Tx { a: sx, b: 0, c: 0, d: sy, tx, ty };
    }
    let sx = ((ow_fp as i64 * FP_ONE as i64) / vbw as i64) as i32;
    let sy = ((oh_fp as i64 * FP_ONE as i64) / vbh as i64) as i32;
    let s = if par.meet { sx.min(sy) } else { sx.max(sy) };
    let tw = fp_mul(vbw, s) / FP_ONE;
    let th = fp_mul(vbh, s) / FP_ONE;
    let (off_x, off_y) = match par.align {
        PAR_XMIN_YMIN => (0, 0),
        PAR_XMAX_YMAX => ((ow - tw) * FP_ONE, (oh - th) * FP_ONE),
        _ => (((ow - tw) * FP_ONE) / 2, ((oh - th) * FP_ONE) / 2),
    };
    let tx = off_x - fp_mul(vbx, s);
    let ty = off_y - fp_mul(vby, s);
    Tx { a: s, b: 0, c: 0, d: s, tx, ty }
}

fn bytes_contains(hay: &[u8], needle: &[u8]) -> bool {
    if needle.len() > hay.len() { return false; }
    for i in 0..=hay.len() - needle.len() {
        if hay[i..i + needle.len()].eq_ignore_ascii_case(needle) { return true; }
    }
    false
}

// ═══════════════════════════════════════════════════════════════════════════════
// Color parsing
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_color_str(v: &[u8], rctx: Option<&RenderContext>, cur: Option<(u8,u8,u8,u8)>) -> Option<(u8,u8,u8,u8)> {
    let v = trim_ascii(v);
    if v.is_empty() || v.eq_ignore_ascii_case(b"none") || v.eq_ignore_ascii_case(b"transparent") { return None; }
    if v.eq_ignore_ascii_case(b"currentColor") || v.eq_ignore_ascii_case(b"currentcolor") {
        return cur;
    }
    if starts_with_ic(v, b"url(") {
        if let Some(ctx) = rctx {
            return resolve_paint_url_color(v, ctx);
        }
        return None;
    }
    if v[0] == b'#' { return parse_hex_color(v); }
    if starts_with_ic(v, b"rgb(") { return parse_rgb_func(v); }
    if starts_with_ic(v, b"rgba(") { return parse_rgba_func(v); }
    if starts_with_ic(v, b"hsl(") { return parse_hsl_func(v); }
    if starts_with_ic(v, b"hsla(") { return parse_hsla_func(v); }
    named_color(v).map(|(r, g, b)| (r, g, b, 255))
}

fn parse_hex_color(v: &[u8]) -> Option<(u8, u8, u8, u8)> {
    match v.len() {
        7 => Some(((hn(v[1])<<4)|hn(v[2]), (hn(v[3])<<4)|hn(v[4]), (hn(v[5])<<4)|hn(v[6]), 255)),
        4 => Some((hn(v[1])*17, hn(v[2])*17, hn(v[3])*17, 255)),
        9 => Some(((hn(v[1])<<4)|hn(v[2]), (hn(v[3])<<4)|hn(v[4]), (hn(v[5])<<4)|hn(v[6]), (hn(v[7])<<4)|hn(v[8]))),
        5 => Some((hn(v[1])*17, hn(v[2])*17, hn(v[3])*17, hn(v[4])*17)),
        _ => None,
    }
}

fn parse_rgb_func(v: &[u8]) -> Option<(u8,u8,u8,u8)> {
    let mut i = 4;
    let r = pn_fp(v, &mut i) / FP_ONE;
    let g = pn_fp(v, &mut i) / FP_ONE;
    let b = pn_fp(v, &mut i) / FP_ONE;
    Some((r.clamp(0,255) as u8, g.clamp(0,255) as u8, b.clamp(0,255) as u8, 255))
}

fn parse_rgba_func(v: &[u8]) -> Option<(u8,u8,u8,u8)> {
    let mut i = 5;
    let r = pn_fp(v, &mut i) / FP_ONE;
    let g = pn_fp(v, &mut i) / FP_ONE;
    let b = pn_fp(v, &mut i) / FP_ONE;
    let a = parse_decimal_u8(v, &mut i);
    Some((r.clamp(0,255) as u8, g.clamp(0,255) as u8, b.clamp(0,255) as u8, a))
}

fn parse_hsl_func(v: &[u8]) -> Option<(u8,u8,u8,u8)> {
    let mut i = 4;
    let h = pn_fp(v, &mut i) / FP_ONE;
    // skip % if present
    while i < v.len() && (v[i] == b'%' || v[i] == b',' || is_ws(v[i])) { i += 1; }
    let s = pn_fp(v, &mut i) / FP_ONE;
    while i < v.len() && (v[i] == b'%' || v[i] == b',' || is_ws(v[i])) { i += 1; }
    let l = pn_fp(v, &mut i) / FP_ONE;
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0, 100), l.clamp(0, 100));
    Some((r, g, b, 255))
}

fn parse_hsla_func(v: &[u8]) -> Option<(u8,u8,u8,u8)> {
    let mut i = 5;
    let h = pn_fp(v, &mut i) / FP_ONE;
    while i < v.len() && (v[i] == b'%' || v[i] == b',' || is_ws(v[i])) { i += 1; }
    let s = pn_fp(v, &mut i) / FP_ONE;
    while i < v.len() && (v[i] == b'%' || v[i] == b',' || is_ws(v[i])) { i += 1; }
    let l = pn_fp(v, &mut i) / FP_ONE;
    let a = parse_decimal_u8(v, &mut i);
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0, 100), l.clamp(0, 100));
    Some((r, g, b, a))
}

fn hsl_to_rgb(h: i32, s: i32, l: i32) -> (u8, u8, u8) {
    if s == 0 { let v = (l * 255 / 100).clamp(0, 255) as u8; return (v, v, v); }
    let h = ((h % 360) + 360) % 360;
    let s = s.clamp(0, 100); let l = l.clamp(0, 100);
    let c = (100 - (2 * l - 100).abs()) * s / 100;
    let hh = h * 10 / 60;
    let x = c * (10 - (hh % 20 - 10).abs()) / 10;
    let (r1, g1, b1) = match hh / 10 {
        0 => (c, x, 0), 1 => (x, c, 0), 2 => (0, c, x),
        3 => (0, x, c), 4 => (x, 0, c), _ => (c, 0, x),
    };
    let m = l - c / 2;
    let scale = |v: i32| ((v + m) * 255 / 100).clamp(0, 255) as u8;
    (scale(r1), scale(g1), scale(b1))
}

#[inline] fn hn(b: u8) -> u8 {
    match b { b'0'..=b'9' => b - b'0', b'a'..=b'f' => 10 + b - b'a', b'A'..=b'F' => 10 + b - b'A', _ => 0 }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Named colors (all 148 CSS named colors)
// ═══════════════════════════════════════════════════════════════════════════════

fn named_color(v: &[u8]) -> Option<(u8, u8, u8)> {
    let mut lower = [0u8; 24];
    let len = v.len().min(24);
    for i in 0..len { lower[i] = v[i].to_ascii_lowercase(); }
    let s = &lower[..len];
    match s {
        b"aliceblue" => Some((240, 248, 255)),
        b"antiquewhite" => Some((250, 235, 215)),
        b"aqua" => Some((0, 255, 255)),
        b"aquamarine" => Some((127, 255, 212)),
        b"azure" => Some((240, 255, 255)),
        b"beige" => Some((245, 245, 220)),
        b"bisque" => Some((255, 228, 196)),
        b"black" => Some((0, 0, 0)),
        b"blanchedalmond" => Some((255, 235, 205)),
        b"blue" => Some((0, 0, 255)),
        b"blueviolet" => Some((138, 43, 226)),
        b"brown" => Some((165, 42, 42)),
        b"burlywood" => Some((222, 184, 135)),
        b"cadetblue" => Some((95, 158, 160)),
        b"chartreuse" => Some((127, 255, 0)),
        b"chocolate" => Some((210, 105, 30)),
        b"coral" => Some((255, 127, 80)),
        b"cornflowerblue" => Some((100, 149, 237)),
        b"cornsilk" => Some((255, 248, 220)),
        b"crimson" => Some((220, 20, 60)),
        b"cyan" => Some((0, 255, 255)),
        b"darkblue" => Some((0, 0, 139)),
        b"darkcyan" => Some((0, 139, 139)),
        b"darkgoldenrod" => Some((184, 134, 11)),
        b"darkgray" => Some((169, 169, 169)),
        b"darkgreen" => Some((0, 100, 0)),
        b"darkgrey" => Some((169, 169, 169)),
        b"darkkhaki" => Some((189, 183, 107)),
        b"darkmagenta" => Some((139, 0, 139)),
        b"darkolivegreen" => Some((85, 107, 47)),
        b"darkorange" => Some((255, 140, 0)),
        b"darkorchid" => Some((153, 50, 204)),
        b"darkred" => Some((139, 0, 0)),
        b"darksalmon" => Some((233, 150, 122)),
        b"darkseagreen" => Some((143, 188, 143)),
        b"darkslateblue" => Some((72, 61, 139)),
        b"darkslategray" => Some((47, 79, 79)),
        b"darkslategrey" => Some((47, 79, 79)),
        b"darkturquoise" => Some((0, 206, 209)),
        b"darkviolet" => Some((148, 0, 211)),
        b"deeppink" => Some((255, 20, 147)),
        b"deepskyblue" => Some((0, 191, 255)),
        b"dimgray" => Some((105, 105, 105)),
        b"dimgrey" => Some((105, 105, 105)),
        b"dodgerblue" => Some((30, 144, 255)),
        b"firebrick" => Some((178, 34, 34)),
        b"floralwhite" => Some((255, 250, 240)),
        b"forestgreen" => Some((34, 139, 34)),
        b"fuchsia" => Some((255, 0, 255)),
        b"gainsboro" => Some((220, 220, 220)),
        b"ghostwhite" => Some((248, 248, 255)),
        b"gold" => Some((255, 215, 0)),
        b"goldenrod" => Some((218, 165, 32)),
        b"gray" => Some((128, 128, 128)),
        b"green" => Some((0, 128, 0)),
        b"greenyellow" => Some((173, 255, 47)),
        b"grey" => Some((128, 128, 128)),
        b"honeydew" => Some((240, 255, 240)),
        b"hotpink" => Some((255, 105, 180)),
        b"indianred" => Some((205, 92, 92)),
        b"indigo" => Some((75, 0, 130)),
        b"ivory" => Some((255, 255, 240)),
        b"khaki" => Some((240, 230, 140)),
        b"lavender" => Some((230, 230, 250)),
        b"lavenderblush" => Some((255, 240, 245)),
        b"lawngreen" => Some((124, 252, 0)),
        b"lemonchiffon" => Some((255, 250, 205)),
        b"lightblue" => Some((173, 216, 230)),
        b"lightcoral" => Some((240, 128, 128)),
        b"lightcyan" => Some((224, 255, 255)),
        b"lightgoldenrodyellow" => Some((250, 250, 210)),
        b"lightgray" => Some((211, 211, 211)),
        b"lightgreen" => Some((144, 238, 144)),
        b"lightgrey" => Some((211, 211, 211)),
        b"lightpink" => Some((255, 182, 193)),
        b"lightsalmon" => Some((255, 160, 122)),
        b"lightseagreen" => Some((32, 178, 170)),
        b"lightskyblue" => Some((135, 206, 250)),
        b"lightslategray" => Some((119, 136, 153)),
        b"lightslategrey" => Some((119, 136, 153)),
        b"lightsteelblue" => Some((176, 196, 222)),
        b"lightyellow" => Some((255, 255, 224)),
        b"lime" => Some((0, 255, 0)),
        b"limegreen" => Some((50, 205, 50)),
        b"linen" => Some((250, 240, 230)),
        b"magenta" => Some((255, 0, 255)),
        b"maroon" => Some((128, 0, 0)),
        b"mediumaquamarine" => Some((102, 205, 170)),
        b"mediumblue" => Some((0, 0, 205)),
        b"mediumorchid" => Some((186, 85, 211)),
        b"mediumpurple" => Some((147, 112, 219)),
        b"mediumseagreen" => Some((60, 179, 113)),
        b"mediumslateblue" => Some((123, 104, 238)),
        b"mediumspringgreen" => Some((0, 250, 154)),
        b"mediumturquoise" => Some((72, 209, 204)),
        b"mediumvioletred" => Some((199, 21, 133)),
        b"midnightblue" => Some((25, 25, 112)),
        b"mintcream" => Some((245, 255, 250)),
        b"mistyrose" => Some((255, 228, 225)),
        b"moccasin" => Some((255, 228, 181)),
        b"navajowhite" => Some((255, 222, 173)),
        b"navy" => Some((0, 0, 128)),
        b"oldlace" => Some((253, 245, 230)),
        b"olive" => Some((128, 128, 0)),
        b"olivedrab" => Some((107, 142, 35)),
        b"orange" => Some((255, 165, 0)),
        b"orangered" => Some((255, 69, 0)),
        b"orchid" => Some((218, 112, 214)),
        b"palegoldenrod" => Some((238, 232, 170)),
        b"palegreen" => Some((152, 251, 152)),
        b"paleturquoise" => Some((175, 238, 238)),
        b"palevioletred" => Some((219, 112, 147)),
        b"papayawhip" => Some((255, 239, 213)),
        b"peachpuff" => Some((255, 218, 185)),
        b"peru" => Some((205, 133, 63)),
        b"pink" => Some((255, 192, 203)),
        b"plum" => Some((221, 160, 221)),
        b"powderblue" => Some((176, 224, 230)),
        b"purple" => Some((128, 0, 128)),
        b"rebeccapurple" => Some((102, 51, 153)),
        b"red" => Some((255, 0, 0)),
        b"rosybrown" => Some((188, 143, 143)),
        b"royalblue" => Some((65, 105, 225)),
        b"saddlebrown" => Some((139, 69, 19)),
        b"salmon" => Some((250, 128, 114)),
        b"sandybrown" => Some((244, 164, 96)),
        b"seagreen" => Some((46, 139, 87)),
        b"seashell" => Some((255, 245, 238)),
        b"sienna" => Some((160, 82, 45)),
        b"silver" => Some((192, 192, 192)),
        b"skyblue" => Some((135, 206, 235)),
        b"slateblue" => Some((106, 90, 205)),
        b"slategray" => Some((112, 128, 144)),
        b"slategrey" => Some((112, 128, 144)),
        b"snow" => Some((255, 250, 250)),
        b"springgreen" => Some((0, 255, 127)),
        b"steelblue" => Some((70, 130, 180)),
        b"tan" => Some((210, 180, 140)),
        b"teal" => Some((0, 128, 128)),
        b"thistle" => Some((216, 191, 216)),
        b"tomato" => Some((255, 99, 71)),
        b"turquoise" => Some((64, 224, 208)),
        b"violet" => Some((238, 130, 238)),
        b"wheat" => Some((245, 222, 179)),
        b"white" => Some((255, 255, 255)),
        b"whitesmoke" => Some((245, 245, 245)),
        b"yellow" => Some((255, 255, 0)),
        b"yellowgreen" => Some((154, 205, 50)),
        _ => None,
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// CSS parsing
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_css_rules(svg: &[u8]) -> Vec<CssRule> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 6 < svg.len() && out.len() < MAX_CSS_RULES {
        if svg[i] == b'<' && svg[i+1..].starts_with(b"style") {
            let mut j = i;
            while j < svg.len() && svg[j] != b'>' { j += 1; }
            if j >= svg.len() { break; }
            let cs = j + 1;
            let mut k = cs;
            let mut end = None;
            while k + 8 <= svg.len() {
                if svg[k..].starts_with(b"</style>") { end = Some(k); break; }
                k += 1;
            }
            if let Some(ce) = end {
                let block = &svg[cs..ce];
                // Strip CDATA wrapper if present: <![CDATA[ ... ]]>
                let block = if let Some(inner) = strip_cdata(block) { inner } else { block };
                parse_css_block(block, &mut out);
                i = ce + 8;
            }
            else { break; }
            continue;
        }
        i += 1;
    }
    out
}

fn strip_cdata(s: &[u8]) -> Option<&[u8]> {
    let s = trim_ascii(s);
    if !s.starts_with(b"<![CDATA[") { return None; }
    let inner = &s[9..];
    if let Some(pos) = inner.windows(3).position(|w| w == b"]]>") {
        Some(&inner[..pos])
    } else {
        Some(inner)
    }
}

fn parse_css_block(block: &[u8], out: &mut Vec<CssRule>) {
    let mut i = 0;
    while i < block.len() && out.len() < MAX_CSS_RULES {
        while i < block.len() && is_ws(block[i]) { i += 1; }
        if i >= block.len() { break; }
        let ss = i;
        while i < block.len() && block[i] != b'{' { i += 1; }
        if i >= block.len() { break; }
        let sels = trim_ascii(&block[ss..i]);
        i += 1;
        let ds = i;
        while i < block.len() && block[i] != b'}' { i += 1; }
        let decls = &block[ds..i];
        if i < block.len() { i += 1; }
        let classes = parse_class_selectors(sels);
        if classes.is_empty() { continue; }
        let mut d = 0;
        while d < decls.len() {
            while d < decls.len() && (is_ws(decls[d]) || decls[d] == b';') { d += 1; }
            if d >= decls.len() { break; }
            let ps = d;
            while d < decls.len() && decls[d] != b':' && decls[d] != b';' { d += 1; }
            if d >= decls.len() || decls[d] != b':' { while d < decls.len() && decls[d] != b';' { d += 1; } continue; }
            let prop = trim_ascii(&decls[ps..d]).to_vec();
            d += 1;
            let vs = d;
            while d < decls.len() && decls[d] != b';' { d += 1; }
            let val = trim_ascii(&decls[vs..d]).to_vec();
            for c in &classes {
                if out.len() >= MAX_CSS_RULES { break; }
                out.push(CssRule { class_name: c.clone(), prop: prop.clone(), value: val.clone() });
            }
        }
    }
}

fn parse_class_selectors(sels: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut s = 0;
    while s < sels.len() {
        while s < sels.len() && (is_ws(sels[s]) || sels[s] == b',') { s += 1; }
        if s >= sels.len() { break; }
        if sels[s] != b'.' { while s < sels.len() && sels[s] != b',' { s += 1; } continue; }
        s += 1;
        let cs = s;
        while s < sels.len() && !is_ws(sels[s]) && sels[s] != b',' && sels[s] != b'{' { s += 1; }
        if s > cs { out.push(sels[cs..s].to_vec()); }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════════
// Attribute parsing
// ═══════════════════════════════════════════════════════════════════════════════

fn get_attr<'a>(attrs: &'a [u8], name: &[u8], ctx: &'a RenderContext) -> Option<&'a [u8]> {
    if let Some(v) = get_attr_raw(attrs, name) { return Some(v); }
    if let Some(style) = get_attr_raw(attrs, b"style") {
        if let Some(v) = get_style_prop(style, name) { return Some(v); }
    }
    class_attr_lookup(attrs, name, ctx)
}

fn class_attr_lookup<'a>(attrs: &[u8], name: &[u8], ctx: &'a RenderContext) -> Option<&'a [u8]> {
    let classes = get_attr_raw(attrs, b"class")?;
    for rule in ctx.css_rules.iter().rev() {
        if rule.prop.eq_ignore_ascii_case(name) && class_list_contains(classes, &rule.class_name) {
            return Some(&rule.value);
        }
    }
    None
}

fn class_list_contains(classes: &[u8], name: &[u8]) -> bool {
    let mut i = 0;
    while i < classes.len() {
        while i < classes.len() && is_ws(classes[i]) {
            i += 1;
        }

        let start = i;

        while i < classes.len() && !is_ws(classes[i]) {
            i += 1;
        }

        if start < i && &classes[start..i] == name {
            return true;
        }
    }

    false
}

fn get_attr_raw<'a>(attrs: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < attrs.len() {
        while i < attrs.len() && is_ws(attrs[i]) { i += 1; }
        if i >= attrs.len() { break; }
        if attrs[i] == b'/' || attrs[i] == b'>' { i += 1; continue; }
        let ns = i;
        while i < attrs.len() && attrs[i] != b'=' && !is_ws(attrs[i]) && attrs[i] != b'>' && attrs[i] != b'/' { i += 1; }
        if ns == i { i += 1; continue; }
        let aname = &attrs[ns..i];
        while i < attrs.len() && is_ws(attrs[i]) { i += 1; }
        if i >= attrs.len() || attrs[i] != b'=' {
            while i < attrs.len() && !is_ws(attrs[i]) && attrs[i] != b'>' && attrs[i] != b'/' { i += 1; }
            continue;
        }
        i += 1;
        while i < attrs.len() && is_ws(attrs[i]) { i += 1; }
        if i >= attrs.len() { break; }
        let val = if attrs[i] == b'"' || attrs[i] == b'\'' {
            let q = attrs[i]; i += 1;
            let vs = i;
            while i < attrs.len() && attrs[i] != q { i += 1; }
            let v = &attrs[vs..i];
            if i < attrs.len() { i += 1; }
            v
        } else {
            let vs = i;
            while i < attrs.len() && !is_ws(attrs[i]) && attrs[i] != b'>' { i += 1; }
            &attrs[vs..i]
        };
        if aname.eq_ignore_ascii_case(name) { return Some(val); }
    }
    None
}

fn get_style_prop<'a>(style: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for part in style.split(|&c| c == b';') {
        let mut kv = part.splitn(2, |&c| c == b':');
        let key = match kv.next() { Some(k) => trim_ascii(k), None => continue };
        let val = match kv.next() { Some(v) => trim_ascii(v), None => continue };
        if key.eq_ignore_ascii_case(name) {
            return Some(val);
        }
    }
    None
}

fn is_display_none(attrs: &[u8], ctx: &RenderContext) -> bool {
    get_attr(attrs, b"display", ctx).map_or(false, |v| trim_ascii(v).eq_ignore_ascii_case(b"none"))
}

fn is_visibility_hidden(attrs: &[u8], ctx: &RenderContext) -> bool {
    get_attr(attrs, b"visibility", ctx).map_or(false, |v| trim_ascii(v).eq_ignore_ascii_case(b"hidden"))
}

#[derive(Clone, Copy)]
enum LengthAxis { Horizontal, Vertical, Normalized }

fn attr_len(attrs: &[u8], name: &[u8], axis: LengthAxis, rctx: &RenderContext) -> i32 {
    get_attr(attrs, name, rctx)
        .map(|v| parse_length(v, axis, rctx.viewport_w, rctx.viewport_h))
        .unwrap_or(0)
}

fn parse_length(v: &[u8], axis: LengthAxis, viewport_w: i32, viewport_h: i32) -> i32 {
    parse_length_with_ref(v, axis_ref(axis, viewport_w, viewport_h))
}

fn parse_length_with_ref(v: &[u8], ref_len: i32) -> i32 {
    let v = trim_ascii(v);
    let mut i = 0;
    let num = parse_number_raw_fp(v, &mut i);
    let rem = trim_ascii(&v[i..]);
    if rem.is_empty() || rem.eq_ignore_ascii_case(b"px") { return num; }
    if rem[0] == b'%' {
        return ((num as i64 * ref_len as i64) / (100 * FP_ONE) as i64)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    }
    scale_unit_length(num, rem)
}

fn axis_ref(axis: LengthAxis, viewport_w: i32, viewport_h: i32) -> i32 {
    match axis {
        LengthAxis::Horizontal => viewport_w.max(0),
        LengthAxis::Vertical => viewport_h.max(0),
        LengthAxis::Normalized => {
            let sum = viewport_w.max(0) as i64 + viewport_h.max(0) as i64;
            (sum / 2).clamp(0, i32::MAX as i64) as i32
        }
    }
}

fn scale_unit_length(num: i32, unit: &[u8]) -> i32 {
    let mul_div = |mul: i32, div: i32| -> i32 {
        ((num as i64 * mul as i64) / div as i64)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32
    };
    if unit.eq_ignore_ascii_case(b"pt") { return mul_div(4, 3); }
    if unit.eq_ignore_ascii_case(b"pc") { return mul_div(16, 1); }
    if unit.eq_ignore_ascii_case(b"in") { return mul_div(96, 1); }
    if unit.eq_ignore_ascii_case(b"cm") { return mul_div(3780, 1000); }
    if unit.eq_ignore_ascii_case(b"mm") { return mul_div(378, 100); }
    if unit.eq_ignore_ascii_case(b"q") { return mul_div(378, 400); }
    if unit.eq_ignore_ascii_case(b"em") || unit.eq_ignore_ascii_case(b"rem") { return mul_div(16, 1); }
    if unit.eq_ignore_ascii_case(b"ex") { return mul_div(8, 1); }
    num
}

// ═══════════════════════════════════════════════════════════════════════════════
// Gradient parsing
// ═══════════════════════════════════════════════════════════════════════════════

fn parse_all_gradients(svg: &[u8], viewport_w: i32, viewport_h: i32) -> Vec<GradientDef> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < svg.len() && out.len() < MAX_GRADIENTS {
        if svg[i] != b'<' { i += 1; continue; }
        let ts = i + 1;
        let mut t = ts;
        while t < svg.len() && !is_ws(svg[t]) && svg[t] != b'>' && svg[t] != b'/' { t += 1; }
        let tag = &svg[ts..t];
        if tag != b"linearGradient" && tag != b"radialGradient" { i += 1; continue; }
        let (ae, self_close) = find_tag_end(svg, t, svg.len());
        if ae >= svg.len() { break; }
        let attrs = &svg[t..ae];
        let id = match get_attr_raw(attrs, b"id") { Some(v) => trim_ascii(v).to_vec(), None => { i = ae + 1; continue; } };
        let obj_bbox = get_attr_raw(attrs, b"gradientUnits")
            .map(|v| !v.eq_ignore_ascii_case(b"userSpaceOnUse")).unwrap_or(true);
        let grad_tx = get_attr_raw(attrs, b"gradientTransform")
            .map(parse_transforms).unwrap_or(Tx::identity());
        let href = get_attr_raw(attrs, b"href").or_else(|| get_attr_raw(attrs, b"xlink:href"));
        let ref_x = if obj_bbox { FP_ONE } else { viewport_w };
        let ref_y = if obj_bbox { FP_ONE } else { viewport_h };
        let ref_r = axis_ref(LengthAxis::Normalized, ref_x, ref_y).max(FP_ONE);
        let kind = if tag == b"linearGradient" {
            GradientKind::Linear {
                x1: get_attr_raw(attrs, b"x1").map(|v| parse_length_with_ref(v, ref_x)).unwrap_or(0),
                y1: get_attr_raw(attrs, b"y1").map(|v| parse_length_with_ref(v, ref_y)).unwrap_or(0),
                x2: get_attr_raw(attrs, b"x2").map(|v| parse_length_with_ref(v, ref_x)).unwrap_or(FP_ONE),
                y2: get_attr_raw(attrs, b"y2").map(|v| parse_length_with_ref(v, ref_y)).unwrap_or(0),
            }
        } else {
            let cx = get_attr_raw(attrs, b"cx").map(|v| parse_length_with_ref(v, ref_x)).unwrap_or(FP_ONE / 2);
            let cy = get_attr_raw(attrs, b"cy").map(|v| parse_length_with_ref(v, ref_y)).unwrap_or(FP_ONE / 2);
            let r = get_attr_raw(attrs, b"r").map(|v| parse_length_with_ref(v, ref_r)).unwrap_or(FP_ONE / 2);
            let fx = get_attr_raw(attrs, b"fx").map(|v| parse_length_with_ref(v, ref_x)).unwrap_or(cx);
            let fy = get_attr_raw(attrs, b"fy").map(|v| parse_length_with_ref(v, ref_y)).unwrap_or(cy);
            GradientKind::Radial { cx, cy, r, fx, fy }
        };
        let mut stops = Vec::new();
        if !self_close {
            let close_pat: &[u8] = if tag == b"linearGradient" { b"</linearGradient>" } else { b"</radialGradient>" };
            let mut e = ae + 1;
            let mut close_pos = None;
            while e + close_pat.len() <= svg.len() {
                if &svg[e..e + close_pat.len()] == close_pat { close_pos = Some(e); break; }
                e += 1;
            }
            if let Some(cp) = close_pos {
                parse_stops(&svg[ae + 1..cp], &mut stops);
                i = cp + close_pat.len();
            } else { i = ae + 1; }
        } else { i = ae + 1; }
        out.push(GradientDef { id, kind, stops, object_bbox: obj_bbox, grad_tx });
        if let Some(link) = href.and_then(|v| parse_href_id_slice(v).or_else(|| parse_url_id_slice(v))) {
            if let Some(base_stops) = out.iter().find(|g| g.id == link).map(|g| g.stops.clone()) {
                if let Some(cur) = out.last_mut() {
                    if cur.stops.is_empty() { cur.stops = base_stops; }
                }
            }
        }
    }
    out
}

fn parse_stops(block: &[u8], out: &mut Vec<(u8, u32)>) {
    let mut s = 0;
    while s < block.len() && out.len() < MAX_GRAD_STOPS {
        if block[s] != b'<' { s += 1; continue; }
        if s + 5 >= block.len() || !block[s+1..].starts_with(b"stop") { s += 1; continue; }
        let mut se = s + 1;
        while se < block.len() && block[se] != b'>' { se += 1; }
        if se >= block.len() { break; }
        let sa = &block[s + 5..se];
        let offset_raw = get_attr_raw(sa, b"offset").or_else(|| {
            get_attr_raw(sa, b"style").and_then(|st| get_style_prop(st, b"offset"))
        });
        let offset = match offset_raw {
            Some(v) => parse_stop_offset(v),
            None => 0,
        };
        let color_raw = get_attr_raw(sa, b"stop-color").or_else(|| {
            get_attr_raw(sa, b"style").and_then(|st| get_style_prop(st, b"stop-color"))
        });
        let opacity_raw = get_attr_raw(sa, b"stop-opacity").or_else(|| {
            get_attr_raw(sa, b"style").and_then(|st| get_style_prop(st, b"stop-opacity"))
        });
        let (r, g, b, mut a) = match color_raw.and_then(|v| parse_color_str(v, None, None)) {
            Some(c) => c,
            None => (0, 0, 0, 255),
        };
        if let Some(ov) = opacity_raw { a = mul_alpha(a, parse_opacity_u8(ov)); }
        out.push((offset, to_argb((r, g, b, a))));
        s = se + 1;
    }
}

fn parse_stop_offset(v: &[u8]) -> u8 {
    let v = trim_ascii(v);
    let mut i = 0;
    let fp = pn_fp(v, &mut i);
    if i < v.len() && v[i] == b'%' {
        (fp * 255 / (100 * FP_ONE)).clamp(0, 255) as u8
    } else {
        (fp * 255 / FP_ONE).clamp(0, 255) as u8
    }
}

fn collect_defs(svg: &[u8]) -> Vec<DefContent> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < svg.len() && out.len() < MAX_DEFS {
        if svg[i] != b'<' { i += 1; continue; }
        let tag_start = i;
        i += 1;
        if i >= svg.len() || svg[i] == b'/' || svg[i] == b'!' || svg[i] == b'?' {
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1; continue;
        }
        let ns = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        let tag = &svg[ns..i];
        if tag != b"symbol" && tag != b"g" { i = tag_start + 1; continue; }
        let a0 = i;
        let (tag_end, sc) = find_tag_end(svg, i, svg.len());
        let a = &svg[a0..tag_end];
        let content_start = tag_end + 1;
        i = tag_end.saturating_add(1);
        if sc { continue; }
        if let Some(id) = get_attr_raw(a, b"id") {
            let ce = find_closing_tag_end(svg, content_start, tag);
            // content_end is before the closing tag
            let content_end = if ce > tag.len() + 3 { ce - tag.len() - 3 } else { ce };
            out.push(DefContent { id: id.to_vec(), start: content_start, end: content_end.min(ce) });
        }
    }
    out
}

fn collect_element_defs(svg: &[u8], tag_name: &[u8], max_defs: usize) -> Vec<ElementDef> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < svg.len() && out.len() < max_defs {
        if svg[i] != b'<' { i += 1; continue; }
        let tag_start = i;
        i += 1;
        if i >= svg.len() || svg[i] == b'/' || svg[i] == b'!' || svg[i] == b'?' {
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1;
            continue;
        }
        let ns = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
        let tag = &svg[ns..i];
        if tag != tag_name { i = tag_start + 1; continue; }
        let a0 = i;
        let (tag_end, self_close) = find_tag_end(svg, i, svg.len());
        let attrs = &svg[a0..tag_end];
        let content_start = tag_end.saturating_add(1);
        i = content_start;
        if self_close { continue; }
        if let Some(id) = get_attr_raw(attrs, b"id") {
            let ce = find_closing_tag_end(svg, content_start, tag);
            let content_end = if ce > tag.len() + 3 { ce - tag.len() - 3 } else { ce };
            out.push(ElementDef {
                id: trim_ascii(id).to_vec(),
                start: tag_start,
                content_start,
                content_end: content_end.min(ce),
            });
        }
    }
    out
}

fn get_element_header<'a>(svg: &'a [u8], start: usize) -> Option<(&'a [u8], &'a [u8], usize, bool)> {
    if start >= svg.len() || svg[start] != b'<' { return None; }
    let mut i = start + 1;
    if i >= svg.len() || svg[i] == b'/' || svg[i] == b'!' || svg[i] == b'?' { return None; }
    let ts = i;
    while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' { i += 1; }
    let tag = &svg[ts..i];
    let a0 = i;
    let (tag_end, self_close) = find_tag_end(svg, i, svg.len());
    Some((tag, &svg[a0..tag_end], tag_end.saturating_add(1), self_close))
}

fn clip_path_id<'a>(attrs: &'a [u8], rctx: &'a RenderContext) -> Option<&'a [u8]> {
    get_attr(attrs, b"clip-path", rctx).and_then(parse_url_id_slice)
}

fn element_bbox_svg(tag: &[u8], attrs: &[u8], rctx: &RenderContext) -> Option<(i32, i32, i32, i32)> {
    match tag {
        b"rect" => {
            let x = attr_len(attrs, b"x", LengthAxis::Horizontal, rctx);
            let y = attr_len(attrs, b"y", LengthAxis::Vertical, rctx);
            let w = attr_len(attrs, b"width", LengthAxis::Horizontal, rctx);
            let h = attr_len(attrs, b"height", LengthAxis::Vertical, rctx);
            if w <= 0 || h <= 0 { None } else { Some((x, y, x + w, y + h)) }
        }
        b"circle" => {
            let cx = attr_len(attrs, b"cx", LengthAxis::Horizontal, rctx);
            let cy = attr_len(attrs, b"cy", LengthAxis::Vertical, rctx);
            let r = attr_len(attrs, b"r", LengthAxis::Normalized, rctx);
            if r <= 0 { None } else { Some((cx - r, cy - r, cx + r, cy + r)) }
        }
        b"ellipse" => {
            let cx = attr_len(attrs, b"cx", LengthAxis::Horizontal, rctx);
            let cy = attr_len(attrs, b"cy", LengthAxis::Vertical, rctx);
            let rx = attr_len(attrs, b"rx", LengthAxis::Horizontal, rctx);
            let ry = attr_len(attrs, b"ry", LengthAxis::Vertical, rctx);
            if rx <= 0 || ry <= 0 { None } else { Some((cx - rx, cy - ry, cx + rx, cy + ry)) }
        }
        b"line" => {
            let x1 = attr_len(attrs, b"x1", LengthAxis::Horizontal, rctx);
            let y1 = attr_len(attrs, b"y1", LengthAxis::Vertical, rctx);
            let x2 = attr_len(attrs, b"x2", LengthAxis::Horizontal, rctx);
            let y2 = attr_len(attrs, b"y2", LengthAxis::Vertical, rctx);
            Some((x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)))
        }
        b"polygon" | b"polyline" => {
            let raw = get_attr(attrs, b"points", rctx)?;
            Some(points_bbox_fp(raw, Tx::identity()))
        }
        b"path" => {
            let d = get_attr(attrs, b"d", rctx)?;
            let (subs, _, _) = parse_path_data(d, Tx::identity());
            Some(compute_subpaths_bbox_from_points(&subs))
        }
        _ => None,
    }
}

fn render_clip_path_mask(
    svg: &[u8], clip_id: &[u8], mask: &mut [u32], ow: i32, oh: i32,
    parent_tx: Tx, rctx: &RenderContext, bbox: (i32, i32, i32, i32),
) -> bool {
    let def = match rctx.clip_paths.iter().find(|d| d.id == clip_id) {
        Some(d) => d,
        None => return false,
    };
    let (_, attrs, _, _) = match get_element_header(svg, def.start) {
        Some(h) => h,
        None => return false,
    };
    let units_obj = get_attr_raw(attrs, b"clipPathUnits")
        .map(|v| trim_ascii(v).eq_ignore_ascii_case(b"objectBoundingBox"))
        .unwrap_or(false);
    let bbox_tx = if units_obj {
        let bw = (bbox.2 - bbox.0).max(0);
        let bh = (bbox.3 - bbox.1).max(0);
        Tx { a: bw, b: 0, c: 0, d: bh, tx: bbox.0, ty: bbox.1 }
    } else {
        Tx::identity()
    };
    let clip_tx = compose_tx(parent_tx, compose_tx(parse_transform_attr(attrs, rctx), bbox_tx));
    render_range(svg, def.content_start, def.content_end, mask, ow, oh, clip_tx, 255, (255, 255, 255, 255), rctx, 0).is_ok()
}

fn blend_masked(dst: &mut [u32], src: &[u32], mask: &[u32]) {
    let n = dst.len().min(src.len()).min(mask.len());
    for i in 0..n {
        let ma = ((mask[i] >> 24) & 0xFF) as u8;
        if ma == 0 { continue; }
        let sa = ((src[i] >> 24) & 0xFF) as u8;
        if sa == 0 { continue; }
        dst[i] = alpha_blend_over(dst[i], alpha_mul_argb(src[i], ma));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Paint resolution
// ═══════════════════════════════════════════════════════════════════════════════

fn fill_paint(
    svg: &[u8], attrs: &[u8], inh_op: u8, inh_col: (u8,u8,u8,u8),
    rctx: &RenderContext, ctx: Tx, bbox: (i32, i32, i32, i32),
) -> Option<Paint> {
    let v = get_attr(attrs, b"fill", rctx).unwrap_or(b"black" as &[u8]);
    if v == b"none" { return None; }
    let cur = resolve_current_color(attrs, inh_col, rctx);
    let mut paint = resolve_paint_value(svg, v, rctx, ctx, bbox, cur)?;
    let fill_op = get_attr(attrs, b"fill-opacity", rctx).map(parse_opacity_u8).unwrap_or(255);
    let op = get_attr(attrs, b"opacity", rctx).map(parse_opacity_u8).unwrap_or(255);
    let am = mul_alpha(mul_alpha(fill_op, op), inh_op);
    apply_alpha_to_paint(&mut paint, am);
    if am == 0 { return None; }
    Some(paint)
}

fn stroke_paint_info(
    svg: &[u8], attrs: &[u8], inh_op: u8, inh_col: (u8,u8,u8,u8),
    rctx: &RenderContext, ctx: Tx,
) -> Option<(Paint, i32)> {
    let v = get_attr(attrs, b"stroke", rctx)?;
    if v == b"none" { return None; }
    let cur = resolve_current_color(attrs, inh_col, rctx);
    let mut paint = resolve_paint_value(svg, v, rctx, ctx, (0,0,0,0), cur)?;
    let stroke_op = get_attr(attrs, b"stroke-opacity", rctx).map(parse_opacity_u8).unwrap_or(255);
    let op = get_attr(attrs, b"opacity", rctx).map(parse_opacity_u8).unwrap_or(255);
    let am = mul_alpha(mul_alpha(stroke_op, op), inh_op);
    apply_alpha_to_paint(&mut paint, am);
    if am == 0 { return None; }
    let sw_fp = get_attr(attrs, b"stroke-width", rctx)
        .map(|v| parse_length(v, LengthAxis::Normalized, rctx.viewport_w, rctx.viewport_h))
        .unwrap_or(FP_ONE);
    let sw_px = (fp_mul(sw_fp, tx_scale_x(ctx)).abs() / FP_ONE).max(1);
    Some((paint, sw_px))
}

fn resolve_paint_value(
    svg: &[u8], v: &[u8], rctx: &RenderContext, ctx: Tx, bbox: (i32,i32,i32,i32), cur: (u8,u8,u8,u8),
) -> Option<Paint> {
    if starts_with_ic(v, b"url(") {
        if let Some(grad) = resolve_gradient_url(v, rctx, ctx, bbox) { return Some(grad); }
        if let Some(pattern) = resolve_pattern_url(svg, v, rctx, ctx, bbox) { return Some(pattern); }
        if let Some(c) = resolve_paint_url_color(v, rctx) { return Some(Paint::Solid(to_argb(c))); }
        return None;
    }
    parse_color_str(v, Some(rctx), Some(cur)).map(|c| Paint::Solid(to_argb(c)))
}

fn resolve_gradient_url(v: &[u8], rctx: &RenderContext, ctx: Tx, bbox: (i32,i32,i32,i32)) -> Option<Paint> {
    let id = parse_url_id_slice(v)?;
    let grad = rctx.gradients.iter().find(|g| g.id == id)?;
    if grad.stops.is_empty() { return None; }
    match grad.kind {
        GradientKind::Linear { x1, y1, x2, y2 } => {
            let (gx1, gy1, gx2, gy2) = if grad.object_bbox {
                let bw = bbox.2 - bbox.0; let bh = bbox.3 - bbox.1;
                (bbox.0 + fp_mul(x1, bw), bbox.1 + fp_mul(y1, bh),
                 bbox.0 + fp_mul(x2, bw), bbox.1 + fp_mul(y2, bh))
            } else {
                (x1, y1, x2, y2)
            };
            let (gx1, gy1) = apply_tx(grad.grad_tx, gx1, gy1);
            let (gx2, gy2) = apply_tx(grad.grad_tx, gx2, gy2);
            let (px1, py1) = tx_to_px(ctx, gx1, gy1);
            let (px2, py2) = tx_to_px(ctx, gx2, gy2);
            Some(Paint::LinearGrad { x1: px1, y1: py1, x2: px2, y2: py2, stops: grad.stops.clone() })
        }
        GradientKind::Radial { cx, cy, r, fx, fy } => {
            let (gcx, gcy, gfx, gfy, grx, gry) = if grad.object_bbox {
                let bw = bbox.2 - bbox.0; let bh = bbox.3 - bbox.1;
                (
                    bbox.0 + fp_mul(cx, bw),
                    bbox.1 + fp_mul(cy, bh),
                    bbox.0 + fp_mul(fx, bw),
                    bbox.1 + fp_mul(fy, bh),
                    fp_mul(r, bw).abs(),
                    fp_mul(r, bh).abs(),
                )
            } else {
                (cx, cy, fx, fy, r.abs(), r.abs())
            };
            let (gcx, gcy) = apply_tx(grad.grad_tx, gcx, gcy);
            let (gfx, gfy) = apply_tx(grad.grad_tx, gfx, gfy);
            let grx = (fp_mul(grx, tx_scale_x(grad.grad_tx)).abs() / FP_ONE).max(1);
            let gry = (fp_mul(gry, tx_scale_y(grad.grad_tx)).abs() / FP_ONE).max(1);
            let (pcx, pcy) = tx_to_px(ctx, gcx, gcy);
            let (pfx, pfy) = tx_to_px(ctx, gfx, gfy);
            let prx = (fp_mul(grx, tx_scale_x(ctx)).abs() / FP_ONE).max(1);
            let pry = (fp_mul(gry, tx_scale_y(ctx)).abs() / FP_ONE).max(1);
            Some(Paint::RadialGrad { cx: pcx, cy: pcy, fx: pfx, fy: pfy, rx: prx, ry: pry, stops: grad.stops.clone() })
        }
    }
}

fn resolve_paint_url_color(v: &[u8], rctx: &RenderContext) -> Option<(u8,u8,u8,u8)> {
    let id = parse_url_id_slice(v)?;
    for g in rctx.gradients.iter().rev() {
        if g.id == id {
            if let Some(&(_off, argb)) = g.stops.last() {
                let a = ((argb >> 24) & 0xFF) as u8;
                let r = ((argb >> 16) & 0xFF) as u8;
                let g = ((argb >> 8) & 0xFF) as u8;
                let b = (argb & 0xFF) as u8;
                return Some((r, g, b, a));
            }
        }
    }
    None
}

fn resolve_pattern_url(svg: &[u8], v: &[u8], rctx: &RenderContext, ctx: Tx, bbox: (i32, i32, i32, i32)) -> Option<Paint> {
    let id = parse_url_id_slice(v)?;
    let def = rctx.patterns.iter().find(|d| d.id == id)?;
    let _ = get_element_header(svg, def.start)?;
    let pattern_units_obj = pattern_attr(svg, def, rctx, b"patternUnits", 0)
        .map(|v| !trim_ascii(v).eq_ignore_ascii_case(b"userSpaceOnUse"))
        .unwrap_or(true);
    let content_units_obj = pattern_attr(svg, def, rctx, b"patternContentUnits", 0)
        .map(|v| trim_ascii(v).eq_ignore_ascii_case(b"objectBoundingBox"))
        .unwrap_or(false);

    let bw = (bbox.2 - bbox.0).max(0);
    let bh = (bbox.3 - bbox.1).max(0);
    let x_attr = pattern_attr(svg, def, rctx, b"x", 0).unwrap_or(b"0");
    let y_attr = pattern_attr(svg, def, rctx, b"y", 0).unwrap_or(b"0");
    let w_attr = pattern_attr(svg, def, rctx, b"width", 0)?;
    let h_attr = pattern_attr(svg, def, rctx, b"height", 0)?;
    let x = if pattern_units_obj {
        bbox.0 + fp_mul(parse_length_with_ref(x_attr, FP_ONE), bw)
    } else {
        parse_length(x_attr, LengthAxis::Horizontal, rctx.viewport_w, rctx.viewport_h)
    };
    let y = if pattern_units_obj {
        bbox.1 + fp_mul(parse_length_with_ref(y_attr, FP_ONE), bh)
    } else {
        parse_length(y_attr, LengthAxis::Vertical, rctx.viewport_w, rctx.viewport_h)
    };
    let pw = if pattern_units_obj {
        fp_mul(parse_length_with_ref(w_attr, FP_ONE), bw)
    } else {
        parse_length(w_attr, LengthAxis::Horizontal, rctx.viewport_w, rctx.viewport_h)
    };
    let ph = if pattern_units_obj {
        fp_mul(parse_length_with_ref(h_attr, FP_ONE), bh)
    } else {
        parse_length(h_attr, LengthAxis::Vertical, rctx.viewport_w, rctx.viewport_h)
    };
    if pw <= 0 || ph <= 0 { return None; }

    let tile_w = (fp_mul(pw, tx_scale_x(ctx)).abs() / FP_ONE).max(1) as u32;
    let tile_h = (fp_mul(ph, tx_scale_y(ctx)).abs() / FP_ONE).max(1) as u32;
    let pixel_count = (tile_w as usize).saturating_mul(tile_h as usize);
    if pixel_count == 0 || pixel_count > MAX_PATTERN_PIXELS { return None; }

    let pattern_tx = pattern_attr(svg, def, rctx, b"patternTransform", 0).map(parse_transforms).unwrap_or(Tx::identity());
    let content_scale = if content_units_obj {
        Tx { a: bw.max(0), b: 0, c: 0, d: bh.max(0), tx: 0, ty: 0 }
    } else {
        Tx::identity()
    };
    let mut tile_tx = compose_tx(ctx, compose_tx(pattern_tx, content_scale));
    let (origin_fx, origin_fy) = apply_tx(tile_tx, x, y);
    tile_tx.tx -= origin_fx;
    tile_tx.ty -= origin_fy;

    let mut pixels = alloc::vec![0u32; pixel_count];
    let (content_start, content_end) = pattern_content_range(svg, def, rctx, 0)?;
    if render_range(svg, content_start, content_end, &mut pixels, tile_w as i32, tile_h as i32, tile_tx, 255, (0, 0, 0, 255), rctx, 0).is_err() {
        return None;
    }
    let (origin_x, origin_y) = tx_to_px(ctx, x, y);
    Some(Paint::Pattern { pixels, width: tile_w, height: tile_h, origin_x, origin_y })
}

fn pattern_attr<'a>(svg: &'a [u8], def: &'a ElementDef, rctx: &'a RenderContext, name: &[u8], depth: u32) -> Option<&'a [u8]> {
    if depth > MAX_PATTERN_DEPTH { return None; }
    let (_, attrs, _, _) = get_element_header(svg, def.start)?;
    if let Some(v) = get_attr_raw(attrs, name) { return Some(v); }
    let href = get_attr_raw(attrs, b"href")
        .or_else(|| get_attr_raw(attrs, b"xlink:href"))
        .and_then(parse_href_id_slice)?;
    let base = rctx.patterns.iter().find(|d| d.id == href)?;
    pattern_attr(svg, base, rctx, name, depth + 1)
}

fn pattern_content_range(svg: &[u8], def: &ElementDef, rctx: &RenderContext, depth: u32) -> Option<(usize, usize)> {
    if depth > MAX_PATTERN_DEPTH { return None; }
    if has_markup(&svg[def.content_start..def.content_end]) {
        return Some((def.content_start, def.content_end));
    }
    let (_, attrs, _, _) = get_element_header(svg, def.start)?;
    let href = get_attr_raw(attrs, b"href")
        .or_else(|| get_attr_raw(attrs, b"xlink:href"))
        .and_then(parse_href_id_slice)?;
    let base = rctx.patterns.iter().find(|d| d.id == href)?;
    pattern_content_range(svg, base, rctx, depth + 1)
}

fn has_markup(s: &[u8]) -> bool {
    s.iter().any(|&b| b == b'<')
}

fn parse_url_id_slice(v: &[u8]) -> Option<&[u8]> {
    let v = trim_ascii(v);
    if !starts_with_ic(v, b"url(") { return None; }
    let mut i = 4;
    while i < v.len() && is_ws(v[i]) { i += 1; }
    if i >= v.len() || v[i] != b'#' { return None; }
    i += 1;
    let s = i;
    while i < v.len() && v[i] != b')' && !is_ws(v[i]) { i += 1; }
    Some(&v[s..i])
}

fn parse_href_id_slice(v: &[u8]) -> Option<&[u8]> {
    let v = trim_ascii(v);
    if v.first().copied()? != b'#' { return None; }
    Some(&v[1..])
}

fn apply_alpha_to_paint(paint: &mut Paint, alpha: u8) {
    match paint {
        Paint::Solid(c) => *c = alpha_mul_argb(*c, alpha),
        Paint::LinearGrad { stops, .. } => {
            for s in stops.iter_mut() { s.1 = alpha_mul_argb(s.1, alpha); }
        }
        Paint::RadialGrad { stops, .. } => {
            for s in stops.iter_mut() { s.1 = alpha_mul_argb(s.1, alpha); }
        }
        Paint::Pattern { pixels, .. } => {
            for p in pixels.iter_mut() { *p = alpha_mul_argb(*p, alpha); }
        }
    }
}

fn resolve_current_color(attrs: &[u8], inherited: (u8,u8,u8,u8), ctx: &RenderContext) -> (u8,u8,u8,u8) {
    if let Some(v) = get_attr(attrs, b"color", ctx) {
        if let Some(c) = parse_color_str(v, Some(ctx), Some(inherited)) { return c; }
    }
    inherited
}

fn group_opacity(attrs: &[u8], ctx: &RenderContext) -> u8 {
    get_attr(attrs, b"opacity", ctx).map(parse_opacity_u8).unwrap_or(255)
}

fn fill_rule_attr(attrs: &[u8], ctx: &RenderContext) -> FillRule {
    match get_attr(attrs, b"fill-rule", ctx) {
        Some(v) if trim_ascii(v).eq_ignore_ascii_case(b"evenodd") => FillRule::EvenOdd,
        _ => FillRule::NonZero,
    }
}

fn stroke_linecap_attr(attrs: &[u8], ctx: &RenderContext) -> LineCap {
    match get_attr(attrs, b"stroke-linecap", ctx) {
        Some(v) if trim_ascii(v).eq_ignore_ascii_case(b"round") => LineCap::Round,
        Some(v) if trim_ascii(v).eq_ignore_ascii_case(b"square") => LineCap::Square,
        _ => LineCap::Butt,
    }
}

fn stroke_linejoin_attr(attrs: &[u8], ctx: &RenderContext) -> LineJoin {
    match get_attr(attrs, b"stroke-linejoin", ctx) {
        Some(v) if trim_ascii(v).eq_ignore_ascii_case(b"round") => LineJoin::Round,
        Some(v) if trim_ascii(v).eq_ignore_ascii_case(b"bevel") => LineJoin::Bevel,
        _ => LineJoin::Miter,
    }
}

#[inline] fn to_argb(c: (u8,u8,u8,u8)) -> u32 {
    (c.3 as u32) << 24 | (c.0 as u32) << 16 | (c.1 as u32) << 8 | c.2 as u32
}

#[inline]
fn alpha_mul_argb(c: u32, alpha: u8) -> u32 {
    let a0 = ((c >> 24) & 0xFF) as u32;
    let r0 = ((c >> 16) & 0xFF) as u32;
    let g0 = ((c >> 8) & 0xFF) as u32;
    let b0 = (c & 0xFF) as u32;

    let a = (a0 * alpha as u32 + 127) / 255;
    let r = (r0 * alpha as u32 + 127) / 255;
    let g = (g0 * alpha as u32 + 127) / 255;
    let b = (b0 * alpha as u32 + 127) / 255;

    (a << 24) | (r << 16) | (g << 8) | b
}

#[inline] fn mul_alpha(a: u8, b: u8) -> u8 {
    (((a as u16) * (b as u16) + 127) / 255) as u8
}

fn parse_opacity_u8(v: &[u8]) -> u8 { let mut i = 0; parse_decimal_u8(v, &mut i) }

fn parse_decimal_u8(v: &[u8], i: &mut usize) -> u8 {
    skip_sep(v, i);
    let mut int_p = 0i32;
    while *i < v.len() && v[*i].is_ascii_digit() {
        int_p = int_p.saturating_mul(10).saturating_add((v[*i] - b'0') as i32); *i += 1;
    }
    let mut frac_p = 0i32; let mut frac_s = 1i32;
    if *i < v.len() && v[*i] == b'.' {
        *i += 1;
        while *i < v.len() && v[*i].is_ascii_digit() {
            frac_p = frac_p.saturating_mul(10).saturating_add((v[*i] - b'0') as i32);
            frac_s = frac_s.saturating_mul(10); *i += 1;
        }
    }
    if int_p >= 1 && frac_s <= 1 { return 255; }
    let num = int_p.saturating_mul(frac_s).saturating_add(frac_p);
    if num <= 0 { return 0; } if num >= frac_s { return 255; }
    ((num.saturating_mul(255) + frac_s / 2) / frac_s) as u8
}

// ═══════════════════════════════════════════════════════════════════════════════
// Scanline polygon fill
// ═══════════════════════════════════════════════════════════════════════════════

fn scanline_fill(px: &mut [u32], w: u32, h: u32, subs: &[Vec<(i32,i32)>], rule: FillRule, paint: &Paint) {
    let mut edges: Vec<ScanEdge> = Vec::new();
    for sub in subs {
        if sub.len() < 3 { continue; }
        let n = sub.len();
        for k in 0..n {
            let (x0, y0) = sub[k];
            let (x1, y1) = sub[(k + 1) % n];
            if y0 == y1 { continue; }
            let (ty, tx, by, bx, dir) = if y0 < y1 {
                (y0, x0, y1, x1, 1i32)
            } else {
                (y1, x1, y0, x0, -1i32)
            };
            let dy = (by - ty) as i64;
            if dy == 0 { continue; }
            edges.push(ScanEdge {
                y_top: ty, y_bot: by,
                x: (tx as i64) << 16,
                dx: (((bx - tx) as i64) << 16) / dy,
                dir,
            });
            if edges.len() >= MAX_SCAN_EDGES { break; }
        }
        if edges.len() >= MAX_SCAN_EDGES { break; }
    }
    if edges.is_empty() { return; }
    edges.sort_by_key(|e| e.y_top);
    let y_min = edges[0].y_top.max(0);
    let y_max = edges.iter().map(|e| e.y_bot).max().unwrap_or(0).min(h as i32 - 1);
    let mut active: Vec<(i64, i64, i32, i32)> = Vec::new(); // (x, dx, y_bot, dir)
    let mut ei = 0;
    let mut xs: Vec<(i64, i32)> = Vec::new();
    for y in y_min..=y_max {
        // Add new edges
        while ei < edges.len() && edges[ei].y_top <= y {
            let e = &edges[ei]; ei += 1;
            if e.y_bot > y {
                let steps = (y - e.y_top) as i64;
                active.push((e.x + steps * e.dx, e.dx, e.y_bot, e.dir));
            }
        }
        // Remove expired and collect
        xs.clear();
        let mut j = 0;
        while j < active.len() {
            if active[j].2 <= y { active.swap_remove(j); continue; }
            xs.push((active[j].0, active[j].3));
            active[j].0 += active[j].1;
            j += 1;
        }
        xs.sort_by_key(|&(x, _)| x);
        match rule {
            FillRule::EvenOdd => {
                let mut k = 0;
                while k + 1 < xs.len() {
                    let xa = (xs[k].0 >> 16).max(0) as i32;
                    let xb = (xs[k+1].0 >> 16).min(w as i64 - 1) as i32;
                    fill_span(px, w, y, xa, xb, paint);
                    k += 2;
                }
            }
            FillRule::NonZero => {
                let mut winding = 0i32;
                let mut fill_x = 0i64;
                for k in 0..xs.len() {
                    let old_w = winding;
                    winding += xs[k].1;
                    if old_w == 0 && winding != 0 {
                        fill_x = xs[k].0;
                    } else if old_w != 0 && winding == 0 {
                        let xa = (fill_x >> 16).max(0) as i32;
                        let xb = (xs[k].0 >> 16).min(w as i64 - 1) as i32;
                        fill_span(px, w, y, xa, xb, paint);
                    }
                }
            }
        }
    }
}

fn fill_span(px: &mut [u32], w: u32, y: i32, xa: i32, xb: i32, paint: &Paint) {
    if y < 0 || y >= px.len() as i32 / w as i32 { return; }
    for x in xa..=xb {
        if x < 0 || x >= w as i32 { continue; }
        let idx = (y as u32 * w + x as u32) as usize;
        if idx < px.len() {
            let color = paint_color_at(paint, x, y);
            px[idx] = alpha_blend_over(px[idx], color);
        }
    }
}

fn paint_color_at(paint: &Paint, x: i32, y: i32) -> u32 {
    match paint {
        Paint::Solid(c) => *c,
        Paint::LinearGrad { x1, y1, x2, y2, stops } => {
            if stops.is_empty() { return 0; }
            if stops.len() == 1 { return stops[0].1; }
            let dx = (*x2 - *x1) as i64; let dy = (*y2 - *y1) as i64;
            let len2 = dx * dx + dy * dy;
            if len2 == 0 { return stops[0].1; }
            let rx = (x - *x1) as i64; let ry = (y - *y1) as i64;
            let dot = rx * dx + ry * dy;
            let t = ((dot * 255 + len2 / 2) / len2).clamp(0, 255) as u8;
            interp_stops(stops, t)
        }
        Paint::RadialGrad { cx, cy, fx, fy, rx, ry, stops } => {
            if stops.is_empty() { return 0; }
            if stops.len() == 1 { return stops[0].1; }
            let dx = (x - *fx) as i64;
            let dy = (y - *fy) as i64;
            let rx = (*rx).max(1) as i64;
            let ry = (*ry).max(1) as i64;
            let nx = (dx * FP_ONE as i64) / rx;
            let ny = (dy * FP_ONE as i64) / ry;
            let dist = isqrt64(nx * nx + ny * ny);
            let center_bias_x = ((fx - cx).abs() as i64 * FP_ONE as i64) / rx;
            let center_bias_y = ((fy - cy).abs() as i64 * FP_ONE as i64) / ry;
            let bias = center_bias_x.max(center_bias_y).min(FP_ONE as i64 / 2);
            let dist = dist.saturating_sub(bias);
            let t = ((dist * 255 + (FP_ONE as i64 / 2)) / FP_ONE as i64).clamp(0, 255) as u8;
            interp_stops(stops, t)
        }
        Paint::Pattern { pixels, width, height, origin_x, origin_y } => {
            if *width == 0 || *height == 0 || pixels.is_empty() { return 0; }
            let tx = rem_euclid_i32(x - *origin_x, *width as i32) as usize;
            let ty = rem_euclid_i32(y - *origin_y, *height as i32) as usize;
            let idx = ty.saturating_mul(*width as usize).saturating_add(tx);
            pixels.get(idx).copied().unwrap_or(0)
        }
    }
}

fn interp_stops(stops: &[(u8, u32)], t: u8) -> u32 {
    if stops.is_empty() { return 0; }
    if t <= stops[0].0 { return stops[0].1; }
    if t >= stops[stops.len() - 1].0 { return stops[stops.len() - 1].1; }
    for i in 1..stops.len() {
        if t <= stops[i].0 {
            let (t0, c0) = stops[i - 1];
            let (t1, c1) = stops[i];
            let range = (t1 - t0) as u32;
            if range == 0 { return c1; }
            let frac = (t - t0) as u32;
            return lerp_argb(c0, c1, frac, range);
        }
    }
    stops[stops.len() - 1].1
}

fn lerp_argb(c0: u32, c1: u32, num: u32, den: u32) -> u32 {
    let l = |s0: u32, s1: u32| -> u32 { (s0 * (den - num) + s1 * num + den / 2) / den };
    let a = l((c0 >> 24) & 0xFF, (c1 >> 24) & 0xFF);
    let r = l((c0 >> 16) & 0xFF, (c1 >> 16) & 0xFF);
    let g = l((c0 >> 8) & 0xFF, (c1 >> 8) & 0xFF);
    let b = l(c0 & 0xFF, c1 & 0xFF);
    (a << 24) | (r << 16) | (g << 8) | b
}

// ═══════════════════════════════════════════════════════════════════════════════
// Stroke rendering
// ═══════════════════════════════════════════════════════════════════════════════

fn stroke_polyline(
    px: &mut [u32], w: u32, h: u32,
    pts: &[(i32, i32)], closed: bool,
    paint: &Paint, sw: i32, cap: LineCap, join: LineJoin,
) {
    if pts.len() < 2 || sw <= 0 { return; }
    let hw = sw / 2;
    for i in 0..pts.len() - 1 {
        thick_line(px, w, h, pts[i].0, pts[i].1, pts[i+1].0, pts[i+1].1, paint, sw);
    }
    if closed && pts.len() >= 3 {
        let n = pts.len();
        thick_line(px, w, h, pts[n-1].0, pts[n-1].1, pts[0].0, pts[0].1, paint, sw);
    }
    // Joins
    let use_circles = join == LineJoin::Round || sw >= 3;
    if use_circles {
        let start = if closed { 0 } else { 1 };
        let end = if closed { pts.len() } else { pts.len() - 1 };
        for i in start..end {
            fill_circle_paint(px, w, h, pts[i].0, pts[i].1, hw, paint);
        }
    }
    // Caps
    if !closed {
        match cap {
            LineCap::Round | LineCap::Square => {
                fill_circle_paint(px, w, h, pts[0].0, pts[0].1, hw, paint);
                let n = pts.len();
                fill_circle_paint(px, w, h, pts[n-1].0, pts[n-1].1, hw, paint);
            }
            LineCap::Butt => {}
        }
    }
}

fn thick_line(px: &mut [u32], w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, paint: &Paint, t: i32) {
    let hw = t / 2;
    let dx = (x1 - x0).abs(); let dy = (y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx - dy;
    let mut cx = x0; let mut cy = y0;
    loop {
        for ty in -hw..=hw {
            let py = cy + ty;
            if py < 0 || py >= h as i32 { continue; }
            for tx in -hw..=hw {
                let ppx = cx + tx;
                if ppx < 0 || ppx >= w as i32 { continue; }
                let idx = (py as u32 * w + ppx as u32) as usize;
                if idx < px.len() {
                    let color = paint_color_at(paint, ppx, py);
                    px[idx] = alpha_blend_over(px[idx], color);
                }
            }
        }
        if cx == x1 && cy == y1 { break; }
        let e2 = 2 * err;
        if e2 > -dy { err -= dy; cx += sx; }
        if e2 < dx { err += dx; cy += sy; }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Drawing primitives
// ═══════════════════════════════════════════════════════════════════════════════

fn fill_rect_paint(px: &mut [u32], w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, paint: &Paint) {
    let xa = x0.max(0); let ya = y0.max(0);
    let xb = x1.min(w as i32 - 1); let yb = y1.min(h as i32 - 1);
    if xa > xb || ya > yb { return; }
    for y in ya..=yb {
        for x in xa..=xb {
            let idx = (y as u32 * w + x as u32) as usize;
            if idx < px.len() {
                let color = paint_color_at(paint, x, y);
                px[idx] = alpha_blend_over(px[idx], color);
            }
        }
    }
}

fn rect_outline_paint(px: &mut [u32], w: u32, h: u32, x0: i32, y0: i32, x1: i32, y1: i32, sw: i32, paint: &Paint) {
    let t = sw.max(1);
    fill_rect_paint(px, w, h, x0, y0, x1, y0 + t - 1, paint);
    fill_rect_paint(px, w, h, x0, y1 - t + 1, x1, y1, paint);
    fill_rect_paint(px, w, h, x0, y0, x0 + t - 1, y1, paint);
    fill_rect_paint(px, w, h, x1 - t + 1, y0, x1, y1, paint);
}

fn fill_circle_paint(px: &mut [u32], w: u32, h: u32, ccx: i32, ccy: i32, r: i32, paint: &Paint) {
    if r <= 0 { return; }
    let r2 = r as i64 * r as i64;
    for dy in -r..=r {
        let y = ccy + dy;
        if y < 0 || y >= h as i32 { continue; }
        let dx = isqrt64(r2 - dy as i64 * dy as i64) as i32;
        let xa = (ccx - dx).max(0);
        let xb = (ccx + dx).min(w as i32 - 1);
        for x in xa..=xb {
            let idx = (y as u32 * w + x as u32) as usize;
            if idx < px.len() {
                let color = paint_color_at(paint, x, y);
                px[idx] = alpha_blend_over(px[idx], color);
            }
        }
    }
}

fn fill_ellipse_paint(px: &mut [u32], w: u32, h: u32, ccx: i32, ccy: i32, rx: i32, ry: i32, paint: &Paint) {
    if rx <= 0 || ry <= 0 { return; }
    for dy in -ry..=ry {
        let y = ccy + dy;
        if y < 0 || y >= h as i32 { continue; }
        let dx = ((rx as i64) * isqrt64((ry as i64 * ry as i64 - dy as i64 * dy as i64).max(0)) / (ry as i64).max(1)) as i32;
        let xa = (ccx - dx).max(0);
        let xb = (ccx + dx).min(w as i32 - 1);
        for x in xa..=xb {
            let idx = (y as u32 * w + x as u32) as usize;
            if idx < px.len() {
                let color = paint_color_at(paint, x, y);
                px[idx] = alpha_blend_over(px[idx], color);
            }
        }
    }
}

fn circle_outline_paint(px: &mut [u32], w: u32, h: u32, ccx: i32, ccy: i32, r: i32, sw: i32, paint: &Paint) {
    if r <= 0 { return; }
    let ro = r; let ri = (r - sw).max(0);
    let ro2 = ro as i64 * ro as i64; let ri2 = ri as i64 * ri as i64;
    for dy in -ro..=ro {
        let y = ccy + dy;
        if y < 0 || y >= h as i32 { continue; }
        let dy2 = dy as i64 * dy as i64;
        let dxo = isqrt64((ro2 - dy2).max(0)) as i32;
        let dxi = if dy2 <= ri2 { isqrt64(ri2 - dy2) as i32 } else { 0 };
        for dx in [-1i32, 1] {
            let (start, end) = if dx < 0 { (-dxo, -dxi) } else { (dxi, dxo) };
            for ddx in start..=end {
                let x = ccx + ddx;
                if x < 0 || x >= w as i32 { continue; }
                let idx = (y as u32 * w + x as u32) as usize;
                if idx < px.len() {
                    let color = paint_color_at(paint, x, y);
                    px[idx] = alpha_blend_over(px[idx], color);
                }
            }
        }
    }
}

fn ellipse_outline_paint(px: &mut [u32], w: u32, h: u32, ccx: i32, ccy: i32, rx: i32, ry: i32, sw: i32, paint: &Paint) {
    if rx <= 0 || ry <= 0 { return; }
    let orx = rx; let ory = ry;
    let irx = (rx - sw).max(0); let iry = (ry - sw).max(0);
    for dy in -ory..=ory {
        let y = ccy + dy;
        if y < 0 || y >= h as i32 { continue; }
        let dxo = ((orx as i64) * isqrt64((ory as i64 * ory as i64 - dy as i64 * dy as i64).max(0)) / (ory as i64).max(1)) as i32;
        let dxi = if irx > 0 && iry > 0 && (dy.abs() as i64) < iry as i64 {
            ((irx as i64) * isqrt64((iry as i64 * iry as i64 - dy as i64 * dy as i64).max(0)) / (iry as i64).max(1)) as i32
        } else { 0 };
        for ddx in -dxo..=-dxi {
            let x = ccx + ddx;
            if x >= 0 && x < w as i32 {
                let idx = (y as u32 * w + x as u32) as usize;
                if idx < px.len() { px[idx] = alpha_blend_over(px[idx], paint_color_at(paint, x, y)); }
            }
        }
        for ddx in dxi..=dxo {
            let x = ccx + ddx;
            if x >= 0 && x < w as i32 {
                let idx = (y as u32 * w + x as u32) as usize;
                if idx < px.len() { px[idx] = alpha_blend_over(px[idx], paint_color_at(paint, x, y)); }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Alpha blending
// ═══════════════════════════════════════════════════════════════════════════════

#[inline]
fn alpha_blend_over(dst: u32, src: u32) -> u32 {
    let sa = (src >> 24) & 0xFF;
    if sa == 0 { return dst; }
    if sa == 255 { return src; }
    let da = (dst >> 24) & 0xFF;
    let inv = 255 - sa;
    let oa = sa + (da * inv + 127) / 255;
    let blend = |sc: u32, dc: u32| -> u32 { (sc * sa + dc * inv + 127) / 255 };
    let or = blend((src >> 16) & 0xFF, (dst >> 16) & 0xFF);
    let og = blend((src >> 8) & 0xFF, (dst >> 8) & 0xFF);
    let ob = blend(src & 0xFF, dst & 0xFF);
    (oa << 24) | (or << 16) | (og << 8) | ob
}

// ═══════════════════════════════════════════════════════════════════════════════
// Number parsing
// ═══════════════════════════════════════════════════════════════════════════════

fn pn_fp(d: &[u8], i: &mut usize) -> i32 {
    let v = parse_number_raw_fp(d, i);
    while *i < d.len() && d[*i].is_ascii_alphabetic() { *i += 1; }
    if *i < d.len() && d[*i] == b'%' { *i += 1; }
    v
}

fn parse_number_raw_fp(d: &[u8], i: &mut usize) -> i32 {
    skip_sep(d, i);
    let neg = if *i < d.len() && d[*i] == b'-' { *i += 1; true }
              else if *i < d.len() && d[*i] == b'+' { *i += 1; false }
              else { false };
    let mut ip: i64 = 0;
    while *i < d.len() && d[*i].is_ascii_digit() {
        ip = ip.saturating_mul(10).saturating_add((d[*i] - b'0') as i64); *i += 1;
    }
    let mut fp: i64 = 0; let mut fd: i64 = 1;
    if *i < d.len() && d[*i] == b'.' {
        *i += 1;
        while *i < d.len() && d[*i].is_ascii_digit() {
            fp = fp.saturating_mul(10).saturating_add((d[*i] - b'0') as i64);
            fd = fd.saturating_mul(10); *i += 1;
        }
    }
    let mut exp: i32 = 0;
    if *i < d.len() && (d[*i] == b'e' || d[*i] == b'E') {
        *i += 1;
        let en = if *i < d.len() && d[*i] == b'-' { *i += 1; true }
                 else if *i < d.len() && d[*i] == b'+' { *i += 1; false }
                 else { false };
        let mut e = 0i32;
        while *i < d.len() && d[*i].is_ascii_digit() {
            e = e.saturating_mul(10).saturating_add((d[*i] - b'0') as i32); *i += 1;
        }
        exp = if en { -e } else { e };
    }
    let mut num = ip * FP_ONE as i64 + fp * FP_ONE as i64 / fd.max(1);
    if exp > 0 { for _ in 0..exp.min(9) { num = num.saturating_mul(10); } }
    else if exp < 0 { for _ in 0..(-exp).min(9) { num /= 10; } }
    let v = num.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if neg { -v } else { v }
}

fn peek_n(d: &[u8], mut i: usize) -> bool {
    while i < d.len() && (is_ws(d[i]) || d[i] == b',') { i += 1; }
    if i >= d.len() { return false; }
    d[i].is_ascii_digit() || d[i] == b'-' || d[i] == b'+' || d[i] == b'.'
}

fn skip_sep(d: &[u8], i: &mut usize) {
    while *i < d.len() && (is_ws(d[*i]) || d[*i] == b',') { *i += 1; }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Utilities
// ═══════════════════════════════════════════════════════════════════════════════

#[inline] fn is_ws(b: u8) -> bool { b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' }

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while !s.is_empty() && is_ws(s[0]) { s = &s[1..]; }
    while !s.is_empty() && is_ws(s[s.len() - 1]) { s = &s[..s.len() - 1]; }
    s
}

#[inline] fn starts_with_ic(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

#[inline] fn rem_euclid_i32(v: i32, m: i32) -> i32 {
    if m <= 0 { return 0; }
    let r = v % m;
    if r < 0 { r + m } else { r }
}

fn isqrt64(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let mut x = n; let mut y = (x + 1) / 2;
    while y < x { x = y; y = (x + n / x) / 2; }
    x
}

fn parse_points_list(raw: &[u8], ctx: Tx) -> Vec<(i32, i32)> {
    let mut pts = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if !peek_n(raw, i) { break; }
        let x = pn_fp(raw, &mut i);
        let y = pn_fp(raw, &mut i);
        pts.push(tx_to_px(ctx, x, y));
    }
    pts
}

fn points_bbox_fp(raw: &[u8], _ctx: Tx) -> (i32, i32, i32, i32) {
    let mut minx = i32::MAX; let mut miny = i32::MAX;
    let mut maxx = i32::MIN; let mut maxy = i32::MIN;
    let mut i = 0;
    while i < raw.len() {
        if !peek_n(raw, i) { break; }
        let x = pn_fp(raw, &mut i);
        let y = pn_fp(raw, &mut i);
        minx = minx.min(x); miny = miny.min(y);
        maxx = maxx.max(x); maxy = maxy.max(y);
    }
    if minx > maxx { (0, 0, 0, 0) } else { (minx, miny, maxx, maxy) }
}