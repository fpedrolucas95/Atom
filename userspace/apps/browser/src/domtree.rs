//! HTML5 tree construction: tokens → DOM tree.
//!
//! Implements the load-bearing parts of the WHATWG tree-construction
//! algorithm on an index-based node arena:
//!
//! * implicit `html`/`head`/`body` synthesis and attribute merging,
//! * void elements and tokenizer content-model switching (script/style/title…),
//! * implied end tags — `<p>` auto-closing before blocks, `<li>`/`<dd>`/`<dt>`
//!   siblings, `<option>`/`<optgroup>`, and table-section/row/cell closing,
//! * scope-aware end-tag matching (button scope for `p`, list item scope for
//!   `li`, table scope for rows/cells),
//! * an active-formatting-elements list with reconstruction and a simplified
//!   adoption agency, so misnested markup like `<b>1<i>2</b>3</i>` styles each
//!   region correctly and `<b><div>x</b>y</div>` does not leak bold,
//! * the Noah's Ark clause (max 3 identical formatting entries),
//! * foster parenting — non-table content (text, `<div>`, `<p>`, …) appearing
//!   directly inside `<table>`/`<tr>`/table sections is moved to just before
//!   the table, as the spec requires, instead of being dropped or nested.

use alloc::string::String;
use alloc::vec::Vec;

use crate::text::SmallStr;
use crate::tokenizer::{Attrs, Mode, Token, Tokenizer};

/// Node id of the document root.
pub const DOCUMENT: usize = 0;

/// Maximum open-element depth. Beyond this, new elements still appear in the
/// tree but are not pushed onto the stack, so their children become siblings.
/// This bounds DOM depth — and with it the flattener's recursion — keeping
/// pathological nesting within the 512 KiB userspace stack.
const MAX_DEPTH: usize = 192;

pub struct Dom {
    pub nodes: Vec<Node>,
}

pub struct Node {
    pub parent: usize,
    pub children: Vec<usize>,
    pub data: NodeData,
}

pub enum NodeData {
    Document,
    Element(Element),
    Text(String),
}

pub struct Element {
    pub tag: SmallStr,
    pub attrs: Attrs,
}

impl Element {
    pub fn tag(&self) -> &str {
        self.tag.as_str()
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

impl Dom {
    pub fn element(&self, id: usize) -> Option<&Element> {
        match &self.nodes[id].data {
            NodeData::Element(el) => Some(el),
            _ => None,
        }
    }

    /// Element tag name, or `""` for text/document nodes.
    pub fn tag(&self, id: usize) -> &str {
        self.element(id).map(Element::tag).unwrap_or("")
    }

    /// Concatenate every descendant text node into `out`.
    pub fn text_content(&self, id: usize, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for &c in &self.nodes[id].children {
                    self.text_content(c, out);
                }
            }
        }
    }

    // ── Mutation (used by the JavaScript DOM bindings) ──────────────────────

