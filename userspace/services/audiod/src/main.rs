//! Atom OS audio service.
//!
//! Backend: Intel 82801AA AC'97, as exposed by QEMU's `-device AC97`.
//!
//! This service owns the audio hardware and exposes a small IPC surface so that
//! *any* application — the shell, a music player, a video player, the browser —
//! can play sound without touching PCI, DMA, or port I/O itself.
//!
//! Design goals (production-ready, system-wide audio):
//!   * Play arbitrary PCM WAV files by path, streamed from the filesystem so a
//!     multi-megabyte song never has to fit in RAM at once.
//!   * Accept the formats real assets actually use: 8-bit unsigned or 16-bit
//!     signed PCM, mono or stereo, at any sample rate from 8 kHz to 48 kHz.
//!     Everything is converted on the fly to the device format (48 kHz, stereo,
//!     signed 16-bit) using a streaming linear resampler.
//!   * Guarantee the startup chime plays on boot, driven by this service itself
//!     rather than depending on whichever shell happens to start.
//!   * Global volume / mute, persisted across reboots.
//!
//! The hardware is fed through a 32-entry AC'97 buffer-descriptor ring that is
//! continuously refilled from the active stream while playback runs, which is
//! what makes long files and future streaming clients possible.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::atom_abi::{DmaAllocParams, DmaMappingInfo, PciBarInfo, PciDeviceInfo};
use atom_syscall::debug::log;
use atom_syscall::fs;
use atom_syscall::ipc::{create_port, try_recv, wait_any, PortId};
use atom_syscall::thread::yield_now;
use libipc::messages::{
    AudioGetStateMsg, AudioPlayFileMsg, AudioSetStateMsg, AudioStateReplyMsg, AudioStopMsg,
    MessageHeader, MessageType,
};
use libipc::protocol::{get_payload, register_service, send_message};

const HEAP_SIZE: usize = 256 * 1024;
const CONFIG_PATH: &str = "/user/config/audio.cfg";
const STARTUP_SOUND_PATH: &str = "/system/sounds/startup.wav";
const STARTUP_SOUND: &[u8] = include_bytes!("../../../system_apps/ui_shell/sounds/startup.wav");

/// Device output format. The AC'97 PCM-out DAC is driven at a fixed rate and
/// every source is resampled to it; QEMU resamples 48 kHz -> the host backend
/// (e.g. 44.1 kHz CoreAudio) on its side.
const DEVICE_RATE: u32 = 48_000;

/// Buffer-descriptor ring geometry. Each descriptor points at one fixed-size
/// chunk of PCM. 32 chunks of 16 KiB give roughly 2.7 s of slack at 48 kHz
/// stereo, so a coarse poll loop never starves the DAC.
const RING_LEN: usize = 32;
const CHUNK_BYTES: usize = 16 * 1024;
const BDL_BYTES: usize = RING_LEN * core::mem::size_of::<BufferDescriptor>();
/// Audio chunks start on a page boundary, after the descriptor list.
const AUDIO_OFFSET: usize = 4096;
const DMA_TOTAL: usize = AUDIO_OFFSET + RING_LEN * CHUNK_BYTES;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let end = aligned.saturating_add(layout.size());
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(current, end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator {
    heap: UnsafeCell::new([0; HEAP_SIZE]),
    next: AtomicUsize::new(0),
};

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    loop {
        yield_now();
    }
}

#[derive(Clone, Copy)]
struct AudioConfig {
    volume: u8,
    muted: bool,
}

