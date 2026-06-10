// Atom OS – System Settings
//
// Unified settings panel.  Sidebar on the right, content on the left.
// Three pages under two categories:
//
//   PERSONALIZATION
//     Desktop Background  – solid colours + wallpaper browser + scaling
//
//   SYSTEM SETTINGS
//     Display Resolution  – scrollable video-mode list
//     About System        – OS / hardware / network summary
//
// Lifecycle: identical to the old display_settings app.
// IPC service name: "system_settings"

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
};

use libnet::net_get_config;

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
            let new_next = aligned + layout.size();
            if new_next > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(cur, new_next,
                    Ordering::SeqCst, Ordering::Relaxed).is_ok() {
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

// ── Theme ────────────────────────────────────────────────────────────────────

mod theme {
    use atom_syscall::graphics::Color;
    pub const BG:           Color = Color::new(0x0B, 0x0E, 0x13);
    pub const SURFACE:      Color = Color::new(0x14, 0x19, 0x22);
    pub const SURFACE_ALT:  Color = Color::new(0x1B, 0x21, 0x30);
    pub const BORDER:       Color = Color::new(0x2A, 0x31, 0x42);
    pub const TEXT:         Color = Color::new(0xE8, 0xEC, 0xF4);
    pub const TEXT_SEC:     Color = Color::new(0xAA, 0xB2, 0xC5);
    pub const TEXT_MUTED:   Color = Color::new(0x6C, 0x74, 0x86);
    pub const ACCENT:       Color = Color::new(0x4C, 0x8D, 0xFF);
    pub const ACCENT_DIM:   Color = Color::new(0x2A, 0x55, 0xAA);
    pub const SEL_BG:       Color = Color::new(0x1E, 0x2E, 0x50);
    pub const HDR_BG:       Color = Color::new(0x1E, 0x26, 0x36);
    pub const SIDEBAR_BG:   Color = Color::new(0x10, 0x14, 0x1E);
    pub const SUCCESS:      Color = Color::new(0x22, 0xC5, 0x5E);
    pub const WARNING:      Color = Color::new(0xF5, 0x9E, 0x0B);
    pub const SCROLLBAR:    Color = Color::new(0x1A, 0x20, 0x30);
    pub const THUMB:        Color = Color::new(0x40, 0x4E, 0x70);
    pub const BTN_PRIMARY:  Color = Color::new(0x4C, 0x8D, 0xFF);
    pub const BTN_RESTORE:  Color = Color::new(0x28, 0x30, 0x48);
    pub const BTN_TEXT:     Color = Color::new(0xFF, 0xFF, 0xFF);
    pub const ITEM_HOVER:   Color = Color::new(0x22, 0x2C, 0x44);
    pub const CAT_LABEL:    Color = Color::new(0x55, 0x60, 0x80);
    pub const DIVIDER:      Color = Color::new(0x22, 0x29, 0x3A);
    pub const INFO_LABEL:   Color = Color::new(0x80, 0x90, 0xB0);
    pub const INFO_VALUE:   Color = Color::new(0xD0, 0xD8, 0xEC);
}

// ── Layout constants ─────────────────────────────────────────────────────────

const CHAR_W:       u32 = 8;
const CHAR_H:       u32 = 8;
const HDR_H:        u32 = 36;
const STATUS_H:     u32 = 22;
const SIDEBAR_W:    u32 = 208;
const PAD:          u32 = 14;
const ITEM_H:       u32 = 28;   // sidebar item row height

// Resolution list
const ROW_H:        u32 = 22;
const VISIBLE_ROWS: usize = 11;
const SB_W:         u32 = 8;

// Wallpaper page
const COLOR_SZ:     u32 = 48;
const COLOR_GAP:    u32 = 10;
const SCALE_BTN_W:  u32 = 62;
const SCALE_BTN_H:  u32 = 24;
const SCALE_BTN_GAP:u32 = 8;
const IMG_W:        u32 = 88;
const IMG_H:        u32 = 76;
const IMG_GAP:      u32 = 10;
const IMG_COLS:     usize = 4;

// Sidebar geometry helpers – computed at runtime based on surface_w
// Sidebar occupies [surface_w - SIDEBAR_W .. surface_w]

// ── Data types ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page { DesktopBackground, DisplayResolution, AboutSystem }

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind { None, Ok, Warn }

struct Status {
    kind: StatusKind, text: [u8; 56], len: usize, ticks: u32,
}
impl Status {
    const fn new() -> Self { Self { kind: StatusKind::None, text: [0; 56], len: 0, ticks: 0 } }
    fn set(&mut self, kind: StatusKind, msg: &[u8]) {
        self.kind = kind;
        self.len = msg.len().min(55);
        self.text[..self.len].copy_from_slice(&msg[..self.len]);
        self.ticks = 180;
    }
    fn tick(&mut self) {
        if self.ticks > 0 { self.ticks -= 1; }
        if self.ticks == 0 { self.kind = StatusKind::None; }
    }
    fn as_str(&self) -> &str { core::str::from_utf8(&self.text[..self.len]).unwrap_or("") }
}

#[derive(Clone, Copy)]
struct Mode { width: u16, height: u16 }

const DEFAULT_W: u16 = 1024;
const DEFAULT_H: u16 = 768;

#[derive(Clone, PartialEq, Eq)]
enum WallpaperSource { SolidColor { rgb: u32 }, Image { path: String } }

#[derive(Clone, Copy, PartialEq, Eq)]
enum ThumbState { Unloaded, Loaded { width: u16, height: u16 }, Failed }

struct WpInfo { name: String, path: String, thumb: ThumbState }

struct WallpaperState {
    colors: [Color; 8],
    images: Vec<WpInfo>,
    selected: Option<WallpaperSource>,
    scaling: ScalingMode,
    discovering: bool,
}

impl WallpaperState {
    fn new() -> Self {
        Self {
            colors: [
                Color::new(18, 20, 28), Color::new(12, 14, 22),
                Color::new(24, 28, 42), Color::new(16, 24, 40),
                Color::new(22, 36, 52), Color::new(28, 18, 36),
                Color::new(20, 30, 24), Color::new(34, 20, 20),
            ],
            images: Vec::new(),
            selected: None,
            scaling: ScalingMode::Fill,
            discovering: false,
        }
    }
    fn is_image_selected(&self) -> bool {
        matches!(self.selected, Some(WallpaperSource::Image { .. }))
    }
    fn select_color(&mut self, i: usize) {
        if i >= self.colors.len() { return; }
        let c = self.colors[i];
        let rgb = ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
        self.selected = Some(WallpaperSource::SolidColor { rgb });
    }
    fn select_image(&mut self, i: usize) {
        if let Some(info) = self.images.get(i) {
            self.selected = Some(WallpaperSource::Image { path: info.path.clone() });
        }
    }
    fn discover(&mut self) {
        use atom_syscall::fs::{open, readdir, close, OpenFlags, FileType};
        self.discovering = true;
        self.images.clear();
        let fd = match open("/system/wallpapers/", OpenFlags::DIRECTORY, 0) {
            Ok(fd) => fd,
            Err(_) => { self.discovering = false; return; }
        };
        let entries = match readdir(fd) {
            Ok(e) => e,
            Err(_) => { let _ = close(fd); self.discovering = false; return; }
        };
        let _ = close(fd);
        for e in entries {
            if e.file_type == FileType::Directory { continue; }
            let low = e.name.to_lowercase();
            if !low.ends_with(".jpg") && !low.ends_with(".jpeg") { continue; }
            let mut path = String::from("/system/wallpapers/");
            path.push_str(&e.name);
            self.images.push(WpInfo { name: e.name, path, thumb: ThumbState::Unloaded });
            if self.images.len() >= 80 { break; }
        }
        self.images.sort_by(|a, b| a.name.cmp(&b.name));
        self.discovering = false;
    }
}

// ── About System cached info ──────────────────────────────────────────────────

struct AboutInfo {
    cpu_brand: [u8; 64], cpu_brand_len: usize,
    cpu_cores: u64,
    mem_total_kb: u64, mem_used_kb: u64,
    uptime_buf: [u8; 32], uptime_len: usize,
    ip_buf: [u8; 48], ip_len: usize,
}

impl AboutInfo {
    fn gather() -> Self {
        let mut cpu_brand = [0u8; 64];
        let cpu_brand_len = get_cpu_brand(&mut cpu_brand);
        let cpu_cores = get_cpu_count();

        let (total_kb, free_kb) = get_memory_info();
        let mem_total_kb = total_kb;
        let mem_used_kb = total_kb.saturating_sub(free_kb);

        // Uptime
        let mut uptime_buf = [0u8; 32];
        let uptime_len = {
            let ticks = get_ticks();
            let secs = ticks / 100;
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            let mut p = 0usize;
            p += write_u64(&mut uptime_buf[p..], h);
            uptime_buf[p] = b'h'; p += 1;
            uptime_buf[p] = b' '; p += 1;
            if m < 10 { uptime_buf[p] = b'0'; p += 1; }
            p += write_u64(&mut uptime_buf[p..], m);
            uptime_buf[p] = b'm'; p += 1;
            uptime_buf[p] = b' '; p += 1;
            if s < 10 { uptime_buf[p] = b'0'; p += 1; }
            p += write_u64(&mut uptime_buf[p..], s);
            uptime_buf[p] = b's'; p += 1;
            p
        };

        // IP address
        let mut ip_buf = [0u8; 48];
        let ip_len = gather_ip(&mut ip_buf);

        AboutInfo { cpu_brand, cpu_brand_len, cpu_cores, mem_total_kb, mem_used_kb,
                    uptime_buf, uptime_len, ip_buf, ip_len }
    }
    fn cpu_brand_str(&self) -> &str {
        core::str::from_utf8(&self.cpu_brand[..self.cpu_brand_len]).unwrap_or("x86_64")
    }
    fn uptime_str(&self) -> &str {
        core::str::from_utf8(&self.uptime_buf[..self.uptime_len]).unwrap_or("-")
    }
    fn ip_str(&self) -> &str {
        core::str::from_utf8(&self.ip_buf[..self.ip_len]).unwrap_or("unavailable")
    }
}

fn gather_ip(buf: &mut [u8; 48]) -> usize {
    let port = match libipc::protocol::lookup_service("netd") {
        Ok(p) => p,
        Err(_) => {
            let msg = b"unavailable";
            buf[..msg.len()].copy_from_slice(msg);
            return msg.len();
        }
    };
    match net_get_config(port) {
        Ok(cfg) => {
            use libnet::IpAddr;
            match cfg.ip {
                IpAddr::V4(b) => {
                    let mut p = 0usize;
                    p += write_u64(&mut buf[p..], b[0] as u64);
                    buf[p] = b'.'; p += 1;
                    p += write_u64(&mut buf[p..], b[1] as u64);
                    buf[p] = b'.'; p += 1;
                    p += write_u64(&mut buf[p..], b[2] as u64);
                    buf[p] = b'.'; p += 1;
                    p += write_u64(&mut buf[p..], b[3] as u64);
                    p
                }
                IpAddr::V6(_) => { let m = b"(IPv6)"; buf[..6].copy_from_slice(m); 6 }
            }
        }
        Err(_) => { let m = b"unavailable"; buf[..m.len()].copy_from_slice(m); m.len() }
    }
}

// ── Main app state ────────────────────────────────────────────────────────────

struct SystemSettings {
    window_id:   u32,
    comp_port:   PortId,
    local_port:  PortId,
    surface:     Option<SharedSurface>,
    sw:          u32,   // surface width
    sh:          u32,   // surface height
    dirty:       bool,
    running:     bool,

    // Navigation
    active_page: Page,

    // Status bar
    status: Status,

    // Resolution page
    modes:      [Mode; VIDEO_MAX_MODES],
    mode_count: usize,
    selected:   usize,
    scroll:     usize,
    sb_drag:    bool,
    sb_drag_off:i32,

    // Wallpaper page
    wp: WallpaperState,
    wp_scroll: usize,

    // About page
    about: AboutInfo,
}

impl SystemSettings {
    fn new(window_id: u32, comp_port: PortId, local_port: PortId,
           surface: SharedSurface,
           modes: [Mode; VIDEO_MAX_MODES], mode_count: usize) -> Self {
        let sw = surface.width();
        let sh = surface.height();
        let sel = default_idx(&modes, mode_count);
        let mut s = Self {
            window_id, comp_port, local_port,
            surface: Some(surface), sw, sh,
            dirty: true, running: true,
            active_page: Page::DesktopBackground,
            status: Status::new(),
            modes, mode_count, selected: sel,
            scroll: 0, sb_drag: false, sb_drag_off: 0,
            wp: WallpaperState::new(), wp_scroll: 0,
            about: AboutInfo::gather(),
        };
        s.clamp_scroll();
        s.wp.discover();
        s
    }

    // ── Layout helpers ────────────────────────────────────────────────────────

    /// X coordinate where the sidebar begins.
    fn sidebar_x(&self) -> u32 { self.sw.saturating_sub(SIDEBAR_W) }

    /// Width of the content area.
    fn content_w(&self) -> u32 { self.sw.saturating_sub(SIDEBAR_W + 1) }

    /// Y where content / sidebar body starts (below header).
    fn body_y(&self) -> u32 { HDR_H + 1 }

    /// Height of content / sidebar body.
    fn body_h(&self) -> u32 { self.sh.saturating_sub(HDR_H + 1 + STATUS_H) }

    // ── Sidebar hit testing ───────────────────────────────────────────────────

    /// Given a click inside the sidebar, return the clicked Page (if any).
    fn sidebar_page_at(&self, cx: i32, cy: i32) -> Option<Page> {
        let sx = self.sidebar_x() as i32;
        if cx < sx { return None; }
        for (page, iy) in self.sidebar_item_positions() {
            if cy >= iy as i32 && cy < (iy + ITEM_H) as i32 {
                return Some(page);
            }
        }
        None
    }

    /// Returns (Page, absolute_y) for every sidebar item, in order.
    fn sidebar_item_positions(&self) -> [(Page, u32); 3] {
        let top = self.body_y() + PAD;
        let cat_h = CHAR_H + 12;   // category row height
        let gap   = ITEM_H + 6;    // inter-item gap

        let y0 = top + cat_h + 4;                               // Desktop Background
        let y1 = y0 + gap + cat_h + 4 + 14;                     // Display Resolution (new cat)
        let y2 = y1 + gap;                                       // About System
        [
            (Page::DesktopBackground,  y0),
            (Page::DisplayResolution,  y1),
            (Page::AboutSystem,        y2),
        ]
    }

    // ── Page switching ────────────────────────────────────────────────────────

    fn switch_page(&mut self, page: Page) {
        self.active_page = page;
        self.dirty = true;
    }

    fn handle_open_in_tab(&mut self, name: &str) {
        match name {
            "Desktop Background" | "Wallpaper" => self.switch_page(Page::DesktopBackground),
            "Display Resolution" | "Resolution" => self.switch_page(Page::DisplayResolution),
            "About System"                      => self.switch_page(Page::AboutSystem),
            _ => return,
        }
    }

    // ── IPC presentation ─────────────────────────────────────────────────────

    fn present(&self) {
        let msg = SurfacePresentMsg { window_id: self.window_id };
        let hdr = MessageHeader::new(MessageType::SurfacePresent, SurfacePresentMsg::SIZE as u32);
        let mut buf = [0u8; MessageHeader::SIZE + SurfacePresentMsg::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        buf[MessageHeader::SIZE..].copy_from_slice(&msg.to_bytes());
        let _ = send(self.comp_port, &buf);
    }

    fn notify_mode_changed(&self) {
        let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
        let mut buf = [0u8; MessageHeader::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        let _ = send(self.comp_port, &buf);
    }

    // ── Resolution helpers ────────────────────────────────────────────────────

    fn clamp_scroll(&mut self) {
        let max = self.mode_count.saturating_sub(VISIBLE_ROWS);
        if self.scroll > max { self.scroll = max; }
        if self.selected < self.scroll { self.scroll = self.selected; }
        if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    fn res_list_geom(&self) -> (u32, u32, u32, u32) {
        let sec_h = CHAR_H + 14;
        let top = self.body_y() + PAD + sec_h;
        let h   = VISIBLE_ROWS as u32 * ROW_H;
        let cw  = self.content_w();
        let w   = cw.saturating_sub(PAD * 2 + SB_W + 4);
        let sbx = cw.saturating_sub(PAD + SB_W);
        (top, h, w, sbx)
    }

    fn scrollbar_geom(&self) -> (u32, u32, u32) {
        let (top, list_h, _, _) = self.res_list_geom();
        let total = self.mode_count as u32;
        let vis   = VISIBLE_ROWS as u32;
        let thumb_h = if total > 0 {
            ((list_h * vis) / total).max(14).min(list_h)
        } else { list_h };
        let travel = list_h.saturating_sub(thumb_h);
        let max_s  = self.mode_count.saturating_sub(VISIBLE_ROWS) as u32;
        let thumb_y = if max_s > 0 {
            top + travel * self.scroll as u32 / max_s
        } else { top };
        (thumb_y, thumb_h, travel)
    }

    fn set_scroll_from_y(&mut self, thumb_y: i32) {
        let (top, _, _, _) = self.res_list_geom();
        let (_, _, travel) = self.scrollbar_geom();
        let max_s = self.mode_count.saturating_sub(VISIBLE_ROWS);
        if travel == 0 || max_s == 0 { self.scroll = 0; return; }
        let rel = (thumb_y - top as i32).clamp(0, travel as i32) as u32;
        self.scroll = ((rel as u64 * max_s as u64 + travel as u64 / 2) / travel as u64) as usize;
        self.dirty = true;
    }

    fn apply_resolution(&mut self) {
        if self.mode_count == 0 { return; }
        let m = self.modes[self.selected];
        match set_video_mode(m.width, m.height, 32) {
            Ok(()) => { self.notify_mode_changed(); self.status.set(StatusKind::Ok, b"Resolution applied."); }
            Err(_) => self.status.set(StatusKind::Warn, b"BGA unavailable or mode rejected."),
        }
        self.dirty = true;
    }

    fn restore_default(&mut self) {
        if self.mode_count == 0 { return; }
        self.selected = default_idx(&self.modes, self.mode_count);
        self.clamp_scroll();
        let m = self.modes[self.selected];
        match set_video_mode(m.width, m.height, 32) {
            Ok(()) => { self.notify_mode_changed(); self.status.set(StatusKind::Ok, b"Restored 1024x768."); }
            Err(_) => self.status.set(StatusKind::Warn, b"BGA unavailable."),
        }
        self.dirty = true;
    }

    // ── Wallpaper helpers ─────────────────────────────────────────────────────

    fn color_tile_rect(&self, i: usize) -> (u32, u32) {
        let row = (i / 4) as u32;
        let col = (i % 4) as u32;
        let x = PAD + col * (COLOR_SZ + COLOR_GAP);
        let y = self.body_y() + PAD + CHAR_H + 10 + row * (COLOR_SZ + COLOR_GAP);
        (x, y)
    }

    fn scaling_btn_rect(&self, i: usize) -> (u32, u32) {
        let row = (i / 3) as u32;
        let col = (i % 3) as u32;
        let base_y = self.body_y() + PAD + CHAR_H + 10
            + 2 * (COLOR_SZ + COLOR_GAP) + 10 + CHAR_H + 6;
        let x = PAD + col * (SCALE_BTN_W + SCALE_BTN_GAP);
        let y = base_y + row * (SCALE_BTN_H + SCALE_BTN_GAP);
        (x, y)
    }

    fn img_tile_rect(&self, visible_idx: usize) -> (u32, u32) {
        let col = (visible_idx % IMG_COLS) as u32;
        let row = (visible_idx / IMG_COLS) as u32;
        let base_y = self.body_y() + PAD + CHAR_H + 10
            + 2 * (COLOR_SZ + COLOR_GAP) + 10 + CHAR_H + 6
            + 2 * (SCALE_BTN_H + SCALE_BTN_GAP) + 14 + CHAR_H + 6;
        let x = PAD + col * (IMG_W + IMG_GAP);
        let y = base_y + row * (IMG_H + IMG_GAP);
        (x, y)
    }

    fn apply_wallpaper(&mut self) {
        let source = match &self.wp.selected {
            Some(s) => s.clone(),
            None => { self.status.set(StatusKind::Warn, b"No background selected."); self.dirty = true; return; }
        };
        let msg = match &source {
            WallpaperSource::SolidColor { rgb } => ApplyWallpaperMsg {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None, color_rgb: Some(*rgb),
                scaling_mode: self.wp.scaling,
            },
            WallpaperSource::Image { path } => ApplyWallpaperMsg {
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
        match send(self.comp_port, &buf) {
            Ok(()) => { self.status.set(StatusKind::Ok, b"Applying background..."); }
            Err(_) => { self.status.set(StatusKind::Warn, b"Failed to apply."); }
        }
        self.dirty = true;
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    fn render(&mut self) {
        if !self.dirty { return; }
        self.dirty = false;
        let surface = match self.surface { Some(ref s) => s as *const SharedSurface, None => return };
        let surface = unsafe { &*surface };
        self.draw_header(surface);
        self.draw_sidebar(surface);
        self.draw_divider(surface);
        self.draw_content(surface);
        self.draw_status_bar(surface);
        self.present();
    }

    fn draw_header(&self, s: &SharedSurface) {
        s.fill_rect(0, 0, self.sw, HDR_H, theme::HDR_BG);
        s.fill_rect(0, HDR_H - 1, self.sw, 1, theme::BORDER);
        // Gear icon (simplified pixel art)
        let ix = PAD;
        let iy = (HDR_H - 14) / 2;
        s.fill_rect(ix + 5, iy, 4, 14, theme::ACCENT);
        s.fill_rect(ix, iy + 5, 14, 4, theme::ACCENT);
        s.fill_rect(ix + 3, iy + 3, 8, 8, theme::ACCENT);
        s.fill_rect(ix + 5, iy + 5, 4, 4, theme::HDR_BG);
        // Title
        let title = "System Settings";
        let tx = ix + 20;
        let ty = (HDR_H - CHAR_H) / 2;
        s.draw_string(tx, ty, title, theme::TEXT, theme::HDR_BG);
    }

    fn draw_sidebar(&self, s: &SharedSurface) {
        let sx = self.sidebar_x();
        let by = self.body_y();
        let bh = self.body_h();
        // Background
        s.fill_rect(sx, by, SIDEBAR_W, bh, theme::SIDEBAR_BG);

        let positions = self.sidebar_item_positions();
        let top = by + PAD;
        let cat_y0 = top;
        let cat_y1 = positions[1].1 - CHAR_H - 4 - 14;

        // Category 1: PERSONALIZATION
        self.draw_sidebar_category(s, sx, cat_y0, "PERSONALIZATION");
        // Item: Desktop Background
        self.draw_sidebar_item(s, sx, positions[0].1,
            "Desktop Background", positions[0].0 == self.active_page);

        // Category 2: SYSTEM SETTINGS
        self.draw_sidebar_category(s, sx, cat_y1, "SYSTEM SETTINGS");
        // Item: Display Resolution
        self.draw_sidebar_item(s, sx, positions[1].1,
            "Display Resolution", positions[1].0 == self.active_page);
        // Item: About System
        self.draw_sidebar_item(s, sx, positions[2].1,
            "About System", positions[2].0 == self.active_page);
    }

    fn draw_sidebar_category(&self, s: &SharedSurface, sx: u32, y: u32, label: &str) {
        s.draw_string(sx + PAD, y + 2, label, theme::CAT_LABEL, theme::SIDEBAR_BG);
    }

    fn draw_sidebar_item(&self, s: &SharedSurface, sx: u32, y: u32, label: &str, active: bool) {
        let bg = if active { theme::SEL_BG } else { theme::SIDEBAR_BG };
        s.fill_rect(sx, y, SIDEBAR_W, ITEM_H, bg);
        if active {
            s.fill_rect(sx, y, 3, ITEM_H, theme::ACCENT);
        }
        let fg = if active { theme::TEXT } else { theme::TEXT_SEC };
        let lx = sx + PAD + (if active { 6 } else { 4 });
        s.draw_string(lx, y + (ITEM_H - CHAR_H) / 2, label, fg, bg);
    }

    fn draw_divider(&self, s: &SharedSurface) {
        let sx = self.sidebar_x();
        s.fill_rect(sx.saturating_sub(1), self.body_y(), 1, self.body_h(), theme::BORDER);
    }

    fn draw_content(&self, s: &SharedSurface) {
        let by = self.body_y();
        let bh = self.body_h();
        let cw = self.content_w();
        s.fill_rect(0, by, cw, bh, theme::BG);
        match self.active_page {
            Page::DesktopBackground => self.render_desktop_bg(s),
            Page::DisplayResolution => self.render_display_res(s),
            Page::AboutSystem       => self.render_about(s),
        }
    }

    fn draw_status_bar(&self, s: &SharedSurface) {
        let sy = self.sh - STATUS_H;
        s.fill_rect(0, sy, self.sw, STATUS_H, theme::HDR_BG);
        s.fill_rect(0, sy, self.sw, 1, theme::BORDER);
        let ty = sy + (STATUS_H - CHAR_H) / 2;
        if self.status.kind != StatusKind::None {
            let col = if self.status.kind == StatusKind::Ok { theme::SUCCESS } else { theme::WARNING };
            s.draw_string(PAD, ty, self.status.as_str(), col, theme::HDR_BG);
        } else {
            let hint = match self.active_page {
                Page::DesktopBackground => "Select color or wallpaper, then Apply",
                Page::DisplayResolution => "Select resolution, then Apply",
                Page::AboutSystem       => "Atom OS System Information",
            };
            s.draw_string(PAD, ty, hint, theme::TEXT_MUTED, theme::HDR_BG);
        }
    }

    // ── Desktop Background page ───────────────────────────────────────────────

    fn render_desktop_bg(&self, s: &SharedSurface) {
        let by = self.body_y();
        let cw = self.content_w();
        let sh = self.sh;

        // Page title
        let title_y = by + PAD;
        s.draw_string(PAD, title_y, "Desktop Background", theme::TEXT, theme::BG);
        s.fill_rect(0, title_y + CHAR_H + 4, cw, 1, theme::DIVIDER);

        // Section: Background Color
        let sec1_y = title_y + CHAR_H + 10;
        s.draw_string(PAD, sec1_y, "Background Color", theme::TEXT_SEC, theme::BG);

        // 4×2 color tile grid
        for i in 0..8 {
            let (tx, ty) = self.color_tile_rect(i);
            if ty + COLOR_SZ > sh { continue; }
            let color = self.wp.colors[i];
            let is_sel = match &self.wp.selected {
                Some(WallpaperSource::SolidColor { rgb }) => {
                    let tile_rgb = ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32;
                    tile_rgb == *rgb
                }
                _ => false,
            };
            s.fill_rect_rounded_aa(tx, ty, COLOR_SZ, COLOR_SZ, 6, color);
            if is_sel {
                s.draw_rect_rounded_aa(tx, ty, COLOR_SZ, COLOR_SZ, 6, theme::ACCENT);
                s.draw_rect_rounded_aa(tx + 1, ty + 1, COLOR_SZ - 2, COLOR_SZ - 2, 6, theme::ACCENT);
            } else {
                s.draw_rect_rounded_aa(tx, ty, COLOR_SZ, COLOR_SZ, 6, theme::BORDER);
            }
        }

        // Scaling section label
        let scale_sec_y = self.body_y() + PAD + CHAR_H + 10
            + 2 * (COLOR_SZ + COLOR_GAP) + 10;
        if scale_sec_y + CHAR_H < sh {
            s.draw_string(PAD, scale_sec_y, "Image Scaling", theme::TEXT_SEC, theme::BG);
        }

        // Scaling buttons
        let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch,
                     ScalingMode::Center, ScalingMode::Tile];
        for (i, mode) in modes.iter().enumerate() {
            let (bx, btn_y) = self.scaling_btn_rect(i);
            if btn_y + SCALE_BTN_H > sh { continue; }
            let enabled = self.wp.is_image_selected();
            let active  = self.wp.scaling == *mode;
            let bg = if !enabled { theme::SURFACE_ALT } else if active { theme::SEL_BG } else { theme::SURFACE };
            let fg = if !enabled { theme::TEXT_MUTED } else { theme::TEXT };
            s.fill_rect_rounded_aa(bx, btn_y, SCALE_BTN_W, SCALE_BTN_H, 4, bg);
            s.draw_rect_rounded_aa(bx, btn_y, SCALE_BTN_W, SCALE_BTN_H, 4,
                if active { theme::ACCENT } else { theme::BORDER });
            let lbl = mode.to_str();
            let lx = bx + (SCALE_BTN_W.saturating_sub(lbl.len() as u32 * CHAR_W)) / 2;
            s.draw_string(lx, btn_y + (SCALE_BTN_H - CHAR_H) / 2, lbl, fg, bg);
        }

        // Wallpaper images section
        let img_sec_base_y = self.body_y() + PAD + CHAR_H + 10
            + 2 * (COLOR_SZ + COLOR_GAP) + 10 + CHAR_H + 6
            + 2 * (SCALE_BTN_H + SCALE_BTN_GAP) + 14;
        if img_sec_base_y + CHAR_H < sh {
            s.draw_string(PAD, img_sec_base_y, "Wallpaper Images", theme::TEXT_SEC, theme::BG);
        }

        let vis_rows = 2usize;
        let vis_count = vis_rows * IMG_COLS;
        let start = self.wp_scroll;
        let end = (start + vis_count).min(self.wp.images.len());
        for idx in start..end {
            let vis_i = idx - start;
            let (ix, iy) = self.img_tile_rect(vis_i);
            if iy + IMG_H > sh { continue; }
            let info = &self.wp.images[idx];
            let sel = matches!(&self.wp.selected, Some(WallpaperSource::Image { path }) if path == &info.path);
            let bg = if sel { theme::SEL_BG } else { theme::SURFACE };
            s.fill_rect_rounded_aa(ix, iy, IMG_W, IMG_H, 6, bg);
            s.draw_rect_rounded_aa(ix, iy, IMG_W, IMG_H, 6,
                if sel { theme::ACCENT } else { theme::BORDER });
            // Thumbnail preview area
            let pw = IMG_W.saturating_sub(16);
            let ph = IMG_H.saturating_sub(22);
            let px = ix + 8; let py = iy + 6;
            s.fill_rect_rounded_aa(px, py, pw, ph, 4, Color::new(35, 42, 60));
            match info.thumb {
                ThumbState::Loaded { width, height } => {
                    let (dw, dh) = fit_within(width as u32, height as u32, pw - 4, ph - 4);
                    let dx = px + (pw - dw) / 2; let dy = py + (ph - dh) / 2;
                    s.fill_rect_rounded_aa(dx, dy, dw, dh, 3, theme::ACCENT_DIM);
                    s.draw_rect_rounded_aa(dx, dy, dw, dh, 3, theme::ACCENT);
                }
                ThumbState::Failed => {
                    s.draw_rect_rounded_aa(px + 8, py + 6, pw - 16, ph - 12, 3, theme::WARNING);
                }
                ThumbState::Unloaded => {
                    let dw = pw / 2; let dh = ph / 2;
                    s.fill_rect_rounded_aa(px + dw / 2, py + dh / 2, dw, dh, 3, theme::THUMB);
                }
            }
            // Filename label (truncated)
            let max_chars = (IMG_W / CHAR_W) as usize;
            let label = if info.name.len() > max_chars { &info.name[..max_chars] } else { &info.name };
            s.draw_string(ix + 4, iy + IMG_H - CHAR_H - 3, label, theme::TEXT_SEC, bg);
        }

        if self.wp.discovering {
            let load_y = img_sec_base_y + CHAR_H + 10;
            if load_y + CHAR_H < sh {
                s.draw_string(PAD, load_y, "Discovering wallpapers...", theme::TEXT_MUTED, theme::BG);
            }
        } else if self.wp.images.is_empty() {
            let msg_y = img_sec_base_y + CHAR_H + 10;
            if msg_y + CHAR_H < sh {
                s.draw_string(PAD, msg_y, "No images in /system/wallpapers/", theme::TEXT_MUTED, theme::BG);
            }
        }

        // Apply button – bottom of content area
        let btn_h = 28u32;
        let btn_w = 88u32;
        let btn_y = self.sh - STATUS_H - btn_h - 10;
        let btn_x = self.content_w().saturating_sub(PAD + btn_w);
        s.fill_rect_rounded_aa(btn_x, btn_y, btn_w, btn_h, 6, theme::BTN_PRIMARY);
        let lbl = "Apply";
        let lx = btn_x + (btn_w - lbl.len() as u32 * CHAR_W) / 2;
        s.draw_string(lx, btn_y + (btn_h - CHAR_H) / 2, lbl, theme::BTN_TEXT, theme::BTN_PRIMARY);
    }

    // ── Display Resolution page ───────────────────────────────────────────────

    fn render_display_res(&self, s: &SharedSurface) {
        let by = self.body_y();
        let cw = self.content_w();

        // Page title
        s.draw_string(PAD, by + PAD, "Display Resolution", theme::TEXT, theme::BG);
        s.fill_rect(0, by + PAD + CHAR_H + 4, cw, 1, theme::DIVIDER);

        let (list_top, list_h, list_w, sbx) = self.res_list_geom();

        // Subtitle
        s.draw_string(PAD, by + PAD + CHAR_H + 8, "Available Resolutions", theme::TEXT_MUTED, theme::BG);

        // Mode rows
        for i in 0..VISIBLE_ROWS {
            let idx = self.scroll + i;
            if idx >= self.mode_count { break; }
            let m = self.modes[idx];
            let ry = list_top + i as u32 * ROW_H;
            let sel = idx == self.selected;
            let (row_bg, fg, dim) = if sel {
                (theme::SEL_BG, theme::TEXT, theme::ACCENT)
            } else {
                (theme::BG, theme::TEXT, theme::TEXT_MUTED)
            };
            s.fill_rect(PAD, ry, list_w, ROW_H - 1, row_bg);
            if sel { s.fill_rect(PAD, ry, 3, ROW_H - 1, theme::ACCENT); }
            let mut lbuf = [0u8; 24];
            let label = fmt_resolution(&mut lbuf, m.width, m.height);
            s.draw_string(PAD + 8, ry + (ROW_H - CHAR_H) / 2, label, fg, row_bg);
            let bpp = "32bpp";
            let bx = PAD + list_w - bpp.len() as u32 * CHAR_W - 6;
            s.draw_string(bx, ry + (ROW_H - CHAR_H) / 2, bpp, dim, row_bg);
            if !sel {
                s.fill_rect(PAD + 8, ry + ROW_H - 1, list_w - 16, 1, theme::DIVIDER);
            }
        }

        // Scrollbar
        s.fill_rect(sbx, list_top, SB_W, list_h, theme::SCROLLBAR);
        let (thy, thh, _) = self.scrollbar_geom();
        let thumb_c = if self.sb_drag { theme::ACCENT } else { theme::THUMB };
        s.fill_rect_rounded_aa(sbx + 1, thy, SB_W - 2, thh, 3, thumb_c);

        // Info line
        if self.mode_count > 0 {
            let m = self.modes[self.selected];
            let mut ibuf = [0u8; 44];
            let info = fmt_info(&mut ibuf, m.width, m.height);
            s.draw_string(PAD, list_top + list_h + 6, info, theme::TEXT_MUTED, theme::BG);
        }

        // Buttons
        let btn_h = 28u32;
        let btn_y = self.sh - STATUS_H - btn_h - 10;
        let aw = 88u32;
        let ax = self.content_w().saturating_sub(PAD + aw);
        s.fill_rect_rounded_aa(ax, btn_y, aw, btn_h, 6, theme::BTN_PRIMARY);
        let al = "Apply";
        s.draw_string(ax + (aw - al.len() as u32 * CHAR_W) / 2,
            btn_y + (btn_h - CHAR_H) / 2, al, theme::BTN_TEXT, theme::BTN_PRIMARY);
        let rw = 128u32;
        let rx = ax.saturating_sub(PAD + rw);
        s.fill_rect_rounded_aa(rx, btn_y, rw, btn_h, 6, theme::BTN_RESTORE);
        s.draw_rect_rounded_aa(rx, btn_y, rw, btn_h, 6, theme::BORDER);
        let rl = "Restore Default";
        s.draw_string(rx + (rw - rl.len() as u32 * CHAR_W) / 2,
            btn_y + (btn_h - CHAR_H) / 2, rl, theme::BTN_TEXT, theme::BTN_RESTORE);
    }

    // ── About System page ─────────────────────────────────────────────────────

    fn render_about(&self, s: &SharedSurface) {
        const OS_NAME:    &str = "Atom OS";
        const OS_VER:     &str = "0.2.0";
        const OS_CODE:    &str = "Beryllium";
        const KERNEL_VER: &str = "0.2.0-smp-microkernel";

        let by = self.body_y();
        let cw = self.content_w();

        // Page title
        s.draw_string(PAD, by + PAD, "About System", theme::TEXT, theme::BG);
        s.fill_rect(0, by + PAD + CHAR_H + 4, cw, 1, theme::DIVIDER);

        let label_x = PAD;
        let value_x = PAD + 112;
        let row_h = 20u32;

        let mut y = by + PAD + CHAR_H + 16;

        // Helper: draw a row
        macro_rules! row {
            ($label:expr, $value:expr) => {
                s.draw_string(label_x, y, $label, theme::INFO_LABEL, theme::BG);
                s.draw_string(value_x, y, $value, theme::INFO_VALUE, theme::BG);
                y += row_h;
            };
        }
        macro_rules! divider {
            () => {
                y += 6;
                s.fill_rect(0, y, cw, 1, theme::DIVIDER);
                y += 10;
            };
        }

        // OS section
        row!("OS Name", OS_NAME);
        let mut ver_buf = [0u8; 32];
        let ver_len = build_ver_str(&mut ver_buf, OS_VER, OS_CODE);
        let ver_str = core::str::from_utf8(&ver_buf[..ver_len]).unwrap_or(OS_VER);
        row!("Version", ver_str);
        row!("Kernel", KERNEL_VER);
        row!("Architecture", "x86_64");

        divider!();

        // CPU section
        let brand = if self.about.cpu_brand_len > 0 {
            self.about.cpu_brand_str()
        } else {
            "x86_64 (Atom Core)"
        };
        // Truncate long brand string
        let brand_disp = if brand.len() * CHAR_W as usize > (cw as usize).saturating_sub(value_x as usize + PAD as usize) {
            let max = ((cw as usize).saturating_sub(value_x as usize + PAD as usize)) / CHAR_W as usize;
            if max < brand.len() { &brand[..max] } else { brand }
        } else { brand };
        row!("Processor", brand_disp);
        let mut cores_buf = [0u8; 16];
        let cores_len = write_u64(&mut cores_buf, self.about.cpu_cores);
        let cores_str = core::str::from_utf8(&cores_buf[..cores_len]).unwrap_or("?");
        row!("CPU Cores", cores_str);

        divider!();

        // Memory section
        let mut mem_buf = [0u8; 48];
        let mem_len = build_mem_str(&mut mem_buf, self.about.mem_used_kb, self.about.mem_total_kb);
        let mem_str = core::str::from_utf8(&mem_buf[..mem_len]).unwrap_or("?");
        row!("Memory", mem_str);
        row!("Uptime", self.about.uptime_str());

        divider!();

        // Network section
        row!("IP Address", self.about.ip_str());
    }

    // ── Input handling ────────────────────────────────────────────────────────

    fn handle_key(&mut self, ev: &IpcKeyEvent) {
        if ev.character == 0 {
            match ev.scancode & 0x7F {
                0x48 => { // Up
                    if self.active_page == Page::DisplayResolution && self.selected > 0 {
                        self.selected -= 1; self.clamp_scroll(); self.dirty = true;
                    }
                }
                0x50 => { // Down
                    if self.active_page == Page::DisplayResolution
                        && self.selected + 1 < self.mode_count {
                        self.selected += 1; self.clamp_scroll(); self.dirty = true;
                    }
                }
                0x49 => { // Page Up
                    if self.active_page == Page::DisplayResolution {
                        self.selected = self.selected.saturating_sub(VISIBLE_ROWS);
                        self.clamp_scroll(); self.dirty = true;
                    }
                }
                0x51 => { // Page Down
                    if self.active_page == Page::DisplayResolution {
                        let last = self.mode_count.saturating_sub(1);
                        self.selected = (self.selected + VISIBLE_ROWS).min(last);
                        self.clamp_scroll(); self.dirty = true;
                    }
                }
                _ => {}
            }
            return;
        }
        match ev.character {
            b'\n' | b'\r' => match self.active_page {
                Page::DesktopBackground => self.apply_wallpaper(),
                Page::DisplayResolution => self.apply_resolution(),
                Page::AboutSystem       => {}
            },
            b'r' | b'R' if self.active_page == Page::DisplayResolution => self.restore_default(),
            _ => {}
        }
    }

    fn handle_mouse_down(&mut self, x: i32, y: i32) {
        // Sidebar click?
        if let Some(page) = self.sidebar_page_at(x, y) {
            self.switch_page(page);
            return;
        }

        // Content area
        match self.active_page {
            Page::DesktopBackground => self.handle_wp_click(x, y),
            Page::DisplayResolution => self.handle_res_click(x, y),
            Page::AboutSystem       => {}
        }
    }

    fn handle_wp_click(&mut self, x: i32, y: i32) {
        // Color tiles
        for i in 0..8 {
            let (tx, ty) = self.color_tile_rect(i);
            if x >= tx as i32 && x < (tx + COLOR_SZ) as i32
               && y >= ty as i32 && y < (ty + COLOR_SZ) as i32 {
                self.wp.select_color(i);
                self.dirty = true;
                return;
            }
        }
        // Scaling buttons
        let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch,
                     ScalingMode::Center, ScalingMode::Tile];
        for (i, mode) in modes.iter().enumerate() {
            let (bx, by) = self.scaling_btn_rect(i);
            if x >= bx as i32 && x < (bx + SCALE_BTN_W) as i32
               && y >= by as i32 && y < (by + SCALE_BTN_H) as i32 {
                if self.wp.is_image_selected() {
                    self.wp.scaling = *mode;
                    self.dirty = true;
                }
                return;
            }
        }
        // Image tiles
        let vis_count = 2 * IMG_COLS;
        let start = self.wp_scroll;
        let end = (start + vis_count).min(self.wp.images.len());
        for idx in start..end {
            let vis_i = idx - start;
            let (ix, iy) = self.img_tile_rect(vis_i);
            if x >= ix as i32 && x < (ix + IMG_W) as i32
               && y >= iy as i32 && y < (iy + IMG_H) as i32 {
                self.wp.select_image(idx);
                self.dirty = true;
                return;
            }
        }
        // Apply button
        let btn_h = 28u32; let btn_w = 88u32;
        let btn_y = (self.sh - STATUS_H - btn_h - 10) as i32;
        let btn_x = (self.content_w().saturating_sub(PAD + btn_w)) as i32;
        if x >= btn_x && x < btn_x + btn_w as i32 && y >= btn_y && y < btn_y + btn_h as i32 {
            self.apply_wallpaper();
        }
    }

    fn handle_res_click(&mut self, x: i32, y: i32) {
        let (list_top, list_h, list_w, sbx) = self.res_list_geom();
        let (thy, thh, _) = self.scrollbar_geom();
        let btn_h = 28u32;
        let btn_y = (self.sh - STATUS_H - btn_h - 10) as i32;
        let aw = 88u32;
        let ax = self.content_w().saturating_sub(PAD + aw) as i32;
        let rw = 128u32;
        let rx = (ax as u32).saturating_sub(PAD + rw) as i32;

        // Apply button
        if y >= btn_y && y < btn_y + btn_h as i32 {
            if x >= ax && x < ax + aw as i32 { self.apply_resolution(); return; }
            if x >= rx && x < rx + rw as i32 { self.restore_default(); return; }
        }
        // Scrollbar
        if x >= sbx as i32 && x < (sbx + SB_W) as i32
           && y >= list_top as i32 && y < (list_top + list_h) as i32 {
            if y >= thy as i32 && y < (thy + thh) as i32 {
                self.sb_drag = true;
                self.sb_drag_off = y - thy as i32;
            } else if (y as u32) < thy {
                self.scroll = self.scroll.saturating_sub(VISIBLE_ROWS);
            } else {
                let max_s = self.mode_count.saturating_sub(VISIBLE_ROWS);
                self.scroll = (self.scroll + VISIBLE_ROWS).min(max_s);
            }
            self.dirty = true;
            return;
        }
        // Mode rows
        if x >= PAD as i32 && x < (PAD + list_w) as i32
           && y >= list_top as i32 && y < (list_top + list_h) as i32 {
            let row = ((y - list_top as i32) / ROW_H as i32) as usize;
            let idx = self.scroll + row;
            if idx < self.mode_count {
                self.selected = idx;
                self.clamp_scroll();
                self.dirty = true;
            }
        }
    }

    fn handle_mouse_move(&mut self, y: i32) {
        if self.sb_drag && self.active_page == Page::DisplayResolution {
            self.set_scroll_from_y(y - self.sb_drag_off);
        }
    }

    fn handle_mouse_up(&mut self) {
        if self.sb_drag { self.sb_drag = false; self.dirty = true; }
    }

    fn handle_scroll(&mut self, dz: i32) {
        let steps = dz.unsigned_abs().max(1) as usize;
        match self.active_page {
            Page::DesktopBackground => {
                if dz > 0 {
                    self.wp_scroll = self.wp_scroll.saturating_sub(steps * IMG_COLS);
                } else if dz < 0 {
                    let max = self.wp.images.len().saturating_sub(2 * IMG_COLS);
                    self.wp_scroll = (self.wp_scroll + steps * IMG_COLS).min(max);
                }
            }
            Page::DisplayResolution => {
                if dz > 0 {
                    self.scroll = self.scroll.saturating_sub(steps);
                } else if dz < 0 {
                    let max = self.mode_count.saturating_sub(VISIBLE_ROWS);
                    self.scroll = (self.scroll + steps).min(max);
                }
            }
            Page::AboutSystem => {}
        }
        self.dirty = true;
    }

    // ── Event loop ────────────────────────────────────────────────────────────

    fn process_msg(&mut self, buf: &[u8], len: usize) {
        if len < MessageHeader::SIZE { return; }
        let hdr = match MessageHeader::from_bytes(buf) { Some(h) => h, None => return };
        let payload = &buf[MessageHeader::SIZE..len.min(buf.len())];
        match hdr.msg_type {
            MessageType::TerminateRequest => { self.running = false; }
            MessageType::SurfaceAssign => {
                if let Some(msg) = SurfaceAssignMsg::from_bytes(payload) {
                    if let Ok(s) = SharedSurface::from_region(msg.region_id, msg.width, msg.height) {
                        self.sw = msg.width; self.sh = msg.height;
                        self.surface = Some(s); self.dirty = true;
                    }
                }
            }
            MessageType::KeyPress => {
                if let Some(ev) = IpcKeyEvent::from_bytes(payload) { self.handle_key(&ev); }
            }
            MessageType::MouseButtonDown => {
                if let Some(ev) = MouseButtonEvent::from_bytes(payload) {
                    if ev.button == MouseButton::Left { self.handle_mouse_down(ev.x, ev.y); }
                }
            }
            MessageType::MouseMove => {
                if let Some(ev) = MouseMoveEvent::from_bytes(payload) { self.handle_mouse_move(ev.y); }
            }
            MessageType::MouseButtonUp => {
                if let Some(ev) = MouseButtonEvent::from_bytes(payload) {
                    if ev.button == MouseButton::Left { self.handle_mouse_up(); }
                }
            }
            MessageType::MouseScroll => {
                if let Some(ev) = MouseScrollEvent::from_bytes(payload) { self.handle_scroll(ev.dz); }
            }
            MessageType::OpenInTab => {
                if let Some(msg) = OpenInTabMsg::from_bytes(payload) { self.handle_open_in_tab(&msg.tab_name); }
            }
            MessageType::WallpaperApplied => {
                if WallpaperAppliedMsg::from_bytes(payload).is_some() {
                    self.status.set(StatusKind::Ok, b"Background applied successfully.");
                    self.dirty = true;
                }
            }
            MessageType::WallpaperFailed => {
                if let Some(msg) = WallpaperFailedMsg::from_bytes(payload) {
                    let mut e = String::from("Failed: ");
                    e.push_str(&msg.error_message);
                    self.status.set(StatusKind::Warn, e.as_bytes());
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn run(&mut self) {
        self.render();
        let mut buf = [0u8; 128];
        let ports = [self.local_port];
        while self.running {
            while let Ok(Some(len)) = try_recv(self.local_port, &mut buf) {
                self.process_msg(&buf, len);
            }
            self.status.tick();
            if self.dirty { self.render(); }
            let _ = wait_any(&ports, 16);
        }
    }

    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buf = [0u8; 128];
        let ports = [port];
        for _ in 0..200 {
            if wait_any(&ports, 50).is_ok() {
                if let Ok(Some(len)) = try_recv(port, &mut buf) {
                    if len >= MessageHeader::SIZE {
                        if let Some(hdr) = MessageHeader::from_bytes(&buf) {
                            if hdr.msg_type == MessageType::SurfaceAssign {
                                if let Some(msg) = SurfaceAssignMsg::from_bytes(
                                    &buf[MessageHeader::SIZE..]) { return Some(msg); }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn query_modes() -> ([Mode; VIDEO_MAX_MODES], usize) {
    let count = video_mode_count().min(VIDEO_MAX_MODES);
    if count == 0 { return ([Mode { width: 0, height: 0 }; VIDEO_MAX_MODES], 0); }
    let mut raw = [VideoModeEntry::default(); VIDEO_MAX_MODES];
    let written = get_video_modes(&mut raw[..count]);
    let mut modes = [Mode { width: 0, height: 0 }; VIDEO_MAX_MODES];
    for i in 0..written { modes[i] = Mode { width: raw[i].width as u16, height: raw[i].height as u16 }; }
    (modes, written)
}

fn default_idx(modes: &[Mode], count: usize) -> usize {
    for i in 0..count {
        if modes[i].width == DEFAULT_W && modes[i].height == DEFAULT_H { return i; }
    }
    0
}

fn write_u64(buf: &mut [u8], mut n: u64) -> usize {
    if n == 0 { if !buf.is_empty() { buf[0] = b'0'; } return 1; }
    let mut tmp = [0u8; 20]; let mut i = 0usize;
    while n > 0 { tmp[i] = b'0' + (n % 10) as u8; i += 1; n /= 10; }
    if i > buf.len() { return 0; }
    for j in 0..i { buf[j] = tmp[i - 1 - j]; }
    i
}

fn fmt_resolution<'a>(buf: &'a mut [u8; 24], w: u16, h: u16) -> &'a str {
    let mut p = 0usize;
    p += write_u64(&mut buf[p..], w as u64);
    buf[p..p+3].copy_from_slice(b" x "); p += 3;
    p += write_u64(&mut buf[p..], h as u64);
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

fn fmt_info<'a>(buf: &'a mut [u8; 44], w: u16, h: u16) -> &'a str {
    let pre = b"Selected: "; buf[..pre.len()].copy_from_slice(pre); let mut p = pre.len();
    p += write_u64(&mut buf[p..], w as u64);
    buf[p..p+3].copy_from_slice(b" x "); p += 3;
    p += write_u64(&mut buf[p..], h as u64);
    let suf = b" @ 32bpp  60Hz"; buf[p..p+suf.len()].copy_from_slice(suf); p += suf.len();
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

fn build_ver_str(buf: &mut [u8; 32], ver: &str, code: &str) -> usize {
    let mut p = 0usize;
    for b in ver.bytes() { if p < 31 { buf[p] = b; p += 1; } }
    let sep = b" (";
    for b in sep { if p < 31 { buf[p] = *b; p += 1; } }
    for b in code.bytes() { if p < 31 { buf[p] = b; p += 1; } }
    if p < 31 { buf[p] = b')'; p += 1; }
    p
}

fn build_mem_str(buf: &mut [u8; 48], used_kb: u64, total_kb: u64) -> usize {
    let mut p = 0usize;
    p += write_u64(&mut buf[p..], used_kb / 1024);
    let mid = b" MB / "; for b in mid { buf[p] = *b; p += 1; }
    p += write_u64(&mut buf[p..], total_kb / 1024);
    let suf = b" MB"; for b in suf { buf[p] = *b; p += 1; }
    p
}

fn fit_within(sw: u32, sh: u32, mw: u32, mh: u32) -> (u32, u32) {
    if sw == 0 || sh == 0 || mw == 0 || mh == 0 { return (0, 0); }
    let scale_w = (mw as u64 * 1024) / sw as u64;
    let scale_h = (mh as u64 * 1024) / sh as u64;
    let scale = scale_w.min(scale_h).max(1);
    (((sw as u64 * scale) / 1024).max(1) as u32, ((sh as u64 * scale) / 1024).max(1) as u32)
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

    log("SystemSettings: looking up compositor.register");
    let reg_port = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(p) => break p,
            Err(_) => yield_now(),
        }
    };

    let mut rmsg = [0u8; MessageHeader::SIZE + 16];
    let hdr = MessageHeader::new(MessageType::AppRegister, 16);
    rmsg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    rmsg[MessageHeader::SIZE..MessageHeader::SIZE + 8].copy_from_slice(&port.to_le_bytes());
    rmsg[MessageHeader::SIZE + 8..MessageHeader::SIZE + 16].copy_from_slice(&0u64.to_le_bytes());
    let _ = send(reg_port, &rmsg[..MessageHeader::SIZE + 16]);

    let sa = match SystemSettings::wait_for_surface(port) {
        Some(sa) => sa,
        None => { log("SystemSettings: surface timeout"); exit(1); }
    };

    let surface = match SharedSurface::from_region(sa.region_id, sa.width, sa.height) {
        Ok(s) => s,
        Err(_) => { log("SystemSettings: surface map failed"); exit(1); }
    };

    let (modes, mode_count) = query_modes();
    let mut app = SystemSettings::new(sa.window_id, sa.compositor_port, port,
                                      surface, modes, mode_count);
    app.run();
    exit(0);
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! { log("SystemSettings: PANIC"); exit(0xFF); }
