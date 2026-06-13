//! HTML5 tokenizer.
//!
//! Follows the WHATWG tokenization algorithm's observable behaviour: start/end
//! tags with fully parsed attributes (quoted, unquoted, boolean, duplicate
//! dropping), character references in data and attribute values, comments
//! (including the abrupt `<!-->` forms and `--!>` closing), bogus comments,
//! DOCTYPE skipping, CDATA sections, and the special content models — RCDATA
//! (`<title>`, `<textarea>`), RAWTEXT (`<style>`, `<xmp>`, …), PLAINTEXT, and
//! script data with the `<!--` escaping states so `</script>` inside a
//! commented-out script does not end it prematurely.
//!
//! Input is treated as UTF-8; non-ASCII codepoints are transliterated to
//! ASCII approximations the bitmap font can draw.

use alloc::string::String;
use alloc::vec::Vec;

use crate::entities::{parse_char_ref, transliterate};
use crate::text::SmallStr;

/// A parsed tag's attributes: lowercase name → decoded value.
pub type Attrs = Vec<(String, String)>;

pub enum Token {
    Start {
        name: SmallStr,
        attrs: Attrs,
        self_closing: bool,
    },
    End(SmallStr),
    Text(String),
    Eof,
}

/// Content model the tree builder switches the tokenizer into after seeing
/// certain start tags (spec: "tokenizer state switching").
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Data,
    /// Raw text until the matching end tag; no entities (style, xmp, …).
    Rawtext,
    /// Like rawtext but with entity decoding (title, textarea).
    Rcdata,
    /// Script data, honouring `<!--` escaped regions.
    ScriptData,
    /// Everything to EOF is text.
    Plaintext,
}

