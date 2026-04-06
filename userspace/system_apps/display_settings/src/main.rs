// Atom OS – Settings (Centralized)
//
// A modern, centralized settings application following the Atom Design System.
// Categories: Monitors, Desktop, About.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::panic::PanicInfo;

use atom_syscall::graphics::{SharedSurface, VideoModeEntry, get_video_modes, video_mode_count,
                              VIDEO_MAX_MODES, Color, set_video_mode};
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::thread::{exit, yield_now, get_ticks};
use atom_syscall::debug::log;
use atom_syscall::fs::{open, readdir, close, OpenFlags, FileType};

use libipc::messages::{
    MessageType, MessageHeader,
    SurfaceAssignMsg,
    KeyEvent as IpcKeyEvent,
    MouseButtonEvent,
    MouseMoveEvent,
    MouseScrollEvent,
    MouseButton,
    ScalingMode,
    ApplyWallpaperMsg,
    WallpaperSourceType,
    WallpaperAppliedMsg,
    WallpaperFailedMsg,
};

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use atom_theme::colors as ds;
use atom_theme::spacing;
use atom_theme::radius;

// ============================================================================
// Heap
// ============================================================================

const HEAP_SIZE: usize = 1024 * 1024; // 1 MB

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
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! { loop {} }

// ============================================================================
// UI Constants
// ============================================================================

const SIDEBAR_W: u32 = 180;
const TOOLBAR_H: u32 = 48;
const CHAR_W:    u32 = 8;
const CHAR_H:    u32 = 8;

const VISIBLE_MODES: usize = 10;
const MODE_ROW_H: u32 = 28;

const IMAGE_TILE_W: u32 = 140;
const IMAGE_TILE_H: u32 = 120;
const IMAGE_TILE_SPACING: u32 = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Monitors,
    Desktop,
    About,
}

impl Category {
    fn label(self) -> &'static str {
        match self {
            Self::Monitors => "Monitors",
            Self::Desktop  => "Desktop",
            Self::About    => "About",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Monitors => "M",
            Self::Desktop  => "D",
            Self::About    => "i",
        }
    }
}

const CATEGORIES: &[Category] = &[
    Category::Monitors,
    Category::Desktop,
    Category::About,
];

// ============================================================================
// State Structures
// ============================================================================

#[derive(Clone, Copy)]
struct Mode { width: u16, height: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThumbnailState {
    Unloaded,
    Loaded { width: u16, height: u16 },
    Failed,
}

struct WallpaperInfo {
    name: String,
    path: String,
    thumbnail: ThumbnailState,
}

struct SettingsApp {
    window_id:        u32,
    compositor_port:  PortId,
    local_port:       PortId,
    surface:          Option<SharedSurface>,
    width:            u32,
    height:           u32,

    active_category:  Category,
    mouse_x:          i32,
    mouse_y:          i32,
    
    // Monitors state
    modes:            [Mode; VIDEO_MAX_MODES],
    mode_count:       usize,
    selected_mode:    usize,
    mode_scroll:      usize,
    
    // Desktop state
    solid_colors:     [Color; 8],
    discovered_images: Vec<WallpaperInfo>,
    selected_source_idx: Option<usize>, // Index into solid_colors or discovered_images
    is_image_selected: bool,
    wallpaper_scroll: usize,
    selected_scaling: ScalingMode,
    
    // About state (cached info)
    cpu_info:         String,
    mem_info:         String,
    storage_info:     String,

