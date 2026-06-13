//! Painting: layout constants, the colour palette, and the per-block drawing
//! routines. The renderer is intentionally a thin, allocation-light painter —
//! all styling decisions were resolved during parsing, so drawing a frame is a
//! straight walk over already-laid-out blocks (focus: low CPU per redraw).

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;
use libgui::surface::Surface;

use crate::dom::{
    Align, AlignItems, Block, BoxStyle, FlexChild, FlexDirection, Hit, Inline, InputKind,
    InputMeta, JustifyContent, Run, TextKind,
};

/// Truncate `text` with an ellipsis so it fits within `width` pixels.
pub fn truncate_for_width(text: &str, width: u32) -> String {
    let max_chars = (width / CHAR_W) as usize;
    if text.chars().count() <= max_chars {
        return String::from(text);
    }
    if max_chars <= 3 {
        return String::from("...");
    }
    let mut out: String = text.chars().take(max_chars - 3).collect();
    out.push_str("...");
    out
}

// ── Glyph / layout metrics ──────────────────────────────────────────────────
pub const CHAR_W: u32 = 8;
pub const CHAR_H: u32 = 8;
pub const LINE_H: u32 = 12;
pub const TOOLBAR_H: u32 = 58;
pub const STATUS_H: u32 = 20;
pub const PADDING: u32 = 12;
pub const ADDR_Y: u32 = 16;
pub const ADDR_H: u32 = 26;
pub const NAV_X: u32 = 12;
pub const NAV_BTN_W: u32 = 28;
pub const NAV_GAP: u32 = 6;

// ── Palette ─────────────────────────────────────────────────────────────────
pub const CHROME: Color = Color::rgb(30, 41, 59);
pub const CHROME_LINE: Color = Color::rgb(56, 68, 88);
pub const TEXT: Color = Color::rgb(226, 232, 240);
pub const MUTED: Color = Color::rgb(148, 163, 184);
pub const ACCENT: Color = Color::rgb(74, 138, 255);
pub const FIELD_BG: Color = Color::rgb(7, 11, 18);
pub const SCROLLBAR_W: u32 = 8;

const HEADING1: Color = Color::rgb(240, 245, 252);
const HEADING3: Color = Color::rgb(180, 190, 205);
const CODE: Color = Color::rgb(255, 177, 107);
const RULE: Color = Color::rgb(42, 52, 68);
const PANEL: Color = Color::rgb(15, 23, 36);
const PANEL_BORDER: Color = Color::rgb(48, 60, 80);
const PLACEHOLDER: Color = Color::rgb(116, 130, 150);
const FIELD_BORDER: Color = Color::rgb(56, 68, 88);

/// Geometry of the address bar: (input_x, input_w, go_x, go_w).
pub fn addr_bar_geom(width: u32) -> (u32, u32, u32, u32) {
    let input_x = NAV_X + (NAV_BTN_W + NAV_GAP) * 4 + 10;
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

    /// A clip that contains nothing: block-drawing routines still advance `y`
    /// correctly but paint no pixels, giving a paint-free measurement pass.
    pub fn hidden() -> Self {
        Self {
            top: i32::MAX,
            bottom: i32::MIN,
        }
    }
}

/// Height of an inline form control.
const CTRL_H: u32 = 20;

/// References to per-control state the renderer needs to paint form fields.
pub struct FormCtx<'a> {
    pub inputs: &'a [InputMeta],
    pub values: &'a [String],
    pub focused: Option<usize>,
}

/// Resolved colour and decoration for a single run.
#[derive(Clone, Copy)]
struct Painted {
    color: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    /// Integer glyph scale (1 = native 8px).
    scale: u32,
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
        italic: run.style.italic,
        underline: run.style.underline || run.link.is_some(),
        strike: run.style.strike,
        scale: (run.style.size.max(1)) as u32,
    }
}

