//! The browser's document model: the flat list of laid-out blocks the renderer
//! consumes, plus the link/input side tables addressed by index.

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;

/// Block-level kind, which drives spacing, weight, and default colour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    H1,
    H2,
    H3,
    Paragraph,
    ListItem,
    Quote,
    Pre,
}

/// Interactive input kind.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Text,
    Search,
    Submit,
}

/// Inline rendering attributes applied to a [`Run`]. Resolved at parse time
/// from HTML tags (`b`, `code`, `a`, …) and CSS so the renderer stays trivial.
#[derive(Clone, Copy, Default)]
pub struct RunStyle {
    pub color: Option<Color>,
    pub bold: bool,
    pub mono: bool,
    pub underline: bool,
}

/// A contiguous run of inline text sharing one style and optional hyperlink
/// (an index into [`Document::links`]).
pub struct Run {
    pub text: String,
    pub link: Option<usize>,
    pub style: RunStyle,
}

/// A laid-out block in document order.
pub enum Block {
    Text {
        kind: TextKind,
        runs: Vec<Run>,
        /// List bullet/number prefix, present only for list items.
        marker: Option<String>,
    },
    Rule,
    Image {
        alt: String,
        img: Option<libimage::DecodedImage>,
        src: String,
    },
    Input {
        idx: usize,
    },
}

/// Metadata for a form control, addressed by index from [`Block::Input`].
pub struct InputMeta {
    pub kind: InputKind,
    pub placeholder: String,
    pub name: String,
    pub action: String,
}

/// A clickable region in screen coordinates tied to a link or input index.
#[derive(Clone, Copy)]
pub struct Hit {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub idx: usize,
}

impl Hit {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A fully parsed document ready for layout.
pub struct Document {
    pub title: String,
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
    pub inputs: Vec<InputMeta>,
}
