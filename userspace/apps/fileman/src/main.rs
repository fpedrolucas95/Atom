// Atom OS GUI File Manager
//
// A graphical file manager that renders to a compositor-assigned shared surface.
// Supports icon view and list view, navigation, and file operations.
//
// Architecture (same as terminal):
//   - Registers with compositor via IPC → receives a SharedSurface
//   - Renders icon/list UI directly into the surface
//   - Handles keyboard & mouse events forwarded by compositor

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::format;
use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::arch::asm;

mod error;
mod fs;
mod svg;

use fs::{Dir, DirEntry, FsOps};

use atom_syscall::graphics::{Color, SharedSurface};
use atom_syscall::ipc::{create_port, close_port, try_recv, send, wait_any, PortId};
use atom_syscall::thread::{exit, yield_now, get_ticks};
use atom_syscall::debug::log;
use atom_syscall::fs as atom_fs;

use libipc::messages::{
    MessageType, MessageHeader, SurfaceAssignMsg, SurfacePresentMsg,
    KeyEvent as IpcKeyEvent, MouseButtonEvent, MouseMoveEvent, MouseButton,
    MouseScrollEvent,
    AppLaunchRequestMsg, AppLaunchReplyMsg, launch_status,
};

// ============================================================================
// Bump Allocator – 4 MB heap for file manager
// ============================================================================

const HEAP_SIZE: usize = 4 * 1024 * 1024;

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

// ============================================================================
// Panic handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("fileman: PANIC");
    exit(0xFF);
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

// ============================================================================
// Theme
// ============================================================================

struct Theme;
impl Theme {
    // Backgrounds
    const BG:          Color = Color::new(20, 22, 30);
    const TOOLBAR_BG:  Color = Color::new(16, 18, 26);
    const STATUS_BG:   Color = Color::new(14, 16, 22);
    const LIST_HDR_BG: Color = Color::new(24, 27, 36);
    const SEPARATOR:   Color = Color::new(42, 48, 64);
    const HOVER_BG:    Color = Color::new(34, 40, 54);
    const SEL_BG:      Color = Color::new(40, 68, 120);
    const BTN_BG:      Color = Color::new(34, 38, 50);
    const BTN_ACTIVE:  Color = Color::new(55, 80, 140);
    // Text
    const TEXT:        Color = Color::new(210, 215, 225);
    const TEXT_DIM:    Color = Color::new(110, 118, 138);
    const TEXT_PATH:   Color = Color::new(72, 199, 142);
    const TEXT_ACCENT: Color = Color::new(99, 143, 255);
    // File type colours (more vibrant)
    const DIR:         Color = Color::new(99, 143, 255);
    const TXT:         Color = Color::new(72, 199, 142);
    const IMG:         Color = Color::new(200, 120, 240);
    const EXEC:        Color = Color::new(240, 85, 96);
    const ZIP:         Color = Color::new(245, 189, 65);
    const AUD:         Color = Color::new(86, 218, 188);
    const VID:         Color = Color::new(170, 120, 240);
    const OTHER:       Color = Color::new(140, 148, 165);
}

// ============================================================================
// File-type detection
// ============================================================================

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
            Self::Dir  => Theme::DIR,
            Self::Txt  => Theme::TXT,
            Self::Img  => Theme::IMG,
            Self::Exec => Theme::EXEC,
            Self::Zip  => Theme::ZIP,
            Self::Aud  => Theme::AUD,
            Self::Vid  => Theme::VID,
            Self::Other=> Theme::OTHER,
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

// ============================================================================
// Cached file entry
// ============================================================================

struct Entry {
    name: String,
    is_dir: bool,
    size: u64,
    kind: FileKind,
    icon_color_override: Option<Color>,
    has_embedded_icon: bool,
    svg_bitmap: Option<svg::SvgBitmap>,
}

impl Entry {
    fn from_dir_entry(de: &DirEntry) -> Self {
        let kind = FileKind::detect(&de.name, de.is_dir());
        Self {
            name: de.name.clone(),
            is_dir: de.is_dir(),
            size: de.size,
            kind,
            icon_color_override: None,
            has_embedded_icon: false,
            svg_bitmap: None,
        }
    }

    fn icon_color(&self) -> Color {
        self.icon_color_override.unwrap_or(self.kind.color())
    }

    fn icon_label(&self) -> &'static str {
        if self.has_embedded_icon && self.kind == FileKind::Exec {
            "APP"
        } else {
            self.kind.label()
        }
    }

    fn size_str(&self) -> String {
        if self.is_dir { return String::from("  --"); }
        format_size(self.size)
    }

    fn ext(&self) -> &str {
        if self.is_dir { return ""; }
        self.name.rfind('.').map(|p| &self.name[p+1..]).unwrap_or("")
    }
}

fn format_size(b: u64) -> String {
    if b < 1024 { format!("{} B", b) }
    else if b < 1024*1024 {
        let kb = b / 1024;
        let f  = (b % 1024) * 10 / 1024;
        format!("{}.{} KB", kb, f)
    } else if b < 1024*1024*1024 {
        let mb = b / (1024*1024);
        let f  = (b % (1024*1024)) * 10 / (1024*1024);
        format!("{}.{} MB", mb, f)
    } else {
        format!("{} GB", b / (1024*1024*1024))
    }
}

// ============================================================================
// View mode + clipboard
// ============================================================================

#[derive(PartialEq)]
enum ViewMode { Icons, List }

struct Clipboard { path: String, cut: bool }

// ============================================================================
// Layout constants
// ============================================================================

const CHAR_W:    u32 = 8;
const CHAR_H:    u32 = 8;
const TOOLBAR_H: u32 = 36;
const STATUS_H:  u32 = 22;

// Icon view cell
const ICON_CELL_W: u32 = 90;
const ICON_CELL_H: u32 = 82;
const ICON_W:      u32 = 52;
const ICON_H:      u32 = 46;

// List view
const LIST_HDR_H:  u32 = 20;
const LIST_ROW_H:  u32 = 18;

// Toolbar buttons (from left)
const TB_BTN_H:  u32 = 26;
const TB_BTN_Y:  u32 = (TOOLBAR_H - TB_BTN_H) / 2;

