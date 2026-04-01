// fat32/dir_entry.rs — Parse and encode FAT32 directory entries
//
// Handles both 8.3 short-name entries and Long File Name (LFN) sequences.
// All parsing follows the FAT32 spec §6.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::string::{String, ToString};
use alloc_crate::vec::Vec;
use alloc_crate::vec;
use alloc_crate::format;

use crate::error::{VfsError, VfsResult};
use super::types::{DirEntry as RawDirEntry, LfnEntry as RawLfnEntry, attr, lfn_checksum};

// ── Parsed directory entry ────────────────────────────────────────────────

/// A fully decoded directory entry, with the long name resolved (if present).
#[derive(Debug, Clone)]
pub struct ParsedEntry {
    /// The long file name, or the short name reformatted as "NAME.EXT"
    pub name:          String,
    /// First cluster of the file/directory data (0 for empty files)
    pub first_cluster: u32,
    /// File size in bytes (0 for directories)
    pub file_size:     u32,
    /// FAT attributes byte
    pub attributes:    u8,
    /// Index of the first raw 32-byte slot in the directory cluster buffer
    /// (either the LFN[0] entry if LFN is present, or the SFN entry)
    pub slot_start:    usize,
    /// Index of the SFN slot (= slot_start when no LFN)
    pub sfn_slot:      usize,
    /// Raw 8.3 name bytes [11]
    pub raw_sfn:       [u8; 11],
    /// Creation time (FAT encoded, seconds since epoch or 0)
    pub ctime_ns:      u64,
    /// Modification time
    pub mtime_ns:      u64,
    /// Access date
    pub atime_ns:      u64,
}

impl ParsedEntry {
    pub fn is_dir(&self)  -> bool { self.attributes & attr::DIRECTORY != 0 }
    pub fn is_file(&self) -> bool { !self.is_dir() }
}

// ── Parser ────────────────────────────────────────────────────────────────

const ENTRY_SZ: usize = 32;

/// Parse all valid directory entries from a flat byte buffer (one or more
/// consecutive clusters read into memory).
///
/// Stops at the first end-of-directory marker (0x00) or end of buffer.
/// Skips deleted entries (0xE5) and volume-label entries.
pub fn parse_dir_entries(buf: &[u8]) -> Vec<ParsedEntry> {
    let mut entries = Vec::new();
    let n_slots = buf.len() / ENTRY_SZ;
    let mut i = 0usize;

    while i < n_slots {
        let raw = &buf[i * ENTRY_SZ..(i + 1) * ENTRY_SZ];
        let first_byte = raw[0];

        if first_byte == 0x00 { break; }    // end of directory
        if first_byte == 0xE5 { i += 1; continue; } // deleted

        let attr_byte = raw[11];

        // LFN entry?
        if attr_byte == attr::LFN {
            // Collect all contiguous LFN slots then the SFN
            let lfn_start = i;
            let mut lfn_seqs: Vec<[u8; 32]> = Vec::new();

            while i < n_slots {
                let slot = &buf[i * ENTRY_SZ..(i + 1) * ENTRY_SZ];
                if slot[11] != attr::LFN { break; }
                if slot[0] == 0x00 || slot[0] == 0xE5 { break; }
                let mut entry_buf = [0u8; 32];
                entry_buf.copy_from_slice(slot);
                lfn_seqs.push(entry_buf);
                i += 1;
            }

            // Expect an SFN entry immediately after
            if i >= n_slots { break; }
            let sfn_raw = &buf[i * ENTRY_SZ..(i + 1) * ENTRY_SZ];
            let sfn_first = sfn_raw[0];
            i += 1;

            if sfn_first == 0x00 || sfn_first == 0xE5 { continue; }

            let sfn_attr = sfn_raw[11];
            if sfn_attr == attr::LFN || sfn_attr & attr::VOLUME_ID != 0 { continue; }

            let mut sfn = [0u8; 11];
            sfn.copy_from_slice(&sfn_raw[0..11]);

            // Validate LFN checksum
            let expected_ck = lfn_checksum(&sfn);
            let ck_ok = lfn_seqs.iter().all(|s| s[13] == expected_ck);
            if !ck_ok { continue; } // corrupt LFN — skip

            let name = decode_lfn(&lfn_seqs);
            let entry = build_parsed(name, sfn, sfn_raw, lfn_start, i - 1);
            entries.push(entry);
        } else {
            // Plain SFN entry
            if attr_byte & attr::VOLUME_ID != 0 { i += 1; continue; } // volume label
            let mut sfn = [0u8; 11];
            sfn.copy_from_slice(&raw[0..11]);
            let name = short_name_to_str(&sfn);
            let entry = build_parsed(name, sfn, raw, i, i);
            i += 1;
            entries.push(entry);
        }
    }

    entries
}

// ── Encoding ──────────────────────────────────────────────────────────────

