#![no_std]
use core::ffi::{c_char, c_void};

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SvgStatus {
    Success = 0,
    NoMemory,
    IoError,
    FileNotFound,
    ResourceNotFound,
    InvalidValue,
    InvalidBaseUri,
    DuplicateElementId,
    MissingRefElementId,
    RelativeUri,
    UnknownRefElement,
    WrongRefElementType,
    RootElementNotSvg,
    MissingSvgNamespaceDecl,
    EmptyDocument,
    AttributeNotFound,
    InvalidCall,
    ParseError,
    XmlParseError,
    XmlTagMismatch,
    XmlUnboundPrefix,
    XmlInvalidToken,
    CssParseError,
    NoRelativeUri,
    ExternalDocumentError,
    InternalError,
}

#[repr(C)]
pub struct SvgOpaque {
    _private: [u8; 0],
}

#[repr(C)]
pub struct SvgElementOpaque {
    _private: [u8; 0],
}

#[repr(C)]
pub struct SvgElementRefOpaque {
    _private: [u8; 0],
}

pub type Svg = SvgOpaque;
pub type SvgElement = SvgElementOpaque;
pub type SvgElementRef = SvgElementRefOpaque;

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SvgLengthUnit {
    Cm,
    Em,
    Ex,
    In,
    Mm,
    Pc,
    Pct,
    Pt,
    Px,
}

#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SvgLengthOrientation {
    Horizontal,
    Vertical,
    Other,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct SvgLength {
    pub value: f64,
    pub unit: SvgLengthUnit,
    pub orientation: SvgLengthOrientation,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct SvgRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub fn status_result(status: SvgStatus) -> Result<(), SvgStatus> {
    match status {
    SvgStatus::Success => Ok(()),
    other => Err(other),
    }
}

unsafe extern "C" {
    pub fn svg_create(svg: *mut *mut Svg) -> SvgStatus;
    pub fn svg_destroy(svg: *mut Svg) -> SvgStatus;

    pub fn svg_set_base_directory(svg: *mut Svg, directory: *const c_char) -> SvgStatus;
    pub fn svg_set_base_uri(svg: *mut Svg, abs_uri: *const c_char) -> SvgStatus;

    pub fn svg_parse(svg: *mut Svg, uri_or_filename: *const c_char) -> SvgStatus;
    pub fn svg_parse_buffer(svg: *mut Svg, buf: *const c_char, count: usize) -> SvgStatus;

    pub fn svg_get_dpi(svg: *const Svg) -> f64;
    pub fn svg_get_size(svg: *const Svg, width: *mut SvgLength, height: *mut SvgLength);

    pub fn svg_get_error_status(svg: *const Svg) -> SvgStatus;
    pub fn svg_get_external_error_status(svg: *const Svg) -> SvgStatus;
    pub fn svg_get_error_string(status: SvgStatus) -> *const c_char;
    pub fn svg_get_error_line_number(svg: *const Svg) -> i64;
    pub fn svg_get_error_element(svg: *const Svg) -> *const c_char;
    pub fn svg_get_error_property(svg: *const Svg) -> *const c_char;
    pub fn svg_get_error_attribute(svg: *const Svg) -> *const c_char;

    pub fn svg_trace_render_engine(svg: *mut Svg) -> SvgStatus;
    pub fn svg_element_render(
    element: *mut SvgElement,
    engine: *mut c_void,
    closure: *mut c_void,
    ) -> SvgStatus;
    pub fn svg_element_ref_render(
    element_ref: *mut SvgElementRef,
    engine: *mut c_void,
    closure: *mut c_void,
    ) -> SvgStatus;
}