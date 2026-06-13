//! Computed styles: cascade resolution and inheritance over the DOM tree.
//!
//! For each element, declaration layers apply in the CSS cascade order:
//! user-agent defaults → presentational attributes (`bgcolor`, `align`,
//! `color`, `face`, `hidden`, body `text`) → stylesheet rules (specificity +
//! source order) → inline `style` → stylesheet `!important` → inline
//! `!important`. Inheritable properties (color, font, decoration, alignment,
//! transform, visibility) flow from the parent's computed style.

use libgui::color::Color;

use crate::css::{self, Decls, FontSize, Stylesheet, TextTransform};
use crate::dom::{Align, AlignItems, BoxShadow, FlexDirection, JustifyContent, Length, Position};
use crate::domtree::{Dom, Element};

/// The default (`medium`) font size in CSS pixels.
pub const DEFAULT_FONT_PX: u16 = 16;

/// Fully resolved style for one element.
#[derive(Clone, Copy)]
pub struct Computed {
    /// `None` means "use the renderer's default for the block kind".
    pub color: Option<Color>,
    /// This element's own background (not inherited; the renderer only paints
    /// the page background, taken from `html`/`body`).
    pub background: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    pub strike: bool,
    /// `visibility`; a hidden ancestor can be overridden back to visible.
    pub visible: bool,
    pub align: Option<Align>,
    pub transform: Option<TextTransform>,
    /// Computed `font-size` in CSS pixels (inherited; `em`/`%` resolve here).
    pub font_px: u16,
    // ── Flex (not inherited; reset per element) ──────────────────────────────
    /// This element establishes a flex formatting context (`display: flex`).
    pub flex_container: bool,
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    /// `flex-wrap: wrap`.
    pub flex_wrap: bool,
    /// `gap` between flex items, in pixels.
    pub gap: u16,
    /// This element's `flex-grow` factor (read when it is a flex item).
    pub flex_grow: u16,
    /// This element's preferred main-axis size (`flex-basis`/`width`).
    pub flex_basis: Option<Length>,
    /// Box-model padding in pixels (top, right, bottom, left).
    pub padding: [u16; 4],
    /// Box-model margin in pixels (top, right, bottom, left).
    pub margin: [u16; 4],
    /// Horizontal `margin: auto` centring.
    pub margin_center: bool,
    /// Uniform border width in pixels and its colour.
    pub border_width: u16,
    pub border_color: Option<Color>,
    /// Corner radius in pixels (`border-radius`).
    pub border_radius: u16,
    /// `box-sizing: border-box`.
    pub box_sizing_border: bool,
    /// Drop shadow (`box-shadow`).
    pub box_shadow: Option<BoxShadow>,
    /// `position` and its `top`/`right`/`bottom`/`left` offsets.
    pub position: Position,
    pub inset: [Option<Length>; 4],
    /// Explicit content width (`width`/`max-width`); may be a percentage.
    pub box_width: Option<Length>,
    /// Fixed content-area height (`height`/`max-height`) in pixels.
    pub box_height: Option<u16>,
    /// Content-area height floor (`min-height`) in pixels.
    pub box_min_height: Option<u16>,
    /// `overflow` other than `visible`.
    pub overflow_clip: bool,
    /// Paint order among positioned boxes (`z-index`).
    pub z_index: i32,
}

impl Default for Computed {
    fn default() -> Self {
        Self {
            color: None,
            background: None,
            bold: false,
            italic: false,
            mono: false,
            underline: false,
            strike: false,
            visible: true,
            align: None,
            transform: None,
            font_px: DEFAULT_FONT_PX,
            flex_container: false,
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::Start,
            align_items: AlignItems::Stretch,
            flex_wrap: false,
            gap: 0,
            flex_grow: 0,
            flex_basis: None,
            padding: [0; 4],
            margin: [0; 4],
            margin_center: false,
            border_width: 0,
            border_color: None,
            border_radius: 0,
            box_sizing_border: false,
            box_shadow: None,
            position: Position::Static,
            inset: [None; 4],
            box_width: None,
            box_height: None,
            box_min_height: None,
            overflow_clip: false,
            z_index: 0,
        }
    }
}

