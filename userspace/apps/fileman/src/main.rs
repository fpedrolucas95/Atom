//! Atom OS Modern File Explorer
//!
//! A redesigned graphical file manager that follows the Atom Design System (v1.0 Luminous Dark).
//! Features a sidebar for quick navigation, a modern toolbar with search, and improved file views.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

mod error;
mod fs;

use fs::{Dir, DirEntry};

use atom_syscall::graphics::{Color, SharedSurface};
use atom_syscall::ipc::{create_port, try_recv, send, PortId};
use atom_syscall::thread::{exit, yield_now, get_ticks};
use atom_syscall::debug::log;

use libipc::messages::{
    MessageType, MessageHeader, SurfaceAssignMsg, SurfacePresentMsg,
    KeyEvent as IpcKeyEvent, MouseButtonEvent, MouseMoveEvent, MouseButton,
    AppRegisterMsg, AppLaunchRequestMsg, AppLaunchReplyMsg,
};
use libipc::protocol::{lookup_service, send_message, try_recv_message, get_payload};

use atom_theme::colors as ds;
use atom_theme::spacing;
use atom_theme::radius;

// ============================================================================
// Bump Allocator – 8 MB heap for the new file manager
// ============================================================================

const HEAP_SIZE: usize = 8 * 1024 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0u8; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size  = layout.size();
        let align = layout.align().max(16);
        loop {
            let cur     = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let new_end = aligned + size;
            if new_end > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(cur, new_end,
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
fn alloc_error(_layout: Layout) -> ! { loop {} }

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log(&format!("fileman: PANIC - {:?}", info));
    exit(0xFF);
}

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

// ============================================================================
// UI Constants & Theme
// ============================================================================

const CHAR_W:    u32 = 8;
const CHAR_H:    u32 = 8;
const TOOLBAR_H: u32 = 48;
const SIDEBAR_W: u32 = 180;
const STATUS_H:  u32 = 24;

// Icon view cell
const ICON_CELL_W: u32 = 100;
const ICON_CELL_H: u32 = 90;
const ICON_W:      u32 = 48;
const ICON_H:      u32 = 48;

// List view
const LIST_HDR_H:  u32 = 28;
const LIST_ROW_H:  u32 = 24;

#[derive(Clone, Copy, PartialEq)]
enum ViewMode { Icons, List }

#[derive(Clone, Copy, PartialEq)]
enum FileKind { Dir, Txt, Img, Exec, Zip, Aud, Vid, Other }

impl FileKind {
    fn detect(name: &str, is_dir: bool) -> Self {
        if is_dir { return Self::Dir; }
        let ext = name.rfind('.').map(|p| &name[p+1..]).unwrap_or("");
        match ext {
            "txt"|"md"|"rst"|"log"|"cfg"|"conf"|"toml"|"ini"|"yaml"|"yml"
            |"rs" |"c"  |"h"  |"cpp"|"cc" |"py" |"js" |"ts" |"sh" |"bash"
            |"json"|"xml"|"csv"|"env" => Self::Txt,
            "png"|"jpg"|"jpeg"|"bmp"|"gif"|"ico"|"tga"|"tiff"|"svg" => Self::Img,
            "exe"|"bin"|"elf"|"atxf"|"so"|"a"|"out" => Self::Exec,
            "zip"|"tar"|"gz"|"xz"|"7z"|"rar"|"bz2"|"lz4" => Self::Zip,
            "mp3"|"wav"|"ogg"|"flac"|"aac"|"m4a" => Self::Aud,
            "mp4"|"avi"|"mkv"|"mov"|"webm"|"m4v" => Self::Vid,
            _ => Self::Other,
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Dir  => ds::ATOM_COLOR_ACCENT,
            Self::Txt  => ds::ATOM_COLOR_SUCCESS,
            Self::Img  => Color::new(200, 120, 240),
            Self::Exec => ds::ATOM_COLOR_ERROR,
            Self::Zip  => ds::ATOM_COLOR_WARNING,
            Self::Aud  => Color::new(86, 218, 188),
            Self::Vid  => Color::new(170, 120, 240),
            Self::Other=> ds::ATOM_COLOR_TEXT_SECONDARY,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Dir  => "DIR",
            Self::Txt  => "TXT",
            Self::Img  => "IMG",
            Self::Exec => "EXE",
            Self::Zip  => "ZIP",
            Self::Aud  => "AUD",
            Self::Vid  => "VID",
            Self::Other=> "FILE",
        }
    }
}

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    kind: FileKind,
}

