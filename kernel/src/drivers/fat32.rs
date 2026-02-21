// FAT32 filesystem driver for Atom OS
// Provides read-only access to FAT32 partitions

#![allow(dead_code, static_mut_refs)]

use super::ahci;
use alloc::vec::Vec;
use alloc::string::{String, ToString};
use alloc::format;

// FAT32 Boot Sector / BPB
#[repr(C, packed)]
struct Fat32Bpb {
    jmp_boot: [u8; 3],
    oem_name: [u8; 8],
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    num_fats: u8,
    root_entry_count: u16,    // 0 for FAT32
    total_sectors_16: u16,    // 0 for FAT32
    media: u8,
    fat_size_16: u16,         // 0 for FAT32
    sectors_per_track: u16,
    num_heads: u16,
    hidden_sectors: u32,
    total_sectors_32: u32,
    // FAT32 specific
    fat_size_32: u32,
    ext_flags: u16,
    fs_version: u16,
    root_cluster: u32,
    fs_info: u16,
    backup_boot_sector: u16,
    reserved: [u8; 12],
    drive_number: u8,
    reserved1: u8,
    boot_sig: u8,
    volume_id: u32,
    volume_label: [u8; 11],
    fs_type: [u8; 8],
}

// Directory entry
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; 11],           // 8.3 filename
    attr: u8,
    nt_reserved: u8,
    create_time_tenth: u8,
    create_time: u16,
    create_date: u16,
    last_access_date: u16,
    first_cluster_hi: u16,
    write_time: u16,
    write_date: u16,
    first_cluster_lo: u16,
    file_size: u32,
}

// Long filename entry
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct LfnEntry {
    order: u8,
    name1: [u16; 5],
    attr: u8,
    lfn_type: u8,
    checksum: u8,
    name2: [u16; 6],
    first_cluster_lo: u16,
    name3: [u16; 2],
}

// File attributes
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

// FAT entry values
const FAT_EOC: u32 = 0x0FFFFFF8; // End of cluster chain

// Cached filesystem info
static mut FS_INFO: Option<FsInfo> = None;

struct FsInfo {
    bytes_per_sector: u32,
    sectors_per_cluster: u32,
    fat_start_sector: u32,
    data_start_sector: u32,
    root_cluster: u32,
    partition_start: u64, // Partition offset in sectors
}

pub fn init() -> bool {
    if !ahci::is_available() {
        crate::log_warn!("fat32", "AHCI not available");
        return false;
    }

    // Read MBR to find partition
    let mbr = match ahci::read_sectors(0, 1) {
        Some(data) => data,
        None => {
            crate::log_error!("fat32", "Failed to read MBR");
            return false;
        }
    };

    // Check MBR signature
    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        crate::log_warn!("fat32", "Invalid MBR signature");
        // Try reading as bare FAT32 (no partition table)
        return init_partition(0);
    }

    // Parse partition table (starts at offset 446)
    for i in 0..4 {
        let offset = 446 + i * 16;
        let partition_type = mbr[offset + 4];
        let start_lba = u32::from_le_bytes([
            mbr[offset + 8],
            mbr[offset + 9],
            mbr[offset + 10],
            mbr[offset + 11],
        ]);

        // FAT32 partition types: 0x0B, 0x0C (LBA), 0x1B, 0x1C (hidden)
        if partition_type == 0x0B || partition_type == 0x0C ||
           partition_type == 0x1B || partition_type == 0x1C {
            crate::log_info!("fat32", "Found FAT32 partition at sector {}", start_lba);
            return init_partition(start_lba as u64);
        }
    }

    // No FAT32 partition found, try reading as bare FAT32
    crate::log_debug!("fat32", "No MBR partition, trying bare FAT32");
    init_partition(0)
}