/// Resolve the computed style of element `el`. Returns the style and whether
/// the element is `display: none` (subtree pruned by the caller).
pub fn compute(dom: &Dom, el: usize, sheet: &Stylesheet, parent: &Computed) -> (Computed, bool) {
    // Inherited properties start from the parent; flex layout inputs and the
    // background are element-local and reset to their initial values.
    let mut out = Computed {
        background: None,
        flex_container: false,
        flex_direction: FlexDirection::Row,
        justify_content: JustifyContent::Start,
        align_items: AlignItems::Stretch,
        flex_wrap: false,
        gap: 0,
        flex_grow: 0,
        flex_basis: None,
        padding: [0; 4],
        margin: [0; 4],
        margin_center: false,
        border_width: 0,
        border_color: None,
        border_radius: 0,
        box_sizing_border: false,
        box_shadow: None,
        position: Position::Static,
        inset: [None; 4],
        box_width: None,
        box_height: None,
        box_min_height: None,
        overflow_clip: false,
        z_index: 0,
        ..*parent
    };
    let Some(element) = dom.element(el) else {
        return (out, false);
    };

    let mut acc = ua_defaults(element.tag());
    presentational_hints(element, &mut acc);

    if !sheet.is_empty() {
        let mut normal = Decls::default();
        let mut important = Decls::default();
        sheet.match_element(dom, el, &mut normal, &mut important);
        acc.overlay(&normal);
        if let Some(style) = element.attr("style") {
            let (inline_normal, inline_important) = css::parse_declarations(style);
            acc.overlay(&inline_normal);
            acc.overlay(&important);
            acc.overlay(&inline_important);
        } else {
            acc.overlay(&important);
        }
    } else if let Some(style) = element.attr("style") {
        let (inline_normal, inline_important) = css::parse_declarations(style);
        acc.overlay(&inline_normal);
        acc.overlay(&inline_important);
    }

    apply(&mut out, &acc);
    // Resolve font-size last: relative units cascade off the parent's pixels.
    if let Some(fs) = acc.font_size {
        out.font_px = match fs {
            FontSize::Px(px) => px,
            FontSize::Percent(p) => ((parent.font_px as u32 * p as u32) / 100).min(4000) as u16,
        }
        .clamp(6, 400);
    }
    (out, acc.display_none.unwrap_or(false))
}

fn apply(out: &mut Computed, d: &Decls) {
    if let Some(c) = d.color {
        out.color = Some(c);
    }
    if let Some(c) = d.background {
        out.background = Some(c);
    }
    if let Some(v) = d.bold {
        out.bold = v;
    }
    if let Some(v) = d.italic {
        out.italic = v;
    }
    if let Some(v) = d.mono {
        out.mono = v;
    }
    if let Some(v) = d.underline {
        out.underline = v;
    }
    if let Some(v) = d.strike {
        out.strike = v;
    }
    if let Some(v) = d.visible {
        out.visible = v;
    }
    if let Some(a) = d.align {
        out.align = Some(a);
    }
    if let Some(t) = d.transform {
        out.transform = t;
    }
    if let Some(v) = d.flex_container {
        out.flex_container = v;
    }
    if let Some(v) = d.flex_direction {
        out.flex_direction = v;
    }
    if let Some(v) = d.justify_content {
        out.justify_content = v;
    }
    if let Some(v) = d.align_items {
        out.align_items = v;
    }
    if let Some(v) = d.flex_wrap {
        out.flex_wrap = v;
    }
    if let Some(v) = d.gap {
        out.gap = v;
    }
    if let Some(v) = d.flex_grow {
        out.flex_grow = v;
    }
    if let Some(v) = d.flex_basis {
        out.flex_basis = Some(v);
    }
    if let Some(v) = d.pad_top {
        out.padding[0] = v;
    }
    if let Some(v) = d.pad_right {
        out.padding[1] = v;
    }
    if let Some(v) = d.pad_bottom {
        out.padding[2] = v;
    }
    if let Some(v) = d.pad_left {
        out.padding[3] = v;
    }
    if let Some(v) = d.border_width {
        out.border_width = v;
    }
    if let Some(c) = d.border_color {
        out.border_color = Some(c);
    }
    if let Some(v) = d.border_radius {
        out.border_radius = v;
    }
    if let Some(v) = d.box_sizing_border {
        out.box_sizing_border = v;
    }
    if let Some(v) = d.box_shadow {
        out.box_shadow = Some(v);
    }
    if let Some(v) = d.position {
        out.position = v;
    }
    if d.inset_top.is_some() {
        out.inset[0] = d.inset_top;
    }
    if d.inset_right.is_some() {
        out.inset[1] = d.inset_right;
    }
    if d.inset_bottom.is_some() {
        out.inset[2] = d.inset_bottom;
    }
    if d.inset_left.is_some() {
        out.inset[3] = d.inset_left;
    }
    if let Some(v) = d.margin_top {
        out.margin[0] = v;
    }
    if let Some(v) = d.margin_right {
        out.margin[1] = v;
    }
    if let Some(v) = d.margin_bottom {
        out.margin[2] = v;
    }
    if let Some(v) = d.margin_left {
        out.margin[3] = v;
    }
    if let Some(v) = d.margin_center {
        out.margin_center = v;
    }
    if let Some(v) = d.box_width {
        out.box_width = Some(v);
    }
    if let Some(v) = d.box_height {
        out.box_height = Some(v);
    }
    if let Some(v) = d.box_min_height {
        out.box_min_height = Some(v);
    }
    if let Some(v) = d.overflow_clip {
        out.overflow_clip = v;
    }
    if let Some(v) = d.z_index {
        out.z_index = v;
    }
}