impl Entry {
    fn from_dir_entry(de: &DirEntry) -> Self {
        let kind = FileKind::detect(&de.name, de.is_dir());
        Self {
            name: de.name.clone(),
            is_dir: de.is_dir(),
            size: de.size,
            kind,
        }
    }
}

struct SidebarItem {
    label: &'static str,
    path: &'static str,
    icon: &'static str,
}

const SIDEBAR_ITEMS: &[SidebarItem] = &[
    SidebarItem { label: "Home",      path: "/home",   icon: "H" },
    SidebarItem { label: "Documents", path: "/home/docs", icon: "D" },
    SidebarItem { label: "Downloads", path: "/home/dl",   icon: "L" },
    SidebarItem { label: "System",    path: "/",       icon: "S" },
    SidebarItem { label: "Temporary", path: "/tmp",    icon: "T" },
];

// ============================================================================
// File Manager State
// ============================================================================

struct FileManager {
    window_id:        u32,
    compositor_port:  PortId,
    local_port:       PortId,
    surface:          Option<SharedSurface>,
    width:            u32,
    height:           u32,

    cwd:              String,
    history:          Vec<String>,
    entries:          Vec<Entry>,
    filtered_entries: Vec<usize>, // Indices into entries
    search_query:     String,

    view_mode:        ViewMode,
    selected:         Option<usize>, // Index into filtered_entries
    scroll_offset:    u32,
    sidebar_selected: Option<usize>,

    mouse_x:          i32,
    mouse_y:          i32,
    last_click_idx:   i32,
    last_click_tick:  u64,

    status_msg:       String,
    running:          bool,
    needs_redraw:     bool,
}

impl FileManager {
    fn new(window_id: u32, compositor_port: PortId, local_port: PortId, surface: SharedSurface) -> Self {
        let w = surface.width();
        let h = surface.height();
        let mut fm = Self {
            window_id, compositor_port, local_port,
            surface: Some(surface),
            width: w, height: h,
            cwd: String::from("/home"),
            history: Vec::new(),
            entries: Vec::new(),
            filtered_entries: Vec::new(),
            search_query: String::new(),
            view_mode: ViewMode::Icons,
            selected: None,
            scroll_offset: 0,
            sidebar_selected: Some(0),
            mouse_x: 0, mouse_y: 0,
            last_click_idx: -1,
            last_click_tick: 0,
            status_msg: String::from("Ready"),
            running: true,
            needs_redraw: true,
        };
        fm.load_dir("/home");
        fm
    }

    fn load_dir(&mut self, path: &str) {
        self.entries.clear();
        self.selected = None;
        self.scroll_offset = 0;
        
        match Dir::open(path) {
            Ok(dir) => {
                if let Ok(list) = dir.list() {
                    for de in list {
                        if de.name == "." || de.name == ".." { continue; }
                        self.entries.push(Entry::from_dir_entry(&de));
                    }
                }
            }
            Err(_) => {
                self.status_msg = format!("Error: Could not open {}", path);
            }
        }
        self.apply_filter();
        self.status_msg = format!("{} items", self.filtered_entries.len());
        self.needs_redraw = true;
    }

