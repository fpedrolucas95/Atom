//! A pragmatic CSS subset.
//!
//! Supports the declarations that meaningfully change a text rendering on
//! Atom's bitmap display: `color`, `font-weight`, `text-decoration`, and
//! `display`/`visibility` hiding. Both inline `style="..."` attributes and
//! `<style>` rule blocks (tag, `.class`, and `*` selectors, comma-grouped) are
//! understood. This is deliberately not a full cascade — there is no
//! specificity ordering beyond source order — but it covers the common cases
//! real pages use for emphasis and theming.

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;

/// A resolved set of style overrides. `None` fields mean "inherit / unset".
#[derive(Clone, Copy, Default)]
pub struct Style {
    pub color: Option<Color>,
    pub bold: Option<bool>,
    pub underline: Option<bool>,
    pub hidden: bool,
}

impl Style {
    /// Overlay `other`'s set fields on top of `self` (last-wins cascade).
    pub fn overlay(&mut self, other: &Style) {
        if other.color.is_some() {
            self.color = other.color;
        }
        if other.bold.is_some() {
            self.bold = other.bold;
        }
        if other.underline.is_some() {
            self.underline = other.underline;
        }
        self.hidden |= other.hidden;
    }
}

/// Parse an inline `style="..."` declaration list into a [`Style`].
pub fn parse_inline(decls: &str) -> Style {
    let mut style = Style::default();
    for decl in decls.split(';') {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        apply_declaration(&mut style, prop.trim(), value.trim());
    }
    style
}

fn apply_declaration(style: &mut Style, prop: &str, value: &str) {
    // Property names are case-insensitive; keywords likewise.
    let prop = AsciiLower(prop);
    let lower = value.to_ascii_lowercase();
    match prop {
        _ if prop.eq("color") => style.color = parse_color(value),
        _ if prop.eq("font-weight") => {
            style.bold = Some(matches!(
                lower.as_str(),
                "bold" | "bolder" | "600" | "700" | "800" | "900"
            ))
        }
        _ if prop.eq("text-decoration") || prop.eq("text-decoration-line") => {
            style.underline = Some(lower.contains("underline"))
        }
        _ if prop.eq("display") && lower == "none" => style.hidden = true,
        _ if prop.eq("visibility") && (lower == "hidden" || lower == "collapse") => {
            style.hidden = true
        }
        _ => {}
    }
}

/// A parsed `<style>` block: an ordered list of selector → declaration rules.
pub struct Stylesheet {
    rules: Vec<Rule>,
}

struct Rule {
    selectors: Vec<Selector>,
    style: Style,
}

enum Selector {
    Universal,
    Tag(String),
    Class(String),
}

/// Cap on retained rules. `style_for` is consulted for every styled element,
/// so an unbounded sheet from a heavy page would make parsing O(elements ×
/// rules); this keeps the common cases working while bounding the worst case.
const MAX_RULES: usize = 256;

impl Stylesheet {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Parse a `<style>` body, appending its rules to this sheet.
    pub fn parse_into(&mut self, css: &str) {
        let cleaned = strip_comments(css);
        let mut rest = cleaned.as_str();
        while let Some((head, tail)) = rest.split_once('{') {
            let Some((body, after)) = tail.split_once('}') else {
                break;
            };
            rest = after;
            if self.rules.len() >= MAX_RULES {
                break;
            }
            let selectors = parse_selectors(head);
            if selectors.is_empty() {
                continue;
            }
            self.rules.push(Rule {
                selectors,
                style: parse_inline(body),
            });
        }
    }

    /// Resolve the cascaded style for an element of `tag` with the given
    /// space-separated `class` attribute.
    pub fn style_for(&self, tag: &str, classes: &str) -> Style {
        let mut style = Style::default();
        for rule in &self.rules {
            if rule.selectors.iter().any(|s| s.matches(tag, classes)) {
                style.overlay(&rule.style);
            }
        }
        style
    }
}

impl Selector {
    fn matches(&self, tag: &str, classes: &str) -> bool {
        match self {
            Selector::Universal => true,
            Selector::Tag(name) => name.eq_ignore_ascii_case(tag),
            Selector::Class(name) => classes.split_whitespace().any(|c| c == name),
        }
    }
}