/// Default block text colour and vertical padding (top, bottom) by kind.
fn block_metrics(kind: TextKind, page_bg: Color) -> (Color, i32, i32) {
    let light_page =
        page_bg.r as u32 * 299 + page_bg.g as u32 * 587 + page_bg.b as u32 * 114 >= 160_000;
    let text = if light_page {
        Color::rgb(32, 37, 45)
    } else {
        TEXT
    };
    let heading = if light_page {
        Color::rgb(20, 24, 31)
    } else {
        HEADING1
    };
    let muted = if light_page {
        Color::rgb(91, 99, 112)
    } else {
        MUTED
    };
    match kind {
        TextKind::H1 => (heading, 12, 8),
        TextKind::H2 => (ACCENT, 10, 6),
        TextKind::H3 => (if light_page { muted } else { HEADING3 }, 8, 4),
        TextKind::ListItem => (text, 4, 4),
        TextKind::Quote => (muted, 8, 8),
        TextKind::Pre => (text, 8, 8),
        TextKind::Paragraph => (text, 6, 6),
    }
}

/// Draw one wrapped, styled word and register a hit region if it is a link.
/// Faux bold/italic and integer scaling are synthesised by the surface; only
/// the underline/strike rules are drawn here, sized to the glyph scale.
fn draw_word(s: &mut Surface, x: u32, y: u32, word: &str, p: &Painted, bg: Color) {
    s.draw_text_styled(x, y, word, p.color, bg, p.scale, p.italic, p.bold);
    let cw = CHAR_W * p.scale;
    let ch = CHAR_H * p.scale;
    let wpx = word.chars().count() as u32 * cw;
    if p.underline {
        s.draw_hline(x, y + ch, wpx, p.color);
    }
    if p.strike {
        s.draw_hline(x, y + ch / 2, wpx, p.color);
    }
}

/// One laid-out inline atom on a line: a word or a form control.
#[derive(Clone, Copy)]
enum Tok<'a> {
    Word {
        text: &'a str,
        paint: Painted,
        link: Option<usize>,
        zone: Option<usize>,
    },
    Ctrl {
        idx: usize,
        w: u32,
        h: u32,
    },
}

impl Tok<'_> {
    fn width(&self) -> u32 {
        match self {
            Tok::Word { text, paint, .. } => text.chars().count() as u32 * CHAR_W * paint.scale,
            Tok::Ctrl { w, .. } => *w,
        }
    }

    fn height(&self) -> u32 {
        match self {
            Tok::Word { paint, .. } => CHAR_H * paint.scale,
            Tok::Ctrl { h, .. } => *h,
        }
    }
}

/// Intrinsic size of a form control. Text fields size from the `size`
/// attribute; buttons and selects size to their label.
fn control_dims(meta: &InputMeta, max_w: u32) -> (u32, u32) {
    let label_chars = meta.placeholder.chars().count() as u32;
    let (chars, pad) = match meta.kind {
        InputKind::Submit => (label_chars.max(2), 16),
        InputKind::Select => (label_chars.max(3) + 2, 16), // room for the arrow
        _ => (meta.size.unwrap_or(20).clamp(4, 40), 14),
    };
    let w = (chars * CHAR_W + pad).min(max_w.max(CHAR_W));
    (w, CTRL_H)
}

/// Draw a form control at the given position.
#[allow(clippy::too_many_arguments)]
fn draw_control(
    s: &mut Surface,
    meta: &InputMeta,
    value: &str,
    focused: bool,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) {
    match meta.kind {
        InputKind::Submit => {
            s.fill_rect(x, y, w, h, ACCENT);
            let label = if meta.placeholder.is_empty() {
                "Submit"
            } else {
                &meta.placeholder
            };
            s.draw_string(
                x + 8,
                y + 6,
                &truncate_for_width(label, w.saturating_sub(12)),
                Color::WHITE,
                ACCENT,
            );
        }
        InputKind::Select => {
            let label = if value.is_empty() {
                meta.placeholder.as_str()
            } else {
                value
            };
            s.fill_rect(x, y, w, h, FIELD_BG);
            s.draw_rect(x, y, w, h, FIELD_BORDER);
            s.draw_string(
                x + 6,
                y + 6,
                &truncate_for_width(label, w.saturating_sub(22)),
                TEXT,
                FIELD_BG,
            );
            s.draw_string(x + w.saturating_sub(12), y + 6, "v", MUTED, FIELD_BG);
        }
        _ => {
            s.fill_rect(x, y, w, h, FIELD_BG);
            s.draw_rect(x, y, w, h, if focused { ACCENT } else { FIELD_BORDER });
            if value.is_empty() && !focused {
                s.draw_string(
                    x + 6,
                    y + 6,
                    &truncate_for_width(&meta.placeholder, w.saturating_sub(12)),
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
                    x + 6,
                    y + 6,
                    &truncate_for_width(&shown, w.saturating_sub(12)),
                    TEXT,
                    FIELD_BG,
                );
            }
        }
    }
}

