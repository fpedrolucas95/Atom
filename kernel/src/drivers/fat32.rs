// FAT32 filesystem driver for Atom OS
// Provides basic read/write access to a FAT32 partition.

#![allow(dead_code, static_mut_refs)]

use super::ahci;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[repr(C, packed)]
struct Fat32Bpb {
    jmp_boot: [u8; 3],
    oem_name: [u8; 8],
    bytes_per_sector: u16,
    sectors_per_cluster: u8,
    reserved_sectors: u16,
    num_fats: u8,
    root_entry_count: u16,
    total_sectors_16: u16,
    media: u8,
    fat_size_16: u16,
    sectors_per_track: u16,
    num_heads: u16,
    hidden_sectors: u32,
    total_sectors_32: u32,
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

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DirEntry {
    name: [u8; 11],
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

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

const FAT_FREE: u32 = 0x0000_0000;
const FAT_BAD: u32 = 0x0FFF_FFF7;
const FAT_EOC: u32 = 0x0FFF_FFF8;

static mut FS_INFO: Option<FsInfo> = None;

struct FsInfo {
    bytes_per_sector: u32,
    sectors_per_cluster: u32,
    reserved_sectors: u32,
    num_fats: u32,
    fat_size_sectors: u32,
    fat_start_sector: u32,
    data_start_sector: u32,
    root_cluster: u32,
    total_sectors: u32,
    total_clusters: u32,
    partition_start: u64,
}

#[derive(Clone, Copy)]
struct LocatedEntry {
    entry: DirEntry,
    dir_cluster: u32,
    entry_index: usize,
}

pub struct FileStat {
    pub size: u64,
    pub is_dir: bool,
}

pub fn init() -> bool {
    if !ahci::is_available() {
        crate::log_warn!("fat32", "AHCI not available");
        return false;
    }

    let mbr = match ahci::read_sectors(0, 1) {
        Some(data) => data,
        None => {
            crate::log_error!("fat32", "Failed to read MBR");
            return false;
        }
    };

    if mbr[510] != 0x55 || mbr[511] != 0xAA {
        crate::log_warn!("fat32", "Invalid MBR signature");
        return init_partition(0);
    }

    for i in 0..4 {
        let offset = 446 + i * 16;
        let partition_type = mbr[offset + 4];
        let start_lba = u32::from_le_bytes([
            mbr[offset + 8],
            mbr[offset + 9],
            mbr[offset + 10],
            mbr[offset + 11],
        ]);

        if partition_type == 0x0B
            || partition_type == 0x0C
            || partition_type == 0x1B
            || partition_type == 0x1C
        {
            crate::log_info!("fat32", "Found FAT32 partition at sector {}", start_lba);
            return init_partition(start_lba as u64);
        }
    }

    crate::log_debug!("fat32", "No MBR partition, trying bare FAT32");
    init_partition(0)
}

fn init_partition(start_sector: u64) -> bool {
    let boot_sector = match ahci::read_sectors(start_sector, 1) {
        Some(data) => data,
        None => {
            crate::log_error!("fat32", "Failed to read boot sector");
            return false;
        }
    };

    let bpb = unsafe { &*(boot_sector.as_ptr() as *const Fat32Bpb) };

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
    let total_sectors = if bpb.total_sectors_16 != 0 {
        u16::from_le(bpb.total_sectors_16) as u32
    } else {
        u32::from_le(bpb.total_sectors_32)
    };

    let fat_start_sector = start_sector as u32 + reserved_sectors;
    let data_start_sector = fat_start_sector + (num_fats * fat_size);
    let data_sectors = total_sectors.saturating_sub(reserved_sectors + num_fats * fat_size);
    let total_clusters = data_sectors / sectors_per_cluster;

    unsafe {
        FS_INFO = Some(FsInfo {
            bytes_per_sector,
            sectors_per_cluster,
            reserved_sectors,
            num_fats,
            fat_size_sectors: fat_size,
            fat_start_sector,
            data_start_sector,
            root_cluster,
            total_sectors,
            total_clusters,
            partition_start: start_sector,
        });
    }

    crate::log_info!(
        "fat32",
        "FAT32: clusters={}, spc={}, fat_start={}, data_start={}, root_cluster={}",
        total_clusters,
        sectors_per_cluster,
        fat_start_sector,
        data_start_sector,
        root_cluster
    );

    true
}

#[inline]
fn fs_info() -> Option<&'static FsInfo> {
    unsafe { FS_INFO.as_ref() }
}

#[inline]
fn cluster_size() -> usize {
    let info = fs_info().unwrap();
    (info.sectors_per_cluster * info.bytes_per_sector) as usize
}

#[inline]
fn cluster_to_sector(cluster: u32) -> u32 {
    let info = fs_info().unwrap();
    info.data_start_sector + (cluster - 2) * info.sectors_per_cluster
}

#[inline]
fn entry_first_cluster(entry: &DirEntry) -> u32 {
    ((u16::from_le(entry.first_cluster_hi) as u32) << 16) | (u16::from_le(entry.first_cluster_lo) as u32)
}

fn set_entry_first_cluster(entry: &mut DirEntry, cluster: u32) {
    entry.first_cluster_hi = ((cluster >> 16) as u16).to_le();
    entry.first_cluster_lo = (cluster as u16).to_le();
}

fn read_fat_entry(cluster: u32) -> Option<u32> {
    let info = fs_info()?;
    let fat_offset = cluster * 4;
    let fat_sector = info.fat_start_sector + fat_offset / info.bytes_per_sector;
    let entry_offset = (fat_offset % info.bytes_per_sector) as usize;
    let sector_data = ahci::read_sectors(fat_sector as u64, 1)?;
    let entry = u32::from_le_bytes([
        sector_data[entry_offset],
        sector_data[entry_offset + 1],
        sector_data[entry_offset + 2],
        sector_data[entry_offset + 3],
    ]);
    Some(entry & 0x0FFF_FFFF)
}

fn write_fat_entry(cluster: u32, value: u32) -> bool {
    let info = match fs_info() {
        Some(info) => info,
        None => return false,
    };
    let fat_offset = cluster * 4;
    let entry_offset = (fat_offset % info.bytes_per_sector) as usize;
    let masked = value & 0x0FFF_FFFF;

    for fat_idx in 0..info.num_fats {
        let fat_sector = info.fat_start_sector + fat_idx * info.fat_size_sectors + fat_offset / info.bytes_per_sector;
        let mut sector_data = match ahci::read_sectors(fat_sector as u64, 1) {
            Some(data) => data.to_vec(),
            None => return false,
        };

        let existing = u32::from_le_bytes([
            sector_data[entry_offset],
            sector_data[entry_offset + 1],
            sector_data[entry_offset + 2],
            sector_data[entry_offset + 3],
        ]);
        let updated = (existing & 0xF000_0000) | masked;
        sector_data[entry_offset..entry_offset + 4].copy_from_slice(&updated.to_le_bytes());

        if !ahci::write_sectors(fat_sector as u64, &sector_data) {
            return false;
        }
    }

    true
}

fn read_cluster(cluster: u32) -> Option<Vec<u8>> {
    let info = fs_info()?;
    let sector = cluster_to_sector(cluster);
    let data = ahci::read_sectors(sector as u64, info.sectors_per_cluster as u16)?;
    Some(data.to_vec())
}

fn write_cluster(cluster: u32, data: &[u8]) -> bool {
    if data.len() != cluster_size() {
        return false;
    }
    ahci::write_sectors(cluster_to_sector(cluster) as u64, data)
}

fn zero_cluster(cluster: u32) -> bool {
    let buf = alloc::vec![0u8; cluster_size()];
    write_cluster(cluster, &buf)
}

fn get_cluster_chain(start_cluster: u32) -> Option<Vec<u32>> {
    if start_cluster < 2 {
        return Some(Vec::new());
    }

    let info = fs_info()?;
    let mut result = Vec::new();
    let mut cluster = start_cluster;
    let mut guard = 0u32;

    loop {
        if cluster < 2 || cluster >= info.total_clusters + 2 {
            return None;
        }
        result.push(cluster);
        let next = read_fat_entry(cluster)?;
        if next >= FAT_EOC {
            break;
        }
        if next == FAT_FREE || next == FAT_BAD {
            return None;
        }
        cluster = next;
        guard += 1;
        if guard > info.total_clusters {
            return None;
        }
    }

    Some(result)
}

fn read_cluster_chain(start_cluster: u32) -> Option<Vec<u8>> {
    let chain = get_cluster_chain(start_cluster)?;
    let mut result = Vec::with_capacity(chain.len() * cluster_size());
    for cluster in chain {
        let data = read_cluster(cluster)?;
        result.extend_from_slice(&data);
    }
    Some(result)
}

fn find_free_cluster() -> Option<u32> {
    let info = fs_info()?;
    for cluster in 2..(info.total_clusters + 2) {
        if read_fat_entry(cluster)? == FAT_FREE {
            return Some(cluster);
        }
    }
    None
}

fn allocate_cluster(prev: Option<u32>) -> Option<u32> {
    let cluster = find_free_cluster()?;
    if !write_fat_entry(cluster, 0x0FFF_FFFF) {
        return None;
    }
    if let Some(prev_cluster) = prev {
        if !write_fat_entry(prev_cluster, cluster) {
            let _ = write_fat_entry(cluster, FAT_FREE);
            return None;
        }
    }
    if !zero_cluster(cluster) {
        let _ = write_fat_entry(cluster, FAT_FREE);
        return None;
    }
    Some(cluster)
}

fn allocate_chain(count: usize) -> Option<Vec<u32>> {
    if count == 0 {
        return Some(Vec::new());
    }

    let mut chain = Vec::new();
    let mut prev = None;
    for _ in 0..count {
        let cluster = allocate_cluster(prev)?;
        prev = Some(cluster);
        chain.push(cluster);
    }
    Some(chain)
}

fn free_cluster_chain(start_cluster: u32) -> bool {
    if start_cluster < 2 {
        return true;
    }
    let chain = match get_cluster_chain(start_cluster) {
        Some(chain) => chain,
        None => return false,
    };
    for cluster in chain {
        if !write_fat_entry(cluster, FAT_FREE) {
            return false;
        }
    }
    true
}

fn decode_short_name(entry: &DirEntry) -> String {
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
}

fn decode_lfn_part(lfn: &LfnEntry) -> String {
    let mut chars = Vec::new();
    let name1 = lfn.name1;
    let name2 = lfn.name2;
    let name3 = lfn.name3;

    for c in name1.iter().chain(name2.iter()).chain(name3.iter()) {
        let c = u16::from_le(*c);
        if c == 0 || c == 0xFFFF {
            break;
        }
        chars.push(c);
    }

    chars.iter().filter_map(|&c| char::from_u32(c as u32)).collect()
}

fn find_in_directory_located(dir_cluster: u32, name: &str) -> Option<LocatedEntry> {
    let upper_name = name.to_uppercase();
    let dir_data = read_cluster_chain(dir_cluster)?;
    let mut i = 0usize;
    let mut lfn_name = String::new();

    while i + 32 <= dir_data.len() {
        let entry = unsafe { core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const DirEntry) };

        if entry.name[0] == 0x00 {
            break;
        }
        if entry.name[0] == 0xE5 {
            i += 32;
            continue;
        }

        if entry.attr == ATTR_LFN {
            let lfn = unsafe { core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const LfnEntry) };
            let part = decode_lfn_part(&lfn);
            if (lfn.order & 0x40) != 0 {
                lfn_name = part;
            } else {
                lfn_name = part + &lfn_name;
            }
            i += 32;
            continue;
        }

