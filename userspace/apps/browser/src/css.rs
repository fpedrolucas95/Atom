//! CSS engine: parsing, selector matching, and cascade support.
//!
//! Implements the parts of CSS that change what Atom's bitmap renderer can
//! draw, with real cascade semantics:
//!
//! * **Selectors** — type, `*`, `.class`, `#id`, attribute selectors with all
//!   match operators, the combinators (descendant, `>`, `+`, `~`), compound
//!   selectors, `:not()`, structural pseudo-classes (`:first-child`,
//!   `:last-child`, `:only-child`, `:nth-child(An+B|odd|even)`, `:root`,
//!   `:empty`), and `:link`/`:any-link`. Dynamic pseudo-classes (`:hover`, …)
//!   parse but never match. Invalid selectors are dropped individually.
//! * **Specificity** — (id, class, type) ordering plus source order, with
//!   separate `!important` accumulation.
//! * **At-rules** — `@media` conditions are evaluated against the viewport
//!   (width/height, orientation, type, `prefers-color-scheme`); `@supports`
//!   blocks are parsed; other at-rules are skipped with correct brace
//!   matching, string-aware.
//! * **Properties** — `color`, `background(-color)`, `font-weight`,
//!   `font-style`, `font-family` (monospace detection), the `font` shorthand,
//!   `text-decoration(-line)`, `display`, `visibility`, `text-align`, and
//!   `text-transform`.
//! * **Colors** — `#rgb[a]`/`#rrggbb[aa]`, `rgb()`/`rgba()` (comma and
//!   space/slash syntax, percentages), `hsl()`/`hsla()` (integer math — no
//!   FPU dependency), and the full CSS named-color table.

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;

use crate::dom::Align;
use crate::domtree::Dom;

/// Viewport used for media-query evaluation (browser window content area).
pub const VIEWPORT_W: u32 = 760;
pub const VIEWPORT_H: u32 = 520;

/// Cap on retained rules, bounding `O(elements × rules)` matching on
/// pathological sheets.
const MAX_RULES: usize = 512;

// ────────────────────────────────────────────────────────────────────────────
// Declarations
// ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    Uppercase,
    Lowercase,
    Capitalize,
}

/// A `font-size` value. Absolute sizes resolve immediately; relative ones
/// (`em`, `%`) are resolved against the parent's computed size in [`crate::style`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontSize {
    /// Absolute pixels.
    Px(u16),
    /// Percentage of the parent's size (`em` and `%`, stored ×1 as percent).
    Percent(u16),
}

/// A set of parsed declaration values. `None` means "not set here".
#[derive(Clone, Copy, Default)]
pub struct Decls {
    pub color: Option<Color>,
    pub background: Option<Color>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub mono: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub font_size: Option<FontSize>,
    /// `Some(true)` for `display: none`, `Some(false)` for any supported
    /// visible display value. The renderer has a single flow layout, but the
    /// option is needed so a later declaration can override `display: none`.
    pub display_none: Option<bool>,
    pub visible: Option<bool>,
    pub align: Option<Align>,
    pub transform: Option<Option<TextTransform>>,
}

impl Decls {
    /// Overlay `other`'s set fields on top of `self` (last wins).
    pub fn overlay(&mut self, other: &Decls) {
        macro_rules! take {
            ($f:ident) => {
                if other.$f.is_some() {
                    self.$f = other.$f;
                }
            };
        }
        take!(color);
        take!(background);
        take!(bold);
        take!(italic);
        take!(mono);
        take!(underline);
        take!(strike);
        take!(font_size);
        take!(display_none);
        take!(visible);
        take!(align);
        take!(transform);
    }
}

/// Parse a declaration list (`prop: value; …`) into normal and `!important`
/// declaration sets.
pub fn parse_declarations(input: &str) -> (Decls, Decls) {
    let mut normal = Decls::default();
    let mut important = Decls::default();
    for decl in split_top_level(input, ';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim();
        let mut value = value.trim();
        let mut is_important = false;
        if let Some(stripped) = strip_important(value) {
            value = stripped;
            is_important = true;
        }
        if value.is_empty() {
            continue;
        }
        let target = if is_important {
            &mut important
        } else {
            &mut normal
        };
        apply_declaration(target, prop, value);
    }
    (normal, important)
}

/// Strip a trailing `!important` (with optional internal whitespace).
fn strip_important(value: &str) -> Option<&str> {
    let v = value.trim_end();
    let lower_pos = v.len().checked_sub("important".len())?;
    if !v[lower_pos..].eq_ignore_ascii_case("important") {
        return None;
    }
    let before = v[..lower_pos].trim_end();
    let bang = before.strip_suffix('!')?;
    Some(bang.trim_end())
}