struct Btn { x: u32, w: u32 }
impl Btn {
    const fn new(x: u32, w: u32) -> Self { Btn { x, w } }
    fn contains(&self, mx: i32, my: i32) -> bool {
        mx >= self.x as i32 && mx < (self.x + self.w) as i32
            && my >= TB_BTN_Y as i32 && my < (TB_BTN_Y + TB_BTN_H) as i32
    }
}

// Fixed left buttons
const BTN_BACK: Btn = Btn::new(4,  32);
const BTN_UP:   Btn = Btn::new(40, 26);

// Right-side buttons (computed at render time relative to surface width)
fn btn_refresh(sw: u32) -> Btn { Btn::new(sw - 36, 30) }
fn btn_icons(sw: u32)   -> Btn { Btn::new(sw - 36 - 4 - 52, 50) }
fn btn_list(sw: u32)    -> Btn { Btn::new(sw - 36 - 4 - 52 - 4 - 44, 42) }
fn btn_new(sw: u32)     -> Btn { Btn::new(sw - 36 - 4 - 52 - 4 - 44 - 4 - 32, 30) }

fn path_bar_rect(sw: u32) -> (u32, u32) {
    // x and width
    let x = BTN_UP.x + BTN_UP.w + 8;
    let w = btn_new(sw).x - x - 8;
    (x, w)
}

// ============================================================================
// Status message helper (no-alloc fixed buffer)
// ============================================================================

struct StatusBuf { buf: [u8; 200], len: usize }

impl StatusBuf {
    fn new() -> Self { Self { buf: [0u8; 200], len: 0 } }
    fn set(&mut self, s: &str) {
        self.len = 0;
        for b in s.bytes() {
            if self.len < 199 { self.buf[self.len] = b; self.len += 1; }
        }
    }
    fn as_str(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

// ============================================================================
// App-launch error type
// ============================================================================

/// Errors that can occur while requesting a launch through the `app_launcher`
/// service.  Used by the `request_app_launch` IPC helper so that `launch_atxf`
/// remains a thin UI wrapper free of IPC mechanics.
enum LaunchError {
    /// Name service did not find `app_launcher`.
    NoLauncher,
    /// Path is too long to fit in an `AppLaunchRequestMsg`.
    PathTooLong,
    /// IPC send to `app_launcher` failed.
    SendFailed,
    /// Unexpected IPC error while waiting for the reply.
    IpcError,
    /// Reply did not arrive within the 2-second deadline.
    Timeout,
    /// Reply arrived but was shorter than `AppLaunchReplyMsg::SIZE`.
    TruncatedReply,
    /// Reply arrived but could not be deserialised.
    MalformedReply,
}

// ============================================================================
// File Manager state
// ============================================================================

struct FileManager {
    // IPC / Window
    window_id:        u32,
    compositor_port:  PortId,
    local_port:       PortId,
    surface:          Option<SharedSurface>,
    surface_width:    u32,
    surface_height:   u32,

    // Navigation
    cwd:              String,
    history:          Vec<String>,   // backward stack

    // File listing
    entries:          Vec<Entry>,

    // UI
    view_mode:        ViewMode,
    selected:         Option<usize>,
    scroll_offset:    u32,

    // Mouse
    mouse_x:          i32,
    mouse_y:          i32,
    last_click_idx:   i32,           // -1 = none
    last_click_tick:  u64,

    // Clipboard
    clipboard:        Option<Clipboard>,

    // Status bar
    status:           StatusBuf,

    // App
    running:          bool,
    needs_redraw:     bool,
}

impl FileManager {
    fn extract_embedded_icon_svg(atxf: &[u8]) -> Option<&[u8]> {
        if atxf.len() < 8 {
            return None;
        }
        let trailer_start = atxf.len() - 8;
        let icon_len = u32::from_le_bytes([
            atxf[trailer_start],
            atxf[trailer_start + 1],
            atxf[trailer_start + 2],
            atxf[trailer_start + 3],
        ]) as usize;
        let magic = u32::from_le_bytes([
            atxf[trailer_start + 4],
            atxf[trailer_start + 5],
            atxf[trailer_start + 6],
            atxf[trailer_start + 7],
        ]);

        const ATXF_ICON_MAGIC: u32 = 0x4154_5849; // "ATXI"
        if magic != ATXF_ICON_MAGIC || icon_len > trailer_start {
            return None;
        }

        let icon_start = trailer_start - icon_len;
        Some(&atxf[icon_start..trailer_start])
    }

    fn hex_nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(10 + b - b'a'),
            b'A'..=b'F' => Some(10 + b - b'A'),
            _ => None,
        }
    }

    fn extract_icon_color(svg: &[u8]) -> Option<Color> {
        let mut best: Option<Color> = None;
        let mut best_score: i32 = -1;
        let mut i = 0usize;
        while i + 7 <= svg.len() {
            if svg[i] == b'#' {
                if let (Some(h1), Some(h2), Some(h3), Some(h4), Some(h5), Some(h6)) = (
                    Self::hex_nibble(svg[i + 1]),
                    Self::hex_nibble(svg[i + 2]),
                    Self::hex_nibble(svg[i + 3]),
                    Self::hex_nibble(svg[i + 4]),
                    Self::hex_nibble(svg[i + 5]),
                    Self::hex_nibble(svg[i + 6]),
                ) {
                    let r = (h1 << 4) | h2;
                    let g = (h3 << 4) | h4;
                    let b = (h5 << 4) | h6;
                    let maxc = r.max(g).max(b) as i32;
                    let minc = r.min(g).min(b) as i32;
                    let sat = maxc - minc;
                    let lum = maxc + minc;
                    if sat >= 18 {
                        let score = sat * 4 + maxc - (lum - 255).abs() / 2;
                        if score > best_score {
                            best_score = score;
                            best = Some(Color::new(r, g, b));
                        }
                    }
                }
            }
            i += 1;
        }
        best
    }