        if entry.attr & ATTR_VOLUME_ID != 0 {
            lfn_name.clear();
            i += 32;
            continue;
        }

        let short_name = decode_short_name(&entry);
        let check_name = if lfn_name.is_empty() {
            short_name.to_uppercase()
        } else {
            lfn_name.to_uppercase()
        };

        if check_name == upper_name || short_name.to_uppercase() == upper_name {
            return Some(LocatedEntry {
                entry,
                dir_cluster,
                entry_index: i / 32,
            });
        }

        lfn_name.clear();
        i += 32;
    }

    None
}

fn find_in_directory(dir_cluster: u32, name: &str) -> Option<DirEntry> {
    Some(find_in_directory_located(dir_cluster, name)?.entry)
}

fn resolve_dir_cluster(path: &str) -> Option<u32> {
    let info = fs_info()?;
    let trimmed = path.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Some(info.root_cluster);
    }

    let mut current_cluster = info.root_cluster;
    for part in trimmed.split(['/', '\\']).filter(|s| !s.is_empty()) {
        let entry = find_in_directory(current_cluster, part)?;
        if entry.attr & ATTR_DIRECTORY == 0 {
            return None;
        }
        current_cluster = entry_first_cluster(&entry);
    }
    Some(current_cluster)
}

fn split_parent(path: &str) -> Option<(String, String)> {
    let trimmed = path.trim_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }

    match trimmed.rfind(['/', '\\']) {
        Some(pos) => {
            let parent = &trimmed[..pos];
            let name = &trimmed[pos + 1..];
            Some((format!("/{}", parent), name.to_string()))
        }
        None => Some(("/".to_string(), trimmed.to_string())),
    }
}

