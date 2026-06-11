// Atom OS – System Settings
//
// Unified settings panel.  Sidebar on the LEFT, content on the RIGHT.
// No inner header – the window chrome already shows the app name.
//
//   PERSONALIZATION
//     Desktop Background  – solid-colour swatches + wallpaper browser + scaling
//
//   SYSTEM SETTINGS
//     Network             – connection status + IP/DNS info + DNS preset buttons
//     Display Resolution  – scrollable video-mode list
//     Date and Time       – internet sync, locale, time zone, and clock format
//     About System        – OS / hardware / network summary
//
// Performance notes
// -----------------
//   * Heavy IPC (net config) is deferred to after the first frame renders.
//   * wait_any uses a 32 ms budget (was 16) – settings panel, not a game.
//   * Dynamic data (network, memory, uptime) is refreshed every 5 s only while
//     the relevant page is active; static CPU/arch data is collected once.
//   * The wallpaper directory scan is also deferred to after first render.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::panic::PanicInfo;

use atom_syscall::graphics::{
    SharedSurface, VideoModeEntry, get_video_modes, video_mode_count, VIDEO_MAX_MODES, Color,
};
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::thread::{exit, yield_now, get_ticks, get_cpu_count};
use atom_syscall::debug::{log, get_cpu_brand, get_memory_info};
use atom_syscall::graphics::set_video_mode;

use libipc::messages::{
    MessageType, MessageHeader,
    SurfaceAssignMsg, SurfacePresentMsg,
    KeyEvent as IpcKeyEvent,
    MouseButtonEvent, MouseButton,
    MouseMoveEvent, MouseScrollEvent,
    OpenInTabMsg,
    WallpaperAppliedMsg, WallpaperFailedMsg,
    ScalingMode, ApplyWallpaperMsg, WallpaperSourceType,
    NetGetConfigMsg, NetGetConfigReplyMsg, NetConfigureMsg,
    TimeGetStateMsg, TimeSetConfigMsg, TimeStateReplyMsg, TIME_LOCALES, TIME_ZONES,
};
use libipc::protocol::{get_payload, send_message};

use alloc::string::String;
use alloc::vec::Vec;

// ── Heap ─────────────────────────────────────────────────────────────────────

const HEAP_SIZE: usize = 512 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpAllocator {}
impl BumpAllocator {
    const fn new() -> Self {
        Self { heap: UnsafeCell::new([0; HEAP_SIZE]), next: AtomicUsize::new(0) }
    }
}
unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let end = aligned + layout.size();
            if end > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(cur, end, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}
#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();
#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! { loop {} }

// ── Theme ─────────────────────────────────────────────────────────────────────

mod theme {
    use atom_syscall::graphics::Color;
    pub const BG:           Color = Color::new(0x0B, 0x0E, 0x13);
    pub const SURFACE:      Color = Color::new(0x14, 0x19, 0x22);
    pub const SIDEBAR_BG:   Color = Color::new(0x0D, 0x11, 0x1A);
    pub const HDR_BG:       Color = Color::new(0x0D, 0x11, 0x1A);
    pub const BORDER:       Color = Color::new(0x28, 0x30, 0x42);
    pub const DIVIDER:      Color = Color::new(0x1A, 0x20, 0x30);
    pub const TEXT:         Color = Color::new(0xE8, 0xEC, 0xF4);
    pub const TEXT_SEC:     Color = Color::new(0xA0, 0xAA, 0xC0);
    pub const TEXT_MUTED:   Color = Color::new(0x55, 0x60, 0x7A);
    pub const ACCENT:       Color = Color::new(0x4C, 0x8D, 0xFF);
    pub const ACCENT_DIM:   Color = Color::new(0x25, 0x4A, 0x88);
    pub const SEL_BG:       Color = Color::new(0x1A, 0x2A, 0x4E);
    pub const ITEM_ACTIVE_BG: Color = Color::new(0x1C, 0x2C, 0x50);
    pub const SUCCESS:      Color = Color::new(0x22, 0xC5, 0x5E);
    pub const WARNING:      Color = Color::new(0xF5, 0x9E, 0x0B);
    pub const SB_TRACK:     Color = Color::new(0x16, 0x1C, 0x2A);
    pub const SB_THUMB:     Color = Color::new(0x38, 0x46, 0x64);
    pub const SB_THUMB_ACT: Color = Color::new(0x4C, 0x8D, 0xFF);
    pub const BTN_PRIMARY:  Color = Color::new(0x4C, 0x8D, 0xFF);
    pub const BTN_GHOST:    Color = Color::new(0x1E, 0x26, 0x3C);
    pub const BTN_TEXT:     Color = Color::new(0xFF, 0xFF, 0xFF);
    pub const CARD_BG:      Color = Color::new(0x10, 0x15, 0x20);
}

// ── Layout constants ──────────────────────────────────────────────────────────

const CW: u32 = 8;
const CH: u32 = 8;

const SIDEBAR_W: u32 = 200;
const SB_PAD:    u32 = 12;
const ITEM_H:    u32 = 30;
const STAT_H:    u32 = 22;
const PAD:       u32 = 16;

const PTITLE_H:  u32 = CH + 14;  // page title block height
const SEC_LBL_H: u32 = CH + 8;   // section label row height

// Desktop Background – mode toggle
const MTOG_H: u32 = 26;
const MTOG_W: u32 = 118;
const MTOG_G: u32 = 2;

// Color swatches – 4 × 4
const CSZ:  u32 = 40;
const CGAP: u32 = 8;

// Scaling buttons – single row of 5
const SBTN_W: u32 = 58;
const SBTN_H: u32 = 22;
const SBTN_G: u32 = 6;

// Wallpaper image tiles
const TW:    u32 = 84;
const TH:    u32 = 72;
const TG:    u32 = 8;
const TCOLS: usize = 4;

// Resolution page
const ROW_H:    u32 = 22;
const VIS_ROWS: usize = 12;
const SBW:      u32 = 8;

// Apply / ghost buttons
const BTN_H:  u32 = 28;
const BTN_W:  u32 = 90;
const RBTN_W: u32 = 128;

// Refresh interval: 500 ticks ≈ 5 s (get_ticks returns centiseconds)
const REFRESH_TICKS: u64 = 500;

const DEF_W: u16 = 1024;
const DEF_H: u16 = 768;

// ── Data types ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page { DesktopBg, Network, DisplayRes, DateTime, AboutSys }

#[derive(Clone, Copy, PartialEq, Eq)]
enum WpMode { Color, Image }

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind { None, Ok, Warn }

struct Status {
    kind: StatusKind, buf: [u8; 64], len: usize, ticks: u32,
}
impl Status {
    const fn new() -> Self { Self { kind: StatusKind::None, buf: [0; 64], len: 0, ticks: 0 } }
    fn set(&mut self, kind: StatusKind, msg: &[u8]) {
        self.kind = kind;
        self.len = msg.len().min(63);
        self.buf[..self.len].copy_from_slice(&msg[..self.len]);
        self.ticks = 300;
    }
    fn tick(&mut self) -> bool {
        if self.ticks > 0 {
            self.ticks -= 1;
            if self.ticks == 0 { self.kind = StatusKind::None; return true; }
        }
        false
    }
    fn as_str(&self) -> &str { core::str::from_utf8(&self.buf[..self.len]).unwrap_or("") }
}

#[derive(Clone, Copy)]
struct Mode { w: u16, h: u16 }

#[derive(Clone, PartialEq, Eq)]
enum WpSrc { Color { rgb: u32 }, Image { path: String } }

#[derive(Clone, Copy)]
enum ThumbSt { Pending, Loaded { w: u16, h: u16 }, Fail }

struct WpImage { name: String, path: String, thumb: ThumbSt }

struct WpState {
    mode:     WpMode,
    swatches: [Color; 16],
    images:   Vec<WpImage>,
    selected: Option<WpSrc>,
    scaling:  ScalingMode,
    loading:  bool,
}
impl WpState {
    fn new() -> Self {
        Self {
            mode: WpMode::Color,
            swatches: [
                // deep darks
                Color::new(0x0B,0x0E,0x13), Color::new(0x11,0x11,0x11),
                Color::new(0x1A,0x1A,0x2E), Color::new(0x16,0x21,0x3E),
                // mid-dark
                Color::new(0x1F,0x2D,0x4E), Color::new(0x2C,0x1A,0x3E),
                Color::new(0x0F,0x2F,0x27), Color::new(0x3D,0x1A,0x1A),
                // medium / warm
                Color::new(0x4A,0x4A,0x5A), Color::new(0x5A,0x46,0x3A),
                Color::new(0x3A,0x4E,0x3A), Color::new(0x6A,0x55,0x44),
                // light
                Color::new(0xC8,0xCC,0xD8), Color::new(0xE8,0xE4,0xD8),
                Color::new(0xC4,0xDC,0xF4), Color::new(0xF4,0xF4,0xF4),
            ],
            images: Vec::new(),
            selected: None,
            scaling: ScalingMode::Fill,
            loading: false,
        }
    }
    fn pick_color(&mut self, i: usize) {
        if i >= 16 { return; }
        let c = self.swatches[i];
        let rgb = ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        self.selected = Some(WpSrc::Color { rgb });
        self.mode = WpMode::Color;
    }
    fn pick_image(&mut self, i: usize) {
        if let Some(img) = self.images.get(i) {
            self.selected = Some(WpSrc::Image { path: img.path.clone() });
            self.mode = WpMode::Image;
        }
    }
    fn discover(&mut self) {
        use atom_syscall::fs::{open, readdir, close, OpenFlags, FileType};
        self.loading = true;
        self.images.clear();
        let fd = match open("/system/wallpapers/", OpenFlags::DIRECTORY, 0) {
            Ok(fd) => fd,
            Err(_) => { self.loading = false; return; }
        };
        let entries = match readdir(fd) {
            Ok(e) => e,
            Err(_) => { let _ = close(fd); self.loading = false; return; }
        };
        let _ = close(fd);
        for e in entries {
            if e.file_type == FileType::Directory { continue; }
            let lc = e.name.to_lowercase();
            if !lc.ends_with(".jpg") && !lc.ends_with(".jpeg") { continue; }
            let mut p = String::from("/system/wallpapers/");
            p.push_str(&e.name);
            self.images.push(WpImage { name: e.name, path: p, thumb: ThumbSt::Pending });
            if self.images.len() >= 80 { break; }
        }
        self.images.sort_by(|a, b| a.name.cmp(&b.name));
        self.loading = false;
    }
}

