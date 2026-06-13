//! Text utilities: case-insensitive ASCII matching, percent/base64 codecs,
//! and a zero-allocation small-string for tag names.
//!
//! Centralising these keeps the parser, URL, and network layers DRY — every
//! module shares one implementation of each primitive. HTML entity handling
//! lives in [`crate::entities`].

use alloc::string::String;
use alloc::vec::Vec;

// ────────────────────────────────────────────────────────────────────────────
// ASCII helpers (no allocation, case-insensitive)
// ────────────────────────────────────────────────────────────────────────────

pub fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
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
/// Tag names are short and parsed on every token of markup, so avoiding a heap
/// allocation per token is a measurable parse-time win (focus: low CPU).
/// Names longer than the capacity are truncated — they never match a known
/// HTML tag anyway.
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
// Display helpers
// ────────────────────────────────────────────────────────────────────────────

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