impl AudioConfig {
    fn load() -> Self {
        let mut config = Self {
            volume: 70,
            muted: false,
        };
        let Ok(bytes) = fs::read_file(CONFIG_PATH) else {
            return config;
        };
        let Ok(text) = core::str::from_utf8(&bytes) else {
            return config;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "volume" => {
                    if let Some(volume) = parse_u8(value.trim()) {
                        config.volume = volume.min(100);
                    }
                }
                "muted" => config.muted = value.trim() != "0",
                _ => {}
            }
        }
        config
    }

    fn save(&self) {
        let mut text = String::from("volume=");
        push_u8(&mut text, self.volume);
        text.push_str("\nmuted=");
        text.push(if self.muted { '1' } else { '0' });
        text.push('\n');
        let _ = fs::mkdir_all("/user/config", 0o755);
        let _ = fs::write_file(CONFIG_PATH, text.as_bytes());
    }
}

// ---------------------------------------------------------------------------
// Source readers + WAV decode + streaming resample
// ---------------------------------------------------------------------------

/// A sequential byte source: either an in-memory asset (the embedded startup
/// chime) or an open file streamed in 8 KiB reads. One-way, no seeking.
///
/// The `File` variant embeds its read buffer inline rather than boxing it: the
/// whole `Source` lives in the persistent `stream` field, so this never hits
/// the (free-less, bump) heap on a per-play basis.
#[allow(clippy::large_enum_variant)]
enum Source {
    Mem {
        data: &'static [u8],
        pos: usize,
    },
    File {
        fd: u64,
        buf: [u8; 8192],
        len: usize,
        pos: usize,
        eof: bool,
    },
}

impl Source {
    fn mem(data: &'static [u8]) -> Self {
        Source::Mem { data, pos: 0 }
    }

    fn file(fd: u64) -> Self {
        Source::File {
            fd,
            buf: [0; 8192],
            len: 0,
            pos: 0,
            eof: false,
        }
    }

    /// Pull the next byte, or `None` at end of stream.
    fn next_byte(&mut self) -> Option<u8> {
        match self {
            Source::Mem { data, pos } => {
                let b = *data.get(*pos)?;
                *pos += 1;
                Some(b)
            }
            Source::File {
                fd,
                buf,
                len,
                pos,
                eof,
            } => {
                if *pos >= *len {
                    if *eof {
                        return None;
                    }
                    match fs::read(*fd, buf) {
                        Ok(0) => {
                            *eof = true;
                            return None;
                        }
                        Ok(n) => {
                            *len = n;
                            *pos = 0;
                        }
                        Err(_) => {
                            *eof = true;
                            return None;
                        }
                    }
                }
                let b = buf[*pos];
                *pos += 1;
                Some(b)
            }
        }
    }

    fn close(&mut self) {
        if let Source::File { fd, .. } = self {
            let _ = fs::close(*fd);
        }
    }
}

/// Streaming WAV reader: parses the RIFF/fmt header from the front of the
/// source, then yields source frames decoded to stereo `i16`. Supports 8-bit
/// unsigned and 16-bit signed PCM, mono or stereo.
struct WavStream {
    source: Source,
    channels: u16,
    bits: u16,
    rate: u32,
    /// Bytes of `data` payload still to read (None = chunk runs to EOF).
    remaining: Option<usize>,
}

impl WavStream {
    /// Parse just enough of the header to locate the `data` chunk. Returns
    /// `None` if the format is unsupported.
    fn open(mut source: Source) -> Option<Self> {
        // RIFF....WAVE
        let mut hdr = [0u8; 12];
        for slot in hdr.iter_mut() {
            *slot = source.next_byte()?;
        }
        if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
            return None;
        }

        let mut channels = 0u16;
        let mut bits = 0u16;
        let mut rate = 0u32;
        let mut fmt_seen = false;