    status_msg:       String,
    status_ticks:     u32,
    running:          bool,
    needs_redraw:     bool,
}

impl SettingsApp {
    fn new(window_id: u32, compositor_port: PortId, local_port: PortId, surface: SharedSurface,
           modes: [Mode; VIDEO_MAX_MODES], mode_count: usize) -> Self {
        let w = surface.width();
        let h = surface.height();
        
        let mut app = Self {
            window_id, compositor_port, local_port,
            surface: Some(surface),
            width: w, height: h,
            active_category: Category::Monitors,
            mouse_x: 0, mouse_y: 0,
            modes,
            mode_count,
            selected_mode: 0,
            mode_scroll: 0,
            solid_colors: [
                Color::new(18, 20, 28),
                Color::new(12, 14, 22),
                Color::new(24, 28, 42),
                Color::new(16, 24, 40),
                Color::new(22, 36, 52),
                Color::new(28, 18, 36),
                Color::new(20, 30, 24),
                Color::new(34, 20, 20),
            ],
            discovered_images: Vec::new(),
            selected_source_idx: None,
            is_image_selected: false,
            wallpaper_scroll: 0,
            selected_scaling: ScalingMode::Fill,
            cpu_info: String::from("Atom x86_64 @ 2.4GHz"),
            mem_info: String::from("2048 MB RAM"),
            storage_info: String::from("System: 120MB / 512MB"),
            status_msg: String::from("Ready"),
            status_ticks: 0,
            running: true,
            needs_redraw: true,
        };
        
        let _ = app.discover_images();
        app
    }

    fn discover_images(&mut self) -> Result<(), &'static str> {
        self.discovered_images.clear();
        let fd = match open("/system/wallpapers/", OpenFlags::DIRECTORY, 0) {
            Ok(fd) => fd,
            Err(_) => return Err("Directory not found"),
        };