/// Draw a flow block (heading, paragraph, list item, or quote) with inline word
/// + control wrapping and optional centering. Returns the y just below it.
#[allow(clippy::too_many_arguments)]
pub fn draw_text_block(
    s: &mut Surface,
    kind: TextKind,
    items: &[Inline],
    align: Align,
    marker: Option<&str>,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
    page_bg: Color,
    link_hits: &mut Vec<Hit>,
    form: &FormCtx,
    input_hits: &mut Vec<Hit>,
    zone_hits: &mut Vec<Hit>,
) -> i32 {
    if kind == TextKind::Pre {
        return draw_pre(s, items, x0, max_w, y, clip);
    }

    let (default_fg, top_pad, bottom_pad) = block_metrics(kind, page_bg);
    y += top_pad;
    let block_top = y;

    let (indent, eff_w) = match kind {
        TextKind::ListItem => {
            let cols = marker.map(|m| m.chars().count() as u32 + 1).unwrap_or(2);
            let indent = (cols * CHAR_W + 8).max(24);
            (indent, max_w.saturating_sub(indent))
        }
        TextKind::Quote => (16u32, max_w.saturating_sub(16)),
        _ => (0u32, max_w),
    };
    let line_x = x0 + indent;

    // Flatten items into a token stream the line-wrapper can measure.
    let mut toks: Vec<Tok> = Vec::new();
    for item in items {
        match item {
            Inline::Run(run) => {
                let paint = paint_for(run, default_fg);
                for word in run.text.split(' ') {
                    if !word.is_empty() {
                        toks.push(Tok::Word {
                            text: word,
                            paint,
                            link: run.link,
                            zone: run.zone,
                        });
                    }
                }
            }
            Inline::Control(idx) => {
                if let Some(meta) = form.inputs.get(*idx) {
                    let (w, h) = control_dims(meta, eff_w);
                    toks.push(Tok::Ctrl { idx: *idx, w, h });
                }
            }
        }
    }

    let space = CHAR_W;
    let mut first_line = true;
    let mut i = 0;
    while i < toks.len() {
        // Greedily measure how many tokens fit on this line.
        let mut line_w = toks[i].width();
        let mut line_h = LINE_H.max(toks[i].height() + ctrl_gap(&toks[i]));
        let mut j = i + 1;
        while j < toks.len() {
            let add = space + toks[j].width();
            if line_w + add > eff_w {
                break;
            }
            line_w += add;
            line_h = line_h.max(toks[j].height() + ctrl_gap(&toks[j]));
            j += 1;
        }

        if first_line {
            if let Some(m) = marker {
                if clip.visible(y, CHAR_H as i32) {
                    s.draw_string(x0 + 8, y as u32, m, ACCENT, page_bg);
                }
            }
            first_line = false;
        }

        let mut x = match align {
            Align::Center => line_x + eff_w.saturating_sub(line_w) / 2,
            Align::Right => line_x + eff_w.saturating_sub(line_w),
            Align::Left => line_x,
        };
        let line_visible = clip.visible(y, line_h as i32);
        for tok in &toks[i..j] {
            let ty = y + (line_h.saturating_sub(tok.height()) / 2) as i32;
            match *tok {
                Tok::Word {
                    text,
                    paint,
                    link,
                    zone,
                } => {
                    if line_visible && ty >= 0 {
                        draw_word(s, x, ty as u32, text, &paint, page_bg);
                    }
                    if let Some(idx) = link {
                        link_hits.push(Hit {
                            x: x as i32,
                            y: ty - 1,
                            w: tok.width() as i32,
                            h: tok.height() as i32 + 3,
                            idx,
                        });
                    }
                    if let Some(idx) = zone {
                        zone_hits.push(Hit {
                            x: x as i32,
                            y: ty - 1,
                            w: tok.width() as i32,
                            h: tok.height() as i32 + 3,
                            idx,
                        });
                    }
                }
                Tok::Ctrl { idx, w, h } => {
                    let meta = &form.inputs[idx];
                    let value = form.values.get(idx).map(String::as_str).unwrap_or("");
                    let focused = form.focused == Some(idx);
                    if line_visible && ty >= 0 {
                        draw_control(s, meta, value, focused, x, ty as u32, w, h);
                    }
                    input_hits.push(Hit {
                        x: x as i32,
                        y: ty,
                        w: w as i32,
                        h: h as i32,
                        idx,
                    });
                }
            }
            x += tok.width() + space;
        }

        y += line_h as i32;
        i = j;
    }

    if toks.is_empty() {
        y += LINE_H as i32;
    }

    if kind == TextKind::Quote {
        let bar_h = (y - block_top).max(LINE_H as i32);
        if clip.visible(block_top, bar_h) && block_top >= 0 {
            s.fill_rect(x0 + 2, block_top as u32, 3, bar_h as u32, ACCENT);
        }
    }

    y + bottom_pad
}

