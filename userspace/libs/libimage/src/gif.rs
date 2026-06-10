//! GIF decoder (GIF87a / GIF89a).
//!
//! Decodes the first image frame of a GIF into RGBA8888. Supports:
//! - Global and local colour tables
//! - Transparency via the Graphic Control Extension
//! - Interlaced frames (4-pass deinterlacing)
//!
//! Animation is intentionally not handled: only the first frame is rendered,
//! which is what a static document view needs. Compression is LZW (the GIF
//! variant, LSB-first), decoded with a fixed prefix/suffix table so no
//! per-code heap allocation occurs.

use alloc::vec;
use alloc::vec::Vec;

use crate::image::{DecodedImage, ImageDecoder, ImageError};

/// Reject frames larger than this many pixels to bound memory use.
const MAX_PIXELS: u64 = 8_000_000;

pub struct GifDecoder;

impl ImageDecoder for GifDecoder {
    fn decode(data: &[u8]) -> Result<DecodedImage, ImageError> {
        GifReader::new(data)?.decode_first_frame()
    }
}

/// An RGB colour table (palette).
struct Palette {
    /// Packed RGB triples: `[r, g, b, r, g, b, ...]`.
    colors: Vec<u8>,
}

impl Palette {
    fn len(&self) -> usize {
        self.colors.len() / 3
    }

    /// RGB of entry `idx`, or black if out of range.
    fn rgb(&self, idx: usize) -> (u8, u8, u8) {
        if idx < self.len() {
            let o = idx * 3;
            (self.colors[o], self.colors[o + 1], self.colors[o + 2])
        } else {
            (0, 0, 0)
        }
    }
}

struct GifReader<'a> {
    data: &'a [u8],
    pos: usize,
    global_palette: Option<Palette>,
    transparent_index: Option<u8>,
}

impl<'a> GifReader<'a> {
    fn new(data: &'a [u8]) -> Result<Self, ImageError> {
        // Header: "GIF87a" or "GIF89a".
        if data.len() < 13 || &data[0..3] != b"GIF" {
            return Err(ImageError::InvalidSignature);
        }
        if &data[3..6] != b"87a" && &data[3..6] != b"89a" {
            return Err(ImageError::InvalidSignature);
        }

        let mut reader = Self {
            data,
            pos: 6,
            global_palette: None,
            transparent_index: None,
        };

        // Logical Screen Descriptor (7 bytes): width, height, packed, bg, aspect.
        let _screen_w = reader.read_u16()?;
        let _screen_h = reader.read_u16()?;
        let packed = reader.read_u8()?;
        let _bg_index = reader.read_u8()?;
        let _aspect = reader.read_u8()?;

        if packed & 0x80 != 0 {
            let size = 2usize << (packed & 0x07);
            reader.global_palette = Some(reader.read_palette(size)?);
        }
        Ok(reader)
    }

    // ── Byte-level reads ────────────────────────────────────────────────────