fn parse_selectors(head: &str) -> Vec<Selector> {
    head.split(',')
        .filter_map(|raw| {
            let sel = raw.trim();
            if sel.is_empty() {
                return None;
            }
            // Keep only the rightmost simple selector of any descendant chain.
            let simple = sel.rsplit([' ', '>', '+', '~']).next().unwrap_or(sel).trim();
            match simple.strip_prefix('.') {
                Some(class) if !class.is_empty() => Some(Selector::Class(String::from(class))),
                _ if simple == "*" => Some(Selector::Universal),
                _ if simple.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') => {
                    Some(Selector::Tag(String::from(simple)))
                }
                _ => None,
            }
        })
        .collect()
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let bytes = css.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            match css[i + 2..].find("*/") {
                Some(end) => i += 2 + end + 2,
                None => break,
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

// ────────────────────────────────────────────────────────────────────────────
// Colour parsing: #rgb, #rrggbb, rgb()/rgba(), and named colours.
// ────────────────────────────────────────────────────────────────────────────

pub fn parse_color(value: &str) -> Option<Color> {
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    if let Some(inner) = v
        .strip_prefix("rgb(")
        .or_else(|| v.strip_prefix("rgba("))
        .and_then(|s| s.strip_suffix(')'))
    {
        let mut it = inner.split(',').map(|c| c.trim().parse::<u32>().ok());
        let r = it.next()??;
        let g = it.next()??;
        let b = it.next()??;
        return Some(Color::rgb(clamp_u8(r), clamp_u8(g), clamp_u8(b)));
    }
    named_color(v)
}

fn parse_hex_color(hex: &str) -> Option<Color> {
    let h = hex.trim();
    let parse = |s: &str| u8::from_str_radix(s, 16).ok();
    match h.len() {
        3 => {
            let r = parse(&h[0..1])?;
            let g = parse(&h[1..2])?;
            let b = parse(&h[2..3])?;
            Some(Color::rgb(r * 17, g * 17, b * 17))
        }
        6 => Some(Color::rgb(parse(&h[0..2])?, parse(&h[2..4])?, parse(&h[4..6])?)),
        _ => None,
    }
}

fn clamp_u8(v: u32) -> u8 {
    v.min(255) as u8
}

fn named_color(name: &str) -> Option<Color> {
    const NAMED: &[(&str, (u8, u8, u8))] = &[
        ("black", (0, 0, 0)),
        ("white", (255, 255, 255)),
        ("red", (255, 0, 0)),
        ("green", (0, 128, 0)),
        ("lime", (0, 255, 0)),
        ("blue", (0, 0, 255)),
        ("navy", (0, 0, 128)),
        ("yellow", (255, 255, 0)),
        ("orange", (255, 165, 0)),
        ("purple", (128, 0, 128)),
        ("fuchsia", (255, 0, 255)),
        ("magenta", (255, 0, 255)),
        ("aqua", (0, 255, 255)),
        ("cyan", (0, 255, 255)),
        ("teal", (0, 128, 128)),
        ("olive", (128, 128, 0)),
        ("maroon", (128, 0, 0)),
        ("silver", (192, 192, 192)),
        ("gray", (128, 128, 128)),
        ("grey", (128, 128, 128)),
        ("darkgray", (64, 64, 64)),
        ("lightgray", (211, 211, 211)),
        ("pink", (255, 192, 203)),
        ("brown", (165, 42, 42)),
        ("gold", (255, 215, 0)),
        ("transparent", (0, 0, 0)),
    ];
    let lower = name.to_ascii_lowercase();
    NAMED
        .iter()
        .find(|(n, _)| *n == lower)
        .map(|(_, (r, g, b))| Color::rgb(*r, *g, *b))
}

/// Tiny case-insensitive comparison wrapper to keep declaration matching tidy.
struct AsciiLower<'a>(&'a str);

impl AsciiLower<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0.eq_ignore_ascii_case(other)
    }
}
