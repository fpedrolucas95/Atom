//! HTML5 character references.
//!
//! Implements the spec's character-reference algorithm: named references (with
//! the legacy semicolon-less forms and the "longest match" rule, including the
//! `&notit;` → `¬it;` behaviour), numeric references with the windows-1252
//! remapping of the C1 range, and the attribute-value restriction that stops
//! `&copy=1` in a query string from being decoded.
//!
//! The display font is ASCII-only, so every reference resolves through
//! [`transliterate`], which maps Unicode codepoints to readable ASCII
//! approximations (`é` → `e`, `—` → `--`, `→` → `->`).

/// A successfully parsed character reference.
pub struct CharRef {
    /// ASCII rendering of the referenced character.
    pub text: &'static str,
    /// Bytes consumed *after* the `&`.
    pub consumed: usize,
}

/// Parse a character reference starting just after an `&`.
///
/// `in_attribute` enables the spec rule that a legacy (semicolon-less) match
/// must not be followed by `=` or an alphanumeric when inside an attribute.
pub fn parse_char_ref(bytes: &[u8], in_attribute: bool) -> Option<CharRef> {
    if bytes.first() == Some(&b'#') {
        return parse_numeric(&bytes[1..]);
    }

    // Collect the maximal alphanumeric run (entity names are alphanumeric).
    let mut len = 0;
    while len < bytes.len() && len < 32 && bytes[len].is_ascii_alphanumeric() {
        len += 1;
    }
    if len == 0 {
        return None;
    }
    let run = &bytes[..len];
    let after = bytes.get(len).copied();

    // Exact match terminated by a semicolon takes priority.
    if after == Some(b';') {
        if let Some(cp) = lookup_named(run) {
            return Some(CharRef {
                text: transliterate(cp),
                consumed: len + 1,
            });
        }
    }

    // Longest legacy prefix without a semicolon (e.g. `&notit` → `¬it`).
    let mut l = len;
    while l >= 2 {
        let prefix = &run[..l];
        if is_legacy(prefix) {
            if in_attribute {
                // In attributes a partial match, or one followed by `=` or an
                // alphanumeric, is left undecoded (spec: historical reasons).
                if l < len || after == Some(b'=') {
                    return None;
                }
            }
            let cp = lookup_named(prefix)?;
            return Some(CharRef {
                text: transliterate(cp),
                consumed: l,
            });
        }
        l -= 1;
    }
    None
}

fn parse_numeric(bytes: &[u8]) -> Option<CharRef> {
    let (digits_start, radix) = match bytes.first() {
        Some(b'x') | Some(b'X') => (1, 16u32),
        _ => (0, 10u32),
    };
    let mut value: u32 = 0;
    let mut i = digits_start;
    let mut any = false;
    while i < bytes.len() {
        let Some(d) = (bytes[i] as char).to_digit(radix) else {
            break;
        };
        any = true;
        value = value.saturating_mul(radix).saturating_add(d);
        i += 1;
    }
    if !any {
        return None;
    }
    // Trailing semicolon is consumed when present (missing one is an error
    // the spec recovers from).
    let mut consumed = i + 1; // +1 for the leading '#'
    if bytes.get(i) == Some(&b';') {
        consumed += 1;
    }
    Some(CharRef {
        text: transliterate(remap_numeric(value)),
        consumed,
    })
}

/// Spec remapping for numeric references: NUL and out-of-range become U+FFFD,
/// and the C1 control range maps through windows-1252.
fn remap_numeric(cp: u32) -> u32 {
    match cp {
        0 => 0xFFFD,
        c if c > 0x10FFFF => 0xFFFD,
        0xD800..=0xDFFF => 0xFFFD,
        0x80 => 0x20AC,
        0x82 => 0x201A,
        0x83 => 0x0192,
        0x84 => 0x201E,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02C6,
        0x89 => 0x2030,
        0x8A => 0x0160,
        0x8B => 0x2039,
        0x8C => 0x0152,
        0x8E => 0x017D,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201C,
        0x94 => 0x201D,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02DC,
        0x99 => 0x2122,
        0x9A => 0x0161,
        0x9B => 0x203A,
        0x9C => 0x0153,
        0x9E => 0x017E,
        0x9F => 0x0178,
        c => c,
    }
}

