#![allow(static_mut_refs)]

// Minimal SVG rasteriser for ATXF embedded icons.
//
// Supported:
//   • Elements: rect, circle, ellipse, path, polygon, polyline, line
//   • Group translate transforms
//   • Attributes: fill, stroke, stroke-width, fill-opacity, opacity
//   • Path commands: M m L l H h V v Z z C c S s Q q A a (bezier/arc → linear approx)
//   • Colors: #RGB, #RRGGBB and named colors (black/white/red/green/blue/yellow…)
//
// Not supported: gradients, clip-paths, text, patterns, complex matrix transforms.
//
// Pixel format: ARGB u32 (0xAARRGGBB). A=0 = transparent.

use alloc::vec::Vec;
use atom_syscall::graphics::{Color, Framebuffer};

const FP_ONE: i32 = 1024;

#[derive(Clone, Copy)]
struct Tx {
    sx: i32,
    sy: i32,
    tx: i32,
    ty: i32,
}

struct CssRule {
    class_name: Vec<u8>,
    prop: Vec<u8>,
    value: Vec<u8>,
}

struct PaintRef {
    id: Vec<u8>,
    color: (u8, u8, u8, u8),
}

static mut CSS_RULES: Option<Vec<CssRule>> = None;
static mut PAINT_REFS: Option<Vec<PaintRef>> = None;

impl Tx {
    const fn identity() -> Self {
        Self { sx: FP_ONE, sy: FP_ONE, tx: 0, ty: 0 }
    }
}

/// Rendered icon bitmap in ARGB format (0xAARRGGBB).
/// Transparent pixels (A < 128) are skipped when blitting.
pub struct SvgBitmap {
    pub pixels: Vec<u32>,
    pub width:  u32,
    pub height: u32,
}

impl SvgBitmap {
    /// Render SVG bytes to a bitmap of `out_w` × `out_h` pixels.
    /// Returns None if the dimensions are zero.
    pub fn render(svg: &[u8], out_w: u32, out_h: u32) -> Option<Self> {
        if out_w == 0 || out_h == 0 { return None; }
        let n = (out_w * out_h) as usize;
        let mut pixels = Vec::with_capacity(n);
        for _ in 0..n { pixels.push(0u32); }
        render_svg(svg, out_w, out_h, &mut pixels);
        Some(SvgBitmap { pixels, width: out_w, height: out_h })
    }

    /// Alpha-blit onto a Framebuffer at position (dx, dy).
    /// Transparent pixels are skipped so the background shows through.
    pub fn blit_fb(&self, fb: &Framebuffer, dx: u32, dy: u32) {
        for py in 0..self.height {
            for px in 0..self.width {
                let p = self.pixels[(py * self.width + px) as usize];
                if p >> 24 != 0 {
                    fb.draw_pixel(
                        dx + px, dy + py,
                        Color::new((p >> 16) as u8, (p >> 8) as u8, p as u8),
                    );
                }
            }
        }
    }

}

// ─── Main renderer ─────────────────────────────────────────────────────────

fn render_svg(svg: &[u8], out_w: u32, out_h: u32, px: &mut Vec<u32>) {
    unsafe {
        CSS_RULES = Some(parse_css_rules(svg));
        PAINT_REFS = Some(parse_paint_refs(svg));
    }

    let (vbw, vbh) = parse_viewbox(svg);
    let vbw = if vbw > 0 { vbw } else { out_w as i32 };
    let vbh = if vbh > 0 { vbh } else { out_h as i32 };
    let ow = out_w as i32;
    let oh = out_h as i32;

    // Transform stack (scale + translate in output-pixel space)
    let mut tstk: [Tx; 8] = [Tx::identity(); 8];
    let mut depth = 0usize;
    let mut skip_depth = 0usize;

    let mut i = 0usize;
    while i < svg.len() {
        if svg[i] != b'<' { i += 1; continue; }
        i += 1;
        if i >= svg.len() { break; }

        // Skip processing instructions, comments, DOCTYPE
        if svg[i] == b'?' || svg[i] == b'!' {
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1;
            continue;
        }

        // Close tag
        if svg[i] == b'/' {
            i += 1;
            let ts = i;
            while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' { i += 1; }
            if skip_depth > 0 {
                skip_depth -= 1;
            } else if &svg[ts..i] == b"g" && depth > 0 {
                depth -= 1;
            }
            while i < svg.len() && svg[i] != b'>' { i += 1; }
            i += 1;
            continue;
        }

        // Tag name
        let ts = i;
        while i < svg.len() && !is_ws(svg[i]) && svg[i] != b'>' && svg[i] != b'/' {
            i += 1;
        }
        let tag = &svg[ts..i];

        // Attributes raw slice (up to but not including '>')
        let as0 = i;
        let mut self_close = false;
        while i < svg.len() && svg[i] != b'>' {
            if svg[i] == b'/' { self_close = true; }
            i += 1;
        }
        let attrs = &svg[as0..i];
        i += 1; // skip '>'

        if skip_depth > 0 {
            if !self_close { skip_depth += 1; }
            continue;
        }

        let skip_tag = matches!(tag,
            b"defs" | b"filter" | b"linearGradient" | b"radialGradient" |
            b"clipPath" | b"mask" | b"pattern" | b"symbol" | b"metadata"
        );
        if skip_tag || is_display_none(attrs) {
            if !self_close { skip_depth = 1; }
            continue;
        }

        let ctx = cur_tx(&tstk, depth);

        match tag {
            b"g" if !self_close => {
                if depth < 8 {
                    tstk[depth] = parse_transform(attrs, vbw, vbh, ow, oh);
                    depth += 1;
                }
            }
            b"rect"     => elem_rect(px, ow, oh, vbw, vbh, attrs, ctx),
            b"circle"   => elem_circle(px, ow, oh, vbw, vbh, attrs, ctx),
            b"ellipse"  => elem_ellipse(px, ow, oh, vbw, vbh, attrs, ctx),
            b"line"     => elem_line(px, ow, oh, vbw, vbh, attrs, ctx),
            b"path"     => elem_path(px, ow, oh, vbw, vbh, attrs, ctx),
            b"polygon"  => elem_poly(px, ow, oh, vbw, vbh, attrs, ctx, true),
            b"polyline" => elem_poly(px, ow, oh, vbw, vbh, attrs, ctx, false),
            _ => {}
        }
    }

    unsafe {
        CSS_RULES = None;
        PAINT_REFS = None;
    }
}

