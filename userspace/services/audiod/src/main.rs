//! Atom OS audio service (`audiod`).
//!
//! This file intentionally preserves the current public contract:
//!   - service name: `audiod`
//!   - IPC messages: AudioGetState, AudioSetState, AudioPlayFile, AudioStop
//!   - persisted config: `/user/config/audio.cfg`
//!   - startup chime path/asset behavior
//!
//! Internally this is a clean-room rewrite around a small set of OSDev rules:
//!   - one owner of the audio hardware;
//!   - explicit device backends behind a stable service facade;
//!   - all external audio converted to one hardware mix format;
//!   - bounded memory, streaming I/O, no unbounded per-play allocation;
//!   - DMA descriptors programmed only after fully initialized memory writes;
//!   - fail closed, keep the service alive even when hardware is absent.

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

// ===========================================================================
// Global service constants
// ===========================================================================

const SERVICE_NAME: &str = "audiod";
const CONFIG_PATH: &str = "/user/config/audio.cfg";
const CONFIG_DIR: &str = "/user/config";
const STARTUP_SOUND_PATH: &str = "/system/sounds/startup.wav";
const STARTUP_SOUND: &[u8] = include_bytes!("../../../system_apps/ui_shell/sounds/startup.wav");

/// The internal device/mixer format. Every accepted source is converted to it.
const DEVICE_RATE: u32 = 48_000;
const DEVICE_CHANNELS: usize = 2;
const DEVICE_BYTES_PER_SAMPLE: usize = 2;
const DEVICE_FRAME_BYTES: usize = DEVICE_CHANNELS * DEVICE_BYTES_PER_SAMPLE;

/// Keep these values power-of-two and page aligned. 32 descriptors mirrors both
/// AC'97 PCM-out and the HDA stream BDL model and gives enough scheduling slack
/// for a polling userspace service.
const RING_LEN: usize = 32;
const CHUNK_BYTES: usize = 16 * 1024;
const PAGE_SIZE: usize = 4096;

/// HDA and AC'97 both fit into this single DMA layout:
///   page 0: controller command rings / descriptor tables
///   page 1+: cyclic audio chunks
const DMA_AUDIO_OFF: usize = PAGE_SIZE;
const DMA_BYTES: usize = DMA_AUDIO_OFF + RING_LEN * CHUNK_BYTES;
const DMA_ALLOC_BYTES: usize = align_up(DMA_BYTES, PAGE_SIZE);

/// File streaming buffer. Kept inside Source so playback never performs per-read
/// heap allocation and never loads a full song into RAM.
const FILE_READ_BUF: usize = 8192;

/// Keep accepted source rates intentionally conservative. HDA supports far more,
/// but this service exposes a simple bounded contract suitable for boot sounds,
/// shell effects, simple players and QEMU-backed development.
const MIN_SOURCE_RATE: u32 = 4_000;
const MAX_SOURCE_RATE: u32 = 48_000;

const IPC_BUF_BYTES: usize = 512;
const ACTIVE_POLL_MS: u64 = 5;
const IDLE_POLL_MS: u64 = 50;

const HEAP_SIZE: usize = 256 * 1024;

const fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

// ===========================================================================
// Minimal allocator
// ===========================================================================

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
            let aligned = align_up(current, align);
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
    fatal("audiod: out of heap");
}

fn fatal(message: &str) -> ! {
    log(message);
    loop {
        yield_now();
    }
}

// ===========================================================================
// Errors and utility types
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioError {
    NoDevice,
    BadDevice,
    DmaAlloc,
    DmaAddressUnsupported,
    Io,
    UnsupportedWave,
    EmptyStream,
    Timeout,
}

type Result<T> = core::result::Result<T, AudioError>;

fn log_error(context: &str, err: AudioError) {
    log(context);
    match err {
        AudioError::NoDevice => log("audiod: error: no supported audio device"),
        AudioError::BadDevice => log("audiod: error: malformed or unsupported PCI audio device"),
        AudioError::DmaAlloc => log("audiod: error: DMA allocation failed"),
        AudioError::DmaAddressUnsupported => log("audiod: error: DMA address cannot be represented by backend"),
        AudioError::Io => log("audiod: error: I/O failure"),
        AudioError::UnsupportedWave => log("audiod: error: unsupported WAV; need PCM 8/16-bit mono/stereo"),
        AudioError::EmptyStream => log("audiod: error: audio stream is empty"),
        AudioError::Timeout => log("audiod: error: hardware timeout"),
    }
}

#[derive(Clone, Copy)]
struct AudioConfig {
    volume: u8,
    muted: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            volume: 70,
            muted: false,
        }
    }
}

impl AudioConfig {
    fn load() -> Self {
        let mut config = Self::default();
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
                    if let Some(v) = parse_u8(value.trim()) {
                        config.volume = v.min(100);
                    }
                }
                "muted" => {
                    let v = value.trim();
                    config.muted = v != "0" && v != "false" && v != "False";
                }
                _ => {}
            }
        }
        config
    }

    fn save(self) {
        let mut text = String::from("volume=");
        push_u8(&mut text, self.volume);
        text.push_str("\nmuted=");
        text.push(if self.muted { '1' } else { '0' });
        text.push('\n');

        let _ = fs::mkdir_all(CONFIG_DIR, 0o755);
        let _ = fs::write_file(CONFIG_PATH, text.as_bytes());
    }
}

fn parse_u8(value: &str) -> Option<u8> {
    if value.is_empty() {
        return None;
    }
    let mut out = 0u16;
    for b in value.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        out = out.checked_mul(10)?.checked_add((b - b'0') as u16)?;
    }
    if out <= u8::MAX as u16 {
        Some(out as u8)
    } else {
        None
    }
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

// ===========================================================================
// Stream source, WAV decoder, resampler
// ===========================================================================

#[allow(clippy::large_enum_variant)]
enum Source {
    Memory {
        data: &'static [u8],
        pos: usize,
    },
    File {
        fd: u64,
        buf: [u8; FILE_READ_BUF],
        len: usize,
        pos: usize,
        eof: bool,
        closed: bool,
    },
}