    /// Create an orphan element (no parent until [`Dom::attach`]).
    pub fn create_element(&mut self, tag: &str, attrs: Attrs) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent: usize::MAX,
            children: Vec::new(),
            data: NodeData::Element(Element {
                tag: SmallStr::lower(tag.as_bytes()),
                attrs,
            }),
        });
        id
    }

    /// Create an orphan text node.
    pub fn create_text(&mut self, text: &str) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent: usize::MAX,
            children: Vec::new(),
            data: NodeData::Text(String::from(text)),
        });
        id
    }

    /// Remove `child` from its parent's child list (it stays in the arena).
    pub fn detach(&mut self, child: usize) {
        let parent = self.nodes[child].parent;
        if parent != usize::MAX {
            self.nodes[parent].children.retain(|&c| c != child);
        }
        self.nodes[child].parent = usize::MAX;
    }

    /// Attach `child` under `parent`, optionally before child index `before`.
    /// Re-attaching detaches from the old parent first. Cycles are refused.
    pub fn attach(&mut self, parent: usize, child: usize, before: Option<usize>) {
        if parent == child || self.is_ancestor(child, parent) {
            return;
        }
        self.detach(child);
        self.nodes[child].parent = parent;
        let children = &mut self.nodes[parent].children;
        match before {
            Some(i) if i <= children.len() => children.insert(i, child),
            _ => children.push(child),
        }
    }

    /// Whether `anc` is an ancestor of `node` (or the node itself).
    pub fn is_ancestor(&self, anc: usize, mut node: usize) -> bool {
        loop {
            if node == anc {
                return true;
            }
            let p = self.nodes[node].parent;
            if p == usize::MAX {
                return false;
            }
            node = p;
        }
    }

    /// Detach every child of `parent`.
    pub fn clear_children(&mut self, parent: usize) {
        let children = core::mem::take(&mut self.nodes[parent].children);
        for c in children {
            self.nodes[c].parent = usize::MAX;
        }
    }

    /// Walk up the ancestor chain from `node` (exclusive) and return the first
    /// node whose tag matches `tag`. Returns `None` at the root.
    pub fn find_ancestor_tag(&self, node: usize, tag: &str) -> Option<usize> {
        let mut cur = self.nodes[node].parent;
        loop {
            if cur == usize::MAX {
                return None;
            }
            if self.tag(cur) == tag {
                return Some(cur);
            }
            cur = self.nodes[cur].parent;
        }
    }

    /// First element with `tag` in tree order, searched iteratively.
    pub fn find_first(&self, tag: &str) -> Option<usize> {
        let mut stack = alloc::vec![DOCUMENT];
        while let Some(id) = stack.pop() {
            if self.tag(id) == tag {
                return Some(id);
            }
            for &c in self.nodes[id].children.iter().rev() {
                stack.push(c);
            }
        }
        None
    }

    pub fn set_attr(&mut self, id: usize, name: &str, value: &str) {
        if let NodeData::Element(el) = &mut self.nodes[id].data {
            let name = name.to_ascii_lowercase();
            if let Some(slot) = el.attrs.iter_mut().find(|(n, _)| *n == name) {
                slot.1 = String::from(value);
            } else {
                el.attrs.push((name, String::from(value)));
            }
        }
    }

    pub fn remove_attr(&mut self, id: usize, name: &str) {
        if let NodeData::Element(el) = &mut self.nodes[id].data {
            let name = name.to_ascii_lowercase();
            el.attrs.retain(|(n, _)| *n != name);
        }
    }

    /// Replace all children of `id` with a single text node.
    pub fn set_text_content(&mut self, id: usize, text: &str) {
        self.clear_children(id);
        if let NodeData::Text(t) = &mut self.nodes[id].data {
            *t = String::from(text);
            return;
        }
        let t = self.create_text(text);
        self.attach(id, t, None);
    }

    /// Deep-copy the subtree at `src` into a new orphan node.
    pub fn deep_copy(&mut self, src: usize) -> usize {
        let (data, children) = {
            let n = &self.nodes[src];
            let data = match &n.data {
                NodeData::Document => NodeData::Document,
                NodeData::Text(t) => NodeData::Text(t.clone()),
                NodeData::Element(el) => NodeData::Element(Element {
                    tag: el.tag,
                    attrs: el.attrs.clone(),
                }),
            };
            (data, n.children.clone())
        };
        let id = self.nodes.len();
        self.nodes.push(Node {
            parent: usize::MAX,
            children: Vec::new(),
            data,
        });
        for c in children {
            let cc = self.deep_copy(c);
            self.nodes[cc].parent = id;
            self.nodes[id].children.push(cc);
        }
        id
    }

    /// Parse `html` as a fragment and insert its body content under `parent`
    /// starting at child index `index`. Returns how many nodes were inserted.
    pub fn graft_fragment(&mut self, html: &str, parent: usize, index: usize) -> usize {
        let frag = build_dom(html);
        let Some(frag_body) = frag.find_first("body") else {
            return 0;
        };
        let roots = frag.nodes[frag_body].children.clone();
        let mut at = index.min(self.nodes[parent].children.len());
        let mut count = 0;
        for root in roots {
            let copy = copy_across(self, &frag, root);
            self.attach(parent, copy, Some(at));
            at += 1;
            count += 1;
        }
        count
    }
}