fn apply_declaration(d: &mut Decls, prop: &str, value: &str) {
    let lower = value.to_ascii_lowercase();
    let lw = lower.as_str();
    match_prop(prop, |p| match p {
        "color" => d.color = parse_color(value),
        "background-color" => {
            if lw != "transparent" && lw != "inherit" {
                d.background = parse_color(value);
            }
        }
        "background" => d.background = first_color_token(value),
        "font-weight" => d.bold = parse_font_weight(lw),
        "font-style" => {
            d.italic = Some(matches!(lw, "italic" | "oblique") || lw.starts_with("oblique"))
        }
        "font-family" => d.mono = Some(is_mono_family(lw)),
        "font-size" => {
            if let Some(fs) = parse_font_size(lw) {
                d.font_size = Some(fs);
            }
        }
        "font" => apply_font_shorthand(d, lw),
        "text-decoration" | "text-decoration-line" => {
            if lw.split_whitespace().any(|t| t == "none") {
                d.underline = Some(false);
                d.strike = Some(false);
            } else {
                if lw.split_whitespace().any(|t| t == "underline") {
                    d.underline = Some(true);
                }
                if lw.split_whitespace().any(|t| t == "line-through") {
                    d.strike = Some(true);
                }
            }
        }
        "display" => match lw {
            "none" => d.display_none = Some(true),
            "inherit" | "initial" | "unset" | "revert" | "revert-layer" => {}
            _ => d.display_none = Some(false),
        },
        "visibility" => match lw {
            "hidden" | "collapse" => d.visible = Some(false),
            "visible" => d.visible = Some(true),
            _ => {}
        },
        "text-align" => {
            d.align = match lw {
                "center" | "-webkit-center" | "-moz-center" => Some(Align::Center),
                "right" | "end" => Some(Align::Right),
                "left" | "start" | "justify" => Some(Align::Left),
                _ => None,
            }
        }
        "text-transform" => {
            d.transform = match lw {
                "uppercase" => Some(Some(TextTransform::Uppercase)),
                "lowercase" => Some(Some(TextTransform::Lowercase)),
                "capitalize" => Some(Some(TextTransform::Capitalize)),
                "none" => Some(None),
                _ => None,
            }
        }
        _ => {}
    });
}

/// Case-insensitive property dispatch without allocating for the common
/// already-lowercase case.
fn match_prop(prop: &str, f: impl FnOnce(&str)) {
    if prop.bytes().any(|b| b.is_ascii_uppercase()) {
        f(&prop.to_ascii_lowercase());
    } else {
        f(prop);
    }
}

fn parse_font_weight(lw: &str) -> Option<bool> {
    match lw {
        "bold" | "bolder" => Some(true),
        "normal" | "lighter" => Some(false),
        _ => lw.trim().parse::<u32>().ok().map(|n| n >= 600),
    }
}

fn is_mono_family(lw: &str) -> bool {
    [
        "monospace",
        "courier",
        "consolas",
        "menlo",
        "monaco",
        "mono",
    ]
    .iter()
    .any(|m| lw.contains(m))
}

/// `font: [style] [weight] size[/line-height] family…`
fn apply_font_shorthand(d: &mut Decls, lw: &str) {
    for tok in lw.split_whitespace() {
        match tok {
            "italic" | "oblique" => d.italic = Some(true),
            "bold" | "bolder" => d.bold = Some(true),
            _ => {
                // The size sits before the family; take the first token that
                // parses as a font-size (units/keyword required, so a weight
                // like `400` is not mistaken for a size). Strip any
                // `/line-height` suffix first.
                if d.font_size.is_none() {
                    let base = tok.split('/').next().unwrap_or(tok);
                    if let Some(fs) = parse_font_size(base) {
                        d.font_size = Some(fs);
                    }
                }
            }
        }
    }
    if is_mono_family(lw) {
        d.mono = Some(true);
    }
}

/// Parse a `font-size` value. Keywords map to pixels; `px`/`pt`/`rem` resolve
/// to absolute pixels; `em`/`%` stay relative (resolved against the parent).
/// Unitless values are rejected so the `font` shorthand cannot confuse a
/// numeric weight for a size.
fn parse_font_size(lw: &str) -> Option<FontSize> {
    match lw {
        "xx-small" => return Some(FontSize::Px(9)),
        "x-small" => return Some(FontSize::Px(10)),
        "small" => return Some(FontSize::Px(13)),
        "medium" => return Some(FontSize::Px(16)),
        "large" => return Some(FontSize::Px(18)),
        "x-large" => return Some(FontSize::Px(24)),
        "xx-large" => return Some(FontSize::Px(32)),
        "smaller" => return Some(FontSize::Percent(80)),
        "larger" => return Some(FontSize::Percent(125)),
        _ => {}
    }
    if let Some(n) = lw.strip_suffix('%') {
        return Some(FontSize::Percent(clamp_size(parse_num_x100(n)? / 100)));
    }
    if let Some(n) = lw.strip_suffix("px") {
        return Some(FontSize::Px(clamp_size((parse_num_x100(n)? + 50) / 100)));
    }
    if let Some(n) = lw.strip_suffix("pt") {
        // 1pt = 4/3 px.
        return Some(FontSize::Px(clamp_size(
            (parse_num_x100(n)? * 4 / 3 + 50) / 100,
        )));
    }
    if let Some(n) = lw.strip_suffix("rem") {
        // rem is relative to the 16px root font size.
        return Some(FontSize::Px(clamp_size(
            (parse_num_x100(n)? * 16 + 50) / 100,
        )));
    }
    if let Some(n) = lw.strip_suffix("em") {
        return Some(FontSize::Percent(clamp_size(parse_num_x100(n)?)));
    }
    None
}

fn clamp_size(v: u32) -> u16 {
    v.clamp(1, 4000) as u16
}

/// Parse a non-negative decimal with up to two fractional digits into a
/// hundredths-scaled integer: `"1.5"` → 150, `"2"` → 200, `".75"` → 75.
fn parse_num_x100(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (int_part, frac_part) = s.split_once('.').unwrap_or((s, ""));
    let int: u32 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac = 0u32;
    for (i, c) in frac_part.chars().take(2).enumerate() {
        let d = c.to_digit(10)?;
        frac += d * if i == 0 { 10 } else { 1 };
    }
    Some(int.checked_mul(100)?.checked_add(frac)?)
}

/// First parseable color in a shorthand value (skipping urls/gradients).
fn first_color_token(value: &str) -> Option<Color> {
    for tok in split_top_level_ws(value) {
        let lt = tok.to_ascii_lowercase();
        if lt.starts_with("url(") || lt.contains("gradient(") || lt == "transparent" {
            continue;
        }
        if let Some(c) = parse_color(tok) {
            return Some(c);
        }
    }
    None
}