        // Walk chunks until we reach `data`.
        loop {
            let mut chunk = [0u8; 8];
            for slot in chunk.iter_mut() {
                *slot = source.next_byte()?;
            }
            let id = &chunk[0..4];
            let len = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;

            if id == b"fmt " {
                if len < 16 {
                    return None;
                }
                let mut fmt = [0u8; 16];
                for slot in fmt.iter_mut() {
                    *slot = source.next_byte()?;
                }
                let format = u16::from_le_bytes([fmt[0], fmt[1]]);
                channels = u16::from_le_bytes([fmt[2], fmt[3]]);
                rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                // 1 = PCM. Reject anything compressed.
                if format != 1 {
                    return None;
                }
                fmt_seen = true;
                // Skip any extension bytes in the fmt chunk.
                for _ in 16..len {
                    source.next_byte()?;
                }
                if len & 1 == 1 {
                    source.next_byte()?;
                }
            } else if id == b"data" {
                if !fmt_seen {
                    return None;
                }
                if channels == 0
                    || channels > 2
                    || !(bits == 8 || bits == 16)
                    || !(4_000..=48_000).contains(&rate)
                {
                    return None;
                }
                return Some(Self {
                    source,
                    channels,
                    bits,
                    rate,
                    remaining: Some(len),
                });
            } else {
                // Unknown chunk: skip its body (and pad byte).
                for _ in 0..len {
                    source.next_byte()?;
                }
                if len & 1 == 1 {
                    source.next_byte()?;
                }
            }
        }
    }

    fn bytes_per_frame(&self) -> usize {
        self.channels as usize * (self.bits as usize / 8)
    }

    /// Decode one source frame to stereo `i16`, or `None` at end of data.
    fn next_frame(&mut self) -> Option<(i16, i16)> {
        let frame_len = self.bytes_per_frame();
        if let Some(rem) = self.remaining {
            if rem < frame_len {
                return None;
            }
        }
        let mut raw = [0u8; 4]; // max: 2 ch * 16-bit
        for slot in raw.iter_mut().take(frame_len) {
            *slot = self.source.next_byte()?;
        }
        if let Some(rem) = self.remaining.as_mut() {
            *rem -= frame_len;
        }

        let (l, r) = match (self.bits, self.channels) {
            (16, 2) => (
                i16::from_le_bytes([raw[0], raw[1]]),
                i16::from_le_bytes([raw[2], raw[3]]),
            ),
            (16, 1) => {
                let s = i16::from_le_bytes([raw[0], raw[1]]);
                (s, s)
            }
            (8, 2) => (u8_to_i16(raw[0]), u8_to_i16(raw[1])),
            (8, 1) => {
                let s = u8_to_i16(raw[0]);
                (s, s)
            }
            _ => return None,
        };
        Some((l, r))
    }
}

#[inline]
fn u8_to_i16(sample: u8) -> i16 {
    // 8-bit WAV is unsigned, centred on 128.
    ((sample as i16) - 128) << 8
}

/// Streaming linear resampler: pulls source frames from a `WavStream` and emits
/// 48 kHz stereo `i16` frames, interpolating between adjacent source frames.
struct Resampler {
    stream: WavStream,
    step: u32,  // source frames advanced per output frame, Q16 fixed point
    phase: u32, // fractional position between `prev` and `next`, Q16
    prev: (i16, i16),
    next: (i16, i16),
    primed: bool,
    drained: bool,
}

impl Resampler {
    fn new(mut stream: WavStream) -> Self {
        let step = ((stream.rate as u64) << 16) / DEVICE_RATE as u64;
        let first = stream.next_frame();
        let (prev, primed, drained) = match first {
            Some(f) => (f, true, false),
            None => ((0, 0), false, true),
        };
        let next = stream.next_frame().unwrap_or(prev);
        Self {
            stream,
            step: step.max(1) as u32,
            phase: 0,
            prev,
            next,
            primed,
            drained,
        }
    }

    /// Emit the next 48 kHz stereo frame, or `None` once the source is drained.
    fn next_output(&mut self) -> Option<(i16, i16)> {
        if !self.primed || self.drained {
            return None;
        }
        // Advance whole source frames consumed by the accumulated phase.
        while self.phase >= 0x1_0000 {
            self.phase -= 0x1_0000;
            self.prev = self.next;
            match self.stream.next_frame() {
                Some(f) => self.next = f,
                None => {
                    self.drained = true;
                    return None;
                }
            }
        }
        let frac = self.phase as i32;
        let l = lerp(self.prev.0, self.next.0, frac);
        let r = lerp(self.prev.1, self.next.1, frac);
        self.phase += self.step;
        Some((l, r))
    }

