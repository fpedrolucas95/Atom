//! Baseline JPEG decoder for Atom OS (libimage)
//!
//! Supports SOF0 (Baseline DCT), Huffman coding, YCbCr 4:4:4, 4:2:2, 4:2:0.
//! Implementation uses fixed-point integer arithmetic for IDCT.

use alloc::vec::Vec;
use alloc::vec;
use crate::image::{DecodedImage, ImageDecoder, ImageError};

pub struct JpgDecoder;

impl ImageDecoder for JpgDecoder {
    fn decode(data: &[u8]) -> Result<DecodedImage, ImageError> {
        let mut decoder = JpgInternalDecoder::new(data);
        decoder.decode()
    }
}

struct JpgInternalDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    width: u16,
    height: u16,
    components: Vec<Component>,
    huffman_tables_dc: [Option<HuffmanTable>; 4],
    huffman_tables_ac: [Option<HuffmanTable>; 4],
    quantization_tables: [Option<[u16; 64]>; 4],
    restart_interval: u16,
    bit_buffer: u32,
    bits_left: u8,
}

struct Component {
    id: u8,
    h_samp: u8,
    v_samp: u8,
    qt_id: u8,
    dc_table_id: u8,
    ac_table_id: u8,
    dc_pred: i32,
    blocks: Vec<[i32; 64]>,
}

#[derive(Clone)]
struct HuffmanTable {
    min_codes: [u32; 17],
    max_codes: [u32; 17],
    val_ptr: [u16; 17],
    values: Vec<u8>,
}

const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