// ── Network state ─────────────────────────────────────────────────────────────
//
// Holds the last known network configuration (raw u32 values + formatted
// strings).  Refreshed on demand via refresh(); DNS can be updated via set_dns().
// "Auto DNS" is the value returned by netd at first successful query.

struct NetState {
    connected: bool,
    ip_raw:  u32,
    nm_raw:  u32,
    gw_raw:  u32,
    dns_raw: u32,
    auto_dns: u32,   // DNS received from DHCP – restored when user picks "Auto"
    mac: [u8; 6],
    // Pre-formatted display strings (no_std: no format! macro)
    ip_s:  [u8; 20], ip_l:  usize,
    nm_s:  [u8; 20], nm_l:  usize,
    gw_s:  [u8; 20], gw_l:  usize,
    dns_s: [u8; 20], dns_l: usize,
    mac_s: [u8; 20], mac_l: usize,
}
impl NetState {
    fn new() -> Self {
        Self {
            connected: false,
            ip_raw: 0, nm_raw: 0, gw_raw: 0, dns_raw: 0, auto_dns: 0,
            mac: [0; 6],
            ip_s:  [0;20], ip_l:  0,
            nm_s:  [0;20], nm_l:  0,
            gw_s:  [0;20], gw_l:  0,
            dns_s: [0;20], dns_l: 0,
            mac_s: [0;20], mac_l: 0,
        }
    }
    fn refresh(&mut self) {
        match query_net_fast() {
            Some(cfg) => {
                use libnet::IpAddr;
                let to_raw = |a: &IpAddr| a.to_u32().unwrap_or(0);
                self.ip_raw  = to_raw(&cfg.ip);
                self.nm_raw  = to_raw(&cfg.netmask);
                self.gw_raw  = to_raw(&cfg.gateway);
                self.dns_raw = to_raw(&cfg.dns);
                self.mac     = cfg.mac;
                self.connected = self.ip_raw != 0;
                if self.auto_dns == 0 { self.auto_dns = self.dns_raw; }
                self.ip_l  = fmt_ipv4(&mut self.ip_s,  self.ip_raw);
                self.nm_l  = fmt_ipv4(&mut self.nm_s,  self.nm_raw);
                self.gw_l  = fmt_ipv4(&mut self.gw_s,  self.gw_raw);
                self.dns_l = fmt_ipv4(&mut self.dns_s, self.dns_raw);
                self.mac_l = fmt_mac(&mut self.mac_s,  &self.mac);
            }
            None => { self.connected = false; }
        }
    }
    fn set_dns(&mut self, new_dns_raw: u32) {
        if let Ok(port) = libipc::protocol::lookup_service("netd") {
            let msg = NetConfigureMsg {
                own_ip: self.ip_raw,
                netmask: self.nm_raw,
                gateway: self.gw_raw,
                dns_server: new_dns_raw,
            };
            let _ = send_message(port, MessageType::NetConfigure, &msg.to_bytes());
            self.dns_raw = new_dns_raw;
            self.dns_l = fmt_ipv4(&mut self.dns_s, new_dns_raw);
        }
    }
    fn ip_str(&self)  -> &str { core::str::from_utf8(&self.ip_s[..self.ip_l]).unwrap_or("-") }
    fn nm_str(&self)  -> &str { core::str::from_utf8(&self.nm_s[..self.nm_l]).unwrap_or("-") }
    fn gw_str(&self)  -> &str { core::str::from_utf8(&self.gw_s[..self.gw_l]).unwrap_or("-") }
    fn dns_str(&self) -> &str { core::str::from_utf8(&self.dns_s[..self.dns_l]).unwrap_or("-") }
    fn mac_str(&self) -> &str { core::str::from_utf8(&self.mac_s[..self.mac_l]).unwrap_or("-") }
}

// ── Static system info (collected once at startup, fast syscalls only) ────────

struct SysInfo {
    cpu_buf: [u8; 64], cpu_len: usize,
    cores:   u64,
    mem_total: u64, mem_used: u64,
    storage_total: u64, storage_used: u64,
    up_buf:  [u8; 32], up_len: usize,
}
impl SysInfo {
    fn empty() -> Self {
        Self { cpu_buf: [0;64], cpu_len: 0, cores: 0,
               mem_total: 0, mem_used: 0,
               storage_total: 0, storage_used: 0,
               up_buf: [0;32], up_len: 0 }
    }
    fn collect_static(&mut self) {
        self.cpu_len = get_cpu_brand(&mut self.cpu_buf);
        self.cores   = get_cpu_count();
        let (total, free) = get_memory_info();
        self.mem_total = total;
        self.mem_used  = total.saturating_sub(free);
        if let Ok(sv) = atom_syscall::fs::statvfs("/") {
            let bs = sv.frsize.max(sv.bsize).max(512);
            self.storage_total = sv.blocks.saturating_mul(bs);
            self.storage_used  = sv.blocks.saturating_sub(sv.bfree).saturating_mul(bs);
        }
    }
    fn refresh_uptime(&mut self) {
        self.up_len = fmt_uptime(&mut self.up_buf);
        let (total, free) = get_memory_info();
        self.mem_total = total;
        self.mem_used  = total.saturating_sub(free);
    }
    fn cpu(&self) -> &str { core::str::from_utf8(&self.cpu_buf[..self.cpu_len]).unwrap_or("x86_64") }
    fn uptime(&self) -> &str { core::str::from_utf8(&self.up_buf[..self.up_len]).unwrap_or("-") }
}

// ── App state ─────────────────────────────────────────────────────────────────

struct App {
    wid:    u32,
    cport:  PortId,
    lport:  PortId,
    surf:   Option<SharedSurface>,
    sw:     u32,
    sh:     u32,
    dirty:  bool,
    alive:  bool,

    page:   Page,
    status: Status,

    // Resolution page
    modes:  [Mode; VIDEO_MAX_MODES],
    mcnt:   usize,
    sel:    usize,
    scroll: usize,
    sbdrag: bool,
    sboff:  i32,

    // Desktop background
    wp:    WpState,
    wpscr: usize,

    // Network + About data
    net:          NetState,
    sinfo:        SysInfo,
    time:         Option<TimeStateReplyMsg>,
    time_port:    Option<PortId>,
    last_time_lookup: u64,
    last_refresh: u64,   // last get_ticks() value when data was refreshed
    deferred_done: bool, // slow init completed
}

impl App {
    fn new(wid: u32, cport: PortId, lport: PortId, surf: SharedSurface,
           modes: [Mode; VIDEO_MAX_MODES], mcnt: usize) -> Self {
        let sw = surf.width(); let sh = surf.height();
        let sel = default_mode(&modes, mcnt);
        let mut a = Self {
            wid, cport, lport, surf: Some(surf), sw, sh,
            dirty: true, alive: true,
            page: Page::DesktopBg, status: Status::new(),
            modes, mcnt, sel, scroll: 0, sbdrag: false, sboff: 0,
            wp: WpState::new(), wpscr: 0,
            net: NetState::new(), sinfo: SysInfo::empty(),
            time: None, time_port: None, last_time_lookup: 0,
            last_refresh: 0, deferred_done: false,
        };
        a.clamp_scroll();
        a
    }

    // Deferred initialisation – called once after the first frame is shown so
    // the window appears instantly and the IPC calls don't block startup.
    fn deferred_init(&mut self) {
        self.sinfo.collect_static();
        self.sinfo.refresh_uptime();
        self.net.refresh();
        self.request_time_state();
        self.wp.discover();
        self.last_refresh = get_ticks();
        self.deferred_done = true;
        self.dirty = true;
    }

    // Periodic refresh for dynamic data (memory, uptime, network).
    // Only runs while on pages that actually display the live data.
    fn maybe_refresh(&mut self) {
        if !self.deferred_done { return; }
        let now = get_ticks();
        if now.wrapping_sub(self.last_refresh) < REFRESH_TICKS { return; }
        match self.page {
            Page::Network => {
                self.net.refresh();
                self.dirty = true;
            }
            Page::AboutSys => {
                self.sinfo.refresh_uptime();
                self.net.refresh();
                self.dirty = true;
            }
            Page::DateTime => {
                self.request_time_state();
            }
            _ => {}
        }
        self.last_refresh = now;
    }

    // ── Geometry helpers ──────────────────────────────────────────────────────

    fn cx(&self) -> u32 { SIDEBAR_W + 1 }
    fn cw(&self) -> u32 { self.sw.saturating_sub(SIDEBAR_W + 1) }
    fn content_h(&self) -> u32 { self.sh.saturating_sub(STAT_H) }

    // ── Sidebar hit-test ──────────────────────────────────────────────────────

    fn nav_items(&self) -> [(Page, u32); 5] {
        let cat  = CH + 10;
        let igap = 4;
        let y0 = SB_PAD + cat;                        // Desktop Bg
        let y1 = y0 + ITEM_H + igap + 14 + cat;       // Network
        let y2 = y1 + ITEM_H + igap;                  // Display Res
        let y3 = y2 + ITEM_H + igap;                  // Date and Time
        let y4 = y3 + ITEM_H + igap;                  // About Sys
        [(Page::DesktopBg, y0), (Page::Network, y1),
         (Page::DisplayRes, y2), (Page::DateTime, y3), (Page::AboutSys, y4)]
    }

    fn nav_hit(&self, mx: i32, my: i32) -> Option<Page> {
        if mx < 0 || mx >= SIDEBAR_W as i32 { return None; }
        for (pg, iy) in self.nav_items() {
            if my >= iy as i32 && my < (iy + ITEM_H) as i32 { return Some(pg); }
        }
        None
    }