/// Copy a subtree from `src` into `dst` (different arenas), returning the new
/// orphan root id in `dst`. Depth is bounded by the builder's `MAX_DEPTH`.
fn copy_across(dst: &mut Dom, src: &Dom, src_id: usize) -> usize {
    let data = match &src.nodes[src_id].data {
        NodeData::Document => NodeData::Document,
        NodeData::Text(t) => NodeData::Text(t.clone()),
        NodeData::Element(el) => NodeData::Element(Element {
            tag: el.tag,
            attrs: el.attrs.clone(),
        }),
    };
    let id = dst.nodes.len();
    dst.nodes.push(Node {
        parent: usize::MAX,
        children: Vec::new(),
        data,
    });
    for &c in &src.nodes[src_id].children {
        let cc = copy_across(dst, src, c);
        dst.nodes[cc].parent = id;
        dst.nodes[id].children.push(cc);
    }
    id
}

/// Parse an HTML document into a DOM tree.
pub fn build_dom(input: &str) -> Dom {
    let mut tk = Tokenizer::new(input);
    let mut b = Builder::new();
    loop {
        match tk.next_token() {
            Token::Start {
                name,
                attrs,
                self_closing,
            } => b.start_tag(name, attrs, self_closing, &mut tk),
            Token::End(name) => b.end_tag(name, &mut tk),
            Token::Text(t) => b.text(t),
            Token::Eof => break,
        }
    }
    b.dom
}

// ────────────────────────────────────────────────────────────────────────────
// Tag classification
// ────────────────────────────────────────────────────────────────────────────

pub fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn is_formatting(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    )
}

/// The spec's "special" category (elements that close formatting implicitly
/// and terminate generic end-tag searches).
fn is_special(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "applet"
            | "area"
            | "article"
            | "aside"
            | "base"
            | "basefont"
            | "bgsound"
            | "blockquote"
            | "body"
            | "br"
            | "button"
            | "caption"
            | "center"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "embed"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "frame"
            | "frameset"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "iframe"
            | "img"
            | "input"
            | "keygen"
            | "li"
            | "link"
            | "listing"
            | "main"
            | "marquee"
            | "menu"
            | "meta"
            | "nav"
            | "noembed"
            | "noframes"
            | "noscript"
            | "object"
            | "ol"
            | "p"
            | "param"
            | "plaintext"
            | "pre"
            | "script"
            | "section"
            | "select"
            | "source"
            | "style"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "template"
            | "textarea"
            | "tfoot"
            | "th"
            | "thead"
            | "title"
            | "tr"
            | "track"
            | "ul"
            | "wbr"
            | "xmp"
    )
}

/// Start tags that implicitly close an open `<p>` (spec "in body" rules).
fn closes_p(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "center"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dd"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "li"
            | "listing"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "plaintext"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "ul"
            | "xmp"
    )
}

/// Elements that belong in `<head>` when seen before body content.
fn is_head_content(tag: &str) -> bool {
    matches!(
        tag,
        "base"
            | "basefont"
            | "bgsound"
            | "link"
            | "meta"
            | "noframes"
            | "script"
            | "style"
            | "template"
            | "title"
    )
}

