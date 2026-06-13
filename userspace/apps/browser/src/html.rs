//! Document assembly: DOM tree + computed styles → renderer blocks.
//!
//! `parse_html` runs the full pipeline: the HTML5 tokenizer and tree builder
//! ([`crate::tokenizer`], [`crate::domtree`]) produce a DOM, every `<style>`
//! element feeds the [`Stylesheet`], and this module's flattener walks the
//! tree resolving each element's computed style ([`crate::style`]) while
//! emitting the flat [`Document`] the renderer consumes.
//!
//! The flattener implements the block model: headings, paragraphs and flow
//! containers, nested ordered/unordered lists (`type`, `start`, `value`,
//! roman/alpha numbering), definition lists, blockquotes, preformatted text,
//! tables linearised one row per line with `|` separators, rules, images,
//! links, and form controls (`input`, `select`, `textarea`, `button`).
//! Whitespace collapses per CSS rules; `text-transform`, visibility, and
//! `display: none` pruning are applied here so the renderer stays a dumb
//! painter.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libgui::color::Color;

use crate::css::{Stylesheet, TextTransform};
use crate::dom::{
    Align, Block, BoxStyle, Document, FlexChild, Inline, InputKind, InputMeta, Position,
    PositionedBox, Run, RunStyle, TextKind,
};
use crate::domtree::{build_dom, Dom, Element, NodeData, DOCUMENT};
use crate::style::{self, Computed};

/// A fully processed page: the renderer document plus anything page scripts
/// wrote to the console (forwarded to the system log by the browser).
pub struct PageOutput {
    pub doc: Document,
    /// Read by the host test harness; the browser itself uses [`load_page`].
    #[allow(dead_code)]
    pub console: Vec<String>,
}

/// A live page: the DOM and script runtime stay around so later events
/// (clicks) can run handlers and the page can be re-flattened.
pub struct LoadedPage {
    pub dom: Dom,
    /// Present when scripting was enabled; holds the global scope and the
    /// page's registered event handlers.
    pub runtime: Option<crate::js::Runtime>,
    pub doc: Document,
    pub console: Vec<String>,
}

/// Parse a full HTML document into the renderer's model, scripting disabled.
/// External stylesheets are not fetched — use [`load_page`].
pub fn parse_html(html: &str) -> Document {
    parse_html_with_css(html, |_| None)
}

/// Parse HTML with external stylesheets but scripting disabled.
pub fn parse_html_with_css(
    html: &str,
    mut fetch_css: impl FnMut(&str) -> Option<String>,
) -> Document {
    parse_document(html, &mut fetch_css, &mut |_| None, false).doc
}

/// One-shot pipeline (the live DOM/runtime are dropped). Used by tests and
/// callers that don't dispatch events.
pub fn parse_document(
    html: &str,
    fetch_css: &mut dyn FnMut(&str) -> Option<String>,
    fetch_js: &mut dyn FnMut(&str) -> Option<String>,
    scripting: bool,
) -> PageOutput {
    let page = load_page(html, fetch_css, fetch_js, scripting);
    PageOutput {
        doc: page.doc,
        console: page.console,
    }
}

/// Run the full page pipeline: tree construction, then (when `scripting`)
/// every `<script>` in document order against the live DOM plus the
/// `DOMContentLoaded`/`load` events, then stylesheet collection and
/// flattening — so script-made DOM and style mutations are visible in the
/// rendered output. The returned [`LoadedPage`] keeps the DOM and runtime
/// alive for event dispatch.
///
/// `fetch_css` / `fetch_js` receive `href`/`src` values as written in the
/// document and return the resource text, or `None` to skip. Keeping fetches
/// behind closures lets the network-aware caller resolve relative URLs and
/// bound how much is loaded, while this module stays pure.
pub fn load_page(
    html: &str,
    fetch_css: &mut dyn FnMut(&str) -> Option<String>,
    fetch_js: &mut dyn FnMut(&str) -> Option<String>,
    scripting: bool,
) -> LoadedPage {
    let mut dom = build_dom(html);
    let mut console = Vec::new();
    let mut runtime = None;
    if scripting {
        let mut rt = crate::js::Runtime::new();
        rt.run_load_scripts(&mut dom, fetch_js, &mut console);
        runtime = Some(rt);
    }

    let clickable = runtime
        .as_ref()
        .map(|rt| rt.click_targets())
        .unwrap_or_default();
    let doc = flatten_dom(&dom, fetch_css, scripting, &clickable);
    LoadedPage {
        dom,
        runtime,
        doc,
        console,
    }
}