impl Source {
    fn memory(data: &'static [u8]) -> Self {
        Self::Memory { data, pos: 0 }
    }

    fn file(fd: u64) -> Self {
        Self::File {
            fd,
            buf: [0; FILE_READ_BUF],
            len: 0,
            pos: 0,
            eof: false,
            closed: false,
        }
    }

    fn next_byte(&mut self) -> Option<u8> {
        match self {
            Self::Memory { data, pos } => {
                let byte = *data.get(*pos)?;
                *pos += 1;
                Some(byte)
            }
            Self::File {
                fd,
                buf,
                len,
                pos,
                eof,
                closed,
            } => {
                if *closed {
                    return None;
                }
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
                            *len = n.min(FILE_READ_BUF);
                            *pos = 0;
                        }
                        Err(_) => {
                            *eof = true;
                            return None;
                        }
                    }
                }
                let byte = buf[*pos];
                *pos += 1;
                Some(byte)
            }
        }
    }

    fn skip(&mut self, mut n: usize) -> bool {
        while n > 0 {
            if self.next_byte().is_none() {
                return false;
            }
            n -= 1;
        }
        true
    }

    fn close(&mut self) {
        if let Self::File { fd, closed, .. } = self {
            if !*closed {
                let _ = fs::close(*fd);
                *closed = true;
            }
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone, Copy)]
struct PcmFormat {
    channels: u16,
    bits_per_sample: u16,
    sample_rate: u32,
    block_align: u16,
}

impl PcmFormat {
    fn bytes_per_frame(self) -> usize {
        self.block_align as usize
    }

    fn validate(self) -> bool {
        let bytes_per_sample = self.bits_per_sample / 8;
        self.channels >= 1
            && self.channels <= 2
            && (self.bits_per_sample == 8 || self.bits_per_sample == 16)
            && (MIN_SOURCE_RATE..=MAX_SOURCE_RATE).contains(&self.sample_rate)
            && self.block_align == self.channels.saturating_mul(bytes_per_sample)
    }
}

struct WavStream {
    source: Source,
    fmt: PcmFormat,
    bytes_left: usize,
}

impl WavStream {
    fn open(mut source: Source) -> Result<Self> {
        let riff = read_exact_12(&mut source).ok_or(AudioError::UnsupportedWave)?;
        if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
            source.close();
            return Err(AudioError::UnsupportedWave);
        }

        let mut fmt: Option<PcmFormat> = None;

