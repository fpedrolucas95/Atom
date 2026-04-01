// fat32/bpb.rs — BPB parsing and FAT type determination
//
// The ONLY correct way to determine FAT type is by CountofClusters,
// as specified in the FAT32 spec p.14-15.  This module implements that
// determination strictly.
//
// All validation rules from the spec are enforced; any violation returns
// VfsError::Corrupted.

#![allow(dead_code)]

extern crate alloc as alloc_crate;
use alloc_crate::string::String;
use alloc_crate::vec::Vec;
use alloc_crate::format;

use crate::error::{VfsError, VfsResult};
use super::types::{BootSector, BpbCommon, BpbFat32Ext, attr};

// ── Parsed volume parameters ──────────────────────────────────────────────────

/// All values derived from the BPB, pre-computed for quick access.
/// Immutable after mount.
#[derive(Debug, Clone)]
pub struct VolumeParams {
    // ── Identification ──────────────────────────────────────────────────
    pub volume_id:          u32,
    pub volume_label:       [u8; 11],

    // ── Sector / cluster geometry ───────────────────────────────────────
    /// Bytes per logical sector (512 / 1024 / 2048 / 4096)
    pub bytes_per_sec:      u32,
    /// Sectors per cluster (1 / 2 / 4 / 8 / … / 128)
    pub secs_per_clus:      u32,
    /// = bytes_per_sec * secs_per_clus  (≤ 32768 per spec)
    pub cluster_size:       u32,

    // ── Region layout (absolute LBA = partition_start + relative) ───────
    /// How many sectors are reserved (contains BPB + FS info + backup)
    pub rsvd_secs:          u32,
    /// Number of FAT copies (typically 2)
    pub num_fats:           u32,
    /// Sectors occupied by one FAT
    pub fat_size_secs:      u64,
    /// Absolute LBA of the first sector of FAT 0
    pub fat0_start_lba:     u64,
    /// Absolute LBA of the first data sector (cluster 2)
    pub data_start_lba:     u64,
    /// First cluster of root directory (typically 2)
    pub root_cluster:       u32,

    // ── Capacity ────────────────────────────────────────────────────────
    /// Number of data clusters (CountofClusters from spec)
    pub count_of_clusters:  u32,
    /// Maximum valid cluster number = cluster 2 + count_of_clusters - 1
    pub max_valid_cluster:  u32,

    // ── FSInfo ──────────────────────────────────────────────────────────
    /// Absolute LBA of the FSInfo sector
    pub fsinfo_lba:         u64,
    /// Absolute LBA of the backup boot sector (BPB_BkBootSec; typically
    /// partition_start + 6).  Set to 0 when BPB_BkBootSec == 0 (no backup).
    pub backup_boot_lba:    u64,