/// HTML tags we recognise; unknown (custom) elements honour `/>`.
fn is_known_html(tag: &str) -> bool {
    is_special(tag)
        || is_formatting(tag)
        || matches!(
            tag,
            "abbr"
                | "acronym"
                | "audio"
                | "bdi"
                | "bdo"
                | "canvas"
                | "cite"
                | "data"
                | "datalist"
                | "del"
                | "dfn"
                | "ins"
                | "kbd"
                | "label"
                | "legend"
                | "map"
                | "mark"
                | "meter"
                | "optgroup"
                | "option"
                | "output"
                | "picture"
                | "progress"
                | "q"
                | "rb"
                | "rp"
                | "rt"
                | "rtc"
                | "ruby"
                | "samp"
                | "slot"
                | "span"
                | "sub"
                | "sup"
                | "time"
                | "var"
                | "video"
        )
}

/// Formatting-list entries that act as scope markers when inserted.
fn is_fmt_marker(tag: &str) -> bool {
    matches!(
        tag,
        "applet" | "caption" | "marquee" | "object" | "table" | "td" | "template" | "th"
    )
}

/// Default scope boundaries (spec "has an element in scope").
fn is_scope_boundary(tag: &str) -> bool {
    matches!(
        tag,
        "applet"
            | "caption"
            | "html"
            | "table"
            | "td"
            | "th"
            | "marquee"
            | "object"
            | "template"
            | "svg"
            | "math"
    )
}

fn implied_end(tag: &str) -> bool {
    matches!(
        tag,
        "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc"
    )
}

/// Tags allowed to nest directly inside a table context without being foster
/// parented out (the table-structure elements plus metadata content).
fn is_table_structural(tag: &str) -> bool {
    matches!(
        tag,
        "caption"
            | "colgroup"
            | "col"
            | "tbody"
            | "tfoot"
            | "thead"
            | "tr"
            | "td"
            | "th"
            | "table"
            | "style"
            | "script"
            | "template"
    )
}

// ────────────────────────────────────────────────────────────────────────────
// Builder
// ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum FmtEntry {
    Marker,
    El(usize),
}

struct Builder {
    dom: Dom,
    /// Open element stack (node ids); `html` sits at the bottom.
    open: Vec<usize>,
    /// Active formatting elements, with markers.
    fmt: Vec<FmtEntry>,
    html: Option<usize>,
    head: Option<usize>,
    body: Option<usize>,
}

impl Builder {
    fn new() -> Self {
        Self {
            dom: Dom {
                nodes: alloc::vec![Node {
                    parent: usize::MAX,
                    children: Vec::new(),
                    data: NodeData::Document,
                }],
            },
            open: Vec::new(),
            fmt: Vec::new(),
            html: None,
            head: None,
            body: None,
        }
    }

    // ── Node plumbing ───────────────────────────────────────────────────────

    fn new_node(&mut self, parent: usize, data: NodeData) -> usize {
        self.new_node_at(parent, None, data)
    }

    /// Append (or insert before `before` index) a fresh node under `parent`.
    fn new_node_at(&mut self, parent: usize, before: Option<usize>, data: NodeData) -> usize {
        let id = self.dom.nodes.len();
        self.dom.nodes.push(Node {
            parent,
            children: Vec::new(),
            data,
        });
        match before {
            Some(idx) if idx <= self.dom.nodes[parent].children.len() => {
                self.dom.nodes[parent].children.insert(idx, id)
            }
            _ => self.dom.nodes[parent].children.push(id),
        }
        id
    }

    fn current(&self) -> usize {
        self.open.last().copied().unwrap_or(DOCUMENT)
    }

    fn insert_element(&mut self, tag: SmallStr, attrs: Attrs) -> usize {
        // Foster parenting: non-table content inserted while a table context is
        // current is relocated to just before the table.
        if self.in_table_insert_context() && !is_table_structural(tag.as_str()) {
            if let Some((parent, idx)) = self.foster_target() {
                return self.new_node_at(
                    parent,
                    Some(idx),
                    NodeData::Element(Element { tag, attrs }),
                );
            }
        }
        let parent = self.current();
        self.new_node(parent, NodeData::Element(Element { tag, attrs }))
    }