        loop {
            let header = match read_exact_8(&mut source) {
                Some(h) => h,
                None => {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
            };
            let id = &header[0..4];
            let len = le_u32(&header[4..8]) as usize;

            if id == b"fmt " {
                if len < 16 {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
                let base = read_exact_16(&mut source).ok_or(AudioError::UnsupportedWave)?;
                let audio_format = le_u16(&base[0..2]);
                let channels = le_u16(&base[2..4]);
                let sample_rate = le_u32(&base[4..8]);
                let block_align = le_u16(&base[12..14]);
                let bits_per_sample = le_u16(&base[14..16]);

                // 1 == WAVE_FORMAT_PCM. Compressed codecs are intentionally
                // rejected here because this service is a PCM renderer, not a
                // codec host.
                if audio_format != 1 {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
                let parsed = PcmFormat {
                    channels,
                    bits_per_sample,
                    sample_rate,
                    block_align,
                };
                if !parsed.validate() {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
                fmt = Some(parsed);
                if len > 16 && !source.skip(len - 16) {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
                if len & 1 != 0 {
                    let _ = source.next_byte();
                }
            } else if id == b"data" {
                let Some(fmt) = fmt else {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                };
                if len < fmt.bytes_per_frame() {
                    source.close();
                    return Err(AudioError::EmptyStream);
                }
                return Ok(Self {
                    source,
                    fmt,
                    bytes_left: len,
                });
            } else {
                if !source.skip(len) {
                    source.close();
                    return Err(AudioError::UnsupportedWave);
                }
                if len & 1 != 0 {
                    let _ = source.next_byte();
                }
            }
        }
    }

    fn next_frame(&mut self) -> Option<(i16, i16)> {
        let frame_bytes = self.fmt.bytes_per_frame();
        if self.bytes_left < frame_bytes {
            return None;
        }

        let mut raw = [0u8; 4];
        for dst in raw.iter_mut().take(frame_bytes) {
            *dst = self.source.next_byte()?;
        }
        self.bytes_left -= frame_bytes;

        match (self.fmt.bits_per_sample, self.fmt.channels) {
            (16, 2) => Some((
                i16::from_le_bytes([raw[0], raw[1]]),
                i16::from_le_bytes([raw[2], raw[3]]),
            )),
            (16, 1) => {
                let s = i16::from_le_bytes([raw[0], raw[1]]);
                Some((s, s))
            }
            (8, 2) => Some((u8_pcm_to_i16(raw[0]), u8_pcm_to_i16(raw[1]))),
            (8, 1) => {
                let s = u8_pcm_to_i16(raw[0]);
                Some((s, s))
            }
            _ => None,
        }
    }

    fn close(&mut self) {
        self.source.close();
    }
}

fn read_exact_8(source: &mut Source) -> Option<[u8; 8]> {
    let mut out = [0u8; 8];
    for byte in out.iter_mut() {
        *byte = source.next_byte()?;
    }
    Some(out)
}

fn read_exact_12(source: &mut Source) -> Option<[u8; 12]> {
    let mut out = [0u8; 12];
    for byte in out.iter_mut() {
        *byte = source.next_byte()?;
    }
    Some(out)
}

fn read_exact_16(source: &mut Source) -> Option<[u8; 16]> {
    let mut out = [0u8; 16];
    for byte in out.iter_mut() {
        *byte = source.next_byte()?;
    }
    Some(out)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[inline]
fn u8_pcm_to_i16(sample: u8) -> i16 {
    ((sample as i16) - 128) << 8
}

struct Resampler {
    stream: WavStream,
    step_q16: u32,
    phase_q16: u32,
    prev: (i16, i16),
    next: (i16, i16),
    active: bool,
}

impl Resampler {
    fn open(stream: WavStream) -> Result<Self> {
        let mut this = Self {
            step_q16: (((stream.fmt.sample_rate as u64) << 16) / DEVICE_RATE as u64).max(1) as u32,
            stream,
            phase_q16: 0,
            prev: (0, 0),
            next: (0, 0),
            active: false,
        };

        let Some(first) = this.stream.next_frame() else {
            this.stream.close();
            return Err(AudioError::EmptyStream);
        };
        this.prev = first;
        this.next = this.stream.next_frame().unwrap_or(first);
        this.active = true;
        Ok(this)
    }

    fn next_output_frame(&mut self) -> Option<(i16, i16)> {
        if !self.active {
            return None;
        }

        while self.phase_q16 >= 0x1_0000 {
            self.phase_q16 -= 0x1_0000;
            self.prev = self.next;
            match self.stream.next_frame() {
                Some(frame) => self.next = frame,
                None => {
                    self.active = false;
                    return None;
                }
            }
        }

        let frac = self.phase_q16 as i32;
        let out = (lerp_i16(self.prev.0, self.next.0, frac), lerp_i16(self.prev.1, self.next.1, frac));
        self.phase_q16 = self.phase_q16.wrapping_add(self.step_q16);
        Some(out)
    }

    fn fill_bytes(&mut self, dst: &mut [u8]) -> usize {
        let mut written = 0usize;
        while written + DEVICE_FRAME_BYTES <= dst.len() {
            let Some((left, right)) = self.next_output_frame() else {
                break;
            };
            dst[written..written + 2].copy_from_slice(&left.to_le_bytes());
            dst[written + 2..written + 4].copy_from_slice(&right.to_le_bytes());
            written += DEVICE_FRAME_BYTES;
        }
        written
    }

    fn close(&mut self) {
        self.stream.close();
        self.active = false;
    }
}

#[inline]
fn lerp_i16(a: i16, b: i16, frac_q16: i32) -> i16 {
    let a = a as i32;
    let b = b as i32;
    (a + (((b - a) * frac_q16) >> 16)) as i16
}

// ===========================================================================
// DMA helpers
// ===========================================================================

struct DmaRegion {
    va: *mut u8,
    phys: u64,
    len: usize,
}

impl DmaRegion {
    fn allocate(require_32_bit: bool) -> Result<Self> {
        let params = DmaAllocParams {
            size: DMA_ALLOC_BYTES as u64,
            align: PAGE_SIZE as u64,
            flags: 0,
        };
        let mut info = DmaMappingInfo::default();
        let cap = atom_syscall::raw::dma_alloc(&params, &mut info);
        if atom_syscall::atom_abi::is_syscall_error(cap) || info.user_va == 0 {
            return Err(AudioError::DmaAlloc);
        }
        let end = info
            .phys_addr
            .checked_add(DMA_ALLOC_BYTES as u64 - 1)
            .ok_or(AudioError::DmaAddressUnsupported)?;
        if require_32_bit && end > u32::MAX as u64 {
            return Err(AudioError::DmaAddressUnsupported);
        }
        let region = Self {
            va: info.user_va as *mut u8,
            phys: info.phys_addr,
            len: DMA_ALLOC_BYTES,
        };
        region.zero_all();
        Ok(region)
    }

    fn zero_all(&self) {
        unsafe { core::ptr::write_bytes(self.va, 0, self.len) }
    }

    fn ptr_at<T>(&self, offset: usize) -> *mut T {
        debug_assert!(offset < self.len);
        unsafe { self.va.add(offset) as *mut T }
    }

    fn chunk_va(&self, slot: usize) -> *mut u8 {
        self.ptr_at::<u8>(DMA_AUDIO_OFF + slot * CHUNK_BYTES)
    }

    fn chunk_phys(&self, slot: usize) -> u64 {
        self.phys + DMA_AUDIO_OFF as u64 + (slot * CHUNK_BYTES) as u64
    }
}

// ===========================================================================
// PCI discovery helpers
// ===========================================================================

fn pci_handles(out: &mut [u64]) -> usize {
    atom_syscall::cap::list(out).min(out.len())
}

fn pci_info(handle: u64) -> Option<PciDeviceInfo> {
    let mut info = PciDeviceInfo::default();
    if atom_syscall::raw::pci_query_device(handle, &mut info) == 0 {
        Some(info)
    } else {
        None
    }
}

fn pci_bar(handle: u64, index: u8) -> Option<PciBarInfo> {
    let mut bar = PciBarInfo::default();
    if atom_syscall::raw::pci_get_bar(handle, index, &mut bar) == 0 {
        Some(bar)
    } else {
        None
    }
}

// ===========================================================================
// AC'97 backend
// ===========================================================================

const AC97_INTEL_VENDOR: u16 = 0x8086;
const AC97_ICH_DEVICE: u16 = 0x2415;

const AC97_MIX_MASTER_VOLUME: u16 = 0x02;
const AC97_MIX_PCM_OUT_VOLUME: u16 = 0x18;
const AC97_EXT_AUDIO_ID: u16 = 0x2a;
const AC97_EXT_AUDIO_CTRL: u16 = 0x2a;
const AC97_PCM_FRONT_DAC_RATE: u16 = 0x2c;

const AC97_GLOB_CNT: u16 = 0x2c;
const AC97_PO_BDBAR: u16 = 0x10;
const AC97_PO_CIV: u16 = 0x14;
const AC97_PO_LVI: u16 = 0x15;
const AC97_PO_SR: u16 = 0x16;
const AC97_PO_CR: u16 = 0x1b;

const AC97_SR_DCH: u16 = 1 << 0;
const AC97_SR_CLEAR: u16 = 0x001c; // FIFOE | BCIS | LVBCI
const AC97_CR_RUN: u8 = 1 << 0;
const AC97_CR_RESET_REGS: u8 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct Ac97BdlEntry {
    addr: u32,
    control: u32,
}

struct Ac97 {
    mixer_base: u16,
    bm_base: u16,
    dma: DmaRegion,
    stream: Option<Resampler>,
    next_fill: usize,
    draining: bool,
}

impl Ac97 {
    fn discover() -> Result<Self> {
        let mut handles = [0u64; 64];
        let count = pci_handles(&mut handles);
        for handle in handles[..count].iter().copied() {
            let Some(info) = pci_info(handle) else {
                continue;
            };
            if info.vendor_id != AC97_INTEL_VENDOR || info.device_id != AC97_ICH_DEVICE {
                continue;
            }
            let mixer = pci_bar(handle, 0).ok_or(AudioError::BadDevice)?;
            let bus_master = pci_bar(handle, 1).ok_or(AudioError::BadDevice)?;
            if mixer.is_mmio != 0 || bus_master.is_mmio != 0 {
                return Err(AudioError::BadDevice);
            }

            let mut dev = Self {
                mixer_base: mixer.base_addr as u16,
                bm_base: bus_master.base_addr as u16,
                dma: DmaRegion::allocate(true)?,
                stream: None,
                next_fill: 0,
                draining: false,
            };
            dev.initialize()?;
            return Ok(dev);
        }
        Err(AudioError::NoDevice)
    }

    fn initialize(&mut self) -> Result<()> {
        self.write32(self.bm_base + AC97_GLOB_CNT, 0x0000_0002)?;
        spin_wait(20_000);

        // Codec reset, enable variable-rate audio, route PCM-out unmuted.
        self.write16(self.mixer_base, 0)?;
        let ext = self.read16(self.mixer_base + AC97_EXT_AUDIO_ID).unwrap_or(0);
        self.write16(self.mixer_base + AC97_EXT_AUDIO_CTRL, ext | 1)?;
        self.write16(self.mixer_base + AC97_PCM_FRONT_DAC_RATE, DEVICE_RATE as u16)?;
        self.write16(self.mixer_base + AC97_MIX_PCM_OUT_VOLUME, 0)?;
        self.reset_pcm_out();
        Ok(())
    }

    fn set_output(&self, volume: u8, muted: bool) {
        let attenuation = 31u16.saturating_sub((volume.min(100) as u16 * 31) / 100);
        let mut reg = attenuation | (attenuation << 8);
        if muted || volume == 0 {
            reg |= 1 << 15;
        }
        let _ = self.write16(self.mixer_base + AC97_MIX_MASTER_VOLUME, reg);
        let _ = self.write16(self.mixer_base + AC97_MIX_PCM_OUT_VOLUME, reg);
    }

    fn start_stream(&mut self, source: Source) -> Result<()> {
        let wav = WavStream::open(source)?;
        let resampler = Resampler::open(wav)?;
        self.stop();
        self.stream = Some(resampler);
        self.draining = false;
        self.next_fill = 0;
        self.reset_pcm_out();
        self.zero_bdl();

        let mut filled = 0usize;
        while filled < RING_LEN - 1 {
            if !self.fill_slot(self.next_fill) {
                self.draining = true;
                break;
            }
            self.next_fill = (self.next_fill + 1) % RING_LEN;
            filled += 1;
        }

        if filled == 0 {
            self.stop();
            return Err(AudioError::EmptyStream);
        }

        let last_valid = (self.next_fill + RING_LEN - 1) % RING_LEN;
        core::sync::atomic::fence(Ordering::Release);
        self.write32(self.bm_base + AC97_PO_BDBAR, self.dma.phys as u32)?;
        self.write8(self.bm_base + AC97_PO_LVI, last_valid as u8)?;
        self.write8(self.bm_base + AC97_PO_CR, AC97_CR_RUN)?;
        Ok(())
    }

    fn pump(&mut self) {
        if self.stream.is_none() {
            return;
        }

        let status = self.read16(self.bm_base + AC97_PO_SR).unwrap_or(0);
        let civ = self.read8(self.bm_base + AC97_PO_CIV).unwrap_or(0) as usize;
        if status & AC97_SR_CLEAR != 0 {
            let _ = self.write16(self.bm_base + AC97_PO_SR, AC97_SR_CLEAR);
        }

        if self.draining {
            if status & AC97_SR_DCH != 0 {
                self.stop();
            }
            return;
        }

        let mut advanced = false;
        let mut last_valid = (self.next_fill + RING_LEN - 1) % RING_LEN;
        while (self.next_fill + 1) % RING_LEN != civ {
            if !self.fill_slot(self.next_fill) {
                self.draining = true;
                break;
            }
            last_valid = self.next_fill;
            self.next_fill = (self.next_fill + 1) % RING_LEN;
            advanced = true;
        }

        if advanced {
            let _ = self.write8(self.bm_base + AC97_PO_LVI, last_valid as u8);
            if status & AC97_SR_DCH != 0 {
                let _ = self.write8(self.bm_base + AC97_PO_CR, AC97_CR_RUN);
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            stream.close();
        }
        let _ = self.write8(self.bm_base + AC97_PO_CR, 0);
        self.reset_pcm_out();
        self.next_fill = 0;
        self.draining = false;
    }

    fn is_playing(&self) -> bool {
        self.stream.is_some()
    }

    fn reset_pcm_out(&self) {
        let _ = self.write8(self.bm_base + AC97_PO_CR, 0);
        let _ = self.write8(self.bm_base + AC97_PO_CR, AC97_CR_RESET_REGS);
        spin_wait(10_000);
        let _ = self.write16(self.bm_base + AC97_PO_SR, AC97_SR_CLEAR);
    }

    fn zero_bdl(&self) {
        unsafe {
            core::ptr::write_bytes(
                self.dma.ptr_at::<u8>(0),
                0,
                RING_LEN * core::mem::size_of::<Ac97BdlEntry>(),
            )
        }
    }

    fn fill_slot(&mut self, slot: usize) -> bool {
        let Some(stream) = self.stream.as_mut() else {
            return false;
        };
        let dst = unsafe { core::slice::from_raw_parts_mut(self.dma.chunk_va(slot), CHUNK_BYTES) };
        let bytes = stream.fill_bytes(dst);
        if bytes == 0 {
            return false;
        }

        // AC'97 length is in 16-bit samples, not bytes.
        let sample_count = (bytes / 2) as u32;
        unsafe {
            core::ptr::write_volatile(
                self.dma.ptr_at::<Ac97BdlEntry>(slot * core::mem::size_of::<Ac97BdlEntry>()),
                Ac97BdlEntry {
                    addr: self.dma.chunk_phys(slot) as u32,
                    control: sample_count | (1 << 31),
                },
            );
        }
        true
    }

    fn read8(&self, port: u16) -> Result<u8> {
        atom_syscall::io::port_read_u8(port).map_err(|_| AudioError::Io)
    }
    fn read16(&self, port: u16) -> Result<u16> {
        atom_syscall::io::port_read_u16(port).map_err(|_| AudioError::Io)
    }
    fn write8(&self, port: u16, value: u8) -> Result<()> {
        atom_syscall::io::port_write_u8(port, value).map_err(|_| AudioError::Io)
    }
    fn write16(&self, port: u16, value: u16) -> Result<()> {
        atom_syscall::io::port_write_u16(port, value).map_err(|_| AudioError::Io)
    }
    fn write32(&self, port: u16, value: u32) -> Result<()> {
        atom_syscall::io::port_write_u32(port, value).map_err(|_| AudioError::Io)
    }
}

// ===========================================================================
// Intel High Definition Audio backend
// ===========================================================================

const HDA_CLASS_MULTIMEDIA: u8 = 0x04;
const HDA_SUBCLASS_HD_AUDIO: u8 = 0x03;

const HDA_GCAP: u32 = 0x00;
const HDA_GCTL: u32 = 0x08;
const HDA_STATESTS: u32 = 0x0e;
const HDA_INTCTL: u32 = 0x20;
const HDA_INTSTS: u32 = 0x24;
const HDA_CORBLBASE: u32 = 0x40;
const HDA_CORBUBASE: u32 = 0x44;
const HDA_CORBWP: u32 = 0x48;
const HDA_CORBRP: u32 = 0x4a;
const HDA_CORBCTL: u32 = 0x4c;
const HDA_CORBSIZE: u32 = 0x4e;
const HDA_RIRBLBASE: u32 = 0x50;
const HDA_RIRBUBASE: u32 = 0x54;
const HDA_RIRBWP: u32 = 0x58;
const HDA_RINTCNT: u32 = 0x5a;
const HDA_RIRBCTL: u32 = 0x5c;
const HDA_RIRBSTS: u32 = 0x5d;
const HDA_RIRBSIZE: u32 = 0x5e;

const SD_CTL: u32 = 0x00;
const SD_STS: u32 = 0x03;
const SD_LPIB: u32 = 0x04;
const SD_CBL: u32 = 0x08;
const SD_LVI: u32 = 0x0c;
const SD_FMT: u32 = 0x12;
const SD_BDPL: u32 = 0x18;
const SD_BDPU: u32 = 0x1c;

const HDA_CORB_OFF: usize = 0;
const HDA_RIRB_OFF: usize = 1024;
const HDA_BDL_OFF: usize = 3072;
const HDA_CORB_ENTRIES: usize = 256;

const HDA_STREAM_TAG: u8 = 1;
const HDA_FMT_48K_S16_STEREO: u16 = 0x0011;
const HDA_RUN: u32 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy)]
struct HdaBdlEntry {
    addr: u64,
    len: u32,
    flags: u32,
}

struct Hda {
    mmio: u64,
    dma: DmaRegion,
    codec: u8,
    dac_nid: u8,
    pin_nid: u8,
    sd_base: u32,
    corb_wp: u16,
    stream: Option<Resampler>,
    next_refill: usize,
    eof: bool,
    total_audio_bytes: u64,
    consumed_bytes: u64,
    last_lpib: u32,
}

impl Hda {
    fn discover() -> Result<Self> {
        let mut handles = [0u64; 64];
        let count = pci_handles(&mut handles);
        for handle in handles[..count].iter().copied() {
            let Some(info) = pci_info(handle) else {
                continue;
            };
            if info.class != HDA_CLASS_MULTIMEDIA || info.subclass != HDA_SUBCLASS_HD_AUDIO {
                continue;
            }
            let Some(bar0) = pci_bar(handle, 0) else {
                return Err(AudioError::BadDevice);
            };
            if bar0.is_mmio == 0 || bar0.mmio_cap == 0 {
                return Err(AudioError::BadDevice);
            }
            let mut va = 0u64;
            if atom_syscall::raw::map_mmio(bar0.mmio_cap, &mut va) != 0 || va == 0 {
                return Err(AudioError::Io);
            }

            let mut dev = Self {
                mmio: va,
                dma: DmaRegion::allocate(false)?,
                codec: 0,
                dac_nid: 0,
                pin_nid: 0,
                sd_base: 0,
                corb_wp: 0,
                stream: None,
                next_refill: 0,
                eof: false,
                total_audio_bytes: 0,
                consumed_bytes: 0,
                last_lpib: 0,
            };
            dev.initialize()?;
            return Ok(dev);
        }
        Err(AudioError::NoDevice)
    }

    fn initialize(&mut self) -> Result<()> {
        self.controller_reset()?;
        self.disable_interrupts();
        self.select_first_output_stream();
        self.setup_corb_rirb()?;
        spin_wait(200_000);
        self.codec = self.detect_codec()?;
        self.discover_widgets()?;
        self.configure_codec();
        Ok(())
    }

    fn controller_reset(&self) -> Result<()> {
        self.mw32(HDA_GCTL, 0);
        wait_until(1000, || self.mr32(HDA_GCTL) & 1 == 0)?;
        self.mw32(HDA_GCTL, 1);
        wait_until(1000, || self.mr32(HDA_GCTL) & 1 == 1)?;
        spin_wait(100_000);
        Ok(())
    }

    fn disable_interrupts(&self) {
        self.mw32(HDA_INTCTL, 0);
        self.mw32(HDA_INTSTS, u32::MAX);
    }

    fn select_first_output_stream(&mut self) {
        let gcap = self.mr16(HDA_GCAP);
        let input_stream_count = ((gcap >> 8) & 0x0f) as u32;
        self.sd_base = 0x80 + input_stream_count * 0x20;
    }

    fn setup_corb_rirb(&mut self) -> Result<()> {
        self.mw8(HDA_CORBCTL, 0);
        self.mw8(HDA_RIRBCTL, 0);

        let corb_phys = self.dma.phys + HDA_CORB_OFF as u64;
        let rirb_phys = self.dma.phys + HDA_RIRB_OFF as u64;
        self.mw32(HDA_CORBLBASE, corb_phys as u32);
        self.mw32(HDA_CORBUBASE, (corb_phys >> 32) as u32);
        self.mw32(HDA_RIRBLBASE, rirb_phys as u32);
        self.mw32(HDA_RIRBUBASE, (rirb_phys >> 32) as u32);

        // Program 256-entry rings where supported. QEMU's ICH6 HDA accepts it.
        self.mw8(HDA_CORBSIZE, 0x02);
        self.mw8(HDA_RIRBSIZE, 0x02);

        self.mw16(HDA_CORBRP, 0x8000);
        wait_until(1000, || self.mr16(HDA_CORBRP) & 0x8000 != 0)?;
        self.mw16(HDA_CORBRP, 0);
        wait_until(1000, || self.mr16(HDA_CORBRP) & 0x8000 == 0)?;

        self.corb_wp = 0;
        self.mw16(HDA_CORBWP, 0);
        self.mw16(HDA_RIRBWP, 0x8000);
        self.mw16(HDA_RINTCNT, 0xff);
        self.mw8(HDA_RIRBSTS, 0x05);

        self.mw8(HDA_CORBCTL, 0x02);
        self.mw8(HDA_RIRBCTL, 0x02);
        Ok(())
    }

    fn detect_codec(&self) -> Result<u8> {
        let state = self.mr16(HDA_STATESTS) & 0x7fff;
        if state == 0 {
            return Err(AudioError::NoDevice);
        }
        Ok(state.trailing_zeros() as u8)
    }

    fn discover_widgets(&mut self) -> Result<()> {
        let root_subnodes = self.get_param(0, 0x04);
        let fg_start = ((root_subnodes >> 16) & 0xff) as u8;
        let fg_count = (root_subnodes & 0xff) as u8;

        for fg in fg_start..fg_start.wrapping_add(fg_count) {
            if self.get_param(fg, 0x05) & 0x7f != 0x01 {
                continue;
            }
            let widgets = self.get_param(fg, 0x04);
            let start = ((widgets >> 16) & 0xff) as u8;
            let count = (widgets & 0xff) as u8;

            for nid in start..start.wrapping_add(count) {
                let caps = self.get_param(nid, 0x09);
                let widget_type = (caps >> 20) & 0x0f;
                if widget_type == 0x0 && self.dac_nid == 0 {
                    self.dac_nid = nid;
                } else if widget_type == 0x4 && self.pin_nid == 0 {
                    let pin_caps = self.get_param(nid, 0x0c);
                    if pin_caps & (1 << 4) != 0 {
                        self.pin_nid = nid;
                    }
                }
            }
        }

        if self.dac_nid == 0 || self.pin_nid == 0 {
            return Err(AudioError::BadDevice);
        }
        Ok(())
    }

    fn configure_codec(&mut self) {
        let dac = self.dac_nid;
        let pin = self.pin_nid;
        self.command(dac, 0x7_0500); // power D0
        self.command(pin, 0x7_0500); // power D0
        self.command(dac, 0x2_0000 | HDA_FMT_48K_S16_STEREO as u32);
        self.command(dac, 0x7_0600 | ((HDA_STREAM_TAG as u32) << 4));
        self.command(pin, 0x7_0100); // select first connection
        self.command(pin, 0x7_0700 | 0x40); // output enable
        self.command(pin, 0x7_0c00 | 0x02); // EAPD/BTL enable where supported
        self.unmute_max(dac);
        self.unmute_max(pin);
    }

    fn set_output(&mut self, volume: u8, muted: bool) {
        let dac = self.dac_nid;
        let pin = self.pin_nid;
        let max_gain = self.output_amp_steps(dac).unwrap_or(0x7f);
        let scaled = ((volume.min(100) as u32 * max_gain as u32) / 100) as u8;
        self.set_output_amp(dac, scaled, muted || volume == 0);
        self.set_output_amp(pin, max_gain, muted || volume == 0);
    }

    fn output_amp_steps(&mut self, nid: u8) -> Option<u8> {
        let caps = self.get_param(nid, 0x12);
        let steps = ((caps >> 8) & 0x7f) as u8;
        if steps == 0 { None } else { Some(steps) }
    }

    fn unmute_max(&mut self, nid: u8) {
        let gain = self.output_amp_steps(nid).unwrap_or(0x7f);
        self.set_output_amp(nid, gain, false);
    }

    fn set_output_amp(&mut self, nid: u8, gain: u8, muted: bool) {
        let mut payload = 0xb000u32 | (gain.min(0x7f) as u32);
        if muted {
            payload |= 1 << 7;
        }
        self.command(nid, 0x3_0000 | payload);
    }

    fn start_stream(&mut self, source: Source) -> Result<()> {
        let wav = WavStream::open(source)?;
        let resampler = Resampler::open(wav)?;
        self.stop();
        self.stream = Some(resampler);
        self.next_refill = 0;
        self.eof = false;
        self.total_audio_bytes = 0;
        self.consumed_bytes = 0;
        self.last_lpib = 0;

        self.stream_reset()?;
        self.program_bdl();

        for slot in 0..RING_LEN {
            self.fill_chunk(slot);
        }
        if self.total_audio_bytes == 0 {
            self.stop();
            return Err(AudioError::EmptyStream);
        }

        let bdl_phys = self.dma.phys + HDA_BDL_OFF as u64;
        self.mw32(self.sd_base + SD_BDPL, bdl_phys as u32);
        self.mw32(self.sd_base + SD_BDPU, (bdl_phys >> 32) as u32);
        self.mw32(self.sd_base + SD_CBL, (RING_LEN * CHUNK_BYTES) as u32);
        self.mw16(self.sd_base + SD_LVI, (RING_LEN - 1) as u16);
        self.mw16(self.sd_base + SD_FMT, HDA_FMT_48K_S16_STEREO);
        core::sync::atomic::fence(Ordering::Release);

        self.mw32(self.sd_base + SD_CTL, ((HDA_STREAM_TAG as u32) << 20) | HDA_RUN);
        Ok(())
    }

    fn pump(&mut self) {
        if self.stream.is_none() {
            return;
        }

        let ring_bytes = (RING_LEN * CHUNK_BYTES) as u32;
        let lpib = self.mr32(self.sd_base + SD_LPIB) % ring_bytes;
        let delta = (lpib + ring_bytes - self.last_lpib) % ring_bytes;
        self.consumed_bytes = self.consumed_bytes.saturating_add(delta as u64);
        self.last_lpib = lpib;

        let current_chunk = (lpib as usize / CHUNK_BYTES) % RING_LEN;
        if !self.eof {
            while self.next_refill != current_chunk {
                self.fill_chunk(self.next_refill);
                self.next_refill = (self.next_refill + 1) % RING_LEN;
                if self.eof {
                    break;
                }
            }
        } else if self.consumed_bytes >= self.total_audio_bytes {
            self.stop();
        }

        // Clear sticky stream status bits. We poll LPIB, so interrupts remain off.
        self.mw8(self.sd_base + SD_STS, 0x1c);
    }

    fn stop(&mut self) {
        if let Some(mut stream) = self.stream.take() {
            stream.close();
        }
        self.mw32(self.sd_base + SD_CTL, 0);
        self.next_refill = 0;
        self.eof = false;
        self.total_audio_bytes = 0;
        self.consumed_bytes = 0;
        self.last_lpib = 0;
    }

    fn is_playing(&self) -> bool {
        self.stream.is_some()
    }

    fn program_bdl(&self) {
        let bdl = self.dma.ptr_at::<HdaBdlEntry>(HDA_BDL_OFF);
        for slot in 0..RING_LEN {
            unsafe {
                core::ptr::write_volatile(
                    bdl.add(slot),
                    HdaBdlEntry {
                        addr: self.dma.chunk_phys(slot),
                        len: CHUNK_BYTES as u32,
                        flags: 0,
                    },
                );
            }
        }
    }

    fn fill_chunk(&mut self, slot: usize) {
        let dst = unsafe { core::slice::from_raw_parts_mut(self.dma.chunk_va(slot), CHUNK_BYTES) };
        let written = match (self.eof, self.stream.as_mut()) {
            (false, Some(stream)) => stream.fill_bytes(dst),
            _ => 0,
        };
        if written < CHUNK_BYTES {
            dst[written..].fill(0);
            self.eof = true;
        }
        self.total_audio_bytes = self.total_audio_bytes.saturating_add(written as u64);
    }

    fn stream_reset(&self) -> Result<()> {
        self.mw32(self.sd_base + SD_CTL, 1);
        wait_until(1000, || self.mr32(self.sd_base + SD_CTL) & 1 != 0)?;
        self.mw32(self.sd_base + SD_CTL, 0);
        wait_until(1000, || self.mr32(self.sd_base + SD_CTL) & 1 == 0)?;
        self.mw8(self.sd_base + SD_STS, 0x1c);
        Ok(())
    }

    fn get_param(&mut self, nid: u8, param: u32) -> u32 {
        self.command(nid, 0xf_0000 | (param & 0xff)).unwrap_or(0)
    }

    fn command(&mut self, nid: u8, verb: u32) -> Option<u32> {
        let cmd = ((self.codec as u32) << 28) | ((nid as u32) << 20) | (verb & 0x000f_ffff);
        let prev_rirb_wp = self.mr16(HDA_RIRBWP) & 0xff;
        self.corb_wp = ((self.corb_wp as usize + 1) % HDA_CORB_ENTRIES) as u16;
        unsafe {
            let corb = self.dma.ptr_at::<u32>(HDA_CORB_OFF);
            core::ptr::write_volatile(corb.add(self.corb_wp as usize), cmd);
        }
        core::sync::atomic::fence(Ordering::Release);
        self.mw16(HDA_CORBWP, self.corb_wp);

        for _ in 0..1_000_000 {
            let wp = self.mr16(HDA_RIRBWP) & 0xff;
            if wp != prev_rirb_wp {
                core::sync::atomic::fence(Ordering::Acquire);
                let response = unsafe {
                    let rirb = self.dma.ptr_at::<u64>(HDA_RIRB_OFF);
                    core::ptr::read_volatile(rirb.add(wp as usize))
                };
                self.mw8(HDA_RIRBSTS, 0x05);
                return Some(response as u32);
            }
            core::hint::spin_loop();
        }
        None
    }

    fn mr16(&self, off: u32) -> u16 {
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u16) }
    }
    fn mr32(&self, off: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.mmio + off as u64) as *const u32) }
    }
    fn mw8(&self, off: u32, value: u8) {
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u8, value) }
    }
    fn mw16(&self, off: u32, value: u16) {
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u16, value) }
    }
    fn mw32(&self, off: u32, value: u32) {
        unsafe { core::ptr::write_volatile((self.mmio + off as u64) as *mut u32, value) }
    }
}

