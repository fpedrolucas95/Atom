// fat32/types.rs — On-disk FAT32 structures
//
// Every struct here is #[repr(C, packed)] and matches the FAT32 spec
// (Microsoft FAT32 File System Specification v1.03) byte-for-byte.
//
// IMPORTANT: All fields are little-endian on disk (x86 native).
// Do not read these structs on big-endian architectures without byte-swapping.
//
// SAFETY: These structs are only constructed by casting aligned byte slices
// produced by the block client.  Direct field access on packed structs must
// use copy semantics (field.to_le_bytes(), or a read helper) to avoid
// UB from unaligned references.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::vec;
use alloc_crate::vec::Vec;
use alloc_crate::string::String;

// ── BPB — common region (offsets 0-35) ──────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct BpbCommon {
    pub bs_jmp_boot:      [u8; 3],   // 0
    pub bs_oem_name:      [u8; 8],   // 3
    pub bpb_byts_per_sec: u16,       // 11  — 512/1024/2048/4096
    pub bpb_sec_per_clus: u8,        // 13  — power of 2, ≤128
    pub bpb_rsvd_sec_cnt: u16,       // 14  — ≥1; FAT32 typically 32
    pub bpb_num_fats:     u8,        // 16  — should be 2
    pub bpb_root_ent_cnt: u16,       // 17  — must be 0 for FAT32
    pub bpb_tot_sec16:    u16,       // 19  — must be 0 for FAT32
    pub bpb_media:        u8,        // 21
    pub bpb_fat_sz16:     u16,       // 22  — must be 0 for FAT32
    pub bpb_sec_per_trk:  u16,       // 24
    pub bpb_num_heads:    u16,       // 26
    pub bpb_hidd_sec:     u32,       // 28
    pub bpb_tot_sec32:    u32,       // 32  — total sectors on volume
}

// ── BPB FAT32 extension (offsets 36-89) ──────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct BpbFat32Ext {
    pub bpb_fat_sz32:   u32,        // 36  — sectors occupied by ONE FAT
    pub bpb_ext_flags:  u16,        // 40  — bit7=mirroring, bits0-3=active FAT
    pub bpb_fs_ver:     u16,        // 42  — must be 0x0000
    pub bpb_root_clus:  u32,        // 44  — first cluster of root dir (usually 2)
    pub bpb_fs_info:    u16,        // 48  — FSInfo sector number (usually 1)
    pub bpb_bk_boot_sec: u16,       // 50  — backup boot sector (usually 6)
    pub bpb_reserved:   [u8; 12],   // 52
    pub bs_drv_num:     u8,         // 64
    pub bs_reserved1:   u8,         // 65
    pub bs_boot_sig:    u8,         // 66  — 0x29 = next 3 fields are valid
    pub bs_vol_id:      u32,        // 67
    pub bs_vol_lab:     [u8; 11],   // 71
    pub bs_fil_sys_type: [u8; 8],   // 82  — "FAT32   " (informational only)
}

/// Combined 512-byte boot sector.  Only the first 90 bytes matter for FAT32.
#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct BootSector {
    pub common: BpbCommon,      // offsets 0-35
    pub ext:    BpbFat32Ext,    // offsets 36-89
    pub _pad:   [u8; 420],      // offsets 90-509
    pub sig1:   u8,             // 510  — must be 0x55
    pub sig2:   u8,             // 511  — must be 0xAA
}

const _: () = assert!(core::mem::size_of::<BootSector>() == 512);

// ── FSInfo sector (spec p.21-22) ─────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct FsInfoSector {
    pub fsi_lead_sig:   u32,        // 0    — 0x41615252
    pub fsi_reserved1:  [u8; 480],  // 4
    pub fsi_struc_sig:  u32,        // 484  — 0x61417272
    pub fsi_free_count: u32,        // 488  — 0xFFFFFFFF = unknown
    pub fsi_nxt_free:   u32,        // 492  — 0xFFFFFFFF = no hint
    pub fsi_reserved2:  [u8; 12],   // 496
    pub fsi_trail_sig:  u32,        // 508  — 0xAA550000
}

