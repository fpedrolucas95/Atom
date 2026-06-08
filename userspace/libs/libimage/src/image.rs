//! Core image abstractions: DecodedImage, ImageFormat, ImageError, ImageDecoder.
 
use alloc::vec::Vec;
 
/// Pixel format of a decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpg,
    Gif,
}
 
/// Errors returned by image decoders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// Magic bytes or file signature did not match.
    InvalidSignature,
    /// Valid format but unsupported feature (e.g. interlaced PNG).
    UnsupportedFormat(&'static str),
    /// Structurally corrupt or truncated data.
    CorruptData(&'static str),
    /// Decompression (DEFLATE/Huffman) failed.
    DecompressionFailed,
    /// Allocation failed (out of memory).
    OutOfMemory,
}
 
/// A decoded image stored as tightly-packed RGBA8888 pixels.
///
/// Pixel layout: `[R, G, B, A, R, G, B, A, ...]` row-major, top-to-bottom.
pub struct DecodedImage {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Raw pixel data: `width * height * 4` bytes, RGBA8888.
    pub pixels: Vec<u8>,
}
 
impl DecodedImage {
    /// Construct a new decoded image.
    ///
    /// # Panics
    /// In debug mode if `pixels.len() != width * height * 4`.
    #[inline]
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        debug_assert_eq!(pixels.len(), (width * height * 4) as usize);
        Self { width, height, pixels }
    }
 
    /// Construct an all-zero (transparent black) image.
    pub fn blank(width: u32, height: u32) -> Self {
        let size = (width as usize) * (height as usize) * 4;
        Self { width, height, pixels: alloc::vec![0u8; size] }
    }
 
    /// Read the RGBA components of pixel `(x, y)`.
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<(u8, u8, u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let off = ((y * self.width + x) * 4) as usize;
        Some((
            self.pixels[off],
            self.pixels[off + 1],
            self.pixels[off + 2],
            self.pixels[off + 3],
        ))
    }
 
    /// Blit this image onto a destination RGBA buffer at pixel offset `(dst_x, dst_y)`.
    ///
    /// Pixels are composited with premultiplied alpha over the destination.
    /// `dst_stride` is the number of **pixels** (not bytes) per row in `dst`.
    pub fn blit_to(&self, dst: &mut [u8], dst_stride: u32, dst_x: u32, dst_y: u32) {
        // FAST PATH: If the image is fully opaque and spans the whole destination row,
        // we can skip alpha math and potentially use optimized copies.
        let mut is_opaque = true;
        for i in 0..self.height {
            for j in 0..self.width {
                if self.pixels[((i * self.width + j) * 4 + 3) as usize] != 255 {
                    is_opaque = false;
                    break;
                }
            }
            if !is_opaque { break; }
        }

        if is_opaque {
            self.blit_opaque(dst, dst_stride, dst_x, dst_y);
            return;
        }

        for row in 0..self.height {
            for col in 0..self.width {
                let src_off = ((row * self.width + col) * 4) as usize;
                let src_r = self.pixels[src_off];
                let src_g = self.pixels[src_off + 1];
                let src_b = self.pixels[src_off + 2];
                let src_a = self.pixels[src_off + 3];
 
                let dst_off = (((dst_y + row) * dst_stride + (dst_x + col)) * 4) as usize;
                if dst_off + 4 > dst.len() {
                    continue;
                }
 
                // Over compositing: out = src_a * src + (1 - src_a) * dst
                let ia = 255u16 - src_a as u16;
                dst[dst_off]     = ((src_r as u16 * src_a as u16 + dst[dst_off]     as u16 * ia) / 255) as u8;
                dst[dst_off + 1] = ((src_g as u16 * src_a as u16 + dst[dst_off + 1] as u16 * ia) / 255) as u8;
                dst[dst_off + 2] = ((src_b as u16 * src_a as u16 + dst[dst_off + 2] as u16 * ia) / 255) as u8;
                dst[dst_off + 3] = src_a.saturating_add(((dst[dst_off + 3] as u16 * ia) / 255) as u8);
            }
        }
    }

    /// Optimized blit for fully opaque images (ignores source alpha, uses memcpy).
    pub fn blit_opaque(&self, dst: &mut [u8], dst_stride: u32, dst_x: u32, dst_y: u32) {
        for row in 0..self.height {
            let src_row_off = (row * self.width * 4) as usize;
            let dst_row_off = (((dst_y + row) * dst_stride + dst_x) * 4) as usize;

            if dst_row_off + (self.width * 4) as usize > dst.len() {
                continue;
            }

            let src_slice = &self.pixels[src_row_off..src_row_off + (self.width * 4) as usize];
            let dst_slice = &mut dst[dst_row_off..dst_row_off + (self.width * 4) as usize];
            dst_slice.copy_from_slice(src_slice);
        }
    }
}
 
/// Trait implemented by all image format decoders.
pub trait ImageDecoder {
    /// Attempt to decode `data` into a `DecodedImage`.
    fn decode(data: &[u8]) -> Result<DecodedImage, ImageError>;
}