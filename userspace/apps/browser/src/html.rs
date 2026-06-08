//! A forgiving, single-pass HTML5 tokenizer and block builder.
//!
//! The parser produces the flat [`Document`] the renderer consumes. It supports
//! a practical subset of HTML5: document metadata (`<title>`), headings,
//! paragraphs and the common flow containers, ordered/unordered lists,
//! blockquotes, preformatted text, rules, images, form controls, and a range of
//! inline formatting elements (`b`/`strong`, `i`/`em`, `code`, `u`, `a`, …).
//! Inline and block styling is resolved here — from element defaults, inline
//! `style="..."` attributes, and a [`Stylesheet`] gathered from `<style>`
//! blocks — so the renderer stays a dumb, fast painter.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libgui::color::Color;

use crate::css::{self, Stylesheet};
use crate::dom::{Align, Block, Document, Inline, InputKind, InputMeta, Run, RunStyle, TextKind};
use crate::text::{
    decode_entities, eq_ignore_ascii_case, find_ignore_ascii_case, resolve_entity, SmallStr,
};

/// Parse a full HTML document into the renderer's model.
pub fn parse_html(html: &str) -> Document {
    let sheet = extract_stylesheet(html);
    let mut parser = HtmlParser::new(html, &sheet);
    parser.parse();
    Document {
        title: parser.title,
        blocks: parser.blocks,
        links: parser.links,
        inputs: parser.inputs,
    }
}

/// Pre-pass: collect every `<style>` block into a single stylesheet so rules in
/// `<head>` apply to the body that follows them.
fn extract_stylesheet(html: &str) -> Stylesheet {
    let bytes = html.as_bytes();
    let mut sheet = Stylesheet::new();
    let mut pos = 0;
    while let Some(rel) = find_ignore_ascii_case(&bytes[pos..], b"<style") {
        let tag_open = pos + rel;
        let Some(gt) = bytes[tag_open..].iter().position(|&b| b == b'>') else {
            break;
        };
        let content_start = tag_open + gt + 1;
        let Some(close_rel) = find_ignore_ascii_case(&bytes[content_start..], b"</style") else {
            break;
        };
        let content_end = content_start + close_rel;
        if let Ok(css) = core::str::from_utf8(&bytes[content_start..content_end]) {
            sheet.parse_into(css);
        }
        pos = content_end;
    }
    sheet
}

// ────────────────────────────────────────────────────────────────────────────
// Attribute parsing (single pass per tag)
// ────────────────────────────────────────────────────────────────────────────

/// Find one attribute by name, scanning the tag's attribute bytes lazily.
///
/// This is deliberately on-demand rather than parsing every attribute up front:
/// large real-world pages have thousands of elements, and eagerly allocating a
/// list (plus a decoded `String` per attribute) for every tag floods the bump
/// heap and pegs the CPU. We allocate only the single value we actually need.
fn find_attr(attrs: &[u8], name: &[u8]) -> Option<String> {
    let mut i = 0;
    while i < attrs.len() {
        skip_ws(attrs, &mut i);
        let name_start = i;
        while i < attrs.len()
            && (attrs[i].is_ascii_alphanumeric()
                || attrs[i] == b'-'
                || attrs[i] == b'_'
                || attrs[i] == b':')
        {
            i += 1;
        }
        let attr_name = &attrs[name_start..i];
        skip_ws(attrs, &mut i);

        if i < attrs.len() && attrs[i] == b'=' {
            i += 1;
            skip_ws(attrs, &mut i);
            let raw = read_attr_value(attrs, &mut i);
            if eq_ignore_ascii_case(attr_name, name) {
                return Some(decode_entities(core::str::from_utf8(raw).unwrap_or("")));
            }
        } else if eq_ignore_ascii_case(attr_name, name) {
            // Boolean attribute present without a value (e.g. `selected`).
            return Some(String::new());
        } else if attr_name.is_empty() {
            i += 1; // skip a stray byte (e.g. `/` in self-closing tags)
        }
    }
    None
}