pub struct Tokenizer<'a> {
    bytes: &'a [u8],
    pos: usize,
    mode: Mode,
    /// The element name whose end tag terminates a raw/rcdata region.
    raw_tag: SmallStr,
    /// End-tag token queued after a raw-text region's text was emitted.
    pending_end: Option<SmallStr>,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self {
            bytes: input.as_bytes(),
            pos: 0,
            mode: Mode::Data,
            raw_tag: SmallStr::lower(b""),
            pending_end: None,
        }
    }

    /// Switch content model. Called by the tree builder right after it
    /// processes a start tag for a raw-text element.
    pub fn set_mode(&mut self, mode: Mode, tag: SmallStr) {
        self.mode = mode;
        self.raw_tag = tag;
    }

    pub fn next_token(&mut self) -> Token {
        if let Some(name) = self.pending_end.take() {
            self.mode = Mode::Data;
            return Token::End(name);
        }
        match self.mode {
            Mode::Data => self.next_data(),
            Mode::Rawtext | Mode::Rcdata | Mode::ScriptData => self.next_raw(),
            Mode::Plaintext => self.next_plaintext(),
        }
    }

    // ── Data state ──────────────────────────────────────────────────────────

    fn next_data(&mut self) -> Token {
        let mut text = String::new();
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b'<' => {
                    match self.classify_tag_open() {
                        TagOpen::Tag => {
                            if !text.is_empty() {
                                return Token::Text(text); // re-enter at '<'
                            }
                            if let Some(tok) = self.parse_tag() {
                                return tok;
                            }
                            // `</>` or EOF-in-tag: nothing to emit, continue.
                        }
                        TagOpen::Markup => self.consume_markup_declaration(),
                        TagOpen::Bogus => self.consume_bogus_comment(),
                        TagOpen::Literal => {
                            self.pos += 1;
                            text.push('<');
                        }
                    }
                }
                b'&' => {
                    self.pos += 1;
                    match parse_char_ref(&self.bytes[self.pos..], false) {
                        Some(cr) => {
                            self.pos += cr.consumed;
                            text.push_str(cr.text);
                        }
                        None => text.push('&'),
                    }
                }
                b if b < 0x80 => {
                    self.pos += 1;
                    if b != b'\r' {
                        text.push(b as char);
                    } else {
                        // Normalise CRLF / CR to LF (spec preprocessing).
                        text.push('\n');
                        if self.bytes.get(self.pos) == Some(&b'\n') {
                            self.pos += 1;
                        }
                    }
                }
                _ => {
                    let cp = self.decode_utf8();
                    text.push_str(transliterate(cp));
                }
            }
        }
        if !text.is_empty() {
            Token::Text(text)
        } else {
            Token::Eof
        }
    }

    /// Decode one (possibly multi-byte) UTF-8 sequence at `pos`.
    fn decode_utf8(&mut self) -> u32 {
        let b0 = self.bytes[self.pos] as u32;
        let (len, init) = match b0 {
            0xC0..=0xDF => (2, b0 & 0x1F),
            0xE0..=0xEF => (3, b0 & 0x0F),
            0xF0..=0xF7 => (4, b0 & 0x07),
            _ => {
                self.pos += 1;
                return 0xFFFD;
            }
        };
        if self.pos + len > self.bytes.len() {
            self.pos = self.bytes.len();
            return 0xFFFD;
        }
        let mut cp = init;
        for i in 1..len {
            let b = self.bytes[self.pos + i] as u32;
            if b & 0xC0 != 0x80 {
                self.pos += 1;
                return 0xFFFD;
            }
            cp = (cp << 6) | (b & 0x3F);
        }
        self.pos += len;
        cp
    }

    // ── Tag-open classification ─────────────────────────────────────────────

    fn classify_tag_open(&self) -> TagOpen {
        match self.bytes.get(self.pos + 1) {
            Some(b) if b.is_ascii_alphabetic() => TagOpen::Tag,
            Some(b'/') => match self.bytes.get(self.pos + 2) {
                Some(b) if b.is_ascii_alphabetic() => TagOpen::Tag,
                Some(b'>') => TagOpen::Tag, // `</>` consumed silently
                Some(_) => TagOpen::Bogus,
                None => TagOpen::Literal,
            },
            Some(b'!') => TagOpen::Markup,
            Some(b'?') => TagOpen::Bogus,
            _ => TagOpen::Literal,
        }
    }

    // ── Tag parsing ─────────────────────────────────────────────────────────

    /// Parse `<name attrs…>` or `</name …>`. `pos` is at the `<`.
    fn parse_tag(&mut self) -> Option<Token> {
        let closing = self.bytes.get(self.pos + 1) == Some(&b'/');
        self.pos += if closing { 2 } else { 1 };

        if closing && self.bytes.get(self.pos) == Some(&b'>') {
            self.pos += 1; // `</>`: spec drops it entirely
            return None;
        }

        // Tag name: until whitespace, '/', or '>'.
        let name_start = self.pos;
        while self.pos < self.bytes.len()
            && !matches!(
                self.bytes[self.pos],
                b'\t' | b'\n' | b'\x0C' | b' ' | b'/' | b'>'
            )
        {
            self.pos += 1;
        }
        let name = SmallStr::lower(&self.bytes[name_start..self.pos]);

        let (attrs, self_closing, eof) = self.parse_attrs(!closing);
        if eof {
            return None; // spec: EOF in tag emits nothing
        }
        Some(if closing {
            Token::End(name)
        } else {
            Token::Start {
                name,
                attrs,
                self_closing,
            }
        })
    }

    /// Parse the attribute list. For end tags (`keep == false`) attributes are
    /// parsed for correct `>` detection but discarded.
    fn parse_attrs(&mut self, keep: bool) -> (Attrs, bool, bool) {
        let mut attrs: Attrs = Vec::new();
        loop {
            self.skip_ws();
            match self.bytes.get(self.pos) {
                None => return (attrs, false, true),
                Some(b'>') => {
                    self.pos += 1;
                    return (attrs, false, false);
                }
                Some(b'/') => {
                    self.pos += 1;
                    if self.bytes.get(self.pos) == Some(&b'>') {
                        self.pos += 1;
                        return (attrs, true, false);
                    }
                    continue; // stray '/': spec parse error, ignored
                }
                Some(_) => {}
            }

            // Attribute name: until ws, '=', '/', '>'.
            let name_start = self.pos;
            while self.pos < self.bytes.len()
                && !matches!(
                    self.bytes[self.pos],
                    b'\t' | b'\n' | b'\x0C' | b' ' | b'=' | b'/' | b'>'
                )
            {
                self.pos += 1;
            }
            let raw_name = &self.bytes[name_start..self.pos];
            let mut name = String::with_capacity(raw_name.len());
            for &b in raw_name {
                name.push(b.to_ascii_lowercase() as char);
            }

            self.skip_ws();
            let value = if self.bytes.get(self.pos) == Some(&b'=') {
                self.pos += 1;
                self.skip_ws();
                self.parse_attr_value()
            } else {
                String::new()
            };

            if keep && !name.is_empty() && !attrs.iter().any(|(n, _)| *n == name) {
                attrs.push((name, value));
            }
        }
    }

    fn parse_attr_value(&mut self) -> String {
        let mut out = String::new();
        match self.bytes.get(self.pos) {
            Some(&q) if q == b'"' || q == b'\'' => {
                self.pos += 1;
                while self.pos < self.bytes.len() && self.bytes[self.pos] != q {
                    self.push_value_byte(&mut out, true);
                }
                if self.pos < self.bytes.len() {
                    self.pos += 1; // closing quote
                }
            }
            _ => {
                // Unquoted: until whitespace or '>'.
                while self.pos < self.bytes.len()
                    && !matches!(self.bytes[self.pos], b'\t' | b'\n' | b'\x0C' | b' ' | b'>')
                {
                    self.push_value_byte(&mut out, true);
                }
            }
        }
        out
    }

    /// Consume one input unit inside an attribute value, decoding character
    /// references and UTF-8.
    fn push_value_byte(&mut self, out: &mut String, in_attr: bool) {
        match self.bytes[self.pos] {
            b'&' => {
                self.pos += 1;
                match parse_char_ref(&self.bytes[self.pos..], in_attr) {
                    Some(cr) => {
                        self.pos += cr.consumed;
                        out.push_str(cr.text);
                    }
                    None => out.push('&'),
                }
            }
            b if b < 0x80 => {
                self.pos += 1;
                out.push(b as char);
            }
            _ => {
                let cp = self.decode_utf8();
                out.push_str(transliterate(cp));
            }
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
        {
            self.pos += 1;
        }
    }

    // ── Markup declarations: comments, DOCTYPE, CDATA ───────────────────────

    /// `pos` is at `<` of `<!…`.
    fn consume_markup_declaration(&mut self) {
        let after = self.pos + 2; // past `<!`
        if self.bytes[after..].starts_with(b"--") {
            self.consume_comment(after + 2);
        } else if starts_with_ci(&self.bytes[after..], b"doctype") {
            self.consume_until_gt(after + 7);
        } else if self.bytes[after..].starts_with(b"[CDATA[") {
            // In HTML content CDATA is a bogus comment, but honouring `]]>`
            // is strictly more useful for the SVG/MathML islands we skip.
            match find_sub(&self.bytes[after + 7..], b"]]>") {
                Some(rel) => self.pos = after + 7 + rel + 3,
                None => self.consume_until_gt(after + 7),
            }
        } else {
            self.consume_until_gt(after);
        }
    }

    /// Comment body starts at `start` (just past `<!--`). Handles the abrupt
    /// `<!-->` / `<!--->` closings and the `--!>` terminator.
    fn consume_comment(&mut self, start: usize) {
        let rest = &self.bytes[start..];
        if rest.starts_with(b">") {
            self.pos = start + 1;
            return;
        }
        if rest.starts_with(b"->") {
            self.pos = start + 2;
            return;
        }
        let mut i = start;
        while i < self.bytes.len() {
            if self.bytes[i..].starts_with(b"-->") {
                self.pos = i + 3;
                return;
            }
            if self.bytes[i..].starts_with(b"--!>") {
                self.pos = i + 4;
                return;
            }
            i += 1;
        }
        self.pos = self.bytes.len();
    }

    fn consume_until_gt(&mut self, from: usize) {
        let mut i = from;
        while i < self.bytes.len() && self.bytes[i] != b'>' {
            i += 1;
        }
        self.pos = (i + 1).min(self.bytes.len());
    }

    /// Bogus comment: `<?…>` or `</` + non-letter. Consumed to the first `>`.
    fn consume_bogus_comment(&mut self) {
        self.consume_until_gt(self.pos + 1);
    }

    // ── Raw text (RAWTEXT / RCDATA / script data) ───────────────────────────

    fn next_raw(&mut self) -> Token {
        let end = match self.mode {
            Mode::ScriptData => self.find_script_end(),
            _ => self.find_appropriate_end_tag(self.pos),
        };
        let (text_end, resume) = match end {
            Some(at) => (at, true),
            None => (self.bytes.len(), false),
        };

        let mut text = String::with_capacity(text_end - self.pos);
        let decode_entities = self.mode == Mode::Rcdata;
        while self.pos < text_end {
            match self.bytes[self.pos] {
                b'&' if decode_entities => {
                    self.pos += 1;
                    match parse_char_ref(&self.bytes[self.pos..], false) {
                        Some(cr) => {
                            self.pos += cr.consumed;
                            text.push_str(cr.text);
                        }
                        None => text.push('&'),
                    }
                }
                b'\r' => {
                    self.pos += 1;
                    text.push('\n');
                    if self.pos < text_end && self.bytes[self.pos] == b'\n' {
                        self.pos += 1;
                    }
                }
                b if b < 0x80 => {
                    self.pos += 1;
                    text.push(b as char);
                }
                _ => {
                    let cp = self.decode_utf8();
                    text.push_str(transliterate(cp));
                }
            }
        }

        if resume {
            // Consume `</tag …>` and queue the end-tag token.
            self.pos = text_end + 2 + self.raw_tag.as_str().len();
            let (_, _, eof) = self.parse_attrs(false);
            if !eof {
                self.pending_end = Some(self.raw_tag);
            } else {
                self.mode = Mode::Data;
            }
        } else {
            self.mode = Mode::Data;
        }

        if text.is_empty() {
            self.next_token()
        } else {
            Token::Text(text)
        }
    }

    /// Find the byte offset of the next `</raw_tag` followed by a delimiter
    /// (the spec's "appropriate end tag" check), searching from `from`.
    fn find_appropriate_end_tag(&self, from: usize) -> Option<usize> {
        let tag = self.raw_tag.as_str().as_bytes();
        let mut i = from;
        while i + 2 + tag.len() <= self.bytes.len() {
            if self.bytes[i] == b'<' && self.bytes[i + 1] == b'/' {
                let name = &self.bytes[i + 2..i + 2 + tag.len()];
                if eq_ci(name, tag) {
                    match self.bytes.get(i + 2 + tag.len()) {
                        None | Some(b'\t') | Some(b'\n') | Some(b'\x0C') | Some(b'\r')
                        | Some(b' ') | Some(b'/') | Some(b'>') => return Some(i),
                        _ => {}
                    }
                }
            }
            i += 1;
        }
        None
    }

    /// Script data with the escaping dance: inside `<!-- … -->`, a nested
    /// `<script>` opens a *double-escaped* region in which `</script>` is
    /// ordinary text rather than the end of the element.
    fn find_script_end(&self) -> Option<usize> {
        let mut escaped = false;
        let mut double = false;
        let mut i = self.pos;
        while i < self.bytes.len() {
            let rest = &self.bytes[i..];
            if !escaped && rest.starts_with(b"<!--") {
                escaped = true;
                i += 4;
                continue;
            }
            if escaped && rest.starts_with(b"-->") {
                escaped = false;
                double = false;
                i += 3;
                continue;
            }
            if rest.starts_with(b"</") && self.tag_name_here(i + 2, b"script") {
                if double {
                    double = false; // leaves double-escaped, still text
                    i += 9;
                    continue;
                }
                return Some(i);
            }
            if escaped && rest.starts_with(b"<") && self.tag_name_here(i + 1, b"script") {
                double = true;
                i += 8;
                continue;
            }
            i += 1;
        }
        None
    }

    /// Whether the bytes at `at` spell `name` (case-insensitive) followed by a
    /// tag-name delimiter.
    fn tag_name_here(&self, at: usize, name: &[u8]) -> bool {
        if at + name.len() > self.bytes.len() {
            return false;
        }
        if !eq_ci(&self.bytes[at..at + name.len()], name) {
            return false;
        }
        matches!(
            self.bytes.get(at + name.len()),
            None | Some(b'\t')
                | Some(b'\n')
                | Some(b'\x0C')
                | Some(b'\r')
                | Some(b' ')
                | Some(b'/')
                | Some(b'>')
        )
    }

    fn next_plaintext(&mut self) -> Token {
        if self.pos >= self.bytes.len() {
            return Token::Eof;
        }
        let mut text = String::with_capacity(self.bytes.len() - self.pos);
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b < 0x80 {
                self.pos += 1;
                text.push(b as char);
            } else {
                let cp = self.decode_utf8();
                text.push_str(transliterate(cp));
            }
        }
        Token::Text(text)
    }
}

enum TagOpen {
    /// A start or end tag follows.
    Tag,
    /// `<!…` — comment, DOCTYPE, or CDATA.
    Markup,
    /// Bogus comment (`<?…`, `</` + non-letter).
    Bogus,
    /// A literal `<` character.
    Literal,
}

fn eq_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.eq_ignore_ascii_case(y))
}

fn starts_with_ci(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && eq_ci(&haystack[..needle.len()], needle)
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}