// ────────────────────────────────────────────────────────────────────────────
// String-aware low-level scanning
// ────────────────────────────────────────────────────────────────────────────

/// Split on `sep`, ignoring separators inside `()`/`[]` and quoted strings.
fn split_top_level(input: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if quote != 0 {
            if b == quote {
                quote = 0;
            } else if b == b'\\' {
                // Skip escaped char (handled by the next iteration naturally).
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = b,
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ if b == sep as u8 && depth <= 0 => {
                out.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&input[start..]);
    out
}

/// Split on whitespace, keeping function arguments (`rgb(1, 2, 3)`) together.
fn split_top_level_ws(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' | b'\t' | b'\n' | b'\r' if depth <= 0 => {
                if let Some(s) = start.take() {
                    out.push(&input[s..i]);
                }
                continue;
            }
            _ => {}
        }
        if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(&input[s..]);
    }
    out
}

/// Strip `/* … */` comments, leaving quoted strings intact.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0;
    let mut quote = 0u8;
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            out.push(b as char);
            if b == quote {
                quote = 0;
            } else if b == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 1;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => {
                quote = b;
                out.push(b as char);
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => match css[i + 2..].find("*/") {
                Some(end) => i += 2 + end + 2,
                None => break,
            },
            _ => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    out
}