    /// Fill `dst` with interleaved stereo `i16` little-endian bytes. Returns the
    /// number of bytes written (a multiple of 4); 0 means the source is done.
    fn fill(&mut self, dst: &mut [u8]) -> usize {
        let mut written = 0;
        while written + 4 <= dst.len() {
            let Some((l, r)) = self.next_output() else {
                break;
            };
            dst[written..written + 2].copy_from_slice(&l.to_le_bytes());
            dst[written + 2..written + 4].copy_from_slice(&r.to_le_bytes());
            written += 4;
        }
        written
    }

    fn close(&mut self) {
        self.stream.source.close();
    }
}

#[inline]
fn lerp(a: i16, b: i16, frac_q16: i32) -> i16 {
    let a = a as i32;
    let b = b as i32;
    (a + (((b - a) * frac_q16) >> 16)) as i16
}

// ---------------------------------------------------------------------------
// AC'97 hardware + descriptor ring
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
struct BufferDescriptor {
    address: u32,
    control: u32,
}

struct Ac97 {
    mixer: u16,
    bus_master: u16,
    dma_va: *mut u8,
    dma_phys: u64,
    dma_ready: bool,
    /// Active playback stream, if any.
    stream: Option<Resampler>,
    /// Next ring slot to fill.
    head: usize,
    /// True once the source is exhausted and we stop refilling.
    source_done: bool,
}

impl Ac97 {
    fn discover() -> Option<Self> {
        let mut handles = [0u64; 64];
        let count = atom_syscall::cap::list(&mut handles).min(handles.len());
        for handle in handles[..count].iter().copied() {
            let mut info = PciDeviceInfo::default();
            if atom_syscall::raw::pci_query_device(handle, &mut info) != 0 {
                continue;
            }
            if info.vendor_id != 0x8086 || info.device_id != 0x2415 {
                continue;
            }

            let mut mixer = PciBarInfo::default();
            let mut bus_master = PciBarInfo::default();
            if atom_syscall::raw::pci_get_bar(handle, 0, &mut mixer) != 0
                || atom_syscall::raw::pci_get_bar(handle, 1, &mut bus_master) != 0
                || mixer.is_mmio != 0
                || bus_master.is_mmio != 0
            {
                return None;
            }

            let mut device = Self {
                mixer: mixer.base_addr as u16,
                bus_master: bus_master.base_addr as u16,
                dma_va: core::ptr::null_mut(),
                dma_phys: 0,
                dma_ready: false,
                stream: None,
                head: 0,
                source_done: false,
            };
            if device.initialize() {
                return Some(device);
            }
            return None;
        }
        None
    }

    fn initialize(&mut self) -> bool {
        // Cold reset and codec reset.
        if self.write32(self.bus_master + 0x2c, 0x0000_0002).is_err() {
            return false;
        }
        for _ in 0..20_000 {
            core::hint::spin_loop();
        }
        let _ = self.write16(self.mixer, 0);

        // Enable variable-rate audio and pin the PCM-out DAC to the device rate.
        let ext = self.read16(self.mixer + 0x2a).unwrap_or(0);
        let _ = self.write16(self.mixer + 0x2a, ext | 1);
        let _ = self.write16(self.mixer + 0x2c, DEVICE_RATE as u16);
        let _ = self.write16(self.mixer + 0x18, 0);
        self.reset_pcm_out();
        self.ensure_dma()
    }