fn encode_short_component(name: &str) -> Option<[u8; 11]> {
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    if name.contains(['/', '\\']) {
        return None;
    }

    let mut parts = name.split('.');
    let base = parts.next()?;
    let ext = parts.next().unwrap_or("");
    if parts.next().is_some() || base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }

    fn valid_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '$' | '~')
    }

    if !base.chars().all(valid_char) || !ext.chars().all(valid_char) {
        return None;
    }

    let mut out = [b' '; 11];
    for (idx, c) in base.chars().enumerate() {
        out[idx] = c.to_ascii_uppercase() as u8;
    }
    for (idx, c) in ext.chars().enumerate() {
        out[8 + idx] = c.to_ascii_uppercase() as u8;
    }
    Some(out)
}

fn build_entry(name: [u8; 11], attr: u8, first_cluster: u32, size: u32) -> DirEntry {
    let mut entry = DirEntry {
        name,
        attr,
        nt_reserved: 0,
        create_time_tenth: 0,
        create_time: 0,
        create_date: 0,
        last_access_date: 0,
        first_cluster_hi: 0,
        write_time: 0,
        write_date: 0,
        first_cluster_lo: 0,
        file_size: size.to_le(),
    };
    set_entry_first_cluster(&mut entry, first_cluster);
    entry
}