/// Lay out and paint a sequence of blocks within the column `[x0, x0+max_w)`,
/// starting at `y` and returning the y just below the last block. This is the
/// single flow walker shared by the page root and every flex item.
#[allow(clippy::too_many_arguments)]
pub fn draw_blocks(
    s: &mut Surface,
    blocks: &[Block],
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
    page_bg: Color,
    link_hits: &mut Vec<Hit>,
    form: &FormCtx,
    input_hits: &mut Vec<Hit>,
    zone_hits: &mut Vec<Hit>,
) -> i32 {
    for block in blocks {
        y = match block {
            Block::Text {
                kind,
                items,
                align,
                marker,
            } => draw_text_block(
                s,
                *kind,
                items,
                *align,
                marker.as_deref(),
                x0,
                max_w,
                y,
                clip,
                page_bg,
                link_hits,
                form,
                input_hits,
                zone_hits,
            ),
            Block::Rule => draw_rule(s, x0, max_w, y, clip),
            Block::Image {
                alt,
                img,
                error,
                align,
                ..
            } => draw_image_block(s, alt, img, error.as_deref(), *align, x0, max_w, y, clip),
            Block::Flex {
                direction,
                justify,
                align,
                gap,
                wrap,
                children,
            } => draw_flex_block(
                s, *direction, *justify, *align, *gap, *wrap, children, x0, max_w, y, clip,
                page_bg, link_hits, form, input_hits, zone_hits,
            ),
            Block::Box { style, children } => draw_box(
                s, style, children, x0, max_w, y, clip, page_bg, link_hits, form, input_hits,
                zone_hits,
            ),
        };
    }
    y
}

/// Fill the part of `[y, y+h)` that lies within the content `clip` band, so a
/// box background never bleeds over the toolbar or status bar when scrolled.
fn fill_clipped(s: &mut Surface, x: u32, y: i32, w: u32, h: u32, clip: Clip, color: Color) {
    let top = y.max(clip.top).max(0);
    let bottom = (y + h as i32).min(clip.bottom);
    if bottom <= top {
        return;
    }
    s.fill_rect(x, top as u32, w, (bottom - top) as u32, color);
}

