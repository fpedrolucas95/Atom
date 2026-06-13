//! The browser's document model: the flat list of laid-out blocks the renderer
//! consumes, plus the link/input side tables addressed by index.

use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;

/// Block-level kind, which drives spacing, weight, and default colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputKind {
    Text,
    Search,
    Submit,
    /// A `<select>` drop-down, rendered read-only showing the chosen option.
    Select,
}

/// Horizontal alignment of a block's content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Main-axis direction of a flex container.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Row,
    Column,
}

/// Main-axis distribution of free space in a flex container
/// (`justify-content`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

/// Cross-axis placement of flex items within a line (`align-items`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
}

/// Inline rendering attributes applied to a [`Run`]. Resolved at parse time
/// from HTML tags (`b`, `code`, `a`, …) and CSS so the renderer stays trivial.
#[derive(Clone, Copy)]
pub struct RunStyle {
    pub color: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    pub strike: bool,
    /// Glyph scale (1 = native 8px), derived from the computed `font-size`.
    pub size: u8,
}

impl Default for RunStyle {
    fn default() -> Self {
        Self {
            color: None,
            bold: false,
            italic: false,
            mono: false,
            underline: false,
            strike: false,
            size: 1,
        }
    }
}

/// A contiguous run of inline text sharing one style and optional hyperlink
/// (an index into [`Document::links`]).
pub struct Run {
    pub text: String,
    pub link: Option<usize>,
    /// Clickable region (an index into [`Document::click_nodes`]) when an
    /// enclosing element has a JavaScript click handler.
    pub zone: Option<usize>,
    pub style: RunStyle,
}

/// An inline-level item within a flow block. Form controls flow alongside text
/// (mirroring HTML's inline-block default) rather than each starting a new line.
pub enum Inline {
    Run(Run),
    /// A form control, indexing into [`Document::inputs`].
    Control(usize),
}

/// A CSS length that may be absolute or relative to a containing size.
/// Percentages are resolved at layout time, when the container width is known.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Length {
    Px(u32),
    /// A percentage (whole number) of the resolution base.
    Pct(u16),
}

impl Length {
    /// Resolve to pixels against `base` (the relevant container dimension).
    pub fn resolve(self, base: u32) -> u32 {
        match self {
            Length::Px(v) => v,
            Length::Pct(p) => base * p as u32 / 100,
        }
    }
}

/// CSS `position`. `Sticky` is parsed but treated as `Relative`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

/// A drop shadow (`box-shadow`): offset, blur, spread, and colour. Inset
/// shadows are not modelled.
#[derive(Clone, Copy)]
pub struct BoxShadow {
    pub dx: i16,
    pub dy: i16,
    pub blur: u16,
    pub spread: u16,
    pub color: Color,
}

/// Box-model decoration for a [`Block::Box`] container: background fill,
/// padding (top/right/bottom/left, in pixels) and a uniform border.
#[derive(Clone, Copy)]
pub struct BoxStyle {
    pub background: Option<Color>,
    /// Padding in pixels, in CSS order: top, right, bottom, left.
    pub padding: [u16; 4],
    /// Margin in pixels, in CSS order: top, right, bottom, left.
    pub margin: [u16; 4],
    /// `margin-left`/`margin-right: auto`: centre the box in its container.
    pub center: bool,
    pub border_width: u16,
    pub border_color: Option<Color>,
    /// Corner radius in pixels (`border-radius`).
    pub radius: u16,
    /// `box-sizing: border-box` — the `width` includes padding + border.
    pub border_box: bool,
    /// Explicit content width (`width`/`max-width`), capped to the available
    /// width; percentages resolve against the container at layout time.
    pub width: Option<Length>,
    /// Content-area height floor (`height`/`min-height`) in pixels.
    pub min_height: Option<u32>,
    /// Drop shadow (`box-shadow`).
    pub shadow: Option<BoxShadow>,
    /// `position`.
    pub position: Position,
    /// `top`, `right`, `bottom`, `left` offsets.
    pub inset: [Option<Length>; 4],
}

impl BoxStyle {
    /// Whether wrapping the element in a box block is worthwhile — it either
    /// paints, insets, offsets, or constrains the size of its content.
    pub fn needs_box(&self) -> bool {
        self.background.is_some()
            || self.border_width > 0
            || self.center
            || self.width.is_some()
            || self.min_height.is_some()
            || self.shadow.is_some()
            || self.position != Position::Static
            || self.padding.iter().any(|&p| p > 0)
            || self.margin.iter().any(|&m| m > 0)
    }
}

/// One child of a flex container: its own sub-flow of blocks plus the flex
/// sizing inputs resolved from the child's computed style.
pub struct FlexChild {
    /// `flex-grow` factor (0 = does not grow).
    pub grow: u16,
    /// Preferred main-axis size (`flex-basis`/`width`), if any; percentages
    /// resolve against the flex container's available width at layout time.
    pub basis: Option<Length>,
    /// The child's content, laid out within its flex line column.
    pub blocks: Vec<Block>,
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
        error: Option<String>,
        src: String,
        align: Align,
    },
    /// A `display: flex` container. Children lay out along `direction`,
    /// distributing free space per `justify` and aligning on the cross axis
    /// per `align`.
    Flex {
        direction: FlexDirection,
        justify: JustifyContent,
        align: AlignItems,
        gap: u16,
        /// `flex-wrap: wrap` — items overflow onto new lines instead of
        /// shrinking to a single line.
        wrap: bool,
        children: Vec<FlexChild>,
    },
    /// A decorated block container (`background`, `padding`, `border`) wrapping
    /// its element's sub-flow.
    Box {
        style: BoxStyle,
        children: Vec<Block>,
    },
}

/// An out-of-flow box (`position: absolute`/`fixed`) hoisted out of the normal
/// flow and painted against the content area in a deferred pass.
pub struct PositionedBox {
    pub style: BoxStyle,
    pub blocks: Vec<Block>,
}

/// Collect a mutable reference to every image block in document order,
/// descending into flex children so nested `<img>` elements are reachable for
/// fetching/decoding.
pub fn collect_image_blocks<'a>(blocks: &'a mut [Block], out: &mut Vec<&'a mut Block>) {
    for block in blocks {
        match block {
            Block::Flex { children, .. } => {
                for child in children.iter_mut() {
                    collect_image_blocks(&mut child.blocks, out);
                }
            }
            Block::Box { children, .. } => collect_image_blocks(children, out),
            Block::Image { .. } => out.push(block),
            _ => {}
        }
    }
}

/// Metadata for a form control, addressed by index from [`Inline::Control`].
pub struct InputMeta {
    pub kind: InputKind,
    pub placeholder: String,
    pub name: String,
    pub action: String,
    /// Requested width in characters (`size` attribute), if any.
    pub size: Option<u32>,
    /// Labels captured from `<option>` children for `<select>` controls.
    pub options: Vec<String>,
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
    pub background: Option<Color>,
    pub blocks: Vec<Block>,
    /// Out-of-flow boxes (`position: absolute`/`fixed`), painted after the flow.
    pub positioned: Vec<PositionedBox>,
    pub links: Vec<String>,
    /// DOM node id behind each entry of `links` (for event dispatch).
    pub link_nodes: Vec<usize>,
    pub inputs: Vec<InputMeta>,
    /// DOM node id behind each entry of `inputs`.
    pub input_nodes: Vec<usize>,
    /// DOM node ids of elements with JavaScript click handlers, indexed by
    /// [`Run::zone`].
    pub click_nodes: Vec<usize>,
}