fn spin_wait(cycles: u32) {
    for _ in 0..cycles {
        core::hint::spin_loop();
    }
}

fn wait_until<F: Fn() -> bool>(tries: usize, predicate: F) -> Result<()> {
    for _ in 0..tries {
        if predicate() {
            return Ok(());
        }
        spin_wait(1000);
    }
    Err(AudioError::Timeout)
}

// ===========================================================================
// Backend facade
// ===========================================================================

enum Backend {
    Hda(Hda),
    Ac97(Ac97),
}

impl Backend {
    fn discover() -> Option<Self> {
        match Hda::discover() {
            Ok(hda) => {
                log("audiod: Intel HD Audio backend ready");
                return Some(Self::Hda(hda));
            }
            Err(AudioError::NoDevice) => {}
            Err(err) => log_error("audiod: Intel HD Audio probe failed", err),
        }

        match Ac97::discover() {
            Ok(ac97) => {
                log("audiod: AC97 backend ready");
                Some(Self::Ac97(ac97))
            }
            Err(AudioError::NoDevice) => None,
            Err(err) => {
                log_error("audiod: AC97 probe failed", err);
                None
            }
        }
    }

    fn start_stream(&mut self, source: Source) -> Result<()> {
        match self {
            Self::Hda(dev) => dev.start_stream(source),
            Self::Ac97(dev) => dev.start_stream(source),
        }
    }