fn init_partition(start_sector: u64) -> bool {
    // Read boot sector
    let boot_sector = match ahci::read_sectors(start_sector, 1) {
        Some(data) => data,
        None => {
            crate::log_error!("fat32", "Failed to read boot sector");
            return false;
        }
    };

    // Parse BPB
    let bpb = unsafe { &*(boot_sector.as_ptr() as *const Fat32Bpb) };

    // Validate FAT32
    let bytes_per_sector = u16::from_le(bpb.bytes_per_sector) as u32;
    let fat_size = if bpb.fat_size_16 != 0 {
        u16::from_le(bpb.fat_size_16) as u32
    } else {
        u32::from_le(bpb.fat_size_32)
    };

    if bytes_per_sector != 512 {
        crate::log_error!("fat32", "Unsupported sector size: {}", bytes_per_sector);
        return false;
    }

    let sectors_per_cluster = bpb.sectors_per_cluster as u32;
    let reserved_sectors = u16::from_le(bpb.reserved_sectors) as u32;
    let num_fats = bpb.num_fats as u32;
    let root_cluster = u32::from_le(bpb.root_cluster);

    let fat_start_sector = start_sector as u32 + reserved_sectors;
    let data_start_sector = fat_start_sector + (num_fats * fat_size);

    crate::log_info!(
        "fat32",
        "FAT32: sectors_per_cluster={}, fat_start={}, data_start={}, root_cluster={}",
        sectors_per_cluster,
        fat_start_sector,
        data_start_sector,
        root_cluster
    );

    unsafe {
        FS_INFO = Some(FsInfo {
            bytes_per_sector,
            sectors_per_cluster,
            fat_start_sector,
            data_start_sector,
            root_cluster,
            partition_start: start_sector,
        });
    }

    true
}

fn cluster_to_sector(cluster: u32) -> u32 {
    unsafe {
        let info = FS_INFO.as_ref().unwrap();
        info.data_start_sector + (cluster - 2) * info.sectors_per_cluster
    }
}

fn read_fat_entry(cluster: u32) -> Option<u32> {
    unsafe {
        let info = FS_INFO.as_ref()?;
        let fat_offset = cluster * 4;
        let fat_sector = info.fat_start_sector + (fat_offset / info.bytes_per_sector);
        let entry_offset = (fat_offset % info.bytes_per_sector) as usize;

        let sector_data = ahci::read_sectors(fat_sector as u64, 1)?;
        let entry = u32::from_le_bytes([
            sector_data[entry_offset],
            sector_data[entry_offset + 1],
            sector_data[entry_offset + 2],
            sector_data[entry_offset + 3],
        ]);

        Some(entry & 0x0FFFFFFF)
    }
}

fn read_cluster(cluster: u32) -> Option<Vec<u8>> {
    unsafe {
        let info = FS_INFO.as_ref()?;
        let sector = cluster_to_sector(cluster);
        let sector_count = info.sectors_per_cluster as u16;

        let data = ahci::read_sectors(sector as u64, sector_count)?;
        Some(data.to_vec())
    }
}

/// Read entire cluster chain starting from given cluster
fn read_cluster_chain(start_cluster: u32) -> Option<Vec<u8>> {
    let mut result = Vec::new();
    let mut cluster = start_cluster;

    loop {
        let data = read_cluster(cluster)?;
        result.extend_from_slice(&data);

        let next = read_fat_entry(cluster)?;
        if next >= FAT_EOC {
            break;
        }
        cluster = next;
    }

    Some(result)
}

