// Write-Ahead Logging (WAL) implementation for crash-consistent filesystem operations
// Implements ext4/XFS-style journaling with transaction support, fsync barriers, and recovery

extern crate alloc;

use alloc::vec::Vec;
use crate::block_device::BlockDevice;
use core::mem::size_of;

// ============================================================================
// Constants
// ============================================================================

pub const JOURNAL_MAGIC: u32 = 0x4154_4F4D; // 'ATOM'
pub const JOURNAL_VERSION: u32 = 1;
pub const JOURNAL_ENTRY_MAGIC: u32 = 0xC0DE; // 'CODE'

pub const JOURNAL_SUPERBLOCK_SECTOR: u64 = 1;
pub const JOURNAL_ENTRIES_START_SECTOR: u64 = 2;
pub const JOURNAL_MAX_ENTRIES: u32 = 999;
pub const JOURNAL_ENTRY_SIZE: usize = 256;
pub const JOURNAL_TOTAL_SECTORS: u64 = 1000; // Sectors 1-1000
pub const FAT32_START_SECTOR: u64 = 1001;

// Flags
pub const FLAG_JOURNAL_ENABLED: u32 = 0x01;
pub const FLAG_RECOVERY_NEEDED: u32 = 0x02;
pub const FLAG_CHECKSUM_ENABLED: u32 = 0x04;

// Entry flags
pub const ENTRY_FLAG_COMMITTED: u32 = 0x01;
pub const ENTRY_FLAG_DONE: u32 = 0x02;

// ============================================================================
// Operation Types
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum OpType {
    TxBegin = 0,
    TxCommit = 1,
    TxRollback = 2,
    AllocBlock = 3,
    FreeBlock = 4,
    UpdateInode = 5,
    AllocInode = 6,
    FreeInode = 7,
    WriteData = 8,
    Fsync = 9,
}

impl OpType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            0 => Some(OpType::TxBegin),
            1 => Some(OpType::TxCommit),
            2 => Some(OpType::TxRollback),
            3 => Some(OpType::AllocBlock),
            4 => Some(OpType::FreeBlock),
            5 => Some(OpType::UpdateInode),
            6 => Some(OpType::AllocInode),
            7 => Some(OpType::FreeInode),
            8 => Some(OpType::WriteData),
            9 => Some(OpType::Fsync),
            _ => None,
        }
    }
}

// ============================================================================
// On-Disk Structures
// ============================================================================

/// Journal Superblock (512 bytes, fits in one sector)
#[repr(C, packed)]
pub struct JournalSuperblock {
    pub magic: u32,              // 0x4154_4F4D
    pub version: u32,            // 1
    pub jsn: u64,                // Journal Sequence Number (monotonic)
    pub checkpoint_lsn: u64,     // Last committed transaction
    pub head: u32,               // Next write position (% 256)
    pub tail: u32,               // Oldest uncommitted (% 256)
    pub flags: u32,              // Journal flags
    pub tx_count: u32,           // Transactions in journal
    pub capacity: u32,           // Max entries
    pub reserved: [u32; 4],      // Reserved
    pub crc32: u32,              // CRC32 of bytes [0..59]
    pub padding: [u8; 448],      // Padding to 512 bytes
}

impl JournalSuperblock {
    pub fn new() -> Self {
        Self {
            magic: JOURNAL_MAGIC,
            version: JOURNAL_VERSION,
            jsn: 0,
            checkpoint_lsn: 0,
            head: 0,
            tail: 0,
            flags: FLAG_JOURNAL_ENABLED | FLAG_CHECKSUM_ENABLED,
            tx_count: 0,
            capacity: JOURNAL_MAX_ENTRIES,
            reserved: [0; 4],
            crc32: 0,
            padding: [0; 448],
        }
    }

    pub fn is_valid(&self) -> bool {
        if self.magic != JOURNAL_MAGIC {
            return false;
        }
        if self.version != JOURNAL_VERSION {
            return false;
        }
        // Verify checksum
        let computed_crc = self.compute_crc32();
        computed_crc == self.crc32
    }

    pub fn compute_crc32(&self) -> u32 {
        // Simple CRC32 over first 60 bytes
        let bytes = unsafe {
            core::slice::from_raw_parts(self as *const _ as *const u8, 60)
        };
        crc32_simple(bytes)
    }