const _: () = assert!(core::mem::size_of::<FsInfoSector>() == 512);

impl FsInfoSector {
    pub const LEAD_SIG:  u32 = 0x4161_5252;
    pub const STRUC_SIG: u32 = 0x6141_7272;
    pub const TRAIL_SIG: u32 = 0xAA55_0000;
    pub const UNKNOWN:   u32 = 0xFFFF_FFFF;

    pub fn is_valid(&self) -> bool {
        // Read packed fields safely
        let lead  = u32::from_le_bytes(unsafe { core::ptr::read_unaligned(
            core::ptr::addr_of!(self.fsi_lead_sig)) }.to_le_bytes());
        let struc = u32::from_le_bytes(unsafe { core::ptr::read_unaligned(
            core::ptr::addr_of!(self.fsi_struc_sig)) }.to_le_bytes());
        let trail = u32::from_le_bytes(unsafe { core::ptr::read_unaligned(
            core::ptr::addr_of!(self.fsi_trail_sig)) }.to_le_bytes());

        lead == Self::LEAD_SIG && struc == Self::STRUC_SIG && trail == Self::TRAIL_SIG
    }
}

// ── Directory entry (short) — 32 bytes ───────────────────────────────────────

#[derive(Clone, Copy, Default)]
#[repr(C, packed)]
pub struct DirEntry {
    pub dir_name:           [u8; 11],   // 0   — 8.3, padded 0x20, uppercase
    pub dir_attr:           u8,         // 11
    pub dir_nt_res:         u8,         // 12  — reserved, always 0
    pub dir_crt_time_tenth: u8,         // 13  — 0-199 (tenths of second)
    pub dir_crt_time:       u16,        // 14
    pub dir_crt_date:       u16,        // 16
    pub dir_lst_acc_date:   u16,        // 18
    pub dir_fst_clus_hi:    u16,        // 20  — high 16 bits of first cluster
    pub dir_wrt_time:       u16,        // 22  — MUST be maintained
    pub dir_wrt_date:       u16,        // 24  — MUST be maintained
    pub dir_fst_clus_lo:    u16,        // 26  — low 16 bits of first cluster
    pub dir_file_size:      u32,        // 28  — bytes; 0 for directories
}

const _: () = assert!(core::mem::size_of::<DirEntry>() == 32);

/// DIR_Attr bits
pub mod attr {
    pub const READ_ONLY:  u8 = 0x01;
    pub const HIDDEN:     u8 = 0x02;
    pub const SYSTEM:     u8 = 0x04;
    pub const VOLUME_ID:  u8 = 0x08;
    pub const DIRECTORY:  u8 = 0x10;
    pub const ARCHIVE:    u8 = 0x20;
    pub const LFN:        u8 = READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID;
    pub const LFN_MASK:   u8 = READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID
                                | DIRECTORY | ARCHIVE;
}

impl DirEntry {
    /// Construct a zeroed entry.
    pub fn zeroed() -> Self {
        unsafe { core::mem::zeroed() }
    }

