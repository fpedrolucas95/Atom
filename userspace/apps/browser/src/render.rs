//! Painting: layout constants, the colour palette, and the per-block drawing
//! routines. The renderer is intentionally a thin, allocation-light painter —
//! all styling decisions were resolved during parsing, so drawing a frame is a
//! straight walk over already-laid-out blocks (focus: low CPU per redraw).

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;
use libgui::surface::Surface;

use crate::dom::{Hit, InputKind, InputMeta, Run, TextKind};
use crate::text::truncate_for_width;

// ── Glyph / layout metrics ──────────────────────────────────────────────────
pub const CHAR_W: u32 = 8;
pub const CHAR_H: u32 = 8;
pub const LINE_H: u32 = 12;
pub const TOOLBAR_H: u32 = 42;
pub const STATUS_H: u32 = 20;
pub const PADDING: u32 = 12;
pub const ADDR_Y: u32 = 8;
pub const ADDR_H: u32 = 24;

// ── Palette ─────────────────────────────────────────────────────────────────
pub const BG: Color = Color::rgb(248, 249, 252);
pub const CHROME: Color = Color::rgb(34, 40, 49);
pub const CHROME_LINE: Color = Color::rgb(74, 86, 104);
pub const TEXT: Color = Color::rgb(28, 34, 43);
pub const MUTED: Color = Color::rgb(94, 105, 120);
pub const ACCENT: Color = Color::rgb(38, 132, 255);
pub const FIELD_BG: Color = Color::rgb(255, 255, 255);

const HEADING1: Color = Color::rgb(17, 24, 39);
const HEADING3: Color = Color::rgb(83, 96, 116);
const CODE: Color = Color::rgb(176, 48, 96);
const RULE: Color = Color::rgb(210, 218, 230);
const PANEL: Color = Color::rgb(230, 236, 245);
const PANEL_BORDER: Color = Color::rgb(203, 213, 225);
const PLACEHOLDER: Color = Color::rgb(150, 160, 175);
const FIELD_BORDER: Color = Color::rgb(180, 190, 205);

/// Geometry of the address bar: (input_x, input_w, go_x, go_w).
pub fn addr_bar_geom(width: u32) -> (u32, u32, u32, u32) {
    let input_x = 62u32;
    let input_w = width.saturating_sub(input_x + 72);
    let go_w = 48u32;
    let go_x = width.saturating_sub(58);
    (input_x, input_w, go_x, go_w)
}

/// A vertical band of the viewport that is currently visible.
#[derive(Clone, Copy)]
pub struct Clip {
    pub top: i32,
    pub bottom: i32,
}

impl Clip {
    fn visible(&self, y: i32, height: i32) -> bool {
        y + height >= self.top && y <= self.bottom && y + height >= 0
    }
}

/// Resolved colour and decoration for a single run.
struct Painted {
    color: Color,
    bold: bool,
    underline: bool,
}

fn paint_for(run: &Run, default: Color) -> Painted {
    let color = run.style.color.unwrap_or(if run.link.is_some() {
        ACCENT
    } else if run.style.mono {
        CODE
    } else {
        default
    });
    Painted {
        color,
        bold: run.style.bold,
        underline: run.style.underline || run.link.is_some(),
    }
}

/// Default block text colour and vertical padding (top, bottom) by kind.
fn block_metrics(kind: TextKind) -> (Color, i32, i32) {
    match kind {
        TextKind::H1 => (HEADING1, 12, 8),
        TextKind::H2 => (ACCENT, 10, 6),
        TextKind::H3 => (HEADING3, 8, 4),
        TextKind::ListItem => (TEXT, 4, 4),
        TextKind::Quote => (MUTED, 8, 8),
        TextKind::Pre => (TEXT, 8, 8),
        TextKind::Paragraph => (TEXT, 6, 6),
    }
}

/// Draw one wrapped, styled word and register a hit region if it is a link.
fn draw_word(s: &mut Surface, x: u32, y: u32, word: &str, p: &Painted, bg: Color) {
    s.draw_string(x, y, word, p.color, bg);
    if p.bold {
        // Bitmap font has no bold face; overstrike one pixel to fake weight.
        s.draw_string(x + 1, y, word, p.color, bg);
    }
    if p.underline {
        let wpx = word.chars().count() as u32 * CHAR_W;
        s.draw_hline(x, y + CHAR_H, wpx, p.color);
    }
}

