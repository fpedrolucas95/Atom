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
    /// A `<select>` drop-down, rendered read-only showing the chosen option.
    Select,
}

/// Horizontal alignment of a block's content.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
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

/// An inline-level item within a flow block. Form controls flow alongside text
/// (mirroring HTML's inline-block default) rather than each starting a new line.
pub enum Inline {
    Run(Run),
    /// A form control, indexing into [`Document::inputs`].
    Control(usize),
}

/// A laid-out block in document order.
pub enum Block {
    Text {
        kind: TextKind,
        items: Vec<Inline>,
        align: Align,
        /// List bullet/number prefix, present only for list items.
        marker: Option<String>,
    },
    Rule,
    Image {
        alt: String,
        img: Option<libimage::DecodedImage>,
        src: String,
        align: Align,
    },
}

/// Metadata for a form control, addressed by index from [`Inline::Control`].
pub struct InputMeta {
    pub kind: InputKind,
    pub placeholder: String,
    pub name: String,
    pub action: String,
    /// Requested width in characters (`size` attribute), if any.
    pub size: Option<u32>,
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