fn cur_tx(stk: &[Tx; 8], depth: usize) -> Tx {
    let mut out = Tx::identity();
    for i in 0..depth {
        out = compose_tx(out, stk[i]);
    }
    out
}

fn compose_tx(a: Tx, b: Tx) -> Tx {
    Tx {
        sx: fp_mul(a.sx, b.sx),
        sy: fp_mul(a.sy, b.sy),
        tx: a.tx + fp_mul(a.sx, b.tx),
        ty: a.ty + fp_mul(a.sy, b.ty),
    }
}

#[inline]
fn fp_mul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) / FP_ONE as i64) as i32
}

#[inline]
fn apply_tx(ctx: Tx, x: i32, y: i32) -> (i32, i32) {
    (fp_mul(x, ctx.sx) + ctx.tx, fp_mul(y, ctx.sy) + ctx.ty)
}

#[inline]
fn map_xy(ctx: Tx, x_svg: i32, y_svg: i32, vbw: i32, vbh: i32, ow: i32, oh: i32) -> (i32, i32) {
    let x = sc(x_svg, vbw, ow);
    let y = sc(y_svg, vbh, oh);
    apply_tx(ctx, x, y)
}

// ─── Element renderers ──────────────────────────────────────────────────────

fn elem_rect(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx,
) {
    let x = attr_i(attrs, b"x");
    let y = attr_i(attrs, b"y");
    let w = attr_i(attrs, b"width");
    let h = attr_i(attrs, b"height");
    if w <= 0 || h <= 0 { return; }

    let (x0, y0) = map_xy(ctx, x, y, vbw, vbh, ow, oh);
    let (x1, y1) = map_xy(ctx, x + w, y + h, vbw, vbh, ow, oh);

    if let Some(fc) = fill_color(attrs) {
        fill_rect_px(px, ow as u32, oh as u32, x0, y0, x1, y1, fc);
    }
    if let Some((sc_col, sw)) = stroke_info(attrs, vbw, ow) {
        rect_outline(px, ow as u32, oh as u32, x0, y0, x1, y1, sw, sc_col);
    }
}

fn elem_circle(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx,
) {
    let cx = attr_i(attrs, b"cx");
    let cy = attr_i(attrs, b"cy");
    let r  = attr_i(attrs, b"r");
    if r <= 0 { return; }
    let (pcx, pcy) = map_xy(ctx, cx, cy, vbw, vbh, ow, oh);
    let rx = fp_mul(sc(r, vbw, ow), ctx.sx.abs());
    let ry = fp_mul(sc(r, vbh, oh), ctx.sy.abs());
    let pr  = rx.max(ry);

    if let Some(fc) = fill_color(attrs) {
        fill_circle_px(px, ow as u32, oh as u32, pcx, pcy, pr, fc);
    }
    if let Some((sc_col, sw)) = stroke_info(attrs, vbw, ow) {
        circle_outline(px, ow as u32, oh as u32, pcx, pcy, pr, sw, sc_col);
    }
}

fn elem_ellipse(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx,
) {
    let cx = attr_i(attrs, b"cx");
    let cy = attr_i(attrs, b"cy");
    let rx = attr_i(attrs, b"rx");
    let ry = attr_i(attrs, b"ry");
    if rx <= 0 || ry <= 0 { return; }
    let (pcx, pcy) = map_xy(ctx, cx, cy, vbw, vbh, ow, oh);
    let prx = fp_mul(sc(rx, vbw, ow), ctx.sx.abs());
    let pry = fp_mul(sc(ry, vbh, oh), ctx.sy.abs());
    let pr  = (prx + pry) / 2;

    if let Some(fc) = fill_color(attrs) {
        fill_circle_px(px, ow as u32, oh as u32, pcx, pcy, pr, fc);
    }
}

fn elem_line(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx,
) {
    let (x1, y1) = map_xy(ctx, attr_i(attrs, b"x1"), attr_i(attrs, b"y1"), vbw, vbh, ow, oh);
    let (x2, y2) = map_xy(ctx, attr_i(attrs, b"x2"), attr_i(attrs, b"y2"), vbw, vbh, ow, oh);
    if let Some((sc_col, sw)) = stroke_info(attrs, vbw, ow) {
        thick_line(px, ow as u32, oh as u32, x1, y1, x2, y2, sc_col, sw);
    }
}

fn elem_poly(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx, closed: bool,
) {
    let raw = match get_attr(attrs, b"points") { Some(v) => v, None => return };
    let pts = parse_points(raw, vbw, vbh, ow, oh, ctx);
    if let Some(fc) = fill_color(attrs) {
        if pts.len() >= 3 { fill_poly_px(px, ow as u32, oh as u32, &pts, fc); }
    }
    if let Some((sc_col, sw)) = stroke_info(attrs, vbw, ow) {
        stroke_lines(px, ow as u32, oh as u32, &pts, closed, sc_col, sw);
    }
}