/// Styling + flattening over an existing DOM: collect author CSS (styles are
/// gathered fresh each time so script-injected `<style>` and mutated `style`
/// attributes take effect), then flatten to renderer blocks. `clickable`
/// lists node ids with script click handlers (sorted), so their text gets
/// clickable regions. Used both at load and to re-render after events.
pub fn flatten_dom(
    dom: &Dom,
    fetch_css: &mut dyn FnMut(&str) -> Option<String>,
    scripting: bool,
    clickable: &[usize],
) -> Document {
    let mut sheet = Stylesheet::new();
    collect_styles(dom, DOCUMENT, &mut sheet, fetch_css);

    let mut f = Flattener::new(dom, &sheet, scripting, clickable);
    f.walk_node(DOCUMENT, &Computed::default(), None, None);
    f.finish()
}

/// Gather author CSS into the stylesheet in tree order, so rules in `<head>`
/// apply to the whole body: inline `<style>` blocks and external sheets linked
/// with `<link rel="stylesheet">` (fetched via `fetch_css`).
fn collect_styles(
    dom: &Dom,
    id: usize,
    sheet: &mut Stylesheet,
    fetch_css: &mut dyn FnMut(&str) -> Option<String>,
) {
    match dom.tag(id) {
        "style" => {
            let mut css = String::new();
            dom.text_content(id, &mut css);
            sheet.parse_into(&css);
            return;
        }
        "link" => {
            if let Some(el) = dom.element(id) {
                let rel = el.attr("rel").unwrap_or("");
                let is_sheet = rel
                    .split_ascii_whitespace()
                    .any(|r| r.eq_ignore_ascii_case("stylesheet"));
                if is_sheet {
                    if let Some(href) = el.attr("href").filter(|h| !h.trim().is_empty()) {
                        if let Some(css) = fetch_css(href) {
                            sheet.parse_into(&css);
                        }
                    }
                }
            }
            return;
        }
        _ => {}
    }
    for &c in &dom.nodes[id].children {
        collect_styles(dom, c, sheet, fetch_css);
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tag → block classification
// ────────────────────────────────────────────────────────────────────────────

/// Block kind for elements that start a new flow block.
fn block_kind(tag: &str) -> Option<TextKind> {
    Some(match tag {
        "h1" => TextKind::H1,
        "h2" => TextKind::H2,
        "h3" | "h4" | "h5" | "h6" => TextKind::H3,
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "center" | "nav"
        | "aside" | "figure" | "figcaption" | "address" | "fieldset" | "legend" | "details"
        | "summary" | "dialog" | "hgroup" | "search" | "caption" | "dt" | "dl" | "marquee" => {
            TextKind::Paragraph
        }
        "blockquote" => TextKind::Quote,
        "pre" | "xmp" | "listing" | "plaintext" => TextKind::Pre,
        _ => return None,
    })
}

/// Subtrees that never produce visible content.
fn skip_subtree(tag: &str) -> bool {
    matches!(
        tag,
        "script"
            | "style"
            | "template"
            | "svg"
            | "math"
            | "iframe"
            | "object"
            | "embed"
            | "applet"
            | "datalist"
            | "colgroup"
            | "map"
            | "noembed"
            | "noframes"
    )
}

/// Elements that never establish a flex container even when CSS asks: void or
/// replaced elements whose layout the renderer already owns.
fn is_flex_ineligible(tag: &str) -> bool {
    matches!(
        tag,
        "br" | "hr" | "img" | "input" | "select" | "textarea" | "button" | "table"
    )
}

/// Flow containers that may carry box decoration. Restricted to generic block
/// boxes and headings so elements with bespoke layout (lists, tables, `pre`,
/// blockquotes) keep their existing rendering.
fn is_boxable(tag: &str) -> bool {
    matches!(
        block_kind(tag),
        Some(TextKind::H1 | TextKind::H2 | TextKind::H3 | TextKind::Paragraph)
    )
}

/// Assemble a [`BoxStyle`] from a computed style.
fn box_style_of(cs: &Computed) -> BoxStyle {
    BoxStyle {
        background: cs.background,
        padding: cs.padding,
        margin: cs.margin,
        center: cs.margin_center,
        border_width: cs.border_width,
        border_color: cs.border_color,
        radius: cs.border_radius,
        border_box: cs.box_sizing_border,
        width: cs.box_width,
        min_height: cs.box_height.map(u32::from),
        shadow: cs.box_shadow,
        position: cs.position,
        inset: cs.inset,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// List numbering
// ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Numbering {
    Decimal,
    AlphaLower,
    AlphaUpper,
    RomanLower,
    RomanUpper,
}

struct ListCtx {
    ordered: bool,
    counter: i32,
    numbering: Numbering,
}

fn format_alpha(mut n: u32) -> String {
    let mut s = String::new();
    while n > 0 {
        n -= 1;
        s.insert(0, (b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    s
}

fn format_roman(n: u32) -> String {
    if n == 0 || n > 3999 {
        return alloc::format!("{}", n);
    }
    const TABLE: &[(u32, &str)] = &[
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    let mut rest = n;
    for &(v, sym) in TABLE {
        while rest >= v {
            s.push_str(sym);
            rest -= v;
        }
    }
    s
}

// ────────────────────────────────────────────────────────────────────────────
// Flattener
// ────────────────────────────────────────────────────────────────────────────

struct Flattener<'a> {
    dom: &'a Dom,
    sheet: &'a Stylesheet,

    blocks: Vec<Block>,
    positioned: Vec<PositionedBox>,
    items: Vec<Inline>,
    cur_kind: TextKind,
    cur_align: Option<Align>,
    cur_marker: Option<String>,
    last_was_space: bool,

    links: Vec<String>,
    link_nodes: Vec<usize>,
    inputs: Vec<InputMeta>,
    input_nodes: Vec<usize>,
    click_nodes: Vec<usize>,
    form_action: String,
    list_stack: Vec<ListCtx>,
    pre_depth: u32,
    quote_depth: u32,

    title: String,
    background: Option<Color>,
    /// With scripting on, `<noscript>` content is inert and must not render.
    scripting: bool,
    /// Sorted node ids carrying script click handlers (from the JS runtime).
    clickable: &'a [usize],
}

impl<'a> Flattener<'a> {
    fn new(dom: &'a Dom, sheet: &'a Stylesheet, scripting: bool, clickable: &'a [usize]) -> Self {
        Self {
            dom,
            sheet,
            scripting,
            clickable,
            blocks: Vec::new(),
            positioned: Vec::new(),
            items: Vec::new(),
            cur_kind: TextKind::Paragraph,
            cur_align: None,
            cur_marker: None,
            last_was_space: true,
            links: Vec::new(),
            link_nodes: Vec::new(),
            inputs: Vec::new(),
            input_nodes: Vec::new(),
            click_nodes: Vec::new(),
            form_action: String::new(),
            list_stack: Vec::new(),
            pre_depth: 0,
            quote_depth: 0,
            title: String::new(),
            background: None,
        }
    }

    fn finish(mut self) -> Document {
        self.flush_block();
        if self.blocks.is_empty() {
            self.blocks.push(Block::Text {
                kind: TextKind::Paragraph,
                items: alloc::vec![Inline::Run(Run {
                    text: String::from("(empty document)"),
                    link: None,
                    zone: None,
                    style: RunStyle::default(),
                })],
                align: Align::Left,
                marker: None,
            });
        }
        Document {
            title: self.title,
            background: self.background,
            blocks: self.blocks,
            positioned: self.positioned,
            links: self.links,
            link_nodes: self.link_nodes,
            inputs: self.inputs,
            input_nodes: self.input_nodes,
            click_nodes: self.click_nodes,
        }
    }

    // ── Tree walk ───────────────────────────────────────────────────────────

    fn walk_children(
        &mut self,
        id: usize,
        cs: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        let dom = self.dom;
        for &c in &dom.nodes[id].children {
            self.walk_node(c, cs, link, zone);
        }
    }

    fn walk_node(
        &mut self,
        id: usize,
        parent: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        let dom = self.dom;
        match &dom.nodes[id].data {
            NodeData::Document => self.walk_children(id, parent, link, zone),
            NodeData::Text(t) => self.emit_text(t, parent, link, zone),
            NodeData::Element(el) => self.walk_element(id, el, parent, link, zone),
        }
    }

    /// Whether `id` carries a JavaScript click handler — via a registered
    /// listener/property handler or an `onclick=""` attribute.
    fn is_clickable(&self, id: usize, el: &Element) -> bool {
        el.attr("onclick").is_some() || self.clickable.binary_search(&id).is_ok()
    }

    fn walk_element(
        &mut self,
        id: usize,
        el: &'a Element,
        parent: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        let tag = el.tag();
        // Open a clickable region for elements with click handlers, so the
        // renderer emits hit rects for their text.
        let zone = if self.scripting && self.is_clickable(id, el) {
            self.click_nodes.push(id);
            Some(self.click_nodes.len() - 1)
        } else {
            zone
        };
        if skip_subtree(tag) {
            return;
        }
        if tag == "noscript" && self.scripting {
            return; // scripting enabled: noscript fallback stays hidden
        }
        if tag == "title" {
            if self.title.is_empty() {
                let mut t = String::new();
                self.dom.text_content(id, &mut t);
                self.title = collapse_ws(&t);
            }
            return;
        }

        let (cs, display_none) = style::compute(self.dom, id, self.sheet, parent);
        if display_none {
            return;
        }

        if (tag == "html" || tag == "body") && cs.background.is_some() {
            self.background = cs.background;
        }

        // A decorated flow container (background/padding/border) is wrapped in
        // a box block; its layout mode (normal flow or flex) becomes the box's
        // content. This composes box decoration with flex on the same element.
        if is_boxable(tag) && box_style_of(&cs).needs_box() {
            self.emit_box(id, tag, &cs, link, zone);
            return;
        }

        // A `display: flex` container lays its element children out along a
        // main axis. Replaced/void elements ignore flex (they have no flow
        // children to arrange).
        if cs.flex_container && !is_flex_ineligible(tag) {
            self.emit_flex(id, &cs, link, zone);
            return;
        }

        match tag {
            "br" => self.line_break(),
            "hr" => {
                self.flush_block();
                if cs.visible {
                    self.blocks.push(Block::Rule);
                }
            }
            "img" => self.emit_image(el, &cs),
            "input" => self.emit_input(id, el, &cs),
            "select" => self.emit_select(id, el, &cs),
            "textarea" => self.emit_textarea(id, el, &cs),
            "button" => self.emit_button(id, el, &cs),
            "a" => {
                let new_link = el.attr("href").map(|href| {
                    self.links.push(String::from(href));
                    self.link_nodes.push(id);
                    self.links.len() - 1
                });
                self.walk_children(id, &cs, new_link.or(link), zone);
            }
            "ul" | "ol" | "menu" | "dir" => self.walk_list(id, el, &cs, link, zone),
            "li" => self.walk_list_item(id, &cs, link, zone),
            "dd" => {
                // Definition descriptions render as indented, markerless items.
                let prev = self.begin_block(TextKind::ListItem);
                self.walk_children(id, &cs, link, zone);
                self.end_block(prev);
            }
            "table" => self.walk_table(id, &cs, link, zone),
            "form" => {
                let prev_action = core::mem::take(&mut self.form_action);
                self.form_action = el.attr("action").unwrap_or("").to_string();
                let prev = self.begin_block(TextKind::Paragraph);
                self.walk_children(id, &cs, link, zone);
                self.end_block(prev);
                self.form_action = prev_action;
            }
            "blockquote" => {
                self.quote_depth += 1;
                let prev = self.begin_block(TextKind::Quote);
                self.walk_children(id, &cs, link, zone);
                self.end_block(prev);
                self.quote_depth -= 1;
            }
            "pre" | "xmp" | "listing" | "plaintext" => {
                self.pre_depth += 1;
                let prev = self.begin_block(TextKind::Pre);
                self.walk_children(id, &cs, link, zone);
                self.end_block(prev);
                self.pre_depth -= 1;
            }
            _ => match block_kind(tag) {
                Some(kind) => {
                    // Paragraph-level blocks inside a blockquote keep the
                    // quote presentation.
                    let kind = if kind == TextKind::Paragraph && self.quote_depth > 0 {
                        TextKind::Quote
                    } else {
                        kind
                    };
                    let prev = self.begin_block(kind);
                    self.walk_children(id, &cs, link, zone);
                    self.end_block(prev);
                }
                None => self.walk_children(id, &cs, link, zone), // inline element
            },
        }
    }

    // ── Block management ────────────────────────────────────────────────────

    /// Flush any open block and switch to `kind`; returns the previous kind
    /// for [`Self::end_block`] to restore.
    fn begin_block(&mut self, kind: TextKind) -> TextKind {
        self.flush_block();
        let prev = self.cur_kind;
        self.cur_kind = kind;
        prev
    }

    fn end_block(&mut self, prev: TextKind) {
        self.flush_block();
        self.cur_kind = prev;
    }

    /// `<br>`: end the current line but stay in the same block kind.
    fn line_break(&mut self) {
        let kind = self.cur_kind;
        self.flush_block();
        self.cur_kind = kind;
    }

    fn flush_block(&mut self) {
        let has_content = self.items.iter().any(|it| match it {
            Inline::Run(r) => r.text.chars().any(|c| !c.is_whitespace()),
            Inline::Control(_) => true,
        });
        if has_content {
            self.blocks.push(Block::Text {
                kind: self.cur_kind,
                items: core::mem::take(&mut self.items),
                align: self.cur_align.unwrap_or(Align::Left),
                marker: self.cur_marker.take(),
            });
        } else {
            self.items.clear();
            self.cur_marker = None;
        }
        self.cur_align = None;
        self.last_was_space = true;
    }

    /// Record the block's alignment the first time content lands in it.
    fn note_align(&mut self, cs: &Computed) {
        if self.cur_align.is_none() {
            self.cur_align = Some(cs.align.unwrap_or(Align::Left));
        }
    }

    // ── Flex containers ───────────────────────────────────────────────────────

    /// Emit a [`Block::Flex`] for a `display: flex` element. Each element child
    /// becomes a flex item with its own captured sub-flow; runs of significant
    /// text between elements become anonymous items so bare text still lays out.
    fn emit_flex(&mut self, id: usize, cs: &Computed, link: Option<usize>, zone: Option<usize>) {
        self.flush_block();
        let dom = self.dom;
        let mut children: Vec<FlexChild> = Vec::new();
        for &c in &dom.nodes[id].children {
            match &dom.nodes[c].data {
                NodeData::Element(_) => {
                    // Resolve the child's style once for its flex sizing inputs
                    // and to honour `display: none`.
                    let (ccs, none) = style::compute(dom, c, self.sheet, cs);
                    if none {
                        continue;
                    }
                    let blocks = self.capture_blocks(|f| f.walk_node(c, cs, link, zone));
                    if blocks.is_empty() {
                        continue;
                    }
                    children.push(FlexChild {
                        grow: ccs.flex_grow,
                        basis: ccs.flex_basis,
                        blocks,
                    });
                }
                NodeData::Text(t) => {
                    if t.chars().any(|ch| !ch.is_whitespace()) {
                        let text = t.clone();
                        let blocks = self.capture_blocks(|f| f.emit_text(&text, cs, link, zone));
                        if !blocks.is_empty() {
                            children.push(FlexChild {
                                grow: 0,
                                basis: None,
                                blocks,
                            });
                        }
                    }
                }
                NodeData::Document => {}
            }
        }
        if children.is_empty() {
            return;
        }
        self.blocks.push(Block::Flex {
            direction: cs.flex_direction,
            justify: cs.justify_content,
            align: cs.align_items,
            gap: cs.gap,
            wrap: cs.flex_wrap,
            children,
        });
    }

    /// Emit a [`Block::Box`] for a decorated flow container. Its content is the
    /// element's captured sub-flow — a flex layout when the element is also a
    /// flex container, otherwise normal flow.
    fn emit_box(
        &mut self,
        id: usize,
        tag: &str,
        cs: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        self.flush_block();
        let style = box_style_of(cs);
        let kind = block_kind(tag).unwrap_or(TextKind::Paragraph);
        let as_flex = cs.flex_container && !is_flex_ineligible(tag);
        let children = self.capture_blocks(|f| {
            if as_flex {
                f.emit_flex(id, cs, link, zone);
            } else {
                f.cur_kind = kind;
                f.walk_children(id, cs, link, zone);
            }
        });
        // An empty, purely-padded box has nothing to show; keep it only if it
        // paints (background/border) or holds content.
        if children.is_empty() && style.background.is_none() && style.border_width == 0 {
            return;
        }
        // Out-of-flow boxes are hoisted to the document level and painted in a
        // deferred pass; they take no space in the normal flow.
        if matches!(cs.position, Position::Absolute | Position::Fixed) {
            self.positioned.push(PositionedBox {
                style,
                blocks: children,
            });
        } else {
            self.blocks.push(Block::Box { style, children });
        }
    }

    /// Run `body` against a fresh block/inline buffer and return the blocks it
    /// produced, restoring the previous flow state afterwards. Side tables
    /// (links, inputs, click zones) stay shared so item-local indices remain
    /// valid in the final [`Document`].
    fn capture_blocks(&mut self, body: impl FnOnce(&mut Self)) -> Vec<Block> {
        let saved_blocks = core::mem::take(&mut self.blocks);
        let saved_items = core::mem::take(&mut self.items);
        let saved_kind = self.cur_kind;
        let saved_align = self.cur_align.take();
        let saved_marker = self.cur_marker.take();
        let saved_space = self.last_was_space;

        self.cur_kind = TextKind::Paragraph;
        self.last_was_space = true;
        body(self);
        self.flush_block();

        let produced = core::mem::replace(&mut self.blocks, saved_blocks);
        self.items = saved_items;
        self.cur_kind = saved_kind;
        self.cur_align = saved_align;
        self.cur_marker = saved_marker;
        self.last_was_space = saved_space;
        produced
    }

    // ── Lists ───────────────────────────────────────────────────────────────

    fn walk_list(
        &mut self,
        id: usize,
        el: &Element,
        cs: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        self.flush_block();
        let ordered = el.tag() == "ol";
        let numbering = match el.attr("type") {
            Some("a") => Numbering::AlphaLower,
            Some("A") => Numbering::AlphaUpper,
            Some("i") => Numbering::RomanLower,
            Some("I") => Numbering::RomanUpper,
            _ => Numbering::Decimal,
        };
        let start = el
            .attr("start")
            .and_then(|s| s.trim().parse::<i32>().ok())
            .unwrap_or(1);
        self.list_stack.push(ListCtx {
            ordered,
            counter: start - 1,
            numbering,
        });
        self.walk_children(id, cs, link, zone);
        self.list_stack.pop();
        self.flush_block();
    }

    fn walk_list_item(
        &mut self,
        id: usize,
        cs: &Computed,
        link: Option<usize>,
        zone: Option<usize>,
    ) {
        let marker = self.next_marker(id);
        let prev = self.begin_block(TextKind::ListItem);
        self.cur_marker = Some(marker);
        self.walk_children(id, cs, link, zone);
        self.end_block(prev);
    }

    fn next_marker(&mut self, li: usize) -> String {
        let depth = self.list_stack.len().saturating_sub(1).min(4);
        let value = self
            .dom
            .element(li)
            .and_then(|el| el.attr("value"))
            .and_then(|v| v.trim().parse::<i32>().ok());
        let mut marker = String::new();
        for _ in 0..depth {
            marker.push_str("  ");
        }
        match self.list_stack.last_mut() {
            Some(ctx) if ctx.ordered => {
                ctx.counter = value.unwrap_or(ctx.counter + 1);
                let n = ctx.counter.max(0) as u32;
                let body = match ctx.numbering {
                    Numbering::Decimal => alloc::format!("{}", ctx.counter),
                    Numbering::AlphaLower => format_alpha(n.max(1)),
                    Numbering::AlphaUpper => format_alpha(n.max(1)).to_ascii_uppercase(),
                    Numbering::RomanLower => format_roman(n.max(1)),
                    Numbering::RomanUpper => format_roman(n.max(1)).to_ascii_uppercase(),
                };
                marker.push_str(&body);
                marker.push('.');
            }
            _ => marker.push(match depth {
                0 => '*',
                1 => '-',
                _ => '+',
            }),
        }
        marker
    }

    // ── Tables ──────────────────────────────────────────────────────────────

    /// Linearise a table: caption as its own block, then one block per row
    /// with `|`-separated cells.
    fn walk_table(&mut self, id: usize, cs: &Computed, link: Option<usize>, zone: Option<usize>) {
        self.flush_block();
        let dom = self.dom;
        for &child in &dom.nodes[id].children {
            match dom.tag(child) {
                "caption" => {
                    let (ccs, none) = style::compute(dom, child, self.sheet, cs);
                    if !none {
                        let prev = self.begin_block(TextKind::Paragraph);
                        self.walk_children(child, &ccs, link, zone);
                        self.end_block(prev);
                    }
                }
                "thead" | "tbody" | "tfoot" => {
                    let (scs, none) = style::compute(dom, child, self.sheet, cs);
                    if !none {
                        for &row in &dom.nodes[child].children {
                            if dom.tag(row) == "tr" {
                                self.walk_row(row, &scs, link, zone);
                            }
                        }
                    }
                }
                "tr" => self.walk_row(child, cs, link, zone),
                _ => {}
            }
        }
        self.flush_block();
    }

    fn walk_row(&mut self, row: usize, cs: &Computed, link: Option<usize>, zone: Option<usize>) {
        let dom = self.dom;
        let (rcs, none) = style::compute(dom, row, self.sheet, cs);
        if none {
            return;
        }
        let prev = self.begin_block(TextKind::Paragraph);
        let mut first = true;
        for &cell in &dom.nodes[row].children {
            if !matches!(dom.tag(cell), "td" | "th") {
                continue;
            }
            let (ccs, cnone) = style::compute(dom, cell, self.sheet, &rcs);
            if cnone {
                continue;
            }
            if !first {
                self.items.push(Inline::Run(Run {
                    text: String::from(" | "),
                    link: None,
                    zone: None,
                    style: RunStyle::default(),
                }));
                self.last_was_space = true;
            }
            first = false;
            self.note_align(&rcs);
            self.walk_children(cell, &ccs, link, zone);
        }
        self.end_block(prev);
    }

    // ── Text ────────────────────────────────────────────────────────────────

    fn run_style(cs: &Computed) -> RunStyle {
        RunStyle {
            color: cs.color,
            bold: cs.bold,
            italic: cs.italic,
            mono: cs.mono,
            underline: cs.underline,
            strike: cs.strike,
            size: scale_for_px(cs.font_px),
        }
    }

    fn emit_text(&mut self, text: &str, cs: &Computed, link: Option<usize>, zone: Option<usize>) {
        if !cs.visible {
            return;
        }
        if self.pre_depth > 0 {
            // Spec: the newline immediately after `<pre>` is dropped.
            let text = if self.items.is_empty() {
                text.strip_prefix('\n').unwrap_or(text)
            } else {
                text
            };
            if text.is_empty() {
                return;
            }
            self.note_align(cs);
            self.items.push(Inline::Run(Run {
                text: String::from(text),
                link,
                zone,
                style: Self::run_style(cs),
            }));
            return;
        }

        let mut out = String::new();
        for ch in text.chars() {
            if ch.is_whitespace() {
                if !self.last_was_space {
                    out.push(' ');
                    self.last_was_space = true;
                }
            } else {
                let ch = match cs.transform {
                    Some(TextTransform::Uppercase) => ch.to_ascii_uppercase(),
                    Some(TextTransform::Lowercase) => ch.to_ascii_lowercase(),
                    Some(TextTransform::Capitalize) if self.last_was_space => {
                        ch.to_ascii_uppercase()
                    }
                    _ => ch,
                };
                out.push(ch);
                self.last_was_space = false;
            }
        }
        if out.is_empty() || out == " " && self.items.is_empty() {
            return;
        }
        self.note_align(cs);
        self.items.push(Inline::Run(Run {
            text: out,
            link,
            zone,
            style: Self::run_style(cs),
        }));
    }

    // ── Replaced elements and form controls ────────────────────────────────

    fn emit_image(&mut self, el: &Element, cs: &Computed) {
        self.flush_block();
        if !cs.visible {
            return;
        }
        self.blocks.push(Block::Image {
            alt: el.attr("alt").unwrap_or("").to_string(),
            img: None,
            error: None,
            src: el.attr("src").unwrap_or("").to_string(),
            align: cs.align.unwrap_or(Align::Left),
        });
    }

    fn emit_input(&mut self, id: usize, el: &Element, cs: &Computed) {
        let kind = match el.attr("type").unwrap_or("text") {
            "submit" | "button" | "reset" => InputKind::Submit,
            "search" => InputKind::Search,
            "hidden" => return,
            _ => InputKind::Text,
        };
        if !cs.visible {
            return;
        }
        let placeholder = el
            .attr("placeholder")
            .or_else(|| el.attr("value"))
            .unwrap_or("")
            .to_string();
        self.push_control(
            id,
            cs,
            InputMeta {
                kind,
                placeholder,
                name: el.attr("name").unwrap_or("").to_string(),
                action: self.form_action.clone(),
                size: el.attr("size").and_then(|s| s.trim().parse().ok()),
                options: Vec::new(),
            },
        );
    }

    fn emit_select(&mut self, id: usize, el: &Element, cs: &Computed) {
        if !cs.visible {
            return;
        }
        let mut options: Vec<String> = Vec::new();
        let mut chosen: Option<usize> = None;
        collect_options(self.dom, id, &mut options, &mut chosen);
        let placeholder = chosen
            .and_then(|i| options.get(i))
            .or_else(|| options.first())
            .cloned()
            .unwrap_or_default();
        self.push_control(
            id,
            cs,
            InputMeta {
                kind: InputKind::Select,
                placeholder,
                name: el.attr("name").unwrap_or("").to_string(),
                action: self.form_action.clone(),
                size: None,
                options,
            },
        );
    }

    fn emit_textarea(&mut self, id: usize, el: &Element, cs: &Computed) {
        if !cs.visible {
            return;
        }
        let mut content = String::new();
        self.dom.text_content(id, &mut content);
        let placeholder = match el.attr("placeholder") {
            Some(p) if !p.is_empty() => String::from(p),
            _ => collapse_ws(&content),
        };
        self.push_control(
            id,
            cs,
            InputMeta {
                kind: InputKind::Text,
                placeholder,
                name: el.attr("name").unwrap_or("").to_string(),
                action: self.form_action.clone(),
                size: el.attr("cols").and_then(|s| s.trim().parse().ok()),
                options: Vec::new(),
            },
        );
    }

    fn emit_button(&mut self, id: usize, el: &Element, cs: &Computed) {
        if !cs.visible {
            return;
        }
        if el.attr("type").is_some_and(|t| t == "hidden") {
            return;
        }
        let mut label = String::new();
        self.dom.text_content(id, &mut label);
        self.push_control(
            id,
            cs,
            InputMeta {
                kind: InputKind::Submit,
                placeholder: collapse_ws(&label),
                name: el.attr("name").unwrap_or("").to_string(),
                action: self.form_action.clone(),
                size: None,
                options: Vec::new(),
            },
        );
    }

    fn push_control(&mut self, node: usize, cs: &Computed, meta: InputMeta) {
        self.note_align(cs);
        let idx = self.inputs.len();
        self.inputs.push(meta);
        self.input_nodes.push(node);
        self.items.push(Inline::Control(idx));
        self.last_was_space = true;
    }
}

/// Collect `<option>` labels under a `<select>`; `chosen` is the first option
/// carrying the `selected` attribute.
fn collect_options(dom: &Dom, id: usize, out: &mut Vec<String>, chosen: &mut Option<usize>) {
    for &c in &dom.nodes[id].children {
        match dom.tag(c) {
            "option" => {
                let mut label = String::new();
                dom.text_content(c, &mut label);
                let label = collapse_ws(&label);
                if !label.is_empty() {
                    if chosen.is_none()
                        && dom.element(c).is_some_and(|e| e.attr("selected").is_some())
                    {
                        *chosen = Some(out.len());
                    }
                    out.push(label);
                }
            }
            "optgroup" => collect_options(dom, c, out, chosen),
            _ => {}
        }
    }
}

/// Map a computed `font-size` in CSS pixels to an integer glyph scale. The
/// base bitmap glyph is 8px, so sizes bucket into a few discrete steps; most
/// body text (≤20px) renders at the native 1× and only larger headings grow.
fn scale_for_px(px: u16) -> u8 {
    match px {
        0..=20 => 1,
        21..=30 => 2,
        31..=46 => 3,
        47..=62 => 4,
        _ => 5,
    }
}

/// Collapse runs of whitespace to single spaces and trim the ends.
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut was_space = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !was_space {
                out.push(' ');
                was_space = true;
            }
        } else {
            out.push(ch);
            was_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}