    fn apply_filter(&mut self) {
        self.filtered_entries.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            if self.search_query.is_empty() || entry.name.to_lowercase().contains(&self.search_query.to_lowercase()) {
                self.filtered_entries.push(i);
            }
        }
        self.selected = None;
        self.needs_redraw = true;
    }

    fn navigate_to(&mut self, path: String) {
        let old = self.cwd.clone();
        self.cwd = path.clone();
        self.load_dir(&path);
        self.history.push(old);
        
        // Update sidebar selection
        self.sidebar_selected = None;
        for (i, item) in SIDEBAR_ITEMS.iter().enumerate() {
            if item.path == self.cwd {
                self.sidebar_selected = Some(i);
                break;
            }
        }
    }

    fn render(&mut self) {
        if !self.needs_redraw { return; }
        self.needs_redraw = false;
        let surface = self.surface.as_ref().unwrap();

        // 1. Background
        surface.fill_rect(0, 0, self.width, self.height, ds::ATOM_COLOR_BG);

        // 2. Sidebar
        self.draw_sidebar(surface);

        // 3. Toolbar
        self.draw_toolbar(surface);

        // 4. Content Area
        self.draw_content(surface);

        // 5. Status Bar
        self.draw_status(surface);
    }

    fn draw_sidebar(&self, surface: &SharedSurface) {
        surface.fill_rect(0, 0, SIDEBAR_W, self.height, ds::ATOM_COLOR_SURFACE);
        surface.fill_rect(SIDEBAR_W - 1, 0, 1, self.height, ds::ATOM_COLOR_BORDER);

        let mut y = TOOLBAR_H + spacing::MD;
        for (i, item) in SIDEBAR_ITEMS.iter().enumerate() {
            let is_sel = self.sidebar_selected == Some(i);
            let bg = if is_sel { ds::ATOM_COLOR_ACCENT } else { ds::ATOM_COLOR_SURFACE };
            let fg = if is_sel { ds::ATOM_COLOR_BG } else { ds::ATOM_COLOR_TEXT_PRIMARY };

            if is_sel {
                surface.fill_rect_rounded_aa(spacing::SM, y, SIDEBAR_W - spacing::LG, 32, radius::SM, bg);
            }

            surface.draw_string(spacing::LG, y + 12, item.icon, fg, bg);
            surface.draw_string(spacing::LG + 20, y + 12, item.label, fg, bg);
            y += 40;
        }
    }

    fn draw_toolbar(&self, surface: &SharedSurface) {
        surface.fill_rect(0, 0, self.width, TOOLBAR_H, ds::ATOM_COLOR_SURFACE_ALT);
        surface.fill_rect(0, TOOLBAR_H - 1, self.width, 1, ds::ATOM_COLOR_BORDER);

        // Navigation buttons
        self.draw_tool_btn(surface, spacing::MD, 10, 32, 28, "<", !self.history.is_empty());
        self.draw_tool_btn(surface, spacing::MD + 40, 10, 32, 28, "^", self.cwd != "/");

        // Path bar
        let path_x = spacing::MD + 80;
        let path_w = self.width - path_x - 200;
        surface.fill_rect_rounded_aa(path_x, 10, path_w, 28, radius::SM, ds::ATOM_COLOR_BG);
        surface.draw_string(path_x + spacing::SM, 20, &self.cwd, ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_BG);

        // Search bar
        let search_x = self.width - 180;
        surface.fill_rect_rounded_aa(search_x, 10, 160, 28, radius::SM, ds::ATOM_COLOR_BG);
        let search_text = if self.search_query.is_empty() { "Search..." } else { &self.search_query };
        surface.draw_string(search_x + spacing::SM, 20, search_text, ds::ATOM_COLOR_TEXT_MUTED, ds::ATOM_COLOR_BG);
    }

    fn draw_tool_btn(&self, surface: &SharedSurface, x: u32, y: u32, w: u32, h: u32, label: &str, enabled: bool) {
        let bg = ds::ATOM_COLOR_SURFACE;
        let fg = if enabled { ds::ATOM_COLOR_TEXT_PRIMARY } else { ds::ATOM_COLOR_TEXT_MUTED };
        surface.fill_rect_rounded_aa(x, y, w, h, radius::XS, bg);
        let tx = x + (w - label.len() as u32 * CHAR_W) / 2;
        let ty = y + (h - CHAR_H) / 2;
        surface.draw_string(tx, ty, label, fg, bg);
    }

    fn draw_content(&self, surface: &SharedSurface) {
        let cx = SIDEBAR_W;
        let cy = TOOLBAR_H;
        let cw = self.width - SIDEBAR_W;
        let ch = self.height - TOOLBAR_H - STATUS_H;

        if self.filtered_entries.is_empty() {
            let msg = "No items found";
            surface.draw_string(cx + (cw - msg.len() as u32 * CHAR_W) / 2, cy + ch / 2, msg, ds::ATOM_COLOR_TEXT_MUTED, ds::ATOM_COLOR_BG);
            return;
        }

        match self.view_mode {
            ViewMode::Icons => self.draw_icons(surface, cx, cy, cw, ch),
            ViewMode::List  => self.draw_list(surface, cx, cy, cw, ch),
        }
    }

    fn draw_icons(&self, surface: &SharedSurface, cx: u32, cy: u32, cw: u32, ch: u32) {
        let cols = (cw / ICON_CELL_W).max(1);
        for (i, &entry_idx) in self.filtered_entries.iter().enumerate() {
            let entry = &self.entries[entry_idx];
            let row = i as u32 / cols;
            let col = i as u32 % cols;
            
            let x = cx + col * ICON_CELL_W + spacing::SM;
            let y = cy + row * ICON_CELL_H + spacing::SM;
            
            if y + ICON_CELL_H > cy + ch { break; }

            let is_sel = self.selected == Some(i);
            if is_sel {
                surface.fill_rect_rounded_aa(x, y, ICON_CELL_W - spacing::MD, ICON_CELL_H - spacing::MD, radius::MD, ds::ATOM_COLOR_SURFACE_ALT);
            }

            // Icon
            let ix = x + (ICON_CELL_W - spacing::MD - ICON_W) / 2;
            let iy = y + spacing::SM;
            surface.fill_rect_rounded_aa(ix, iy, ICON_W, ICON_H, radius::SM, entry.kind.color());
            surface.draw_string(ix + (ICON_W - 3 * CHAR_W) / 2, iy + (ICON_H - CHAR_H) / 2, entry.kind.label(), ds::ATOM_COLOR_BG, entry.kind.color());

            // Label
            let name = if entry.name.len() > 10 { format!("{}..", &entry.name[..8]) } else { entry.name.clone() };
            let lx = x + (ICON_CELL_W - spacing::MD - name.len() as u32 * CHAR_W) / 2;
            surface.draw_string(lx, iy + ICON_H + spacing::SM, &name, ds::ATOM_COLOR_TEXT_PRIMARY, if is_sel { ds::ATOM_COLOR_SURFACE_ALT } else { ds::ATOM_COLOR_BG });
        }
    }

    fn draw_list(&self, surface: &SharedSurface, cx: u32, cy: u32, cw: u32, ch: u32) {
        // Header
        surface.fill_rect(cx, cy, cw, LIST_HDR_H, ds::ATOM_COLOR_SURFACE_ALT);
        surface.draw_string(cx + spacing::MD, cy + 10, "Name", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_SURFACE_ALT);
        surface.draw_string(cx + cw - 100, cy + 10, "Size", ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_SURFACE_ALT);

        for (i, &entry_idx) in self.filtered_entries.iter().enumerate() {
            let entry = &self.entries[entry_idx];
            let y = cy + LIST_HDR_H + i as u32 * LIST_ROW_H;
            if y + LIST_ROW_H > cy + ch { break; }

            let is_sel = self.selected == Some(i);
            let bg = if is_sel { ds::ATOM_COLOR_SURFACE_ALT } else { ds::ATOM_COLOR_BG };
            if is_sel {
                surface.fill_rect(cx, y, cw, LIST_ROW_H, bg);
            }

            let icon_color = entry.kind.color();
            surface.fill_rect(cx + spacing::SM, y + 4, 16, 16, icon_color);
            surface.draw_string(cx + spacing::LG + 8, y + 8, &entry.name, ds::ATOM_COLOR_TEXT_PRIMARY, bg);
            
            let size_str = if entry.is_dir { String::from("--") } else { format_size(entry.size) };
            surface.draw_string(cx + cw - 100, y + 8, &size_str, ds::ATOM_COLOR_TEXT_SECONDARY, bg);
        }
    }

    fn draw_status(&self, surface: &SharedSurface) {
        let y = self.height - STATUS_H;
        surface.fill_rect(0, y, self.width, STATUS_H, ds::ATOM_COLOR_SURFACE);
        surface.fill_rect(0, y, self.width, 1, ds::ATOM_COLOR_BORDER);
        surface.draw_string(spacing::MD, y + 8, &self.status_msg, ds::ATOM_COLOR_TEXT_SECONDARY, ds::ATOM_COLOR_SURFACE);
    }

    fn handle_event(&mut self) {
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
                    self.handle_key(ev);
                }
                _ => {}
            }
        }
    }

    fn handle_click(&mut self) {
        // Toolbar click — check first (takes priority over sidebar)
        if self.mouse_y < TOOLBAR_H as i32 {
            if self.mouse_x >= spacing::MD as i32 && self.mouse_x < (spacing::MD + 32) as i32 {
                self.go_back();
            } else if self.mouse_x >= (spacing::MD + 40) as i32 && self.mouse_x < (spacing::MD + 72) as i32 {
                self.go_up();
            }
        }
        // Sidebar click
        else if self.mouse_x < SIDEBAR_W as i32 {
            let mut y = TOOLBAR_H + spacing::MD;
            for i in 0..SIDEBAR_ITEMS.len() {
                if self.mouse_y >= y as i32 && self.mouse_y < (y + 40) as i32 {
                    self.sidebar_selected = Some(i);
                    self.navigate_to(String::from(SIDEBAR_ITEMS[i].path));
                    break;
                }
                y += 40;
            }
        }
        // Content click
        else {
            let cx = SIDEBAR_W as i32;
            let cy = TOOLBAR_H as i32;
            let mx = self.mouse_x - cx;
            let my = self.mouse_y - cy;
            
            match self.view_mode {
                ViewMode::Icons => {
                    let cols = ((self.width - SIDEBAR_W) / ICON_CELL_W).max(1);
                    let col = mx as u32 / ICON_CELL_W;
                    let row = my as u32 / ICON_CELL_H;
                    let idx = (row * cols + col) as usize;
                    if idx < self.filtered_entries.len() {
                        if self.selected == Some(idx) {
                            let tick = get_ticks();
                            if tick - self.last_click_tick < 50 { // Double click
                                self.activate(idx);
                            }
                            self.last_click_tick = tick;
                        } else {
                            self.selected = Some(idx);
                            self.last_click_tick = get_ticks();
                        }
                    }
                }
                ViewMode::List => {
                    let idx = ((my as u32 - LIST_HDR_H) / LIST_ROW_H) as usize;
                    if idx < self.filtered_entries.len() {
                        if self.selected == Some(idx) {
                            let tick = get_ticks();
                            if tick - self.last_click_tick < 50 {
                                self.activate(idx);
                            }
                            self.last_click_tick = tick;
                        } else {
                            self.selected = Some(idx);
                            self.last_click_tick = get_ticks();
                        }
                    }
                }
            }
        }
        self.needs_redraw = true;
    }

    fn handle_key(&mut self, ev: IpcKeyEvent) {
        // Simple search input — use ev.character for printable chars
        if ev.character >= 32 && ev.character <= 126 {
            self.search_query.push(ev.character as char);
            self.apply_filter();
        } else if ev.character == 8 || ev.scancode == 0x0E { // Backspace
            self.search_query.pop();
            self.apply_filter();
        } else if ev.character == 27 || ev.scancode == 0x01 { // Esc
            self.search_query.clear();
            self.apply_filter();
        }
        self.needs_redraw = true;
    }

    fn activate(&mut self, idx: usize) {
        let entry_idx = self.filtered_entries[idx];
        let entry = &self.entries[entry_idx];
        if entry.is_dir {
            let new_path = if self.cwd.ends_with('/') {
                format!("{}{}", self.cwd, entry.name)
            } else {
                format!("{}/{}", self.cwd, entry.name)
            };
            self.navigate_to(new_path);
        } else {
            if entry.name.ends_with(".atxf") {
                let path = self.full_path(entry_idx);
                self.status_msg = format!("Launching {}...", entry.name);
                self.needs_redraw = true;
                self.launch_app(&path);
            } else {
                self.status_msg = format!("Selected: {} ({})", entry.name, format_size(entry.size));
            }
        }
    }

    fn launch_app(&mut self, path: &str) {
        // Look up app_launcher service
        let launcher_port = match lookup_service("app_launcher") {
            Ok(p) => p,
            Err(_) => {
                self.status_msg = String::from("Error: app_launcher not available");
                log("fileman: app_launcher not found in namesvc");
                return;
            }
        };

        // Build the launch request
        let req = match AppLaunchRequestMsg::new(self.local_port as u64, path) {
            Some(r) => r,
            None => {
                self.status_msg = String::from("Error: path too long");
                return;
            }
        };

        // Send the request
        if send_message(launcher_port, MessageType::AppLaunchRequest, &req.to_bytes()).is_err() {
            self.status_msg = String::from("Error: failed to send launch request");
            log("fileman: failed to send AppLaunchRequest");
            return;
        }

        // Wait for reply (up to ~5 seconds at 100Hz)
        let mut buf = [0u8; 512];
        for _ in 0..500 {
            match try_recv_message(self.local_port, &mut buf) {
                Ok(Some((header, len))) if header.msg_type == MessageType::AppLaunchReply => {
                    let payload = get_payload(&buf, len);
                    if let Some(reply) = AppLaunchReplyMsg::from_bytes(payload) {
                        if reply.status == libipc::messages::launch_status::LAUNCH_OK {
                            self.status_msg = format!("Launched (pid={})", reply.pid);
                            log("fileman: app launched successfully");
                        } else {
                            let err = reply.err_msg_str();
                            self.status_msg = format!("Launch failed: {}", err);
                            let msg = format!("fileman: launch error — {}", err);
                            log(&msg);
                        }
                    }
                    return;
                }
                Ok(Some(_)) => {
                    // Other message (e.g. mouse/key event) — re-queue by ignoring for now
                }
                _ => {
                    atom_syscall::thread::yield_now();
                }
            }
        }

        self.status_msg = String::from("Launch timed out");
        log("fileman: timed out waiting for AppLaunchReply");
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.cwd = prev.clone();
            self.load_dir(&prev);
        }
    }

    fn go_up(&mut self) {
        if self.cwd == "/" { return; }
        let parent = match self.cwd.rfind('/') {
            Some(0) => String::from("/"),
            Some(pos) => String::from(&self.cwd[..pos]),
            None => String::from("/"),
        };
        self.navigate_to(parent);
    }

    fn full_path(&self, idx: usize) -> String {
        if self.cwd.ends_with('/') {
            format!("{}{}", self.cwd, self.entries[idx].name)
        } else {
            format!("{}/{}", self.cwd, self.entries[idx].name)
        }
    }
}

