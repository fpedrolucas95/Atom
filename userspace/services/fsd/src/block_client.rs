// block_client.rs — IPC client for the block device service
//
// Sends BlockRead / BlockWrite / BlockFlush messages to PORT_BLOCK_SERVICE
// and waits for the reply.  This is the ONLY place that performs actual disk
// I/O in fsd.  All higher layers call these functions.
//
// Protocol (libipc/src/messages.rs):
//   BlockRead  (1000) → BlockReadReply  (1001)
//   BlockWrite (1002) → BlockWriteReply (1003)
//   BlockFlush (1004) → BlockFlushReply (1005)
//
// Wire format for request:
//   [MessageHeader: 16B][BlockReadReq: 12B][data (write only)]
// Wire format for reply:
//   [MessageHeader: 16B][status: u64][bytes: u64]

#![allow(dead_code)]

extern crate alloc;
use alloc::vec::Vec;

use atom_syscall::ipc;
use atom_abi::{PORT_BLOCK_SERVICE, ESUCCESS};
use libipc::messages::{BlockIdentifyInfo, MessageHeader, MessageType};

use crate::error::{VfsError, VfsResult};

// ── Constants ────────────────────────────────────────────────────────────────

/// Maximum bytes transferred in a single BlockRead/Write call.
/// Matches the kernel IPC message size limit.
pub const BLOCK_MAX_TRANSFER: usize = 65536;

/// Size of BlockReadReq / BlockWriteReq fields (lba: u64, device_id: u16, _pad: u16, count: u32).
/// Must match libipc::messages::BlockReadReq.
const BLOCK_REQ_PAYLOAD: usize = 12;

// ── BlockClient ──────────────────────────────────────────────────────────────

/// Client-side handle for the block device service.
///
/// `device_id` identifies which physical device/partition this client
/// represents (0 = first disk, 1 = second, …).
pub struct BlockClient {
    port:      u64,
    device_id: u16,
    /// Bytes per logical sector (queried from BlockIdentify at init).
    pub sector_size: u32,
    /// Total sectors on the device.
    pub total_sectors: u64,
}

impl BlockClient {
    /// Create a new client.  Does NOT perform any I/O during construction.
    pub fn new(device_id: u16) -> Self {
        BlockClient {
            port:         PORT_BLOCK_SERVICE,
            device_id,
            sector_size:  512,
            total_sectors: 0,
        }
    }

    // ── Public interface ─────────────────────────────────────────────────

    /// Query the block device for its geometry (sector size, total sectors,
    /// read-only flag, model string).
    ///
    /// This MUST be called once before any read/write operation so that
    /// `sector_size` and `total_sectors` are accurate.
    ///
    /// Returns `Err(VfsError::Io)` if the block service is unreachable.
    pub fn identify(&mut self) -> VfsResult<BlockIdentifyInfo> {
        // Request: [header][device_id: u16][_pad: 6B]
        let hdr = MessageHeader::new(MessageType::BlockIdentify, 8);
        let mut msg = alloc::vec![0u8; MessageHeader::SIZE + 8];
        msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        msg[MessageHeader::SIZE..MessageHeader::SIZE + 2]
            .copy_from_slice(&self.device_id.to_le_bytes());

        ipc::send(self.port, &msg).map_err(|_| VfsError::Io)?;

        // Reply: [header][status: u64][BlockIdentifyInfo: 256B]
        let reply_size = MessageHeader::SIZE + 8 + BlockIdentifyInfo::SIZE;
        let mut reply = alloc::vec![0u8; reply_size];
        ipc::recv(self.port, &mut reply).map_err(|_| VfsError::Io)?;

        let status = u64::from_le_bytes(
            reply[MessageHeader::SIZE..MessageHeader::SIZE + 8]
                .try_into().unwrap_or([0u8; 8]));
        if status != ESUCCESS { return Err(VfsError::Io); }

        let info_off = MessageHeader::SIZE + 8;
        let info = BlockIdentifyInfo::from_bytes(&reply[info_off..])
            .ok_or(VfsError::Io)?;

        // Update cached geometry so all subsequent I/O uses correct values
        self.sector_size   = if info.sector_size > 0 { info.sector_size } else { 512 };
        self.total_sectors = info.total_sectors;

        Ok(info)
    }