fn locate_entry_slot(dir_cluster: u32, entry_index: usize) -> Option<(u32, usize)> {
    let chain = get_cluster_chain(dir_cluster)?;
    let entries_per_cluster = cluster_size() / 32;
    let cluster = *chain.get(entry_index / entries_per_cluster)?;
    let offset = (entry_index % entries_per_cluster) * 32;
    Some((cluster, offset))
}

fn write_dir_entry_at(dir_cluster: u32, entry_index: usize, entry: &DirEntry) -> bool {
    let (cluster, offset) = match locate_entry_slot(dir_cluster, entry_index) {
        Some(v) => v,
        None => return false,
    };

    let mut cluster_data = match read_cluster(cluster) {
        Some(data) => data,
        None => return false,
    };

    let entry_bytes = unsafe {
        core::slice::from_raw_parts((entry as *const DirEntry) as *const u8, 32)
    };
    cluster_data[offset..offset + 32].copy_from_slice(entry_bytes);
    write_cluster(cluster, &cluster_data)
}

fn mark_dir_entry_deleted(dir_cluster: u32, entry_index: usize) -> bool {
    let (cluster, offset) = match locate_entry_slot(dir_cluster, entry_index) {
        Some(v) => v,
        None => return false,
    };
    let mut cluster_data = match read_cluster(cluster) {
        Some(data) => data,
        None => return false,
    };
    cluster_data[offset] = 0xE5;
    write_cluster(cluster, &cluster_data)
}

fn find_free_dir_entry(dir_cluster: u32) -> Option<usize> {
    let chain = get_cluster_chain(dir_cluster)?;
    let entries_per_cluster = cluster_size() / 32;

    for (cluster_idx, cluster) in chain.iter().enumerate() {
        let data = read_cluster(*cluster)?;
        for slot in 0..entries_per_cluster {
            let offset = slot * 32;
            let first = data[offset];
            if first == 0x00 || first == 0xE5 {
                return Some(cluster_idx * entries_per_cluster + slot);
            }
        }
    }

    let new_cluster = allocate_cluster(chain.last().copied())?;
    if !zero_cluster(new_cluster) {
        return None;
    }
    Some(chain.len() * entries_per_cluster)
}

fn create_dir_entry(parent_cluster: u32, entry: &DirEntry) -> bool {
    let slot = match find_free_dir_entry(parent_cluster) {
        Some(slot) => slot,
        None => return false,
    };
    write_dir_entry_at(parent_cluster, slot, entry)
}

fn ensure_parent_and_name(path: &str) -> Option<(u32, [u8; 11])> {
    let (parent_path, leaf_name) = split_parent(path)?;
    let parent_cluster = resolve_dir_cluster(&parent_path)?;
    let encoded = encode_short_component(&leaf_name)?;
    Some((parent_cluster, encoded))
}

fn create_empty_file(path: &str) -> bool {
    let (parent_cluster, name) = match ensure_parent_and_name(path) {
        Some(v) => v,
        None => return false,
    };
    let (_, leaf_name) = match split_parent(path) {
        Some(v) => v,
        None => return false,
    };
    if find_in_directory(parent_cluster, &leaf_name).is_some() {
        return true;
    }
    let entry = build_entry(name, ATTR_ARCHIVE, 0, 0);
    create_dir_entry(parent_cluster, &entry)
}