    // ── Desktop Background layout helpers ─────────────────────────────────────

    fn wp_toggle_y(&self) -> u32 { PTITLE_H + 20 }
    fn wp_body_y(&self)   -> u32 { self.wp_toggle_y() + MTOG_H + 16 }

    fn wp_swatch_row_y(&self, row: u32) -> u32 {
        self.wp_body_y() + SEC_LBL_H + row * (CSZ + CGAP)
    }
    fn swatch_rect(&self, i: usize) -> (u32, u32) {
        let col = (i % 4) as u32; let row = (i / 4) as u32;
        (self.cx() + PAD + 10 + col * (CSZ + CGAP), self.wp_swatch_row_y(row))
    }

    fn wp_scale_y(&self)      -> u32 { self.wp_body_y() }
    fn wp_sbtn_y(&self)       -> u32 { self.wp_scale_y() + SEC_LBL_H }
    fn wp_tiles_label_y(&self)-> u32 { self.wp_sbtn_y() + SBTN_H + 12 }
    fn wp_tile_row_y(&self, row: u32) -> u32 {
        self.wp_tiles_label_y() + SEC_LBL_H + row * (TH + TG)
    }
    fn sbtn_rect(&self, i: usize) -> (u32, u32) {
        (self.cx() + PAD + 10 + i as u32 * (SBTN_W + SBTN_G), self.wp_sbtn_y())
    }
    fn tile_rect(&self, vis: usize) -> (u32, u32) {
        let col = (vis % TCOLS) as u32; let row = (vis / TCOLS) as u32;
        (self.cx() + PAD + 10 + col * (TW + TG), self.wp_tile_row_y(row))
    }

    // ── Resolution page helpers ───────────────────────────────────────────────

    fn res_list_top(&self) -> u32 { PTITLE_H + SEC_LBL_H + 6 }
    fn res_geom(&self) -> (u32, u32, u32, u32) {
        let top = self.res_list_top();
        let h   = VIS_ROWS as u32 * ROW_H;
        let w   = self.cw().saturating_sub(PAD * 2 + SBW + 14);
        let sbx = self.cx() + self.cw().saturating_sub(PAD + SBW);
        (top, h, w, sbx)
    }
    fn sb_geom(&self) -> (u32, u32, u32) {
        let (top, lh, _, _) = self.res_geom();
        let total = self.mcnt as u32; let vis = VIS_ROWS as u32;
        let th = if total > 0 { ((lh * vis) / total).max(14).min(lh) } else { lh };
        let tr = lh.saturating_sub(th);
        let ms = self.mcnt.saturating_sub(VIS_ROWS) as u32;
        let ty = if ms > 0 { top + tr * self.scroll as u32 / ms } else { top };
        (ty, th, tr)
    }
    fn clamp_scroll(&mut self) {
        let max = self.mcnt.saturating_sub(VIS_ROWS);
        if self.scroll > max { self.scroll = max; }
        if self.sel < self.scroll { self.scroll = self.sel; }
        if self.sel >= self.scroll + VIS_ROWS { self.scroll = self.sel + 1 - VIS_ROWS; }
    }
    fn set_scroll_from_drag(&mut self, y: i32) {
        let (top, _, _, _) = self.res_geom();
        let (_, _, tr) = self.sb_geom();
        let max = self.mcnt.saturating_sub(VIS_ROWS);
        if tr == 0 || max == 0 { self.scroll = 0; return; }
        let rel = (y - top as i32).clamp(0, tr as i32) as u32;
        self.scroll = ((rel as u64 * max as u64 + tr as u64 / 2) / tr as u64) as usize;
        self.dirty = true;
    }

    // ── Button positions ──────────────────────────────────────────────────────

    fn apply_btn(&self) -> (u32, u32) {
        let y = self.sh.saturating_sub(STAT_H + BTN_H + 10);
        let x = (self.cx() + self.cw()).saturating_sub(PAD + BTN_W);
        (x, y)
    }
    fn restore_btn(&self) -> (u32, u32) {
        let (ax, ay) = self.apply_btn();
        (ax.saturating_sub(8 + RBTN_W), ay)
    }

    // ── Actions ───────────────────────────────────────────────────────────────

    fn request_time_state(&mut self) {
        if self.time_port.is_none() {
            let now = get_ticks();
            if self.last_time_lookup != 0
                && now.wrapping_sub(self.last_time_lookup) < 3_000
            {
                return;
            }
            self.last_time_lookup = now;
            self.time_port = libipc::protocol::lookup_service("timesync").ok();
        }
        let Some(port) = self.time_port else {
            return;
        };
        let request = TimeGetStateMsg {
            reply_port: self.lport,
        };
        if send_message(port, MessageType::TimeGetState, &request.to_bytes()).is_err() {
            self.time_port = None;
            self.last_time_lookup = get_ticks();
        }
    }

    fn update_time_config(&mut self, automatic: bool, format_24h: bool, locale_id: u8,
                          timezone_id: u8) {
        let Some(port) = self.time_port.or_else(|| {
            let found = libipc::protocol::lookup_service("timesync").ok();
            self.time_port = found;
            found
        }) else {
            self.status.set(StatusKind::Warn, b"Date and time service unavailable.");
            self.dirty = true;
            return;
        };
        let request = TimeSetConfigMsg {
            reply_port: self.lport,
            automatic,
            format_24h,
            locale_id,
            timezone_id,
        };
        if send_message(port, MessageType::TimeSetConfig, &request.to_bytes()).is_ok() {
            self.status.set(StatusKind::Ok, b"Date and time preferences saved.");
        } else {
            self.time_port = None;
            self.status.set(StatusKind::Warn, b"Failed to update date and time.");
        }
        self.dirty = true;
    }

    fn sync_time_now(&mut self) {
        let Some(port) = self.time_port else {
            self.request_time_state();
            return;
        };
        let request = TimeGetStateMsg {
            reply_port: self.lport,
        };
        if send_message(port, MessageType::TimeSyncNow, &request.to_bytes()).is_ok() {
            self.status.set(StatusKind::Ok, b"Synchronizing with the internet...");
        } else {
            self.time_port = None;
            self.status.set(StatusKind::Warn, b"Could not start synchronization.");
        }
        self.dirty = true;
    }