        let mut buf = [0u8; 1024];
        while let Ok(count) = readdir(fd, &mut buf) {
            if count == 0 { break; }
            let mut offset = 0;
            while offset < count {
                let entry_type = buf[offset];
                let name_ptr = &buf[offset + 1..];
                let name_len = name_ptr.iter().position(|&b| b == 0).unwrap_or(0);
                let name = core::str::from_utf8(&name_ptr[..name_len]).unwrap_or("");
                
                if entry_type == FileType::Regular as u8 {
                    let lower = name.to_lowercase();
                    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
                        self.discovered_images.push(WallpaperInfo {
                            name: String::from(name),
                            path: format!("/system/wallpapers/{}", name),
                            thumbnail: ThumbnailState::Unloaded,
                        });
                    }
                }
                offset += 1 + name_len + 1;
            }
        }
        let _ = close(fd);
        Ok(())
    }

    fn run(&mut self) {
        let mut buf = [0u8; 1024];
        let ports = [self.local_port];
        
        while self.running {
            if self.needs_redraw {
                self.render();
            }
            
            // Block until message or timeout (16ms for ~60fps ticks)
            if let Ok(Some(len)) = try_recv(self.local_port, &mut buf) {
                self.process_message(&buf, len);
            } else {
                let _ = wait_any(&ports, 16);
            }
            
            if self.status_ticks > 0 {
                self.status_ticks -= 1;
                if self.status_ticks == 0 {
                    self.status_msg = String::from("Ready");
                    self.needs_redraw = true;
                }
            }
        }
    }

    fn process_message(&mut self, buf: &[u8], len: usize) {
        let hdr = match MessageHeader::from_bytes(&buf[..len]) {
            Some(h) => h,
            None => return,
        };
        let payload = &buf[MessageHeader::SIZE..len];

        match hdr.msg_type {
            MessageType::MouseMove => {
                if let Some(ev) = MouseMoveEvent::from_bytes(payload) {
                    self.mouse_x = ev.x;
                    self.mouse_y = ev.y;
                }
            }
            MessageType::MouseButtonDown => {
                if let Some(ev) = MouseButtonEvent::from_bytes(payload) {
                    if ev.button == MouseButton::Left {
                        self.handle_click();
                    }
                }
            }
            MessageType::MouseScroll => {
                if let Some(ev) = MouseScrollEvent::from_bytes(payload) {
                    self.handle_scroll(ev.dz);
                }
            }
            MessageType::KeyPress => {
                if let Some(ev) = IpcKeyEvent::from_bytes(payload) {
                    self.handle_key(ev);
                }
            }
            MessageType::WallpaperApplied => {
                self.status_msg = String::from("Wallpaper applied");
                self.status_ticks = 120;
                self.needs_redraw = true;
            }
            MessageType::WallpaperFailed => {
                if let Some(msg) = WallpaperFailedMsg::from_bytes(payload) {
                    self.status_msg = format!("Error: {}", msg.error_message);
                    self.status_ticks = 180;
                    self.needs_redraw = true;
                }
            }
            _ => {}
        }
    }

    fn render(&mut self) {
        self.needs_redraw = false;
        let surface = self.surface.as_ref().unwrap();

        // Background
        surface.fill_rect(0, 0, self.width, self.height, ds::ATOM_COLOR_BG);

        // Sidebar
        self.draw_sidebar(surface);

        // Toolbar / Header
        self.draw_header(surface);

        // Content Area
        self.draw_content(surface);

        // Present
        let mut msg = [0u8; MessageHeader::SIZE + 8];
        let hdr = MessageHeader::new(MessageType::SurfacePresent, 8);
        msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        msg[MessageHeader::SIZE..MessageHeader::SIZE + 8].copy_from_slice(&self.window_id.to_le_bytes());
        let _ = send(self.compositor_port, &msg);
    }

    fn draw_sidebar(&self, surface: &SharedSurface) {
        surface.fill_rect(0, 0, SIDEBAR_W, self.height, ds::ATOM_COLOR_SURFACE);
        surface.fill_rect(SIDEBAR_W - 1, 0, 1, self.height, ds::ATOM_COLOR_BORDER);

        let mut y = TOOLBAR_H + spacing::MD;
        for category in CATEGORIES {
            let is_sel = self.active_category == *category;
            let bg = if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_SURFACE };
            let fg = if is_sel { ds::ATOM_COLOR_BG } else { ds::ATOM_COLOR_TEXT_PRIMARY };

            if is_sel {
                surface.fill_rect_rounded_aa(spacing::SM, y, SIDEBAR_W - spacing::LG, 32, radius::SM, bg);
            }

            surface.draw_string(spacing::LG, y + 12, category.icon(), fg, bg);
            surface.draw_string(spacing::LG + 20, y + 12, category.label(), fg, bg);
            y += 40;
        }
    }

    fn draw_header(&self, surface: &SharedSurface) {
        surface.fill_rect(0, 0, self.width, TOOLBAR_H, ds::ATOM_COLOR_SURFACE_ALT);
        surface.fill_rect(0, TOOLBAR_H - 1, self.width, 1, ds::ATOM_COLOR_BORDER);

        let title = self.active_category.label();
        surface.draw_string(SIDEBAR_W + spacing::MD, 20, title, ds::ATOM_COLOR_TEXT_PRIMARY, ds::ATOM_COLOR_SURFACE_ALT);
        
        // Status message on the right
        let sx = self.width - (self.status_msg.len() as u32 * CHAR_W) - spacing::MD;
        surface.draw_string(sx, 20, &self.status_msg, ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_SURFACE_ALT);
    }

    fn draw_content(&self, surface: &SharedSurface) {
        let cx = SIDEBAR_W + spacing::LG;
        let cy = TOOLBAR_H + spacing::LG;

        match self.active_category {
            Category::Monitors => self.draw_monitors(surface, cx, cy),
            Category::Desktop  => self.draw_desktop(surface, cx, cy),
            Category::About    => self.draw_about(surface, cx, cy),
        }
    }

    fn draw_monitors(&self, surface: &SharedSurface, cx: u32, cy: u32) {
        surface.draw_string(cx, cy, "Available Resolutions:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        
        let mut y = cy + 30;
        let end = (self.mode_scroll + VISIBLE_MODES).min(self.mode_count);
        for i in self.mode_scroll..end {
            let mode = self.modes[i];
            let is_sel = self.selected_mode == i;
            let bg = if is_sel { ds::ATOM_COLOR_SURFACE_ALT } else { ds::ATOM_COLOR_BG };
            let fg = if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_TEXT_PRIMARY };
            
            if is_sel {
                surface.fill_rect_rounded_aa(cx - 4, y - 4, 200, 24, radius::XS, bg);
            }
            
            let label = format!("{} x {}", mode.width, mode.height);
            surface.draw_string(cx, y, &label, fg, bg);
            y += MODE_ROW_H;
        }

        // Apply button
        let btn_y = self.height - 60;
        surface.fill_rect_rounded_aa(cx, btn_y, 120, 32, radius::SM, ds::ATOM_COLOR_ACCENT);
        surface.draw_string(cx + 35, btn_y + 12, "Apply", ds::ATOM_COLOR_BG, ds::ATOM_COLOR_ACCENT);
        
        // Restore button
        surface.fill_rect_rounded_aa(cx + 140, btn_y, 120, 32, radius::SM, ds::ATOM_COLOR_SURFACE_ALT);
        surface.draw_string(cx + 165, btn_y + 12, "Restore", ds::ATOM_COLOR_TEXT_PRIMARY, ds::ATOM_COLOR_SURFACE_ALT);
    }

    fn draw_desktop(&self, surface: &SharedSurface, cx: u32, cy: u32) {
        surface.draw_string(cx, cy, "Solid Colors:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        
        let mut x = cx;
        let mut y = cy + 30;
        for (i, color) in self.solid_colors.iter().enumerate() {
            let is_sel = !self.is_image_selected && self.selected_source_idx == Some(i);
            if is_sel {
                surface.draw_rect_rounded_aa(x - 2, y - 2, 44, 44, radius::XS, ds::ATOM_COLOR_ACCENT);
            }
            surface.fill_rect_rounded_aa(x, y, 40, 40, radius::XS, *color);
            
            x += 50;
            if (i + 1) % 4 == 0 {
                x = cx;
                y += 50;
            }
        }

        let img_y = y + 20;
        surface.draw_string(cx, img_y, "Wallpaper Images:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        
        let mut ix = cx;
        let mut iy = img_y + 25;
        let visible_count = 4; // 2x2 grid visible
        let end = (self.wallpaper_scroll + visible_count).min(self.discovered_images.len());
        
        if self.discovered_images.is_empty() {
            surface.fill_rect_rounded_aa(cx, iy, 300, 150, radius::MD, ds::ATOM_COLOR_SURFACE_ALT);
            surface.draw_string(cx + 80, iy + 70, "No images found", ds::ATOM_COLOR_TEXT_MUTED, ds::ATOM_COLOR_SURFACE_ALT);
        } else {
            for i in self.wallpaper_scroll..end {
                let info = &self.discovered_images[i];
                let is_sel = self.is_image_selected && self.selected_source_idx == Some(i);
                
                let bg = if is_sel { ds::ATOM_COLOR_SURFACE_ALT } else { ds::ATOM_COLOR_BG };
                surface.fill_rect_rounded_aa(ix, iy, IMAGE_TILE_W, IMAGE_TILE_H, radius::MD, bg);
                surface.draw_rect_rounded_aa(ix, iy, IMAGE_TILE_W, IMAGE_TILE_H, radius::MD, if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_BORDER });
                
                // Preview placeholder
                surface.fill_rect_rounded_aa(ix + 10, iy + 10, IMAGE_TILE_W - 20, 70, radius::SM, Color::new(40, 45, 60));
                
                let name = if info.name.len() > 15 { format!("{}..", &info.name[..13]) } else { info.name.clone() };
                surface.draw_string(ix + 10, iy + 90, &name, ds::ATOM_COLOR_TEXT_PRIMARY, bg);
                
                ix += IMAGE_TILE_W + IMAGE_TILE_SPACING;
                if (i - self.wallpaper_scroll + 1) % 2 == 0 {
                    ix = cx;
                    iy += IMAGE_TILE_H + IMAGE_TILE_SPACING;
                }
            }
        }

        // Scaling modes (only if image selected)
        if self.is_image_selected {
            let sx = cx + 320;
            let mut sy = img_y + 25;
            surface.draw_string(sx, sy - 20, "Scaling:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
            
            let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch, ScalingMode::Center, ScalingMode::Tile];
            for mode in modes {
                let is_sel = self.selected_scaling == mode;
                let bg = if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_SURFACE_ALT };
                let fg = if is_sel { ds::ATOM_COLOR_BG } else { ds::ATOM_COLOR_TEXT_PRIMARY };
                
                surface.fill_rect_rounded_aa(sx, sy, 80, 24, radius::XS, bg);
                surface.draw_string(sx + 10, sy + 8, mode.to_str(), fg, bg);
                sy += 30;
            }
        }
        
        // Apply button
        let btn_y = self.height - 60;
        surface.fill_rect_rounded_aa(cx, btn_y, 120, 32, radius::SM, ds::ATOM_COLOR_ACCENT);
        surface.draw_string(cx + 35, btn_y + 12, "Apply", ds::ATOM_COLOR_BG, ds::ATOM_COLOR_ACCENT);
    }

    fn draw_about(&self, surface: &SharedSurface, cx: u32, cy: u32) {
        surface.fill_rect_rounded_aa(cx, cy, 64, 64, radius::MD, ds::ATOM_COLOR_ACCENT);
        surface.draw_string(cx + 24, cy + 28, "A", ds::ATOM_COLOR_BG, ds::ATOM_COLOR_ACCENT);

        surface.draw_string(cx + 80, cy + 10, "Atom OS", ds::ATOM_COLOR_TEXT_PRIMARY, ds::ATOM_COLOR_BG);
        surface.draw_string(cx + 80, cy + 30, "Version 1.0 Luminous", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);

        let mut y = cy + 100;
        self.draw_info_row(surface, cx, y, "Processor:", &self.cpu_info);
        y += 40;
        self.draw_info_row(surface, cx, y, "Memory:", &self.mem_info);
        y += 40;
        self.draw_info_row(surface, cx, y, "Storage:", &self.storage_info);
        y += 60;
        
        surface.draw_string(cx, y, "© 2026 Atom Project Contributors", ds::ATOM_COLOR_TEXT_MUTED, ds::ATOM_COLOR_BG);
    }

    fn draw_info_row(&self, surface: &SharedSurface, x: u32, y: u32, label: &str, value: &str) {
        surface.draw_string(x, y, label, ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        surface.draw_string(x + 120, y, value, ds::ATOM_COLOR_TEXT_PRIMARY, ds::ATOM_COLOR_BG);
    }

    fn handle_click(&mut self) {
        if self.mouse_x < SIDEBAR_W as i32 {
            let mut y = TOOLBAR_H + spacing::MD;
            for category in CATEGORIES {
                if self.mouse_y >= y as i32 && self.mouse_y < (y + 40) as i32 {
                    self.active_category = *category;
                    self.needs_redraw = true;
                    break;
                }
                y += 40;
            }
        } else {
            let cx = SIDEBAR_W + spacing::LG;
            let cy = TOOLBAR_H + spacing::LG;
            
            match self.active_category {
                Category::Monitors => {
                    let mut y = cy + 30;
                    let end = (self.mode_scroll + VISIBLE_MODES).min(self.mode_count);
                    for i in self.mode_scroll..end {
                        if self.mouse_y >= (y - 4) as i32 && self.mouse_y < (y + 20) as i32 {
                            self.selected_mode = i;
                            self.needs_redraw = true;
                        }
                        y += MODE_ROW_H;
                    }
                    
                    let btn_y = self.height - 60;
                    if self.mouse_y >= btn_y as i32 && self.mouse_y < (btn_y + 32) as i32 {
                        if self.mouse_x >= cx as i32 && self.mouse_x < (cx + 120) as i32 {
                            self.apply_resolution();
                        } else if self.mouse_x >= (cx + 140) as i32 && self.mouse_x < (cx + 260) as i32 {
                            self.restore_default();
                        }
                    }
                }
                Category::Desktop => {
                    // Solid colors
                    let mut x = cx;
                    let mut y = cy + 30;
                    for i in 0..self.solid_colors.len() {
                        if self.mouse_x >= x as i32 && self.mouse_x < (x + 40) as i32 &&
                           self.mouse_y >= y as i32 && self.mouse_y < (y + 40) as i32 {
                            self.selected_source_idx = Some(i);
                            self.is_image_selected = false;
                            self.needs_redraw = true;
                        }
                        x += 50;
                        if (i + 1) % 4 == 0 { x = cx; y += 50; }
                    }
                    
                    // Images
                    let img_y = y + 20;
                    let mut ix = cx;
                    let mut iy = img_y + 25;
                    let visible_count = 4;
                    let end = (self.wallpaper_scroll + visible_count).min(self.discovered_images.len());
                    for i in self.wallpaper_scroll..end {
                        if self.mouse_x >= ix as i32 && self.mouse_x < (ix + IMAGE_TILE_W) as i32 &&
                           self.mouse_y >= iy as i32 && self.mouse_y < (iy + IMAGE_TILE_H) as i32 {
                            self.selected_source_idx = Some(i);
                            self.is_image_selected = true;
                            self.needs_redraw = true;
                        }
                        ix += IMAGE_TILE_W + IMAGE_TILE_SPACING;
                        if (i - self.wallpaper_scroll + 1) % 2 == 0 { ix = cx; iy += IMAGE_TILE_H + IMAGE_TILE_SPACING; }
                    }
                    
                    // Scaling
                    if self.is_image_selected {
                        let sx = cx + 320;
                        let mut sy = img_y + 25;
                        let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch, ScalingMode::Center, ScalingMode::Tile];
                        for mode in modes {
                            if self.mouse_x >= sx as i32 && self.mouse_x < (sx + 80) as i32 &&
                               self.mouse_y >= sy as i32 && self.mouse_y < (sy + 24) as i32 {
                                self.selected_scaling = mode;
                                self.needs_redraw = true;
                            }
                            sy += 30;
                        }
                    }
                    
                    // Apply
                    let btn_y = self.height - 60;
                    if self.mouse_x >= cx as i32 && self.mouse_x < (cx + 120) as i32 &&
                       self.mouse_y >= btn_y as i32 && self.mouse_y < (btn_y + 32) as i32 {
                        self.apply_wallpaper();
                    }
                }
                Category::About => {}
            }
        }
    }

    fn handle_scroll(&mut self, dz: i32) {
        match self.active_category {
            Category::Monitors => {
                if dz > 0 { self.mode_scroll = self.mode_scroll.saturating_sub(1); }
                else { self.mode_scroll = (self.mode_scroll + 1).min(self.mode_count.saturating_sub(VISIBLE_MODES)); }
            }
            Category::Desktop => {
                if dz > 0 { self.wallpaper_scroll = self.wallpaper_scroll.saturating_sub(2); }
                else { self.wallpaper_scroll = (self.wallpaper_scroll + 2).min(self.discovered_images.len().saturating_sub(4)); }
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    fn handle_key(&mut self, ev: IpcKeyEvent) {
        if ev.character == 27 { self.running = false; return; }
        
        match self.active_category {
            Category::Monitors => {
                if ev.character == 0 {
                    match ev.scancode & 0x7F {
                        0x48 => { // Up
                            if self.selected_mode > 0 { self.selected_mode -= 1; }
                            self.clamp_mode_scroll();
                        }
                        0x50 => { // Down
                            if self.selected_mode + 1 < self.mode_count { self.selected_mode += 1; }
                            self.clamp_mode_scroll();
                        }
                        _ => {}
                    }
                } else if ev.character == b'\n' as u32 || ev.character == b'\r' as u32 {
                    self.apply_resolution();
                } else if ev.character == b'r' as u32 || ev.character == b'R' as u32 {
                    self.restore_default();
                }
            }
            Category::Desktop => {
                if ev.character == b'\n' as u32 || ev.character == b'\r' as u32 {
                    self.apply_wallpaper();
                }
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    fn clamp_mode_scroll(&mut self) {
        if self.selected_mode < self.mode_scroll { self.mode_scroll = self.selected_mode; }
        if self.selected_mode >= self.mode_scroll + VISIBLE_MODES {
            self.mode_scroll = self.selected_mode + 1 - VISIBLE_MODES;
        }
    }

    fn apply_resolution(&mut self) {
        let mode = self.modes[self.selected_mode];
        log(&format!("Settings: Applying resolution {}x{}", mode.width, mode.height));
        
        if let Err(_) = set_video_mode(mode.width as u32, mode.height as u32, 32) {
            self.status_msg = String::from("Failed to set mode");
        } else {
            // Notify compositor
            let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
            let _ = send(self.compositor_port, &hdr.to_bytes());
            
            self.status_msg = format!("Applied {}x{}", mode.width, mode.height);
        }
        self.status_ticks = 120;
        self.needs_redraw = true;
    }

    fn restore_default(&mut self) {
        let (w, h) = (1024, 768);
        if let Err(_) = set_video_mode(w, h, 32) {
            self.status_msg = String::from("Failed to restore");
        } else {
            let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
            let _ = send(self.compositor_port, &hdr.to_bytes());
            self.status_msg = String::from("Restored 1024x768");
        }
        self.status_ticks = 120;
        self.needs_redraw = true;
    }

    fn apply_wallpaper(&mut self) {
        let idx = match self.selected_source_idx {
            Some(i) => i,
            None => { self.status_msg = String::from("Select a wallpaper"); self.status_ticks = 120; return; }
        };

        let wallpaper_msg = if self.is_image_selected {
            ApplyWallpaperMsg {
                source_type: WallpaperSourceType::Image,
                image_path: Some(self.discovered_images[idx].path.clone()),
                color_rgb: None,
                scaling_mode: self.selected_scaling,
            }
        } else {
            let color = self.solid_colors[idx];
            let rgb = ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32);
            ApplyWallpaperMsg {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(rgb),
                scaling_mode: ScalingMode::Fill,
            }
        };
        
        let payload = wallpaper_msg.to_bytes();
        let mut msg = Vec::with_capacity(MessageHeader::SIZE + payload.len());
        let hdr = MessageHeader::new(MessageType::ApplyWallpaper, payload.len() as u32);
        msg.extend_from_slice(&hdr.to_bytes());
        msg.extend_from_slice(&payload);
        
        let _ = send(self.compositor_port, &msg);
        self.status_msg = String::from("Applying...");
        self.status_ticks = 120;
        self.needs_redraw = true;
    }

    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buf = [0u8; 1024];
        let ports = [port];
        for _ in 0..100 {
            if wait_any(&ports, 50).is_ok() {
                if let Ok(Some(len)) = try_recv(port, &mut buf) {
                    let hdr = MessageHeader::from_bytes(&buf[..len]).unwrap();
                    if hdr.msg_type == MessageType::SurfaceAssign {
                        return SurfaceAssignMsg::from_bytes(&buf[MessageHeader::SIZE..len]);
                    }
                }
            }
        }
        None
    }
}

fn query_modes() -> ([Mode; VIDEO_MAX_MODES], usize) {
    let count = video_mode_count().min(VIDEO_MAX_MODES);
    if count == 0 {
        return ([Mode { width: 1024, height: 768 }; VIDEO_MAX_MODES], 1);
    }

    let mut raw = [VideoModeEntry::default(); VIDEO_MAX_MODES];
    let written = get_video_modes(&mut raw[..count]);

    let mut modes = [Mode { width: 0, height: 0 }; VIDEO_MAX_MODES];
    for i in 0..written {
        modes[i] = Mode { width: raw[i].width as u16, height: raw[i].height as u16 };
    }
    (modes, written)
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let port = match create_port() {
        Ok(p) => p,
        Err(_) => exit(1),
    };
    
    let _ = libipc::protocol::register_service("display_settings", port);
    
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
    let _ = send(reg_port, &rmsg);

    let sa = match SettingsApp::wait_for_surface(port) {
        Some(sa) => sa,
        None => exit(1),
    };

    let surface = match SharedSurface::from_region(sa.region_id, sa.width, sa.height) {
        Ok(s) => s,
        Err(_) => exit(1),
    };

    let (modes, mode_count) = query_modes();
    let mut app = SettingsApp::new(sa.window_id, sa.compositor_port, port, surface, modes, mode_count);
    app.run();
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! { exit(0xFF); }