/// Find the matching `}` for the `{` at `open`, string-aware. Returns the
/// index of the closing brace, or the input length if unbalanced.
fn matching_brace(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut i = open;
    while i < bytes.len() {
        let b = bytes[i];
        if quote != 0 {
            if b == quote {
                quote = 0;
            } else if b == b'\\' {
                i += 1;
            }
        } else {
            match b {
                b'"' | b'\'' => quote = b,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return i;
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    bytes.len()
}

// ────────────────────────────────────────────────────────────────────────────
// Selectors
// ────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Comb {
    /// The rightmost (subject) compound.
    Subject,
    Descendant,
    Child,
    /// `+`
    Next,
    /// `~`
    Following,
}

enum Pseudo {
    Root,
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(i32, i32),
    NthLastChild(i32, i32),
    Empty,
    Link,
    Not(Compound),
    /// Valid but dynamically false (`:hover`, `:focus`, …).
    Never,
}

enum AttrOp {
    Exists,
    Eq,
    Includes,
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

struct AttrSel {
    name: String,
    op: AttrOp,
    value: String,
    case_insensitive: bool,
}

#[derive(Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrSel>,
    pseudos: Vec<Pseudo>,
}

pub struct Selector {
    /// Packed specificity: ids × 1_000_000 + (classes+attrs+pseudos) × 1_000
    /// + types.
    spec: u32,
    /// Subject compound first, then leftward compounds with their combinator.
    parts: Vec<(Comb, Compound)>,
}

impl Compound {
    fn specificity(&self) -> u32 {
        let ids = self.id.is_some() as u32;
        let classes = (self.classes.len() + self.attrs.len()) as u32
            + self
                .pseudos
                .iter()
                .filter(|p| !matches!(p, Pseudo::Not(_)))
                .count() as u32
            + self
                .pseudos
                .iter()
                .map(|p| match p {
                    Pseudo::Not(inner) => inner.specificity() / 1_000 % 1_000,
                    _ => 0,
                })
                .sum::<u32>();
        let types = self.tag.as_deref().map_or(0, |t| (t != "*") as u32);
        ids * 1_000_000 + classes.min(999) * 1_000 + types
    }

    fn matches(&self, dom: &Dom, el: usize) -> bool {
        let Some(element) = dom.element(el) else {
            return false;
        };
        if let Some(tag) = &self.tag {
            if tag != "*" && element.tag() != tag.as_str() {
                return false;
            }
        }
        if let Some(id) = &self.id {
            if element.attr("id") != Some(id.as_str()) {
                return false;
            }
        }
        if !self.classes.is_empty() {
            let class_attr = element.attr("class").unwrap_or("");
            for c in &self.classes {
                if !class_attr.split_ascii_whitespace().any(|x| x == c) {
                    return false;
                }
            }
        }
        for a in &self.attrs {
            if !a.matches(element.attr(&a.name)) {
                return false;
            }
        }
        for p in &self.pseudos {
            if !p.matches(dom, el) {
                return false;
            }
        }
        true
    }
}

impl AttrSel {
    fn matches(&self, actual: Option<&str>) -> bool {
        let Some(actual) = actual else {
            return false;
        };
        let (actual_buf, value_buf);
        let (actual, value) = if self.case_insensitive {
            actual_buf = actual.to_ascii_lowercase();
            value_buf = self.value.to_ascii_lowercase();
            (actual_buf.as_str(), value_buf.as_str())
        } else {
            (actual, self.value.as_str())
        };
        match self.op {
            AttrOp::Exists => true,
            AttrOp::Eq => actual == value,
            AttrOp::Includes => actual.split_ascii_whitespace().any(|t| t == value),
            AttrOp::DashMatch => {
                actual == value
                    || (actual.len() > value.len()
                        && actual.starts_with(value)
                        && actual.as_bytes()[value.len()] == b'-')
            }
            AttrOp::Prefix => !value.is_empty() && actual.starts_with(value),
            AttrOp::Suffix => !value.is_empty() && actual.ends_with(value),
            AttrOp::Substring => !value.is_empty() && actual.contains(value),
        }
    }
}

impl Pseudo {
    fn matches(&self, dom: &Dom, el: usize) -> bool {
        match self {
            Pseudo::Root => dom.tag(el) == "html",
            Pseudo::FirstChild => element_sibling_index(dom, el).0 == 0,
            Pseudo::LastChild => {
                let (idx, total) = element_sibling_index(dom, el);
                idx + 1 == total
            }
            Pseudo::OnlyChild => element_sibling_index(dom, el).1 == 1,
            Pseudo::NthChild(a, b) => nth_matches(*a, *b, element_sibling_index(dom, el).0 + 1),
            Pseudo::NthLastChild(a, b) => {
                let (idx, total) = element_sibling_index(dom, el);
                nth_matches(*a, *b, total - idx)
            }
            Pseudo::Empty => dom.nodes[el].children.is_empty(),
            Pseudo::Link => {
                matches!(dom.tag(el), "a" | "area")
                    && dom.element(el).is_some_and(|e| e.attr("href").is_some())
            }
            Pseudo::Not(inner) => !inner.matches(dom, el),
            Pseudo::Never => false,
        }
    }
}

/// `(index, count)` of `el` among its parent's *element* children (0-based).
fn element_sibling_index(dom: &Dom, el: usize) -> (i32, i32) {
    let parent = dom.nodes[el].parent;
    if parent == usize::MAX {
        return (0, 1);
    }
    let mut idx = 0;
    let mut total = 0;
    for &c in &dom.nodes[parent].children {
        if dom.element(c).is_some() {
            if c == el {
                idx = total;
            }
            total += 1;
        }
    }
    (idx, total.max(1))
}

/// Does `an+b` produce `idx` for some n ≥ 0?
fn nth_matches(a: i32, b: i32, idx: i32) -> bool {
    if a == 0 {
        return idx == b;
    }
    let diff = idx - b;
    diff % a == 0 && diff / a >= 0
}

impl Selector {
    pub fn matches(&self, dom: &Dom, el: usize) -> bool {
        self.parts.first().is_some_and(|(_, c)| c.matches(dom, el)) && self.matches_from(dom, el, 0)
    }

    /// `parts[idx]` matched at `node`; try to satisfy the rest leftward,
    /// with backtracking for the indefinite combinators.
    fn matches_from(&self, dom: &Dom, node: usize, idx: usize) -> bool {
        let Some((comb, comp)) = self.parts.get(idx + 1) else {
            return true;
        };
        match comb {
            Comb::Subject => true, // unreachable by construction
            Comb::Descendant => {
                let mut cur = dom.nodes[node].parent;
                while cur != usize::MAX {
                    if comp.matches(dom, cur) && self.matches_from(dom, cur, idx + 1) {
                        return true;
                    }
                    cur = dom.nodes[cur].parent;
                }
                false
            }
            Comb::Child => {
                let parent = dom.nodes[node].parent;
                parent != usize::MAX
                    && comp.matches(dom, parent)
                    && self.matches_from(dom, parent, idx + 1)
            }
            Comb::Next => {
                let Some(prev) = prev_element_sibling(dom, node) else {
                    return false;
                };
                comp.matches(dom, prev) && self.matches_from(dom, prev, idx + 1)
            }
            Comb::Following => {
                let mut cur = prev_element_sibling(dom, node);
                while let Some(p) = cur {
                    if comp.matches(dom, p) && self.matches_from(dom, p, idx + 1) {
                        return true;
                    }
                    cur = prev_element_sibling(dom, p);
                }
                false
            }
        }
    }
}

fn prev_element_sibling(dom: &Dom, el: usize) -> Option<usize> {
    let parent = dom.nodes[el].parent;
    if parent == usize::MAX {
        return None;
    }
    let siblings = &dom.nodes[parent].children;
    let pos = siblings.iter().position(|&c| c == el)?;
    siblings[..pos]
        .iter()
        .rev()
        .copied()
        .find(|&c| dom.element(c).is_some())
}

// ── Selector parsing ─────────────────────────────────────────────────────────

/// Parse a selector list, dropping individually invalid selectors.
pub fn parse_selector_list(input: &str) -> Vec<Selector> {
    split_top_level(input, ',')
        .into_iter()
        .filter_map(parse_complex_selector)
        .collect()
}

fn parse_complex_selector(input: &str) -> Option<Selector> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    // Tokenize into compounds, each carrying the combinator *before* it.
    let bytes = input.as_bytes();
    let mut compounds: Vec<(Comb, Compound)> = Vec::new();
    let mut pending = Comb::Subject;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' | b'\n' | b'\r' => {
                if pending == Comb::Subject && !compounds.is_empty() {
                    pending = Comb::Descendant;
                }
                i += 1;
            }
            b'>' => {
                pending = Comb::Child;
                i += 1;
            }
            b'+' => {
                pending = Comb::Next;
                i += 1;
            }
            b'~' => {
                pending = Comb::Following;
                i += 1;
            }
            _ => {
                let start = i;
                let mut depth = 0i32;
                while i < bytes.len() {
                    let b = bytes[i];
                    match b {
                        b'(' | b'[' => depth += 1,
                        b')' | b']' => depth -= 1,
                        b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'+' | b'~' if depth <= 0 => break,
                        _ => {}
                    }
                    i += 1;
                }
                if compounds.is_empty() && pending != Comb::Subject {
                    return None; // leading combinator
                }
                let comp = parse_compound(&input[start..i])?;
                compounds.push((pending, comp));
                pending = Comb::Subject;
            }
        }
    }
    if compounds.is_empty() || pending != Comb::Subject {
        return None; // empty, or trailing combinator
    }

    let spec = compounds
        .iter()
        .map(|(_, c)| c.specificity())
        .fold(0u32, |a, s| a.saturating_add(s));

    // Reverse into matching order: parts[0] is the subject compound; each
    // later part carries the combinator that joined its *right* neighbour
    // (which is the combinator the matcher must walk to reach it).
    let mut parts: Vec<(Comb, Compound)> = Vec::with_capacity(compounds.len());
    let mut carry = Comb::Subject;
    while let Some((comb_before, comp)) = compounds.pop() {
        parts.push((carry, comp));
        carry = comb_before;
    }

    Some(Selector { spec, parts })
}