    fn pump(&mut self) {
        match self {
            Self::Hda(dev) => dev.pump(),
            Self::Ac97(dev) => dev.pump(),
        }
    }

    fn stop(&mut self) {
        match self {
            Self::Hda(dev) => dev.stop(),
            Self::Ac97(dev) => dev.stop(),
        }
    }

    fn is_playing(&self) -> bool {
        match self {
            Self::Hda(dev) => dev.is_playing(),
            Self::Ac97(dev) => dev.is_playing(),
        }
    }

    fn set_output(&mut self, volume: u8, muted: bool) {
        match self {
            Self::Hda(dev) => dev.set_output(volume, muted),
            Self::Ac97(dev) => dev.set_output(volume, muted),
        }
    }
}

// ===========================================================================
// Service layer: owns policy and IPC contract
// ===========================================================================

struct AudioService {
    port: PortId,
    config: AudioConfig,
    device: Option<Backend>,
    boot_chime_done: bool,
}

impl AudioService {
    fn new(port: PortId) -> Self {
        let config = AudioConfig::load();
        let mut device = Backend::discover();
        if let Some(dev) = device.as_mut() {
            dev.set_output(config.volume, config.muted);
        } else {
            log("audiod: no supported audio hardware; IPC service will still run");
        }
        Self {
            port,
            config,
            device,
            boot_chime_done: false,
        }
    }