    /// Read `count` bytes starting at logical byte address `byte_offset`
    /// into `buf`.  `buf` must be exactly `count` bytes.
    ///
    /// Internally aligns to sector boundaries and may issue multiple IPC
    /// calls if `count > BLOCK_MAX_TRANSFER`.
    pub fn read_bytes(&self, byte_offset: u64, buf: &mut [u8]) -> VfsResult<()> {
        let sector_size = self.sector_size as u64;
        let lba_start   = byte_offset / sector_size;
        let offset_in_sector = (byte_offset % sector_size) as usize;
        let total_bytes = buf.len();

        // How many sectors we need to cover the range
        let bytes_needed = offset_in_sector + total_bytes;
        let sectors_needed = (bytes_needed + self.sector_size as usize - 1)
                              / self.sector_size as usize;

        // Scratch buffer aligned to full sectors
        let scratch_len = sectors_needed * self.sector_size as usize;
        let mut scratch = alloc::vec![0u8; scratch_len];

        self.read_sectors(lba_start, sectors_needed as u32, &mut scratch)?;

        buf.copy_from_slice(&scratch[offset_in_sector .. offset_in_sector + total_bytes]);
        Ok(())
    }

    /// Write `data` at `byte_offset`.
    ///
    /// Performs a read-modify-write for the first and last partial sectors.
    pub fn write_bytes(&self, byte_offset: u64, data: &[u8]) -> VfsResult<()> {
        let sector_size  = self.sector_size as u64;
        let lba_start    = byte_offset / sector_size;
        let off_in_first = (byte_offset % sector_size) as usize;
        let total_bytes  = data.len();

        let bytes_needed  = off_in_first + total_bytes;
        let sectors_needed = (bytes_needed + self.sector_size as usize - 1)
                              / self.sector_size as usize;

        let scratch_len = sectors_needed * self.sector_size as usize;
        let mut scratch = alloc::vec![0u8; scratch_len];

        // Read existing content for partial first/last sector
        if off_in_first != 0
            || total_bytes % self.sector_size as usize != 0
        {
            self.read_sectors(lba_start, sectors_needed as u32, &mut scratch)?;
        }

        scratch[off_in_first .. off_in_first + total_bytes].copy_from_slice(data);
        self.write_sectors(lba_start, sectors_needed as u32, &scratch)
    }

    /// Issue a write barrier.  Returns Ok(()) only after the device confirms
    /// the flush (all prior writes are durable).
    pub fn flush(&self) -> VfsResult<()> {
        // Build request: [header][device_id: u16][_pad: 6B]
        let hdr = MessageHeader::new(MessageType::BlockFlush, 8);
        let mut msg = alloc::vec![0u8; MessageHeader::SIZE + 8];
        msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        msg[MessageHeader::SIZE..MessageHeader::SIZE + 2]
            .copy_from_slice(&self.device_id.to_le_bytes());

        ipc::send(self.port, &msg).map_err(|_| VfsError::Io)?;

        let mut reply = alloc::vec![0u8; MessageHeader::SIZE + 16];
        ipc::recv(self.port, &mut reply).map_err(|_| VfsError::Io)?;

        let status = u64::from_le_bytes(
            reply[MessageHeader::SIZE..MessageHeader::SIZE + 8]
                .try_into().unwrap_or([0u8; 8]));

        if status == ESUCCESS { Ok(()) } else { Err(VfsError::Io) }
    }

    // ── Sector-granular helpers ───────────────────────────────────────────
    //
    // These are the only functions that actually marshal IPC messages.