/// Lay out a decorated box: apply margins, resolve the box width (explicit or
/// container-filling, optionally centred), paint background + border, inset the
/// content by border + padding, and draw the children. Returns the y below the
/// box including its bottom margin.
#[allow(clippy::too_many_arguments)]
fn draw_box(
    s: &mut Surface,
    style: &BoxStyle,
    children: &[Block],
    x0: u32,
    max_w: u32,
    y: i32,
    clip: Clip,
    page_bg: Color,
    link_hits: &mut Vec<Hit>,
    form: &FormCtx,
    input_hits: &mut Vec<Hit>,
    zone_hits: &mut Vec<Hit>,
) -> i32 {
    let bw = style.border_width as u32;
    let [pt, pr, pb, pl] = [
        style.padding[0] as u32,
        style.padding[1] as u32,
        style.padding[2] as u32,
        style.padding[3] as u32,
    ];
    let [mt, mr, mb, ml] = [
        style.margin[0] as u32,
        style.margin[1] as u32,
        style.margin[2] as u32,
        style.margin[3] as u32,
    ];

    // Horizontal: margins shrink the available band; an explicit width caps the
    // box, and a centred box splits the remaining space evenly.
    let avail = max_w.saturating_sub(ml + mr);
    let frame = bw * 2 + pl + pr;
    let outer_w = match style.width {
        // `width`/`max-width` give the content width; percentages resolve
        // against the available band.
        Some(w) => (w.resolve(avail) + frame).min(avail).max(frame + CHAR_W),
        None => avail,
    };
    let box_x = if style.center && outer_w < avail {
        x0 + ml + (avail - outer_w) / 2
    } else {
        x0 + ml
    };
    let content_x = box_x + bw + pl;
    let content_w = outer_w.saturating_sub(frame).max(CHAR_W);

    // Children paint against the box background so glyph anti-aliasing blends
    // correctly and text colour adapts to the box's own backdrop.
    let inner_bg = style.background.unwrap_or(page_bg);

    let measured = measure_blocks(s, children, content_w, inner_bg, form).max(0) as u32;
    let content_h = measured.max(style.min_height.unwrap_or(0));
    let outer_h = bw * 2 + pt + pb + content_h;

    let top = y + mt as i32;
    if let Some(bg) = style.background {
        fill_clipped(s, box_x, top, outer_w, outer_h, clip, bg);
    }
    if bw > 0 {
        let bc = style.border_color.unwrap_or(Color::rgb(148, 163, 184));
        fill_clipped(s, box_x, top, outer_w, bw, clip, bc); // top
        fill_clipped(s, box_x, top + (outer_h - bw) as i32, outer_w, bw, clip, bc); // bottom
        fill_clipped(s, box_x, top, bw, outer_h, clip, bc); // left
        fill_clipped(s, box_x + outer_w - bw, top, bw, outer_h, clip, bc); // right
    }

    let content_y = top + (bw + pt) as i32;
    draw_blocks(
        s, children, content_x, content_w, content_y, clip, inner_bg, link_hits, form, input_hits,
        zone_hits,
    );

    top + outer_h as i32 + mb as i32
}

/// Measure the height a block sequence occupies at width `w` without painting.
fn measure_blocks(
    s: &mut Surface,
    blocks: &[Block],
    w: u32,
    page_bg: Color,
    form: &FormCtx,
) -> i32 {
    let mut junk_l = Vec::new();
    let mut junk_i = Vec::new();
    let mut junk_z = Vec::new();
    draw_blocks(
        s,
        blocks,
        0,
        w,
        0,
        Clip::hidden(),
        page_bg,
        &mut junk_l,
        form,
        &mut junk_i,
        &mut junk_z,
    )
}