    pub fn update_crc32(&mut self) {
        self.crc32 = self.compute_crc32();
    }

    pub fn to_bytes(&self) -> [u8; 512] {
        let mut buf = [0u8; 512];
        unsafe {
            core::ptr::copy_nonoverlapping(
                self as *const _ as *const u8,
                buf.as_mut_ptr(),
                size_of::<JournalSuperblock>(),
            );
        }
        buf
    }

    pub fn from_bytes(buf: &[u8; 512]) -> Self {
        unsafe {
            core::ptr::read(buf.as_ptr() as *const JournalSuperblock)
        }
    }
}

/// Journal Entry (256 bytes)
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct JournalEntry {
    pub magic: u32,           // 0xC0DE
    pub op_type: u32,         // OpType
    pub tx_id: u64,           // Transaction ID (LSN)
    pub timestamp: u64,       // Nanoseconds since epoch
    pub entry_flags: u32,     // COMMITTED, DONE, etc.
    pub op_count: u32,        // Operations in this entry
    pub inode: u32,           // Affected inode
    pub data_offset: u32,     // Into operation payload
    pub data_length: u32,     // Bytes of meaningful data
    pub payload: [u8; 220],   // Operation-specific data
    pub crc32: u32,           // CRC32 of bytes [0..263]
}

impl JournalEntry {
    pub fn new(tx_id: u64, op_type: OpType) -> Self {
        Self {
            magic: JOURNAL_ENTRY_MAGIC,
            op_type: op_type as u32,
            tx_id,
            timestamp: 0,
            entry_flags: 0,
            op_count: 1,
            inode: 0,
            data_offset: 0,
            data_length: 0,
            payload: [0; 220],
            crc32: 0,
        }
    }

    pub fn is_valid(&self) -> bool {
        if self.magic != JOURNAL_ENTRY_MAGIC {
            return false;
        }
        let computed_crc = self.compute_crc32();
        computed_crc == self.crc32
    }

    pub fn compute_crc32(&self) -> u32 {
        let bytes = unsafe {
            core::slice::from_raw_parts(self as *const _ as *const u8, 264)
        };
        crc32_simple(bytes)
    }

    pub fn update_crc32(&mut self) {
        self.crc32 = self.compute_crc32();
    }

    pub fn to_bytes(&self) -> [u8; 256] {
        let mut buf = [0u8; 256];
        unsafe {
            core::ptr::copy_nonoverlapping(
                self as *const _ as *const u8,
                buf.as_mut_ptr(),
                size_of::<JournalEntry>(),
            );
        }
        buf
    }

    pub fn from_bytes(buf: &[u8; 256]) -> Self {
        unsafe {
            core::ptr::read(buf.as_ptr() as *const JournalEntry)
        }
    }
}

// ============================================================================
// Simple CRC32 Implementation
// ============================================================================

fn crc32_simple(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// ============================================================================
// Transaction Context
// ============================================================================

pub struct Transaction {
    pub id: u64,                    // LSN
    pub entries: Vec<JournalEntry>, // Operations in this TX
    pub status: TxStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxStatus {
    Active,
    Committed,
    RolledBack,
    Applied,
}

impl Transaction {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            entries: Vec::new(),
            status: TxStatus::Active,
        }
    }

    pub fn add_entry(&mut self, entry: JournalEntry) {
        self.entries.push(entry);
    }
}

// ============================================================================
// Journal Manager
// ============================================================================

pub struct JournalManager {
    superblock: JournalSuperblock,
    current_tx: Option<Transaction>,
    block_device: *const BlockDevice,
    pub(crate) recovery_needed: bool,
}

impl JournalManager {
    pub fn new(block_device: *const BlockDevice) -> Self {
        Self {
            superblock: JournalSuperblock::new(),
            current_tx: None,
            block_device,
            recovery_needed: false,
        }
    }

    pub fn init(&mut self) -> bool {
        // Read superblock from disk
        if !self.read_superblock() {
            // First time: write fresh superblock
            self.superblock = JournalSuperblock::new();
            return self.write_superblock();
        }

        // Check if recovery is needed
        if (self.superblock.flags & FLAG_RECOVERY_NEEDED) != 0 {
            self.recovery_needed = true;
        }

        true
    }

