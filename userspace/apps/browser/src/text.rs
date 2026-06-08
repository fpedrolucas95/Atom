//! Text utilities: case-insensitive ASCII matching, percent/base64 codecs,
//! HTML entity resolution, and a zero-allocation small-string for tag names.
//!
//! Centralising these keeps the parser, URL, and network layers DRY — every
//! module shares one implementation of each primitive.

use alloc::string::String;
use alloc::vec::Vec;

use crate::render::CHAR_W;

// ────────────────────────────────────────────────────────────────────────────
// ASCII helpers (no allocation, case-insensitive)
// ────────────────────────────────────────────────────────────────────────────

pub fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.eq_ignore_ascii_case(y))
}

pub fn starts_with_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && eq_ignore_ascii_case(&haystack[..needle.len()], needle)
}

pub fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| eq_ignore_ascii_case(w, needle))
}

// ────────────────────────────────────────────────────────────────────────────
// SmallStr — inline lowercase buffer for tag / attribute names
// ────────────────────────────────────────────────────────────────────────────

const SMALL_STR_CAP: usize = 16;

/// A fixed-capacity ASCII-lowercased string kept entirely on the stack.
///
/// Tag and attribute names are short and parsed on every byte of markup, so
/// avoiding a heap allocation per token is a measurable parse-time win (focus:
/// low CPU). Names longer than the capacity are truncated — they never match a
/// known HTML tag anyway.
#[derive(Clone, Copy)]
pub struct SmallStr {
    buf: [u8; SMALL_STR_CAP],
    len: usize,
}

impl SmallStr {
    pub fn lower(bytes: &[u8]) -> Self {
        let len = bytes.len().min(SMALL_STR_CAP);
        let mut buf = [0u8; SMALL_STR_CAP];
        for (dst, src) in buf.iter_mut().zip(&bytes[..len]) {
            *dst = src.to_ascii_lowercase();
        }
        Self { buf, len }
    }

    pub fn as_str(&self) -> &str {
        // Safety: populated only from ASCII-lowercased bytes.
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Compare against an already-lowercase literal.
    pub fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Percent-encoding (RFC 3986, form style)
// ────────────────────────────────────────────────────────────────────────────

pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0xF));
            }
        }
    }
    out
}

pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex_digit(v: u8) -> char {
    if v < 10 {
        (b'0' + v) as char
    } else {
        (b'A' + v - 10) as char
    }
}

pub fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Base64 (standard alphabet, used by data: URIs)
// ────────────────────────────────────────────────────────────────────────────

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        match c {
            b'=' => break,
            b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => {}
        }
        buf = (buf << 6) | val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ────────────────────────────────────────────────────────────────────────────
// HTML entities (named + numeric), resolved to the renderer's ASCII font
// ────────────────────────────────────────────────────────────────────────────

/// Resolve a named entity (without `&`/`;`) to its ASCII rendering.
///
/// The bitmap font is ASCII-only, so typographic characters are mapped to the
/// closest ASCII equivalent (e.g. `&mdash;` → `--`) rather than emitting raw
/// UTF-8 the renderer cannot draw.
pub fn named_entity(name: &[u8]) -> Option<&'static str> {
    let s = match name {
        b"amp" => "&",
        b"lt" => "<",
        b"gt" => ">",
        b"quot" => "\"",
        b"apos" => "'",
        b"nbsp" => " ",
        b"copy" => "(c)",
        b"reg" => "(R)",
        b"trade" => "(TM)",
        b"hellip" => "...",
        b"mdash" => "--",
        b"ndash" => "-",
        b"minus" => "-",
        b"middot" | b"bull" => "*",
        b"lsquo" | b"rsquo" | b"sbquo" => "'",
        b"ldquo" | b"rdquo" | b"bdquo" => "\"",
        b"laquo" => "<<",
        b"raquo" => ">>",
        b"times" => "x",
        b"divide" => "/",
        b"deg" => "deg",
        b"plusmn" => "+/-",
        b"frac12" => "1/2",
        b"frac14" => "1/4",
        b"frac34" => "3/4",
        b"cent" => "c",
        b"pound" => "GBP",
        b"euro" => "EUR",
        b"yen" => "JPY",
        b"sect" => "S",
        b"para" => "P",
        b"dagger" => "+",
        b"larr" => "<-",
        b"rarr" => "->",
        b"harr" => "<->",
        b"uarr" => "^",
        b"darr" => "v",
        b"check" => "v",
        b"emsp" | b"ensp" | b"thinsp" => " ",
        _ => return None,
    };
    Some(s)
}