    fn apply_resolution(&mut self) {
        if self.mcnt == 0 { return; }
        let m = self.modes[self.sel];
        match set_video_mode(m.w, m.h, 32) {
            Ok(()) => {
                let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
                let mut b = [0u8; MessageHeader::SIZE];
                b[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
                let _ = send(self.cport, &b);
                self.status.set(StatusKind::Ok, b"Resolution applied.");
            }
            Err(_) => self.status.set(StatusKind::Warn, b"BGA unavailable or mode rejected."),
        }
        self.dirty = true;
    }

    fn restore_default(&mut self) {
        if self.mcnt == 0 { return; }
        self.sel = default_mode(&self.modes, self.mcnt);
        self.clamp_scroll();
        let m = self.modes[self.sel];
        match set_video_mode(m.w, m.h, 32) {
            Ok(()) => {
                let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
                let mut b = [0u8; MessageHeader::SIZE];
                b[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
                let _ = send(self.cport, &b);
                self.status.set(StatusKind::Ok, b"Restored 1024x768.");
            }
            Err(_) => self.status.set(StatusKind::Warn, b"BGA unavailable."),
        }
        self.dirty = true;
    }

    fn apply_wallpaper(&mut self) {
        let src = match &self.wp.selected {
            Some(s) => s.clone(),
            None => { self.status.set(StatusKind::Warn, b"Select a background first."); self.dirty=true; return; }
        };
        let msg = match &src {
            WpSrc::Color { rgb } => ApplyWallpaperMsg {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None, color_rgb: Some(*rgb),
                scaling_mode: self.wp.scaling,
            },
            WpSrc::Image { path } => ApplyWallpaperMsg {
                source_type: WallpaperSourceType::Image,
                image_path: Some(path.clone()), color_rgb: None,
                scaling_mode: self.wp.scaling,
            },
        };
        let payload = msg.to_bytes();
        let hdr = MessageHeader::new(MessageType::ApplyWallpaper, payload.len() as u32);
        let mut buf = Vec::new();
        buf.extend_from_slice(&hdr.to_bytes());
        buf.extend_from_slice(&payload);
        match send(self.cport, &buf) {
            Ok(()) => self.status.set(StatusKind::Ok, b"Applying background..."),
            Err(_) => self.status.set(StatusKind::Warn, b"Failed to send request."),
        }
        self.dirty = true;
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    fn render(&mut self) {
        if !self.dirty { return; }
        self.dirty = false;

        let sp = match self.surf { Some(ref s) => s as *const SharedSurface, None => return };
        let s = unsafe { &*sp };

        s.fill_rect(0, 0, self.sw, self.sh, theme::BG);
        self.draw_sidebar(s);
        s.fill_rect(SIDEBAR_W, 0, 1, self.content_h(), theme::BORDER);
        self.draw_content(s);
        self.draw_status(s);

        let msg = SurfacePresentMsg { window_id: self.wid };
        let hdr = MessageHeader::new(MessageType::SurfacePresent, SurfacePresentMsg::SIZE as u32);
        let mut b = [0u8; MessageHeader::SIZE + SurfacePresentMsg::SIZE];
        b[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        b[MessageHeader::SIZE..].copy_from_slice(&msg.to_bytes());
        let _ = send(self.cport, &b);
    }

    // ── Sidebar ───────────────────────────────────────────────────────────────

    fn draw_sidebar(&self, s: &SharedSurface) {
        let h = self.content_h();
        s.fill_rect(0, 0, SIDEBAR_W, h, theme::SIDEBAR_BG);

        let items = self.nav_items();
        let cat2_y = items[1].1.saturating_sub(CH + 10 + 4);

        self.draw_cat_label(s, SB_PAD, "PERSONALIZATION");
        self.draw_nav_item(s, items[0].1, "Desktop Background", items[0].0 == self.page);

        self.draw_cat_label(s, cat2_y, "SYSTEM SETTINGS");
        self.draw_nav_item(s, items[1].1, "Network",            items[1].0 == self.page);
        self.draw_nav_item(s, items[2].1, "Display Resolution", items[2].0 == self.page);
        self.draw_nav_item(s, items[3].1, "Date and Time",      items[3].0 == self.page);
        self.draw_nav_item(s, items[4].1, "About System",       items[4].0 == self.page);
    }

    fn draw_cat_label(&self, s: &SharedSurface, y: u32, label: &str) {
        s.draw_string(SB_PAD + 4, y + 2, label, theme::TEXT_MUTED, theme::SIDEBAR_BG);
    }

    fn draw_nav_item(&self, s: &SharedSurface, y: u32, label: &str, active: bool) {
        if active {
            s.fill_rect_rounded_aa(6, y, SIDEBAR_W - 12, ITEM_H, 6, theme::ITEM_ACTIVE_BG);
            s.fill_rect_rounded_aa(4, y + 4, 3, ITEM_H - 8, 2, theme::ACCENT);
        }
        let fg = if active { theme::TEXT } else { theme::TEXT_SEC };
        let bg = if active { theme::ITEM_ACTIVE_BG } else { theme::SIDEBAR_BG };
        s.draw_string(SB_PAD + 10, y + (ITEM_H - CH) / 2, label, fg, bg);
    }

    // ── Content dispatch ──────────────────────────────────────────────────────

    fn draw_content(&self, s: &SharedSurface) {
        let cx = self.cx(); let cw = self.cw(); let ch = self.content_h();
        s.fill_rect(cx, 0, cw, ch, theme::BG);
        match self.page {
            Page::DesktopBg  => self.draw_desktop_bg(s),
            Page::Network    => self.draw_network(s),
            Page::DisplayRes => self.draw_display_res(s),
            Page::DateTime   => self.draw_date_time(s),
            Page::AboutSys   => self.draw_about(s),
        }
    }

    // ── Shared drawing helpers ────────────────────────────────────────────────

    fn draw_page_title(&self, s: &SharedSurface, title: &str) {
        let cx = self.cx(); let cw = self.cw();
        let ty = (PTITLE_H - CH) / 2;
        s.draw_string(cx + PAD, ty, title, theme::TEXT, theme::BG);
        let lw = (title.len() as u32 * CW).min(cw.saturating_sub(PAD * 2));
        s.fill_rect(cx + PAD, PTITLE_H - 2, lw, 2, theme::ACCENT_DIM);
    }

    fn draw_card_label(&self, s: &SharedSurface, y: u32, label: &str) {
        s.draw_string(
            self.cx() + PAD + 10,
            y + (SEC_LBL_H - CH) / 2,
            label,
            theme::TEXT_SEC,
            theme::CARD_BG,
        );
    }

    // Draw a filled card background with a subtle border.
    fn draw_card(&self, s: &SharedSurface, y: u32, h: u32) {
        let cx = self.cx(); let cw = self.cw();
        let x = cx + PAD; let w = cw.saturating_sub(PAD * 2);
        s.fill_rect_rounded_aa(x, y, w, h, 8, theme::CARD_BG);
        s.draw_rect_rounded_aa(x, y, w, h, 8, theme::BORDER);
    }

    // ── Desktop Background page ───────────────────────────────────────────────

    fn draw_wp_toggle(&self, s: &SharedSurface) {
        let y  = self.wp_toggle_y();
        let x0 = self.cx() + PAD + 10;
        let tw = MTOG_W * 2 + MTOG_G + 4;
        s.fill_rect_rounded_aa(x0, y, tw, MTOG_H, MTOG_H / 2, theme::SURFACE);
        s.draw_rect_rounded_aa(x0, y, tw, MTOG_H, MTOG_H / 2, theme::BORDER);
        let pill_x = if self.wp.mode == WpMode::Color { x0 + 2 }
                     else { x0 + 2 + MTOG_W + MTOG_G };
        s.fill_rect_rounded_aa(pill_x, y + 2, MTOG_W, MTOG_H - 4, (MTOG_H - 4) / 2, theme::ACCENT);
        let labels = ["Solid Color", "Wallpaper Image"];
        for (i, lbl) in labels.iter().enumerate() {
            let lx = x0 + 2 + i as u32 * (MTOG_W + MTOG_G);
            let active = (i == 0 && self.wp.mode == WpMode::Color)
                      || (i == 1 && self.wp.mode == WpMode::Image);
            let fg = if active { theme::BTN_TEXT } else { theme::TEXT_SEC };
            let bg = if active { theme::ACCENT } else { theme::SURFACE };
            let tx = lx + (MTOG_W.saturating_sub(lbl.len() as u32 * CW)) / 2;
            s.draw_string(tx, y + (MTOG_H - CH) / 2, lbl, fg, bg);
        }
    }

    fn draw_desktop_bg(&self, s: &SharedSurface) {
        self.draw_page_title(s, "Desktop Background");
        self.draw_card(s, PTITLE_H + 10, MTOG_H + 20);
        self.draw_wp_toggle(s);
        match self.wp.mode {
            WpMode::Color => self.draw_wp_color_section(s),
            WpMode::Image => self.draw_wp_image_section(s),
        }
        let (ax, ay) = self.apply_btn();
        s.fill_rect_rounded_aa(ax, ay, BTN_W, BTN_H, 7, theme::BTN_PRIMARY);
        let lbl = "Apply";
        s.draw_string(ax + (BTN_W.saturating_sub(lbl.len() as u32 * CW)) / 2,
                      ay + (BTN_H - CH) / 2, lbl, theme::BTN_TEXT, theme::BTN_PRIMARY);
    }

    fn draw_wp_color_section(&self, s: &SharedSurface) {
        let card_y = self.wp_body_y();
        let card_h = SEC_LBL_H + 4 * (CSZ + CGAP) + 4;
        self.draw_card(s, card_y, card_h);
        self.draw_card_label(s, card_y, "Background Color");
        for i in 0..16 {
            let (sx, sy) = self.swatch_rect(i);
            let c = self.wp.swatches[i];
            let rgb = ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
            let active = matches!(&self.wp.selected, Some(WpSrc::Color { rgb: r }) if *r == rgb);
            s.fill_rect_rounded_aa(sx, sy, CSZ, CSZ, 7, c);
            if active {
                s.draw_rect_rounded_aa(sx.saturating_sub(2), sy.saturating_sub(2), CSZ+4, CSZ+4, 8, theme::ACCENT);
                s.draw_rect_rounded_aa(sx.saturating_sub(3), sy.saturating_sub(3), CSZ+6, CSZ+6, 9, theme::ACCENT_DIM);
            } else {
                let border = if c.r > 0xB0 { theme::TEXT_MUTED } else { theme::BORDER };
                s.draw_rect_rounded_aa(sx, sy, CSZ, CSZ, 7, border);
            }
        }
        let row_labels = ["Dark", "Deep", "Mid", "Light"];
        for (r, lbl) in row_labels.iter().enumerate() {
            let ry = self.wp_swatch_row_y(r as u32) + (CSZ - CH) / 2;
            let lx = self.cx() + PAD + 10 + 4 * (CSZ + CGAP) + 10;
            s.draw_string(lx, ry, lbl, theme::TEXT_MUTED, theme::CARD_BG);
        }
    }

    fn draw_wp_image_section(&self, s: &SharedSurface) {
        let scale_card_y = self.wp_scale_y();
        self.draw_card(s, scale_card_y, SEC_LBL_H + SBTN_H + 10);
        self.draw_card_label(s, scale_card_y, "Image Scaling");
        let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch,
                     ScalingMode::Center, ScalingMode::Tile];
        for (i, mode) in modes.iter().enumerate() {
            let (bx, by) = self.sbtn_rect(i);
            let is_sel = self.wp.scaling == *mode;
            let bg = if is_sel { theme::SEL_BG } else { theme::SURFACE };
            let border = if is_sel { theme::ACCENT } else { theme::BORDER };
            s.fill_rect_rounded_aa(bx, by, SBTN_W, SBTN_H, 5, bg);
            s.draw_rect_rounded_aa(bx, by, SBTN_W, SBTN_H, 5, border);
            let lbl = mode.to_str();
            let lx = bx + (SBTN_W.saturating_sub(lbl.len() as u32 * CW)) / 2;
            s.draw_string(lx, by + (SBTN_H - CH) / 2, lbl, theme::TEXT, bg);
        }
        let tiles_card_y = self.wp_tiles_label_y();
        let tiles_card_h = SEC_LBL_H + 2 * (TH + TG) + 2;
        self.draw_card(s, tiles_card_y, tiles_card_h);
        self.draw_card_label(s, tiles_card_y, "Wallpaper Images");
        let start = self.wpscr;
        let vis   = 2 * TCOLS;
        let end   = (start + vis).min(self.wp.images.len());
        for idx in start..end {
            let vi = idx - start;
            let (tx, ty) = self.tile_rect(vi);
            if ty + TH > self.sh { continue; }
            let img = &self.wp.images[idx];
            let sel = matches!(&self.wp.selected, Some(WpSrc::Image { path: p }) if p == &img.path);
            let bg  = if sel { theme::SEL_BG } else { theme::SURFACE };
            let brd = if sel { theme::ACCENT  } else { theme::BORDER };
            s.fill_rect_rounded_aa(tx, ty, TW, TH, 6, bg);
            s.draw_rect_rounded_aa(tx, ty, TW, TH, 6, brd);
            let ph = TH.saturating_sub(CH + 8);
            let pw = TW.saturating_sub(12);
            let px = tx + 6; let py = ty + 4;
            s.fill_rect_rounded_aa(px, py, pw, ph, 4, Color::new(28, 34, 50));
            match img.thumb {
                ThumbSt::Loaded { w, h } => {
                    let (dw, dh) = fit_in(w as u32, h as u32, pw-4, ph-4);
                    s.fill_rect_rounded_aa(px+(pw-dw)/2, py+(ph-dh)/2, dw, dh, 3, theme::ACCENT_DIM);
                }
                ThumbSt::Fail => {
                    s.draw_rect_rounded_aa(px+6, py+4, pw-12, ph-8, 3, theme::WARNING);
                }
                ThumbSt::Pending => {
                    let dw=pw/3; let dh=ph/3;
                    s.fill_rect_rounded_aa(px+dw, py+dh, dw, dh, 3, theme::SB_THUMB);
                }
            }
            let max_c = (TW / CW) as usize;
            let lbl = if img.name.len() > max_c { &img.name[..max_c] } else { &img.name };
            s.draw_string(tx+4, ty+TH-CH-2, lbl, theme::TEXT_SEC, bg);
        }
        if self.wp.loading {
            s.draw_string(self.cx() + PAD + 10, self.wp_tiles_label_y() + SEC_LBL_H + 4,
                          "Loading...", theme::TEXT_MUTED, theme::CARD_BG);
        } else if self.wp.images.is_empty() {
            s.draw_string(self.cx() + PAD + 10, self.wp_tiles_label_y() + SEC_LBL_H + 4,
                          "No images in /system/wallpapers/", theme::TEXT_MUTED, theme::CARD_BG);
        }
    }

    // ── Network page ──────────────────────────────────────────────────────────
    //
    // Uses the same stacked cards and right-aligned values as Date and Time.
    //
    // net_dns_btn_geom() centralises the DNS button positions so draw and click
    // share exactly the same geometry.

    fn net_dns_btn_geom(&self) -> (u32, u32, u32, u32, u32) {
        let row_h    = 36;
        let card1_y  = PTITLE_H + 10;
        let card2_y  = card1_y + row_h * 4 + 12;
        let btn_y    = card2_y + row_h + 6;
        let kx       = self.cx() + PAD + 10;
        (kx, btn_y, 90, 24, 8) // (kx, btn_y, btn_w, btn_h, btn_gap)
    }

    fn draw_network(&self, s: &SharedSurface) {
        self.draw_page_title(s, "Network");
        let row_h = 36u32;

        // ── Card 1 – Connection ──────────────────────────────────────────────
        let card1_y = PTITLE_H + 10;
        let card1_h = row_h * 4;
        self.draw_card(s, card1_y, card1_h);
        self.draw_time_row(
            s, card1_y, row_h, "Connection",
            if !self.deferred_done { "Loading..." } else if self.net.connected { "Connected" } else { "No connection" },
            false,
        );
        self.draw_time_row(s, card1_y + row_h, row_h, "IP Address", self.net.ip_str(), false);
        self.draw_time_row(s, card1_y + row_h * 2, row_h, "Subnet Mask", self.net.nm_str(), false);
        self.draw_time_row(s, card1_y + row_h * 3, row_h, "Gateway", self.net.gw_str(), false);

        // ── Card 2 – DNS ─────────────────────────────────────────────────────
        let card2_y = card1_y + card1_h + 12;
        let (_, dns_btn_y, dns_btn_w, dns_btn_h, dns_btn_g) = self.net_dns_btn_geom();
        let card2_h = row_h * 2;
        self.draw_card(s, card2_y, card2_h);
        self.draw_time_row(s, card2_y, row_h, "DNS Server", self.net.dns_str(), false);

        let presets    = ["Auto", "8.8.8.8", "1.1.1.1"];
        let preset_dns = [self.net.auto_dns, 0x0808_0808u32, 0x0101_0101u32];
        let kx = self.cx() + PAD + 10;
        for (i, lbl) in presets.iter().enumerate() {
            let bx = kx + i as u32 * (dns_btn_w + dns_btn_g);
            let active = preset_dns[i] == self.net.dns_raw && self.net.dns_raw != 0;
            let bg     = if active { theme::SEL_BG  } else { theme::BTN_GHOST };
            let border = if active { theme::ACCENT   } else { theme::BORDER };
            s.fill_rect_rounded_aa(bx, dns_btn_y, dns_btn_w, dns_btn_h, 5, bg);
            s.draw_rect_rounded_aa(bx, dns_btn_y, dns_btn_w, dns_btn_h, 5, border);
            let tx = bx + (dns_btn_w.saturating_sub(lbl.len() as u32 * CW)) / 2;
            s.draw_string(tx, dns_btn_y + (dns_btn_h - CH) / 2, lbl, theme::TEXT, bg);
        }

        // ── Card 3 – Hardware ────────────────────────────────────────────────
        let card3_y = card2_y + card2_h + 12;
        let card3_h = row_h * 4;
        self.draw_card(s, card3_y, card3_h);
        self.draw_time_row(s, card3_y, row_h, "Interface", "eth0", false);
        self.draw_time_row(s, card3_y + row_h, row_h, "Type", "Ethernet", false);
        self.draw_time_row(s, card3_y + row_h * 2, row_h, "MAC", self.net.mac_str(), false);
        self.draw_time_row(
            s, card3_y + row_h * 3, row_h, "Link",
            if self.net.connected { "Up" } else { "Down" },
            false,
        );

        // ── Refresh button ────────────────────────────────────────────────────
        let (ax, ay) = self.apply_btn();
        let rlbl = "Refresh";
        s.fill_rect_rounded_aa(ax, ay, BTN_W, BTN_H, 7, theme::BTN_GHOST);
        s.draw_rect_rounded_aa(ax, ay, BTN_W, BTN_H, 7, theme::BORDER);
        s.draw_string(ax + (BTN_W.saturating_sub(rlbl.len() as u32 * CW)) / 2,
                      ay + (BTN_H - CH) / 2, rlbl, theme::TEXT, theme::BTN_GHOST);
    }

    // ── Display Resolution page ───────────────────────────────────────────────

    fn draw_display_res(&self, s: &SharedSurface) {
        let cx = self.cx();
        self.draw_page_title(s, "Display Resolution");

        let (ltop, lh, lw, sbx) = self.res_geom();
        self.draw_card(s, PTITLE_H + 10, lh + 30);
        for i in 0..VIS_ROWS {
            let idx = self.scroll + i;
            if idx >= self.mcnt { break; }
            let m = self.modes[idx];
            let ry = ltop + i as u32 * ROW_H;
            let active = idx == self.sel;
            let (rbg, fg, dim) = if active {
                (theme::SEL_BG, theme::TEXT, theme::ACCENT)
            } else {
                (theme::CARD_BG, theme::TEXT_SEC, theme::TEXT_MUTED)
            };
            s.fill_rect(cx + PAD + 10, ry, lw, ROW_H - 1, rbg);
            if active { s.fill_rect(cx + PAD + 10, ry, 3, ROW_H-1, theme::ACCENT); }
            let mut lb = [0u8; 24];
            let label = fmt_res(&mut lb, m.w, m.h);
            s.draw_string(cx + PAD + 18, ry+(ROW_H-CH)/2, label, fg, rbg);
            let bpp = "32bpp";
            let bx = cx + PAD + 10 + lw - bpp.len() as u32 * CW - 6;
            s.draw_string(bx, ry+(ROW_H-CH)/2, bpp, dim, rbg);
            if !active { s.fill_rect(cx + PAD + 18, ry+ROW_H-1, lw-16, 1, theme::DIVIDER); }
        }
        s.fill_rect(sbx, ltop, SBW, lh, theme::SB_TRACK);
        let (ty, th, _) = self.sb_geom();
        let tc = if self.sbdrag { theme::SB_THUMB_ACT } else { theme::SB_THUMB };
        s.fill_rect_rounded_aa(sbx+1, ty, SBW-2, th, 3, tc);

        if self.mcnt > 0 {
            let m = self.modes[self.sel];
            let mut ib = [0u8; 44];
            let info = fmt_info(&mut ib, m.w, m.h);
            s.draw_string(cx + PAD + 10, ltop+lh+6, info, theme::TEXT_MUTED, theme::CARD_BG);
        }
        let (ax, ay) = self.apply_btn();
        let (rx, ry) = self.restore_btn();
        s.fill_rect_rounded_aa(ax, ay, BTN_W, BTN_H, 7, theme::BTN_PRIMARY);
        let al = "Apply";
        s.draw_string(ax+(BTN_W-al.len() as u32*CW)/2, ay+(BTN_H-CH)/2, al, theme::BTN_TEXT, theme::BTN_PRIMARY);
        s.fill_rect_rounded_aa(rx, ry, RBTN_W, BTN_H, 7, theme::BTN_GHOST);
        s.draw_rect_rounded_aa(rx, ry, RBTN_W, BTN_H, 7, theme::BORDER);
        let rl = "Restore Default";
        s.draw_string(rx+(RBTN_W-rl.len() as u32*CW)/2, ry+(BTN_H-CH)/2, rl, theme::BTN_TEXT, theme::BTN_GHOST);
    }

    // ── Date and Time page ───────────────────────────────────────────────────

    fn draw_date_time(&self, s: &SharedSurface) {
        self.draw_page_title(s, "Date and Time");
        let cx = self.cx();
        let row_h = 36u32;
        let card1_y = PTITLE_H + 10;
        let card2_y = card1_y + row_h * 2 + 12;
        let card3_y = card2_y + row_h * 2 + 12;

        self.draw_card(s, card1_y, row_h * 2);
        self.draw_card(s, card2_y, row_h * 2);
        self.draw_card(s, card3_y, row_h);

        let state = self.time;
        let automatic = state.map(|v| v.automatic).unwrap_or(true);
        let format_24h = state.map(|v| v.format_24h).unwrap_or(true);
        let locale_id = state.map(|v| v.locale_id as usize).unwrap_or(1);
        let timezone_id = state.map(|v| v.timezone_id as usize).unwrap_or(1);

        self.draw_time_row(s, card1_y, row_h, "Automatic Date & Time", "", false);
        self.draw_switch(s, card1_y + (row_h - 20) / 2, automatic);

        let mut date_buf = [0u8; 40];
        let date_value = match state {
            Some(value) if value.synced => {
                format_date_time(&mut date_buf, value, get_ticks())
            }
            Some(value) if value.syncing => "Synchronizing...",
            Some(value) if value.last_error != 0 => "Internet unavailable",
            _ => "Waiting for internet",
        };
        self.draw_time_row(s, card1_y + row_h, row_h, "Date & Time", date_value, true);

        self.draw_time_row(
            s,
            card2_y,
            row_h,
            "Locale",
            TIME_LOCALES.get(locale_id).copied().unwrap_or("pt-BR"),
            true,
        );
        self.draw_time_row(
            s,
            card2_y + row_h,
            row_h,
            "Time Zone",
            TIME_ZONES.get(timezone_id).copied().unwrap_or("America/Sao_Paulo"),
            true,
        );

        self.draw_time_row(
            s,
            card3_y,
            row_h,
            "Time Format",
            if format_24h { "24-hour" } else { "12-hour" },
            true,
        );

        let help_y = card3_y + row_h + 16;
        let help = if automatic {
            "Time is synchronized from the internet every 6 hours."
        } else {
            "Automatic sync is off. The last synchronized time keeps running."
        };
        s.draw_string(cx + PAD, help_y, help, theme::TEXT_MUTED, theme::BG);
    }

    fn draw_time_row(&self, s: &SharedSurface, y: u32, h: u32, label: &str,
                     value: &str, chevron: bool) {
        let x = self.cx() + PAD + 10;
        let right = self.cx() + self.cw() - PAD - 10;
        let ty = y + (h - CH) / 2;
        s.draw_string(x, ty, label, theme::TEXT, theme::CARD_BG);
        let arrow_w = if chevron { 16 } else { 0 };
        let value_w = value.len() as u32 * CW;
        let value_x = right.saturating_sub(value_w + arrow_w);
        s.draw_string(value_x, ty, value, theme::TEXT_SEC, theme::CARD_BG);
        if chevron {
            s.draw_string(right - 8, ty, ">", theme::TEXT_MUTED, theme::CARD_BG);
        }
        s.fill_rect(x, y + h - 1, right.saturating_sub(x), 1, theme::DIVIDER);
    }

    fn draw_switch(&self, s: &SharedSurface, y: u32, enabled: bool) {
        let w = 38u32;
        let h = 20u32;
        let x = self.cx() + self.cw() - PAD - 10 - w;
        let bg = if enabled { theme::ACCENT } else { theme::BTN_GHOST };
        s.fill_rect_rounded_aa(x, y, w, h, h / 2, bg);
        s.draw_rect_rounded_aa(x, y, w, h, h / 2, theme::BORDER);
        let knob_x = if enabled { x + w - 17 } else { x + 3 };
        s.fill_rect_rounded_aa(knob_x, y + 3, 14, 14, 7, theme::BTN_TEXT);
    }

    // ── About System page ─────────────────────────────────────────────────────
    //
    // System, hardware, and runtime information use the same row-card pattern.

    fn draw_about(&self, s: &SharedSurface) {
        self.draw_page_title(s, "About System");
        let row_h = 36u32;
        let mut y = PTITLE_H + 10;
        let max_c = (self.cw().saturating_sub(PAD * 2 + 140) / CW) as usize;

        // Card 1 – System
        self.draw_card(s, y, row_h * 3);
        self.draw_time_row(s, y, row_h, "Operating System", "Atom OS 0.2.0", false);
        self.draw_time_row(s, y + row_h, row_h, "Kernel", "0.2.0-smp", false);
        self.draw_time_row(s, y + row_h * 2, row_h, "Architecture", "x86_64", false);
        y += row_h * 3 + 12;

        // Card 2 – Hardware
        let cpu_raw = self.sinfo.cpu();
        let cpu = if cpu_raw.len() > max_c { &cpu_raw[..max_c] } else { cpu_raw };
        let mut cb = [0u8;  8]; let cl = fmt_u64(&mut cb, self.sinfo.cores);
        let mut mb = [0u8; 48]; let ml = fmt_mem(&mut mb, self.sinfo.mem_used, self.sinfo.mem_total);
        let mut sb = [0u8; 48]; let sl = fmt_storage(&mut sb, self.sinfo.storage_used, self.sinfo.storage_total);
        self.draw_card(s, y, row_h * 4);
        self.draw_time_row(s, y, row_h, "Processor", cpu, false);
        self.draw_time_row(s, y + row_h, row_h, "CPU Cores", core::str::from_utf8(&cb[..cl]).unwrap_or("?"), false);
        self.draw_time_row(s, y + row_h * 2, row_h, "Memory", core::str::from_utf8(&mb[..ml]).unwrap_or("?"), false);
        self.draw_time_row(s, y + row_h * 3, row_h, "Storage", core::str::from_utf8(&sb[..sl]).unwrap_or("?"), false);
        y += row_h * 4 + 12;

        // Card 3 – Runtime
        let ip = if self.net.connected { self.net.ip_str() } else { "unavailable" };
        self.draw_card(s, y, row_h * 2);
        self.draw_time_row(s, y, row_h, "Uptime", self.sinfo.uptime(), false);
        self.draw_time_row(s, y + row_h, row_h, "IP Address", ip, false);
    }

    // ── Status bar ────────────────────────────────────────────────────────────

    fn draw_status(&self, s: &SharedSurface) {
        let sy = self.sh.saturating_sub(STAT_H);
        s.fill_rect(0, sy, self.sw, STAT_H, theme::HDR_BG);
        s.fill_rect(0, sy, self.sw, 1, theme::BORDER);
        let ty = sy + (STAT_H - CH) / 2;
        let base_x = SIDEBAR_W + 1 + PAD;
        if self.status.kind != StatusKind::None {
            let col = if self.status.kind == StatusKind::Ok { theme::SUCCESS } else { theme::WARNING };
            s.draw_string(base_x, ty, self.status.as_str(), col, theme::HDR_BG);
        } else {
            let hint = match self.page {
                Page::DesktopBg  => "Select color or image, then Apply",
                Page::Network    => "Network configuration  |  Click a DNS preset to switch",
                Page::DisplayRes => "Select resolution, then Apply  |  R to restore default",
                Page::DateTime   => "Internet time service and regional clock preferences",
                Page::AboutSys   => "Atom OS System Information",
            };
            s.draw_string(base_x, ty, hint, theme::TEXT_MUTED, theme::HDR_BG);
        }
    }

    // ── Input handling ────────────────────────────────────────────────────────

    fn on_key(&mut self, ev: &IpcKeyEvent) {
        if ev.character == 0 {
            if self.page == Page::DisplayRes {
                match ev.scancode & 0x7F {
                    0x48 => { if self.sel>0 { self.sel-=1; self.clamp_scroll(); self.dirty=true; } }
                    0x50 => { if self.sel+1<self.mcnt { self.sel+=1; self.clamp_scroll(); self.dirty=true; } }
                    0x49 => { self.sel=self.sel.saturating_sub(VIS_ROWS); self.clamp_scroll(); self.dirty=true; }
                    0x51 => { let l=self.mcnt.saturating_sub(1); self.sel=(self.sel+VIS_ROWS).min(l); self.clamp_scroll(); self.dirty=true; }
                    _ => {}
                }
            }
            return;
        }
        match ev.character {
            b'\n'|b'\r' => match self.page {
                Page::DesktopBg  => self.apply_wallpaper(),
                Page::DisplayRes => self.apply_resolution(),
                _ => {}
            },
            b'r'|b'R' if self.page == Page::DisplayRes => self.restore_default(),
            _ => {}
        }
    }

    fn on_mouse_down(&mut self, mx: i32, my: i32) {
        if let Some(pg) = self.nav_hit(mx, my) {
            self.page = pg; self.dirty = true; return;
        }
        match self.page {
            Page::DesktopBg  => self.on_wp_click(mx, my),
            Page::Network    => self.on_net_click(mx, my),
            Page::DisplayRes => self.on_res_click(mx, my),
            Page::DateTime   => self.on_date_time_click(mx, my),
            Page::AboutSys   => {}
        }
    }

    fn on_wp_click(&mut self, mx: i32, my: i32) {
        let (ax, ay) = self.apply_btn();
        if in_rect(mx, my, ax, ay, BTN_W, BTN_H) { self.apply_wallpaper(); return; }

        let ty = self.wp_toggle_y();
        let x0 = self.cx() + PAD + 10;
        if in_rect(mx, my, x0 + 2, ty, MTOG_W, MTOG_H) {
            self.wp.mode = WpMode::Color; self.dirty = true; return;
        }
        if in_rect(mx, my, x0 + 2 + MTOG_W + MTOG_G, ty, MTOG_W, MTOG_H) {
            self.wp.mode = WpMode::Image; self.dirty = true; return;
        }

        match self.wp.mode {
            WpMode::Color => {
                for i in 0..16 {
                    let (sx, sy) = self.swatch_rect(i);
                    if in_rect(mx, my, sx, sy, CSZ, CSZ) {
                        self.wp.pick_color(i); self.dirty = true; return;
                    }
                }
            }
            WpMode::Image => {
                let smodes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch,
                              ScalingMode::Center, ScalingMode::Tile];
                for (i, mode) in smodes.iter().enumerate() {
                    let (bx, by) = self.sbtn_rect(i);
                    if in_rect(mx, my, bx, by, SBTN_W, SBTN_H) {
                        self.wp.scaling = *mode; self.dirty = true; return;
                    }
                }
                let start = self.wpscr;
                let end = (start + 2 * TCOLS).min(self.wp.images.len());
                for idx in start..end {
                    let (tx, tiy) = self.tile_rect(idx - start);
                    if in_rect(mx, my, tx, tiy, TW, TH) {
                        self.wp.pick_image(idx); self.dirty = true; return;
                    }
                }
            }
        }
    }

    fn on_net_click(&mut self, mx: i32, my: i32) {
        // Refresh button
        let (ax, ay) = self.apply_btn();
        if in_rect(mx, my, ax, ay, BTN_W, BTN_H) {
            self.net.refresh();
            self.last_refresh = get_ticks();
            self.status.set(StatusKind::Ok, b"Network info refreshed.");
            self.dirty = true;
            return;
        }

        if !self.deferred_done { return; }

        // DNS preset buttons — delegate geometry to the same helper draw_network uses.
        let (kx, dns_btn_y, dns_btn_w, dns_btn_h, dns_btn_g) = self.net_dns_btn_geom();

        let presets    = [self.net.auto_dns, 0x0808_0808u32, 0x0101_0101u32];
        for (i, &dns) in presets.iter().enumerate() {
            let bx = kx + i as u32 * (dns_btn_w + dns_btn_g);
            if in_rect(mx, my, bx, dns_btn_y, dns_btn_w, dns_btn_h) {
                if dns == 0 { return; } // Auto DNS not yet known
                self.net.set_dns(dns);
                // Small delay then refresh to confirm
                self.net.refresh();
                self.last_refresh = get_ticks();
                self.status.set(StatusKind::Ok, b"DNS updated.");
                self.dirty = true;
                return;
            }
        }
    }

    fn on_res_click(&mut self, mx: i32, my: i32) {
        let (ltop, lh, lw, sbx) = self.res_geom();
        let (ty, th, _) = self.sb_geom();
        let cx = self.cx();

        let (ax, ay) = self.apply_btn();
        let (rx, ry) = self.restore_btn();
        if in_rect(mx, my, ax, ay, BTN_W, BTN_H) { self.apply_resolution(); return; }
        if in_rect(mx, my, rx, ry, RBTN_W, BTN_H) { self.restore_default(); return; }

        if in_rect(mx, my, sbx, ltop, SBW, lh) {
            if in_rect(mx, my, sbx, ty, SBW, th) {
                self.sbdrag = true; self.sboff = my - ty as i32;
            } else if (my as u32) < ty {
                self.scroll = self.scroll.saturating_sub(VIS_ROWS);
            } else {
                self.scroll = (self.scroll + VIS_ROWS).min(self.mcnt.saturating_sub(VIS_ROWS));
            }
            self.dirty = true; return;
        }
        if in_rect(mx, my, cx + PAD + 10, ltop, lw, lh) {
            let row = ((my - ltop as i32) / ROW_H as i32) as usize;
            let idx = self.scroll + row;
            if idx < self.mcnt { self.sel = idx; self.clamp_scroll(); self.dirty = true; }
        }
    }

    fn on_date_time_click(&mut self, mx: i32, my: i32) {
        let x = self.cx() + PAD;
        let w = self.cw().saturating_sub(PAD * 2);
        if mx < x as i32 || mx >= (x + w) as i32 {
            return;
        }

        let row_h = 36u32;
        let card1_y = PTITLE_H + 10;
        let card2_y = card1_y + row_h * 2 + 12;
        let card3_y = card2_y + row_h * 2 + 12;
        let Some(state) = self.time else {
            self.request_time_state();
            return;
        };

        if in_rect(mx, my, x, card1_y, w, row_h) {
            self.update_time_config(
                !state.automatic,
                state.format_24h,
                state.locale_id,
                state.timezone_id,
            );
        } else if in_rect(mx, my, x, card1_y + row_h, w, row_h) {
            self.sync_time_now();
        } else if in_rect(mx, my, x, card2_y, w, row_h) {
            self.update_time_config(
                state.automatic,
                state.format_24h,
                (state.locale_id as usize + 1).rem_euclid(TIME_LOCALES.len()) as u8,
                state.timezone_id,
            );
        } else if in_rect(mx, my, x, card2_y + row_h, w, row_h) {
            self.update_time_config(
                state.automatic,
                state.format_24h,
                state.locale_id,
                (state.timezone_id as usize + 1).rem_euclid(TIME_ZONES.len()) as u8,
            );
        } else if in_rect(mx, my, x, card3_y, w, row_h) {
            self.update_time_config(
                state.automatic,
                !state.format_24h,
                state.locale_id,
                state.timezone_id,
            );
        }
    }

    fn on_mouse_move(&mut self, my: i32) {
        if self.sbdrag && self.page == Page::DisplayRes {
            self.set_scroll_from_drag(my - self.sboff);
        }
    }
    fn on_mouse_up(&mut self) {
        if self.sbdrag { self.sbdrag = false; self.dirty = true; }
    }

    fn on_scroll(&mut self, dz: i32) {
        let steps = dz.unsigned_abs().max(1) as usize;
        match self.page {
            Page::DesktopBg => {
                if dz > 0 { self.wpscr = self.wpscr.saturating_sub(steps * TCOLS); }
                else { let max = self.wp.images.len().saturating_sub(2*TCOLS); self.wpscr=(self.wpscr+steps*TCOLS).min(max); }
            }
            Page::DisplayRes => {
                if dz > 0 { self.scroll = self.scroll.saturating_sub(steps); }
                else { let max=self.mcnt.saturating_sub(VIS_ROWS); self.scroll=(self.scroll+steps).min(max); }
            }
            _ => {}
        }
        self.dirty = true;
    }

    // ── Message handling ──────────────────────────────────────────────────────

    fn on_msg(&mut self, buf: &[u8], len: usize) {
        if len < MessageHeader::SIZE { return; }
        let hdr = match MessageHeader::from_bytes(buf) { Some(h) => h, None => return };
        let pay = &buf[MessageHeader::SIZE..len.min(buf.len())];
        match hdr.msg_type {
            MessageType::TerminateRequest => { self.alive = false; }
            MessageType::SurfaceAssign => {
                if let Some(m) = SurfaceAssignMsg::from_bytes(pay) {
                    if let Ok(s) = SharedSurface::from_region(m.region_id, m.width, m.height) {
                        self.sw = m.width; self.sh = m.height; self.surf = Some(s); self.dirty = true;
                    }
                }
            }
            MessageType::KeyPress => {
                if let Some(ev) = IpcKeyEvent::from_bytes(pay) { self.on_key(&ev); }
            }
            MessageType::MouseButtonDown => {
                if let Some(ev) = MouseButtonEvent::from_bytes(pay) {
                    if ev.button == MouseButton::Left { self.on_mouse_down(ev.x, ev.y); }
                }
            }
            MessageType::MouseMove => {
                if let Some(ev) = MouseMoveEvent::from_bytes(pay) { self.on_mouse_move(ev.y); }
            }
            MessageType::MouseButtonUp => {
                if let Some(ev) = MouseButtonEvent::from_bytes(pay) {
                    if ev.button == MouseButton::Left { self.on_mouse_up(); }
                }
            }
            MessageType::MouseScroll => {
                if let Some(ev) = MouseScrollEvent::from_bytes(pay) { self.on_scroll(ev.dz); }
            }
            MessageType::OpenInTab => {
                if let Some(m) = OpenInTabMsg::from_bytes(pay) {
                    self.page = match m.tab_name.as_str() {
                        "Desktop Background"|"Wallpaper"     => Page::DesktopBg,
                        "Network"                            => Page::Network,
                        "Display Resolution"|"Resolution"    => Page::DisplayRes,
                        "Date and Time"|"DateTime"            => Page::DateTime,
                        "About System"                       => Page::AboutSys,
                        _ => return,
                    };
                    self.dirty = true;
                }
            }
            MessageType::WallpaperApplied => {
                if WallpaperAppliedMsg::from_bytes(pay).is_some() {
                    self.status.set(StatusKind::Ok, b"Background applied.");
                    self.dirty = true;
                }
            }
            MessageType::WallpaperFailed => {
                if let Some(m) = WallpaperFailedMsg::from_bytes(pay) {
                    let mut e = String::from("Error: ");
                    e.push_str(&m.error_message);
                    self.status.set(StatusKind::Warn, e.as_bytes());
                    self.dirty = true;
                }
            }
            MessageType::TimeStateReply => {
                if let Some(state) = TimeStateReplyMsg::from_bytes(pay) {
                    self.time = Some(state);
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn run(&mut self) {
        // Render the first empty frame immediately so the window appears fast.
        self.render();

        // Now do the expensive init (network IPC, filesystem scan) with short
        // timeouts. The window is already visible, so any delay here is hidden.
        self.deferred_init();
        self.render();

        let mut buf = [0u8; 128];
        let ports = [self.lport];
        while self.alive {
            while let Ok(Some(len)) = try_recv(self.lport, &mut buf) { self.on_msg(&buf, len); }
            if self.status.tick() { self.dirty = true; }
            self.maybe_refresh();
            if self.dirty { self.render(); }
            let _ = wait_any(&ports, 32); // 32 ms ≈ 30 fps ceiling; settings panel not a game
        }
    }

    fn wait_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buf = [0u8; 128];
        let ports = [port];
        for _ in 0..200 {
            if wait_any(&ports, 50).is_ok() {
                if let Ok(Some(len)) = try_recv(port, &mut buf) {
                    if len >= MessageHeader::SIZE {
                        if let Some(hdr) = MessageHeader::from_bytes(&buf) {
                            if hdr.msg_type == MessageType::SurfaceAssign {
                                if let Some(m) = SurfaceAssignMsg::from_bytes(&buf[MessageHeader::SIZE..]) {
                                    return Some(m);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

// ── Small helpers ─────────────────────────────────────────────────────────────

fn in_rect(mx: i32, my: i32, x: u32, y: u32, w: u32, h: u32) -> bool {
    mx >= x as i32 && mx < (x+w) as i32 && my >= y as i32 && my < (y+h) as i32
}

fn default_mode(modes: &[Mode], cnt: usize) -> usize {
    for i in 0..cnt { if modes[i].w == DEF_W && modes[i].h == DEF_H { return i; } }
    0
}

fn fmt_u64(buf: &mut [u8], mut n: u64) -> usize {
    if buf.is_empty() { return 0; }
    if n == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 20]; let mut cnt = 0;
    while n > 0 { tmp[cnt] = b'0'+(n%10) as u8; cnt+=1; n/=10; }
    if cnt > buf.len() { return 0; }
    for i in 0..cnt { buf[i] = tmp[cnt-1-i]; }
    cnt
}

fn fmt_uptime(buf: &mut [u8; 32]) -> usize {
    let s = get_ticks() / 100;
    let h = s / 3600; let m = (s % 3600) / 60; let ss = s % 60;
    let mut p = 0usize;
    p += fmt_u64(&mut buf[p..], h); buf[p]=b'h'; p+=1; buf[p]=b' '; p+=1;
    if m<10 { buf[p]=b'0'; p+=1; }
    p += fmt_u64(&mut buf[p..], m); buf[p]=b'm'; p+=1; buf[p]=b' '; p+=1;
    if ss<10 { buf[p]=b'0'; p+=1; }
    p += fmt_u64(&mut buf[p..], ss); buf[p]=b's'; p+=1;
    p
}

fn format_date_time<'a>(
    buf: &'a mut [u8; 40],
    state: TimeStateReplyMsg,
    now_tick: u64,
) -> &'a str {
    let local = state.local_unix_seconds(now_tick);
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let mut p = 0usize;

    match state.locale_id {
        0 => {
            p += fmt_u64(&mut buf[p..], month as u64);
            buf[p] = b'/'; p += 1;
            p += fmt_u64(&mut buf[p..], day as u64);
            buf[p] = b'/'; p += 1;
            p += fmt_u64(&mut buf[p..], year as u64);
        }
        5 => {
            p += fmt_u64(&mut buf[p..], year as u64);
            buf[p] = b'/'; p += 1;
            if month < 10 { buf[p] = b'0'; p += 1; }
            p += fmt_u64(&mut buf[p..], month as u64);
            buf[p] = b'/'; p += 1;
            if day < 10 { buf[p] = b'0'; p += 1; }
            p += fmt_u64(&mut buf[p..], day as u64);
        }
        _ => {
            if day < 10 { buf[p] = b'0'; p += 1; }
            p += fmt_u64(&mut buf[p..], day as u64);
            buf[p] = b'/'; p += 1;
            if month < 10 { buf[p] = b'0'; p += 1; }
            p += fmt_u64(&mut buf[p..], month as u64);
            buf[p] = b'/'; p += 1;
            p += fmt_u64(&mut buf[p..], year as u64);
        }
    }

    buf[p] = b','; p += 1;
    buf[p] = b' '; p += 1;
    let mut hour = (seconds / 3_600) as u8;
    let minute = ((seconds % 3_600) / 60) as u8;
    if state.format_24h {
        if hour < 10 { buf[p] = b'0'; p += 1; }
        p += fmt_u64(&mut buf[p..], hour as u64);
        buf[p] = b':'; p += 1;
        if minute < 10 { buf[p] = b'0'; p += 1; }
        p += fmt_u64(&mut buf[p..], minute as u64);
    } else {
        let pm = hour >= 12;
        hour %= 12;
        if hour == 0 { hour = 12; }
        p += fmt_u64(&mut buf[p..], hour as u64);
        buf[p] = b':'; p += 1;
        if minute < 10 { buf[p] = b'0'; p += 1; }
        p += fmt_u64(&mut buf[p..], minute as u64);
        buf[p] = b' '; p += 1;
        buf[p] = if pm { b'P' } else { b'A' }; p += 1;
        buf[p] = b'M'; p += 1;
    }

    core::str::from_utf8(&buf[..p]).unwrap_or("-")
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

fn fmt_ipv4(buf: &mut [u8; 20], ip: u32) -> usize {
    let b = ip.to_be_bytes();
    let mut p = 0usize;
    p += fmt_u64(&mut buf[p..], b[0] as u64); buf[p]=b'.'; p+=1;
    p += fmt_u64(&mut buf[p..], b[1] as u64); buf[p]=b'.'; p+=1;
    p += fmt_u64(&mut buf[p..], b[2] as u64); buf[p]=b'.'; p+=1;
    p += fmt_u64(&mut buf[p..], b[3] as u64);
    p
}

fn fmt_mac(buf: &mut [u8; 20], mac: &[u8; 6]) -> usize {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut p = 0usize;
    for (i, &b) in mac.iter().enumerate() {
        buf[p] = HEX[(b >> 4) as usize]; p+=1;
        buf[p] = HEX[(b & 0xF) as usize]; p+=1;
        if i < 5 { buf[p] = b':'; p+=1; }
    }
    p
}

fn fmt_res<'a>(buf: &'a mut [u8; 24], w: u16, h: u16) -> &'a str {
    let mut p = 0usize;
    p += fmt_u64(&mut buf[p..], w as u64);
    buf[p..p+3].copy_from_slice(b" x "); p+=3;
    p += fmt_u64(&mut buf[p..], h as u64);
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

fn fmt_info<'a>(buf: &'a mut [u8; 44], w: u16, h: u16) -> &'a str {
    let pre = b"Selected: "; buf[..pre.len()].copy_from_slice(pre); let mut p=pre.len();
    p += fmt_u64(&mut buf[p..], w as u64);
    buf[p..p+3].copy_from_slice(b" x "); p+=3;
    p += fmt_u64(&mut buf[p..], h as u64);
    let suf = b" @ 32bpp  60Hz"; buf[p..p+suf.len()].copy_from_slice(suf); p+=suf.len();
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

fn fmt_mem(buf: &mut [u8; 48], used: u64, total: u64) -> usize {
    let mut p = 0;
    p += fmt_u64(&mut buf[p..], used/1024);
    let mid=b" MB / "; for b in mid { buf[p]=*b; p+=1; }
    p += fmt_u64(&mut buf[p..], total/1024);
    let suf=b" MB"; for b in suf { buf[p]=*b; p+=1; }
    p
}

fn fmt_storage(buf: &mut [u8; 48], used: u64, total: u64) -> usize {
    if total == 0 {
        let m = b"N/A"; buf[..m.len()].copy_from_slice(m); return m.len();
    }
    let used_mb  = used  / (1024 * 1024);
    let total_mb = total / (1024 * 1024);
    let mut p = 0;
    p += fmt_u64(&mut buf[p..], used_mb);
    let mid = b" MB / "; for b in mid { buf[p]=*b; p+=1; }
    p += fmt_u64(&mut buf[p..], total_mb);
    let suf = b" MB"; for b in suf { buf[p]=*b; p+=1; }
    p
}

fn fit_in(sw: u32, sh: u32, mw: u32, mh: u32) -> (u32, u32) {
    if sw==0||sh==0||mw==0||mh==0 { return (1,1); }
    let s = ((mw as u64*1024)/sw as u64).min((mh as u64*1024)/sh as u64).max(1);
    (((sw as u64*s)/1024).max(1) as u32, ((sh as u64*s)/1024).max(1) as u32)
}

// Network config query with a short 500 ms timeout so startup is not blocked.
fn query_net_fast() -> Option<libnet::config::NetworkConfig> {
    let netd_port = libipc::protocol::lookup_service("netd").ok()?;
    let reply_port = atom_syscall::ipc::create_port().ok()?;

    let msg = NetGetConfigMsg { reply_port: reply_port as u64 };
    if send_message(netd_port, MessageType::NetGetConfig, &msg.to_bytes()).is_err() {
        let _ = atom_syscall::ipc::close_port(reply_port);
        return None;
    }
    match atom_syscall::ipc::wait_any(&[reply_port], 500) {
        Ok(_) => {
            let mut buf = [0u8; 64];
            if let Ok(len) = atom_syscall::ipc::recv(reply_port, &mut buf) {
                if let Some(hdr) = MessageHeader::from_bytes(&buf) {
                    if hdr.msg_type == MessageType::NetGetConfigReply {
                        let payload = get_payload(&buf, len);
                        if let Some(reply) = NetGetConfigReplyMsg::from_bytes(payload) {
                            let _ = atom_syscall::ipc::close_port(reply_port);
                            use libnet::IpAddr;
                            return Some(libnet::config::NetworkConfig {
                                ip:      IpAddr::from_u32(reply.own_ip),
                                netmask: IpAddr::from_u32(reply.netmask),
                                gateway: IpAddr::from_u32(reply.gateway),
                                dns:     IpAddr::from_u32(reply.dns_server),
                                mac:     reply.mac,
                            });
                        }
                    }
                }
            }
            let _ = atom_syscall::ipc::close_port(reply_port);
            None
        }
        _ => { let _ = atom_syscall::ipc::close_port(reply_port); None }
    }
}

fn query_modes() -> ([Mode; VIDEO_MAX_MODES], usize) {
    let cnt = video_mode_count().min(VIDEO_MAX_MODES);
    if cnt == 0 { return ([Mode{w:0,h:0}; VIDEO_MAX_MODES], 0); }
    let mut raw = [VideoModeEntry::default(); VIDEO_MAX_MODES];
    let n = get_video_modes(&mut raw[..cnt]);
    let mut modes = [Mode{w:0,h:0}; VIDEO_MAX_MODES];
    for i in 0..n { modes[i] = Mode { w: raw[i].width as u16, h: raw[i].height as u16 }; }
    (modes, n)
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

fn main() -> ! {
    log("SystemSettings: starting");
    let port = match create_port() {
        Ok(p) => p,
        Err(_) => { log("SystemSettings: create_port failed"); exit(1); }
    };
    let _ = libipc::protocol::register_service("system_settings", port);

    let reg = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(p) => break p,
            Err(_) => yield_now(),
        }
    };
    let mut rmsg = [0u8; MessageHeader::SIZE + 16];
    let hdr = MessageHeader::new(MessageType::AppRegister, 16);
    rmsg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    rmsg[MessageHeader::SIZE..MessageHeader::SIZE+8].copy_from_slice(&port.to_le_bytes());
    rmsg[MessageHeader::SIZE+8..MessageHeader::SIZE+16].copy_from_slice(&0u64.to_le_bytes());
    let _ = send(reg, &rmsg[..MessageHeader::SIZE+16]);

    let sa = match App::wait_surface(port) {
        Some(sa) => sa,
        None => { log("SystemSettings: surface timeout"); exit(1); }
    };
    let surf = match SharedSurface::from_region(sa.region_id, sa.width, sa.height) {
        Ok(s) => s,
        Err(_) => { log("SystemSettings: surface map failed"); exit(1); }
    };
    let (modes, mcnt) = query_modes();
    let mut app = App::new(sa.window_id, sa.compositor_port, port, surf, modes, mcnt);
    app.run();
    exit(0);
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! { log("SystemSettings: PANIC"); exit(0xFF); }