/// Look up a named entity (without `&` or `;`), returning its codepoint.
pub fn lookup_named(name: &[u8]) -> Option<u32> {
    NAMED
        .iter()
        .find(|(n, _)| n.as_bytes() == name)
        .map(|&(_, cp)| cp)
}

/// Whether `name` may legally appear without a trailing semicolon.
///
/// The spec's legacy list is exactly the HTML 3.2 / Latin-1 era names; all of
/// them resolve to codepoints ≤ 0xFF, and of our table only `apos` shares that
/// range without being legacy.
fn is_legacy(name: &[u8]) -> bool {
    if name == b"apos" {
        return false;
    }
    matches!(lookup_named(name), Some(cp) if cp <= 0xFF)
}

/// Named entity table: `name` (case-sensitive, no `&`/`;`) → codepoint.
/// Covers the full Latin-1 set, Greek, common punctuation, currency, arrows,
/// math operators and symbols — the references that occur on real pages.
static NAMED: &[(&str, u32)] = &[
    // Markup-critical
    ("amp", 0x26), ("AMP", 0x26), ("lt", 0x3C), ("LT", 0x3C),
    ("gt", 0x3E), ("GT", 0x3E), ("quot", 0x22), ("QUOT", 0x22),
    ("apos", 0x27),
    // Spaces and format controls
    ("nbsp", 0xA0), ("NonBreakingSpace", 0xA0), ("ensp", 0x2002),
    ("emsp", 0x2003), ("emsp13", 0x2004), ("emsp14", 0x2005),
    ("thinsp", 0x2009), ("hairsp", 0x200A), ("ZeroWidthSpace", 0x200B),
    ("zwnj", 0x200C), ("zwj", 0x200D), ("lrm", 0x200E), ("rlm", 0x200F),
    ("shy", 0xAD),
    // Latin-1 punctuation and signs
    ("iexcl", 0xA1), ("cent", 0xA2), ("pound", 0xA3), ("curren", 0xA4),
    ("yen", 0xA5), ("brvbar", 0xA6), ("sect", 0xA7), ("uml", 0xA8),
    ("copy", 0xA9), ("COPY", 0xA9), ("ordf", 0xAA), ("laquo", 0xAB),
    ("not", 0xAC), ("reg", 0xAE), ("REG", 0xAE), ("macr", 0xAF),
    ("deg", 0xB0), ("plusmn", 0xB1), ("sup2", 0xB2), ("sup3", 0xB3),
    ("acute", 0xB4), ("micro", 0xB5), ("para", 0xB6), ("middot", 0xB7),
    ("cedil", 0xB8), ("sup1", 0xB9), ("ordm", 0xBA), ("raquo", 0xBB),
    ("frac14", 0xBC), ("frac12", 0xBD), ("frac34", 0xBE), ("iquest", 0xBF),
    ("times", 0xD7), ("divide", 0xF7),
    // Latin-1 letters
    ("Agrave", 0xC0), ("Aacute", 0xC1), ("Acirc", 0xC2), ("Atilde", 0xC3),
    ("Auml", 0xC4), ("Aring", 0xC5), ("AElig", 0xC6), ("Ccedil", 0xC7),
    ("Egrave", 0xC8), ("Eacute", 0xC9), ("Ecirc", 0xCA), ("Euml", 0xCB),
    ("Igrave", 0xCC), ("Iacute", 0xCD), ("Icirc", 0xCE), ("Iuml", 0xCF),
    ("ETH", 0xD0), ("Ntilde", 0xD1), ("Ograve", 0xD2), ("Oacute", 0xD3),
    ("Ocirc", 0xD4), ("Otilde", 0xD5), ("Ouml", 0xD6), ("Oslash", 0xD8),
    ("Ugrave", 0xD9), ("Uacute", 0xDA), ("Ucirc", 0xDB), ("Uuml", 0xDC),
    ("Yacute", 0xDD), ("THORN", 0xDE), ("szlig", 0xDF),
    ("agrave", 0xE0), ("aacute", 0xE1), ("acirc", 0xE2), ("atilde", 0xE3),
    ("auml", 0xE4), ("aring", 0xE5), ("aelig", 0xE6), ("ccedil", 0xE7),
    ("egrave", 0xE8), ("eacute", 0xE9), ("ecirc", 0xEA), ("euml", 0xEB),
    ("igrave", 0xEC), ("iacute", 0xED), ("icirc", 0xEE), ("iuml", 0xEF),
    ("eth", 0xF0), ("ntilde", 0xF1), ("ograve", 0xF2), ("oacute", 0xF3),
    ("ocirc", 0xF4), ("otilde", 0xF5), ("ouml", 0xF6), ("oslash", 0xF8),
    ("ugrave", 0xF9), ("uacute", 0xFA), ("ucirc", 0xFB), ("uuml", 0xFC),
    ("yacute", 0xFD), ("thorn", 0xFE), ("yuml", 0xFF),
    // Latin extended / ligatures
    ("OElig", 0x152), ("oelig", 0x153), ("Scaron", 0x160), ("scaron", 0x161),
    ("Yuml", 0x178), ("fnof", 0x192), ("circ", 0x2C6), ("tilde", 0x2DC),
    // General punctuation
    ("ndash", 0x2013), ("mdash", 0x2014), ("horbar", 0x2015),
    ("lsquo", 0x2018), ("rsquo", 0x2019), ("sbquo", 0x201A),
    ("ldquo", 0x201C), ("rdquo", 0x201D), ("bdquo", 0x201E),
    ("dagger", 0x2020), ("Dagger", 0x2021), ("bull", 0x2022),
    ("bullet", 0x2022), ("hellip", 0x2026), ("permil", 0x2030),
    ("prime", 0x2032), ("Prime", 0x2033), ("lsaquo", 0x2039),
    ("rsaquo", 0x203A), ("oline", 0x203E), ("frasl", 0x2044),
    ("euro", 0x20AC),
    // Letterlike
    ("weierp", 0x2118), ("image", 0x2111), ("real", 0x211C),
    ("trade", 0x2122), ("TRADE", 0x2122), ("alefsym", 0x2135),
    ("numero", 0x2116),
    // Arrows
    ("larr", 0x2190), ("uarr", 0x2191), ("rarr", 0x2192), ("darr", 0x2193),
    ("harr", 0x2194), ("crarr", 0x21B5), ("lArr", 0x21D0), ("uArr", 0x21D1),
    ("rArr", 0x21D2), ("dArr", 0x21D3), ("hArr", 0x21D4),
    ("leftarrow", 0x2190), ("rightarrow", 0x2192), ("uparrow", 0x2191),
    ("downarrow", 0x2193),
    // Mathematical operators
    ("forall", 0x2200), ("part", 0x2202), ("exist", 0x2203),
    ("empty", 0x2205), ("nabla", 0x2207), ("isin", 0x2208),
    ("notin", 0x2209), ("ni", 0x220B), ("prod", 0x220F), ("sum", 0x2211),
    ("minus", 0x2212), ("lowast", 0x2217), ("radic", 0x221A),
    ("prop", 0x221D), ("infin", 0x221E), ("ang", 0x2220), ("and", 0x2227),
    ("or", 0x2228), ("cap", 0x2229), ("cup", 0x222A), ("int", 0x222B),
    ("there4", 0x2234), ("sim", 0x223C), ("cong", 0x2245),
    ("asymp", 0x2248), ("ne", 0x2260), ("equiv", 0x2261), ("le", 0x2264),
    ("leq", 0x2264), ("ge", 0x2265), ("geq", 0x2265), ("sub", 0x2282),
    ("sup", 0x2283), ("nsub", 0x2284), ("sube", 0x2286), ("supe", 0x2287),
    ("oplus", 0x2295), ("otimes", 0x2297), ("perp", 0x22A5),
    ("sdot", 0x22C5),
    // Technical and shapes
    ("lceil", 0x2308), ("rceil", 0x2309), ("lfloor", 0x230A),
    ("rfloor", 0x230B), ("lang", 0x27E8), ("rang", 0x27E9), ("loz", 0x25CA),
    ("spades", 0x2660), ("clubs", 0x2663), ("hearts", 0x2665),
    ("diams", 0x2666), ("check", 0x2713), ("checkmark", 0x2713),
    ("cross", 0x2717), ("starf", 0x2605), ("star", 0x2606),
    // Greek
    ("Alpha", 0x391), ("Beta", 0x392), ("Gamma", 0x393), ("Delta", 0x394),
    ("Epsilon", 0x395), ("Zeta", 0x396), ("Eta", 0x397), ("Theta", 0x398),
    ("Iota", 0x399), ("Kappa", 0x39A), ("Lambda", 0x39B), ("Mu", 0x39C),
    ("Nu", 0x39D), ("Xi", 0x39E), ("Omicron", 0x39F), ("Pi", 0x3A0),
    ("Rho", 0x3A1), ("Sigma", 0x3A3), ("Tau", 0x3A4), ("Upsilon", 0x3A5),
    ("Phi", 0x3A6), ("Chi", 0x3A7), ("Psi", 0x3A8), ("Omega", 0x3A9),
    ("alpha", 0x3B1), ("beta", 0x3B2), ("gamma", 0x3B3), ("delta", 0x3B4),
    ("epsilon", 0x3B5), ("zeta", 0x3B6), ("eta", 0x3B7), ("theta", 0x3B8),
    ("iota", 0x3B9), ("kappa", 0x3BA), ("lambda", 0x3BB), ("mu", 0x3BC),
    ("nu", 0x3BD), ("xi", 0x3BE), ("omicron", 0x3BF), ("pi", 0x3C0),
    ("rho", 0x3C1), ("sigmaf", 0x3C2), ("sigma", 0x3C3), ("tau", 0x3C4),
    ("upsilon", 0x3C5), ("phi", 0x3C6), ("chi", 0x3C7), ("psi", 0x3C8),
    ("omega", 0x3C9), ("thetasym", 0x3D1), ("upsih", 0x3D2), ("piv", 0x3D6),
];