    // ── Mirroring ───────────────────────────────────────────────────────
    /// true = FAT is mirrored to all copies (EXT_FLAGS bit 7 == 0)
    pub fat_mirroring:      bool,
    /// Active FAT index (only valid when mirroring is disabled)
    pub active_fat_idx:     u32,
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Parse the boot sector and derive all volume parameters.
///
/// `partition_start` is the absolute LBA of sector 0 of the partition.
/// The caller must have already read 512 bytes into `sector0`.
pub fn parse_bpb(sector0: &[u8; 512], partition_start: u64) -> VfsResult<VolumeParams> {
    // ── Boot signature ────────────────────────────────────────────────────
    if sector0[510] != 0x55 || sector0[511] != 0xAA {
        return Err(VfsError::Corrupted); // spec: mandatory signature
    }

    // ── Read packed fields safely ─────────────────────────────────────────
    let byts_per_sec = u16::from_le_bytes([sector0[11], sector0[12]]) as u32;
    let sec_per_clus = sector0[13] as u32;
    let rsvd_sec_cnt = u16::from_le_bytes([sector0[14], sector0[15]]) as u32;
    let num_fats     = sector0[16] as u32;
    let root_ent_cnt = u16::from_le_bytes([sector0[17], sector0[18]]) as u32;
    let tot_sec16    = u16::from_le_bytes([sector0[19], sector0[20]]) as u32;
    let fat_sz16     = u16::from_le_bytes([sector0[22], sector0[23]]) as u32;
    let tot_sec32    = u32::from_le_bytes([sector0[32], sector0[33], sector0[34], sector0[35]]);

    // FAT32 extension fields (offset 36+)
    let fat_sz32     = u32::from_le_bytes([sector0[36], sector0[37], sector0[38], sector0[39]]);
    let ext_flags    = u16::from_le_bytes([sector0[40], sector0[41]]);
    let fs_ver       = u16::from_le_bytes([sector0[42], sector0[43]]);
    let root_clus    = u32::from_le_bytes([sector0[44], sector0[45], sector0[46], sector0[47]]);
    let fs_info_sec  = u16::from_le_bytes([sector0[48], sector0[49]]) as u32;
    let bk_boot_sec  = u16::from_le_bytes([sector0[50], sector0[51]]) as u64;
    let vol_id       = u32::from_le_bytes([sector0[67], sector0[68], sector0[69], sector0[70]]);
    let mut vol_lab  = [0u8; 11];
    vol_lab.copy_from_slice(&sector0[71..82]);

    // ── Spec validation rules ─────────────────────────────────────────────

    // Rule 1: BPB_BytsPerSec ∈ {512, 1024, 2048, 4096}
    if !matches!(byts_per_sec, 512 | 1024 | 2048 | 4096) {
        return Err(VfsError::Corrupted);
    }

    // Rule 2: BPB_SecPerClus must be a power of 2, 1 ≤ x ≤ 128
    if sec_per_clus == 0 || !sec_per_clus.is_power_of_two() || sec_per_clus > 128 {
        return Err(VfsError::Corrupted);
    }

    // Rule 3: Cluster size ≤ 32768 bytes
    let cluster_size = byts_per_sec * sec_per_clus;
    if cluster_size > 32768 {
        return Err(VfsError::Corrupted);
    }

    // Rule 4: BPB_RsvdSecCnt > 0
    if rsvd_sec_cnt == 0 {
        return Err(VfsError::Corrupted);
    }

    // Rule 5: BPB_NumFATs ≥ 1
    if num_fats == 0 {
        return Err(VfsError::Corrupted);
    }

    // Rules 6-8: FAT32 must have these fields = 0
    if root_ent_cnt != 0 { return Err(VfsError::Corrupted); }
    if tot_sec16    != 0 { return Err(VfsError::Corrupted); }
    if fat_sz16     != 0 { return Err(VfsError::Corrupted); }

    // Rule 9: BPB_TotSec32 > 0 for FAT32
    if tot_sec32 == 0 { return Err(VfsError::Corrupted); }

    // Rule 10: BPB_FATSz32 > 0
    if fat_sz32 == 0 { return Err(VfsError::Corrupted); }

    // Rule 11: Reject future FS versions (spec says drivers must not mount them)
    if fs_ver != 0x0000 { return Err(VfsError::Corrupted); }

    // ── FAT type determination (spec p.14-15, the ONLY correct method) ────
    //
    // RootDirSectors = 0 for FAT32 (root_ent_cnt == 0)
    let fat_size     = fat_sz32 as u64;
    let root_dir_secs: u64 = 0; // root_ent_cnt == 0 for FAT32
    let data_secs    = tot_sec32 as u64
        - (rsvd_sec_cnt as u64 + num_fats as u64 * fat_size + root_dir_secs);
    let count_of_clusters = (data_secs / sec_per_clus as u64) as u32;

    // The ONLY way to check FAT type — spec p.15
    if count_of_clusters < 65525 {
        // Volume has < 65525 clusters → FAT12 or FAT16, not FAT32
        return Err(VfsError::NotSupported);
    }

    // ── Derived layout values ─────────────────────────────────────────────
    let fat0_start_lba = partition_start + rsvd_sec_cnt as u64;
    let data_start_lba = fat0_start_lba + num_fats as u64 * fat_size;
    let fsinfo_lba     = partition_start + fs_info_sec as u64;
    let max_valid_clus = 1 + count_of_clusters; // clusters 2..=(count+1) are valid

    // BPB_BkBootSec: 0 and 1 are reserved/invalid; spec recommends 6.
    // Store 0 to mean "no backup" when the field is clearly invalid.
    let backup_boot_lba = if bk_boot_sec >= 2 {
        partition_start + bk_boot_sec
    } else {
        0
    };

    // ── Mirroring flags ───────────────────────────────────────────────────
    let fat_mirroring = ext_flags & 0x0080 == 0;   // bit7 = 0 → mirror
    let active_fat    = (ext_flags & 0x000F) as u32;

    Ok(VolumeParams {
        volume_id:         vol_id,
        volume_label:      vol_lab,
        bytes_per_sec:     byts_per_sec,
        secs_per_clus:     sec_per_clus,
        cluster_size,
        rsvd_secs:         rsvd_sec_cnt,
        num_fats,
        fat_size_secs:     fat_size,
        fat0_start_lba,
        data_start_lba,
        root_cluster:      root_clus,
        count_of_clusters,
        max_valid_cluster: max_valid_clus,
        fsinfo_lba,
        backup_boot_lba,
        fat_mirroring,
        active_fat_idx:    active_fat,
    })
}

/// Validate that `cluster` is within the addressable range for this volume.
#[inline]
pub fn validate_cluster(params: &VolumeParams, cluster: u32) -> VfsResult<()> {
    if cluster < 2 || cluster > params.max_valid_cluster {
        Err(VfsError::Corrupted)
    } else {
        Ok(())
    }
}

/// Compute the absolute LBA of the first sector of `cluster`.
#[inline]
pub fn cluster_to_lba(params: &VolumeParams, cluster: u32) -> u64 {
    params.data_start_lba + (cluster as u64 - 2) * params.secs_per_clus as u64
}

/// Produce a multi-line summary of the volume parameters for boot logging.
///
/// Each line is prefixed with `prefix` (e.g. `"fsd: "`).
/// Returns a Vec of individual log messages (caller logs each one).
pub fn format_volume_summary(params: &VolumeParams, prefix: &str) -> Vec<String> {
    // Decode volume label (strip trailing spaces)
    let label_str = core::str::from_utf8(&params.volume_label)
        .unwrap_or("????????   ")
        .trim_end_matches(' ');

    let total_bytes = params.count_of_clusters as u64 * params.cluster_size as u64;
    let total_mb    = total_bytes / (1024 * 1024);

    alloc_crate::vec![
        format!("{}=== FAT32 Volume Mounted ===", prefix),
        format!("{}  Label         : {}", prefix, label_str),
        format!("{}  Volume ID     : {:08X}", prefix, params.volume_id),
        format!("{}  Bytes/sector  : {}", prefix, params.bytes_per_sec),
        format!("{}  Secs/cluster  : {}  ({} bytes/cluster)",
                prefix, params.secs_per_clus, params.cluster_size),
        format!("{}  Reserved secs : {}", prefix, params.rsvd_secs),
        format!("{}  FAT copies    : {}  (size: {} sectors each)",
                prefix, params.num_fats, params.fat_size_secs),
        format!("{}  FAT0 LBA      : {}", prefix, params.fat0_start_lba),
        format!("{}  Data LBA      : {}", prefix, params.data_start_lba),
        format!("{}  Root cluster  : {}", prefix, params.root_cluster),
        format!("{}  Clusters      : {}  (~{} MiB usable)",
                prefix, params.count_of_clusters, total_mb),
        format!("{}  FSInfo LBA    : {}", prefix, params.fsinfo_lba),
        format!("{}  Backup BPB    : {}",
                prefix,
                if params.backup_boot_lba == 0 { format!("none") }
                else { format!("LBA {}", params.backup_boot_lba) }),
        format!("{}  Mirroring     : {}",
                prefix,
                if params.fat_mirroring { "enabled (all FATs updated)" }
                else { "disabled (active FAT only)" }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid FAT32 boot sector for testing.
    /// `bk_boot`: BPB_BkBootSec value (bytes 50-51); 0 means "no backup".
    fn make_fat32_sector_full(
        byts_per_sec: u16,
        sec_per_clus: u8,
        rsvd: u16,
        num_fats: u8,
        tot_sec32: u32,
        fat_sz32: u32,
        root_clus: u32,
        fs_info: u16,
        bk_boot: u16,
    ) -> [u8; 512] {
        let mut s = [0u8; 512];
        s[11..13].copy_from_slice(&byts_per_sec.to_le_bytes());
        s[13] = sec_per_clus;
        s[14..16].copy_from_slice(&rsvd.to_le_bytes());
        s[16] = num_fats;
        // root_ent_cnt = 0 (already zero)
        // tot_sec16 = 0 (already zero)
        s[32..36].copy_from_slice(&tot_sec32.to_le_bytes());
        s[36..40].copy_from_slice(&fat_sz32.to_le_bytes());
        s[44..48].copy_from_slice(&root_clus.to_le_bytes());
        s[48..50].copy_from_slice(&fs_info.to_le_bytes());
        s[50..52].copy_from_slice(&bk_boot.to_le_bytes());
        s[510] = 0x55;
        s[511] = 0xAA;
        s
    }

    fn make_fat32_sector(
        byts_per_sec: u16,
        sec_per_clus: u8,
        rsvd: u16,
        num_fats: u8,
        tot_sec32: u32,
        fat_sz32: u32,
        root_clus: u32,
        fs_info: u16,
    ) -> [u8; 512] {
        make_fat32_sector_full(
            byts_per_sec, sec_per_clus, rsvd, num_fats,
            tot_sec32, fat_sz32, root_clus, fs_info,
            6, // default BPB_BkBootSec = 6
        )
    }

    #[test]
    fn test_valid_fat32_volume() {
        // 1 GB disk: 2097152 sectors of 512 B
        // FAT32: rsvd=32, num_fats=2, fat_sz32=256
        let sec = make_fat32_sector(512, 8, 32, 2, 2097152, 256, 2, 1);
        let p = parse_bpb(&sec, 0).unwrap();
        assert!(p.count_of_clusters >= 65525);
        assert_eq!(p.bytes_per_sec, 512);
        assert_eq!(p.secs_per_clus, 8);
    }

    #[test]
    fn test_fat_type_boundary_65524_rejected() {
        // Craft a volume with exactly 65524 clusters → should be FAT16 → Err
        // DataSec = 65524 * 8 = 524192
        // tot_sec32 = 32 + 2*256 + 524192 = 524736
        let sec = make_fat32_sector(512, 8, 32, 2, 524736, 256, 2, 1);
        let r = parse_bpb(&sec, 0);
        assert!(r.is_err(), "65524 clusters must not mount as FAT32");
    }

    #[test]
    fn test_fat_type_boundary_65525_accepted() {
        // 65525 clusters * 8 secs/clus = 524200 data sectors
        // tot_sec32 = 32 + 2*256 + 524200 = 524744
        let sec = make_fat32_sector(512, 8, 32, 2, 524744, 256, 2, 1);
        let r = parse_bpb(&sec, 0);
        assert!(r.is_ok(), "65525 clusters must mount as FAT32");
    }

    #[test]
    fn test_bad_signature_rejected() {
        let mut sec = make_fat32_sector(512, 8, 32, 2, 2097152, 256, 2, 1);
        sec[510] = 0x00;
        assert!(parse_bpb(&sec, 0).is_err());
    }

    #[test]
    fn test_nonzero_fs_ver_rejected() {
        let mut sec = make_fat32_sector(512, 8, 32, 2, 2097152, 256, 2, 1);
        sec[42] = 0x01; // fs_ver major != 0
        assert!(parse_bpb(&sec, 0).is_err());
    }

    #[test]
    fn test_invalid_sec_per_clus_rejected() {
        // sec_per_clus = 3 is not a power of 2
        let sec = make_fat32_sector(512, 3, 32, 2, 2097152, 256, 2, 1);
        assert!(parse_bpb(&sec, 0).is_err());
    }

    #[test]
    fn test_cluster_to_lba() {
        let sec = make_fat32_sector(512, 8, 32, 2, 2097152, 256, 2, 1);
        let p = parse_bpb(&sec, 0).unwrap();
        // cluster 2 must equal data_start_lba
        assert_eq!(cluster_to_lba(&p, 2), p.data_start_lba);
        // cluster 3 = data_start + 8 sectors
        assert_eq!(cluster_to_lba(&p, 3), p.data_start_lba + 8);
    }

    // ── Phase 5: backup boot sector tests ────────────────────────────────────

    #[test]
    fn test_backup_boot_lba_stored_correctly() {
        // BPB_BkBootSec = 6, partition_start = 0 → backup_boot_lba = 6
        let sec = make_fat32_sector_full(512, 8, 32, 2, 2097152, 256, 2, 1, 6);
        let p = parse_bpb(&sec, 0).unwrap();
        assert_eq!(p.backup_boot_lba, 6, "backup LBA must be partition_start + bk_boot_sec");
    }

    #[test]
    fn test_backup_boot_lba_with_nonzero_partition_start() {
        // partition at LBA 2048; BPB_BkBootSec = 6 → absolute backup LBA = 2048+6 = 2054
        let sec = make_fat32_sector_full(512, 8, 32, 2, 2097152, 256, 2, 1, 6);
        let p = parse_bpb(&sec, 2048).unwrap();
        assert_eq!(p.backup_boot_lba, 2048 + 6);
    }

    #[test]
    fn test_backup_boot_lba_zero_when_invalid() {
        // BPB_BkBootSec = 0 → no backup (spec: 0 means "not present")
        let sec = make_fat32_sector_full(512, 8, 32, 2, 2097152, 256, 2, 1, 0);
        let p = parse_bpb(&sec, 0).unwrap();
        assert_eq!(p.backup_boot_lba, 0, "BPB_BkBootSec=0 must map to backup_boot_lba=0");
    }

    #[test]
    fn test_backup_boot_lba_one_treated_as_invalid() {
        // BPB_BkBootSec = 1 is reserved/invalid per spec → backup_boot_lba = 0
        let sec = make_fat32_sector_full(512, 8, 32, 2, 2097152, 256, 2, 1, 1);
        let p = parse_bpb(&sec, 0).unwrap();
        assert_eq!(p.backup_boot_lba, 0, "BPB_BkBootSec=1 is reserved, must produce backup_boot_lba=0");
    }

    #[test]
    fn test_backup_boot_lba_non_default_value() {
        // BPB_BkBootSec = 12 (non-standard but valid ≥2)
        let sec = make_fat32_sector_full(512, 8, 32, 2, 2097152, 256, 2, 1, 12);
        let p = parse_bpb(&sec, 100).unwrap();
        assert_eq!(p.backup_boot_lba, 100 + 12);
    }
}