/// Decode all entities in a string eagerly (used for attribute values, where
/// streaming whitespace collapsing is not required).
pub fn decode_entities(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some(semi) = bytes[i + 1..].iter().take(12).position(|&b| b == b';') {
                let entity = &bytes[i + 1..i + 1 + semi];
                if let Some(rep) = resolve_entity(entity) {
                    out.push_str(rep);
                    i += semi + 2;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Resolve an entity body (between `&` and `;`) to its ASCII rendering,
/// handling both named and numeric (`#123` / `#x1F`) forms. Returns an owned
/// `String` because numeric forms may map to multi-byte ASCII.
pub fn resolve_entity(entity: &[u8]) -> Option<&'static str> {
    if let Some(rest) = entity.strip_prefix(b"#") {
        let cp = if let Some(hex) = rest.strip_prefix(b"x").or_else(|| rest.strip_prefix(b"X")) {
            parse_radix(hex, 16)?
        } else {
            parse_radix(rest, 10)?
        };
        return Some(codepoint_to_ascii(cp));
    }
    named_entity(entity)
}

fn parse_radix(digits: &[u8], radix: u32) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    let mut value: u32 = 0;
    for &b in digits {
        let d = (b as char).to_digit(radix)?;
        value = value.checked_mul(radix)?.checked_add(d)?;
    }
    Some(value)
}

/// Map a Unicode codepoint to an ASCII rendering. ASCII passes through; common
/// punctuation is approximated; anything else becomes `?`.
pub fn codepoint_to_ascii(cp: u32) -> &'static str {
    if cp < 0x80 {
        // Single ASCII byte: return a 1-char static slice via a lookup table.
        return ASCII_TABLE[cp as usize];
    }
    match cp {
        0x00A0 => " ",
        0x2013 => "-",
        0x2014 => "--",
        0x2018 | 0x2019 => "'",
        0x201C | 0x201D => "\"",
        0x2026 => "...",
        0x2022 => "*",
        0x00A9 => "(c)",
        0x00AE => "(R)",
        0x2122 => "(TM)",
        0x00D7 => "x",
        0x2192 => "->",
        0x2190 => "<-",
        _ => "?",
    }
}

/// Static one-byte string slices for every ASCII codepoint, so
/// [`codepoint_to_ascii`] can return `&'static str` without allocating.
static ASCII_TABLE: [&str; 128] = build_ascii_table();

const fn build_ascii_table() -> [&'static str; 128] {
    // Each entry is a distinct 1-byte static string literal.
    const T: [&str; 128] = [
        "\u{0}", "\u{1}", "\u{2}", "\u{3}", "\u{4}", "\u{5}", "\u{6}", "\u{7}", "\u{8}", "\t",
        "\n", "\u{b}", "\u{c}", "\r", "\u{e}", "\u{f}", "\u{10}", "\u{11}", "\u{12}", "\u{13}",
        "\u{14}", "\u{15}", "\u{16}", "\u{17}", "\u{18}", "\u{19}", "\u{1a}", "\u{1b}", "\u{1c}",
        "\u{1d}", "\u{1e}", "\u{1f}", " ", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", "+",
        ",", "-", ".", "/", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", ":", ";", "<", "=",
        ">", "?", "@", "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O",
        "P", "Q", "R", "S", "T", "U", "V", "W", "X", "Y", "Z", "[", "\\", "]", "^", "_", "`", "a",
        "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
        "t", "u", "v", "w", "x", "y", "z", "{", "|", "}", "~", "\u{7f}",
    ];
    T
}

// ────────────────────────────────────────────────────────────────────────────
// Display helpers
// ────────────────────────────────────────────────────────────────────────────

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

/// Escape text for safe interpolation into the browser's own error pages.
pub fn escape_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}
