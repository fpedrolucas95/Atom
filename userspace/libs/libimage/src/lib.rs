//! libimage — Image decoding for Atom OS
//!
//! Provides PNG, JPEG, and GIF decoders with a unified output abstraction.
//! All code is `no_std` with `alloc`.
//!
//! # Architecture
//!
//! ```text
//! raw bytes
//!     │
//!     ├─ PngDecoder ──────────────────┐
//!     │   └─ deflate (DEFLATE/zlib)   │
//!     │                               ├──> DecodedImage (RGBA8888)
//!     └─ JpgDecoder ──────────────────┘
//!         └─ huffman, dct, ycbcr
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use libimage::{PngDecoder, ImageDecoder};
//!
//! let data: &[u8] = /* raw PNG bytes */;
//! let img = PngDecoder::decode(data).expect("valid PNG");
//! let (r, g, b, a) = img.get_pixel(0, 0).unwrap();
//! ```
 
#![no_std]
 
extern crate alloc;
 
pub mod image;
pub mod deflate;
pub mod png;
pub mod jpg;
pub mod gif;
pub mod cache;
 
pub use image::{DecodedImage, ImageFormat, ImageError, ImageDecoder};
pub use png::PngDecoder;
pub use jpg::JpgDecoder;
pub use gif::GifDecoder;
pub use cache::ImageCache;