    fn new(window_id: u32, compositor_port: PortId, local_port: PortId,
           surface: SharedSurface) -> Self {
        let w = surface.width();
        let h = surface.height();
        Self {
            window_id, compositor_port, local_port,
            surface: Some(surface),
            surface_width: w, surface_height: h,
            cwd: String::from("/"),
            history: Vec::new(),
            entries: Vec::new(),
            view_mode: ViewMode::Icons,
            selected: None,
            scroll_offset: 0,
            mouse_x: 0, mouse_y: 0,
            last_click_idx: -1,
            last_click_tick: 0,
            clipboard: None,
            status: StatusBuf::new(),
            running: true,
            needs_redraw: true,
        }
    }

    // ─── Content area helpers ────────────────────────────────────────────────

    fn content_y(&self) -> u32 { TOOLBAR_H }
    fn content_h(&self) -> u32 {
        self.surface_height.saturating_sub(TOOLBAR_H + STATUS_H)
    }
    fn status_y(&self) -> u32 { self.surface_height - STATUS_H }

    // ─── Load directory ──────────────────────────────────────────────────────

    fn load_dir(&mut self, path: &str) {
        self.entries.clear();
        self.selected = None;
        self.scroll_offset = 0;

        match Dir::open(path) {
            Ok(dir) => {
                match dir.list() {
                    Ok(list) => {
                        for de in &list {
                            if de.name == "." || de.name == ".." { continue; }
                            let mut entry = Entry::from_dir_entry(de);
                            if !entry.is_dir && entry.name.ends_with(".atxf") {
                                let full_path = if path.ends_with('/') {
                                    format!("{}{}", path, entry.name)
                                } else {
                                    format!("{}/{}", path, entry.name)
                                };
                                if let Ok(atxf_bytes) = atom_fs::read_file(&full_path) {
                                    if let Some(icon_svg) = Self::extract_embedded_icon_svg(&atxf_bytes) {
                                        entry.has_embedded_icon = true;
                                        if let Some(c) = Self::extract_icon_color(icon_svg) {
                                            entry.icon_color_override = Some(c);
                                        }
                                        entry.svg_bitmap = svg::SvgBitmap::render(
                                            icon_svg, ICON_W, ICON_H,
                                        );
                                    }
                                }
                            }
                            self.entries.push(entry);
                        }
                        let msg = format!("{} items", self.entries.len());
                        self.status.set(&msg);
                    }
                    Err(_) => { self.status.set("error: cannot list directory"); }
                }
            }
            Err(_) => { self.status.set("error: cannot open directory"); }
        }
        self.needs_redraw = true;
    }

    fn navigate_to(&mut self, path: String) {
        let old = self.cwd.clone();
        self.cwd = path.clone();
        self.load_dir(&path);
        self.history.push(old);
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
        let old = self.cwd.clone();
        self.history.push(old);
        self.cwd = parent.clone();
        self.load_dir(&parent);
    }

    // ─── Hit testing ─────────────────────────────────────────────────────────

    fn item_at_pos(&self, mx: i32, my: i32) -> Option<usize> {
        let cy = self.content_y() as i32;
        let ch = self.content_h();
        if my < cy || my >= (cy + ch as i32) { return None; }

        match self.view_mode {
            ViewMode::Icons => {
                let cols = (self.surface_width / ICON_CELL_W).max(1);
                let rx   = mx as u32;
                let ry   = (my - cy) as u32 + self.scroll_offset * ICON_CELL_H;
                let col  = rx / ICON_CELL_W;
                let row  = ry / ICON_CELL_H;
                if col >= cols { return None; }
                let idx = (row * cols + col) as usize;
                if idx < self.entries.len() { Some(idx) } else { None }
            }
            ViewMode::List => {
                let hdr_bot = cy + LIST_HDR_H as i32;
                if my < hdr_bot { return None; }
                let ry   = (my - hdr_bot) as u32;
                let idx  = (ry / LIST_ROW_H + self.scroll_offset) as usize;
                if idx < self.entries.len() { Some(idx) } else { None }
            }
        }
    }

    fn visible_list_rows(&self) -> u32 {
        self.content_h().saturating_sub(LIST_HDR_H) / LIST_ROW_H
    }

    // ─── Open / activate item ────────────────────────────────────────────────

    fn activate(&mut self, idx: usize) {
        if idx >= self.entries.len() { return; }
        if self.entries[idx].is_dir {
            let new_path = if self.cwd.ends_with('/') {
                format!("{}{}", self.cwd, self.entries[idx].name)
            } else {
                format!("{}/{}", self.cwd, self.entries[idx].name)
            };
            self.navigate_to(new_path);
        } else {
            let name = &self.entries[idx].name;
            if name.ends_with(".atxf") {
                // Delegate to app_launcher service via IPC
                let path = self.full_path(idx);
                self.launch_atxf(&path);
            } else {
                // Non-executable: show info in status bar
                let msg = format!("open: {} ({})",
                    self.entries[idx].name, self.entries[idx].size_str());
                self.status.set(&msg);
                self.needs_redraw = true;
            }
        }
    }

    // ─── Launch an ATXF application via app_launcher ────────────────────────
    //
    // Security: the file manager does NOT call SYS_SPAWN_FROM_PATH directly.
    // Instead it delegates to `request_app_launch`, which handles all IPC
    // mechanics, and then maps the result to a status-bar message.

    fn launch_atxf(&mut self, path: &str) {
        log("fileman: launch_atxf request");

        // Update status to show progress before the blocking IPC call.
        let msg = format!("launching {}…", path);
        self.status.set(&msg);
        self.needs_redraw = true;

        match self.request_app_launch(path) {
            Ok(reply) => {
                if reply.status == launch_status::LAUNCH_OK {
                    let msg = format!("launched (pid={})", reply.pid);
                    self.status.set(&msg);
                } else {
                    let err_text = reply.err_msg_str();
                    let msg = if err_text.is_empty() {
                        format!("launch error (code={})", reply.status)
                    } else {
                        format!("error: {}", err_text)
                    };
                    self.status.set(&msg);
                    let log_msg = format!("fileman: launch failed — {}", err_text);
                    log(&log_msg);
                }
            }
            Err(LaunchError::NoLauncher) => {
                self.status.set("error: app_launcher not available");
                log("fileman: app_launcher not found in name service");
            }
            Err(LaunchError::PathTooLong) => {
                let msg = format!("error: path too long ({})", path);
                self.status.set(&msg);
            }
            Err(LaunchError::SendFailed) => {
                self.status.set("error: could not contact app_launcher");
                log("fileman: IPC send to app_launcher failed");
            }
            Err(LaunchError::IpcError) => {
                self.status.set("error: IPC error waiting for launch reply");
            }
            Err(LaunchError::Timeout) => {
                self.status.set("error: app_launcher timed out");
                log("fileman: timed out waiting for AppLaunchReply");
            }
            Err(LaunchError::TruncatedReply) => {
                self.status.set("error: truncated launch reply");
            }
            Err(LaunchError::MalformedReply) => {
                self.status.set("error: malformed launch reply");
            }
        }
        self.needs_redraw = true;
    }

