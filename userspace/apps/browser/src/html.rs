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
use crate::dom::{Align, Block, Document, Inline, InputKind, InputMeta, Run, RunStyle, TextKind};
use crate::domtree::{build_dom, Dom, Element, NodeData, DOCUMENT};
use crate::style::{self, Computed};

/// Parse a full HTML document into the renderer's model.
pub fn parse_html(html: &str) -> Document {
    let dom = build_dom(html);
    let mut sheet = Stylesheet::new();
    collect_styles(&dom, DOCUMENT, &mut sheet);

    let mut f = Flattener::new(&dom, &sheet);
    f.walk_node(DOCUMENT, &Computed::default(), None);
    f.finish()
}

/// Gather every `<style>` element's text into the stylesheet, in tree order,
/// so rules in `<head>` apply to the whole body.
fn collect_styles(dom: &Dom, id: usize, sheet: &mut Stylesheet) {
    if dom.tag(id) == "style" {
        let mut css = String::new();
        dom.text_content(id, &mut css);
        sheet.parse_into(&css);
        return;
    }
    for &c in &dom.nodes[id].children {
        collect_styles(dom, c, sheet);
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
        "script" | "style" | "template" | "svg" | "math" | "iframe" | "object" | "embed"
            | "applet" | "datalist" | "colgroup" | "map" | "noembed" | "noframes"
    )
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
        (1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"),
        (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i"),
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
    items: Vec<Inline>,
    cur_kind: TextKind,
    cur_align: Option<Align>,
    cur_marker: Option<String>,
    last_was_space: bool,

    links: Vec<String>,
    inputs: Vec<InputMeta>,
    form_action: String,
    list_stack: Vec<ListCtx>,
    pre_depth: u32,
    quote_depth: u32,

    title: String,
    background: Option<Color>,
}

impl<'a> Flattener<'a> {
    fn new(dom: &'a Dom, sheet: &'a Stylesheet) -> Self {
        Self {
            dom,
            sheet,
            blocks: Vec::new(),
            items: Vec::new(),
            cur_kind: TextKind::Paragraph,
            cur_align: None,
            cur_marker: None,
            last_was_space: true,
            links: Vec::new(),
            inputs: Vec::new(),
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
            links: self.links,
            inputs: self.inputs,
        }
    }

    // ── Tree walk ───────────────────────────────────────────────────────────

    fn walk_children(&mut self, id: usize, cs: &Computed, link: Option<usize>) {
        let dom = self.dom;
        for &c in &dom.nodes[id].children {
            self.walk_node(c, cs, link);
        }
    }

    fn walk_node(&mut self, id: usize, parent: &Computed, link: Option<usize>) {
        let dom = self.dom;
        match &dom.nodes[id].data {
            NodeData::Document => self.walk_children(id, parent, link),
            NodeData::Text(t) => self.emit_text(t, parent, link),
            NodeData::Element(el) => self.walk_element(id, el, parent, link),
        }
    }

    fn walk_element(&mut self, id: usize, el: &'a Element, parent: &Computed, link: Option<usize>) {
        let tag = el.tag();
        if skip_subtree(tag) {
            return;
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

        match tag {
            "br" => self.line_break(),
            "hr" => {
                self.flush_block();
                if cs.visible {
                    self.blocks.push(Block::Rule);
                }
            }
            "img" => self.emit_image(el, &cs),
            "input" => self.emit_input(el, &cs),
            "select" => self.emit_select(id, el, &cs),
            "textarea" => self.emit_textarea(id, el, &cs),
            "button" => self.emit_button(id, el, &cs),
            "a" => {
                let new_link = el.attr("href").map(|href| {
                    self.links.push(String::from(href));
                    self.links.len() - 1
                });
                self.walk_children(id, &cs, new_link.or(link));
            }
            "ul" | "ol" | "menu" | "dir" => self.walk_list(id, el, &cs, link),
            "li" => self.walk_list_item(id, &cs, link),
            "dd" => {
                // Definition descriptions render as indented, markerless items.
                let prev = self.begin_block(TextKind::ListItem);
                self.walk_children(id, &cs, link);
                self.end_block(prev);
            }
            "table" => self.walk_table(id, &cs, link),
            "form" => {
                let prev_action = core::mem::take(&mut self.form_action);
                self.form_action = el.attr("action").unwrap_or("").to_string();
                let prev = self.begin_block(TextKind::Paragraph);
                self.walk_children(id, &cs, link);
                self.end_block(prev);
                self.form_action = prev_action;
            }
            "blockquote" => {
                self.quote_depth += 1;
                let prev = self.begin_block(TextKind::Quote);
                self.walk_children(id, &cs, link);
                self.end_block(prev);
                self.quote_depth -= 1;
            }
            "pre" | "xmp" | "listing" | "plaintext" => {
                self.pre_depth += 1;
                let prev = self.begin_block(TextKind::Pre);
                self.walk_children(id, &cs, link);
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
                    self.walk_children(id, &cs, link);
                    self.end_block(prev);
                }
                None => self.walk_children(id, &cs, link), // inline element
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

    // ── Lists ───────────────────────────────────────────────────────────────

    fn walk_list(&mut self, id: usize, el: &Element, cs: &Computed, link: Option<usize>) {
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
        self.walk_children(id, cs, link);
        self.list_stack.pop();
        self.flush_block();
    }

    fn walk_list_item(&mut self, id: usize, cs: &Computed, link: Option<usize>) {
        let marker = self.next_marker(id);
        let prev = self.begin_block(TextKind::ListItem);
        self.cur_marker = Some(marker);
        self.walk_children(id, cs, link);
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
    fn walk_table(&mut self, id: usize, cs: &Computed, link: Option<usize>) {
        self.flush_block();
        let dom = self.dom;
        for &child in &dom.nodes[id].children {
            match dom.tag(child) {
                "caption" => {
                    let (ccs, none) = style::compute(dom, child, self.sheet, cs);
                    if !none {
                        let prev = self.begin_block(TextKind::Paragraph);
                        self.walk_children(child, &ccs, link);
                        self.end_block(prev);
                    }
                }
                "thead" | "tbody" | "tfoot" => {
                    let (scs, none) = style::compute(dom, child, self.sheet, cs);
                    if !none {
                        for &row in &dom.nodes[child].children {
                            if dom.tag(row) == "tr" {
                                self.walk_row(row, &scs, link);
                            }
                        }
                    }
                }
                "tr" => self.walk_row(child, cs, link),
                _ => {}
            }
        }
        self.flush_block();
    }

    fn walk_row(&mut self, row: usize, cs: &Computed, link: Option<usize>) {
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
                    style: RunStyle::default(),
                }));
                self.last_was_space = true;
            }
            first = false;
            self.note_align(&rcs);
            self.walk_children(cell, &ccs, link);
        }
        self.end_block(prev);
    }

    // ── Text ────────────────────────────────────────────────────────────────

    fn run_style(cs: &Computed) -> RunStyle {
        RunStyle {
            color: cs.color,
            bold: cs.bold,
            mono: cs.mono,
            underline: cs.underline,
            strike: cs.strike,
        }
    }

    fn emit_text(&mut self, text: &str, cs: &Computed, link: Option<usize>) {
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

    fn emit_input(&mut self, el: &Element, cs: &Computed) {
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

    fn push_control(&mut self, cs: &Computed, meta: InputMeta) {
        self.note_align(cs);
        let idx = self.inputs.len();
        self.inputs.push(meta);
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