    /// Allocate the descriptor ring + chunk pool once.
    fn ensure_dma(&mut self) -> bool {
        if self.dma_ready {
            return true;
        }
        let params = DmaAllocParams {
            size: DMA_TOTAL.div_ceil(4096).saturating_mul(4096) as u64,
            align: 4096,
            flags: 0,
        };
        let mut info = DmaMappingInfo::default();
        let cap = atom_syscall::raw::dma_alloc(&params, &mut info);
        if atom_syscall::atom_abi::is_syscall_error(cap) || info.user_va == 0 {
            log("audiod: DMA allocation failed");
            return false;
        }
        self.dma_va = info.user_va as *mut u8;
        self.dma_phys = info.phys_addr;
        self.dma_ready = true;
        true
    }

    fn set_output(&self, volume: u8, muted: bool) {
        let attenuation = 31u16.saturating_sub((volume.min(100) as u16 * 31) / 100);
        let mut value = attenuation | (attenuation << 8);
        if muted || volume == 0 {
            value |= 1 << 15;
        }
        let _ = self.write16(self.mixer + 0x02, value);
    }

    fn reset_pcm_out(&self) {
        let _ = self.write8(self.bus_master + 0x1b, 0);
        let _ = self.write8(self.bus_master + 0x1b, 0x02);
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        // Clear sticky status bits (LVBCI | BCIS | FIFOE).
        let _ = self.write16(self.bus_master + 0x16, 0x001c);
    }

    fn chunk_phys(&self, slot: usize) -> u64 {
        self.dma_phys + AUDIO_OFFSET as u64 + (slot * CHUNK_BYTES) as u64
    }

    fn chunk_va(&self, slot: usize) -> *mut u8 {
        unsafe { self.dma_va.add(AUDIO_OFFSET + slot * CHUNK_BYTES) }
    }

    fn descriptor(&self, slot: usize) -> *mut BufferDescriptor {
        unsafe { (self.dma_va as *mut BufferDescriptor).add(slot) }
    }

    /// Fill one ring slot from the active stream. Returns false at end of data.
    fn fill_slot(&mut self, slot: usize) -> bool {
        let va = self.chunk_va(slot);
        let Some(stream) = self.stream.as_mut() else {
            return false;
        };
        let dst = unsafe { core::slice::from_raw_parts_mut(va, CHUNK_BYTES) };
        let bytes = stream.fill(dst);
        if bytes == 0 {
            return false;
        }
        let samples = (bytes / 2) as u32; // 16-bit sample units
        unsafe {
            core::ptr::write_volatile(
                self.descriptor(slot),
                BufferDescriptor {
                    address: self.chunk_phys(slot) as u32,
                    control: samples | (1 << 31), // IOC
                },
            );
        }
        true
    }

    /// Begin streaming a freshly opened source. Any current playback is stopped.
    fn start_stream(&mut self, source: Source) -> bool {
        if !self.ensure_dma() {
            return false;
        }
        let Some(wav) = WavStream::open(source) else {
            log("audiod: unsupported audio asset (need PCM WAV, 8/16-bit, mono/stereo)");
            return false;
        };
        self.stop();
        self.stream = Some(Resampler::new(wav));
        self.source_done = false;
        self.head = 0;

        self.reset_pcm_out();
        unsafe {
            core::ptr::write_bytes(self.dma_va, 0, BDL_BYTES);
        }

        // Prime the ring, leaving one slot free so head never collides with the
        // device's current index at startup.
        let mut filled = 0usize;
        while filled < RING_LEN - 1 {
            if !self.fill_slot(self.head) {
                self.source_done = true;
                break;
            }
            self.head = (self.head + 1) % RING_LEN;
            filled += 1;
        }
        if filled == 0 {
            // Empty or unreadable stream.
            self.stop();
            return false;
        }
        let last = (self.head + RING_LEN - 1) % RING_LEN;

        core::sync::atomic::fence(Ordering::Release);
        if self
            .write32(self.bus_master + 0x10, self.dma_phys as u32)
            .is_err()
        {
            self.stop();
            return false;
        }
        let _ = self.write8(self.bus_master + 0x15, last as u8); // LVI
        let _ = self.write8(self.bus_master + 0x1b, 0x01); // run
        true
    }

