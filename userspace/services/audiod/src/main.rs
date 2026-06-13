//! Atom OS audio service.
//!
//! Initial backend: Intel 82801AA AC'97, as exposed by QEMU's `-device AC97`.
//! Clients send WAV paths and global volume requests over IPC; only this
//! service owns the PCI device and DMA buffers.

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
    AudioGetStateMsg, AudioPlayFileMsg, AudioSetStateMsg, AudioStateReplyMsg, MessageHeader,
    MessageType,
};
use libipc::protocol::{get_payload, register_service, send_message};

const HEAP_SIZE: usize = 256 * 1024;
const CONFIG_PATH: &str = "/user/config/audio.cfg";
const STARTUP_SOUND_PATH: &str = "/system/sounds/startup.wav";
const STARTUP_SOUND: &[u8] = include_bytes!("../../../system_apps/ui_shell/sounds/startup.wav");
const MAX_DESCRIPTORS: usize = 32;
const BDL_BYTES: usize = MAX_DESCRIPTORS * core::mem::size_of::<BufferDescriptor>();
const AUDIO_OFFSET: usize = 4096;

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
    dma_size: usize,
    playing: bool,
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
                dma_size: 0,
                playing: false,
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

        // Variable-rate audio, fixed to the WAV format accepted below.
        let ext = self.read16(self.mixer + 0x2a).unwrap_or(0);
        let _ = self.write16(self.mixer + 0x2a, ext | 1);
        let _ = self.write16(self.mixer + 0x2c, 48_000);
        let _ = self.write16(self.mixer + 0x18, 0);
        self.reset_pcm_out();
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
        let _ = self.write16(self.bus_master + 0x16, 0x001c);
    }

    fn play_wav(&mut self, bytes: &[u8]) -> bool {
        let Some(pcm) = parse_pcm_wav(bytes) else {
            log("audiod: unsupported WAV (need 48 kHz, stereo, signed 16-bit PCM)");
            return false;
        };
        if pcm.is_empty() {
            return false;
        }

        let descriptor_count = pcm.len().div_ceil(0x1fffc);
        if descriptor_count == 0 || descriptor_count > MAX_DESCRIPTORS {
            log("audiod: WAV too large for initial AC97 descriptor ring");
            return false;
        }

        let required = AUDIO_OFFSET + pcm.len();
        if required > self.dma_size {
            let params = DmaAllocParams {
                size: required.div_ceil(4096).saturating_mul(4096) as u64,
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
            self.dma_size = info.size as usize;
        }

        self.reset_pcm_out();
        unsafe {
            core::ptr::write_bytes(self.dma_va, 0, BDL_BYTES);
            core::ptr::copy_nonoverlapping(pcm.as_ptr(), self.dma_va.add(AUDIO_OFFSET), pcm.len());
        }

        let descriptors = self.dma_va as *mut BufferDescriptor;
        let mut offset = 0usize;
        let mut index = 0usize;
        while offset < pcm.len() {
            let bytes_here = (pcm.len() - offset).min(0x1fffc) & !1;
            let samples = (bytes_here / 2) as u32;
            let last = offset + bytes_here >= pcm.len();
            unsafe {
                core::ptr::write_volatile(
                    descriptors.add(index),
                    BufferDescriptor {
                        address: (self.dma_phys + AUDIO_OFFSET as u64 + offset as u64) as u32,
                        control: samples | if last { 1 << 31 } else { 0 },
                    },
                );
            }
            offset += bytes_here;
            index += 1;
        }

        core::sync::atomic::fence(Ordering::Release);
        if self
            .write32(self.bus_master + 0x10, self.dma_phys as u32)
            .is_err()
        {
            return false;
        }
        let _ = self.write8(self.bus_master + 0x15, (index - 1) as u8);
        let _ = self.write8(self.bus_master + 0x1b, 0x01);
        self.playing = true;
        true
    }

    fn refresh_playing(&mut self) {
        if !self.playing {
            return;
        }
        if self.read16(self.bus_master + 0x16).unwrap_or(1) & 1 != 0 {
            self.playing = false;
        }
    }

    fn read8(&self, port: u16) -> Result<u8, ()> {
        atom_syscall::io::port_read_u8(port).map_err(|_| ())
    }

    fn read16(&self, port: u16) -> Result<u16, ()> {
        Ok(self.read8(port)? as u16 | ((self.read8(port + 1)? as u16) << 8))
    }

    fn write8(&self, port: u16, value: u8) -> Result<(), ()> {
        atom_syscall::io::port_write_u8(port, value).map_err(|_| ())
    }

    fn write16(&self, port: u16, value: u16) -> Result<(), ()> {
        self.write8(port, value as u8)?;
        self.write8(port + 1, (value >> 8) as u8)
    }

    fn write32(&self, port: u16, value: u32) -> Result<(), ()> {
        for shift in [0, 8, 16, 24] {
            self.write8(port + (shift / 8) as u16, (value >> shift) as u8)?;
        }
        Ok(())
    }
}

struct AudioService {
    port: PortId,
    config: AudioConfig,
    device: Option<Ac97>,
}

impl AudioService {
    fn state(&mut self) -> AudioStateReplyMsg {
        if let Some(device) = self.device.as_mut() {
            device.refresh_playing();
        }
        AudioStateReplyMsg {
            volume: self.config.volume,
            muted: self.config.muted,
            available: self.device.is_some(),
            playing: self.device.as_ref().map(|d| d.playing).unwrap_or(false),
        }
    }

    fn send_state(&mut self, reply_port: PortId) {
        let state = self.state();
        let _ = send_message(reply_port, MessageType::AudioStateReply, &state.to_bytes());
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
                    if let Some(device) = self.device.as_ref() {
                        device.set_output(self.config.volume, self.config.muted);
                    }
                    self.config.save();
                    self.send_state(request.reply_port);
                }
            }
            MessageType::AudioPlayFile => {
                if self.config.muted {
                    return;
                }
                if let Some(request) = AudioPlayFileMsg::from_bytes(payload) {
                    if request.path == STARTUP_SOUND_PATH {
                        if let Some(device) = self.device.as_mut() {
                            if device.play_wav(STARTUP_SOUND) {
                                log("audiod: startup chime playing");
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_pcm_wav(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.get(0..4)? != b"RIFF" || bytes.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut offset = 12usize;
    let mut format_ok = false;
    let mut data = None;
    while offset + 8 <= bytes.len() {
        let id = bytes.get(offset..offset + 4)?;
        let len = u32::from_le_bytes(bytes.get(offset + 4..offset + 8)?.try_into().ok()?) as usize;
        let start = offset + 8;
        let end = start.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        if id == b"fmt " && len >= 16 {
            let format = u16::from_le_bytes(bytes.get(start..start + 2)?.try_into().ok()?);
            let channels = u16::from_le_bytes(bytes.get(start + 2..start + 4)?.try_into().ok()?);
            let rate = u32::from_le_bytes(bytes.get(start + 4..start + 8)?.try_into().ok()?);
            let bits = u16::from_le_bytes(bytes.get(start + 14..start + 16)?.try_into().ok()?);
            format_ok = format == 1 && channels == 2 && rate == 48_000 && bits == 16;
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        offset = end + (len & 1);
    }
    if format_ok {
        data
    } else {
        None
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
    };
    let ports = [service.port];
    let mut buffer = [0u8; 512];
    loop {
        while let Ok(Some(len)) = try_recv(service.port, &mut buffer) {
            service.handle(&buffer[..len]);
        }
        if let Some(device) = service.device.as_mut() {
            device.refresh_playing();
        }
        let _ = wait_any(&ports, 50);
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
