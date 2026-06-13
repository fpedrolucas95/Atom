//! JavaScript lexer.
//!
//! Tokenizes a whole script up front (enabling the arbitrary lookahead the
//! parser needs for arrow functions and ASI). Handles line/block comments,
//! string escapes, template literals with `${…}` (inner expression source is
//! captured for the parser to re-lex), numeric literals (decimal, hex, octal,
//! binary, exponents), and regex literals — distinguished from division by
//! tracking whether the previous token can end an expression.
//!
//! Each token records whether a line terminator preceded it, which drives
//! automatic semicolon insertion in the parser.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

pub struct Token {
    pub kind: Tok,
    /// A newline appeared between the previous token and this one (ASI).
    pub nl_before: bool,
}

pub enum Tok {
    Num(f64),
    Str(Rc<str>),
    Template(Vec<RawTplPart>),
    Ident(String),
    Punct(&'static str),
    Regex(Rc<str>),
    Eof,
}

/// Template literal parts as raw source; `Expr` segments are re-lexed by the
/// parser.
pub enum RawTplPart {
    Str(String),
    Expr(String),
}

/// Longest-match punctuator table (longest first).
static PUNCTS: &[&str] = &[
    ">>>=", "===", "!==", "**=", "...", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%",
    "&", "|", "^", "!", "~", "?", ":", "=", ".",
];

pub fn lex(src: &str) -> Result<Vec<Token>, String> {
    let bytes = src.as_bytes();
    let mut toks: Vec<Token> = Vec::new();
    let mut i = 0;
    let mut nl = false;
    // Whether a `/` here would be a regex (true) or division (false).
    let mut regex_ok = true;

    while i < bytes.len() {
        let b = bytes[i];
        // Whitespace and comments.
        match b {
            b'\n' => {
                nl = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | b'\r' | 0x0C => {
                i += 1;
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut j = i + 2;
                while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                    if bytes[j] == b'\n' {
                        nl = true;
                    }
                    j += 1;
                }
                i = (j + 2).min(bytes.len());
                continue;
            }
            _ => {}
        }

        let kind = if b.is_ascii_digit()
            || (b == b'.' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit()))
        {
            lex_number(src, &mut i)?
        } else if b == b'"' || b == b'\'' {
            lex_string(src, &mut i)?
        } else if b == b'`' {
            lex_template(src, &mut i)?
        } else if b == b'/' && regex_ok {
            lex_regex(src, &mut i)?
        } else if is_ident_start(b) {
            let start = i;
            while i < bytes.len() && is_ident_part(bytes[i]) {
                i += 1;
            }
            Tok::Ident(String::from(&src[start..i]))
        } else if let Some(p) = PUNCTS.iter().find(|p| src[i..].starts_with(**p)) {
            i += p.len();
            Tok::Punct(p)
        } else {
            return Err(alloc::format!("unexpected character `{}`", b as char));
        };

        regex_ok = match &kind {
            Tok::Num(_) | Tok::Str(_) | Tok::Template(_) | Tok::Regex(_) => false,
            Tok::Ident(name) => matches!(
                name.as_str(),
                "return"
                    | "typeof"
                    | "instanceof"
                    | "in"
                    | "of"
                    | "new"
                    | "delete"
                    | "void"
                    | "throw"
                    | "case"
                    | "do"
                    | "else"
                    | "yield"
            ),
            Tok::Punct(p) => !matches!(*p, ")" | "]" | "++" | "--"),
            Tok::Eof => true,
        };
        toks.push(Token {
            kind,
            nl_before: nl,
        });
        nl = false;
    }
    toks.push(Token {
        kind: Tok::Eof,
        nl_before: nl,
    });
    Ok(toks)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_part(b: u8) -> bool {
    is_ident_start(b) || b.is_ascii_digit()
}

fn lex_number(src: &str, i: &mut usize) -> Result<Tok, String> {
    let bytes = src.as_bytes();
    let start = *i;
    if bytes[*i] == b'0' && *i + 1 < bytes.len() {
        let (radix, skip) = match bytes[*i + 1] | 0x20 {
            b'x' => (16, 2),
            b'o' => (8, 2),
            b'b' => (2, 2),
            _ => (10, 0),
        };
        if radix != 10 {
            *i += skip;
            let ds = *i;
            let mut v: u64 = 0;
            while *i < bytes.len() && (bytes[*i] as char).is_digit(radix) {
                v = v.wrapping_mul(radix as u64)
                    + (bytes[*i] as char).to_digit(radix).unwrap() as u64;
                *i += 1;
            }
            if *i == ds {
                return Err(String::from("invalid number literal"));
            }
            return Ok(Tok::Num(v as f64));
        }
    }
    while *i < bytes.len() && bytes[*i].is_ascii_digit() {
        *i += 1;
    }
    if bytes.get(*i) == Some(&b'.') {
        *i += 1;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
    }
    if matches!(bytes.get(*i), Some(b'e') | Some(b'E')) {
        let mut j = *i + 1;
        if matches!(bytes.get(j), Some(b'+') | Some(b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(|c| c.is_ascii_digit()) {
            *i = j;
            while *i < bytes.len() && bytes[*i].is_ascii_digit() {
                *i += 1;
            }
        }
    }
    src[start..*i]
        .parse::<f64>()
        .map(Tok::Num)
        .map_err(|_| String::from("invalid number literal"))
}

fn lex_string(src: &str, i: &mut usize) -> Result<Tok, String> {
    let bytes = src.as_bytes();
    let quote = bytes[*i];
    *i += 1;
    let mut out = String::new();
    while *i < bytes.len() {
        let b = bytes[*i];
        if b == quote {
            *i += 1;
            return Ok(Tok::Str(Rc::from(out.as_str())));
        }
        if b == b'\\' {
            *i += 1;
            push_escape(src, i, &mut out);
            continue;
        }
        if b == b'\n' {
            return Err(String::from("unterminated string"));
        }
        push_char(src, i, &mut out);
    }
    Err(String::from("unterminated string"))
}

/// Append the escape sequence starting at `*i` (just past the backslash).
fn push_escape(src: &str, i: &mut usize, out: &mut String) {
    let bytes = src.as_bytes();
    let Some(&e) = bytes.get(*i) else { return };
    *i += 1;
    match e {
        b'n' => out.push('\n'),
        b't' => out.push('\t'),
        b'r' => out.push('\r'),
        b'b' => out.push('\u{8}'),
        b'f' => out.push('\u{c}'),
        b'v' => out.push('\u{b}'),
        b'0' if !bytes.get(*i).is_some_and(|c| c.is_ascii_digit()) => out.push('\0'),
        b'\n' => {} // line continuation
        b'x' => {
            let cp = read_hex(bytes, i, 2);
            push_cp(out, cp);
        }
        b'u' => {
            if bytes.get(*i) == Some(&b'{') {
                *i += 1;
                let mut cp = 0u32;
                while *i < bytes.len() && bytes[*i] != b'}' {
                    cp = cp.wrapping_mul(16) + hex_digit(bytes[*i]);
                    *i += 1;
                }
                *i = (*i + 1).min(bytes.len());
                push_cp(out, cp);
            } else {
                let cp = read_hex(bytes, i, 4);
                push_cp(out, cp);
            }
        }
        _ => {
            *i -= 1;
            push_char(src, i, out);
        }
    }
}

fn read_hex(bytes: &[u8], i: &mut usize, n: usize) -> u32 {
    let mut cp = 0u32;
    for _ in 0..n {
        let Some(&b) = bytes.get(*i) else { break };
        if !b.is_ascii_hexdigit() {
            break;
        }
        cp = cp * 16 + hex_digit(b);
        *i += 1;
    }
    cp
}

fn hex_digit(b: u8) -> u32 {
    (b as char).to_digit(16).unwrap_or(0)
}

fn push_cp(out: &mut String, cp: u32) {
    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
}

/// Push one (possibly multi-byte) character, advancing past it.
fn push_char(src: &str, i: &mut usize, out: &mut String) {
    if let Some(c) = src[*i..].chars().next() {
        out.push(c);
        *i += c.len_utf8();
    } else {
        *i += 1;
    }
}

fn lex_template(src: &str, i: &mut usize) -> Result<Tok, String> {
    let bytes = src.as_bytes();
    *i += 1; // backtick
    let mut parts: Vec<RawTplPart> = Vec::new();
    let mut cur = String::new();
    while *i < bytes.len() {
        let b = bytes[*i];
        if b == b'`' {
            *i += 1;
            if !cur.is_empty() || parts.is_empty() {
                parts.push(RawTplPart::Str(cur));
            }
            return Ok(Tok::Template(parts));
        }
        if b == b'\\' {
            *i += 1;
            push_escape(src, i, &mut cur);
            continue;
        }
        if b == b'$' && bytes.get(*i + 1) == Some(&b'{') {
            parts.push(RawTplPart::Str(core::mem::take(&mut cur)));
            *i += 2;
            // Capture the expression source up to the matching brace,
            // string-aware so `${f("}")}` works.
            let start = *i;
            let mut depth = 1i32;
            let mut quote = 0u8;
            while *i < bytes.len() && depth > 0 {
                let c = bytes[*i];
                if quote != 0 {
                    if c == b'\\' {
                        *i += 1;
                    } else if c == quote {
                        quote = 0;
                    }
                } else {
                    match c {
                        b'"' | b'\'' | b'`' => quote = c,
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                }
                if depth > 0 {
                    *i += 1;
                }
            }
            parts.push(RawTplPart::Expr(String::from(&src[start..*i])));
            *i = (*i + 1).min(bytes.len()); // closing brace
            continue;
        }
        push_char(src, i, &mut cur);
    }
    Err(String::from("unterminated template literal"))
}

fn lex_regex(src: &str, i: &mut usize) -> Result<Tok, String> {
    let bytes = src.as_bytes();
    let start = *i;
    *i += 1; // opening slash
    let mut in_class = false;
    while *i < bytes.len() {
        match bytes[*i] {
            b'\\' => *i += 1,
            b'[' => in_class = true,
            b']' => in_class = false,
            b'/' if !in_class => {
                *i += 1;
                while *i < bytes.len() && bytes[*i].is_ascii_alphabetic() {
                    *i += 1; // flags
                }
                return Ok(Tok::Regex(Rc::from(&src[start..*i])));
            }
            b'\n' => return Err(String::from("unterminated regex literal")),
            _ => {}
        }
        *i += 1;
    }
    Err(String::from("unterminated regex literal"))
}