    /// Return the first cluster number (combined hi+lo fields).
    pub fn first_cluster(&self) -> u32 {
        let hi = u16::from_le(unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.dir_fst_clus_hi))
        });
        let lo = u16::from_le(unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.dir_fst_clus_lo))
        });
        ((hi as u32) << 16) | (lo as u32)
    }

    /// Set the first cluster number.
    pub fn set_first_cluster(&mut self, cluster: u32) {
        let hi = ((cluster >> 16) & 0xFFFF) as u16;
        let lo = (cluster & 0xFFFF) as u16;
        unsafe {
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.dir_fst_clus_hi), hi.to_le());
            core::ptr::write_unaligned(core::ptr::addr_of_mut!(self.dir_fst_clus_lo), lo.to_le());
        }
    }

    pub fn file_size(&self) -> u32 {
        u32::from_le(unsafe {
            core::ptr::read_unaligned(core::ptr::addr_of!(self.dir_file_size))
        })
    }

    pub fn attr(&self) -> u8 { self.dir_attr }

    pub fn is_lfn(&self) -> bool {
        self.dir_attr & attr::LFN_MASK == attr::LFN
    }

    pub fn is_dir(&self) -> bool {
        self.dir_attr & attr::DIRECTORY != 0 && !self.is_lfn()
    }

    pub fn is_file(&self) -> bool {
        self.dir_attr & attr::DIRECTORY == 0 && !self.is_lfn()
            && self.dir_attr & attr::VOLUME_ID == 0
    }

    /// DIR_Name[0] == 0x00 → end of directory
    pub fn is_end_of_dir(&self) -> bool { self.dir_name[0] == 0x00 }

    /// DIR_Name[0] == 0xE5 → deleted entry
    pub fn is_deleted(&self) -> bool { self.dir_name[0] == 0xE5 }

    pub fn is_dot_or_dotdot(&self) -> bool {
        self.dir_name[0] == b'.' && (
            self.dir_name[1] == b' ' || self.dir_name[1] == b'.'
        )
    }
}

// ── LFN entry — 32 bytes ─────────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(C, packed)]
pub struct LfnEntry {
    pub ldir_ord:        u8,        // 0   — order; last entry OR'd with 0x40
    pub ldir_name1:      [u16; 5],  // 1   — chars 1-5 (UTF-16LE)
    pub ldir_attr:       u8,        // 11  — must be ATTR_LFN (0x0F)
    pub ldir_type:       u8,        // 12  — must be 0
    pub ldir_chksum:     u8,        // 13  — checksum of 8.3 short name
    pub ldir_name2:      [u16; 6],  // 14  — chars 6-11
    pub ldir_fst_clus_lo: u16,      // 26  — must be 0
    pub ldir_name3:      [u16; 2],  // 28  — chars 12-13
}

const _: () = assert!(core::mem::size_of::<LfnEntry>() == 32);

pub const LFN_LAST_LONG_ENTRY: u8 = 0x40;
pub const LFN_CHARS_PER_ENTRY: usize = 13;

impl LfnEntry {
    /// Extract the 13 UTF-16 code units for this LFN entry.
    /// Logical chars (not physical order in struct).
    pub fn chars(&self) -> [u16; LFN_CHARS_PER_ENTRY] {
        // Must use read_unaligned because the struct is packed
        let mut out = [0u16; LFN_CHARS_PER_ENTRY];
        for i in 0..5 {
            out[i] = u16::from_le(unsafe {
                core::ptr::read_unaligned(core::ptr::addr_of!(self.ldir_name1[i]))
            });
        }
        for i in 0..6 {
            out[5 + i] = u16::from_le(unsafe {
                core::ptr::read_unaligned(core::ptr::addr_of!(self.ldir_name2[i]))
            });
        }
        for i in 0..2 {
            out[11 + i] = u16::from_le(unsafe {
                core::ptr::read_unaligned(core::ptr::addr_of!(self.ldir_name3[i]))
            });
        }
        out
    }

    pub fn order(&self) -> u8        { self.ldir_ord }
    pub fn checksum(&self) -> u8     { self.ldir_chksum }
    pub fn is_last(&self) -> bool    { self.ldir_ord & LFN_LAST_LONG_ENTRY != 0 }
    pub fn seq_num(&self) -> u8      { self.ldir_ord & !LFN_LAST_LONG_ENTRY }
}

// ── FAT32 cluster chain sentinel values ──────────────────────────────────────

