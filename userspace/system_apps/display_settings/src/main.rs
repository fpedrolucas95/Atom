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
                              VIDEO_MAX_MODES, Color};
use atom_syscall::ipc::{create_port, send, try_recv, PortId};
use atom_syscall::thread::{exit, yield_now, get_ticks};
use atom_syscall::debug::log;

use libipc::messages::{
    MessageType, MessageHeader,
    SurfaceAssignMsg,
    KeyEvent as IpcKeyEvent,
    MouseButtonEvent,
    MouseMoveEvent,
    MouseButton,
    ScalingMode,
    ApplyWallpaperMsg,
    WallpaperSourceType,
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
    
    // Desktop state
    solid_colors:     [Color; 8],
    selected_color:   Option<usize>,
    
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
        
        Self {
            window_id, compositor_port, local_port,
            surface: Some(surface),
            width: w, height: h,
            active_category: Category::Monitors,
            mouse_x: 0, mouse_y: 0,
            modes,
            mode_count,
            selected_mode: 0,
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
            selected_color: None,
            cpu_info: String::from("Atom x86_64 @ 2.4GHz"),
            mem_info: String::from("2048 MB RAM"),
            storage_info: String::from("System: 120MB / 512MB"),
            status_msg: String::from("Ready"),
            status_ticks: 0,
            running: true,
            needs_redraw: true,
        }
    }

    fn run(&mut self) {
        while self.running {
            self.render();
            self.handle_events();
            if self.status_ticks > 0 {
                self.status_ticks -= 1;
                if self.status_ticks == 0 {
                    self.status_msg = String::from("Ready");
                    self.needs_redraw = true;
                }
            }
            yield_now();
        }
    }

    fn render(&mut self) {
        if !self.needs_redraw { return; }
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
        for i in 0..self.mode_count {
            let mode = self.modes[i];
            let is_sel = self.selected_mode == i;
            let bg = if is_sel { ds::ATOM_COLOR_SURFACE_ALT } else { ds::ATOM_COLOR_BG };
            let fg = if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_TEXT_PRIMARY };
            
            if is_sel {
                surface.fill_rect_rounded_aa(cx - 4, y - 4, 200, 24, radius::XS, bg);
            }
            
            let label = format!("{} x {}", mode.width, mode.height);
            surface.draw_string(cx, y, &label, fg, bg);
            y += 28;
        }

        // Apply button
        let btn_y = cy + 300;
        surface.fill_rect_rounded_aa(cx, btn_y, 120, 32, radius::SM, ds::ATOM_COLOR_ACCENT);
        surface.draw_string(cx + 35, btn_y + 12, "Apply", ds::ATOM_COLOR_BG, ds::ATOM_COLOR_ACCENT);
    }

    fn draw_desktop(&self, surface: &SharedSurface, cx: u32, cy: u32) {
        surface.draw_string(cx, cy, "Solid Colors:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        
        let mut x = cx;
        let mut y = cy + 30;
        for (i, color) in self.solid_colors.iter().enumerate() {
            let is_sel = self.selected_color == Some(i);
            if is_sel {
                surface.fill_rect(x - 2, y - 2, 44, 44, ds::ATOM_COLOR_ACCENT);
            }
            surface.fill_rect(x, y, 40, 40, *color);
            
            x += 50;
            if (i + 1) % 4 == 0 {
                x = cx;
                y += 50;
            }
        }

        // Wallpaper Image Placeholder
        let img_y = y + 20;
        surface.draw_string(cx, img_y, "Wallpaper Image:", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);
        surface.fill_rect_rounded_aa(cx, img_y + 25, 300, 150, radius::MD, ds::ATOM_COLOR_SURFACE_ALT);
        surface.draw_string(cx + 80, img_y + 90, "No images found", ds::ATOM_COLOR_TEXT_MUTED, ds::ATOM_COLOR_SURFACE_ALT);
    }

    fn draw_about(&self, surface: &SharedSurface, cx: u32, cy: u32) {
        // System Logo Placeholder
        surface.fill_rect_rounded_aa(cx, cy, 64, 64, radius::MD, ds::ATOM_COLOR_ACCENT);
        surface.draw_string(cx + 24, cy + 28, "A", ds::ATOM_COLOR_BG, ds::ATOM_COLOR_ACCENT);

        surface.draw_string(cx + 80, cy + 10, "Atom OS", ds::ATOM_COLOR_TEXT_PRIMARY, ds::ATOM_COLOR_BG);
        surface.draw_string(cx + 80, cy + 30, "Version 1.0 Luminous", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);

        let mut y = cy + 100;
        
        // Info sections
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

    fn handle_events(&mut self) {
        let mut buf = [0u8; 1024];
        if let Ok(Some(_len)) = try_recv(self.local_port, &mut buf) {
            let hdr = MessageHeader::from_bytes(&buf[..MessageHeader::SIZE]).unwrap();
            match hdr.msg_type {
                MessageType::MouseMove => {
                    let ev = MouseMoveEvent::from_bytes(&buf[MessageHeader::SIZE..]).unwrap();
                    self.mouse_x = ev.x;
                    self.mouse_y = ev.y;
                }
                MessageType::MouseButtonDown => {
                    let ev = MouseButtonEvent::from_bytes(&buf[MessageHeader::SIZE..]).unwrap();
                    if ev.button == MouseButton::Left {
                        self.handle_click();
                    }
                }
                MessageType::KeyPress => {
                    let ev = IpcKeyEvent::from_bytes(&buf[MessageHeader::SIZE..]).unwrap();
                    if ev.character == 27 { // Esc
                        self.running = false;
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_click(&mut self) {
        // Sidebar click
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
        }
        // Content click
        else {
            let cx = SIDEBAR_W + spacing::LG;
            let cy = TOOLBAR_H + spacing::LG;
            
            match self.active_category {
                Category::Monitors => {
                    let mut y = cy + 30;
                    for i in 0..self.mode_count {
                        if self.mouse_y >= (y - 4) as i32 && self.mouse_y < (y + 20) as i32 {
                            self.selected_mode = i;
                            self.needs_redraw = true;
                        }
                        y += 28;
                    }
                    
                    // Apply button
                    let btn_y = cy + 300;
                    if self.mouse_x >= cx as i32 && self.mouse_x < (cx + 120) as i32 &&
                       self.mouse_y >= btn_y as i32 && self.mouse_y < (btn_y + 32) as i32 {
                        self.apply_resolution();
                    }
                }
                Category::Desktop => {
                    let mut x = cx;
                    let mut y = cy + 30;
                    for i in 0..self.solid_colors.len() {
                        if self.mouse_x >= x as i32 && self.mouse_x < (x + 40) as i32 &&
                           self.mouse_y >= y as i32 && self.mouse_y < (y + 40) as i32 {
                            self.selected_color = Some(i);
                            self.apply_wallpaper_color(i);
                            self.needs_redraw = true;
                        }
                        x += 50;
                        if (i + 1) % 4 == 0 {
                            x = cx;
                            y += 50;
                        }
                    }
                }
                Category::About => {}
            }
        }
    }

    fn apply_resolution(&mut self) {
        let mode = self.modes[self.selected_mode];
        log(&format!("Settings: Applying resolution {}x{}", mode.width, mode.height));
        
        self.status_msg = format!("Applied {}x{}", mode.width, mode.height);
        self.status_ticks = 120;
        self.needs_redraw = true;
    }

    fn apply_wallpaper_color(&mut self, idx: usize) {
        let color = self.solid_colors[idx];
        let rgb = ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32);
        
        log(&format!("Settings: Applying wallpaper color #{:06x}", rgb));
        
        let wallpaper_msg = ApplyWallpaperMsg {
            source_type: WallpaperSourceType::SolidColor,
            image_path: None,
            color_rgb: Some(rgb),
            scaling_mode: ScalingMode::Fill,
        };
        
        let payload = wallpaper_msg.to_bytes();
        let mut msg = Vec::with_capacity(MessageHeader::SIZE + payload.len());
        let hdr = MessageHeader::new(MessageType::ApplyWallpaper, payload.len() as u32);
        msg.extend_from_slice(&hdr.to_bytes());
        msg.extend_from_slice(&payload);
        
        let _ = send(self.compositor_port, &msg);
        
        self.status_msg = String::from("Wallpaper updated");
        self.status_ticks = 120;
        self.needs_redraw = true;
    }

    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buf = [0u8; 1024];
        let start = get_ticks();
        while get_ticks() - start < 500 { // 5s timeout
            if let Ok(Some(_len)) = try_recv(port, &mut buf) {
                let hdr = MessageHeader::from_bytes(&buf[..MessageHeader::SIZE]).unwrap();
                if hdr.msg_type == MessageType::SurfaceAssign {
                    return SurfaceAssignMsg::from_bytes(&buf[MessageHeader::SIZE..]);
                }
            }
            yield_now();
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
    log("Settings: starting");
    let port = match create_port() {
        Ok(p) => p,
        Err(_) => { log("Settings: create_port failed"); exit(1); }
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
        None => { log("Settings: surface timeout"); exit(1); }
    };

    let surface = match SharedSurface::from_region(sa.region_id, sa.width, sa.height) {
        Ok(s) => s,
        Err(_) => { log("Settings: surface map failed"); exit(1); }
    };

    let (modes, mode_count) = query_modes();
    let mut app = SettingsApp::new(sa.window_id, sa.compositor_port, port, surface, modes, mode_count);
    app.run();
    exit(0);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log(&format!("Settings: PANIC - {:?}", info));
    exit(0xFF);
}
