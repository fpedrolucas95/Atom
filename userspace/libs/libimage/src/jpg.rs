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
    dc_pred: i16,
    blocks: Vec<[i16; 64]>,
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
                table[i] = if precision == 0 {
                    self.read_u8()? as u16
                } else {
                    self.read_u16()?
                };
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
            if code <= table.max_codes[i] {
                let idx = (table.val_ptr[i] as u32 + (code - table.min_codes[i])) as usize;
                if idx < table.values.len() {
                    return Ok(table.values[idx]);
                }
            }
        }
        Err(ImageError::CorruptData("Invalid Huffman code"))
    }

    fn receive_extend(&mut self, category: u8) -> Result<i16, ImageError> {
        if category == 0 { return Ok(0); }
        let vt = self.get_bits(category)?;
        let mut v = vt as i16;
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
             self.components[comp_idx].blocks = vec![[0i16; 64]; blocks_count];
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

                            let mut block = [0i16; 64];
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
                                for i in 0..64 { block[i] = block[i].wrapping_mul(qt[i] as i16); }
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

fn idct_8x8(b: &[i16; 64]) -> [i16; 64] {
    let mut out = [0i16; 64];
    let mut temp = [0i32; 64];

    // Constants for fixed-point IDCT (scaled by 2^13)
    const FIX_0_298631336: i32 = 2446;
    const FIX_0_390180644: i32 = 3196;
    const FIX_0_541196100: i32 = 4433;
    const FIX_0_765366865: i32 = 6270;
    const FIX_0_899976223: i32 = 7373;
    const FIX_1_175875602: i32 = 9633;
    const FIX_1_501321110: i32 = 12300;
    const FIX_1_847759065: i32 = 15137;
    const FIX_1_961570560: i32 = 16069;
    const FIX_2_053119869: i32 = 16819;
    const FIX_2_562915447: i32 = 20995;
    const FIX_3_072711035: i32 = 25172;

    // Horizontal pass
    for i in 0..8 {
        let row = &b[i * 8..(i + 1) * 8];
        if row[1] == 0 && row[2] == 0 && row[3] == 0 && row[4] == 0 && row[5] == 0 && row[6] == 0 && row[7] == 0 {
            let dc = (row[0] as i32) << 2;
            for j in 0..8 { temp[i * 8 + j] = dc; }
            continue;
        }

        let z2 = row[2] as i32;
        let z3 = row[6] as i32;
        let z1 = (z2 + z3) * FIX_0_541196100;
        let tmp2 = z1 + z3 * (-FIX_1_847759065);
        let tmp3 = z1 + z2 * FIX_0_765366865;

        let z2 = row[0] as i32;
        let z3 = row[4] as i32;
        let tmp0 = (z2 + z3) << 13;
        let tmp1 = (z2 - z3) << 13;

        let tmp10 = tmp0 + tmp3;
        let tmp13 = tmp0 - tmp3;
        let tmp11 = tmp1 + tmp2;
        let tmp12 = tmp1 - tmp2;

        let tmp4 = row[1] as i32;
        let tmp5 = row[3] as i32;
        let tmp6 = row[5] as i32;
        let tmp7 = row[7] as i32;

        let z1 = tmp4 + tmp7;
        let z2 = tmp5 + tmp6;
        let z3 = tmp4 + tmp5;
        let z4 = tmp6 + tmp7;
        let z5 = (z3 + z4) * FIX_1_175875602;

        let tmp4 = tmp4 * FIX_0_298631336;
        let tmp5 = tmp5 * FIX_2_053119869;
        let tmp6 = tmp6 * FIX_3_072711035;
        let tmp7 = tmp7 * FIX_1_501321110;
        let z3 = z3 * (-FIX_1_961570560);
        let z4 = z4 * (-FIX_0_390180644);
        let z3 = z3 + z5;
        let z4 = z4 + z5;

        let tmp4 = tmp4 + z1 * (-FIX_0_899976223) + z3;
        let tmp5 = tmp5 + z2 * (-FIX_2_562915447) + z4;
        let tmp6 = tmp6 + z2 * (-FIX_1_961570560) + z4;
        let tmp7 = tmp7 + z1 * (-FIX_0_390180644) + z3;

        temp[i * 8 + 0] = (tmp10 + tmp7 + (1 << 10)) >> 11;
        temp[i * 8 + 7] = (tmp10 - tmp7 + (1 << 10)) >> 11;
        temp[i * 8 + 1] = (tmp11 + tmp6 + (1 << 10)) >> 11;
        temp[i * 8 + 6] = (tmp11 - tmp6 + (1 << 10)) >> 11;
        temp[i * 8 + 2] = (tmp12 + tmp5 + (1 << 10)) >> 11;
        temp[i * 8 + 5] = (tmp12 - tmp5 + (1 << 10)) >> 11;
        temp[i * 8 + 3] = (tmp13 + tmp4 + (1 << 10)) >> 11;
        temp[i * 8 + 4] = (tmp13 - tmp4 + (1 << 10)) >> 11;
    }

    // Vertical pass
    for i in 0..8 {
        if temp[1*8+i] == 0 && temp[2*8+i] == 0 && temp[3*8+i] == 0 && temp[4*8+i] == 0 && temp[5*8+i] == 0 && temp[6*8+i] == 0 && temp[7*8+i] == 0 {
            let dc = (temp[i] + 4) >> 3;
            for j in 0..8 { out[j * 8 + i] = dc as i16; }
            continue;
        }

        let z2 = temp[2 * 8 + i];
        let z3 = temp[6 * 8 + i];
        let z1 = (z2 + z3) * FIX_0_541196100;
        let tmp2 = z1 + z3 * (-FIX_1_847759065);
        let tmp3 = z1 + z2 * FIX_0_765366865;

        let tmp0 = (temp[0 * 8 + i] + temp[4 * 8 + i]) << 13;
        let tmp1 = (temp[0 * 8 + i] - temp[4 * 8 + i]) << 13;

        let tmp10 = tmp0 + tmp3;
        let tmp13 = tmp0 - tmp3;
        let tmp11 = tmp1 + tmp2;
        let tmp12 = tmp1 - tmp2;

        let tmp4 = temp[1 * 8 + i];
        let tmp5 = temp[3 * 8 + i];
        let tmp6 = temp[5 * 8 + i];
        let tmp7 = temp[7 * 8 + i];

        let z1 = tmp4 + tmp7;
        let z2 = tmp5 + tmp6;
        let z3 = tmp4 + tmp5;
        let z4 = tmp6 + tmp7;
        let z5 = (z3 + z4) * FIX_1_175875602;

        let tmp4 = tmp4 * FIX_0_298631336;
        let tmp5 = tmp5 * FIX_2_053119869;
        let tmp6 = tmp6 * FIX_3_072711035;
        let tmp7 = tmp7 * FIX_1_501321110;
        let z3 = z3 * (-FIX_1_961570560);
        let z4 = z4 * (-FIX_0_390180644);
        let z3 = z3 + z5;
        let z4 = z4 + z5;

        let tmp4 = tmp4 + z1 * (-FIX_0_899976223) + z3;
        let tmp5 = tmp5 + z2 * (-FIX_2_562915447) + z4;
        let tmp6 = tmp6 + z2 * (-FIX_1_961570560) + z4;
        let tmp7 = tmp7 + z1 * (-FIX_0_390180644) + z3;

        out[0 * 8 + i] = ((tmp10 + tmp7 + (1 << 17)) >> 18) as i16;
        out[7 * 8 + i] = ((tmp10 - tmp7 + (1 << 17)) >> 18) as i16;
        out[1 * 8 + i] = ((tmp11 + tmp6 + (1 << 17)) >> 18) as i16;
        out[6 * 8 + i] = ((tmp11 - tmp6 + (1 << 17)) >> 18) as i16;
        out[2 * 8 + i] = ((tmp12 + tmp5 + (1 << 17)) >> 18) as i16;
        out[5 * 8 + i] = ((tmp12 - tmp5 + (1 << 17)) >> 18) as i16;
        out[3 * 8 + i] = ((tmp13 + tmp4 + (1 << 17)) >> 18) as i16;
        out[4 * 8 + i] = ((tmp13 - tmp4 + (1 << 17)) >> 18) as i16;
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