    /// Whether the current node is a table context that triggers fostering.
    fn in_table_insert_context(&self) -> bool {
        matches!(
            self.tag_at(self.current()),
            "table" | "tbody" | "tfoot" | "thead" | "tr"
        )
    }

    /// Where fostered content goes: `(parent, index)` placing it immediately
    /// before the nearest open `<table>` in that table's parent. `None` when
    /// there is no suitable table (then the caller falls back to `current`).
    fn foster_target(&self) -> Option<(usize, usize)> {
        let table = self
            .open
            .iter()
            .rev()
            .copied()
            .find(|&n| self.tag_at(n) == "table")?;
        let parent = self.dom.nodes[table].parent;
        if parent == usize::MAX {
            return None;
        }
        let idx = self.dom.nodes[parent]
            .children
            .iter()
            .position(|&c| c == table)?;
        Some((parent, idx))
    }

    fn tag_at(&self, id: usize) -> &str {
        self.dom.tag(id)
    }

    // ── Implicit document structure ─────────────────────────────────────────

    fn ensure_html(&mut self) {
        if self.html.is_none() {
            let id = self.new_node(
                DOCUMENT,
                NodeData::Element(Element {
                    tag: SmallStr::lower(b"html"),
                    attrs: Vec::new(),
                }),
            );
            self.html = Some(id);
            self.open.insert(0, id);
        }
    }

    fn ensure_head_open(&mut self) {
        self.ensure_html();
        if self.head.is_none() {
            let html = self.html.unwrap();
            let id = self.new_node(
                html,
                NodeData::Element(Element {
                    tag: SmallStr::lower(b"head"),
                    attrs: Vec::new(),
                }),
            );
            self.head = Some(id);
            self.open.push(id);
        } else if !self.open.contains(&self.head.unwrap()) && self.body.is_none() {
            self.open.push(self.head.unwrap());
        }
    }

    fn ensure_body(&mut self) {
        self.ensure_html();
        if self.body.is_none() {
            // Close the head (and anything stuck in it).
            let html = self.html.unwrap();
            if let Some(pos) = self.open.iter().position(|&n| n == html) {
                self.open.truncate(pos + 1);
            }
            let id = self.new_node(
                html,
                NodeData::Element(Element {
                    tag: SmallStr::lower(b"body"),
                    attrs: Vec::new(),
                }),
            );
            self.body = Some(id);
            self.open.push(id);
        }
    }

    fn merge_attrs(&mut self, id: usize, attrs: Attrs) {
        if let NodeData::Element(el) = &mut self.dom.nodes[id].data {
            for (n, v) in attrs {
                if !el.attrs.iter().any(|(en, _)| *en == n) {
                    el.attrs.push((n, v));
                }
            }
        }
    }

    // ── Scope queries ───────────────────────────────────────────────────────

    /// Whether `name` is on the stack, not hidden behind a scope boundary.
    /// `extra` adds boundaries (button scope, list item scope).
    fn in_scope(&self, name: &str, extra: &[&str]) -> bool {
        for &id in self.open.iter().rev() {
            let tag = self.tag_at(id);
            if tag == name {
                return true;
            }
            if is_scope_boundary(tag) || extra.contains(&tag) {
                return false;
            }
        }
        false
    }