fn format_size(b: u64) -> String {
    if b < 1024 { format!("{} B", b) }
    else if b < 1024*1024 { format!("{} KB", b / 1024) }
    else { format!("{} MB", b / (1024*1024)) }
}

fn main() -> ! {
    let local_port = create_port().unwrap();

    // Wait for compositor.register to be available and register
    let register_port = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(port) => break port,
            Err(_) => { atom_syscall::thread::yield_now(); }
        }
    };

    let reg = AppRegisterMsg { app_port: local_port, pid: 0 };
    let hdr = MessageHeader::new(MessageType::AppRegister, AppRegisterMsg::SIZE as u32);
    let mut buf = [0u8; 128];
    buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    buf[MessageHeader::SIZE..MessageHeader::SIZE + AppRegisterMsg::SIZE].copy_from_slice(&reg.to_bytes());
    send(register_port, &buf[..MessageHeader::SIZE + AppRegisterMsg::SIZE]).unwrap();

    // Also get the main compositor port for SurfacePresent messages
    let compositor_port = libipc::protocol::lookup_service("compositor").unwrap();

    let mut fm: Option<FileManager> = None;

    loop {
        if let Some(ref mut f) = fm {
            f.handle_event();
            f.render();
            
            // Present surface
            let pres = SurfacePresentMsg { window_id: f.window_id };
            let hdr = MessageHeader::new(MessageType::SurfacePresent, SurfacePresentMsg::SIZE as u32);
            let mut buf = [0u8; 64];
            buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
            buf[MessageHeader::SIZE..MessageHeader::SIZE + SurfacePresentMsg::SIZE].copy_from_slice(&pres.to_bytes());
            let total = MessageHeader::SIZE + SurfacePresentMsg::SIZE;
            if send(compositor_port, &buf[..total]).is_err() {
                yield_now();
                continue;
            }
        } else {
            let mut buf = [0u8; 1024];
            if let Ok(Some(_)) = try_recv(local_port, &mut buf) {
                let hdr = MessageHeader::from_bytes(&buf[..MessageHeader::SIZE]).unwrap();
                if hdr.msg_type == MessageType::SurfaceAssign {
                    let msg = SurfaceAssignMsg::from_bytes(&buf[MessageHeader::SIZE..]).unwrap();
                    let surface = SharedSurface::from_region(msg.region_id, msg.width, msg.height).unwrap();
                    fm = Some(FileManager::new(msg.window_id, compositor_port, local_port, surface));
                }
            }
        }
        yield_now();
    }
}