fn write_bytes_to_chain(chain: &[u32], data: &[u8]) -> bool {
    let size = cluster_size();
    for (idx, cluster) in chain.iter().enumerate() {
        let start = idx * size;
        let end = core::cmp::min(start + size, data.len());
        let mut block = alloc::vec![0u8; size];
        if start < end {
            block[..end - start].copy_from_slice(&data[start..end]);
        }
        if !write_cluster(*cluster, &block) {
            return false;
        }
    }
    true
}

pub fn write_file(path: &str, data: &[u8]) -> bool {
    let located = match find_path_entry(path) {
        Some(located) => located,
        None => {
            if !create_empty_file(path) {
                return false;
            }
            match find_path_entry(path) {
                Some(located) => located,
                None => return false,
            }
        }
    };

    if located.entry.attr & ATTR_DIRECTORY != 0 {
        return false;
    }

    let old_cluster = entry_first_cluster(&located.entry);
    let needed_clusters = if data.is_empty() {
        0
    } else {
        (data.len() + cluster_size() - 1) / cluster_size()
    };

    let new_chain = match allocate_chain(needed_clusters) {
        Some(chain) => chain,
        None => return false,
    };

    if !new_chain.is_empty() && !write_bytes_to_chain(&new_chain, data) {
        let _ = free_cluster_chain(new_chain[0]);
        return false;
    }

    let mut updated = located.entry;
    set_entry_first_cluster(&mut updated, new_chain.first().copied().unwrap_or(0));
    updated.file_size = (data.len() as u32).to_le();

    if !write_dir_entry_at(located.dir_cluster, located.entry_index, &updated) {
        if let Some(first) = new_chain.first().copied() {
            let _ = free_cluster_chain(first);
        }
        return false;
    }

    if old_cluster >= 2 {
        let _ = free_cluster_chain(old_cluster);
    }

    true
}