/// Find a file in the given directory cluster
fn find_in_directory(dir_cluster: u32, name: &str) -> Option<DirEntry> {
    let upper_name = name.to_uppercase();
    let dir_data = read_cluster_chain(dir_cluster)?;

    let mut i = 0;
    let mut lfn_name = String::new();

    while i + 32 <= dir_data.len() {
        let entry = unsafe { *(dir_data.as_ptr().add(i) as *const DirEntry) };

        // End of directory
        if entry.name[0] == 0x00 {
            break;
        }

        // Deleted entry
        if entry.name[0] == 0xE5 {
            i += 32;
            continue;
        }

        // LFN entry
        if entry.attr == ATTR_LFN {
            let lfn = unsafe { core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const LfnEntry) };
            let mut chars = Vec::new();

            // Copy arrays from packed struct to avoid alignment issues
            let name1 = lfn.name1;
            let name2 = lfn.name2;
            let name3 = lfn.name3;

            for c in name1.iter() {
                let c = u16::from_le(*c);
                if c == 0 || c == 0xFFFF {
                    break;
                }
                chars.push(c);
            }
            for c in name2.iter() {
                let c = u16::from_le(*c);
                if c == 0 || c == 0xFFFF {
                    break;
                }
                chars.push(c);
            }
            for c in name3.iter() {
                let c = u16::from_le(*c);
                if c == 0 || c == 0xFFFF {
                    break;
                }
                chars.push(c);
            }

            // LFN entries are in reverse order
            let part: String = chars.iter().filter_map(|&c| char::from_u32(c as u32)).collect();
            if (lfn.order & 0x40) != 0 {
                lfn_name = part;
            } else {
                lfn_name = part + &lfn_name;
            }

            i += 32;
            continue;
        }

        // Regular entry
        if entry.attr & ATTR_VOLUME_ID != 0 {
            i += 32;
            lfn_name.clear();
            continue;
        }

        // Build 8.3 name
        let short_name = {
            let name_part: String = entry.name[0..8]
                .iter()
                .map(|&c| c as char)
                .collect::<String>()
                .trim()
                .to_string();
            let ext_part: String = entry.name[8..11]
                .iter()
                .map(|&c| c as char)
                .collect::<String>()
                .trim()
                .to_string();

            if ext_part.is_empty() {
                name_part
            } else {
                format!("{}.{}", name_part, ext_part)
            }
        };

        // Check both LFN and short name
        let check_name = if lfn_name.is_empty() {
            short_name.to_uppercase()
        } else {
            lfn_name.to_uppercase()
        };

        if check_name == upper_name || short_name.to_uppercase() == upper_name {
            return Some(entry);
        }

        lfn_name.clear();
        i += 32;
    }

    None
}

/// Open a file by path (e.g., "/drivers/terminal.atxf")
pub fn open(path: &str) -> Option<Vec<u8>> {
    unsafe {
        let info = FS_INFO.as_ref()?;
        let path = path.trim_start_matches(['/', '\\']);

        let parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return None;
        }

        let mut current_cluster = info.root_cluster;

        // Navigate directories
        for (idx, part) in parts.iter().enumerate() {
            let entry = find_in_directory(current_cluster, part)?;

            if idx == parts.len() - 1 {
                // This is the file
                if entry.attr & ATTR_DIRECTORY != 0 {
                    return None; // Expected file, got directory
                }

                let first_cluster =
                    ((u16::from_le(entry.first_cluster_hi) as u32) << 16) |
                    (u16::from_le(entry.first_cluster_lo) as u32);
                let file_size = u32::from_le(entry.file_size) as usize;

                if first_cluster < 2 {
                    return Some(Vec::new()); // Empty file
                }

                let mut data = read_cluster_chain(first_cluster)?;
                data.truncate(file_size);
                return Some(data);
            } else {
                // Navigate into directory
                if entry.attr & ATTR_DIRECTORY == 0 {
                    return None; // Expected directory, got file
                }

                current_cluster =
                    ((u16::from_le(entry.first_cluster_hi) as u32) << 16) |
                    (u16::from_le(entry.first_cluster_lo) as u32);
            }
        }

        None
    }
}

/// Check if filesystem is initialized
pub fn is_available() -> bool {
    unsafe { FS_INFO.is_some() }
}

/// File metadata returned by stat_path (no file content is read).
pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}