/// Draw a flow text block (heading, paragraph, list item, or quote) with word
/// wrapping. Returns the y position just below the block.
#[allow(clippy::too_many_arguments)]
pub fn draw_text_block(
    s: &mut Surface,
    kind: TextKind,
    runs: &[Run],
    marker: Option<&str>,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
    link_hits: &mut Vec<Hit>,
) -> i32 {
    if kind == TextKind::Pre {
        return draw_pre(s, runs, x0, max_w, y, clip);
    }

    let (default_fg, top_pad, bottom_pad) = block_metrics(kind);
    y += top_pad;
    let block_top = y;

    let (indent, eff_w) = match kind {
        TextKind::ListItem => {
            let m = marker.unwrap_or("*");
            if clip.visible(y, CHAR_H as i32) {
                s.draw_string(x0 + 8, y as u32, m, ACCENT, BG);
            }
            let indent = ((m.chars().count() as u32 + 1) * CHAR_W + 8).max(24);
            (indent, max_w.saturating_sub(indent))
        }
        TextKind::Quote => (16u32, max_w.saturating_sub(16)),
        _ => (0u32, max_w),
    };

    let cols = (eff_w / CHAR_W).max(1);
    let line_x = x0 + indent;
    let mut cx: u32 = 0;

    for run in runs {
        let p = paint_for(run, default_fg);
        for word in run.text.split(' ') {
            if word.is_empty() {
                continue;
            }
            let wlen = word.chars().count() as u32;
            if cx != 0 && cx + 1 + wlen > cols {
                y += LINE_H as i32;
                cx = 0;
            }
            if cx != 0 {
                cx += 1;
            }
            let px = line_x + cx * CHAR_W;
            let wpx = wlen * CHAR_W;
            if clip.visible(y - CHAR_H as i32, (CHAR_H * 2) as i32) {
                draw_word(s, px, y as u32, word, &p, BG);
            }
            if let Some(idx) = run.link {
                link_hits.push(Hit {
                    x: px as i32,
                    y: y - 1,
                    w: wpx as i32,
                    h: (CHAR_H + 3) as i32,
                    idx,
                });
            }
            cx += wlen;
        }
    }

    if kind == TextKind::Quote {
        let bar_h = (y - block_top + LINE_H as i32).max(LINE_H as i32);
        if clip.visible(block_top, bar_h) && block_top >= 0 {
            s.fill_rect(x0 + 2, block_top.max(0) as u32, 3, bar_h as u32, ACCENT);
        }
    }

    y + LINE_H as i32 + bottom_pad
}

fn draw_pre(s: &mut Surface, runs: &[Run], x0: u32, max_w: u32, mut y: i32, clip: Clip) -> i32 {
    let fg = Color::rgb(20, 28, 40);
    y += 8;
    let mut text = String::new();
    for run in runs {
        text.push_str(&run.text);
    }
    let line_count = text.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let box_h = line_count * LINE_H + 12;
    if clip.visible(y, box_h as i32) {
        s.fill_rect(x0, y as u32, max_w, box_h, PANEL);
        s.draw_rect(x0, y as u32, max_w, box_h, PANEL_BORDER);
    }
    let max_cols = (max_w.saturating_sub(16) / CHAR_W).max(1) as usize;
    let mut ly = y + 6;
    for raw in text.split('\n') {
        let visible: String = raw.chars().take(max_cols).collect();
        if clip.visible(ly, CHAR_H as i32) {
            s.draw_string(x0 + 8, ly as u32, &visible, fg, PANEL);
        }
        ly += LINE_H as i32;
    }
    y + box_h as i32 + 8
}

/// Draw a horizontal rule, returning the y below it.
pub fn draw_rule(s: &mut Surface, x0: u32, content_w: u32, y: i32, clip: Clip) -> i32 {
    if clip.visible(y, 16) {
        s.draw_hline(x0, (y + 8).max(0) as u32, content_w, RULE);
    }
    y + 24
}