    /// Keep the descriptor ring fed and detect end of playback. Called every
    /// poll while playing.
    fn pump(&mut self) {
        if self.stream.is_none() {
            return;
        }
        let status = self.read16(self.bus_master + 0x16).unwrap_or(0);
        let civ = self.read8(self.bus_master + 0x14).unwrap_or(0) as usize;

        // Acknowledge sticky completion bits so they don't wedge the controller.
        if status & 0x001c != 0 {
            let _ = self.write16(self.bus_master + 0x16, 0x001c);
        }

        if self.source_done {
            // Wait for the DMA engine to halt (DCH, bit 0) after draining.
            if status & 0x01 != 0 {
                self.stop();
            }
            return;
        }

        // Refill every slot the device has already consumed, keeping a one-slot
        // gap ahead of `civ`.
        let mut last = (self.head + RING_LEN - 1) % RING_LEN;
        let mut produced = false;
        while (self.head + 1) % RING_LEN != civ {
            if !self.fill_slot(self.head) {
                self.source_done = true;
                break;
            }
            last = self.head;
            self.head = (self.head + 1) % RING_LEN;
            produced = true;
            if (self.head + 1) % RING_LEN == civ {
                break;
            }
        }
        if produced {
            let _ = self.write8(self.bus_master + 0x15, last as u8); // advance LVI
                                                                     // If the controller halted because it briefly ran dry, restart it.
            if status & 0x01 != 0 {
                let _ = self.write8(self.bus_master + 0x1b, 0x01);
            }
        }
    }