/// Read (without decoding) a quoted or unquoted attribute value slice.
fn read_attr_value<'a>(attrs: &'a [u8], i: &mut usize) -> &'a [u8] {
    if *i >= attrs.len() {
        return &[];
    }
    if attrs[*i] == b'\'' || attrs[*i] == b'"' {
        let quote = attrs[*i];
        *i += 1;
        let start = *i;
        while *i < attrs.len() && attrs[*i] != quote {
            *i += 1;
        }
        let v = &attrs[start..*i];
        if *i < attrs.len() {
            *i += 1;
        }
        v
    } else {
        let start = *i;
        while *i < attrs.len() && !attrs[*i].is_ascii_whitespace() {
            *i += 1;
        }
        &attrs[start..*i]
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Inline style stack
// ────────────────────────────────────────────────────────────────────────────

/// One styled element on the open-element stack, recording exactly which global
/// effects it applied so they can be undone precisely when it closes.
#[derive(Clone, Copy)]
struct Frame {
    tag: SmallStr,
    bold: bool,
    mono: bool,
    underline: bool,
    color: bool,
    hidden: bool,
    center: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Skip {
    None,
    Script,
    Style,
}

struct HtmlParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    sheet: &'a Stylesheet,

    blocks: Vec<Block>,
    items: Vec<Inline>,
    cur: String,
    cur_kind: TextKind,
    cur_marker: Option<String>,
    cur_align: Option<Align>,

    links: Vec<String>,
    cur_link: Option<usize>,
    inputs: Vec<InputMeta>,
    form_action: String,
    list_stack: Vec<ListCtx>,

    // `<select>` accumulation: option text is captured, not rendered as body.
    select_idx: Option<usize>,
    opt_text: String,
    opt_selected: bool,
    opt_chosen: Option<String>,
    opt_first: Option<String>,

    // Inline style accumulation.
    frames: Vec<Frame>,
    bold_depth: u32,
    mono_depth: u32,
    underline_depth: u32,
    hidden_depth: u32,
    center_depth: u32,
    color_stack: Vec<Color>,

    title: String,
    in_title: bool,
    in_pre: bool,
    skip: Skip,
    last_was_space: bool,
}

struct ListCtx {
    ordered: bool,
    counter: u32,
}

impl<'a> HtmlParser<'a> {
    fn new(html: &'a str, sheet: &'a Stylesheet) -> Self {
        Self {
            bytes: html.as_bytes(),
            pos: 0,
            sheet,
            blocks: Vec::new(),
            items: Vec::new(),
            cur: String::new(),
            cur_kind: TextKind::Paragraph,
            cur_marker: None,
            cur_align: None,
            links: Vec::new(),
            cur_link: None,
            inputs: Vec::new(),
            form_action: String::new(),
            list_stack: Vec::new(),
            select_idx: None,
            opt_text: String::new(),
            opt_selected: false,
            opt_chosen: None,
            opt_first: None,
            frames: Vec::new(),
            bold_depth: 0,
            mono_depth: 0,
            underline_depth: 0,
            hidden_depth: 0,
            center_depth: 0,
            color_stack: Vec::new(),
            title: String::new(),
            in_title: false,
            in_pre: false,
            skip: Skip::None,
            last_was_space: true,
        }
    }

    fn parse(&mut self) {
        while self.pos < self.bytes.len() {
            if self.skip != Skip::None {
                self.skip_element();
                continue;
            }
            match self.bytes[self.pos] {
                b'<' => self.parse_tag(),
                b'&' => self.parse_entity(),
                b => {
                    self.pos += 1;
                    self.push_char(b as char);
                }
            }
        }
        self.flush_block();
        if self.blocks.is_empty() {
            self.push_text_block(TextKind::Paragraph, "(empty document)");
        }
    }

    // ── Tag dispatch ────────────────────────────────────────────────────────

    fn parse_tag(&mut self) {
        let start = self.pos + 1;
        let Some(end) = self.bytes[start..].iter().position(|&b| b == b'>') else {
            self.pos = self.bytes.len();
            return;
        };
        let tag_bytes = &self.bytes[start..start + end];
        self.pos = start + end + 1;

        if tag_bytes.starts_with(b"!--") || tag_bytes.starts_with(b"!") || tag_bytes.starts_with(b"?")
        {
            return; // comments, <!doctype>, processing instructions
        }

        let mut idx = 0;
        skip_ws(tag_bytes, &mut idx);
        let closing = tag_bytes.get(idx) == Some(&b'/');
        if closing {
            idx += 1;
        }
        skip_ws(tag_bytes, &mut idx);
        let name_start = idx;
        while idx < tag_bytes.len()
            && (tag_bytes[idx].is_ascii_alphanumeric() || tag_bytes[idx] == b'-')
        {
            idx += 1;
        }
        let name = SmallStr::lower(&tag_bytes[name_start..idx]);
        if name.is_empty() {
            return;
        }

        if closing {
            self.close_tag(name.as_str());
        } else {
            let attrs = &tag_bytes[idx..];
            self.open_tag(name, attrs);
        }
    }

    fn open_tag(&mut self, name: SmallStr, attrs: &[u8]) {
        let tag = name.as_str();
        match tag {
            "script" => {
                self.skip = Skip::Script;
                return;
            }
            "style" => {
                self.skip = Skip::Style;
                return;
            }
            "title" => {
                self.flush_block();
                self.in_title = true;
                self.cur.clear();
                self.last_was_space = true;
                return;
            }
            "br" => {
                self.flush_block();
                return;
            }
            "hr" => {
                self.flush_block();
                if self.hidden_depth == 0 {
                    self.blocks.push(Block::Rule);
                }
                return;
            }
            "img" => {
                self.emit_image(attrs);
                return;
            }
            "input" => {
                self.emit_input(attrs);
                return;
            }
            "select" => {
                self.open_select(attrs);
                return;
            }
            "option" => {
                self.begin_option(attrs);
                return;
            }
            "ul" | "ol" => {
                self.flush_block();
                self.list_stack.push(ListCtx {
                    ordered: tag == "ol",
                    counter: 0,
                });
            }
            "form" => {
                self.form_action = find_attr(attrs, b"action").unwrap_or_default();
            }
            "a" => {
                self.flush_run();
                let href = find_attr(attrs, b"href").unwrap_or_default();
                self.cur_link = Some(self.links.len());
                self.links.push(href);
            }
            _ => {}
        }

        // Block elements: start a new block and remember any list marker.
        if let Some(kind) = block_kind(tag) {
            self.start_block(kind);
            if kind == TextKind::ListItem {
                self.cur_marker = Some(self.next_list_marker());
            }
            if kind == TextKind::Pre {
                self.in_pre = true;
            }
        }

        // Apply element + CSS + inline styling as an unwindable frame.
        self.push_frame(name, attrs);
    }

    fn close_tag(&mut self, name: &str) {
        self.pop_frame(name);
        match name {
            "title" => {
                self.title = self.cur.trim().to_string();
                self.cur.clear();
                self.in_title = false;
                self.last_was_space = true;
            }
            "ul" | "ol" => {
                self.flush_block();
                self.list_stack.pop();
            }
            "select" => self.close_select(),
            "option" => self.commit_option(),
            "form" => self.form_action.clear(),
            "a" => {
                self.flush_run();
                self.cur_link = None;
            }
            "pre" => {
                self.flush_block();
                self.in_pre = false;
                self.cur_kind = TextKind::Paragraph;
            }
            _ if block_kind(name).is_some() => {
                self.flush_block();
                self.cur_kind = TextKind::Paragraph;
            }
            _ => {}
        }
    }

    // ── Styling frames ──────────────────────────────────────────────────────

    fn push_frame(&mut self, name: SmallStr, attrs: &[u8]) {
        let tag = name.as_str();
        let (mut bold, mono, mut underline) = inline_defaults(tag);
        let mut color: Option<Color> = None;
        let mut hidden = false;

        // Cascade: stylesheet rules (only when a sheet exists — avoids a class
        // lookup and rule scan for every element on large unstyled pages), then
        // the inline `style="..."` attribute, then a legacy `color` attribute.
        if !self.sheet.is_empty() {
            let classes = find_attr(attrs, b"class").unwrap_or_default();
            let css_style = self.sheet.style_for(tag, &classes);
            merge_css(&css_style, &mut bold, &mut underline, &mut color, &mut hidden);
        }
        if let Some(inline) = find_attr(attrs, b"style") {
            let s = css::parse_inline(&inline);
            merge_css(&s, &mut bold, &mut underline, &mut color, &mut hidden);
        }
        if let Some(c) = find_attr(attrs, b"color").as_deref().and_then(css::parse_color) {
            color = Some(c);
        }

        // Centre alignment from `<center>` or an `align="center"` attribute.
        let center = tag == "center"
            || matches!(
                find_attr(attrs, b"align").as_deref(),
                Some("center") | Some("middle")
            );

        if !(bold || mono || underline || color.is_some() || hidden || center) {
            return; // no effect — nothing to unwind later
        }

        self.flush_run();
        if bold {
            self.bold_depth += 1;
        }
        if mono {
            self.mono_depth += 1;
        }
        if underline {
            self.underline_depth += 1;
        }
        if hidden {
            self.hidden_depth += 1;
        }
        if center {
            self.center_depth += 1;
        }
        if let Some(c) = color {
            self.color_stack.push(c);
        }
        self.frames.push(Frame {
            tag: name,
            bold,
            mono,
            underline,
            color: color.is_some(),
            hidden,
            center,
        });
    }

    fn pop_frame(&mut self, name: &str) {
        let Some(pos) = self.frames.iter().rposition(|f| f.tag.eq(name)) else {
            return;
        };
        self.flush_run();
        let frame = self.frames.remove(pos);
        if frame.bold {
            self.bold_depth -= 1;
        }
        if frame.mono {
            self.mono_depth -= 1;
        }
        if frame.underline {
            self.underline_depth -= 1;
        }
        if frame.hidden {
            self.hidden_depth -= 1;
        }
        if frame.center {
            self.center_depth -= 1;
        }
        if frame.color {
            self.color_stack.pop();
        }
    }

    fn current_style(&self) -> RunStyle {
        RunStyle {
            color: self.color_stack.last().copied(),
            bold: self.bold_depth > 0,
            mono: self.mono_depth > 0,
            underline: self.underline_depth > 0,
        }
    }

    // ── Void elements ───────────────────────────────────────────────────────

    fn emit_image(&mut self, attrs: &[u8]) {
        self.flush_block();
        if self.hidden_depth > 0 {
            return;
        }
        self.blocks.push(Block::Image {
            alt: find_attr(attrs, b"alt").unwrap_or_default(),
            img: None,
            src: find_attr(attrs, b"src").unwrap_or_default(),
            align: self.current_align(),
        });
    }

    /// Emit a form control inline within the current flow block.
    fn emit_input(&mut self, attrs: &[u8]) {
        let kind = match find_attr(attrs, b"type").as_deref().unwrap_or("text") {
            "submit" | "button" | "reset" => InputKind::Submit,
            "search" => InputKind::Search,
            "hidden" => return,
            _ => InputKind::Text,
        };
        if self.hidden_depth > 0 {
            return;
        }
        let placeholder = find_attr(attrs, b"placeholder")
            .or_else(|| find_attr(attrs, b"value"))
            .unwrap_or_default();
        let size = find_attr(attrs, b"size").and_then(|s| s.trim().parse().ok());
        self.push_control(InputMeta {
            kind,
            placeholder,
            name: find_attr(attrs, b"name").unwrap_or_default(),
            action: self.form_action.clone(),
            size,
        });
    }

    fn open_select(&mut self, attrs: &[u8]) {
        if self.hidden_depth > 0 {
            return;
        }
        self.opt_text.clear();
        self.opt_selected = false;
        self.opt_chosen = None;
        self.opt_first = None;
        let idx = self.inputs.len();
        self.select_idx = Some(idx);
        self.push_control(InputMeta {
            kind: InputKind::Select,
            placeholder: String::new(),
            name: find_attr(attrs, b"name").unwrap_or_default(),
            action: self.form_action.clone(),
            size: None,
        });
    }

    /// Finalise the option being captured, then start a new one.
    fn begin_option(&mut self, attrs: &[u8]) {
        self.commit_option();
        self.opt_selected = find_attr(attrs, b"selected").is_some();
    }

    fn commit_option(&mut self) {
        if self.select_idx.is_none() {
            return;
        }
        let text = self.opt_text.trim();
        if !text.is_empty() {
            if self.opt_first.is_none() {
                self.opt_first = Some(text.to_string());
            }
            if self.opt_selected && self.opt_chosen.is_none() {
                self.opt_chosen = Some(text.to_string());
            }
        }
        self.opt_text.clear();
        self.opt_selected = false;
    }

    fn close_select(&mut self) {
        self.commit_option();
        if let Some(idx) = self.select_idx.take() {
            let label = self
                .opt_chosen
                .take()
                .or_else(|| self.opt_first.take())
                .unwrap_or_default();
            if let Some(meta) = self.inputs.get_mut(idx) {
                meta.placeholder = label;
            }
        }
    }

    /// Register an input's metadata and place it inline in the current block.
    fn push_control(&mut self, meta: InputMeta) {
        self.flush_run();
        self.note_align();
        let idx = self.inputs.len();
        self.inputs.push(meta);
        self.items.push(Inline::Control(idx));
    }

    // ── Text accumulation ───────────────────────────────────────────────────

    fn parse_entity(&mut self) {
        let start = self.pos + 1;
        let max_end = (start + 12).min(self.bytes.len());
        for end in start..max_end {
            if self.bytes[end] == b';' {
                let entity = &self.bytes[start..end];
                self.pos = end + 1;
                let rep = resolve_entity(entity).unwrap_or(" ");
                self.push_text(rep);
                return;
            }
        }
        self.pos += 1;
        self.push_char('&');
    }

    fn push_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.push_char(ch);
        }
    }

    fn push_char(&mut self, ch: char) {
        if self.skip != Skip::None || self.hidden_depth > 0 {
            return;
        }
        // Inside a <select>, text belongs to <option> labels, not the page body.
        if self.select_idx.is_some() {
            self.opt_text.push(ch);
            return;
        }
        if self.in_title {
            self.cur.push(ch);
            return;
        }
        if self.in_pre {
            if ch != '\r' {
                self.cur.push(ch);
            }
            return;
        }
        if ch.is_whitespace() {
            if !self.last_was_space && !self.cur.is_empty() {
                self.cur.push(' ');
                self.last_was_space = true;
            }
        } else {
            self.cur.push(ch);
            self.last_was_space = false;
        }
    }

    // ── Block / run flushing ────────────────────────────────────────────────

    fn start_block(&mut self, kind: TextKind) {
        self.flush_block();
        self.cur_kind = kind;
        self.last_was_space = true;
    }

    fn next_list_marker(&mut self) -> String {
        match self.list_stack.last_mut() {
            Some(ctx) if ctx.ordered => {
                ctx.counter += 1;
                alloc::format!("{}.", ctx.counter)
            }
            _ => String::from("*"),
        }
    }

    fn flush_run(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        self.note_align();
        let text = core::mem::take(&mut self.cur);
        self.items.push(Inline::Run(Run {
            text,
            link: self.cur_link,
            style: self.current_style(),
        }));
    }

    fn flush_block(&mut self) {
        self.flush_run();
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
        self.cur_kind = TextKind::Paragraph;
        self.last_was_space = true;
    }

    /// Record the current alignment the first time content lands in a block.
    fn note_align(&mut self) {
        if self.cur_align.is_none() {
            self.cur_align = Some(self.current_align());
        }
    }

    fn current_align(&self) -> Align {
        if self.center_depth > 0 {
            Align::Center
        } else {
            Align::Left
        }
    }

    fn push_text_block(&mut self, kind: TextKind, text: &str) {
        self.blocks.push(Block::Text {
            kind,
            items: alloc::vec![Inline::Run(Run {
                text: String::from(text),
                link: None,
                style: RunStyle::default(),
            })],
            align: Align::Left,
            marker: None,
        });
    }

    // ── Skipping raw-text elements (script/style) ───────────────────────────

    fn skip_element(&mut self) {
        let needle: &[u8] = match self.skip {
            Skip::Script => b"</script",
            Skip::Style => b"</style",
            Skip::None => return,
        };
        match find_ignore_ascii_case(&self.bytes[self.pos..], needle) {
            Some(rel) => {
                self.pos += rel;
                if let Some(gt) = self.bytes[self.pos..].iter().position(|&b| b == b'>') {
                    self.pos += gt + 1;
                } else {
                    self.pos = self.bytes.len();
                }
            }
            None => self.pos = self.bytes.len(),
        }
        self.skip = Skip::None;
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Tag classification helpers
// ────────────────────────────────────────────────────────────────────────────

/// Map a tag to its block kind, or `None` for inline / structural tags.
fn block_kind(tag: &str) -> Option<TextKind> {
    Some(match tag {
        "h1" => TextKind::H1,
        "h2" => TextKind::H2,
        "h3" | "h4" | "h5" | "h6" => TextKind::H3,
        "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "center" | "nav"
        | "aside" | "figure" | "figcaption" | "td" | "th" | "tr" | "caption" | "dd" | "dt"
        | "dl" => TextKind::Paragraph,
        "li" => TextKind::ListItem,
        "blockquote" => TextKind::Quote,
        "pre" => TextKind::Pre,
        _ => return None,
    })
}

/// Default inline effects (bold, mono, underline) for a formatting tag.
fn inline_defaults(tag: &str) -> (bool, bool, bool) {
    match tag {
        "b" | "strong" | "mark" => (true, false, false),
        "code" | "tt" | "kbd" | "samp" | "var" => (false, true, false),
        "u" | "ins" => (false, false, true),
        _ => (false, false, false),
    }
}

fn merge_css(
    s: &css::Style,
    bold: &mut bool,
    underline: &mut bool,
    color: &mut Option<Color>,
    hidden: &mut bool,
) {
    if let Some(b) = s.bold {
        *bold = b;
    }
    if let Some(u) = s.underline {
        *underline = u;
    }
    if let Some(c) = s.color {
        *color = Some(c);
    }
    *hidden |= s.hidden;
}

fn skip_ws(bytes: &[u8], idx: &mut usize) {
    while *idx < bytes.len() && bytes[*idx].is_ascii_whitespace() {
        *idx += 1;
    }
}