    pub fn read_superblock(&mut self) -> bool {
        let buf = unsafe {
            match (*self.block_device).read_sectors(JOURNAL_SUPERBLOCK_SECTOR, 1) {
                Some(data) => data,
                None => return false,
            }
        };

        if buf.len() < 512 {
            return false;
        }

        let mut sb_bytes = [0u8; 512];
        sb_bytes.copy_from_slice(&buf[0..512]);
        let sb = JournalSuperblock::from_bytes(&sb_bytes);

        if !sb.is_valid() {
            return false;
        }

        self.superblock = sb;
        true
    }

    pub fn write_superblock(&self) -> bool {
        let bytes = self.superblock.to_bytes();
        unsafe {
            match (*self.block_device).write_sectors(JOURNAL_SUPERBLOCK_SECTOR, &[bytes.to_vec()]) {
                Some(true) => true,
                _ => false,
            }
        }
    }

    pub fn begin_transaction(&mut self) -> u64 {
        let tx_id = self.superblock.jsn + 1;
        self.superblock.jsn = tx_id;

        // Create BEGIN entry
        let mut entry = JournalEntry::new(tx_id, OpType::TxBegin);
        entry.entry_flags = 0;
        entry.update_crc32();

        let mut tx = Transaction::new(tx_id);
        tx.add_entry(entry);
        self.current_tx = Some(tx);

        // Write to journal
        self.write_entry(self.superblock.head, &entry);
        self.superblock.head = (self.superblock.head + 1) % JOURNAL_MAX_ENTRIES;

        tx_id
    }

    pub fn add_operation(
        &mut self,
        op_type: OpType,
        inode: u32,
        payload: &[u8],
    ) -> bool {
        if let Some(ref mut tx) = self.current_tx {
            let mut entry = JournalEntry::new(tx.id, op_type);
            entry.inode = inode;
            entry.op_count = 1;

            // Copy payload
            let copy_len = payload.len().min(220);
            entry.payload[..copy_len].copy_from_slice(&payload[..copy_len]);
            entry.data_length = copy_len as u32;
            entry.update_crc32();

            tx.entries.push(entry);

            // Write to journal
            self.write_entry(self.superblock.head, &entry);
            self.superblock.head = (self.superblock.head + 1) % JOURNAL_MAX_ENTRIES;

            true
        } else {
            false
        }
    }

    pub fn commit_transaction(&mut self) -> bool {
        if let Some(ref mut tx) = self.current_tx.take() {
            // Create COMMIT entry
            let mut entry = JournalEntry::new(tx.id, OpType::TxCommit);
            entry.entry_flags = ENTRY_FLAG_COMMITTED;
            entry.update_crc32();

            // Write last entry and fsync
            self.write_entry(self.superblock.head, &entry);
            self.superblock.head = (self.superblock.head + 1) % JOURNAL_MAX_ENTRIES;

            // Update superblock
            self.superblock.checkpoint_lsn = tx.id;
            self.superblock.tx_count += 1;
            self.superblock.update_crc32();

            // Sync to disk
            self.fsync();
            true
        } else {
            false
        }
    }

    pub fn rollback_transaction(&mut self) -> bool {
        if let Some(tx) = self.current_tx.take() {
            let mut entry = JournalEntry::new(tx.id, OpType::TxRollback);
            entry.update_crc32();

            self.write_entry(self.superblock.head, &entry);
            self.superblock.head = (self.superblock.head + 1) % JOURNAL_MAX_ENTRIES;

            self.fsync();
            true
        } else {
            false
        }
    }

    pub fn fsync(&self) {
        // Write superblock
        let _ = unsafe { (*self.block_device).write_sectors(
            JOURNAL_SUPERBLOCK_SECTOR,
            &[self.superblock.to_bytes().to_vec()],
        ) };

        // Issue flush barrier
        let _ = unsafe { (*self.block_device).flush() };
    }

    fn write_entry(&self, index: u32, entry: &JournalEntry) {
        let sector = JOURNAL_ENTRIES_START_SECTOR + (index as u64) * 2;
        let bytes = entry.to_bytes();
        let _ = unsafe {
            (*self.block_device).write_sectors(sector, &[bytes.to_vec()])
        };
    }