    fn state(&self) -> AudioStateReplyMsg {
        AudioStateReplyMsg {
            volume: self.config.volume,
            muted: self.config.muted,
            available: self.device.is_some(),
            playing: self.device.as_ref().map(|d| d.is_playing()).unwrap_or(false),
        }
    }

    fn send_state(&self, reply_port: PortId) {
        if reply_port == 0 {
            return;
        }
        let _ = send_message(reply_port, MessageType::AudioStateReply, &self.state().to_bytes());
    }

    fn play_startup_chime(&mut self) {
        if !self.boot_chime_done {
            self.play_path(STARTUP_SOUND_PATH);
        }
    }

    fn play_path(&mut self, path: &str) {
        if self.config.muted {
            return;
        }
        let Some(device) = self.device.as_mut() else {
            return;
        };

        let result = if path == STARTUP_SOUND_PATH {
            device.start_stream(Source::memory(STARTUP_SOUND))
        } else {
            match fs::open(path, fs::OpenFlags::RDONLY, 0) {
                Ok(fd) => device.start_stream(Source::file(fd)),
                Err(_) => Err(AudioError::Io),
            }
        };

        match result {
            Ok(()) => {
                if path == STARTUP_SOUND_PATH {
                    self.boot_chime_done = true;
                    log("audiod: startup chime playing");
                }
            }
            Err(err) => log_error("audiod: play failed", err),
        }
    }