/// Lay out a flex container. Row items size from their `flex-basis` or content
/// (max-content), grow into free space, shrink toward min-content on overflow,
/// and — when `flex-wrap` is set — overflow onto new lines. `justify-content`
/// distributes spare main-axis space per line and `align-items` places items on
/// the cross axis. Column containers stack the children with `gap` between them.
#[allow(clippy::too_many_arguments)]
fn draw_flex_block(
    s: &mut Surface,
    direction: FlexDirection,
    justify: JustifyContent,
    align: AlignItems,
    gap: u16,
    wrap: bool,
    children: &[FlexChild],
    x0: u32,
    max_w: u32,
    y: i32,
    clip: Clip,
    page_bg: Color,
    link_hits: &mut Vec<Hit>,
    form: &FormCtx,
    input_hits: &mut Vec<Hit>,
    zone_hits: &mut Vec<Hit>,
) -> i32 {
    if children.is_empty() {
        return y;
    }
    let gap = gap as u32;

    if direction == FlexDirection::Column {
        let mut cy = y;
        for (i, child) in children.iter().enumerate() {
            if i > 0 {
                cy += gap as i32;
            }
            cy = draw_blocks(
                s,
                &child.blocks,
                x0,
                max_w,
                cy,
                clip,
                page_bg,
                link_hits,
                form,
                input_hits,
                zone_hits,
            );
        }
        return cy;
    }

    // ── Row layout: resolve each item's flex base and min size ────────────────
    let bases: Vec<(u32, u32)> = children
        .iter()
        .map(|c| {
            let (min, pref) = intrinsic_width(&c.blocks, form);
            let base = c.basis.map(|b| b.resolve(max_w)).unwrap_or(pref).min(max_w);
            (base, min.min(base))
        })
        .collect();

    // Group items into lines: a single line for nowrap, otherwise break before
    // an item that would overflow the current (non-empty) line.
    let mut lines: Vec<core::ops::Range<usize>> = Vec::new();
    let mut start = 0usize;
    let mut line_w = 0u32;
    for (i, &(base, _)) in bases.iter().enumerate() {
        let add = base + if i > start { gap } else { 0 };
        if wrap && i > start && line_w + add > max_w {
            lines.push(start..i);
            start = i;
            line_w = base;
        } else {
            line_w += add;
        }
    }
    lines.push(start..children.len());

    let mut cy = y;
    for (li, line) in lines.iter().enumerate() {
        if li > 0 {
            cy += gap as i32;
        }
        cy = draw_flex_line(
            s,
            children,
            &bases,
            line.clone(),
            justify,
            align,
            gap,
            x0,
            max_w,
            cy,
            clip,
            page_bg,
            link_hits,
            form,
            input_hits,
            zone_hits,
        );
    }
    cy
}