impl<'a> JpgInternalDecoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            width: 0,
            height: 0,
            components: Vec::new(),
            huffman_tables_dc: [None, None, None, None],
            huffman_tables_ac: [None, None, None, None],
            quantization_tables: [None, None, None, None],
            restart_interval: 0,
            bit_buffer: 0,
            bits_left: 0,
        }
    }

    fn read_u8(&mut self) -> Result<u8, ImageError> {
        if self.pos >= self.data.len() {
            return Err(ImageError::CorruptData("Unexpected EOF"));
        }
        let val = self.data[self.pos];
        self.pos += 1;
        Ok(val)
    }

    fn read_u16(&mut self) -> Result<u16, ImageError> {
        let h = self.read_u8()? as u16;
        let l = self.read_u8()? as u16;
        Ok((h << 8) | l)
    }

    fn decode(&mut self) -> Result<DecodedImage, ImageError> {
        if self.read_u8()? != 0xFF || self.read_u8()? != 0xD8 {
            return Err(ImageError::InvalidSignature);
        }

        loop {
            let mut marker = self.read_u8()?;
            while marker != 0xFF { marker = self.read_u8()?; }
            while marker == 0xFF { marker = self.read_u8()?; }

            match marker {
                0xD9 => break, // EOI
                0xC0 => self.parse_sof0()?,
                0xC4 => self.parse_dht()?,
                0xDB => self.parse_dqt()?,
                0xDD => self.parse_dri()?,
                0xDA => {
                    self.parse_sos()?;
                    break;
                }
                0xE0..=0xEF | 0xFE => {
                    let len = self.read_u16()?;
                    self.pos += (len as usize).saturating_sub(2);
                }
                _ => {}
            }
        }

        if self.width == 0 || self.height == 0 || self.components.is_empty() {
             return Err(ImageError::CorruptData("Incomplete JPEG data"));
        }

        self.render()
    }

    fn parse_sof0(&mut self) -> Result<(), ImageError> {
        let _len = self.read_u16()?;
        let precision = self.read_u8()?;
        if precision != 8 {
            return Err(ImageError::UnsupportedFormat("Only 8-bit precision supported"));
        }
        self.height = self.read_u16()?;
        self.width = self.read_u16()?;
        let num_components = self.read_u8()?;
        for _ in 0..num_components {
            let id = self.read_u8()?;
            let samp = self.read_u8()?;
            let h_samp = samp >> 4;
            let v_samp = samp & 0x0F;
            let qt_id = self.read_u8()?;
            self.components.push(Component {
                id, h_samp, v_samp, qt_id,
                dc_table_id: 0, ac_table_id: 0,
                dc_pred: 0,
                blocks: Vec::new(),
            });
        }
        Ok(())
    }

    fn parse_dqt(&mut self) -> Result<(), ImageError> {
        let len = self.read_u16()? as usize - 2;
        let end = self.pos + len;
        while self.pos < end {
            let info = self.read_u8()?;
            let id = (info & 0x0F) as usize;
            let precision = info >> 4;
            if id >= 4 { return Err(ImageError::CorruptData("Invalid QT ID")); }
            let mut table = [0u16; 64];
            for i in 0..64 {
                let value = if precision == 0 {
                    self.read_u8()? as u16
                } else {
                    self.read_u16()?
                };
                table[ZIGZAG[i]] = value;
            }
            self.quantization_tables[id] = Some(table);
        }
        Ok(())
    }

    fn parse_dht(&mut self) -> Result<(), ImageError> {
        let len = self.read_u16()? as usize - 2;
        let end = self.pos + len;
        while self.pos < end {
            let info = self.read_u8()?;
            let id = (info & 0x0F) as usize;
            let table_class = info >> 4;
            if id >= 4 { return Err(ImageError::CorruptData("Invalid DHT ID")); }

            let mut counts = [0u8; 16];
            for i in 0..16 { counts[i] = self.read_u8()?; }
            let total: usize = counts.iter().map(|&c| c as usize).sum();
            let mut values = vec![0u8; total];
            for i in 0..total { values[i] = self.read_u8()?; }

            let table = self.build_huffman_table(&counts, values);
            if table_class == 0 {
                self.huffman_tables_dc[id] = Some(table);
            } else {
                self.huffman_tables_ac[id] = Some(table);
            }
        }
        Ok(())
    }

    fn build_huffman_table(&self, counts: &[u8; 16], values: Vec<u8>) -> HuffmanTable {
        let mut h = HuffmanTable {
            min_codes: [0; 17],
            max_codes: [0xFFFFFFFF; 17],
            val_ptr: [0; 17],
            values,
        };
        let mut code = 0u32;
        let mut k = 0;
        for i in 1..=16 {
            h.val_ptr[i] = k as u16;
            if counts[i-1] > 0 {
                h.min_codes[i] = code;
                code += counts[i-1] as u32;
                h.max_codes[i] = code - 1;
            } else {
                h.max_codes[i] = 0xFFFFFFFF;
            }
            code <<= 1;
            k += counts[i-1] as usize;
        }
        h
    }

    fn parse_dri(&mut self) -> Result<(), ImageError> {
        let _len = self.read_u16()?;
        self.restart_interval = self.read_u16()?;
        Ok(())
    }

    fn parse_sos(&mut self) -> Result<(), ImageError> {
        let _len = self.read_u16()?;
        let num_components = self.read_u8()?;
        let mut scan_components = Vec::new();
        for _ in 0..num_components {
            let id = self.read_u8()?;
            let table_ids = self.read_u8()?;
            if let Some(comp_idx) = self.components.iter().position(|c| c.id == id) {
                self.components[comp_idx].dc_table_id = table_ids >> 4;
                self.components[comp_idx].ac_table_id = table_ids & 0x0F;
                scan_components.push(comp_idx);
            }
        }
        let _ss = self.read_u8()?;
        let _se = self.read_u8()?;
        let _ah_al = self.read_u8()?;

        self.decode_scan(&scan_components)
    }

    fn next_bit(&mut self) -> Result<u8, ImageError> {
        if self.bits_left == 0 {
            let b = self.read_u8()?;
            self.bit_buffer = b as u32;
            self.bits_left = 8;
            if b == 0xFF {
                let next = self.read_u8()?;
                if next != 0x00 {
                    // Marker encountered.
                }
            }
        }
        self.bits_left -= 1;
        Ok(((self.bit_buffer >> self.bits_left) & 1) as u8)
    }

    fn get_bits(&mut self, n: u8) -> Result<u16, ImageError> {
        let mut res = 0u16;
        for _ in 0..n {
            res = (res << 1) | self.next_bit()? as u16;
        }
        Ok(res)
    }

    fn decode_huffman(&mut self, table_idx: usize, is_ac: bool) -> Result<u8, ImageError> {
        let mut code = 0u32;
        for i in 1..=16 {
            code = (code << 1) | self.next_bit()? as u32;
            let table = if is_ac {
                self.huffman_tables_ac[table_idx].as_ref()
            } else {
                self.huffman_tables_dc[table_idx].as_ref()
            }.ok_or(ImageError::CorruptData("Missing Huffman table"))?;
            if table.max_codes[i] != 0xFFFFFFFF
                && code >= table.min_codes[i]
                && code <= table.max_codes[i]
            {
                let idx = (table.val_ptr[i] as u32 + (code - table.min_codes[i])) as usize;
                if idx < table.values.len() {
                    return Ok(table.values[idx]);
                }
            }
        }
        Err(ImageError::CorruptData("Invalid Huffman code"))
    }

    fn receive_extend(&mut self, category: u8) -> Result<i32, ImageError> {
        if category == 0 { return Ok(0); }
        let vt = self.get_bits(category)?;
        let mut v = vt as i32;
        if v < (1 << (category - 1)) {
            v -= (1 << category) - 1;
        }
        Ok(v)
    }

    fn decode_scan(&mut self, scan_components: &[usize]) -> Result<(), ImageError> {
        let max_h = self.components.iter().map(|c| c.h_samp).max().unwrap_or(1);
        let max_v = self.components.iter().map(|c| c.v_samp).max().unwrap_or(1);
        let mcus_x = (self.width as usize + (max_h as usize * 8) - 1) / (max_h as usize * 8);
        let mcus_y = (self.height as usize + (max_v as usize * 8) - 1) / (max_v as usize * 8);

        for comp_idx in 0..self.components.len() {
             let c = &self.components[comp_idx];
             let blocks_count = (mcus_x * c.h_samp as usize) * (mcus_y * c.v_samp as usize);
             self.components[comp_idx].blocks = vec![[0i32; 64]; blocks_count];
        }

        for my in 0..mcus_y {
            for mx in 0..mcus_x {
                for &comp_idx in scan_components {
                    let h_samp = self.components[comp_idx].h_samp as usize;
                    let v_samp = self.components[comp_idx].v_samp as usize;
                    for vy in 0..v_samp {
                        for hx in 0..h_samp {
                            let block_x = mx * h_samp + hx;
                            let block_y = my * v_samp + vy;
                            let block_idx = block_y * (mcus_x * h_samp) + block_x;

                            let mut block = [0i32; 64];
                            let s = self.decode_huffman(self.components[comp_idx].dc_table_id as usize, false)?;
                            let diff = self.receive_extend(s)?;
                            self.components[comp_idx].dc_pred += diff;
                            block[0] = self.components[comp_idx].dc_pred;

                            let mut k = 1usize;
                            while k < 64 {
                                let rs = self.decode_huffman(self.components[comp_idx].ac_table_id as usize, true)?;
                                let r = rs >> 4;
                                let s = rs & 0x0F;
                                if s == 0 { if r == 15 { k += 16; } else { break; } }
                                else { k += r as usize; if k < 64 { block[ZIGZAG[k]] = self.receive_extend(s)?; k += 1; } }
                            }

                            if let Some(qt) = self.quantization_tables[self.components[comp_idx].qt_id as usize] {
                                for i in 0..64 { block[i] *= qt[i] as i32; }
                            }
                            self.components[comp_idx].blocks[block_idx] = block;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn render(&self) -> Result<DecodedImage, ImageError> {
        let mut pixels = vec![0u8; self.width as usize * self.height as usize * 4];
        let max_h = self.components.iter().map(|c| c.h_samp).max().unwrap_or(1) as usize;
        let max_v = self.components.iter().map(|c| c.v_samp).max().unwrap_or(1) as usize;
        let mcus_x = (self.width as usize + (max_h * 8) - 1) / (max_h * 8);

        let mut block_buffers = Vec::new();
        for c in &self.components {
            let mut decoded_blocks = vec![[0i16; 64]; c.blocks.len()];
            for (i, block) in c.blocks.iter().enumerate() {
                decoded_blocks[i] = idct_8x8(block);
            }
            block_buffers.push(decoded_blocks);
        }

        for y in 0..self.height as usize {
            for x in 0..self.width as usize {
                let mut yuv = [128i32; 3];
                for (i, c) in self.components.iter().enumerate() {
                    let bx = (x * c.h_samp as usize / max_h) / 8;
                    let by = (y * c.v_samp as usize / max_v) / 8;
                    let lx = (x * c.h_samp as usize / max_h) % 8;
                    let ly = (y * c.v_samp as usize / max_v) % 8;
                    let block_idx = by * (mcus_x * c.h_samp as usize) + bx;
                    if block_idx < block_buffers[i].len() {
                        yuv[i] = block_buffers[i][block_idx][ly * 8 + lx] as i32 + 128;
                    }
                }
                let r = yuv[0] + ((1402 * (yuv[2] - 128)) / 1000);
                let g = yuv[0] - ((344 * (yuv[1] - 128)) / 1000) - ((714 * (yuv[2] - 128)) / 1000);
                let b = yuv[0] + ((1772 * (yuv[1] - 128)) / 1000);
                let off = (y * self.width as usize + x) * 4;
                pixels[off]   = r.clamp(0, 255) as u8;
                pixels[off+1] = g.clamp(0, 255) as u8;
                pixels[off+2] = b.clamp(0, 255) as u8;
                pixels[off+3] = 255;
            }
        }
        Ok(DecodedImage::new(self.width as u32, self.height as u32, pixels))
    }
}

fn idct_8x8(b: &[i32; 64]) -> [i16; 64] {
    const BASIS: [[i32; 8]; 8] = [
        [11585, 11585, 11585, 11585, 11585, 11585, 11585, 11585],
        [16069, 13623, 9102, 3196, -3196, -9102, -13623, -16069],
        [15137, 6270, -6270, -15137, -15137, -6270, 6270, 15137],
        [13623, -3196, -16069, -9102, 9102, 16069, 3196, -13623],
        [11585, -11585, -11585, 11585, 11585, -11585, -11585, 11585],
        [9102, -16069, 3196, 13623, -13623, -3196, 16069, -9102],
        [6270, -15137, 15137, -6270, -6270, 15137, -15137, 6270],
        [3196, -9102, 13623, -16069, 16069, -13623, 9102, -3196],
    ];

    let mut out = [0i16; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0i64;
            for v in 0..8 {
                for u in 0..8 {
                    sum += b[v * 8 + u] as i64
                        * BASIS[u][x] as i64
                        * BASIS[v][y] as i64;
                }
            }
            let value = ((sum + (1 << 29)) >> 30)
                .clamp(i16::MIN as i64, i16::MAX as i64) as i16;
            out[y * 8 + x] = value;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_invalid_signature() {
        let data = [0u8; 10];
        assert_eq!(JpgDecoder::decode(&data).err(), Some(ImageError::InvalidSignature));
    }
}