    fn set_state(&mut self, volume: u8, muted: bool) {
        self.config.volume = volume.min(100);
        self.config.muted = muted;
        if let Some(dev) = self.device.as_mut() {
            dev.set_output(self.config.volume, self.config.muted);
            if self.config.muted {
                dev.stop();
            }
        }
        self.config.save();
    }

    fn stop(&mut self) {
        if let Some(dev) = self.device.as_mut() {
            dev.stop();
        }
    }

    fn handle_message(&mut self, bytes: &[u8]) {
        let Some(header) = MessageHeader::from_bytes(bytes) else {
            return;
        };
        let payload = get_payload(bytes, bytes.len());

        match header.msg_type {
            MessageType::AudioGetState => {
                if let Some(req) = AudioGetStateMsg::from_bytes(payload) {
                    self.send_state(req.reply_port);
                }
            }
            MessageType::AudioSetState => {
                if let Some(req) = AudioSetStateMsg::from_bytes(payload) {
                    self.set_state(req.volume, req.muted);
                    self.send_state(req.reply_port);
                }
            }
            MessageType::AudioPlayFile => {
                if let Some(req) = AudioPlayFileMsg::from_bytes(payload) {
                    self.play_path(&req.path);
                }
            }
            MessageType::AudioStop => {
                self.stop();
                if let Some(req) = AudioStopMsg::from_bytes(payload) {
                    self.send_state(req.reply_port);
                }
            }
            _ => {}
        }
    }

    fn pump(&mut self) -> bool {
        if let Some(dev) = self.device.as_mut() {
            dev.pump();
            dev.is_playing()
        } else {
            false
        }
    }
}

fn main() -> ! {
    log("audiod: starting audio service");
    let port = match create_port() {
        Ok(port) => port,
        Err(_) => fatal("audiod: create_port failed"),
    };

    let _ = register_service(SERVICE_NAME, port);
    let mut service = AudioService::new(port);
    service.play_startup_chime();

    let ports = [service.port];
    let mut buffer = [0u8; IPC_BUF_BYTES];
    loop {
        while let Ok(Some(len)) = try_recv(service.port, &mut buffer) {
            service.handle_message(&buffer[..len]);
        }
        let playing = service.pump();
        let _ = wait_any(&ports, if playing { ACTIVE_POLL_MS } else { IDLE_POLL_MS });
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    fatal("audiod: panic");
}