    fn read_u8(&mut self) -> Result<u8, ImageError> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or(ImageError::CorruptData("unexpected EOF"))?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u16(&mut self) -> Result<u16, ImageError> {
        let lo = self.read_u8()? as u16;
        let hi = self.read_u8()? as u16;
        Ok(lo | (hi << 8))
    }

    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], ImageError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or(ImageError::CorruptData("truncated"))?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_palette(&mut self, entries: usize) -> Result<Palette, ImageError> {
        let bytes = self.read_bytes(entries * 3)?;
        Ok(Palette {
            colors: bytes.to_vec(),
        })
    }

    /// Skip a chain of length-prefixed sub-blocks (terminated by a 0 length).
    fn skip_sub_blocks(&mut self) -> Result<(), ImageError> {
        loop {
            let len = self.read_u8()? as usize;
            if len == 0 {
                return Ok(());
            }
            self.read_bytes(len)?;
        }
    }

    /// Concatenate a chain of length-prefixed sub-blocks into one buffer.
    fn read_sub_blocks(&mut self) -> Result<Vec<u8>, ImageError> {
        let mut out = Vec::new();
        loop {
            let len = self.read_u8()? as usize;
            if len == 0 {
                return Ok(out);
            }
            out.extend_from_slice(self.read_bytes(len)?);
        }
    }

    // ── Block stream ────────────────────────────────────────────────────────

    fn decode_first_frame(&mut self) -> Result<DecodedImage, ImageError> {
        loop {
            match self.read_u8()? {
                // Extension introducer.
                0x21 => {
                    let label = self.read_u8()?;
                    if label == 0xF9 {
                        self.parse_graphic_control()?;
                    } else {
                        self.skip_sub_blocks()?;
                    }
                }
                // Image descriptor — decode and return.
                0x2C => return self.decode_image_block(),
                // Trailer / unknown — no frame.
                0x3B => return Err(ImageError::CorruptData("no image frame")),
                _ => return Err(ImageError::CorruptData("bad block")),
            }
        }
    }

    /// Graphic Control Extension carries the transparent colour index.
    fn parse_graphic_control(&mut self) -> Result<(), ImageError> {
        let block_size = self.read_u8()?;
        if block_size != 4 {
            // Tolerate by skipping the rest as sub-blocks.
            self.pos -= 1;
            return self.skip_sub_blocks();
        }
        let packed = self.read_u8()?;
        let _delay = self.read_u16()?;
        let transparent = self.read_u8()?;
        let _terminator = self.read_u8()?;
        self.transparent_index = (packed & 0x01 != 0).then_some(transparent);
        Ok(())
    }

    fn decode_image_block(&mut self) -> Result<DecodedImage, ImageError> {
        let _left = self.read_u16()?;
        let _top = self.read_u16()?;
        let width = self.read_u16()? as usize;
        let height = self.read_u16()? as usize;
        let packed = self.read_u8()?;

        if width == 0 || height == 0 {
            return Err(ImageError::CorruptData("empty frame"));
        }
        if (width as u64) * (height as u64) > MAX_PIXELS {
            return Err(ImageError::UnsupportedFormat("image too large"));
        }

        let has_local = packed & 0x80 != 0;
        let interlaced = packed & 0x40 != 0;
        let local_palette = if has_local {
            let size = 2usize << (packed & 0x07);
            Some(self.read_palette(size)?)
        } else {
            None
        };

        let min_code_size = self.read_u8()?;
        if !(2..=8).contains(&min_code_size) {
            return Err(ImageError::CorruptData("bad LZW code size"));
        }
        let lzw_data = self.read_sub_blocks()?;
        let indices = lzw_decode(&lzw_data, min_code_size, width * height)?;

        // Resolve the palette only now that all mutable reads are done.
        let palette = local_palette
            .as_ref()
            .or(self.global_palette.as_ref())
            .ok_or(ImageError::CorruptData("no colour table"))?;

        Ok(self.compose(&indices, width, height, interlaced, palette))
    }

    /// Map palette indices to RGBA8888, deinterlacing if needed.
    fn compose(
        &self,
        indices: &[u8],
        width: usize,
        height: usize,
        interlaced: bool,
        palette: &Palette,
    ) -> DecodedImage {
        let mut pixels = vec![0u8; width * height * 4];
        for (storage_row, dst_row) in row_order(height, interlaced).enumerate() {
            let src_base = storage_row * width;
            if src_base >= indices.len() {
                break;
            }
            for col in 0..width {
                let Some(&index) = indices.get(src_base + col) else {
                    break;
                };
                let (r, g, b) = palette.rgb(index as usize);
                let transparent = self.transparent_index == Some(index);
                let o = (dst_row * width + col) * 4;
                pixels[o] = r;
                pixels[o + 1] = g;
                pixels[o + 2] = b;
                pixels[o + 3] = if transparent { 0 } else { 255 };
            }
        }
        DecodedImage::new(width as u32, height as u32, pixels)
    }
}

/// Iterate destination rows in the order frame data is stored. For progressive
/// (non-interlaced) frames this is simply `0..height`; interlaced frames follow
/// the GIF 4-pass scheme.
fn row_order(height: usize, interlaced: bool) -> impl Iterator<Item = usize> {
    // Collect into a Vec so both branches share one iterator type.
    let mut rows = Vec::with_capacity(height);
    if interlaced {
        for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
            let mut r = start;
            while r < height {
                rows.push(r);
                r += step;
            }
        }
    } else {
        rows.extend(0..height);
    }
    rows.into_iter()
}

// ────────────────────────────────────────────────────────────────────────────
// LZW decompression (GIF variant: LSB-first, variable code width 2..=12)
// ────────────────────────────────────────────────────────────────────────────

/// A LSB-first bit reader over the concatenated LZW sub-block data.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            buffer: 0,
            bits: 0,
        }
    }

    fn read(&mut self, count: u32) -> Option<usize> {
        while self.bits < count {
            let &byte = self.data.get(self.pos)?;
            self.pos += 1;
            self.buffer |= (byte as u32) << self.bits;
            self.bits += 8;
        }
        let mask = (1u32 << count) - 1;
        let value = (self.buffer & mask) as usize;
        self.buffer >>= count;
        self.bits -= count;
        Some(value)
    }
}