fn parse_compound(input: &str) -> Option<Compound> {
    let bytes = input.as_bytes();
    let mut comp = Compound::default();
    let mut i = 0;

    // Optional leading type or universal selector.
    if i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric()
            || bytes[i] == b'*'
            || bytes[i] == b'-'
            || bytes[i] == b'_')
    {
        if bytes[i] == b'*' {
            comp.tag = Some(String::from("*"));
            i += 1;
        } else {
            let start = i;
            while i < bytes.len() && is_ident_byte(bytes[i]) {
                i += 1;
            }
            comp.tag = Some(input[start..i].to_ascii_lowercase());
        }
    }

    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let name = read_ident(input, &mut i)?;
                comp.classes.push(name);
            }
            b'#' => {
                i += 1;
                let name = read_ident(input, &mut i)?;
                comp.id = Some(name);
            }
            b'[' => {
                let close = input[i..].find(']')? + i;
                let attr = parse_attr_selector(&input[i + 1..close])?;
                comp.attrs.push(attr);
                i = close + 1;
            }
            b':' => {
                i += 1;
                if bytes.get(i) == Some(&b':') {
                    return None; // pseudo-elements (::before…) unsupported
                }
                let name = read_ident(input, &mut i)?;
                let arg = if bytes.get(i) == Some(&b'(') {
                    let close = matching_paren(bytes, i)?;
                    let a = &input[i + 1..close];
                    i = close + 1;
                    Some(a)
                } else {
                    None
                };
                comp.pseudos.push(parse_pseudo(&name, arg)?);
            }
            _ => return None,
        }
    }
    if comp.tag.is_none()
        && comp.id.is_none()
        && comp.classes.is_empty()
        && comp.attrs.is_empty()
        && comp.pseudos.is_empty()
    {
        return None;
    }
    Some(comp)
}

fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (off, &b) in bytes[open..].iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + off);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_pseudo(name: &str, arg: Option<&str>) -> Option<Pseudo> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "root" => Pseudo::Root,
        "first-child" => Pseudo::FirstChild,
        "last-child" => Pseudo::LastChild,
        "only-child" => Pseudo::OnlyChild,
        "empty" => Pseudo::Empty,
        "link" | "any-link" => Pseudo::Link,
        "nth-child" => {
            let (a, b) = parse_nth(arg?)?;
            Pseudo::NthChild(a, b)
        }
        "nth-last-child" => {
            let (a, b) = parse_nth(arg?)?;
            Pseudo::NthLastChild(a, b)
        }
        "not" => Pseudo::Not(parse_compound(arg?.trim())?),
        // Dynamic states the browser doesn't track: valid, never match.
        "hover" | "active" | "focus" | "visited" | "focus-within" | "focus-visible" | "target"
        | "checked" | "disabled" | "enabled" | "required" | "optional" | "placeholder-shown"
        | "first-of-type" | "last-of-type" | "only-of-type" | "nth-of-type"
        | "nth-last-of-type" => Pseudo::Never,
        _ => return None,
    })
}