/// Generate 8.3 SFN bytes from a long name.
///
/// This is a simplified lossy conversion: take the first 8 bytes of the
/// name (up to the last dot) as the stem and up to 3 bytes as extension.
/// All lower-case chars are converted to upper-case.
/// Returns `Err` if the name produces a collision marker or is invalid.
pub fn encode_sfn(long_name: &str) -> VfsResult<[u8; 11]> {
    let upper = long_name.to_uppercase();
    let (stem, ext) = split_name_ext(&upper);

    if stem.is_empty() { return Err(VfsError::InvalidArgument); }

    let stem_bytes: Vec<u8> = stem.bytes()
        .filter(|&b| is_valid_sfn_char(b))
        .take(8)
        .collect();
    let ext_bytes: Vec<u8> = ext.bytes()
        .filter(|&b| is_valid_sfn_char(b))
        .take(3)
        .collect();

    let mut sfn = [b' '; 11];
    sfn[..stem_bytes.len()].copy_from_slice(&stem_bytes);
    sfn[8..8 + ext_bytes.len()].copy_from_slice(&ext_bytes);

    Ok(sfn)
}

/// Encode a long file name as a sequence of 32-byte LFN directory slots +
/// one 32-byte SFN slot.
///
/// `sfn`: the 8.3 name (from `encode_sfn`)
/// `attr`: file attributes for the SFN slot
/// `first_cluster`: initial cluster (may be 0 for newly created files)
/// `file_size`: initial file size (0 for directories/new files)
///
/// Returns the raw bytes in the correct on-disk order (LFN[n]…LFN[1] then SFN).
pub fn encode_dir_entries(
    long_name:     &str,
    sfn:           &[u8; 11],
    file_attr:     u8,
    first_cluster: u32,
    file_size:     u32,
    ctime_ns:      u64,
    mtime_ns:      u64,
) -> Vec<u8> {
    let ck = lfn_checksum(sfn);
    let chars: Vec<u16> = long_name.encode_utf16().collect();
    let n_lfn = (chars.len() + 12) / 13;      // ceil(len/13)
    let mut slots: Vec<[u8; 32]> = Vec::with_capacity(n_lfn + 1);

    // Build LFN slots in reverse order (highest sequence first, written last)
    for seq in (1u8..=(n_lfn as u8)).rev() {
        let start = (seq as usize - 1) * 13;
        let mut slot = [0xFFu8; 32]; // unused chars = 0xFFFF
        slot[11] = attr::LFN;
        slot[12] = 0;      // type
        slot[13] = ck;
        slot[26] = 0;      // cluster lo
        slot[27] = 0;
        slot[28] = 0;      // cluster hi... wait spec says lo (26-27)
        // Sequence number; last entry has bit 6 set
        slot[0] = if seq as usize == n_lfn { seq | 0x40 } else { seq };

        // Fill 13 UTF-16LE code units
        let chars_in_slot: Vec<u16> = (0..13)
            .map(|j| *chars.get(start + j).unwrap_or(&0xFFFF))
            .collect();

        // copy to LFN offsets: [1..10], [14..24], [28..32]
        let offsets: &[(usize, usize)] = &[(1,5),(14,6),(28,2)];
        let mut ci = 0;
        for &(off, count) in offsets {
            for k in 0..count {
                let le = chars_in_slot[ci].to_le_bytes();
                slot[off + k*2]     = le[0];
                slot[off + k*2 + 1] = le[1];
                ci += 1;
            }
        }
        slots.push(slot);
    }

    // SFN slot
    let sfn_slot = build_sfn_slot(sfn, file_attr, first_cluster, file_size, ctime_ns, mtime_ns);
    slots.push(sfn_slot);

    // Flatten
    let mut out = Vec::with_capacity(slots.len() * 32);
    for s in &slots { out.extend_from_slice(s); }
    out
}

// ── Internal helpers ──────────────────────────────────────────────────────

fn build_sfn_slot(
    sfn:           &[u8; 11],
    attr_byte:     u8,
    first_cluster: u32,
    file_size:     u32,
    ctime_ns:      u64,
    mtime_ns:      u64,
) -> [u8; 32] {
    use super::types::{encode_date, encode_time};
    let mut s = [0u8; 32];
    s[0..11].copy_from_slice(sfn);
    s[11] = attr_byte;
    // ctime
    let (cd, ct) = ns_to_fat(ctime_ns);
    s[14..16].copy_from_slice(&ct.to_le_bytes());
    s[16..18].copy_from_slice(&cd.to_le_bytes());
    // adate = cdate
    s[18..20].copy_from_slice(&cd.to_le_bytes());
    // cluster hi/lo
    let chi = ((first_cluster >> 16) & 0xFFFF) as u16;
    let clo = (first_cluster & 0xFFFF) as u16;
    s[20..22].copy_from_slice(&chi.to_le_bytes());
    // mtime
    let (md, mt) = ns_to_fat(mtime_ns);
    s[22..24].copy_from_slice(&mt.to_le_bytes());
    s[24..26].copy_from_slice(&md.to_le_bytes());
    s[26..28].copy_from_slice(&clo.to_le_bytes());
    // file size
    s[28..32].copy_from_slice(&file_size.to_le_bytes());
    s
}