    /// Stop playback and release the stream.
    fn stop(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            stream.close();
        }
        let _ = self.write8(self.bus_master + 0x1b, 0x00);
        self.reset_pcm_out();
        self.source_done = false;
        self.head = 0;
    }

    fn is_playing(&self) -> bool {
        self.stream.is_some()
    }

    // AC'97 registers are width-sensitive: the NAM mixer and several NABM
    // registers (SR, PICB) only respond to word accesses, and BDBAR/GLOB_CNT
    // require a dword access. Each helper issues a single in/out of that width.
    fn read8(&self, port: u16) -> Result<u8, ()> {
        atom_syscall::io::port_read_u8(port).map_err(|_| ())
    }

    fn read16(&self, port: u16) -> Result<u16, ()> {
        atom_syscall::io::port_read_u16(port).map_err(|_| ())
    }

    fn write8(&self, port: u16, value: u8) -> Result<(), ()> {
        atom_syscall::io::port_write_u8(port, value).map_err(|_| ())
    }

    fn write16(&self, port: u16, value: u16) -> Result<(), ()> {
        atom_syscall::io::port_write_u16(port, value).map_err(|_| ())
    }

    fn write32(&self, port: u16, value: u32) -> Result<(), ()> {
        atom_syscall::io::port_write_u32(port, value).map_err(|_| ())
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

struct AudioService {
    port: PortId,
    config: AudioConfig,
    device: Option<Ac97>,
    boot_chime_done: bool,
}

impl AudioService {
    fn state(&self) -> AudioStateReplyMsg {
        AudioStateReplyMsg {
            volume: self.config.volume,
            muted: self.config.muted,
            available: self.device.is_some(),
            playing: self
                .device
                .as_ref()
                .map(|d| d.is_playing())
                .unwrap_or(false),
        }
    }

    fn send_state(&self, reply_port: PortId) {
        if reply_port == 0 {
            return;
        }
        let _ = send_message(
            reply_port,
            MessageType::AudioStateReply,
            &self.state().to_bytes(),
        );
    }

    /// Open a file and stream it through the device. Honours mute.
    fn play_path(&mut self, path: &str) {
        if self.config.muted {
            return;
        }
        let Some(device) = self.device.as_mut() else {
            return;
        };
        // The startup chime is embedded so it can play even before the
        // filesystem mounts; everything else streams from disk.
        if path == STARTUP_SOUND_PATH {
            if device.start_stream(Source::mem(STARTUP_SOUND)) {
                self.boot_chime_done = true;
                log("audiod: startup chime playing");
            }
            return;
        }
        match fs::open(path, fs::OpenFlags::RDONLY, 0) {
            Ok(fd) => {
                if !device.start_stream(Source::file(fd)) {
                    let _ = fs::close(fd);
                }
            }
            Err(_) => log("audiod: failed to open audio asset"),
        }
    }

    /// Play the startup chime once, driven by the service itself so boot audio
    /// never depends on a particular shell starting.
    fn play_boot_chime(&mut self) {
        if self.boot_chime_done {
            return;
        }
        self.play_path(STARTUP_SOUND_PATH);
    }

    fn handle(&mut self, bytes: &[u8]) {
        let Some(header) = MessageHeader::from_bytes(bytes) else {
            return;
        };
        let payload = get_payload(bytes, bytes.len());
        match header.msg_type {
            MessageType::AudioGetState => {
                if let Some(request) = AudioGetStateMsg::from_bytes(payload) {
                    self.send_state(request.reply_port);
                }
            }
            MessageType::AudioSetState => {
                if let Some(request) = AudioSetStateMsg::from_bytes(payload) {
                    self.config.volume = request.volume.min(100);
                    self.config.muted = request.muted;
                    if let Some(device) = self.device.as_mut() {
                        device.set_output(self.config.volume, self.config.muted);
                        if self.config.muted {
                            device.stop();
                        }
                    }
                    self.config.save();
                    self.send_state(request.reply_port);
                }
            }
            MessageType::AudioPlayFile => {
                if let Some(request) = AudioPlayFileMsg::from_bytes(payload) {
                    // Skip a redundant startup request if we already chimed.
                    if request.path == STARTUP_SOUND_PATH && self.boot_chime_done {
                        return;
                    }
                    self.play_path(&request.path);
                }
            }
            MessageType::AudioStop => {
                if let Some(device) = self.device.as_mut() {
                    device.stop();
                }
                if let Some(request) = AudioStopMsg::from_bytes(payload) {
                    self.send_state(request.reply_port);
                }
            }
            _ => {}
        }
    }
}

fn parse_u8(value: &str) -> Option<u8> {
    let mut out = 0u16;
    if value.is_empty() {
        return None;
    }
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        out = out.checked_mul(10)?.checked_add((byte - b'0') as u16)?;
    }
    u8::try_from(out).ok()
}

fn push_u8(out: &mut String, value: u8) {
    if value >= 100 {
        out.push((b'0' + value / 100) as char);
    }
    if value >= 10 {
        out.push((b'0' + (value / 10) % 10) as char);
    }
    out.push((b'0' + value % 10) as char);
}

fn main() -> ! {
    log("audiod: starting AC97 audio service");
    let port = create_port().expect("audiod: create_port failed");
    let config = AudioConfig::load();
    let device = Ac97::discover();
    if let Some(device) = device.as_ref() {
        device.set_output(config.volume, config.muted);
        log("audiod: AC97 ready");
    } else {
        log("audiod: no supported AC97 device; service remains available");
    }
    let _ = register_service("audiod", port);

    let mut service = AudioService {
        port,
        config,
        device,
        boot_chime_done: false,
    };

    // Boot chime, owned by the service: guaranteed to sound on every boot.
    service.play_boot_chime();

    let ports = [service.port];
    let mut buffer = [0u8; 512];
    loop {
        while let Ok(Some(len)) = try_recv(service.port, &mut buffer) {
            service.handle(&buffer[..len]);
        }
        let playing = if let Some(device) = service.device.as_mut() {
            device.pump();
            device.is_playing()
        } else {
            false
        };
        // Poll tightly while audio is flowing so the ring never starves; idle
        // slowly otherwise.
        let _ = wait_any(&ports, if playing { 10 } else { 50 });
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    log("audiod: panic");
    loop {
        yield_now();
    }
}