fn elem_path(
    px: &mut Vec<u32>, ow: i32, oh: i32, vbw: i32, vbh: i32,
    attrs: &[u8], ctx: Tx,
) {
    let d = match get_attr(attrs, b"d") { Some(v) => v, None => return };
    let has_fill   = fill_color(attrs).is_some();
    let has_stroke = stroke_info(attrs, vbw, ow).is_some();
    if !has_fill && !has_stroke { return; }

    // Walk path commands, accumulating subpaths
    let mut i = 0usize;
    let mut cx = 0i32; let mut cy = 0i32;
    let mut sx = 0i32; let mut sy = 0i32; // subpath start
    let mut sub: Vec<(i32, i32)> = Vec::new();

    let flush = |sub: &mut Vec<(i32,i32)>, closed: bool,
                  px: &mut Vec<u32>, ow: i32, oh: i32, attrs: &[u8], vbw: i32| {
        if sub.is_empty() { return; }
        if let Some(fc) = fill_color(attrs) {
            if sub.len() >= 3 {
                fill_poly_px(px, ow as u32, oh as u32, sub, fc);
            }
        }
        if let Some((sc_col, sw)) = stroke_info(attrs, vbw, ow) {
            stroke_lines(px, ow as u32, oh as u32, sub, closed, sc_col, sw);
        }
    };

    while i < d.len() {
        skip_sep(d, &mut i);
        if i >= d.len() { break; }
        let cmd = d[i]; i += 1;
        match cmd {
            b'M' => {
                flush(&mut sub, false, px, ow, oh, attrs, vbw);
                sub.clear();
                let x = pn(d, &mut i); let y = pn(d, &mut i);
                cx = x; cy = y; sx = cx; sy = cy;
                sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                while i < d.len() && peek_n(d, i) {
                    let x = pn(d, &mut i); let y = pn(d, &mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'm' => {
                flush(&mut sub, false, px, ow, oh, attrs, vbw);
                sub.clear();
                let dx = pn(d, &mut i); let dy = pn(d, &mut i);
                cx += dx; cy += dy; sx = cx; sy = cy;
                sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                while i < d.len() && peek_n(d, i) {
                    let dx = pn(d, &mut i); let dy = pn(d, &mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'L' => {
                while i < d.len() && peek_n(d, i) {
                    let x = pn(d, &mut i); let y = pn(d, &mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'l' => {
                while i < d.len() && peek_n(d, i) {
                    let dx = pn(d, &mut i); let dy = pn(d, &mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'H' => {
                while i < d.len() && peek_n(d, i) {
                    cx = pn(d, &mut i);
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'h' => {
                while i < d.len() && peek_n(d, i) {
                    cx += pn(d, &mut i);
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'V' => {
                while i < d.len() && peek_n(d, i) {
                    cy = pn(d, &mut i);
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'v' => {
                while i < d.len() && peek_n(d, i) {
                    cy += pn(d, &mut i);
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            // Cubic bezier – take endpoint only
            b'C' => {
                while i < d.len() && peek_n(d, i) {
                    let _x1 = pn(d,&mut i); let _y1 = pn(d,&mut i);
                    let _x2 = pn(d,&mut i); let _y2 = pn(d,&mut i);
                    let x = pn(d,&mut i); let y = pn(d,&mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'c' => {
                while i < d.len() && peek_n(d, i) {
                    let _x1 = pn(d,&mut i); let _y1 = pn(d,&mut i);
                    let _x2 = pn(d,&mut i); let _y2 = pn(d,&mut i);
                    let dx = pn(d,&mut i); let dy = pn(d,&mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            // Smooth cubic bezier
            b'S' => {
                while i < d.len() && peek_n(d, i) {
                    let _x2 = pn(d,&mut i); let _y2 = pn(d,&mut i);
                    let x = pn(d,&mut i); let y = pn(d,&mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b's' => {
                while i < d.len() && peek_n(d, i) {
                    let _x2 = pn(d,&mut i); let _y2 = pn(d,&mut i);
                    let dx = pn(d,&mut i); let dy = pn(d,&mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            // Quadratic bezier
            b'Q' => {
                while i < d.len() && peek_n(d, i) {
                    let _x1 = pn(d,&mut i); let _y1 = pn(d,&mut i);
                    let x = pn(d,&mut i); let y = pn(d,&mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'q' => {
                while i < d.len() && peek_n(d, i) {
                    let _x1 = pn(d,&mut i); let _y1 = pn(d,&mut i);
                    let dx = pn(d,&mut i); let dy = pn(d,&mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            // Arc – approximate as line to endpoint
            b'A' => {
                while i < d.len() && peek_n(d, i) {
                    let _rx = pn(d,&mut i); let _ry = pn(d,&mut i);
                    let _xa = pn(d,&mut i);
                    let _la = pn(d,&mut i); let _sw = pn(d,&mut i);
                    let x = pn(d,&mut i); let y = pn(d,&mut i);
                    cx = x; cy = y;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'a' => {
                while i < d.len() && peek_n(d, i) {
                    let _rx = pn(d,&mut i); let _ry = pn(d,&mut i);
                    let _xa = pn(d,&mut i);
                    let _la = pn(d,&mut i); let _sw = pn(d,&mut i);
                    let dx = pn(d,&mut i); let dy = pn(d,&mut i);
                    cx += dx; cy += dy;
                    sub.push(map_xy(ctx, cx, cy, vbw, vbh, ow, oh));
                }
            }
            b'Z' | b'z' => {
                cx = sx; cy = sy;
                flush(&mut sub, true, px, ow, oh, attrs, vbw);
                sub.clear();
            }
            _ => {} // unknown / whitespace
        }
    }
    // Flush open path at end
    if !sub.is_empty() {
        flush(&mut sub, false, px, ow, oh, attrs, vbw);
    }
}

// ─── Color helpers ───────────────────────────────────────────────────────────

/// ARGB fill color from element attributes, or None to skip fill.
fn fill_color(attrs: &[u8]) -> Option<u32> {
    let v = match get_attr(attrs, b"fill") {
        Some(v) => v,
        None => b"black", // SVG default fill is black
    };
    if v == b"none" || v.starts_with(b"url(") { return None; }

    let (r, g, b, base_a) = parse_color_str(v)?;
    let fill_op = get_attr(attrs, b"fill-opacity")
        .map(parse_opacity_u8)
        .unwrap_or(255);
    let op = get_attr(attrs, b"opacity")
        .map(parse_opacity_u8)
        .unwrap_or(255);
    let a = mul_alpha(mul_alpha(base_a, fill_op), op);
    if a == 0 { return None; }
    Some(((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32)
}

/// Stroke color + pixel width from element attributes, or None if no stroke.
fn stroke_info(attrs: &[u8], vbw: i32, ow: i32) -> Option<(u32, i32)> {
    let v = get_attr(attrs, b"stroke")?;
    if v == b"none" || v.starts_with(b"url(") { return None; }
    let (r, g, b, base_a) = parse_color_str(v)?;
    let stroke_op = get_attr(attrs, b"stroke-opacity")
        .map(parse_opacity_u8)
        .unwrap_or(255);
    let op = get_attr(attrs, b"opacity")
        .map(parse_opacity_u8)
        .unwrap_or(255);
    let a = mul_alpha(mul_alpha(base_a, stroke_op), op);
    if a == 0 { return None; }
    let sw_svg = get_attr(attrs, b"stroke-width")
        .map(parse_i32_b)
        .unwrap_or(1);
    let sw_px = (sw_svg * ow / vbw).max(1);
    Some((((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32, sw_px))
}
fn parse_color_str(v: &[u8]) -> Option<(u8, u8, u8, u8)> {
    let v = trim_ascii(v);
    if v.eq_ignore_ascii_case(b"none") || v.eq_ignore_ascii_case(b"transparent") {
        return None;
    }

    if starts_with_ignore_ascii_case(v, b"url(") {
        return resolve_paint_url(v);
    }

    if v.starts_with(b"#") {
        if v.len() == 7 {
            return Some((
                (hn(v[1]) << 4) | hn(v[2]),
                (hn(v[3]) << 4) | hn(v[4]),
                (hn(v[5]) << 4) | hn(v[6]),
                255,
            ));
        }
        if v.len() == 4 {
            return Some((hn(v[1]) * 17, hn(v[2]) * 17, hn(v[3]) * 17, 255));
        }
        if v.len() == 9 {
            return Some((
                (hn(v[1]) << 4) | hn(v[2]),
                (hn(v[3]) << 4) | hn(v[4]),
                (hn(v[5]) << 4) | hn(v[6]),
                (hn(v[7]) << 4) | hn(v[8]),
            ));
        }
        if v.len() == 5 {
            return Some((
                hn(v[1]) * 17,
                hn(v[2]) * 17,
                hn(v[3]) * 17,
                hn(v[4]) * 17,
            ));
        }
        return None;
    }

    if starts_with_ignore_ascii_case(v, b"rgb(") {
        let mut i = 4usize;
        let r = pn(v, &mut i).clamp(0, 255) as u8;
        let g = pn(v, &mut i).clamp(0, 255) as u8;
        let b = pn(v, &mut i).clamp(0, 255) as u8;
        return Some((r, g, b, 255));
    }

    if starts_with_ignore_ascii_case(v, b"rgba(") {
        let mut i = 5usize;
        let r = pn(v, &mut i).clamp(0, 255) as u8;
        let g = pn(v, &mut i).clamp(0, 255) as u8;
        let b = pn(v, &mut i).clamp(0, 255) as u8;
        let a = parse_decimal_0_1_to_u8_from(v, &mut i);
        return Some((r, g, b, a));
    }

    match v {
        b"black"   => Some((0, 0, 0)),
        b"white"   => Some((255, 255, 255)),
        b"red"     => Some((255, 0, 0)),
        b"lime"    => Some((0, 255, 0)),
        b"green"   => Some((0, 128, 0)),
        b"blue"    => Some((0, 0, 255)),
        b"yellow"  => Some((255, 255, 0)),
        b"cyan"    => Some((0, 255, 255)),
        b"magenta" => Some((255, 0, 255)),
        b"gray" | b"grey" => Some((128, 128, 128)),
        b"silver"  => Some((192, 192, 192)),
        b"orange"  => Some((255, 165, 0)),
        b"purple"  => Some((128, 0, 128)),
        b"maroon"  => Some((128, 0, 0)),
        b"navy"    => Some((0, 0, 128)),
        b"teal"    => Some((0, 128, 128)),
        b"rebeccapurple" => Some((102, 51, 153)),
        _ => None,
    }.map(|(r, g, b)| (r, g, b, 255))
}

#[inline] fn hn(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => 10 + b - b'a',
        b'A'..=b'F' => 10 + b - b'A',
        _ => 0,
    }
}

// ─── Attribute parser ─────────────────────────────────────────────────────

fn get_attr<'a>(attrs: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    if let Some(v) = get_attr_raw(attrs, name) {
        return Some(v);
    }
    let style = get_attr_raw(attrs, b"style")?;
    if let Some(v) = get_style_prop(style, name) {
        return Some(v);
    }
    class_style_prop(attrs, name)
}

fn class_style_prop<'a>(attrs: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let classes = get_attr_raw(attrs, b"class")?;
    unsafe {
        let rules = CSS_RULES.as_ref()?;
        let mut i = rules.len();
        while i > 0 {
            i -= 1;
            let r = &rules[i];
            if !r.prop.eq_ignore_ascii_case(name) {
                continue;
            }
            if class_list_contains(classes, &r.class_name) {
                let p: *const [u8] = r.value.as_slice();
                return Some(&*p);
            }
        }
    }
    None
}

fn class_list_contains(classes: &[u8], class_name: &[u8]) -> bool {
    let mut i = 0usize;
    while i < classes.len() {
        while i < classes.len() && is_ws(classes[i]) { i += 1; }
        if i >= classes.len() { break; }
        let s = i;
        while i < classes.len() && !is_ws(classes[i]) { i += 1; }
        if classes[s..i] == *class_name { return true; }
    }
    false
}

fn parse_css_rules(svg: &[u8]) -> Vec<CssRule> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 6 < svg.len() {
        if svg[i] == b'<' && i + 6 < svg.len() && svg[i + 1..].starts_with(b"style") {
            let mut j = i;
            while j < svg.len() && svg[j] != b'>' { j += 1; }
            if j >= svg.len() { break; }
            let content_start = j + 1;
            let mut k = content_start;
            let mut end = None;
            while k + 8 <= svg.len() {
                if svg[k] == b'<' && svg[k..].starts_with(b"</style>") {
                    end = Some(k);
                    break;
                }
                k += 1;
            }
            let content_end = match end { Some(v) => v, None => break };
            parse_css_block(&svg[content_start..content_end], &mut out);
            i = content_end + 8;
            continue;
        }
        i += 1;
    }
    out
}

fn parse_css_block(block: &[u8], out: &mut Vec<CssRule>) {
    let mut i = 0usize;
    while i < block.len() {
        while i < block.len() && is_ws(block[i]) { i += 1; }
        if i >= block.len() { break; }

        let sel_start = i;
        while i < block.len() && block[i] != b'{' { i += 1; }
        if i >= block.len() { break; }
        let selectors = trim_ascii(&block[sel_start..i]);
        i += 1;

        let decl_start = i;
        while i < block.len() && block[i] != b'}' { i += 1; }
        let decls = &block[decl_start..i];
        if i < block.len() { i += 1; }

        let mut classes: Vec<Vec<u8>> = Vec::new();
        let mut s = 0usize;
        while s < selectors.len() {
            while s < selectors.len() && (is_ws(selectors[s]) || selectors[s] == b',') { s += 1; }
            if s >= selectors.len() { break; }
            if selectors[s] != b'.' {
                while s < selectors.len() && selectors[s] != b',' { s += 1; }
                continue;
            }
            s += 1;
            let cs = s;
            while s < selectors.len() && !is_ws(selectors[s]) && selectors[s] != b',' && selectors[s] != b'{' {
                s += 1;
            }
            if s > cs {
                classes.push(selectors[cs..s].to_vec());
            }
        }

        if classes.is_empty() { continue; }

        let mut d = 0usize;
        while d < decls.len() {
            while d < decls.len() && (is_ws(decls[d]) || decls[d] == b';') { d += 1; }
            if d >= decls.len() { break; }

            let ps = d;
            while d < decls.len() && decls[d] != b':' && decls[d] != b';' { d += 1; }
            if d >= decls.len() || decls[d] != b':' {
                while d < decls.len() && decls[d] != b';' { d += 1; }
                continue;
            }
            let prop = trim_ascii(&decls[ps..d]).to_vec();
            d += 1;
            let vs = d;
            while d < decls.len() && decls[d] != b';' { d += 1; }
            let val = trim_ascii(&decls[vs..d]).to_vec();

            for c in &classes {
                out.push(CssRule {
                    class_name: c.clone(),
                    prop: prop.clone(),
                    value: val.clone(),
                });
            }
        }
    }
}

fn parse_paint_refs(svg: &[u8]) -> Vec<PaintRef> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < svg.len() {
        if svg[i] != b'<' { i += 1; continue; }
        let ts = i + 1;
        let mut t = ts;
        while t < svg.len() && !is_ws(svg[t]) && svg[t] != b'>' && svg[t] != b'/' { t += 1; }
        if t >= svg.len() { break; }
        let tag = &svg[ts..t];
        if tag != b"linearGradient" && tag != b"radialGradient" {
            i += 1;
            continue;
        }

        let mut a_end = t;
        while a_end < svg.len() && svg[a_end] != b'>' { a_end += 1; }
        if a_end >= svg.len() { break; }
        let attrs = &svg[t..a_end];
        let id = match get_attr_raw(attrs, b"id") {
            Some(v) => trim_ascii(v).to_vec(),
            None => { i = a_end + 1; continue; }
        };

        let close_pat: &[u8] = if tag == b"linearGradient" { b"</linearGradient>" } else { b"</radialGradient>" };
        let mut e = a_end + 1;
        let mut close = None;
        while e + close_pat.len() <= svg.len() {
            if &svg[e..e + close_pat.len()] == close_pat {
                close = Some(e);
                break;
            }
            e += 1;
        }
        let end = match close { Some(v) => v, None => { i = a_end + 1; continue; } };
        let block = &svg[a_end + 1..end];

        let mut first: Option<(u8, u8, u8, u8)> = None;
        let mut last: Option<(u8, u8, u8, u8)> = None;
        let mut s = 0usize;
        while s < block.len() {
            if block[s] != b'<' { s += 1; continue; }
            if !block[s + 1..].starts_with(b"stop") { s += 1; continue; }
            let mut se = s + 1;
            while se < block.len() && block[se] != b'>' { se += 1; }
            if se >= block.len() { break; }
            let sattrs = &block[s + 5..se];
            let col = get_attr(sattrs, b"stop-color")
                .and_then(|v| parse_color_str(v));
            if let Some(c) = col {
                if first.is_none() { first = Some(c); }
                last = Some(c);
            }
            s = se + 1;
        }

        if let Some(c) = last.or(first) {
            out.push(PaintRef { id, color: c });
        }

        i = end + close_pat.len();
    }
    out
}

fn resolve_paint_url(v: &[u8]) -> Option<(u8, u8, u8, u8)> {
    let v = trim_ascii(v);
    if !starts_with_ignore_ascii_case(v, b"url(") { return None; }
    let mut i = 4usize;
    while i < v.len() && is_ws(v[i]) { i += 1; }
    if i >= v.len() || v[i] != b'#' { return None; }
    i += 1;
    let s = i;
    while i < v.len() && v[i] != b')' && !is_ws(v[i]) { i += 1; }
    if i <= s { return None; }
    let id = &v[s..i];

    unsafe {
        let refs = PAINT_REFS.as_ref()?;
        for r in refs.iter().rev() {
            if r.id == *id {
                return Some(r.color);
            }
        }
    }
    None
}

fn get_attr_raw<'a>(attrs: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < attrs.len() {
        while i < attrs.len() && is_ws(attrs[i]) { i += 1; }
        if i >= attrs.len() { break; }
        if attrs[i] == b'/' || attrs[i] == b'>' {
            i += 1;
            continue;
        }
        // Read attribute name
        let ns = i;
        while i < attrs.len() && attrs[i] != b'=' && !is_ws(attrs[i]) && attrs[i] != b'>'
              && attrs[i] != b'/' {
            i += 1;
        }
        if ns == i {
            i += 1;
            continue;
        }
        let aname = &attrs[ns..i];
        while i < attrs.len() && is_ws(attrs[i]) { i += 1; }
        if i >= attrs.len() {
            break;
        }
        if attrs[i] != b'=' {
            while i < attrs.len() && !is_ws(attrs[i]) && attrs[i] != b'>' && attrs[i] != b'/' {
                i += 1;
            }
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
        if aname == name { return Some(val); }
    }
    None
}

fn get_style_prop<'a>(style: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0usize;
    while i < style.len() {
        while i < style.len() && (is_ws(style[i]) || style[i] == b';') { i += 1; }
        if i >= style.len() { break; }

        let ks = i;
        while i < style.len() && style[i] != b':' && style[i] != b';' { i += 1; }
        if i >= style.len() || style[i] != b':' {
            while i < style.len() && style[i] != b';' { i += 1; }
            continue;
        }
        let key = trim_ascii(&style[ks..i]);
        i += 1;
        let vs = i;
        while i < style.len() && style[i] != b';' { i += 1; }
        let val = trim_ascii(&style[vs..i]);

        if key.eq_ignore_ascii_case(name) {
            return Some(val);
        }
    }
    None
}

fn is_display_none(attrs: &[u8]) -> bool {
    match get_attr(attrs, b"display") {
        Some(v) => trim_ascii(v).eq_ignore_ascii_case(b"none"),
        None => false,
    }
}

#[inline]
fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while !s.is_empty() && is_ws(s[0]) { s = &s[1..]; }
    while !s.is_empty() && is_ws(s[s.len() - 1]) { s = &s[..s.len() - 1]; }
    s
}

#[inline]
fn mul_alpha(a: u8, b: u8) -> u8 {
    (((a as u16) * (b as u16) + 127) / 255) as u8
}

fn parse_opacity_u8(v: &[u8]) -> u8 {
    let mut i = 0usize;
    parse_decimal_0_1_to_u8_from(v, &mut i)
}

fn parse_decimal_0_1_to_u8_from(v: &[u8], i: &mut usize) -> u8 {
    skip_sep(v, i);
    let mut int_part = 0i32;
    while *i < v.len() && v[*i].is_ascii_digit() {
        int_part = int_part.saturating_mul(10).saturating_add((v[*i] - b'0') as i32);
        *i += 1;
    }

    let mut frac_part = 0i32;
    let mut frac_scale = 1i32;
    if *i < v.len() && v[*i] == b'.' {
        *i += 1;
        while *i < v.len() && v[*i].is_ascii_digit() {
            frac_part = frac_part.saturating_mul(10).saturating_add((v[*i] - b'0') as i32);
            frac_scale = frac_scale.saturating_mul(10);
            *i += 1;
        }
    }

    let mut num = int_part.saturating_mul(frac_scale).saturating_add(frac_part);
    if num <= 0 { return 0; }
    if frac_scale <= 0 { return 255; }
    if num >= frac_scale { return 255; }

    num = num.saturating_mul(255);
    ((num + frac_scale / 2) / frac_scale) as u8
}

fn starts_with_ignore_ascii_case(hay: &[u8], needle: &[u8]) -> bool {
    if hay.len() < needle.len() { return false; }
    hay[..needle.len()].eq_ignore_ascii_case(needle)
}

fn attr_i(attrs: &[u8], name: &[u8]) -> i32 {
    get_attr(attrs, name).map(parse_i32_b).unwrap_or(0)
}

fn parse_i32_b(s: &[u8]) -> i32 {
    let mut i = 0;
    while i < s.len() && is_ws(s[i]) { i += 1; }
    let neg = if i < s.len() && s[i] == b'-' { i += 1; true } else { false };
    if i < s.len() && s[i] == b'+' { i += 1; }
    let mut v = 0i32;
    while i < s.len() && s[i] >= b'0' && s[i] <= b'9' {
        v = v.saturating_mul(10).saturating_add((s[i] - b'0') as i32);
        i += 1;
    }
    if i < s.len() && s[i] == b'.' {
        i += 1;
        if i < s.len() && s[i] >= b'5' && s[i] <= b'9' {
            v = v.saturating_add(1);
        }
    }
    if neg { -v } else { v }
}

// ─── Streaming number parser ─────────────────────────────────────────────────

/// Parse the next number from `d` starting at `i`, advancing `i` past it.
fn pn(d: &[u8], i: &mut usize) -> i32 {
    skip_sep(d, i);
    let neg = if *i < d.len() && d[*i] == b'-' { *i += 1; true }
              else if *i < d.len() && d[*i] == b'+' { *i += 1; false }
              else { false };

    let mut int_part: i64 = 0;
    while *i < d.len() && d[*i].is_ascii_digit() {
        int_part = int_part.saturating_mul(10).saturating_add((d[*i] - b'0') as i64);
        *i += 1;
    }

    let mut frac_part: i64 = 0;
    let mut frac_div: i64 = 1;
    if *i < d.len() && d[*i] == b'.' {
        *i += 1;
        while *i < d.len() && d[*i].is_ascii_digit() {
            frac_part = frac_part.saturating_mul(10).saturating_add((d[*i] - b'0') as i64);
            frac_div = frac_div.saturating_mul(10);
            *i += 1;
        }
    }

    let mut exp10: i32 = 0;
    if *i < d.len() && (d[*i] == b'e' || d[*i] == b'E') {
        *i += 1;
        let exp_neg = if *i < d.len() && d[*i] == b'-' { *i += 1; true }
                      else if *i < d.len() && d[*i] == b'+' { *i += 1; false }
                      else { false };
        while *i < d.len() && d[*i].is_ascii_digit() {
            exp10 = exp10.saturating_mul(10).saturating_add((d[*i] - b'0') as i32);
            *i += 1;
        }
        if exp_neg { exp10 = -exp10; }
    }

    let mut num = int_part.saturating_mul(frac_div).saturating_add(frac_part);
    let mut den = frac_div.max(1);

    if exp10 > 0 {
        for _ in 0..exp10.min(9) {
            num = num.saturating_mul(10);
        }
    } else if exp10 < 0 {
        for _ in 0..(-exp10).min(9) {
            den = den.saturating_mul(10);
        }
    }

    let mut v = ((num + den / 2) / den) as i32;
    if neg { v = -v; }
    v
}

/// Returns true if position i in d is the start of a number.
fn peek_n(d: &[u8], mut i: usize) -> bool {
    while i < d.len() && (is_ws(d[i]) || d[i] == b',') { i += 1; }
    if i >= d.len() { return false; }
    let b = d[i];
    b.is_ascii_digit() || b == b'-' || b == b'+' || b == b'.'
}

fn skip_sep(d: &[u8], i: &mut usize) {
    while *i < d.len() && (is_ws(d[*i]) || d[*i] == b',') { *i += 1; }
}

fn is_ws(b: u8) -> bool { b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' }

// ─── Transform / viewBox parsing ─────────────────────────────────────────────

fn parse_viewbox(svg: &[u8]) -> (i32, i32) {
    let mut i = 0;
    while i + 8 < svg.len() {
        if &svg[i..i + 8] == b"viewBox=" {
            i += 8;
            if i < svg.len() && (svg[i] == b'"' || svg[i] == b'\'') { i += 1; }
            let _x = pn(svg, &mut i);
            let _y = pn(svg, &mut i);
            let w = pn(svg, &mut i);
            let h = pn(svg, &mut i);
            if w > 0 && h > 0 { return (w, h); }
        }
        i += 1;
    }
    (0, 0)
}

fn parse_transform(attrs: &[u8], vbw: i32, vbh: i32, ow: i32, oh: i32) -> Tx {
    let t = match get_attr(attrs, b"transform") { Some(v) => v, None => return Tx::identity() };
    let mut i = 0usize;
    let mut cur = Tx::identity();

    while i < t.len() {
        while i < t.len() && (is_ws(t[i]) || t[i] == b',') { i += 1; }
        if i >= t.len() { break; }

        if i + 10 <= t.len() && t[i..i+10].eq_ignore_ascii_case(b"translate(") {
            i += 10;
            let tx_svg = pn(t, &mut i);
            let ty_svg = if peek_n(t, i) { pn(t, &mut i) } else { 0 };
            let local = Tx {
                sx: FP_ONE,
                sy: FP_ONE,
                tx: sc(tx_svg, vbw, ow),
                ty: sc(ty_svg, vbh, oh),
            };
            cur = compose_tx(cur, local);
            continue;
        }

        if i + 6 <= t.len() && t[i..i+6].eq_ignore_ascii_case(b"scale(") {
            i += 6;
            let sx = parse_fp_num(t, &mut i);
            let sy = if peek_n(t, i) { parse_fp_num(t, &mut i) } else { sx };
            let local = Tx { sx, sy, tx: 0, ty: 0 };
            cur = compose_tx(cur, local);
            continue;
        }

        if i + 7 <= t.len() && t[i..i+7].eq_ignore_ascii_case(b"matrix(") {
            i += 7;
            let a = parse_fp_num(t, &mut i);
            let _b = if peek_n(t, i) { parse_fp_num(t, &mut i) } else { 0 };
            let _c = if peek_n(t, i) { parse_fp_num(t, &mut i) } else { 0 };
            let d = if peek_n(t, i) { parse_fp_num(t, &mut i) } else { FP_ONE };
            let e = if peek_n(t, i) { pn(t, &mut i) } else { 0 };
            let f = if peek_n(t, i) { pn(t, &mut i) } else { 0 };
            let local = Tx {
                sx: a,
                sy: d,
                tx: sc(e, vbw, ow),
                ty: sc(f, vbh, oh),
            };
            cur = compose_tx(cur, local);
            continue;
        }

        i += 1;
    }

    cur
}

fn parse_fp_num(d: &[u8], i: &mut usize) -> i32 {
    skip_sep(d, i);
    let neg = if *i < d.len() && d[*i] == b'-' { *i += 1; true }
              else if *i < d.len() && d[*i] == b'+' { *i += 1; false }
              else { false };

    let mut int_part = 0i64;
    while *i < d.len() && d[*i].is_ascii_digit() {
        int_part = int_part.saturating_mul(10).saturating_add((d[*i] - b'0') as i64);
        *i += 1;
    }

    let mut frac_part = 0i64;
    let mut frac_div = 1i64;
    if *i < d.len() && d[*i] == b'.' {
        *i += 1;
        while *i < d.len() && d[*i].is_ascii_digit() {
            frac_part = frac_part.saturating_mul(10).saturating_add((d[*i] - b'0') as i64);
            frac_div = frac_div.saturating_mul(10);
            *i += 1;
        }
    }

    let mut v = int_part.saturating_mul(FP_ONE as i64);
    v = v.saturating_add((frac_part.saturating_mul(FP_ONE as i64)) / frac_div.max(1));
    let v = v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
    if neg { -v } else { v }
}

fn parse_points(
    raw: &[u8], vbw: i32, vbh: i32, ow: i32, oh: i32, ctx: Tx,
) -> Vec<(i32, i32)> {
    let mut pts = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        if !peek_n(raw, i) { break; }
        let x = pn(raw, &mut i);
        let y = pn(raw, &mut i);
        pts.push(map_xy(ctx, x, y, vbw, vbh, ow, oh));
    }
    pts
}

// ─── Coordinate scaling ──────────────────────────────────────────────────────

#[inline]
fn sc(v: i32, from: i32, to: i32) -> i32 {
    if from == 0 { return v; }
    (v as i64 * to as i64 / from as i64) as i32
}

// ─── Pixel drawing primitives ────────────────────────────────────────────────

#[inline]
fn plot(px: &mut Vec<u32>, w: u32, h: u32, x: i32, y: i32, color: u32) {
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 { return; }
    let idx = (y as u32 * w + x as u32) as usize;
    if idx < px.len() { px[idx] = color; }
}

fn fill_rect_px(
    px: &mut Vec<u32>, w: u32, h: u32,
    x0: i32, y0: i32, x1: i32, y1: i32, color: u32,
) {
    let x0 = x0.max(0) as u32;
    let y0 = y0.max(0) as u32;
    let x1 = x1.min(w as i32 - 1);
    let y1 = y1.min(h as i32 - 1);
    if x1 < 0 || y1 < 0 { return; }
    let x1 = x1 as u32; let y1 = y1 as u32;
    if x1 < x0 || y1 < y0 { return; }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let idx = (y * w + x) as usize;
            if idx < px.len() { px[idx] = alpha_blend_over(px[idx], color); }
        }
    }
}

fn rect_outline(
    px: &mut Vec<u32>, w: u32, h: u32,
    x0: i32, y0: i32, x1: i32, y1: i32, sw: i32, color: u32,
) {
    let t = sw.max(1);
    fill_rect_px(px, w, h, x0, y0, x1, y0 + t - 1, color);         // top
    fill_rect_px(px, w, h, x0, y1 - t + 1, x1, y1, color);         // bottom
    fill_rect_px(px, w, h, x0, y0, x0 + t - 1, y1, color);         // left
    fill_rect_px(px, w, h, x1 - t + 1, y0, x1, y1, color);         // right
}

fn fill_circle_px(
    px: &mut Vec<u32>, w: u32, h: u32, cx: i32, cy: i32, r: i32, color: u32,
) {
    if r <= 0 { return; }
    let r2 = r * r;
    for dy in -r..=r {
        let y = cy + dy;
        if y < 0 || y >= h as i32 { continue; }
        let dx = isqrt32(r2 - dy * dy);
        let x0 = (cx - dx).max(0);
        let x1 = (cx + dx).min(w as i32 - 1);
        for x in x0..=x1 {
            let idx = (y as u32 * w + x as u32) as usize;
            if idx < px.len() { px[idx] = alpha_blend_over(px[idx], color); }
        }
    }
}

fn circle_outline(
    px: &mut Vec<u32>, w: u32, h: u32, cx: i32, cy: i32, r: i32, sw: i32, color: u32,
) {
    if r <= 0 { return; }
    let ro = r; let ri = (r - sw).max(0);
    let r2o = ro * ro; let r2i = ri * ri;
    for dy in -ro..=ro {
        let y = cy + dy; let dy2 = dy * dy;
        if y < 0 || y >= h as i32 { continue; }
        let dxo = isqrt32(r2o - dy2.min(r2o));
        let dxi = if dy2 <= r2i { isqrt32(r2i - dy2) } else { 0 };
        for dx in -dxo..=-dxi { plot(px, w, h, cx + dx, y, color); }
        for dx in  dxi..= dxo { plot(px, w, h, cx + dx, y, color); }
    }
}

fn thick_line(
    px: &mut Vec<u32>, w: u32, h: u32,
    x0: i32, y0: i32, x1: i32, y1: i32, color: u32, t: i32,
) {
    let t2 = t / 2;
    let dx = (x1 - x0).abs(); let dy = (y1 - y0).abs();
    let sx = if x0 < x1 { 1i32 } else { -1 };
    let sy = if y0 < y1 { 1i32 } else { -1 };
    let mut err = dx - dy;
    let mut cx = x0; let mut cy = y0;
    loop {
        for tx in -t2..=t2 {
            for ty in -t2..=t2 {
                plot(px, w, h, cx + tx, cy + ty, color);
            }
        }
        if cx == x1 && cy == y1 { break; }
        let e2 = 2 * err;
        if e2 > -dy { err -= dy; cx += sx; }
        if e2 <  dx { err += dx; cy += sy; }
    }
}

fn stroke_lines(
    px: &mut Vec<u32>, w: u32, h: u32,
    pts: &[(i32, i32)], closed: bool, color: u32, sw: i32,
) {
    if pts.len() < 2 { return; }
    for i in 0..pts.len() - 1 {
        thick_line(px, w, h, pts[i].0, pts[i].1, pts[i+1].0, pts[i+1].1, color, sw);
    }
    if closed {
        let n = pts.len();
        thick_line(px, w, h, pts[n-1].0, pts[n-1].1, pts[0].0, pts[0].1, color, sw);
    }
}

/// Scanline polygon fill (even-odd rule).
fn fill_poly_px(px: &mut Vec<u32>, w: u32, h: u32, pts: &[(i32, i32)], color: u32) {
    if pts.len() < 3 { return; }
    let min_y = pts.iter().map(|p| p.1).min().unwrap_or(0).max(0);
    let max_y = pts.iter().map(|p| p.1).max().unwrap_or(0).min(h as i32 - 1);
    let n = pts.len();
    let mut xs = [0i32; 64];
    for y in min_y..=max_y {
        let mut cnt = 0usize;
        for k in 0..n {
            let (x0, y0) = pts[k];
            let (x1, y1) = pts[(k + 1) % n];
            if (y0 <= y && y1 > y) || (y1 <= y && y0 > y) {
                let xi = x0 + (y - y0) * (x1 - x0) / (y1 - y0);
                if cnt < 64 { xs[cnt] = xi; cnt += 1; }
            }
        }
        // Insertion sort
        for k in 1..cnt {
            let key = xs[k]; let mut j = k;
            while j > 0 && xs[j-1] > key { xs[j] = xs[j-1]; j -= 1; }
            xs[j] = key;
        }
        let mut j = 0;
        while j + 1 < cnt {
            let xa = xs[j].max(0);
            let xb = xs[j+1].min(w as i32 - 1);
            for x in xa..=xb {
                let idx = (y as u32 * w + x as u32) as usize;
                if idx < px.len() { px[idx] = alpha_blend_over(px[idx], color); }
            }
            j += 2;
        }
    }
}

#[inline]
fn alpha_blend_over(dst: u32, src: u32) -> u32 {
    let sa = ((src >> 24) & 0xFF) as u32;
    if sa == 0 { return dst; }
    if sa == 255 { return src; }

    let da = ((dst >> 24) & 0xFF) as u32;
    let sr = (src >> 16) & 0xFF;
    let sg = (src >> 8) & 0xFF;
    let sb = src & 0xFF;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;

    let inv_sa = 255 - sa;
    let out_a = sa + ((da * inv_sa + 127) / 255);
    let out_r = (sr * sa + dr * inv_sa + 127) / 255;
    let out_g = (sg * sa + dg * inv_sa + 127) / 255;
    let out_b = (sb * sa + db * inv_sa + 127) / 255;

    (out_a << 24) | (out_r << 16) | (out_g << 8) | out_b
}

fn isqrt32(n: i32) -> i32 {
    if n <= 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x { x = y; y = (x + n / x) / 2; }
    x
}