    fn element_in_scope(&self, el: usize) -> bool {
        for &id in self.open.iter().rev() {
            if id == el {
                return true;
            }
            if is_scope_boundary(self.tag_at(id)) {
                return false;
            }
        }
        false
    }

    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        while let Some(&top) = self.open.last() {
            let tag = self.tag_at(top);
            if implied_end(tag) && Some(tag) != except && Some(top) != self.html {
                self.open.pop();
            } else {
                break;
            }
        }
    }

    /// Pop the stack until an element named `name` (inclusive) is removed.
    fn pop_until(&mut self, name: &str) {
        while let Some(top) = self.open.pop() {
            if self.tag_at(top) == name {
                break;
            }
        }
    }

    fn close_p(&mut self) {
        self.generate_implied_end_tags(Some("p"));
        self.pop_until("p");
    }

    /// Close `name` if present within the given extra scope boundaries.
    fn close_in_scope(&mut self, name: &str, extra: &[&str]) {
        if self.in_scope(name, extra) {
            self.generate_implied_end_tags(Some(name));
            self.pop_until(name);
        }
    }

    // ── Active formatting elements ──────────────────────────────────────────

    fn fmt_push(&mut self, id: usize) {
        // Noah's Ark: at most 3 entries with the same tag since the last marker.
        let tag = SmallStr::lower(self.tag_at(id).as_bytes());
        let mut count = 0;
        let mut earliest = None;
        for (i, e) in self.fmt.iter().enumerate().rev() {
            match e {
                FmtEntry::Marker => break,
                FmtEntry::El(n) => {
                    if self.tag_at(*n) == tag.as_str() {
                        count += 1;
                        earliest = Some(i);
                    }
                }
            }
        }
        if count >= 3 {
            if let Some(i) = earliest {
                self.fmt.remove(i);
            }
        }
        self.fmt.push(FmtEntry::El(id));
    }

    /// Recreate formatting elements that were closed implicitly, so following
    /// content keeps their styling (spec "reconstruct the active formatting
    /// elements").
    fn reconstruct_fmt(&mut self) {
        if self.fmt.is_empty() {
            return;
        }
        let mut i = self.fmt.len();
        while i > 0 {
            match self.fmt[i - 1] {
                FmtEntry::Marker => break,
                FmtEntry::El(n) => {
                    if self.open.contains(&n) {
                        break;
                    }
                }
            }
            i -= 1;
        }
        for j in i..self.fmt.len() {
            if self.open.len() >= MAX_DEPTH {
                break;
            }
            if let FmtEntry::El(n) = self.fmt[j] {
                let (tag, attrs) = match self.dom.element(n) {
                    Some(el) => (el.tag, el.attrs.clone()),
                    None => continue,
                };
                let new = self.insert_element(tag, attrs);
                self.open.push(new);
                self.fmt[j] = FmtEntry::El(new);
            }
        }
    }

    /// Clear formatting entries back to (and including) the last marker.
    fn fmt_clear_to_marker(&mut self) {
        while let Some(e) = self.fmt.pop() {
            if matches!(e, FmtEntry::Marker) {
                break;
            }
        }
    }

    /// Simplified adoption agency for a formatting end tag. Elements above
    /// the formatting element are closed; non-formatting ("special") ones are
    /// reopened outside it so block structure survives, while formatting ones
    /// stay in the active list and reconstruct lazily.
    fn adoption(&mut self, name: &str) {
        let mut fmt_idx = None;
        for (i, e) in self.fmt.iter().enumerate().rev() {
            match e {
                FmtEntry::Marker => break,
                FmtEntry::El(n) => {
                    if self.tag_at(*n) == name {
                        fmt_idx = Some((i, *n));
                        break;
                    }
                }
            }
        }
        let Some((fi, el)) = fmt_idx else {
            // No active entry: treat as a generic end tag.
            self.close_in_scope(name, &[]);
            return;
        };
        let Some(sp) = self.open.iter().rposition(|&n| n == el) else {
            self.fmt.remove(fi);
            return;
        };
        if !self.element_in_scope(el) {
            return;
        }
        let above: Vec<usize> = self.open[sp + 1..].to_vec();
        self.open.truncate(sp);
        self.fmt.remove(fi);

        // Reopen popped non-formatting descendants as fresh elements outside
        // the closed formatting element.
        let mut reopened = 0;
        for n in above {
            let Some(elref) = self.dom.element(n) else {
                continue;
            };
            let tag = elref.tag;
            if !is_formatting(tag.as_str()) && reopened < 8 {
                let attrs = elref.attrs.clone();
                let new = self.insert_element(tag, attrs);
                self.open.push(new);
                reopened += 1;
            }
        }
    }

    // ── Token handlers ──────────────────────────────────────────────────────

    fn start_tag(&mut self, name: SmallStr, attrs: Attrs, self_closing: bool, tk: &mut Tokenizer) {
        let tag_owned = name;
        let tag = tag_owned.as_str();

        match tag {
            "html" => {
                self.ensure_html();
                self.merge_attrs(self.html.unwrap(), attrs);
                return;
            }
            "head" => {
                self.ensure_head_open();
                return;
            }
            "body" => {
                self.ensure_body();
                self.merge_attrs(self.body.unwrap(), attrs);
                return;
            }
            "frameset" | "frame" => return, // unsupported legacy framing
            _ => {}
        }

        if self.body.is_none() && is_head_content(tag) {
            self.ensure_head_open();
        } else {
            self.ensure_body();

            if closes_p(tag) && self.in_scope("p", &["button"]) {
                self.close_p();
            }
            match tag {
                "li" => self.close_in_scope("li", &["ol", "ul"]),
                "dd" | "dt" => {
                    self.close_in_scope("dd", &[]);
                    self.close_in_scope("dt", &[]);
                }
                "option" => self.pop_if_current("option"),
                "optgroup" => {
                    self.pop_if_current("option");
                    self.pop_if_current("optgroup");
                }
                "tr" => {
                    self.close_cell();
                    self.close_in_table_scope("tr");
                }
                "td" | "th" => self.close_cell(),
                "thead" | "tbody" | "tfoot" => {
                    self.close_cell();
                    self.close_in_table_scope("tr");
                    self.close_in_table_scope("thead");
                    self.close_in_table_scope("tbody");
                    self.close_in_table_scope("tfoot");
                }
                "caption" | "colgroup" | "col" => {
                    self.close_cell();
                    self.close_in_table_scope("caption");
                    self.close_in_table_scope("colgroup");
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    if matches!(
                        self.tag_at(self.current()),
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                    ) {
                        self.open.pop();
                    }
                }
                "button" => self.close_in_scope("button", &[]),
                "select" => self.close_in_scope("select", &[]),
                "a" => {
                    let has_a = self
                        .fmt
                        .iter()
                        .rev()
                        .take_while(|e| !matches!(e, FmtEntry::Marker))
                        .any(|e| matches!(e, FmtEntry::El(n) if self.tag_at(*n) == "a"));
                    if has_a {
                        self.adoption("a");
                    }
                }
                "nobr" => {
                    if self.in_scope("nobr", &[]) {
                        self.adoption("nobr");
                    }
                }
                _ => {}
            }

            if !is_special(tag) {
                self.reconstruct_fmt();
            }
        }

        let id = self.insert_element(tag_owned, attrs);
        let tag = tag_owned.as_str();

        if is_fmt_marker(tag) {
            self.fmt.push(FmtEntry::Marker);
        }

        if !is_void(tag) && self.open.len() < MAX_DEPTH {
            // The HTML syntax ignores `/>` on known elements; honour it only
            // for unknown (custom / foreign) ones.
            if !(self_closing && !is_known_html(tag)) {
                self.open.push(id);
            }
        }

        if is_formatting(tag) {
            self.fmt_push(id);
        }

        match tag {
            "script" => tk.set_mode(Mode::ScriptData, tag_owned),
            "style" | "xmp" | "iframe" | "noembed" | "noframes" => {
                tk.set_mode(Mode::Rawtext, tag_owned)
            }
            "title" | "textarea" => tk.set_mode(Mode::Rcdata, tag_owned),
            "plaintext" => tk.set_mode(Mode::Plaintext, tag_owned),
            _ => {}
        }
    }

    fn pop_if_current(&mut self, name: &str) {
        if self.tag_at(self.current()) == name {
            self.open.pop();
        }
    }

    /// Close `name` within table scope (boundaries: html, table only).
    fn close_in_table_scope(&mut self, name: &str) {
        for &id in self.open.iter().rev() {
            let tag = self.tag_at(id);
            if tag == name {
                self.pop_until(name);
                return;
            }
            if tag == "table" || tag == "html" || tag == "template" {
                return;
            }
        }
    }

    fn close_cell(&mut self) {
        self.close_in_table_scope("td");
        self.close_in_table_scope("th");
    }

    fn end_tag(&mut self, name: SmallStr, _tk: &mut Tokenizer) {
        let tag = name.as_str();
        match tag {
            "br" => {
                // Spec: `</br>` acts like `<br>`.
                self.ensure_body();
                self.insert_element(name, Vec::new());
            }
            "p" => {
                if self.in_scope("p", &["button"]) {
                    self.close_p();
                }
            }
            "body" | "html" => {} // keep accepting content
            "head" => {
                if let Some(head) = self.head {
                    if let Some(pos) = self.open.iter().position(|&n| n == head) {
                        self.open.truncate(pos);
                    }
                }
            }
            "li" => self.close_in_scope("li", &["ol", "ul"]),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                // Any open heading is closed by any heading end tag.
                let heading_open = ["h1", "h2", "h3", "h4", "h5", "h6"]
                    .iter()
                    .any(|h| self.in_scope(h, &[]));
                if heading_open {
                    self.generate_implied_end_tags(None);
                    while let Some(top) = self.open.pop() {
                        if matches!(self.tag_at(top), "h1" | "h2" | "h3" | "h4" | "h5" | "h6") {
                            break;
                        }
                    }
                }
            }
            t if is_formatting(t) => self.adoption(t),
            t if is_fmt_marker(t) => {
                if self.in_scope(t, &[]) {
                    self.generate_implied_end_tags(None);
                    self.pop_until(t);
                    self.fmt_clear_to_marker();
                }
            }
            _ => {
                if self.in_scope(tag, &[]) {
                    self.generate_implied_end_tags(Some(tag));
                    self.pop_until(tag);
                }
            }
        }
    }

    fn text(&mut self, t: String) {
        let only_ws = t
            .bytes()
            .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\x0C' | b'\r'));
        if only_ws {
            // Inter-element whitespace: keep it where it is unless nothing is
            // open yet (whitespace before <html> is dropped per spec).
            if self.open.is_empty() {
                return;
            }
        } else {
            if self.body.is_none() && !self.in_head_raw_element() {
                self.ensure_body();
            }
            self.reconstruct_fmt();
        }
        self.append_text(t);
    }

    /// Whether the current node lives in the head and receives raw text
    /// (`<title>`, `<style>`, `<script>` content before body starts).
    fn in_head_raw_element(&self) -> bool {
        matches!(
            self.tag_at(self.current()),
            "title" | "style" | "script" | "template" | "noframes"
        )
    }

    fn append_text(&mut self, t: String) {
        // Foster non-whitespace text out of a table context, merging with the
        // fostered text node immediately before the table when possible.
        if self.in_table_insert_context() && !t.trim().is_empty() {
            if let Some((parent, idx)) = self.foster_target() {
                if idx > 0 {
                    let prev = self.dom.nodes[parent].children[idx - 1];
                    if let NodeData::Text(existing) = &mut self.dom.nodes[prev].data {
                        existing.push_str(&t);
                        return;
                    }
                }
                self.new_node_at(parent, Some(idx), NodeData::Text(t));
                return;
            }
        }
        let target = self.current();
        if let Some(&last) = self.dom.nodes[target].children.last() {
            if let NodeData::Text(existing) = &mut self.dom.nodes[last].data {
                existing.push_str(&t);
                return;
            }
        }
        self.new_node(target, NodeData::Text(t));
    }
}