/// Draw an image (scaled to fit) or a placeholder if undecoded.
pub fn draw_image_block(
    s: &mut Surface,
    alt: &str,
    img: &Option<libimage::DecodedImage>,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
) -> i32 {
    y += 8;
    match img {
        Some(im) if im.width > 0 && im.height > 0 => {
            let (tw, th) = fit_dimensions(im.width, im.height, max_w, 320);
            if clip.visible(y, th as i32) {
                blit_scaled(s, im, x0, y, tw, th, clip);
            }
            y + th as i32 + 10
        }
        _ => {
            let (w, h) = (max_w.min(260), 64u32);
            if clip.visible(y, h as i32) {
                s.fill_rect(x0, y as u32, w, h, Color::rgb(238, 240, 244));
                s.draw_rect(x0, y as u32, w, h, PANEL_BORDER);
                let label = if alt.is_empty() {
                    String::from("[image]")
                } else {
                    let mut l = String::from("[image] ");
                    l.push_str(alt);
                    l
                };
                s.draw_string(
                    x0 + 8,
                    y as u32 + h / 2 - 4,
                    &truncate_for_width(&label, w.saturating_sub(16)),
                    MUTED,
                    Color::rgb(238, 240, 244),
                );
            }
            y + h as i32 + 10
        }
    }
}

/// Scale `(w, h)` to fit within `(max_w, max_h)` preserving aspect ratio.
fn fit_dimensions(mut w: u32, mut h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w > max_w {
        h = (h * max_w / w).max(1);
        w = max_w;
    }
    if h > max_h {
        w = (w * max_h / h).max(1);
        h = max_h;
    }
    (w, h)
}

fn blit_scaled(
    s: &mut Surface,
    im: &libimage::DecodedImage,
    x0: u32,
    y: i32,
    tw: u32,
    th: u32,
    clip: Clip,
) {
    for ry in 0..th {
        let dy = y + ry as i32;
        if dy < clip.top || dy > clip.bottom || dy < 0 {
            continue;
        }
        let sy = ry * im.height / th;
        for rx in 0..tw {
            let sx = rx * im.width / tw;
            if let Some((r, g, b, a)) = im.get_pixel(sx, sy) {
                if a >= 16 {
                    s.set_pixel(x0 + rx, dy as u32, Color::rgb(r, g, b));
                }
            }
        }
    }
}

/// Draw a form control and register its hit region.
#[allow(clippy::too_many_arguments)]
pub fn draw_input_block(
    s: &mut Surface,
    meta: &InputMeta,
    value: &str,
    focused: bool,
    idx: usize,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
    input_hits: &mut Vec<Hit>,
) -> i32 {
    y += 6;
    let h = 26u32;
    let w = match meta.kind {
        InputKind::Submit => 120u32.min(max_w),
        _ => max_w.min(440),
    };
    if clip.visible(y, h as i32) {
        match meta.kind {
            InputKind::Submit => {
                s.fill_rect(x0, y as u32, w, h, ACCENT);
                let label = if meta.placeholder.is_empty() {
                    "Search"
                } else {
                    &meta.placeholder
                };
                s.draw_string(x0 + 10, y as u32 + 9, label, Color::WHITE, ACCENT);
            }
            _ => {
                s.fill_rect(x0, y as u32, w, h, FIELD_BG);
                s.draw_rect(
                    x0,
                    y as u32,
                    w,
                    h,
                    if focused { ACCENT } else { FIELD_BORDER },
                );
                if value.is_empty() && !focused {
                    s.draw_string(
                        x0 + 8,
                        y as u32 + 9,
                        &truncate_for_width(&meta.placeholder, w.saturating_sub(16)),
                        PLACEHOLDER,
                        FIELD_BG,
                    );
                } else {
                    let shown = if focused {
                        let mut v = String::from(value);
                        v.push('_');
                        v
                    } else {
                        String::from(value)
                    };
                    s.draw_string(
                        x0 + 8,
                        y as u32 + 9,
                        &truncate_for_width(&shown, w.saturating_sub(16)),
                        TEXT,
                        FIELD_BG,
                    );
                }
            }
        }
    }
    input_hits.push(Hit {
        x: x0 as i32,
        y,
        w: w as i32,
        h: h as i32,
        idx,
    });
    y + h as i32 + 8
}