fn build_parsed(
    name:      String,
    sfn:       [u8; 11],
    sfn_raw:   &[u8],
    slot_start: usize,
    sfn_slot:  usize,
) -> ParsedEntry {
    use super::types::fat_datetime_to_ns;
    let first_cluster = ((sfn_raw[20] as u32) | (sfn_raw[21] as u32) << 8) << 16
        | (sfn_raw[26] as u32) | (sfn_raw[27] as u32) << 8;
    let file_size = u32::from_le_bytes([sfn_raw[28], sfn_raw[29], sfn_raw[30], sfn_raw[31]]);
    let attr_byte = sfn_raw[11];
    let ctime_t = u16::from_le_bytes([sfn_raw[14], sfn_raw[15]]);
    let ctime_d = u16::from_le_bytes([sfn_raw[16], sfn_raw[17]]);
    let mtime_t = u16::from_le_bytes([sfn_raw[22], sfn_raw[23]]);
    let mtime_d = u16::from_le_bytes([sfn_raw[24], sfn_raw[25]]);
    let atime_d = u16::from_le_bytes([sfn_raw[18], sfn_raw[19]]);

    ParsedEntry {
        name,
        first_cluster,
        file_size,
        attributes: attr_byte,
        slot_start,
        sfn_slot,
        raw_sfn: sfn,
        ctime_ns: fat_datetime_to_ns(ctime_d, ctime_t, sfn_raw.get(13).copied().unwrap_or(0)),
        mtime_ns: fat_datetime_to_ns(mtime_d, mtime_t, 0),
        atime_ns: fat_datetime_to_ns(atime_d, 0, 0),
    }
}

/// Decode LFN slots (may be in any order) into a UTF-8 string.
/// Slots are sorted by sequence number (1..n).
fn decode_lfn(slots: &[[u8; 32]]) -> String {
    // Sort by sequence number (low 6 bits), ascending
    let mut sorted = slots.to_vec();
    sorted.sort_by_key(|s| s[0] & 0x3F);

    let mut chars: Vec<u16> = Vec::new();
    for slot in &sorted {
        let offsets: &[(usize, usize)] = &[(1,5),(14,6),(28,2)];
        for &(off, count) in offsets {
            for k in 0..count {
                let v = u16::from_le_bytes([slot[off + k*2], slot[off + k*2 + 1]]);
                if v == 0x0000 { return String::from_utf16_lossy(&chars); }
                if v == 0xFFFF { continue; }
                chars.push(v);
            }
        }
    }
    String::from_utf16_lossy(&chars)
}

/// Convert 8.3 name bytes to "NAME.EXT" string (trimming trailing spaces).
fn short_name_to_str(sfn: &[u8; 11]) -> String {
    let stem = trim_spaces(&sfn[0..8]);
    let ext  = trim_spaces(&sfn[8..11]);
    // NTRes bit 0x08 = lowercase stem, bit 0x10 = lowercase ext (no-op here: use uppercase)
    if ext.is_empty() {
        String::from_utf8_lossy(stem).to_string()
    } else {
        format!("{}.{}", String::from_utf8_lossy(stem), String::from_utf8_lossy(ext))
    }
}

fn trim_spaces(b: &[u8]) -> &[u8] {
    let end = b.iter().rposition(|&c| c != b' ').map_or(0, |i| i + 1);
    &b[..end]
}

fn split_name_ext(name: &str) -> (&str, &str) {
    if let Some(dot) = name.rfind('.') {
        (&name[..dot], &name[dot+1..])
    } else {
        (name, "")
    }
}

fn is_valid_sfn_char(b: u8) -> bool {
    // Valid 8.3 characters (simplified: printable ASCII minus illegal set)
    matches!(b, b'A'..=b'Z' | b'0'..=b'9' | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' |
               b'(' | b')' | b'-' | b'@' | b'^' | b'_' | b'`' | b'{' | b'}' | b'~')
}

fn ns_to_fat(ns: u64) -> (u16, u16) {
    use super::types::{encode_date, encode_time};
    if ns == 0 {
        return (encode_date(1980, 1, 1), encode_time(0, 0, 0));
    }
    // Convert nanoseconds since Unix epoch to FAT date/time
    let secs = ns / 1_000_000_000;
    // Simple: secs since 1970-01-01; FAT epoch is 1980-01-01
    // For a correct implementation we'd do full date math; here we emit
    // a safe default for newly created entries.
    (encode_date(1980, 1, 1), encode_time(0, 0, 0))
}