/// Default rendering for HTML elements (the user-agent stylesheet).
fn ua_defaults(tag: &str) -> Decls {
    let mut d = Decls::default();
    match tag {
        "b" | "strong" | "mark" | "th" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            d.bold = Some(true)
        }
        "i" | "em" | "var" | "cite" | "dfn" | "address" => d.italic = Some(true),
        "code" | "tt" | "kbd" | "samp" | "pre" | "xmp" | "listing" | "plaintext" => {
            d.mono = Some(true)
        }
        "u" | "ins" => d.underline = Some(true),
        "s" | "strike" | "del" => d.strike = Some(true),
        "center" => d.align = Some(Align::Center),
        "caption" => d.align = Some(Align::Center),
        _ => {}
    }
    // Heading and small-print default sizes (the bitmap font scales in integer
    // steps, so these bucket into a few distinct glyph scales in the renderer).
    d.font_size = match tag {
        "h1" => Some(FontSize::Px(32)),
        "h2" => Some(FontSize::Px(26)),
        "h3" => Some(FontSize::Px(20)),
        "h4" => Some(FontSize::Px(16)),
        "h5" => Some(FontSize::Px(14)),
        "h6" | "small" | "sub" | "sup" => Some(FontSize::Px(13)),
        "big" => Some(FontSize::Px(20)),
        _ => None,
    };
    if tag == "th" {
        d.align = Some(Align::Center);
    }
    if tag == "var" {
        d.mono = Some(true);
    }
    d
}

/// Legacy presentational attributes, applied below author CSS in the cascade.
fn presentational_hints(el: &Element, d: &mut Decls) {
    if el.attr("hidden").is_some() {
        d.display_none = Some(true);
    }
    if let Some(c) = el.attr("color").and_then(css::parse_color) {
        d.color = Some(c);
    }
    if let Some(c) = el.attr("bgcolor").and_then(css::parse_color) {
        d.background = Some(c);
    }
    if el.tag() == "body" {
        if let Some(c) = el.attr("text").and_then(css::parse_color) {
            d.color = Some(c);
        }
    }
    if let Some(face) = el.attr("face") {
        if ["mono", "courier", "consolas"]
            .iter()
            .any(|m| face.to_ascii_lowercase().contains(m))
        {
            d.mono = Some(true);
        }
    }
    if let Some(align) = el.attr("align") {
        d.align = match align.to_ascii_lowercase().as_str() {
            "center" | "middle" => Some(Align::Center),
            "right" => Some(Align::Right),
            "left" => Some(Align::Left),
            _ => d.align,
        };
    }
}