pub mod fat_val {
    pub const FREE:    u32 = 0x0000_0000;
    pub const RSVD:    u32 = 0x0000_0001;
    pub const BAD:     u32 = 0x0FFF_FFF7;
    pub const EOC_MIN: u32 = 0x0FFF_FFF8;
    pub const EOC:     u32 = 0x0FFF_FFFF;  // value to write for EOC

    pub fn is_eoc(v: u32)  -> bool { v >= EOC_MIN }
    pub fn is_free(v: u32) -> bool { v == FREE }
    pub fn is_bad(v: u32)  -> bool { v == BAD }
    pub fn is_data(v: u32) -> bool { v >= 2 && v < EOC_MIN && v != BAD }
}

// ── FAT32 dirty-volume flags in FAT[1] ───────────────────────────────────────

pub const FAT32_CLN_SHUT_BIT: u32 = 0x0800_0000;
pub const FAT32_HRD_ERR_BIT:  u32 = 0x0400_0000;

// ── LFN checksum (spec p.27-28) ──────────────────────────────────────────────

/// Compute the 8-bit checksum of the 11-byte short-name DIR_Name field.
/// Algorithm from the FAT32 spec, verbatim.
pub fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    short_name.iter().fold(0u8, |sum, &c| {
        // Unsigned char rotate right by 1, then add next byte
        ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(c)
    })
}

// ── FAT date / time helpers ──────────────────────────────────────────────────
//
// FAT date: bits 0-4 = day, 5-8 = month, 9-15 = years since 1980
// FAT time: bits 0-4 = 2-sec count, 5-10 = minutes, 11-15 = hours

/// Encode a date to FAT16 format.
pub fn encode_date(year: u16, month: u8, day: u8) -> u16 {
    let y = year.saturating_sub(1980).min(127) as u16;
    ((y & 0x7F) << 9) | ((month as u16 & 0x0F) << 5) | (day as u16 & 0x1F)
}

/// Encode a time to FAT16 format (2-second granularity).
pub fn encode_time(hour: u8, minute: u8, second: u8) -> u16 {
    ((hour as u16 & 0x1F) << 11)
        | ((minute as u16 & 0x3F) << 5)
        | ((second as u16 / 2) & 0x1F)
}

/// Decode a FAT16 date → (year, month, day).
pub fn decode_date(date: u16) -> (u16, u8, u8) {
    let year  = ((date >> 9) & 0x7F) + 1980;
    let month = ((date >> 5) & 0x0F) as u8;
    let day   = (date & 0x1F) as u8;
    (year, month, day)
}

/// Decode a FAT16 time → (hour, minute, second).
pub fn decode_time(time: u16) -> (u8, u8, u8) {
    let hour   = ((time >> 11) & 0x1F) as u8;
    let minute = ((time >> 5)  & 0x3F) as u8;
    let second = ((time & 0x1F) * 2)   as u8;
    (hour, minute, second)
}

/// Convert a FAT date+time pair to nanoseconds since the FAT epoch 1980-01-01.
/// Simplified (ignores leapseconds and time zones — FAT has no TZ concept).
pub fn fat_datetime_to_ns(date: u16, time: u16, tenths: u8) -> u64 {
    let (year, month, day)     = decode_date(date);
    let (hour, minute, second) = decode_time(time);

    // Approximate: days since 1980-01-01 using Julian Day arithmetic
    let ym   = (month as i32 - 14) / 12;
    let y    = year as i64 + ym as i64;
    let m    = month as i64 - 2 - 12 * ym as i64;
    let jdn  = (1461 * (y + 4800)) / 4
             + (367 * m) / 12
             - (3 * ((y + 4900) / 100)) / 4
             + day as i64 - 32075i64;

    // Julian Day of 1980-01-01 = 2444240
    let days_since_epoch = (jdn - 2444240).max(0) as u64;

    let secs = days_since_epoch * 86400
             + hour   as u64 * 3600
             + minute as u64 * 60
             + second as u64;

    secs * 1_000_000_000 + tenths as u64 * 10_000_000
}