pub fn mkdir(path: &str) -> bool {
    let (parent_cluster, name) = match ensure_parent_and_name(path) {
        Some(v) => v,
        None => return false,
    };
    let (_, leaf_name) = match split_parent(path) {
        Some(v) => v,
        None => return false,
    };
    if find_in_directory(parent_cluster, &leaf_name).is_some() {
        return false;
    }

    let cluster = match allocate_cluster(None) {
        Some(cluster) => cluster,
        None => return false,
    };

    let mut initial = alloc::vec![0u8; cluster_size()];
    let dot = build_entry([b'.', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '], ATTR_DIRECTORY, cluster, 0);
    let dotdot = build_entry([b'.', b'.', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' ', b' '], ATTR_DIRECTORY, parent_cluster, 0);
    let dot_bytes = unsafe { core::slice::from_raw_parts((&dot as *const DirEntry) as *const u8, 32) };
    let dotdot_bytes = unsafe { core::slice::from_raw_parts((&dotdot as *const DirEntry) as *const u8, 32) };
    initial[0..32].copy_from_slice(dot_bytes);
    initial[32..64].copy_from_slice(dotdot_bytes);
    if !write_cluster(cluster, &initial) {
        let _ = free_cluster_chain(cluster);
        return false;
    }

    let entry = build_entry(name, ATTR_DIRECTORY, cluster, 0);
    if !create_dir_entry(parent_cluster, &entry) {
        let _ = free_cluster_chain(cluster);
        return false;
    }

    true
}

fn is_directory_empty(cluster: u32) -> bool {
    let data = match read_cluster_chain(cluster) {
        Some(data) => data,
        None => return false,
    };

    let mut i = 0usize;
    while i + 32 <= data.len() {
        let entry = unsafe { core::ptr::read_unaligned(data.as_ptr().add(i) as *const DirEntry) };
        if entry.name[0] == 0x00 {
            break;
        }
        if entry.name[0] == 0xE5 || entry.attr == ATTR_LFN || entry.attr & ATTR_VOLUME_ID != 0 {
            i += 32;
            continue;
        }
        let name = decode_short_name(&entry);
        if name != "." && name != ".." {
            return false;
        }
        i += 32;
    }

    true
}

pub fn unlink(path: &str) -> bool {
    let located = match find_path_entry(path) {
        Some(located) => located,
        None => return false,
    };
    if located.entry.attr & ATTR_DIRECTORY != 0 {
        return false;
    }
    let first_cluster = entry_first_cluster(&located.entry);
    if !mark_dir_entry_deleted(located.dir_cluster, located.entry_index) {
        return false;
    }
    if first_cluster >= 2 {
        let _ = free_cluster_chain(first_cluster);
    }
    true
}

pub fn rmdir(path: &str) -> bool {
    let located = match find_path_entry(path) {
        Some(located) => located,
        None => return false,
    };
    if located.entry.attr & ATTR_DIRECTORY == 0 {
        return false;
    }
    let cluster = entry_first_cluster(&located.entry);
    if cluster < 2 || !is_directory_empty(cluster) {
        return false;
    }
    if !mark_dir_entry_deleted(located.dir_cluster, located.entry_index) {
        return false;
    }
    free_cluster_chain(cluster)
}

pub fn rename(old_path: &str, new_path: &str) -> bool {
    let old_located = match find_path_entry(old_path) {
        Some(located) => located,
        None => return false,
    };
    let (old_parent_path, _) = match split_parent(old_path) {
        Some(v) => v,
        None => return false,
    };
    let (new_parent_path, new_leaf) = match split_parent(new_path) {
        Some(v) => v,
        None => return false,
    };

    if old_parent_path != new_parent_path {
        return false;
    }
    let new_parent_cluster = match resolve_dir_cluster(&new_parent_path) {
        Some(cluster) => cluster,
        None => return false,
    };
    if find_in_directory(new_parent_cluster, &new_leaf).is_some() {
        return false;
    }
    let new_name = match encode_short_component(&new_leaf) {
        Some(name) => name,
        None => return false,
    };

    let mut updated = old_located.entry;
    updated.name = new_name;
    write_dir_entry_at(old_located.dir_cluster, old_located.entry_index, &updated)
}

fn find_path_entry(path: &str) -> Option<LocatedEntry> {
    let info = fs_info()?;
    let trimmed = path.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() {
        return None;
    }

    let mut current_cluster = info.root_cluster;
    let parts: Vec<&str> = trimmed.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    for (idx, part) in parts.iter().enumerate() {
        let located = find_in_directory_located(current_cluster, part)?;
        if idx == parts.len() - 1 {
            return Some(located);
        }
        if located.entry.attr & ATTR_DIRECTORY == 0 {
            return None;
        }
        current_cluster = entry_first_cluster(&located.entry);
    }
    None
}

pub fn open(path: &str) -> Option<Vec<u8>> {
    let located = find_path_entry(path)?;
    if located.entry.attr & ATTR_DIRECTORY != 0 {
        return None;
    }
    let first_cluster = entry_first_cluster(&located.entry);
    let file_size = u32::from_le(located.entry.file_size) as usize;
    if first_cluster < 2 {
        return Some(Vec::new());
    }
    let mut data = read_cluster_chain(first_cluster)?;
    data.truncate(file_size);
    Some(data)
}

pub fn is_available() -> bool {
    unsafe { FS_INFO.is_some() }
}

pub fn stat_path(path: &str) -> Option<FileStat> {
    let trimmed = path.trim_start_matches(['/', '\\']);
    if trimmed.is_empty() {
        return Some(FileStat { size: 0, is_dir: true });
    }
    let located = find_path_entry(path)?;
    Some(FileStat {
        size: u32::from_le(located.entry.file_size) as u64,
        is_dir: located.entry.attr & ATTR_DIRECTORY != 0,
    })
}

pub fn list_directory(path: &str) -> Option<Vec<String>> {
    let current_cluster = resolve_dir_cluster(path)?;
    let dir_data = read_cluster_chain(current_cluster)?;
    let mut files = Vec::new();
    let mut i = 0usize;
    let mut lfn_name = String::new();

    while i + 32 <= dir_data.len() {
        let entry = unsafe { core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const DirEntry) };

        if entry.name[0] == 0x00 {
            break;
        }
        if entry.name[0] == 0xE5 {
            i += 32;
            continue;
        }
        if entry.attr == ATTR_LFN {
            let lfn = unsafe { core::ptr::read_unaligned(dir_data.as_ptr().add(i) as *const LfnEntry) };
            let part = decode_lfn_part(&lfn);
            if (lfn.order & 0x40) != 0 {
                lfn_name = part;
            } else {
                lfn_name = part + &lfn_name;
            }
            i += 32;
            continue;
        }
        if entry.attr & ATTR_VOLUME_ID != 0 {
            lfn_name.clear();
            i += 32;
            continue;
        }

        let display_name = if lfn_name.is_empty() {
            decode_short_name(&entry)
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