/// Lay out one flex line: resolve final widths (grow into slack / shrink toward
/// min-content on overflow), distribute the leftover per `justify-content`,
/// place each item on the cross axis per `align-items`, and paint. Returns the
/// y below the line.
#[allow(clippy::too_many_arguments)]
fn draw_flex_line(
    s: &mut Surface,
    children: &[FlexChild],
    bases: &[(u32, u32)],
    line: core::ops::Range<usize>,
    justify: JustifyContent,
    align: AlignItems,
    gap: u32,
    x0: u32,
    max_w: u32,
    y: i32,
    clip: Clip,
    page_bg: Color,
    link_hits: &mut Vec<Hit>,
    form: &FormCtx,
    input_hits: &mut Vec<Hit>,
    zone_hits: &mut Vec<Hit>,
) -> i32 {
    let idx: Vec<usize> = line.clone().collect();
    let n = idx.len() as u32;
    let total_gap = gap.saturating_mul(n.saturating_sub(1));
    let avail = max_w.saturating_sub(total_gap).max(n);

    let mut widths: Vec<u32> = idx.iter().map(|&i| bases[i].0.min(avail)).collect();
    let sum_base: u32 = widths.iter().sum();
    let total_grow: u32 = idx.iter().map(|&i| children[i].grow as u32).sum();

    if total_grow > 0 && sum_base < avail {
        let free = avail - sum_base;
        let last_grow = idx.iter().rposition(|&i| children[i].grow > 0);
        let mut spent = 0u32;
        for (k, &i) in idx.iter().enumerate() {
            let g = children[i].grow as u32;
            if g == 0 {
                continue;
            }
            let add = if Some(k) == last_grow {
                free - spent
            } else {
                free * g / total_grow
            };
            widths[k] += add;
            spent += add;
        }
    } else if sum_base > avail {
        // Shrink toward each item's min-content size, proportional to slack.
        let overflow = sum_base - avail;
        let slack: u32 = idx
            .iter()
            .enumerate()
            .map(|(k, &i)| widths[k].saturating_sub(bases[i].1))
            .sum();
        // `denom` keeps the proportional division total; when there's no slack
        // (all items already at min-content) every cut is zero.
        let denom = slack.max(1);
        let mut removed = 0u32;
        for (k, &i) in idx.iter().enumerate() {
            let s_k = widths[k].saturating_sub(bases[i].1);
            let cut = (overflow * s_k / denom).min(s_k);
            widths[k] -= cut;
            removed += cut;
        }
        // Any rounding remainder comes off the widest still-shrinkable item.
        let mut rem = overflow.saturating_sub(removed);
        while rem > 0 {
            let Some((k, _)) = idx
                .iter()
                .enumerate()
                .filter(|&(k, &i)| widths[k] > bases[i].1)
                .max_by_key(|&(k, _)| widths[k])
            else {
                break;
            };
            widths[k] -= 1;
            rem -= 1;
        }
    }

    // Measure heights at the resolved widths to find the line height + cross
    // placement.
    let mut heights: Vec<i32> = Vec::with_capacity(idx.len());
    let mut row_h = 0i32;
    for (k, &i) in idx.iter().enumerate() {
        let h = measure_blocks(s, &children[i].blocks, widths[k], page_bg, form);
        heights.push(h);
        row_h = row_h.max(h);
    }

    let used: u32 = widths.iter().sum::<u32>() + total_gap;
    let leftover = max_w.saturating_sub(used);
    let (start_x, extra_gap) = match justify {
        JustifyContent::Start => (x0, 0),
        JustifyContent::Center => (x0 + leftover / 2, 0),
        JustifyContent::End => (x0 + leftover, 0),
        JustifyContent::SpaceBetween if n > 1 => (x0, leftover / (n - 1)),
        JustifyContent::SpaceBetween => (x0, 0),
        JustifyContent::SpaceAround => {
            let e = leftover / n;
            (x0 + e / 2, e)
        }
    };

    let mut cx = start_x;
    for (k, &i) in idx.iter().enumerate() {
        let cy = match align {
            AlignItems::Start | AlignItems::Stretch => y,
            AlignItems::Center => y + (row_h - heights[k]) / 2,
            AlignItems::End => y + (row_h - heights[k]),
        };
        draw_blocks(
            s,
            &children[i].blocks,
            cx,
            widths[k],
            cy,
            clip,
            page_bg,
            link_hits,
            form,
            input_hits,
            zone_hits,
        );
        cx += widths[k] + gap + extra_gap;
    }

    y + row_h
}

/// The (min-content, max-content) width of a block sequence in pixels. Blocks
/// stack vertically, so the sequence's width is the max over its blocks.
fn intrinsic_width(blocks: &[Block], form: &FormCtx) -> (u32, u32) {
    let mut min = 0;
    let mut pref = 0;
    for block in blocks {
        let (bmin, bpref) = match block {
            Block::Text { items, .. } => text_intrinsic(items, form),
            Block::Image { img, .. } => {
                let w = img.as_ref().map(|i| i.width).unwrap_or(64).min(320);
                (w.min(64), w)
            }
            Block::Rule => (0, 0),
            Block::Flex {
                direction,
                gap,
                children,
                ..
            } => flex_intrinsic(children, *gap as u32, *direction, form),
            Block::Box { style, children } => {
                let (cmin, cpref) = intrinsic_width(children, form);
                let frame = (style.border_width as u32) * 2
                    + style.padding[1] as u32
                    + style.padding[3] as u32
                    + style.margin[1] as u32
                    + style.margin[3] as u32;
                match style.width {
                    Some(crate::dom::Length::Px(w)) => (w + frame, w + frame),
                    _ => (cmin + frame, cpref + frame),
                }
            }
        };
        min = min.max(bmin);
        pref = pref.max(bpref);
    }
    (min, pref)
}