/// Parse the `An+B` micro-syntax (plus `odd` / `even`).
fn parse_nth(input: &str) -> Option<(i32, i32)> {
    let s: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let s = s.to_ascii_lowercase();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(n_pos) = s.find('n') {
        let a_str = &s[..n_pos];
        let a = match a_str {
            "" | "+" => 1,
            "-" => -1,
            _ => a_str.parse().ok()?,
        };
        let b_str = &s[n_pos + 1..];
        let b = if b_str.is_empty() {
            0
        } else {
            b_str.strip_prefix('+').unwrap_or(b_str).parse().ok()?
        };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

fn parse_attr_selector(input: &str) -> Option<AttrSel> {
    let mut body = input.trim();
    let mut case_insensitive = false;
    // Trailing case flag: [attr=value i]
    if body.len() >= 2 {
        let b = body.as_bytes();
        let last = b[b.len() - 1].to_ascii_lowercase();
        if (last == b'i' || last == b's') && b[b.len() - 2].is_ascii_whitespace() {
            case_insensitive = last == b'i';
            body = body[..body.len() - 1].trim_end();
        }
    }
    let ops: [(&str, AttrOp); 6] = [
        ("~=", AttrOp::Includes),
        ("|=", AttrOp::DashMatch),
        ("^=", AttrOp::Prefix),
        ("$=", AttrOp::Suffix),
        ("*=", AttrOp::Substring),
        ("=", AttrOp::Eq),
    ];
    for (sym, op) in ops {
        if let Some(pos) = body.find(sym) {
            let name = body[..pos].trim().to_ascii_lowercase();
            let mut value = body[pos + sym.len()..].trim();
            if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                value = &value[1..value.len() - 1];
            }
            if name.is_empty() {
                return None;
            }
            return Some(AttrSel {
                name,
                op,
                value: String::from(value),
                case_insensitive,
            });
        }
    }
    let name = body.trim().to_ascii_lowercase();
    if name.is_empty() || !name.bytes().all(is_ident_byte) {
        return None;
    }
    Some(AttrSel {
        name,
        op: AttrOp::Exists,
        value: String::new(),
        case_insensitive,
    })
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn read_ident(input: &str, i: &mut usize) -> Option<String> {
    let bytes = input.as_bytes();
    let start = *i;
    while *i < bytes.len() && is_ident_byte(bytes[*i]) {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    Some(String::from(&input[start..*i]))
}

// ────────────────────────────────────────────────────────────────────────────
// Stylesheet
// ────────────────────────────────────────────────────────────────────────────

struct Rule {
    selectors: Vec<Selector>,
    normal: Decls,
    important: Decls,
}

pub struct Stylesheet {
    rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Parse a `<style>` body, appending its rules to this sheet.
    pub fn parse_into(&mut self, css: &str) {
        let clean = strip_comments(css);
        self.parse_rules(&clean);
    }

    fn parse_rules(&mut self, css: &str) {
        let bytes = css.as_bytes();
        let mut i = 0;
        while i < bytes.len() && self.rules.len() < MAX_RULES {
            // Skip whitespace and stray semicolons.
            while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b';') {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b'@' {
                i = self.parse_at_rule(css, i);
                continue;
            }
            // Qualified rule: prelude { body }
            let Some(brace_rel) = css[i..].find('{') else {
                break;
            };
            let brace = i + brace_rel;
            let close = matching_brace(bytes, brace);
            let prelude = &css[i..brace];
            let body = &css[brace + 1..close.min(css.len())];
            i = (close + 1).min(bytes.len());

            let selectors = parse_selector_list(prelude);
            if selectors.is_empty() {
                continue;
            }
            let (normal, important) = parse_declarations(body);
            self.rules.push(Rule {
                selectors,
                normal,
                important,
            });
        }
    }

    /// Parse one at-rule starting at the `@`; returns the resume index.
    fn parse_at_rule(&mut self, css: &str, at: usize) -> usize {
        let bytes = css.as_bytes();
        let mut i = at + 1;
        while i < bytes.len() && is_ident_byte(bytes[i]) {
            i += 1;
        }
        let name = css[at + 1..i].to_ascii_lowercase();

        // Find whichever comes first: a `;` (statement at-rule) or `{`.
        let mut j = i;
        let mut quote = 0u8;
        while j < bytes.len() {
            let b = bytes[j];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else {
                match b {
                    b'"' | b'\'' => quote = b,
                    b';' => return j + 1, // @import / @charset / @namespace
                    b'{' => break,
                    _ => {}
                }
            }
            j += 1;
        }
        if j >= bytes.len() {
            return bytes.len();
        }
        let close = matching_brace(bytes, j);
        let prelude = &css[i..j];
        let inner = &css[j + 1..close.min(css.len())];
        match name.as_str() {
            "media" => {
                if media_matches(prelude) {
                    self.parse_rules(inner);
                }
            }
            // Progressive-enhancement blocks: parse the contents; unknown
            // properties inside are ignored anyway.
            "supports" | "layer" | "scope" => self.parse_rules(inner),
            _ => {} // @font-face, @keyframes, @page, …
        }
        (close + 1).min(bytes.len())
    }

    /// Accumulate the declarations of every rule matching `el`, in cascade
    /// order (specificity, then source order), into the two layers.
    pub fn match_element(&self, dom: &Dom, el: usize, normal: &mut Decls, important: &mut Decls) {
        let mut matched: Vec<(u32, u32, &Rule)> = Vec::new();
        for (order, rule) in self.rules.iter().enumerate() {
            let mut best: Option<u32> = None;
            for sel in &rule.selectors {
                if best.map_or(true, |b| sel.spec > b) && sel.matches(dom, el) {
                    best = Some(best.map_or(sel.spec, |b| b.max(sel.spec)));
                }
            }
            if let Some(spec) = best {
                matched.push((spec, order as u32, rule));
            }
        }
        matched.sort_unstable_by_key(|&(spec, order, _)| (spec, order));
        for (_, _, rule) in matched {
            normal.overlay(&rule.normal);
            important.overlay(&rule.important);
        }
    }
}

// ── Media queries ────────────────────────────────────────────────────────────

/// Evaluate a media-query list against Atom's fixed viewport.
fn media_matches(prelude: &str) -> bool {
    let lower = prelude.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return true;
    }
    split_top_level(&lower, ',')
        .into_iter()
        .any(media_clause_matches)
}

fn media_clause_matches(clause: &str) -> bool {
    let mut negate = false;
    let mut result = true;
    let mut rest = clause.trim();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("not ") {
            negate = !negate;
            rest = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix("only ") {
            rest = r.trim_start();
            continue;
        }
        if let Some(r) = rest.strip_prefix("and ") {
            rest = r.trim_start();
            continue;
        }
        if rest.starts_with('(') {
            let Some(close) = matching_paren(rest.as_bytes(), 0) else {
                return false;
            };
            result &= media_feature_matches(&rest[1..close]);
            rest = rest[close + 1..].trim_start();
            continue;
        }
        // Media type keyword.
        let word: &str = rest.split([' ', '\t']).next().unwrap_or(rest);
        match word {
            "all" | "screen" => {}
            "" => break,
            _ => result = false, // print, speech, tty, …
        }
        rest = rest[word.len()..].trim_start();
    }
    result != negate
}

fn media_feature_matches(inner: &str) -> bool {
    let inner = inner.trim();
    let Some((name, value)) = inner.split_once(':') else {
        // Boolean form: (width), (color), …
        return matches!(inner, "width" | "height" | "color" | "orientation");
    };
    let name = name.trim();
    let value = value.trim();
    match name {
        "min-width" => parse_css_length(value).is_some_and(|v| VIEWPORT_W >= v),
        "max-width" => parse_css_length(value).is_some_and(|v| VIEWPORT_W <= v),
        "width" => parse_css_length(value) == Some(VIEWPORT_W),
        "min-height" => parse_css_length(value).is_some_and(|v| VIEWPORT_H >= v),
        "max-height" => parse_css_length(value).is_some_and(|v| VIEWPORT_H <= v),
        "height" => parse_css_length(value) == Some(VIEWPORT_H),
        "orientation" => value == "landscape",
        "prefers-color-scheme" => value == "dark",
        "prefers-reduced-motion" => true,
        _ => false,
    }
}

/// Parse a CSS length into pixels (supports px, em, rem; 1em = 16px).
fn parse_css_length(value: &str) -> Option<u32> {
    let v = value.trim();
    let (num, scale) = if let Some(n) = v.strip_suffix("px") {
        (n, 1)
    } else if let Some(n) = v.strip_suffix("rem").or_else(|| v.strip_suffix("em")) {
        (n, 16)
    } else {
        (v, 1)
    };
    let num = num.trim();
    // Accept simple decimals by truncating.
    let whole = num.split('.').next()?;
    whole.parse::<u32>().ok().map(|n| n * scale)
}

// ────────────────────────────────────────────────────────────────────────────
// Colors
// ────────────────────────────────────────────────────────────────────────────

pub fn parse_color(value: &str) -> Option<Color> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    let lower = v.to_ascii_lowercase();
    if let Some(inner) = strip_func(&lower, "rgb").or_else(|| strip_func(&lower, "rgba")) {
        return parse_rgb_args(inner);
    }
    if let Some(inner) = strip_func(&lower, "hsl").or_else(|| strip_func(&lower, "hsla")) {
        return parse_hsl_args(inner);
    }
    named_color(&lower)
}

fn strip_func<'a>(v: &'a str, name: &str) -> Option<&'a str> {
    v.strip_prefix(name)
        .and_then(|s| s.trim_start().strip_prefix('('))
        .and_then(|s| s.trim_end().strip_suffix(')'))
}