/// Map a Unicode codepoint to an ASCII approximation the bitmap font can draw.
pub fn transliterate(cp: u32) -> &'static str {
    if cp < 0x80 {
        return ASCII_TABLE[cp as usize];
    }
    match cp {
        // Latin-1 punctuation and signs
        0xA0 => " ",
        0xA1 => "!",
        0xA2 => "c",
        0xA3 => "GBP",
        0xA4 => "o",
        0xA5 => "JPY",
        0xA6 => "|",
        0xA7 => "S",
        0xA8 => "\"",
        0xA9 => "(c)",
        0xAA => "a",
        0xAB => "<<",
        0xAC => "-",
        0xAD => "",
        0xAE => "(R)",
        0xAF => "-",
        0xB0 => "deg",
        0xB1 => "+/-",
        0xB2 => "2",
        0xB3 => "3",
        0xB4 => "'",
        0xB5 => "u",
        0xB6 => "P",
        0xB7 => "*",
        0xB8 => ",",
        0xB9 => "1",
        0xBA => "o",
        0xBB => ">>",
        0xBC => "1/4",
        0xBD => "1/2",
        0xBE => "3/4",
        0xBF => "?",
        0xD7 => "x",
        0xF7 => "/",
        // Latin-1 letters
        0xC0..=0xC5 => "A",
        0xC6 => "AE",
        0xC7 => "C",
        0xC8..=0xCB => "E",
        0xCC..=0xCF => "I",
        0xD0 => "D",
        0xD1 => "N",
        0xD2..=0xD6 | 0xD8 => "O",
        0xD9..=0xDC => "U",
        0xDD => "Y",
        0xDE => "Th",
        0xDF => "ss",
        0xE0..=0xE5 => "a",
        0xE6 => "ae",
        0xE7 => "c",
        0xE8..=0xEB => "e",
        0xEC..=0xEF => "i",
        0xF0 => "d",
        0xF1 => "n",
        0xF2..=0xF6 | 0xF8 => "o",
        0xF9..=0xFC => "u",
        0xFD | 0xFF => "y",
        0xFE => "th",
        // Latin Extended-A (common subset, mapped to base letters)
        0x100 | 0x102 | 0x104 => "A",
        0x101 | 0x103 | 0x105 => "a",
        0x106 | 0x108 | 0x10A | 0x10C => "C",
        0x107 | 0x109 | 0x10B | 0x10D => "c",
        0x10E | 0x110 => "D",
        0x10F | 0x111 => "d",
        0x112..=0x11A if cp % 2 == 0 => "E",
        0x113..=0x11B if cp % 2 == 1 => "e",
        0x11C | 0x11E | 0x120 | 0x122 => "G",
        0x11D | 0x11F | 0x121 | 0x123 => "g",
        0x128 | 0x12A | 0x12C | 0x12E | 0x130 => "I",
        0x129 | 0x12B | 0x12D | 0x12F | 0x131 => "i",
        0x141 => "L",
        0x142 => "l",
        0x143 | 0x145 | 0x147 => "N",
        0x144 | 0x146 | 0x148 => "n",
        0x14C | 0x14E | 0x150 => "O",
        0x14D | 0x14F | 0x151 => "o",
        0x152 => "OE",
        0x153 => "oe",
        0x154 | 0x156 | 0x158 => "R",
        0x155 | 0x157 | 0x159 => "r",
        0x15A | 0x15C | 0x15E | 0x160 => "S",
        0x15B | 0x15D | 0x15F | 0x161 => "s",
        0x162 | 0x164 => "T",
        0x163 | 0x165 => "t",
        0x168 | 0x16A | 0x16C | 0x16E | 0x170 | 0x172 => "U",
        0x169 | 0x16B | 0x16D | 0x16F | 0x171 | 0x173 => "u",
        0x178 => "Y",
        0x179 | 0x17B | 0x17D => "Z",
        0x17A | 0x17C | 0x17E => "z",
        0x192 => "f",
        // Spacing modifiers
        0x2C6 => "^",
        0x2DC => "~",
        // Greek
        0x391 => "A",
        0x392 => "B",
        0x393 => "G",
        0x394 => "D",
        0x395 => "E",
        0x396 => "Z",
        0x397 => "H",
        0x398 => "Th",
        0x399 => "I",
        0x39A => "K",
        0x39B => "L",
        0x39C => "M",
        0x39D => "N",
        0x39E => "X",
        0x39F => "O",
        0x3A0 => "Pi",
        0x3A1 => "R",
        0x3A3 => "S",
        0x3A4 => "T",
        0x3A5 => "Y",
        0x3A6 => "Phi",
        0x3A7 => "Ch",
        0x3A8 => "Ps",
        0x3A9 => "O",
        0x3B1 => "a",
        0x3B2 => "b",
        0x3B3 => "g",
        0x3B4 => "d",
        0x3B5 => "e",
        0x3B6 => "z",
        0x3B7 => "n",
        0x3B8 => "th",
        0x3B9 => "i",
        0x3BA => "k",
        0x3BB => "l",
        0x3BC => "u",
        0x3BD => "v",
        0x3BE => "x",
        0x3BF => "o",
        0x3C0 => "pi",
        0x3C1 => "r",
        0x3C2 | 0x3C3 => "s",
        0x3C4 => "t",
        0x3C5 => "u",
        0x3C6 => "phi",
        0x3C7 => "ch",
        0x3C8 => "ps",
        0x3C9 => "w",
        0x3D1 => "th",
        0x3D2 => "Y",
        0x3D6 => "pi",
        // General punctuation
        0x2000..=0x200B => " ",
        0x200C..=0x200F => "",
        0x2010..=0x2012 => "-",
        0x2013 => "-",
        0x2014 | 0x2015 => "--",
        0x2018 | 0x2019 | 0x201A | 0x201B => "'",
        0x201C | 0x201D | 0x201E | 0x201F => "\"",
        0x2020 => "+",
        0x2021 => "++",
        0x2022 | 0x2023 | 0x2043 => "*",
        0x2026 => "...",
        0x2030 => "0/00",
        0x2032 => "'",
        0x2033 => "\"",
        0x2039 => "<",
        0x203A => ">",
        0x203C => "!!",
        0x203E => "-",
        0x2044 => "/",
        0x204A => "&",
        // Currency
        0x20A0..=0x20A9 => "$",
        0x20AC => "EUR",
        0x20BF => "BTC",
        // Letterlike
        0x2111 => "I",
        0x2116 => "No.",
        0x2118 => "P",
        0x211C => "R",
        0x2122 => "(TM)",
        0x2126 => "Ohm",
        0x2135 => "N",
        // Fractions
        0x2150 => "1/7",
        0x2153 => "1/3",
        0x2154 => "2/3",
        0x215B => "1/8",
        0x215C => "3/8",
        0x215D => "5/8",
        0x215E => "7/8",
        // Arrows
        0x2190 | 0x21D0 | 0x21E0 => "<-",
        0x2191 | 0x21D1 => "^",
        0x2192 | 0x21D2 | 0x21E2 => "->",
        0x2193 | 0x21D3 => "v",
        0x2194 | 0x21D4 => "<->",
        0x21B5 => "<-'",
        // Mathematical operators
        0x2200 => "A",
        0x2202 => "d",
        0x2203 => "E",
        0x2205 => "0",
        0x2207 => "V",
        0x2208 | 0x220B => "in",
        0x2209 => "!in",
        0x220F => "TT",
        0x2211 => "E",
        0x2212 => "-",
        0x2215 => "/",
        0x2217 => "*",
        0x221A => "sqrt",
        0x221D => "~",
        0x221E => "inf",
        0x2227 => "^",
        0x2228 => "v",
        0x2229 => "n",
        0x222A => "u",
        0x222B => "S",
        0x2234 => "::",
        0x223C => "~",
        0x2245 | 0x2248 => "~=",
        0x2260 => "!=",
        0x2261 => "==",
        0x2264 => "<=",
        0x2265 => ">=",
        0x2282 => "<",
        0x2283 => ">",
        0x22A5 => "_|_",
        0x22C5 => ".",
        // Technical
        0x2308 | 0x230A => "|",
        0x2309 | 0x230B => "|",
        0x2329 | 0x27E8 => "<",
        0x232A | 0x27E9 => ">",
        // Box drawing → ASCII art
        0x2502 => "|",
        0x2500..=0x250B => "-",
        0x250C..=0x254B => "+",
        0x2550 => "=",
        0x2551 => "|",
        0x2552..=0x256C => "+",
        0x2580..=0x259F => "#",
        // Shapes and symbols
        0x25A0 | 0x25AA => "#",
        0x25A1 | 0x25AB => "[]",
        0x25B2 | 0x25B4 => "^",
        0x25B6 | 0x25B8 | 0x25BA => ">",
        0x25BC | 0x25BE => "v",
        0x25C0 | 0x25C2 | 0x25C4 => "<",
        0x25CA => "<>",
        0x25CB | 0x25EF => "o",
        0x25CF => "*",
        0x2605 | 0x2606 | 0x2B50 => "*",
        0x2660 => "S",
        0x2663 => "C",
        0x2665 | 0x2764 => "<3",
        0x2666 => "D",
        0x2713 | 0x2714 => "v",
        0x2715..=0x2718 => "x",
        _ => "?",
    }
}

/// Static one-byte string slices for every ASCII codepoint, so
/// [`transliterate`] can return `&'static str` without allocating.
static ASCII_TABLE: [&str; 128] = [
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