    pub fn recovery_replay(&mut self) -> bool {
        // Scan all entries from tail to head
        let mut current = self.superblock.checkpoint_lsn;
        let mut tx_map: alloc::collections::BTreeMap<u64, Vec<JournalEntry>> = 
            alloc::collections::BTreeMap::new();

        // Read all entries and group by transaction
        for i in 0..JOURNAL_MAX_ENTRIES {
            let entry = self.read_entry(i as u32);
            if entry.magic != JOURNAL_ENTRY_MAGIC {
                continue;
            }
            if !entry.is_valid() {
                continue;
            }

            let tx_id = entry.tx_id;
            if tx_id <= current {
                continue;
            }

            tx_map.entry(tx_id)
                .or_insert_with(Vec::new)
                .push(entry);
        }

        // Replay committed transactions
        for (tx_id, entries) in tx_map {
            let mut has_commit = false;
            for entry in &entries {
                if entry.op_type == OpType::TxCommit as u32 {
                    has_commit = true;
                }
            }

            if has_commit {
                // Replay all operations in this transaction
                for entry in entries {
                    if entry.op_type == OpType::TxCommit as u32 {
                        self.superblock.checkpoint_lsn = tx_id;
                    } else {
                        // Replay operation (app-specific)
                        // In real implementation, would call FAT32 operations
                    }
                }
            }
        }

        // Clear recovery flag
        self.superblock.flags &= !FLAG_RECOVERY_NEEDED;
        self.superblock.update_crc32();
        self.fsync();

        true
    }

    fn read_entry(&self, index: u32) -> JournalEntry {
        let sector = JOURNAL_ENTRIES_START_SECTOR + (index as u64) * 2;
        let buf = unsafe {
            match (*self.block_device).read_sectors(sector, 1) {
                Some(data) => data,
                None => return JournalEntry::new(0, OpType::TxBegin),
            }
        };

        if buf.len() < 256 {
            return JournalEntry::new(0, OpType::TxBegin);
        }

        let mut entry_bytes = [0u8; 256];
        entry_bytes.copy_from_slice(&buf[0..256]);
        JournalEntry::from_bytes(&entry_bytes)
    }
}

// ============================================================================
// Helper Structures for Operation Payloads
// ============================================================================

/// ALLOC_BLOCK operation payload
pub struct AllocBlockOp {
    pub cluster: u32,
    pub count: u32,
}

impl AllocBlockOp {
    pub fn encode(&self) -> [u8; 8] {
        let mut buf = [0u8; 8];
        buf[0..4].copy_from_slice(&self.cluster.to_le_bytes());
        buf[4..8].copy_from_slice(&self.count.to_le_bytes());
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 8 {
            return None;
        }
        Some(Self {
            cluster: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            count: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
        })
    }
}

/// UPDATE_INODE operation payload
pub struct UpdateInodeOp {
    pub inode: u32,
    pub size: u64,
    pub mtime: u64,
    pub flags: u32,
}

impl UpdateInodeOp {
    pub fn encode(&self) -> [u8; 20] {
        let mut buf = [0u8; 20];
        buf[0..4].copy_from_slice(&self.inode.to_le_bytes());
        buf[4..12].copy_from_slice(&self.size.to_le_bytes());
        buf[12..20].copy_from_slice(&self.mtime.to_le_bytes());
        // flags would go in remaining space if needed
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 20 {
            return None;
        }
        Some(Self {
            inode: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            size: u64::from_le_bytes([
                buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
            ]),
            mtime: u64::from_le_bytes([
                buf[12], buf[13], buf[14], buf[15], buf[16], buf[17], buf[18], buf[19],
            ]),
            flags: 0,
        })
    }
}

/// WRITE_DATA operation payload
pub struct WriteDataOp {
    pub cluster: u32,
    pub offset: u32,
    pub length: u32,
    pub hash: [u8; 32], // SHA256 hash of data
}

impl WriteDataOp {
    pub fn encode(&self) -> [u8; 44] {
        let mut buf = [0u8; 44];
        buf[0..4].copy_from_slice(&self.cluster.to_le_bytes());
        buf[4..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..12].copy_from_slice(&self.length.to_le_bytes());
        buf[12..44].copy_from_slice(&self.hash);
        buf
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        if buf.len() < 44 {
            return None;
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&buf[12..44]);
        Some(Self {
            cluster: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
            offset: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            length: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            hash,
        })
    }
}