    /// Read `count` sectors starting at LBA `lba` into `buf`.
    /// `buf.len()` must equal `count * sector_size`.
    pub fn read_sectors(&self, lba: u64, count: u32, buf: &mut [u8])
        -> VfsResult<()>
    {
        let expected = count as usize * self.sector_size as usize;
        if buf.len() != expected { return Err(VfsError::InvalidArgument); }

        let mut remaining_lba   = lba;
        let mut remaining_count = count;
        let mut buf_off         = 0usize;

        while remaining_count > 0 {
            let batch = remaining_count.min(
                (BLOCK_MAX_TRANSFER / self.sector_size as usize) as u32,
            );
            let batch_bytes = batch as usize * self.sector_size as usize;

            let req = BlockReq {
                lba:       remaining_lba,
                device_id: self.device_id,
                _pad:      0,
                count:     batch,
            };

            let hdr = MessageHeader::new(
                MessageType::BlockRead,
                BLOCK_REQ_PAYLOAD as u32,
            );
            let mut msg = alloc::vec![0u8; MessageHeader::SIZE + BLOCK_REQ_PAYLOAD];
            msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
            req.encode(&mut msg[MessageHeader::SIZE..]);

            ipc::send(self.port, &msg).map_err(|_| VfsError::Io)?;

            // Reply: [header][status: u64][bytes_read: u64][data...]
            let reply_size = MessageHeader::SIZE + 16 + batch_bytes;
            let mut reply = alloc::vec![0u8; reply_size];
            ipc::recv(self.port, &mut reply).map_err(|_| VfsError::Io)?;

            let status = u64::from_le_bytes(
                reply[MessageHeader::SIZE..MessageHeader::SIZE + 8]
                    .try_into().unwrap_or([0u8; 8]));
            if status != ESUCCESS { return Err(VfsError::Io); }

            let data_off = MessageHeader::SIZE + 16;
            buf[buf_off..buf_off + batch_bytes]
                .copy_from_slice(&reply[data_off..data_off + batch_bytes]);

            buf_off         += batch_bytes;
            remaining_lba   += batch as u64;
            remaining_count -= batch;
        }

        Ok(())
    }

    /// Write `count` sectors from `buf` starting at LBA `lba`.
    pub fn write_sectors(&self, lba: u64, count: u32, buf: &[u8])
        -> VfsResult<()>
    {
        let expected = count as usize * self.sector_size as usize;
        if buf.len() != expected { return Err(VfsError::InvalidArgument); }

        let mut remaining_lba   = lba;
        let mut remaining_count = count;
        let mut buf_off         = 0usize;

        while remaining_count > 0 {
            let batch = remaining_count.min(
                (BLOCK_MAX_TRANSFER / self.sector_size as usize) as u32,
            );
            let batch_bytes = batch as usize * self.sector_size as usize;

            let req = BlockReq {
                lba:       remaining_lba,
                device_id: self.device_id,
                _pad:      0,
                count:     batch,
            };

            let payload_len = BLOCK_REQ_PAYLOAD + batch_bytes;
            let hdr = MessageHeader::new(
                MessageType::BlockWrite,
                payload_len as u32,
            );
            let mut msg = alloc::vec![0u8; MessageHeader::SIZE + payload_len];
            msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
            req.encode(&mut msg[MessageHeader::SIZE..]);
            msg[MessageHeader::SIZE + BLOCK_REQ_PAYLOAD
                ..MessageHeader::SIZE + payload_len]
                .copy_from_slice(&buf[buf_off..buf_off + batch_bytes]);

            ipc::send(self.port, &msg).map_err(|_| VfsError::Io)?;

            let mut reply = alloc::vec![0u8; MessageHeader::SIZE + 16];
            ipc::recv(self.port, &mut reply).map_err(|_| VfsError::Io)?;

            let status = u64::from_le_bytes(
                reply[MessageHeader::SIZE..MessageHeader::SIZE + 8]
                    .try_into().unwrap_or([0u8; 8]));
            if status != ESUCCESS { return Err(VfsError::Io); }

            buf_off         += batch_bytes;
            remaining_lba   += batch as u64;
            remaining_count -= batch;
        }

        Ok(())
    }
}

// ── BlockReq wire format ──────────────────────────────────────────────────────

/// Matches libipc::messages::BlockReadReq layout exactly.
#[derive(Clone, Copy)]
struct BlockReq {
    lba:       u64,
    device_id: u16,
    _pad:      u16,
    count:     u32,
}

impl BlockReq {
    fn encode(self, out: &mut [u8]) {
        // 12 bytes: lba[8] | device_id[2] | _pad[2] | ... hmm, spec says 12B
        // libipc BlockReadReq: lba: u64, sector_count: u32 = 12 bytes
        out[0..8].copy_from_slice(&self.lba.to_le_bytes());
        // For the actual libipc structure: sector_count at offset 8
        out[8..12].copy_from_slice(&self.count.to_le_bytes());
    }
}