fn lzw_decode(data: &[u8], min_code_size: u8, expected: usize) -> Result<Vec<u8>, ImageError> {
    const MAX_CODES: usize = 4096;
    let clear_code = 1usize << min_code_size;
    let eoi_code = clear_code + 1;

    let mut prefix = [0u16; MAX_CODES];
    let mut suffix = [0u8; MAX_CODES];
    for (i, s) in suffix.iter_mut().enumerate().take(clear_code) {
        *s = i as u8;
    }

    let mut stack = [0u8; MAX_CODES];
    let mut out = Vec::with_capacity(expected);
    let mut reader = BitReader::new(data);

    let mut code_size = min_code_size as u32 + 1;
    let mut next_code = clear_code + 2;
    let mut old_code: Option<usize> = None;
    let mut first_byte: u8 = 0;

    while let Some(code) = reader.read(code_size) {
        if code == clear_code {
            code_size = min_code_size as u32 + 1;
            next_code = clear_code + 2;
            old_code = None;
            continue;
        }
        if code == eoi_code {
            break;
        }

        let Some(prev) = old_code else {
            // First code after a clear: emit it as a literal.
            if code >= clear_code {
                return Err(ImageError::DecompressionFailed);
            }
            first_byte = suffix[code];
            out.push(first_byte);
            old_code = Some(code);
            continue;
        };

        let mut sp = 0usize;
        let mut cur = code;
        if cur >= next_code {
            // KwKwK: code not yet in the table.
            stack[sp] = first_byte;
            sp += 1;
            cur = prev;
        }
        while cur >= clear_code {
            if sp >= MAX_CODES || cur >= MAX_CODES {
                return Err(ImageError::DecompressionFailed);
            }
            stack[sp] = suffix[cur];
            sp += 1;
            cur = prefix[cur] as usize;
        }
        first_byte = suffix[cur];
        stack[sp] = first_byte;
        sp += 1;
        while sp > 0 {
            sp -= 1;
            out.push(stack[sp]);
        }

        if next_code < MAX_CODES {
            prefix[next_code] = prev as u16;
            suffix[next_code] = first_byte;
            next_code += 1;
            if next_code == (1usize << code_size) && code_size < 12 {
                code_size += 1;
            }
        }
        old_code = Some(code);

        if out.len() >= expected {
            break;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::image::ImageError;
    use std::vec::Vec;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decode_basic_frame() {
        // 4x2, palette [red, green, blue], indices [0,1,2,0, 2,1,0,1].
        let bytes = unhex(
            "47494638396104000200810000ff000000ff000000ff0000002c00000000040\
             0020000080b0001041000404000810101003b",
        );
        let img = GifDecoder::decode(&bytes).expect("valid gif");
        assert_eq!((img.width, img.height), (4, 2));
        let pal = [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255)];
        let idx = [0usize, 1, 2, 0, 2, 1, 0, 1];
        for (i, &p) in idx.iter().enumerate() {
            let (r, g, b, a) = img.get_pixel((i % 4) as u32, (i / 4) as u32).unwrap();
            assert_eq!((r, g, b, a), (pal[p].0, pal[p].1, pal[p].2, 255));
        }
    }

    #[test]
    fn decode_interlaced_with_transparency() {
        // 4x5 interlaced; transparent index 0; index = (x + y) % 2.
        let bytes = unhex(
            "474946383961040005008000000a141e28323c21f90401000000002c0000000\
             00400050040080d000104104870a0418305010404003b",
        );
        let img = GifDecoder::decode(&bytes).expect("valid gif");
        assert_eq!((img.width, img.height), (4, 5));
        for y in 0..5u32 {
            for x in 0..4u32 {
                let (r, g, b, a) = img.get_pixel(x, y).unwrap();
                if (x + y) % 2 == 0 {
                    // index 0 → transparent
                    assert_eq!(a, 0, "pixel {x},{y} should be transparent");
                    assert_eq!((r, g, b), (10, 20, 30));
                } else {
                    assert_eq!((r, g, b, a), (40, 50, 60, 255));
                }
            }
        }
    }

    #[test]
    fn rejects_non_gif() {
        assert_eq!(
            GifDecoder::decode(b"\x89PNG\r\n\x1a\n").unwrap_err(),
            ImageError::InvalidSignature
        );
    }
}