/// Intrinsic widths of one flow line of inline items: min-content is the widest
/// single atom; max-content is everything on one unwrapped line.
fn text_intrinsic(items: &[Inline], form: &FormCtx) -> (u32, u32) {
    let mut min = 0u32;
    let mut pref = 0u32;
    let mut first = true;
    for item in items {
        match item {
            Inline::Run(run) => {
                let scale = run.style.size.max(1) as u32;
                for word in run.text.split(' ') {
                    if word.is_empty() {
                        continue;
                    }
                    let w = word.chars().count() as u32 * CHAR_W * scale;
                    min = min.max(w);
                    pref += w + if first { 0 } else { CHAR_W };
                    first = false;
                }
            }
            Inline::Control(idx) => {
                if let Some(meta) = form.inputs.get(*idx) {
                    let (w, _) = control_dims(meta, u32::MAX);
                    min = min.max(w);
                    pref += w + if first { 0 } else { CHAR_W };
                    first = false;
                }
            }
        }
    }
    (min, pref)
}

/// Intrinsic widths of a flex container, honouring explicit pixel bases.
fn flex_intrinsic(
    children: &[FlexChild],
    gap: u32,
    direction: FlexDirection,
    form: &FormCtx,
) -> (u32, u32) {
    let item = |c: &FlexChild| -> (u32, u32) {
        match c.basis {
            Some(crate::dom::Length::Px(w)) => (w, w),
            _ => intrinsic_width(&c.blocks, form),
        }
    };
    if direction == FlexDirection::Column {
        children
            .iter()
            .map(item)
            .fold((0, 0), |(amin, apref), (bmin, bpref)| {
                (amin.max(bmin), apref.max(bpref))
            })
    } else {
        let n = children.len() as u32;
        let total_gap = gap.saturating_mul(n.saturating_sub(1));
        let (mut min, mut pref) = (0u32, total_gap);
        for c in children {
            let (cmin, cpref) = item(c);
            min = min.max(cmin); // items may wrap, so min-content is the widest
            pref += cpref;
        }
        (min, pref)
    }
}

/// Extra vertical breathing room a control adds to its line.
fn ctrl_gap(tok: &Tok) -> u32 {
    match tok {
        Tok::Ctrl { .. } => 6,
        Tok::Word { .. } => 0,
    }
}

fn draw_pre(s: &mut Surface, items: &[Inline], x0: u32, max_w: u32, mut y: i32, clip: Clip) -> i32 {
    let fg = Color::rgb(20, 28, 40);
    y += 8;
    let mut text = String::new();
    for item in items {
        if let Inline::Run(run) = item {
            text.push_str(&run.text);
        }
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
#[allow(clippy::too_many_arguments)]
pub fn draw_image_block(
    s: &mut Surface,
    alt: &str,
    img: &Option<libimage::DecodedImage>,
    error: Option<&str>,
    align: Align,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip: Clip,
) -> i32 {
    y += 8;
    let centered = |w: u32| match align {
        Align::Center => x0 + max_w.saturating_sub(w) / 2,
        Align::Right => x0 + max_w.saturating_sub(w),
        Align::Left => x0,
    };
    match img {
        Some(im) if im.width > 0 && im.height > 0 => {
            let (tw, th) = fit_dimensions(im.width, im.height, max_w, 320);
            if clip.visible(y, th as i32) {
                blit_scaled(s, im, centered(tw), y, tw, th, clip);
            }
            y + th as i32 + 10
        }
        _ => {
            let (w, h) = (max_w.min(260), 64u32);
            let ix = centered(w);
            if clip.visible(y, h as i32) {
                s.fill_rect(ix, y as u32, w, h, PANEL);
                s.draw_rect(ix, y as u32, w, h, PANEL_BORDER);
                let label = if alt.is_empty() {
                    String::from("[image]")
                } else {
                    let mut l = String::from("[image] ");
                    l.push_str(alt);
                    l
                };
                let label = if let Some(err) = error {
                    let mut l = label;
                    l.push_str(" - ");
                    l.push_str(err);
                    l
                } else {
                    label
                };
                s.draw_string(
                    ix + 8,
                    y as u32 + h / 2 - 4,
                    &truncate_for_width(&label, w.saturating_sub(16)),
                    MUTED,
                    PANEL,
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