/// Split function arguments on commas or whitespace, dropping a `/ alpha`
/// component (modern syntax).
fn color_args(inner: &str) -> Vec<&str> {
    let main = inner.split('/').next().unwrap_or(inner);
    main.split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_rgb_args(inner: &str) -> Option<Color> {
    let args = color_args(inner);
    if args.len() < 3 {
        return None;
    }
    // 4th comma argument is alpha; alpha 0 → treat as unset (invisible).
    if let Some(alpha) = args.get(3) {
        if parse_alpha_is_zero(alpha) {
            return None;
        }
    }
    let r = parse_color_component(args[0])?;
    let g = parse_color_component(args[1])?;
    let b = parse_color_component(args[2])?;
    Some(Color::rgb(r, g, b))
}

fn parse_color_component(s: &str) -> Option<u8> {
    if let Some(p) = s.strip_suffix('%') {
        let v: u32 = p.split('.').next()?.parse().ok()?;
        return Some((v.min(100) * 255 / 100) as u8);
    }
    let v: u32 = s.split('.').next()?.parse().ok()?;
    Some(v.min(255) as u8)
}

fn parse_alpha_is_zero(s: &str) -> bool {
    matches!(s.trim(), "0" | "0.0" | ".0" | "0%" | "0.00")
}

fn parse_hsl_args(inner: &str) -> Option<Color> {
    let args = color_args(inner);
    if args.len() < 3 {
        return None;
    }
    let h: i32 = args[0]
        .trim_end_matches("deg")
        .split('.')
        .next()?
        .parse()
        .ok()?;
    let s: i32 = args[1].strip_suffix('%')?.split('.').next()?.parse().ok()?;
    let l: i32 = args[2].strip_suffix('%')?.split('.').next()?.parse().ok()?;
    Some(hsl_to_rgb(
        h.rem_euclid(360),
        s.clamp(0, 100),
        l.clamp(0, 100),
    ))
}

/// Integer HSL → RGB (values scaled by 100 internally; no floating point).
fn hsl_to_rgb(h: i32, s: i32, l: i32) -> Color {
    let c = (100 - (2 * l - 100).abs()) * s / 100; // chroma, 0..100
    let x = c * (60 - ((h % 120) - 60).abs()) / 60;
    let m = l - c / 2;
    let (r, g, b) = match h / 60 {
        0 => (c, x, 0),
        1 => (x, c, 0),
        2 => (0, c, x),
        3 => (0, x, c),
        4 => (x, 0, c),
        _ => (c, 0, x),
    };
    let to8 = |v: i32| (((v + m).clamp(0, 100)) * 255 / 100) as u8;
    Color::rgb(to8(r), to8(g), to8(b))
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let h = hex.trim();
    let d = |i: usize| -> Option<u8> {
        let c = *h.as_bytes().get(i)?;
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    match h.len() {
        3 | 4 => {
            if h.len() == 4 && d(3)? == 0 {
                return None; // fully transparent
            }
            Some(Color::rgb(d(0)? * 17, d(1)? * 17, d(2)? * 17))
        }
        6 | 8 => {
            if h.len() == 8 && (d(6)? * 16 + d(7)?) == 0 {
                return None;
            }
            Some(Color::rgb(
                d(0)? * 16 + d(1)?,
                d(2)? * 16 + d(3)?,
                d(4)? * 16 + d(5)?,
            ))
        }
        _ => None,
    }
}

fn named_color(lower: &str) -> Option<Color> {
    let (r, g, b) = match lower {
        "aliceblue" => (240, 248, 255),
        "antiquewhite" => (250, 235, 215),
        "aqua" | "cyan" => (0, 255, 255),
        "aquamarine" => (127, 255, 212),
        "azure" => (240, 255, 255),
        "beige" => (245, 245, 220),
        "bisque" => (255, 228, 196),
        "black" => (0, 0, 0),
        "blanchedalmond" => (255, 235, 205),
        "blue" => (0, 0, 255),
        "blueviolet" => (138, 43, 226),
        "brown" => (165, 42, 42),
        "burlywood" => (222, 184, 135),
        "cadetblue" => (95, 158, 160),
        "chartreuse" => (127, 255, 0),
        "chocolate" => (210, 105, 30),
        "coral" => (255, 127, 80),
        "cornflowerblue" => (100, 149, 237),
        "cornsilk" => (255, 248, 220),
        "crimson" => (220, 20, 60),
        "darkblue" => (0, 0, 139),
        "darkcyan" => (0, 139, 139),
        "darkgoldenrod" => (184, 134, 11),
        "darkgray" | "darkgrey" => (169, 169, 169),
        "darkgreen" => (0, 100, 0),
        "darkkhaki" => (189, 183, 107),
        "darkmagenta" => (139, 0, 139),
        "darkolivegreen" => (85, 107, 47),
        "darkorange" => (255, 140, 0),
        "darkorchid" => (153, 50, 204),
        "darkred" => (139, 0, 0),
        "darksalmon" => (233, 150, 122),
        "darkseagreen" => (143, 188, 143),
        "darkslateblue" => (72, 61, 139),
        "darkslategray" | "darkslategrey" => (47, 79, 79),
        "darkturquoise" => (0, 206, 209),
        "darkviolet" => (148, 0, 211),
        "deeppink" => (255, 20, 147),
        "deepskyblue" => (0, 191, 255),
        "dimgray" | "dimgrey" => (105, 105, 105),
        "dodgerblue" => (30, 144, 255),
        "firebrick" => (178, 34, 34),
        "floralwhite" => (255, 250, 240),
        "forestgreen" => (34, 139, 34),
        "fuchsia" | "magenta" => (255, 0, 255),
        "gainsboro" => (220, 220, 220),
        "ghostwhite" => (248, 248, 255),
        "gold" => (255, 215, 0),
        "goldenrod" => (218, 165, 32),
        "gray" | "grey" => (128, 128, 128),
        "green" => (0, 128, 0),
        "greenyellow" => (173, 255, 47),
        "honeydew" => (240, 255, 240),
        "hotpink" => (255, 105, 180),
        "indianred" => (205, 92, 92),
        "indigo" => (75, 0, 130),
        "ivory" => (255, 255, 240),
        "khaki" => (240, 230, 140),
        "lavender" => (230, 230, 250),
        "lavenderblush" => (255, 240, 245),
        "lawngreen" => (124, 252, 0),
        "lemonchiffon" => (255, 250, 205),
        "lightblue" => (173, 216, 230),
        "lightcoral" => (240, 128, 128),
        "lightcyan" => (224, 255, 255),
        "lightgoldenrodyellow" => (250, 250, 210),
        "lightgray" | "lightgrey" => (211, 211, 211),
        "lightgreen" => (144, 238, 144),
        "lightpink" => (255, 182, 193),
        "lightsalmon" => (255, 160, 122),
        "lightseagreen" => (32, 178, 170),
        "lightskyblue" => (135, 206, 250),
        "lightslategray" | "lightslategrey" => (119, 136, 153),
        "lightsteelblue" => (176, 196, 222),
        "lightyellow" => (255, 255, 224),
        "lime" => (0, 255, 0),
        "limegreen" => (50, 205, 50),
        "linen" => (250, 240, 230),
        "maroon" => (128, 0, 0),
        "mediumaquamarine" => (102, 205, 170),
        "mediumblue" => (0, 0, 205),
        "mediumorchid" => (186, 85, 211),
        "mediumpurple" => (147, 112, 219),
        "mediumseagreen" => (60, 179, 113),
        "mediumslateblue" => (123, 104, 238),
        "mediumspringgreen" => (0, 250, 154),
        "mediumturquoise" => (72, 209, 204),
        "mediumvioletred" => (199, 21, 133),
        "midnightblue" => (25, 25, 112),
        "mintcream" => (245, 255, 250),
        "mistyrose" => (255, 228, 225),
        "moccasin" => (255, 228, 181),
        "navajowhite" => (255, 222, 173),
        "navy" => (0, 0, 128),
        "oldlace" => (253, 245, 230),
        "olive" => (128, 128, 0),
        "olivedrab" => (107, 142, 35),
        "orange" => (255, 165, 0),
        "orangered" => (255, 69, 0),
        "orchid" => (218, 112, 214),
        "palegoldenrod" => (238, 232, 170),
        "palegreen" => (152, 251, 152),
        "paleturquoise" => (175, 238, 238),
        "palevioletred" => (219, 112, 147),
        "papayawhip" => (255, 239, 213),
        "peachpuff" => (255, 218, 185),
        "peru" => (205, 133, 63),
        "pink" => (255, 192, 203),
        "plum" => (221, 160, 221),
        "powderblue" => (176, 224, 230),
        "purple" => (128, 0, 128),
        "rebeccapurple" => (102, 51, 153),
        "red" => (255, 0, 0),
        "rosybrown" => (188, 143, 143),
        "royalblue" => (65, 105, 225),
        "saddlebrown" => (139, 69, 19),
        "salmon" => (250, 128, 114),
        "sandybrown" => (244, 164, 96),
        "seagreen" => (46, 139, 87),
        "seashell" => (255, 245, 238),
        "sienna" => (160, 82, 45),
        "silver" => (192, 192, 192),
        "skyblue" => (135, 206, 235),
        "slateblue" => (106, 90, 205),
        "slategray" | "slategrey" => (112, 128, 144),
        "snow" => (255, 250, 250),
        "springgreen" => (0, 255, 127),
        "steelblue" => (70, 130, 180),
        "tan" => (210, 180, 140),
        "teal" => (0, 128, 128),
        "thistle" => (216, 191, 216),
        "tomato" => (255, 99, 71),
        "turquoise" => (64, 224, 208),
        "violet" => (238, 130, 238),
        "wheat" => (245, 222, 179),
        "white" => (255, 255, 255),
        "whitesmoke" => (245, 245, 245),
        "yellow" => (255, 255, 0),
        "yellowgreen" => (154, 205, 50),
        _ => return None,
    };
    Some(Color::rgb(r, g, b))
}