/// Stat a file or directory by path — returns metadata without reading content.
/// Much cheaper than `open()` because it only reads directory entries, not file data.
pub fn stat_path(path: &str) -> Option<FileStat> {
    unsafe {
        let info = FS_INFO.as_ref()?;
        let path = path.trim_start_matches(['/', '\\']);

        // Root directory
        if path.is_empty() {
            return Some(FileStat { size: 0, is_dir: true });
        }

        let parts: alloc::vec::Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Some(FileStat { size: 0, is_dir: true });
        }

        let mut current_cluster = info.root_cluster;

        for (idx, part) in parts.iter().enumerate() {
            let entry = find_in_directory(current_cluster, part)?;

            if idx == parts.len() - 1 {
                // Target entry found — extract metadata from DirEntry
                let is_dir = entry.attr & ATTR_DIRECTORY != 0;
                let size = u32::from_le(entry.file_size) as u64;
                return Some(FileStat { size, is_dir });
            } else {
                // Navigate into subdirectory
                if entry.attr & ATTR_DIRECTORY == 0 {
                    return None;
                }
                current_cluster =
                    ((u16::from_le(entry.first_cluster_hi) as u32) << 16) |
                    (u16::from_le(entry.first_cluster_lo) as u32);
            }
        }

        None
    }
}

/// List files in a directory (for debugging)
pub fn list_directory(path: &str) -> Option<Vec<String>> {
    unsafe {
        let info = FS_INFO.as_ref()?;
        let path = path.trim_start_matches(['/', '\\']);

        let mut current_cluster = info.root_cluster;

        // Navigate to directory
        if !path.is_empty() {
            let parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
            for part in parts {
                let entry = find_in_directory(current_cluster, part)?;
                if entry.attr & ATTR_DIRECTORY == 0 {
                    return None;
                }
                current_cluster =
                    ((u16::from_le(entry.first_cluster_hi) as u32) << 16) |
                    (u16::from_le(entry.first_cluster_lo) as u32);
            }
        }

        // List entries
        let dir_data = read_cluster_chain(current_cluster)?;
        let mut files = Vec::new();
        let mut i = 0;
        let mut lfn_name = String::new();

        while i + 32 <= dir_data.len() {
            let entry = *(dir_data.as_ptr().add(i) as *const DirEntry);

            if entry.name[0] == 0x00 {
                break;
            }

            if entry.name[0] == 0xE5 {
                i += 32;
                continue;
            }

            if entry.attr == ATTR_LFN {
                let lfn = core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const LfnEntry);
                let mut chars = Vec::new();
                let name1 = lfn.name1;
                let name2 = lfn.name2;
                let name3 = lfn.name3;
                for c in name1.iter() {
                    let c = u16::from_le(*c);
                    if c == 0 || c == 0xFFFF { break; }
                    chars.push(c);
                }
                for c in name2.iter() {
                    let c = u16::from_le(*c);
                    if c == 0 || c == 0xFFFF { break; }
                    chars.push(c);
                }
                for c in name3.iter() {
                    let c = u16::from_le(*c);
                    if c == 0 || c == 0xFFFF { break; }
                    chars.push(c);
                }
                let part: String = chars.iter().filter_map(|&c| char::from_u32(c as u32)).collect();
                if (lfn.order & 0x40) != 0 {
                    lfn_name = part;
                } else {
                    lfn_name = part + &lfn_name;
                }
                i += 32;
                continue;
            }

            if entry.attr & ATTR_VOLUME_ID != 0 {
                i += 32;
                lfn_name.clear();
                continue;
            }

            let display_name = if lfn_name.is_empty() {
                let name_part: String = entry.name[0..8].iter().map(|&c| c as char).collect::<String>().trim().to_string();
                let ext_part: String = entry.name[8..11].iter().map(|&c| c as char).collect::<String>().trim().to_string();
                if ext_part.is_empty() { name_part } else { format!("{}.{}", name_part, ext_part) }
            } else {
                lfn_name.clone()
            };

            let suffix = if entry.attr & ATTR_DIRECTORY != 0 { "/" } else { "" };
            files.push(format!("{}{}", display_name, suffix));
            lfn_name.clear();
            i += 32;
        }

        Some(files)
    }
}