    // ─── IPC helper: send a launch request, wait for the reply ──────────────
    //
    // A dedicated one-shot reply port is created for each request so that
    // unrelated messages arriving on `self.local_port` (input events, window
    // manager notifications, etc.) are never silently discarded.
    //
    // The caller (`launch_atxf`) is kept as a thin UI wrapper that only maps
    // the result to status-bar text.

    fn request_app_launch(&self, path: &str) -> Result<AppLaunchReplyMsg, LaunchError> {
        // ── 1. Resolve the app_launcher port ────────────────────────────────
        let launcher_port = libipc::protocol::lookup_service("app_launcher")
            .map_err(|_| LaunchError::NoLauncher)?;

        // ── 2. Create a dedicated one-shot reply port ────────────────────────
        //
        // Using a fresh port means only the AppLaunchReply will ever be
        // delivered here; no other IPC traffic can interleave and be lost.
        let reply_port = create_port().map_err(|_| LaunchError::IpcError)?;

        // ── 3. Build and send the request ───────────────────────────────────
        let req = AppLaunchRequestMsg::new(reply_port, path)
            .ok_or(LaunchError::PathTooLong)?;

        let hdr = MessageHeader::new(
            MessageType::AppLaunchRequest,
            AppLaunchRequestMsg::SIZE as u32,
        );
        let mut buf = [0u8; MessageHeader::SIZE + AppLaunchRequestMsg::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        buf[MessageHeader::SIZE..].copy_from_slice(&req.to_bytes());

        if send(launcher_port, &buf).is_err() {
            let _ = close_port(reply_port);
            return Err(LaunchError::SendFailed);
        }

        // ── 4. Poll the reply port until we get the response or time out ────
        let deadline = get_ticks() + 200; // 200 × 10 ms = 2 s
        let mut reply_buf = [0u8; MessageHeader::SIZE + AppLaunchReplyMsg::SIZE + 16];
        let result = loop {
            match try_recv(reply_port, &mut reply_buf) {
                Ok(Some(len)) if len >= MessageHeader::SIZE + AppLaunchReplyMsg::SIZE => {
                    let payload_start = MessageHeader::SIZE;
                    match AppLaunchReplyMsg::from_bytes(&reply_buf[payload_start..]) {
                        Some(reply) => break Ok(reply),
                        None => break Err(LaunchError::MalformedReply),
                    }
                }
                Ok(Some(_)) => break Err(LaunchError::TruncatedReply),
                Ok(None) => { /* no message yet — keep polling */ }
                Err(_) => break Err(LaunchError::IpcError),
            }
            if get_ticks() >= deadline {
                break Err(LaunchError::Timeout);
            }
            yield_now();
        };

        // ── 5. Clean up the one-shot port regardless of outcome ─────────────
        let _ = close_port(reply_port);
        result
    }

    // ─── File operations ─────────────────────────────────────────────────────

    fn delete_selected(&mut self) {
        let Some(idx) = self.selected else { return; };
        if idx >= self.entries.len() { return; }
        let path = self.full_path(idx);
        let result = if self.entries[idx].is_dir {
            FsOps::rm_recursive(&path)
        } else {
            FsOps::unlink(&path)
        };
        match result {
            Ok(()) => {
                let msg = format!("deleted: {}", self.entries[idx].name);
                self.status.set(&msg);
                let cwd = self.cwd.clone();
                self.load_dir(&cwd);
            }
            Err(_) => { self.status.set("error: delete failed"); }
        }
    }

    fn copy_selected(&mut self) {
        let Some(idx) = self.selected else {
            self.status.set("nothing selected");
            return;
        };
        let path = self.full_path(idx);
        let msg = format!("copied: {}", self.entries[idx].name);
        self.clipboard = Some(Clipboard { path, cut: false });
        self.status.set(&msg);
        self.needs_redraw = true;
    }

    fn cut_selected(&mut self) {
        let Some(idx) = self.selected else {
            self.status.set("nothing selected");
            return;
        };
        let path = self.full_path(idx);
        let msg = format!("cut: {}", self.entries[idx].name);
        self.clipboard = Some(Clipboard { path, cut: true });
        self.status.set(&msg);
        self.needs_redraw = true;
    }

    fn paste(&mut self) {
        let Some(ref cb) = self.clipboard else {
            self.status.set("clipboard empty");
            return;
        };
        let src  = cb.path.clone();
        let is_cut = cb.cut;
        let fname = src.rfind('/').map(|p| &src[p+1..]).unwrap_or(&src);
        let dst = if self.cwd.ends_with('/') {
            format!("{}{}", self.cwd, fname)
        } else {
            format!("{}/{}", self.cwd, fname)
        };

        let result = if is_cut {
            FsOps::rename(&src, &dst)
        } else {
            FsOps::copy(&src, &dst)
        };

        match result {
            Ok(()) => {
                let msg = format!("pasted: {}", fname);
                self.status.set(&msg);
                if is_cut { self.clipboard = None; }
                let cwd = self.cwd.clone();
                self.load_dir(&cwd);
            }
            Err(_) => { self.status.set("error: paste failed"); }
        }
    }

    fn new_folder(&mut self) {
        let path = if self.cwd.ends_with('/') {
            format!("{}new_folder", self.cwd)
        } else {
            format!("{}/new_folder", self.cwd)
        };
        match FsOps::mkdir(&path) {
            Ok(()) => {
                self.status.set("created: new_folder");
                let cwd = self.cwd.clone();
                self.load_dir(&cwd);
            }
            Err(_) => { self.status.set("error: could not create folder"); }
        }
    }

    fn full_path(&self, idx: usize) -> String {
        if self.cwd.ends_with('/') {
            format!("{}{}", self.cwd, self.entries[idx].name)
        } else {
            format!("{}/{}", self.cwd, self.entries[idx].name)
        }
    }

    // ─── Scroll helpers ──────────────────────────────────────────────────────

    fn scroll_down(&mut self) {
        let max = self.max_scroll();
        if self.scroll_offset < max { self.scroll_offset += 1; self.needs_redraw = true; }
    }

    fn scroll_up(&mut self) {
        if self.scroll_offset > 0 { self.scroll_offset -= 1; self.needs_redraw = true; }
    }

    fn max_scroll(&self) -> u32 {
        match self.view_mode {
            ViewMode::Icons => {
                let cols = (self.surface_width / ICON_CELL_W).max(1);
                let rows = (self.entries.len() as u32 + cols - 1) / cols;
                let vis  = self.content_h() / ICON_CELL_H;
                rows.saturating_sub(vis)
            }
            ViewMode::List => {
                let vis = self.visible_list_rows();
                (self.entries.len() as u32).saturating_sub(vis)
            }
        }
    }

    // ─── Rendering ───────────────────────────────────────────────────────────

    fn render(&mut self) {
        if !self.needs_redraw { return; }
        self.needs_redraw = false;

        let surface = match self.surface { Some(ref s) => s, None => return };

        // Full background
        surface.fill_rect(0, 0, self.surface_width, self.surface_height, Theme::BG);

        // Toolbar
        self.draw_toolbar(surface);

        // File area
        match self.view_mode {
            ViewMode::Icons => self.draw_icons(surface),
            ViewMode::List  => self.draw_list(surface),
        }

        // Status bar
        self.draw_status(surface);
    }

    fn draw_toolbar(&self, surface: &SharedSurface) {
        let sw = self.surface_width;
        surface.fill_rect(0, 0, sw, TOOLBAR_H, Theme::TOOLBAR_BG);
        // Bottom separator
        surface.fill_rect(0, TOOLBAR_H - 1, sw, 1, Theme::SEPARATOR);

        // Back button
        self.draw_btn(surface, &BTN_BACK, "<-",
            !self.history.is_empty());

        // Up button
        let can_up = self.cwd != "/";
        self.draw_btn(surface, &BTN_UP, "^", can_up);

        // Right buttons
        let b_ref  = btn_refresh(sw);
        let b_ico  = btn_icons(sw);
        let b_lst  = btn_list(sw);
        let b_new  = btn_new(sw);
        self.draw_btn(surface, &b_ref, "R",   true);
        self.draw_btn_active(surface, &b_ico, "ICONS",
            self.view_mode == ViewMode::Icons);
        self.draw_btn_active(surface, &b_lst, "LIST",
            self.view_mode == ViewMode::List);
        self.draw_btn(surface, &b_new, "+",   true);

        // Path bar (rounded)
        let (pbx, pbw) = path_bar_rect(sw);
        surface.fill_rect_rounded_aa(pbx, TB_BTN_Y, pbw, TB_BTN_H, 4,
            Color::new(12, 14, 20));
        // Path text
        let path = &self.cwd;
        let max_chars = ((pbw.saturating_sub(8)) / CHAR_W) as usize;
        let display = if path.len() > max_chars {
            &path[path.len() - max_chars..]
        } else {
            path.as_str()
        };
        surface.draw_string(pbx + 4, TB_BTN_Y + (TB_BTN_H - CHAR_H) / 2,
            display, Theme::TEXT_PATH, Color::new(12, 14, 20));
    }

    fn draw_btn(&self, surface: &SharedSurface, btn: &Btn, label: &str, _enabled: bool) {
        surface.fill_rect_rounded_aa(btn.x, TB_BTN_Y, btn.w, TB_BTN_H, 4, Theme::BTN_BG);
        let tx = btn.x + (btn.w.saturating_sub(label.len() as u32 * CHAR_W)) / 2;
        let ty = TB_BTN_Y + (TB_BTN_H - CHAR_H) / 2;
        surface.draw_string(tx, ty, label, Theme::TEXT, Theme::BTN_BG);
    }

    fn draw_btn_active(&self, surface: &SharedSurface, btn: &Btn, label: &str, active: bool) {
        let bg = if active { Theme::BTN_ACTIVE } else { Theme::BTN_BG };
        let fg = if active { Theme::TEXT_ACCENT } else { Theme::TEXT };
        surface.fill_rect_rounded_aa(btn.x, TB_BTN_Y, btn.w, TB_BTN_H, 4, bg);
        let tx = btn.x + (btn.w.saturating_sub(label.len() as u32 * CHAR_W)) / 2;
        let ty = TB_BTN_Y + (TB_BTN_H - CHAR_H) / 2;
        surface.draw_string(tx, ty, label, fg, bg);
    }

    fn draw_icons(&self, surface: &SharedSurface) {
        let sw   = self.surface_width;
        let cols = (sw / ICON_CELL_W).max(1);
        let cy   = self.content_y();
        let ch   = self.content_h();

        // How many rows fit on screen?
        let visible_rows = ch / ICON_CELL_H;

        for (i, entry) in self.entries.iter().enumerate() {
            let row = i as u32 / cols;
            // Only render visible rows
            if row < self.scroll_offset { continue; }
            let vis_row = row - self.scroll_offset;
            if vis_row >= visible_rows { break; }

            let col = i as u32 % cols;
            let cx  = col * ICON_CELL_W;
            let cell_y = cy + vis_row * ICON_CELL_H;

            // Cell background
            let is_sel = self.selected == Some(i);
            let cell_bg = if is_sel { Theme::SEL_BG } else { Theme::BG };
            if is_sel {
                surface.fill_rect_rounded_aa(cx + 2, cell_y + 2, ICON_CELL_W - 4, ICON_CELL_H - 4, 6, cell_bg);
            }

            // Icon box (rounded, centred horizontally in cell)
            let icon_x = cx + (ICON_CELL_W - ICON_W) / 2;
            let icon_y = cell_y + 6;

            if let Some(ref bm) = entry.svg_bitmap {
                // Draw neutral dark background, then blit the SVG on top
                surface.fill_rect_rounded_aa(icon_x, icon_y, ICON_W, ICON_H, 8,
                    Color::new(20, 22, 30));
                bm.blit_surface(surface, icon_x, icon_y);
            } else {
                // Fallback: coloured rect + type label
                surface.fill_rect_rounded_aa(icon_x, icon_y, ICON_W, ICON_H, 8, entry.icon_color());
                let lbl   = entry.icon_label();
                let lbl_w = lbl.len() as u32 * CHAR_W;
                let lbl_x = icon_x + (ICON_W - lbl_w) / 2;
                let lbl_y = icon_y + (ICON_H - CHAR_H) / 2;
                surface.draw_string(lbl_x, lbl_y, lbl,
                    Color::new(255, 255, 255), entry.icon_color());
            }

            // File name below icon (max ~10 chars, truncated)
            let max_name = (ICON_CELL_W / CHAR_W).saturating_sub(2) as usize;
            let name = if entry.name.len() > max_name {
                &entry.name[..max_name]
            } else {
                &entry.name
            };
            let name_w = name.len() as u32 * CHAR_W;
            let name_x = cx + (ICON_CELL_W.saturating_sub(name_w)) / 2;
            let name_y = icon_y + ICON_H + 4;
            let name_bg = if is_sel { cell_bg } else { Theme::BG };
            surface.draw_string(name_x, name_y, name, Theme::TEXT, name_bg);
        }

        // Empty directory label
        if self.entries.is_empty() {
            let msg = "Empty directory";
            let mx  = (sw.saturating_sub(msg.len() as u32 * CHAR_W)) / 2;
            let my  = cy + ch / 2;
            surface.draw_string(mx, my, msg, Theme::TEXT_DIM, Theme::BG);
        }
    }

    fn draw_list(&self, surface: &SharedSurface) {
        let sw  = self.surface_width;
        let cy  = self.content_y();

        // Column X positions
        let col_icon_x = 4u32;
        let col_name_x = col_icon_x + 48;
        let col_size_x = sw.saturating_sub(160);
        let col_ext_x  = sw.saturating_sub(80);
        let col_date_x = sw.saturating_sub(40);   // placeholder

        // Header row
        surface.fill_rect(0, cy, sw, LIST_HDR_H, Theme::LIST_HDR_BG);
        surface.fill_rect(0, cy + LIST_HDR_H - 1, sw, 1, Theme::SEPARATOR);
        surface.draw_string(col_icon_x, cy + (LIST_HDR_H - CHAR_H) / 2,
            "Type", Theme::TEXT_ACCENT, Theme::LIST_HDR_BG);
        surface.draw_string(col_name_x, cy + (LIST_HDR_H - CHAR_H) / 2,
            "Name", Theme::TEXT_ACCENT, Theme::LIST_HDR_BG);
        surface.draw_string(col_size_x, cy + (LIST_HDR_H - CHAR_H) / 2,
            "Size", Theme::TEXT_ACCENT, Theme::LIST_HDR_BG);
        surface.draw_string(col_ext_x,  cy + (LIST_HDR_H - CHAR_H) / 2,
            "Ext",  Theme::TEXT_ACCENT, Theme::LIST_HDR_BG);

        let list_y  = cy + LIST_HDR_H;
        let vis     = self.visible_list_rows();

        for (vi, ei) in (self.scroll_offset as usize ..).enumerate() {
            if vi as u32 >= vis || ei >= self.entries.len() { break; }
            let entry  = &self.entries[ei];
            let row_y  = list_y + vi as u32 * LIST_ROW_H;

            let is_sel = self.selected == Some(ei);
            let row_bg = if is_sel { Theme::SEL_BG }
                else if vi % 2 == 0 { Theme::BG }
                else { Color::new(24, 27, 36) };

            surface.fill_rect(0, row_y, sw, LIST_ROW_H, row_bg);

            // Type colour chip (rounded)
            let chip_h = LIST_ROW_H - 6;
            if let Some(ref bm) = entry.svg_bitmap {
                // SVG icon scaled to fit the chip area (36 × chip_h)
                surface.fill_rect_rounded_aa(col_icon_x, row_y + 3, 36, chip_h, 3,
                    Color::new(20, 22, 30));
                // Center the bitmap in the chip (it's rendered at ICON_W×ICON_H,
                // so scale it down by drawing only the top-left corner that fits)
                let draw_w = bm.width.min(36);
                let draw_h = bm.height.min(chip_h);
                for py in 0..draw_h {
                    for px in 0..draw_w {
                        // Sample bitmap pixel at scaled position
                        let sx = px * bm.width / draw_w;
                        let sy = py * bm.height / draw_h;
                        let p = bm.pixels[(sy * bm.width + sx) as usize];
                        if p >> 24 >= 128 {
                            surface.draw_pixel(
                                col_icon_x + px,
                                row_y + 3 + py,
                                Color::new((p >> 16) as u8, (p >> 8) as u8, p as u8),
                            );
                        }
                    }
                }
            } else {
                surface.fill_rect_rounded_aa(col_icon_x, row_y + 3, 36, chip_h, 3,
                    entry.icon_color());
                let lbl   = entry.icon_label();
                let lbl_x = col_icon_x + (36u32.saturating_sub(lbl.len() as u32 * CHAR_W)) / 2;
                surface.draw_string(lbl_x, row_y + (LIST_ROW_H - CHAR_H) / 2,
                    lbl, Color::new(255, 255, 255), entry.icon_color());
            }

            // Name (truncate to fit)
            let max_name = ((col_size_x - col_name_x).saturating_sub(8) / CHAR_W) as usize;
            let name = if entry.name.len() > max_name {
                &entry.name[..max_name]
            } else {
                &entry.name
            };
            let name_fg = if entry.is_dir { Theme::DIR } else { Theme::TEXT };
            surface.draw_string(col_name_x, row_y + (LIST_ROW_H - CHAR_H) / 2,
                name, name_fg, row_bg);

            // Size
            let sz = entry.size_str();
            surface.draw_string(col_size_x, row_y + (LIST_ROW_H - CHAR_H) / 2,
                &sz, Theme::TEXT_DIM, row_bg);

            // Extension
            let ext = entry.ext();
            if !ext.is_empty() {
                surface.draw_string(col_ext_x, row_y + (LIST_ROW_H - CHAR_H) / 2,
                    ext, Theme::TEXT_DIM, row_bg);
            }
        }

        // Empty label
        if self.entries.is_empty() {
            let msg = "Empty directory";
            let mx  = (sw.saturating_sub(msg.len() as u32 * CHAR_W)) / 2;
            surface.draw_string(mx, list_y + 20, msg, Theme::TEXT_DIM, Theme::BG);
        }
    }

    fn draw_status(&self, surface: &SharedSurface) {
        let sw = self.surface_width;
        let sy = self.status_y();
        surface.fill_rect(0, sy, sw, STATUS_H, Theme::STATUS_BG);
        surface.fill_rect(0, sy, sw, 1, Theme::SEPARATOR);

        // Left: item count / selected
        let left_msg = if let Some(idx) = self.selected {
            if idx < self.entries.len() {
                let e = &self.entries[idx];
                if e.is_dir {
                    format!("{} items  |  {} [dir]", self.entries.len(), e.name)
                } else {
                    format!("{} items  |  {}  {}  .{}",
                        self.entries.len(), e.name, e.size_str(), e.ext())
                }
            } else {
                format!("{} items", self.entries.len())
            }
        } else {
            format!("{} items", self.entries.len())
        };

        surface.draw_string(8, sy + (STATUS_H - CHAR_H) / 2,
            &left_msg, Theme::TEXT_DIM, Theme::STATUS_BG);

        // Right: status message (copy/paste/error etc.)
        let s = self.status.as_str();
        if !s.is_empty() {
            let tx = sw.saturating_sub(s.len() as u32 * CHAR_W + 8);
            surface.draw_string(tx, sy + (STATUS_H - CHAR_H) / 2,
                s, Theme::TEXT_ACCENT, Theme::STATUS_BG);
        }

        // Clipboard indicator
        if let Some(ref cb) = self.clipboard {
            let marker = if cb.cut { "[cut]" } else { "[copied]" };
            let mx = sw / 2 - marker.len() as u32 * CHAR_W / 2;
            surface.draw_string(mx, sy + (STATUS_H - CHAR_H) / 2,
                marker, Theme::TEXT_ACCENT, Theme::STATUS_BG);
        }
    }

    // ─── IPC: notify compositor ───────────────────────────────────────────────

    fn present(&self) {
        let msg   = SurfacePresentMsg { window_id: self.window_id };
        let hdr   = MessageHeader::new(MessageType::SurfacePresent,
                        SurfacePresentMsg::SIZE as u32);
        let mut buf = [0u8; MessageHeader::SIZE + SurfacePresentMsg::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        buf[MessageHeader::SIZE..].copy_from_slice(&msg.to_bytes());
        let _ = send(self.compositor_port, &buf);
    }

    // ─── Event handlers ───────────────────────────────────────────────────────

    fn handle_mouse_move(&mut self, ev: &MouseMoveEvent) {
        self.mouse_x = ev.x;
        self.mouse_y = ev.y;
    }

    fn handle_mouse_button_down(&mut self, ev: &MouseButtonEvent) {
        if ev.button != MouseButton::Left { return; }
        let mx = ev.x;
        let my = ev.y;
        let sw = self.surface_width;

        // Toolbar button hit test
        if my >= 0 && my < TOOLBAR_H as i32 {
            if BTN_BACK.contains(mx, my) { self.go_back(); return; }
            if BTN_UP.contains(mx, my)   { self.go_up();   return; }
            if btn_icons(sw).contains(mx, my) {
                self.view_mode = ViewMode::Icons;
                self.scroll_offset = 0;
                self.needs_redraw = true; return;
            }
            if btn_list(sw).contains(mx, my) {
                self.view_mode = ViewMode::List;
                self.scroll_offset = 0;
                self.needs_redraw = true; return;
            }
            if btn_refresh(sw).contains(mx, my) {
                let cwd = self.cwd.clone();
                self.load_dir(&cwd); return;
            }
            if btn_new(sw).contains(mx, my) {
                self.new_folder(); return;
            }
            return; // click on toolbar but no button
        }

        // File area hit test
        if let Some(idx) = self.item_at_pos(mx, my) {
            let now   = get_ticks();
            let dbl   = self.last_click_idx == idx as i32
                     && now.saturating_sub(self.last_click_tick) < 40; // ~400ms

            self.selected        = Some(idx);
            self.last_click_idx  = idx as i32;
            self.last_click_tick = now;
            self.needs_redraw    = true;

            if dbl { self.activate(idx); }
            else {
                // Single click: show info
                let e = &self.entries[idx];
                if e.is_dir {
                    let msg = format!("{} [directory]", e.name);
                    self.status.set(&msg);
                } else {
                    let msg = format!("{}  {}  .{}", e.name, e.size_str(), e.ext());
                    self.status.set(&msg);
                }
                self.needs_redraw = true;
            }
        } else {
            // Click on empty area → deselect
            self.selected = None;
            self.last_click_idx = -1;
            self.needs_redraw = true;
        }
    }

    fn handle_scroll(&mut self, ev: &MouseScrollEvent) {
        if ev.dz < 0 { self.scroll_down(); }
        else          { self.scroll_up();   }
    }

    fn handle_key(&mut self, ev: &IpcKeyEvent) {
        let ch  = ev.character;
        let ctrl = ev.modifiers.ctrl;

        match ch {
            // Ctrl shortcuts
            _ if ctrl => match ch {
                b'c' | b'C' => self.copy_selected(),
                b'x' | b'X' => self.cut_selected(),
                b'v' | b'V' => self.paste(),
                b'r' | b'R' | b'f' | b'F' => {
                    let cwd = self.cwd.clone();
                    self.load_dir(&cwd);
                }
                _ => {}
            },
            // Backspace = go up
            0x08 => self.go_up(),
            // Delete = delete selected
            0x7F | 0xFF => self.delete_selected(),
            // Enter = open/activate
            b'\n' | b'\r' => {
                if let Some(idx) = self.selected { self.activate(idx); }
            }
            // Escape = deselect
            0x1B => {
                self.selected = None;
                self.needs_redraw = true;
            }
            // Arrow keys (sent as special scancodes; check scancode)
            _ => {
                // Arrow key scancodes from the compositor
                match ev.scancode {
                    0x48 => { // Up arrow
                        self.scroll_up();
                        // Or move selection up
                        if let Some(sel) = self.selected {
                            if sel > 0 {
                                self.selected = Some(sel - 1);
                                self.needs_redraw = true;
                            }
                        }
                    }
                    0x50 => { // Down arrow
                        self.scroll_down();
                        // Or move selection down
                        if let Some(sel) = self.selected {
                            if sel + 1 < self.entries.len() {
                                self.selected = Some(sel + 1);
                                self.needs_redraw = true;
                            }
                        } else if !self.entries.is_empty() {
                            self.selected = Some(0);
                            self.needs_redraw = true;
                        }
                    }
                    0x4B => { // Left arrow – go back
                        self.go_back();
                    }
                    0x4D => { // Right arrow – enter if dir
                        if let Some(idx) = self.selected {
                            if idx < self.entries.len() && self.entries[idx].is_dir {
                                self.activate(idx);
                            }
                        }
                    }
                    0x49 => self.scroll_up(),   // Page Up
                    0x51 => self.scroll_down(),  // Page Down
                    _ => {}
                }
            }
        }
    }

    // ─── IPC message dispatch ─────────────────────────────────────────────────

    fn process_message(&mut self, buf: &[u8], len: usize) {
        if len < MessageHeader::SIZE { return; }
        let Some(hdr) = MessageHeader::from_bytes(buf) else { return };

        match hdr.msg_type {
            MessageType::TerminateRequest => {
                self.running = false;
            }
            MessageType::SurfaceAssign => {
                let p = MessageHeader::SIZE;
                if let Some(msg) = SurfaceAssignMsg::from_bytes(&buf[p..]) {
                    if let Ok(s) = SharedSurface::from_region(
                            msg.region_id, msg.width, msg.height) {
                        self.surface_width  = msg.width;
                        self.surface_height = msg.height;
                        self.compositor_port = msg.compositor_port as u64;
                        self.surface = Some(s);
                        self.needs_redraw = true;
                    }
                }
            }
            MessageType::KeyPress => {
                let p = MessageHeader::SIZE;
                if len >= p + 3 {
                    if let Some(ev) = IpcKeyEvent::from_bytes(&buf[p..]) {
                        self.handle_key(&ev);
                    }
                }
            }
            MessageType::MouseMove => {
                let p = MessageHeader::SIZE;
                if let Some(ev) = MouseMoveEvent::from_bytes(&buf[p..]) {
                    self.handle_mouse_move(&ev);
                }
            }
            MessageType::MouseButtonDown => {
                let p = MessageHeader::SIZE;
                if let Some(ev) = MouseButtonEvent::from_bytes(&buf[p..]) {
                    self.handle_mouse_button_down(&ev);
                }
            }
            MessageType::MouseScroll => {
                let p = MessageHeader::SIZE;
                if let Some(ev) = MouseScrollEvent::from_bytes(&buf[p..]) {
                    self.handle_scroll(&ev);
                }
            }
            _ => {}
        }
    }

    // ─── Surface assignment (wait on startup) ─────────────────────────────────

    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buf   = [0u8; 64];
        let ports = [port];
        for _ in 0..100 {
            if wait_any(&ports, 100).is_ok() {
                if let Ok(Some(len)) = try_recv(port, &mut buf) {
                    if len >= MessageHeader::SIZE {
                        if let Some(hdr) = MessageHeader::from_bytes(&buf) {
                            if hdr.msg_type == MessageType::SurfaceAssign {
                                let p = MessageHeader::SIZE;
                                if len >= p + SurfaceAssignMsg::SIZE {
                                    return SurfaceAssignMsg::from_bytes(&buf[p..]);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }

    // ─── Main loop ────────────────────────────────────────────────────────────

    fn run(&mut self) {
        log("fileman: entering main loop");

        // Load root directory on startup
        self.load_dir("/");

        let mut msg_buf = [0u8; 256];
        let ports = [self.local_port];

        while self.running {
            // Drain incoming IPC messages
            while let Ok(Some(len)) = try_recv(self.local_port, &mut msg_buf) {
                self.process_message(&msg_buf, len);
            }

            // Render if dirty
            if self.needs_redraw {
                self.render();
                self.present();
            }

            // Block until next event (100ms timeout)
            let _ = wait_any(&ports, 100);
        }

        log("fileman: exiting");
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() -> ! {
    log("fileman: starting GUI file manager");

    let local_port = match create_port() {
        Ok(p) => p,
        Err(_) => { log("fileman: failed to create port"); exit(1); }
    };

    // Register service name
    let _ = libipc::protocol::register_service("fileman", local_port);

    // Find compositor registration port
    log("fileman: looking up compositor.register...");
    let register_port = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(p) => break p,
            Err(_) => yield_now(),
        }
    };

    // Send AppRegister
    let mut full_msg = [0u8; 48];
    let hdr = MessageHeader::new(MessageType::AppRegister, 16);
    full_msg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + 8]
        .copy_from_slice(&local_port.to_le_bytes());
    full_msg[MessageHeader::SIZE + 8..MessageHeader::SIZE + 16]
        .copy_from_slice(&0u64.to_le_bytes());

    log("fileman: registering with compositor");
    let _ = send(register_port, &full_msg[..MessageHeader::SIZE + 16]);

    // Wait for surface
    let surf_info = match FileManager::wait_for_surface(local_port) {
        Some(i) => i,
        None => { log("fileman: timeout waiting for surface"); exit(1); }
    };

    log("fileman: surface received");

    let surface = match SharedSurface::from_region(
            surf_info.region_id, surf_info.width, surf_info.height) {
        Ok(s) => s,
        Err(_) => { log("fileman: failed to map surface"); exit(1); }
    };

    let mut fm = FileManager::new(
        surf_info.window_id,
        surf_info.compositor_port as u64,
        local_port,
        surface,
    );

    fm.run();
    exit(0);
}
