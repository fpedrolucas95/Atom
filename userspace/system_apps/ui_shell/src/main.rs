//! Atom Desktop Environment alpha

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

struct BumpAllocator {
    start: AtomicUsize,
    size: AtomicUsize,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }

    fn init(&self, start: usize, size: usize) {
        self.start.store(start, Ordering::SeqCst);
        self.size.store(size, Ordering::SeqCst);
        self.next.store(0, Ordering::SeqCst);
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);

        let heap_start = self.start.load(Ordering::Relaxed);
        let heap_size = self.size.load(Ordering::Relaxed);

        if heap_start == 0 || heap_size == 0 {
            return core::ptr::null_mut();
        }

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > heap_size {
                return core::ptr::null_mut();
            }

            if self.next.compare_exchange_weak(
                current, new_next, Ordering::SeqCst, Ordering::Relaxed
            ).is_ok() {
                return (heap_start as *mut u8).add(aligned);
            }
        }
    }


    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // LIFO optimisation: reclaim the most-recently-allocated block so
        // transient Vec allocations (IPC buffers etc.) don't exhaust the heap.
        let heap_start = self.start.load(Ordering::Relaxed);
        let blk_offset = ptr as usize - heap_start;
        let blk_end = blk_offset + layout.size();

        let _ = self.next.compare_exchange(
            blk_end, blk_offset, Ordering::SeqCst, Ordering::Relaxed,
        );
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(layout: Layout) -> ! {
    log("atom_desktop: allocation failure");
    let _ = layout;
    loop {}
}

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use atom_syscall::graphics::{Color, Framebuffer, SharedSurface, SharedRegionId, SharedMemFlags, shared_region_create, shared_region_map, get_framebuffer};
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::interrupts::register_irq_handler;
use atom_syscall::thread::{exit, yield_now};
use atom_syscall::debug::log;
use atom_syscall::process::{spawn_process, spawn_from_path};
use atom_syscall::input::{MouseDriver, keyboard_poll, scancode_to_ascii, scancodes};
use atom_syscall::fs;
use atom_syscall::SyscallError;

use libipc::messages::{MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers, MouseMoveEvent, MouseButtonEvent, MouseButton, OpenInTabMsg, ApplyWallpaperMsg, WallpaperAppliedMsg, WallpaperFailedMsg};
use libipc::protocol::send_message_async;
use libimage::{DecodedImage, ImageDecoder, JpgDecoder, PngDecoder};

/// Shell visual theme — all values sourced from `atom_theme` (DS v1.0 Luminous Dark).
///
/// **No hard-coded RGB values.**  Every constant is an alias or a
/// direct mapping of an `atom_theme::colors` token to the
/// `atom_syscall::graphics::Color` type used by the compositor.
///
/// ## Shell-specific metrics
///
/// Layout numbers (panel height, dock height, etc.) come from
/// `atom_theme::shell`.  The raw `const` values below are kept for
/// backward-compatibility with the rest of the file; they match the DS spec.
mod theme {
    // Import DS token source
    use atom_theme::colors as ds;
    use atom_syscall::graphics::Color;

    // ── Desktop ───────────────────────────────────────────────────────────
    /// Main desktop background (DS: ATOM_COLOR_BG  #0B0E13)
    pub const DESKTOP_BG: Color = ds::ATOM_COLOR_BG;

    // ── Top bar / panel ───────────────────────────────────────────────────
    /// Panel background (DS: ATOM_COLOR_BG — darkest layer)
    pub const PANEL_BG: Color = ds::ATOM_COLOR_BG;
    /// Panel background accent row (DS: ATOM_COLOR_SURFACE)
    pub const PANEL_BG_ACCENT: Color = ds::ATOM_COLOR_SURFACE;
    /// Panel text (DS: ATOM_COLOR_TEXT_PRIMARY)
    pub const PANEL_TEXT: Color = ds::ATOM_COLOR_TEXT_PRIMARY;
    /// Panel dimmed text (DS: ATOM_COLOR_TEXT_SECONDARY)
    pub const PANEL_TEXT_DIM: Color = ds::ATOM_COLOR_TEXT_SECONDARY;
    /// Panel border (DS: ATOM_COLOR_BORDER)
    pub const PANEL_BORDER: Color = ds::ATOM_COLOR_BORDER;

    // ── Accent ────────────────────────────────────────────────────────────
    /// Primary accent blue (DS: ATOM_COLOR_ACCENT  #4C8DFF)
    pub const ACCENT: Color = ds::ATOM_COLOR_ACCENT;

    // ── Windows ───────────────────────────────────────────────────────────
    /// Window content background (DS: ATOM_COLOR_SURFACE_ALT)
    pub const WINDOW_BG: Color = ds::ATOM_COLOR_WIN_BG;
    /// Window title bar — unfocused (DS: ATOM_COLOR_WIN_HDR)
    pub const WINDOW_HEADER: Color = ds::ATOM_COLOR_WIN_HDR;
    /// Window title bar — focused (DS: ATOM_COLOR_WIN_HDR_FOC)
    pub const WINDOW_HEADER_FOCUSED: Color = ds::ATOM_COLOR_WIN_HDR_FOC;
    /// Window border — unfocused (DS: ATOM_COLOR_BORDER)
    pub const WINDOW_BORDER: Color = ds::ATOM_COLOR_WIN_BORDER;
    /// Window border — focused (DS: ATOM_COLOR_WIN_BORDER_FOC)
    pub const WINDOW_BORDER_FOCUSED: Color = ds::ATOM_COLOR_WIN_BORDER_FOC;

    // ── Dock ──────────────────────────────────────────────────────────────
    /// Dock background (DS: ATOM_COLOR_DOCK_BG)
    pub const DOCK_BG: Color = ds::ATOM_COLOR_DOCK_BG;
    /// Dock border (DS: ATOM_COLOR_DOCK_BORDER)
    pub const DOCK_BORDER: Color = ds::ATOM_COLOR_DOCK_BORDER;

    // ── Cursor ────────────────────────────────────────────────────────────
    pub const CURSOR_FILL: Color    = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;

    // ── Shadows ───────────────────────────────────────────────────────────
    /// Shadow base colour (DS: ATOM_COLOR_SHADOW — near-black blue tint)
    pub const SHADOW: Color = ds::ATOM_COLOR_SHADOW;

    // ── Window traffic-light buttons ──────────────────────────────────────
    /// Close button (DS: ATOM_COLOR_ERROR  #EF4444)
    pub const BTN_CLOSE: Color    = ds::ATOM_COLOR_BTN_CLOSE;
    /// Maximise button (DS: ATOM_COLOR_SUCCESS  #22C55E)
    pub const BTN_MAXIMIZE: Color = ds::ATOM_COLOR_BTN_MAX;
    /// Minimise button (DS: ATOM_COLOR_WARNING  #F59E0B)
    pub const BTN_MINIMIZE: Color = ds::ATOM_COLOR_BTN_MIN;
    /// Inactive button (unfocused window)
    pub const BTN_INACTIVE: Color = ds::ATOM_COLOR_BTN_INACTIVE;

    // ── Context menu ─────────────────────────────────────────────────────
    /// Menu panel background (DS: ATOM_COLOR_SURFACE)
    pub const MENU_BG: Color     = ds::ATOM_COLOR_MENU_BG;
    /// Menu border (DS: ATOM_COLOR_BORDER)
    pub const MENU_BORDER: Color = ds::ATOM_COLOR_MENU_BORDER;
    /// Menu text (DS: ATOM_COLOR_TEXT_PRIMARY)
    pub const MENU_TEXT: Color   = ds::ATOM_COLOR_MENU_TEXT;

    // ── Desktop icon labels ───────────────────────────────────────────────
    /// Icon label text (DS: ATOM_COLOR_TEXT_SECONDARY)
    pub const ICON_LABEL: Color = ds::ATOM_COLOR_TEXT_SECONDARY;
}

const WINDOW_HEADER_HEIGHT: u32 = atom_theme::shell::WINDOW_TITLE_HEIGHT; // 36 px
const WINDOW_BORDER_WIDTH: u32  = 1;
const WINDOW_MIN_WIDTH: u32     = 150;
const WINDOW_MIN_HEIGHT: u32    = 100;
const PANEL_HEIGHT: u32         = atom_theme::shell::TOP_BAR_HEIGHT;       // 32 px
const DOCK_HEIGHT: u32          = atom_theme::shell::DOCK_HEIGHT;          // 64 px

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
}

struct Window {
    id: WindowId,
    title: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    state: WindowState,
    visible: bool,
    focused: bool,
    event_port: Option<PortId>,
    surface: Option<SharedSurface>,
    surface_region_id: Option<SharedRegionId>,
    content_dirty: bool,
    /// True once the app has presented at least one frame into the current surface.
    /// Prevents blitting a blank/black surface after resize until the app redraws.
    surface_ready: bool,
    saved_x: i32,
    saved_y: i32,
    saved_width: u32,
    saved_height: u32,
}

impl Window {
    fn new(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            id,
            title: String::from(title),
            x,
            y,
            width: width.max(WINDOW_MIN_WIDTH),
            height: height.max(WINDOW_MIN_HEIGHT),
            state: WindowState::Normal,
            visible: true,
            focused: false,
            event_port: None,
            surface: None,
            surface_region_id: None,
            content_dirty: false,
            surface_ready: false,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        }
    }

    fn new_with_process(
        id: WindowId,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        event_port: PortId,
    ) -> Option<Self> {
        let width = width.max(WINDOW_MIN_WIDTH);
        let height = height.max(WINDOW_MIN_HEIGHT);

        let content_width = width.saturating_sub(WINDOW_BORDER_WIDTH * 2);
        let content_height = height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH);

        let surface = match SharedSurface::create(content_width, content_height) {
            Ok(s) => s,
            Err(_) => return None,
        };

        let region_id = surface.region_id();

        Some(Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            state: WindowState::Normal,
            visible: true,
            focused: false,
            event_port: Some(event_port),
            surface_region_id: Some(region_id),
            surface: Some(surface),
            content_dirty: false,
            surface_ready: false,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        })
    }

    fn content_x(&self) -> u32 {
        (self.x as u32).wrapping_add(WINDOW_BORDER_WIDTH)
    }

    fn content_y(&self) -> u32 {
        (self.y as u32).wrapping_add(WINDOW_HEADER_HEIGHT)
    }

    fn content_width(&self) -> u32 {
        self.width.saturating_sub(WINDOW_BORDER_WIDTH * 2)
    }

    fn content_height(&self) -> u32 {
        self.height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH)
    }

    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + self.height as i32
    }

    fn header_contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + WINDOW_HEADER_HEIGHT as i32
    }
}

struct WindowManager {
    windows: Vec<Window>,
    next_id: WindowId,
    focused_id: Option<WindowId>,
}

impl WindowManager {
    fn new() -> Self {
        Self {
            windows: Vec::new(),
            next_id: 1,
            focused_id: None,
        }
    }

    fn create_window(&mut self, title: &str, x: i32, y: i32, width: u32, height: u32) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new(id, title, x, y, width, height);
        self.windows.push(window);
        self.focus_window(id);
        id
    }

    fn create_window_with_process(
        &mut self,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        event_port: PortId,
    ) -> Option<WindowId> {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new_with_process(id, title, x, y, width, height, event_port)?;
        self.windows.push(window);
        self.focus_window(id);
        Some(id)
    }

    fn get_window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn get_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn focus_window(&mut self, id: WindowId) {
        if let Some(prev_id) = self.focused_id {
            if prev_id == id {
                if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                    let window = self.windows.remove(pos);
                    self.windows.push(window);
                }
                return;
            }
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev_id) {
                w.focused = false;
                if let Some(port) = w.event_port {
                    if port != 0 {
                        let msg = libipc::messages::WmWindowEventMsg {
                            window_id: prev_id,
                            event_type: libipc::messages::WindowEventType::Unfocus,
                            x: w.x, y: w.y, width: w.width, height: w.height,
                        };
                        let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
                        let mut full_msg = [0u8; 64];
                        full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                        full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&msg.to_bytes());
                        let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
                    }
                }
            }
        }

        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.focused = true;
            window.visible = true;
            if window.state == WindowState::Minimized {
                window.state = WindowState::Normal;
            }

            if let Some(port) = window.event_port {
                if port != 0 {
                    let msg = libipc::messages::WmWindowEventMsg {
                        window_id: id,
                        event_type: libipc::messages::WindowEventType::Focus,
                        x: window.x, y: window.y, width: window.width, height: window.height,
                    };
                    let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&msg.to_bytes());
                    let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
                }
            }

            self.windows.push(window);
            self.focused_id = Some(id);
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        for window in self.windows.iter().rev() {
            if window.visible && window.state != WindowState::Minimized && window.contains(x, y) {
                return Some(window.id);
            }
        }
        None
    }

    fn close_window(&mut self, id: WindowId) {
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.iter().filter(|w| w.visible && w.state != WindowState::Minimized).last().map(|w| w.id);
            if let Some(new_focus) = self.focused_id {
                self.focus_window(new_focus);
            }
        }
    }

    fn resize_window(&mut self, id: WindowId, width: u32, height: u32) {
        if let Some(window) = self.get_window_mut(id) {
            if window.state == WindowState::Maximized {
                return;
            }
            let new_width = width.max(WINDOW_MIN_WIDTH);
            let new_height = height.max(WINDOW_MIN_HEIGHT);

            if window.width != new_width || window.height != new_height {
                window.width = new_width;
                window.height = new_height;
                window.content_dirty = true;
            }
        }
    }

    fn toggle_maximize(&mut self, id: WindowId, work_x: i32, work_y: i32, work_w: u32, work_h: u32) -> bool {
        if let Some(window) = self.get_window_mut(id) {
            match window.state {
                WindowState::Maximized => {
                    window.state = WindowState::Normal;
                    window.x = window.saved_x;
                    window.y = window.saved_y;
                    window.width = window.saved_width;
                    window.height = window.saved_height;
                }
                _ => {
                    window.saved_x = window.x;
                    window.saved_y = window.y;
                    window.saved_width = window.width;
                    window.saved_height = window.height;

                    window.state = WindowState::Maximized;
                    window.x = work_x;
                    window.y = work_y;
                    window.width = work_w;
                    window.height = work_h;
                }
            }
            window.content_dirty = true;
            return true;
        }
        false
    }

    fn minimize_window(&mut self, id: WindowId) {
        if let Some(window) = self.get_window_mut(id) {
            window.state = WindowState::Minimized;
            window.focused = false;
        }
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.iter().filter(|w| w.visible && w.state != WindowState::Minimized).last().map(|w| w.id);
            if let Some(new_focus) = self.focused_id {
                self.focus_window(new_focus);
            }
        }
    }
}

struct CursorState {
    x: i32,
    y: i32,
}

impl CursorState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            x: (width / 2) as i32,
            y: (height / 2) as i32,
        }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        self.x = (self.x + dx).clamp(0, (width - 1) as i32);
        self.y = (self.y - dy).clamp(0, (height - 1) as i32);
    }
}

struct PendingWindow {
    window_id: WindowId,
}

/// A window whose close has been requested but whose shared-surface destruction
/// is deferred so the client has time to notice the TerminateRequest and exit
/// cleanly (avoiding page faults from accessing unmapped shared memory).
struct PendingClose {
    window_id: WindowId,
    deadline_tick: u32,
}

/// Grace period (in ~10 ms ticks) before a closing window's shared memory is
/// actually destroyed.  200 ticks ≈ 2 seconds — plenty of time for any client
/// to process the TerminateRequest and unmap its side.
const CLOSE_GRACE_TICKS: u32 = 200;

struct DesktopIcon {
    label: String,
    executable: String,
    x: i32,
    y: i32,
    color: Color,
}

struct DockApp {
    label: String,
    executable: String,
    color: Color,
    monogram: String,
}

struct ContextMenu {
    x: i32,
    y: i32,
    visible: bool,
    items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CurrentWallpaperSource {
    SolidColor { rgb: u32 },
    Image { path: String },
}

// ============================================================================
// Desktop Configuration Structures (Task 1.3)
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallpaperSourceType {
    Image,
    SolidColor,
}

impl WallpaperSourceType {
    fn from_ipc(value: libipc::messages::WallpaperSourceType) -> Self {
        match value {
            libipc::messages::WallpaperSourceType::Image => Self::Image,
            libipc::messages::WallpaperSourceType::SolidColor => Self::SolidColor,
        }
    }

    fn to_str(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::SolidColor => "SolidColor",
        }
    }
    
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Image" => Some(Self::Image),
            "SolidColor" => Some(Self::SolidColor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalingMode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

impl ScalingMode {
    fn from_ipc(value: libipc::messages::ScalingMode) -> Self {
        match value {
            libipc::messages::ScalingMode::Fill => Self::Fill,
            libipc::messages::ScalingMode::Fit => Self::Fit,
            libipc::messages::ScalingMode::Stretch => Self::Stretch,
            libipc::messages::ScalingMode::Center => Self::Center,
            libipc::messages::ScalingMode::Tile => Self::Tile,
        }
    }

    fn to_str(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Fit => "Fit",
            Self::Stretch => "Stretch",
            Self::Center => "Center",
            Self::Tile => "Tile",
        }
    }
    
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Fill" => Some(Self::Fill),
            "Fit" => Some(Self::Fit),
            "Stretch" => Some(Self::Stretch),
            "Center" => Some(Self::Center),
            "Tile" => Some(Self::Tile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WallpaperConfig {
    source_type: WallpaperSourceType,
    image_path: Option<String>,
    color_rgb: Option<u32>,
    scaling_mode: ScalingMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolutionConfig {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopConfig {
    wallpaper: WallpaperConfig,
    resolution: ResolutionConfig,
}

#[derive(Debug)]
enum ParseError {
    InvalidJson,
    MissingField(&'static str),
    InvalidFieldValue(&'static str),
}

#[derive(Debug)]
enum ValidationError {
    InvalidResolution,
    MissingImagePath,
    EmptyImagePath,
    InvalidImageExtension,
    InvalidImagePath,
    MissingColorRgb,
}

impl DesktopConfig {
    fn default_config(width: u32, height: u32) -> Self {
        Self {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(0x12141C),
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig { width, height },
        }
    }

    fn to_json(&self) -> Result<String, ()> {
        let mut json = String::from("{\n");
        
        // Wallpaper section
        json.push_str("  \"wallpaper\": {\n");
        json.push_str("    \"source_type\": \"");
        json.push_str(self.wallpaper.source_type.to_str());
        json.push_str("\",\n");
        
        match self.wallpaper.source_type {
            WallpaperSourceType::Image => {
                if let Some(ref path) = self.wallpaper.image_path {
                    json.push_str("    \"image_path\": \"");
                    // Escape special characters
                    for ch in path.chars() {
                        match ch {
                            '"' => json.push_str("\\\""),
                            '\\' => json.push_str("\\\\"),
                            '\n' => json.push_str("\\n"),
                            '\r' => json.push_str("\\r"),
                            '\t' => json.push_str("\\t"),
                            _ => json.push(ch),
                        }
                    }
                    json.push_str("\",\n");
                }
            }
            WallpaperSourceType::SolidColor => {
                if let Some(rgb) = self.wallpaper.color_rgb {
                    json.push_str("    \"color_rgb\": ");
                    json.push_str(&format_u32(rgb));
                    json.push_str(",\n");
                }
            }
        }
        
        json.push_str("    \"scaling_mode\": \"");
        json.push_str(self.wallpaper.scaling_mode.to_str());
        json.push_str("\"\n");
        json.push_str("  },\n");
        
        // Resolution section
        json.push_str("  \"resolution\": {\n");
        json.push_str("    \"width\": ");
        json.push_str(&format_u32(self.resolution.width));
        json.push_str(",\n");
        json.push_str("    \"height\": ");
        json.push_str(&format_u32(self.resolution.height));
        json.push_str("\n");
        json.push_str("  }\n");
        json.push_str("}");
        
        Ok(json)
    }
    
    fn from_json(json: &str) -> Result<Self, ParseError> {
        // Simple JSON parser for no_std environment
        let json = json.trim();
        
        // Extract wallpaper section
        let wallpaper_start = json.find("\"wallpaper\"")
            .ok_or(ParseError::MissingField("wallpaper"))?;
        let wallpaper_obj_start = json[wallpaper_start..].find('{')
            .ok_or(ParseError::InvalidJson)?;
        
        // Extract source_type
        let source_type_str = extract_string_field(json, "source_type")
            .ok_or(ParseError::MissingField("source_type"))?;
        let source_type = WallpaperSourceType::from_str(&source_type_str)
            .ok_or(ParseError::InvalidFieldValue("source_type"))?;
        
        // Extract scaling_mode
        let scaling_mode_str = extract_string_field(json, "scaling_mode")
            .ok_or(ParseError::MissingField("scaling_mode"))?;
        let scaling_mode = ScalingMode::from_str(&scaling_mode_str)
            .ok_or(ParseError::InvalidFieldValue("scaling_mode"))?;
        
        // Extract conditional fields
        let image_path = match source_type {
            WallpaperSourceType::Image => {
                let path = extract_string_field(json, "image_path")
                    .ok_or(ParseError::MissingField("image_path"))?;
                if path.is_empty() {
                    return Err(ParseError::InvalidFieldValue("image_path"));
                }
                Some(path)
            }
            WallpaperSourceType::SolidColor => None,
        };
        
        let color_rgb = match source_type {
            WallpaperSourceType::SolidColor => {
                let rgb = extract_number_field(json, "color_rgb")
                    .ok_or(ParseError::MissingField("color_rgb"))?;
                Some(rgb)
            }
            WallpaperSourceType::Image => None,
        };
        
        // Extract resolution section
        let width = extract_number_field(json, "width")
            .ok_or(ParseError::MissingField("width"))?;
        let height = extract_number_field(json, "height")
            .ok_or(ParseError::MissingField("height"))?;
        
        let config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type,
                image_path,
                color_rgb,
                scaling_mode,
            },
            resolution: ResolutionConfig {
                width,
                height,
            },
        };
        
        // Validate
        config.validate().map_err(|_| ParseError::InvalidFieldValue("validation"))?;
        
        Ok(config)
    }
    
    fn validate(&self) -> Result<(), ValidationError> {
        // Validate resolution bounds
        if self.resolution.width < 640 || self.resolution.width > 1920 {
            return Err(ValidationError::InvalidResolution);
        }
        if self.resolution.height < 480 || self.resolution.height > 1080 {
            return Err(ValidationError::InvalidResolution);
        }
        
        // Validate wallpaper config consistency
        match self.wallpaper.source_type {
            WallpaperSourceType::Image => {
                if self.wallpaper.image_path.is_none() {
                    return Err(ValidationError::MissingImagePath);
                }
                let path = self.wallpaper.image_path.as_ref().unwrap();
                if path.is_empty() {
                    return Err(ValidationError::EmptyImagePath);
                }
                if !validate_wallpaper_path(path) {
                    let lower = path.to_lowercase();
                    if !lower.ends_with(".jpg") && !lower.ends_with(".jpeg") {
                        return Err(ValidationError::InvalidImageExtension);
                    }
                    return Err(ValidationError::InvalidImagePath);
                }
            }
            WallpaperSourceType::SolidColor => {
                if self.wallpaper.color_rgb.is_none() {
                    return Err(ValidationError::MissingColorRgb);
                }
            }
        }
        
        Ok(())
    }
}

// Helper functions for JSON parsing
fn extract_string_field(json: &str, field_name: &str) -> Option<String> {
    let pattern = alloc::format!("\"{}\"", field_name);
    let field_start = json.find(&pattern)?;
    let after_field = &json[field_start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = after_field[colon_pos + 1..].trim_start();
    
    if !after_colon.starts_with('"') {
        return None;
    }
    
    let value_start = 1;
    let mut value_end = value_start;
    let chars: Vec<char> = after_colon.chars().collect();
    
    while value_end < chars.len() {
        if chars[value_end] == '\\' && value_end + 1 < chars.len() {
            value_end += 2;
            continue;
        }
        if chars[value_end] == '"' {
            break;
        }
        value_end += 1;
    }
    
    if value_end >= chars.len() {
        return None;
    }
    
    let value: String = chars[value_start..value_end].iter().collect();
    Some(value)
}

fn extract_number_field(json: &str, field_name: &str) -> Option<u32> {
    let pattern = alloc::format!("\"{}\"", field_name);
    let field_start = json.find(&pattern)?;
    let after_field = &json[field_start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = after_field[colon_pos + 1..].trim_start();
    
    let mut num_str = String::new();
    for ch in after_colon.chars() {
        if ch.is_ascii_digit() {
            num_str.push(ch);
        } else {
            break;
        }
    }
    
    parse_u32(&num_str)
}

fn format_u32(n: u32) -> String {
    if n == 0 {
        return String::from("0");
    }
    
    let mut result = String::new();
    let mut num = n;
    let mut digits = Vec::new();
    
    while num > 0 {
        digits.push((num % 10) as u8 + b'0');
        num /= 10;
    }
    
    for &digit in digits.iter().rev() {
        result.push(digit as char);
    }
    
    result
}

fn validate_wallpaper_path(path: &str) -> bool {
    if path.is_empty() || path.len() > ApplyWallpaperMsg::MAX_PATH_LEN {
        return false;
    }
    if !path.starts_with("/system/wallpapers/") || path.contains("..") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn wallpaper_png_sidecar_path(path: &str) -> Option<String> {
    let lower = path.as_bytes();
    if lower.len() < 5 {
        return None;
    }
    let ext_start = path.rfind('.')?;
    let ext = &path[ext_start..];
    if ext.eq_ignore_ascii_case(".jpg") || ext.eq_ignore_ascii_case(".jpeg") {
        let mut png = String::from(&path[..ext_start]);
        png.push_str(".png");
        Some(png)
    } else {
        None
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    
    let mut result: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_digit() {
            return None;
        }
        let digit = (ch as u8 - b'0') as u32;
        result = result.checked_mul(10)?;
        result = result.checked_add(digit)?;
    }
    
    Some(result)
}

// ============================================================================
// End Desktop Configuration Structures
// ============================================================================



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOperation {
    None,
    Move { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_x: i32, start_win_y: i32 },
    Resize { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_w: u32, start_win_h: u32 },
}

fn isqrt_helper(n: u32) -> u32 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

fn frac_sqrt_256_helper(n: u32, sq_int: u32) -> u32 {
    if sq_int == 0 { return 0; }
    let step = 2 * sq_int + 1;
    let remainder = n - sq_int * sq_int;
    (remainder * 256) / step
}

fn rgb32_to_color(pixel: u32) -> Color {
    Color::new(
        ((pixel >> 16) & 0xFF) as u8,
        ((pixel >> 8) & 0xFF) as u8,
        (pixel & 0xFF) as u8,
    )
}

struct Compositor {
    fb: Framebuffer,
    backbuffer_fb: Framebuffer,
    _backbuffer: Vec<u32>,
    wm: WindowManager,
    cursor: CursorState,
    event_port: PortId,
    register_port: PortId,
    pending_windows: Vec<PendingWindow>,
    pending_close: Vec<PendingClose>,
    dirty: bool,
    mouse_left_down: bool,
    mouse_right_down: bool,
    drag_op: DragOperation,
    captured_window: Option<WindowId>,
    mouse_driver: MouseDriver,
    keyboard_shift: bool,
    desktop_bg: Color,
    icons: Vec<DesktopIcon>,
    dock_apps: Vec<DockApp>,
    ticks: u32,
    click_counter: u32,
    last_click_tick: u32,
    last_click_icon: Option<usize>,
    context_menu: ContextMenu,
    desktop_config: DesktopConfig,
    current_wallpaper_source: CurrentWallpaperSource,
    current_scaling_mode: ScalingMode,
    scaled_wallpaper: Option<libimage::DecodedImage>,
    wallpaper_recompute_pending: bool,
    config_save_pending: bool,
    image_cache: libimage::ImageCache,
}

impl Compositor {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();

        let event_port = create_port().expect("Failed to create event port");
        let register_port = create_port().expect("Failed to create registration port");

        let mut mouse_driver = MouseDriver::new();
        mouse_driver.init();

        let _ = register_irq_handler(1, event_port);
        let _ = register_irq_handler(12, event_port);

        let mut icons = Vec::new();
        icons.push(DesktopIcon {
            label: String::from("Files"),
            executable: String::from("fileman"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 28,
            color: atom_theme::colors::ATOM_COLOR_ACCENT_GLOW,
        });
        icons.push(DesktopIcon {
            label: String::from("Rectangles"),
            executable: String::from("demo_rects"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 120,
            color: atom_theme::colors::ATOM_COLOR_ACCENT,
        });
        icons.push(DesktopIcon {
            label: String::from("Text"),
            executable: String::from("demo_text"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 212,
            color: atom_theme::colors::ATOM_COLOR_SUCCESS,
        });

        let dock_apps = Self::build_dock_apps();

        // Allocate at max mode capacity so VideoModeChanged never needs reallocation.
        const MAX_BACKBUFFER_PIXELS: usize = 1920 * 1080;
        let init_pixels = (fb.stride() * fb.height()) as usize;
        let mut backbuffer = alloc::vec![0u32; MAX_BACKBUFFER_PIXELS.max(init_pixels)];
        let backbuffer_fb = Framebuffer::new_custom(
            backbuffer.as_mut_ptr() as u64,
            fb.width(),
            fb.height(),
            fb.stride(),
            fb.bytes_per_pixel() as u32,
        ).expect("Failed to create backbuffer framebuffer");

        Self {
            fb,
            backbuffer_fb,
            _backbuffer: backbuffer,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            event_port,
            register_port,
            pending_windows: Vec::new(),
            pending_close: Vec::new(),
            dirty: true,
            mouse_left_down: false,
            mouse_right_down: false,
            drag_op: DragOperation::None,
            captured_window: None,
            mouse_driver,
            keyboard_shift: false,
            desktop_bg: theme::DESKTOP_BG,
            icons,
            dock_apps,
            ticks: 0,
            click_counter: 0,
            last_click_tick: 0,
            last_click_icon: None,
            context_menu: ContextMenu {
                x: 0,
                y: 0,
                visible: false,
                items: {
                    let mut v = Vec::new();
                    v.push(String::from("Change Wallpaper"));
                    v
                },
            },
            desktop_config: DesktopConfig::default_config(width, height),
            current_wallpaper_source: CurrentWallpaperSource::SolidColor { rgb: 0x12141C },
            current_scaling_mode: ScalingMode::Fill,
            scaled_wallpaper: None,
            wallpaper_recompute_pending: false,
            config_save_pending: false,
            image_cache: libimage::ImageCache::with_capacity(1),
        }
    }

    fn run(&mut self) -> ! {
        self.load_persisted_config();
        self.flush_deferred_desktop_work();
        self.draw_all();

        let mut reg_buffer = [0u8; 64];

        loop {
            let ports = [self.register_port, self.event_port];

            while let Ok(Some(len)) = try_recv(self.register_port, &mut reg_buffer) {
                self.handle_register_message(&reg_buffer[..len]);
            }

            let mut event_buffer = [0u8; 64];
            while let Ok(Some(len)) = try_recv(self.event_port, &mut event_buffer) {
                self.handle_app_event(&event_buffer[..len]);
            }

            self.poll_input();

            self.reap_pending_closes();
            self.flush_deferred_desktop_work();

            if self.dirty {
                self.draw_all();
                self.dirty = false;
            }

            let _ = wait_any(&ports, 10);
            self.ticks = self.ticks.wrapping_add(1);
        }
    }

    fn poll_input(&mut self) {
        while let Some(event) = self.mouse_driver.poll_event() {
            self.cursor.apply_delta(event.dx, event.dy, self.fb.width(), self.fb.height());
            self.dirty = true;

            if event.left_button {
                if !self.mouse_left_down {
                    self.click_counter = self.click_counter.wrapping_add(1);
                    self.handle_click(self.cursor.x, self.cursor.y);
                } else {
                    self.handle_mouse_drag(self.cursor.x, self.cursor.y);
                }

                if matches!(self.drag_op, DragOperation::None) {
                    self.dispatch_mouse_move(self.cursor.x, self.cursor.y, event.dx as i16, event.dy as i16);
                }

                self.mouse_left_down = true;
            } else if event.right_button {
                if !self.mouse_right_down {
                    self.handle_right_click(self.cursor.x, self.cursor.y);
                }
                self.mouse_right_down = true;
            } else {
                if self.mouse_left_down {
                    self.handle_mouse_up(self.cursor.x, self.cursor.y);
                } else {
                    self.dispatch_mouse_move(self.cursor.x, self.cursor.y, event.dx as i16, event.dy as i16);
                }
                self.mouse_left_down = false;
                self.mouse_right_down = false;
            }
        }

        while let Some(scancode) = keyboard_poll() {
            let pressed = (scancode & 0x80) == 0;
            let code = scancode & 0x7F;

            match code {
                // 0xE0 extended-key prefix arrives as 0x60 after masking — skip it
                0x60 => {}
                scancodes::LEFT_SHIFT | scancodes::RIGHT_SHIFT => {
                    self.keyboard_shift = pressed;
                    self.dispatch_key_event(code, 0, pressed);
                }
                _ => {
                    // Send ALL keys (press and release) to the focused window so
                    // that game-style apps (Doom, etc.) can handle non-ASCII keys
                    // like arrows, Ctrl (fire), Shift, and key-up events.
                    let ascii = if pressed {
                        scancode_to_ascii(code, self.keyboard_shift)
                            .map(|c| c as u8)
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    self.dispatch_key_event(code, ascii, pressed);
                }
            }
        }
    }

    fn dispatch_mouse_move(&mut self, x: i32, y: i32, dx: i16, dy: i16) {
        let target_id = self.captured_window.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            let target = self.wm.get_window(id).and_then(|w| {
                w.event_port.map(|port| (port, w.content_x(), w.content_y()))
            });

            if let Some((port, content_x, content_y)) = target {
                let rel_x = x - content_x as i32;
                let rel_y = y - content_y as i32;

                let event = MouseMoveEvent {
                    x: rel_x,
                    y: rel_y,
                    dx,
                    dy,
                };
                self.send_window_event_async(id, port, MessageType::MouseMove, &event.to_bytes());
            }
        }
    }

    /// Dispatch a key event to the focused window.
    ///
    /// - Always sends `KeyDown` or `KeyUp` so game-aware apps receive every key.
    /// - Additionally sends `KeyPress` for printable ASCII key-down events for
    ///   backward compatibility with apps that rely on that message type (e.g.
    ///   terminal).
    fn dispatch_key_event(&mut self, scancode: u8, ascii: u8, pressed: bool) {
        let event = KeyEvent {
            scancode,
            character: ascii,
            modifiers: KeyModifiers {
                shift: self.keyboard_shift,
                ctrl: false,
                alt: false,
                caps_lock: false,
            },
        };

        let target = self.wm.focused_id
            .and_then(|id| self.wm.get_window(id).and_then(|w| w.event_port.map(|port| (id, port))));

        if let Some((window_id, port)) = target {
            // Primary: KeyDown / KeyUp for full key tracking
            let primary_type = if pressed { MessageType::KeyDown } else { MessageType::KeyUp };
            self.send_window_event_async(window_id, port, primary_type, &event.to_bytes());

            // Compat: also send KeyPress for printable key-down events
            if pressed && ascii != 0 {
                self.send_window_event_async(window_id, port, MessageType::KeyPress, &event.to_bytes());
            }
        }
    }

    fn send_window_event_async(&mut self, window_id: WindowId, port: PortId, msg_type: MessageType, payload: &[u8]) {
        if port == 0 {
            return;
        }

        if let Err(err) = send_message_async(port, msg_type, payload) {
            // Only treat NotFound (port closed / process exited) as a
            // definitive sign the process is dead.  Transient errors such
            // as WouldBlock (queue full) or InvalidArgument must NOT
            // destroy the window — the client may still be alive and
            // rendering into the shared surface.
            if matches!(err, SyscallError::NotFound) {
                self.drop_dead_window(window_id);
            }
        }
    }

    fn drop_dead_window(&mut self, window_id: WindowId) {
        self.pending_close.retain(|pc| pc.window_id != window_id);

        if self.captured_window == Some(window_id) {
            self.captured_window = None;
        }

        match self.drag_op {
            DragOperation::Move { window_id: id, .. } | DragOperation::Resize { window_id: id, .. }
                if id == window_id =>
            {
                self.drag_op = DragOperation::None;
            }
            _ => {}
        }

        self.wm.close_window(window_id);
        self.dirty = true;
    }

    fn handle_register_message(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
            MessageType::AppRegister => self.handle_app_registration(data),
            MessageType::WmRequest => self.handle_wm_request(&data[MessageHeader::SIZE..]),
            MessageType::WmCommitFrame => {
                if let Some(msg) = libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        window.surface_ready = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + libipc::messages::SurfacePresentMsg::SIZE {
                    if let Some(msg) = libipc::messages::SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                            window.surface_ready = true;
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_app_registration(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        if header.msg_type != MessageType::AppRegister {
            return;
        }

        let payload_start = MessageHeader::SIZE;
        if data.len() < payload_start + AppRegisterMsg::SIZE {
            return;
        }

        let reg_msg = match AppRegisterMsg::from_bytes(&data[payload_start..]) {
            Some(m) => m,
            None => return,
        };

        if !self.pending_windows.is_empty() {
            let pending = self.pending_windows.remove(0);

            let result = if let Some(window) = self.wm.get_window_mut(pending.window_id) {
                window.event_port = Some(reg_msg.app_port);
                window.surface_region_id.map(|rid| (rid, window.content_width(), window.content_height()))
            } else {
                None
            };

            if let Some((region_id, cw, ch)) = result {
                self.send_surface_assignment(
                    pending.window_id,
                    reg_msg.app_port,
                    region_id,
                    cw,
                    ch,
                    None
                );
            }
        }
    }

    fn handle_app_event(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
            MessageType::IrqNotification => {
                self.poll_input();
            }
            MessageType::WmRequest => {
                self.handle_wm_request(&data[MessageHeader::SIZE..]);
            }
            MessageType::WmCommitFrame => {
                if let Some(msg) = libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        window.surface_ready = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::MouseMove => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseMoveEvent::from_bytes(&data[payload_start..]) {
                    self.cursor.apply_delta(event.dx as i32, event.dy as i32, self.fb.width(), self.fb.height());
                    self.dirty = true;
                }
            }
            MessageType::MouseButtonDown => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseButtonEvent::from_bytes(&data[payload_start..]) {
                    if event.button == MouseButton::Left {
                        if !self.mouse_left_down {
                            self.click_counter = self.click_counter.wrapping_add(1);
                            self.handle_click(self.cursor.x, self.cursor.y);
                        }
                        self.mouse_left_down = true;
                    }
                }
            }
            MessageType::MouseButtonUp => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseButtonEvent::from_bytes(&data[payload_start..]) {
                    if event.button == MouseButton::Left {
                        self.mouse_left_down = false;
                    }
                }
            }
            MessageType::VideoModeChanged => {
                self.handle_video_mode_changed();
            }
            MessageType::ApplyWallpaper => {
                if let Some(msg) = ApplyWallpaperMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    self.handle_apply_wallpaper_msg(&msg);
                } else {
                    self.send_wallpaper_failed("Invalid wallpaper request");
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + SurfacePresentMsg::SIZE {
                    if let Some(msg) = SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                            window.surface_ready = true;
                        }
                        self.dirty = true;
                    }
                }
            }
            MessageType::KeyPress => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + 3 {
                    if let Some(key_event) = KeyEvent::from_bytes(&data[payload_start..]) {
                        let event_port = if let Some(focused_id) = self.wm.focused_id {
                            if let Some(window) = self.wm.get_window(focused_id) {
                                window.event_port
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some(focused_id) = self.wm.focused_id {
                            if let Some(port) = event_port {
                                self.send_window_event_async(focused_id, port, MessageType::KeyPress, &key_event.to_bytes());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        if self.context_menu.visible {
            if self.handle_context_menu_click(x, y) {
                self.context_menu.visible = false;
                self.dirty = true;
                return;
            }
            self.context_menu.visible = false;
            self.dirty = true;
        }

        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                self.wm.focus_window(id);
                self.dirty = true;
            }

            self.captured_window = Some(id);

            if let Some(w) = self.wm.get_window(id) {
                let btn_y = w.y + (WINDOW_HEADER_HEIGHT as i32 - 12) / 2;
                let btn_size = 12;
                let close_x = w.x + w.width as i32 - 22;
                let max_x = close_x - 20;
                let min_x = max_x - 20;

                if y >= btn_y && y < btn_y + btn_size {
                    if x >= close_x && x < close_x + btn_size {
                        self.handle_close_window(id);
                        return;
                    } else if x >= max_x && x < max_x + btn_size {
                        let (wx, wy, ww, wh) = self.get_work_area();
                        if self.wm.toggle_maximize(id, wx, wy, ww, wh) {
                            self.reallocate_window_surface(id);
                            self.dirty = true;
                        }
                        return;
                    } else if x >= min_x && x < min_x + btn_size {
                        self.wm.minimize_window(id);
                        self.dirty = true;
                        return;
                    }
                }

                if w.header_contains(x, y) {
                    self.drag_op = DragOperation::Move {
                        window_id: id,
                        start_mouse_x: x,
                        start_mouse_y: y,
                        start_win_x: w.x,
                        start_win_y: w.y,
                    };
                    return;
                }

                if x >= w.x + w.width as i32 - 16 && y >= w.y + w.height as i32 - 16 {
                    self.drag_op = DragOperation::Resize {
                        window_id: id,
                        start_mouse_x: x,
                        start_mouse_y: y,
                        start_win_w: w.width,
                        start_win_h: w.height,
                    };
                    return;
                }

                if let Some(port) = w.event_port {
                    let rel_x = x - w.content_x() as i32;
                    let rel_y = y - w.content_y() as i32;
                    if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                        let event = MouseButtonEvent {
                            button: MouseButton::Left,
                            x: rel_x,
                            y: rel_y,
                        };
                        self.send_window_event_async(id, port, MessageType::MouseButtonDown, &event.to_bytes());
                    }
                }
            }
            return;
        }

        if let Some(icon_index) = self.dock_icon_at(x, y) {
            self.handle_dock_click(icon_index);
            return;
        }

        if let Some(icon_idx) = self.icon_at(x, y) {
            let current = self.click_counter;
            if self.last_click_icon == Some(icon_idx) && current.wrapping_sub(self.last_click_tick) < 2 {
                let exe = self.icons[icon_idx].executable.clone();
                self.spawn_app(&exe);
                self.last_click_icon = None;
            } else {
                self.last_click_icon = Some(icon_idx);
                self.last_click_tick = current;
            }
            return;
        }
    }

    fn handle_mouse_drag(&mut self, x: i32, y: i32) {
        match self.drag_op {
            DragOperation::Move { window_id, start_mouse_x, start_mouse_y, start_win_x, start_win_y } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;
                if let Some(w) = self.wm.get_window_mut(window_id) {
                    w.x = start_win_x + dx;
                    w.y = start_win_y + dy;
                    self.dirty = true;
                }
            }
            DragOperation::Resize { window_id, start_mouse_x, start_mouse_y, start_win_w, start_win_h } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;
                let new_w = (start_win_w as i32 + dx).max(WINDOW_MIN_WIDTH as i32) as u32;
                let new_h = (start_win_h as i32 + dy).max(WINDOW_MIN_HEIGHT as i32) as u32;
                self.wm.resize_window(window_id, new_w, new_h);
                self.dirty = true;
            }
            DragOperation::None => {}
        }
    }

    /// Called when a `VideoModeChanged` IPC message arrives.
    ///
    /// Re-acquires the kernel framebuffer (which reflects the new BGA mode) and
    /// rebuilds `backbuffer_fb` to point at the same pre-allocated buffer with
    /// the updated stride and dimensions.  No heap allocation is performed here
    /// because the buffer was sized for the largest supported mode at startup.
    fn handle_video_mode_changed(&mut self) {
        let new_fb = match Framebuffer::new() {
            Some(fb) => fb,
            None => return,
        };
        let new_backbuffer_fb = match Framebuffer::new_custom(
            self._backbuffer.as_mut_ptr() as u64,
            new_fb.width(),
            new_fb.height(),
            new_fb.stride(),
            new_fb.bytes_per_pixel() as u32,
        ) {
            Some(fb) => fb,
            None => return,
        };
        self.fb           = new_fb;
        self.backbuffer_fb = new_backbuffer_fb;
        // Clamp cursor to new screen extents.
        let w = self.fb.width()  as i32;
        let h = self.fb.height() as i32;
        if self.cursor.x >= w { self.cursor.x = w - 1; }
        if self.cursor.y >= h { self.cursor.y = h - 1; }
        self.desktop_config.resolution = ResolutionConfig { width: self.fb.width(), height: self.fb.height() };
        if matches!(self.current_wallpaper_source, CurrentWallpaperSource::Image { .. }) {
            self.scaled_wallpaper = None;
            self.wallpaper_recompute_pending = true;
        }
        self.config_save_pending = true;
        self.dirty = true;
    }

    fn is_on_panel(&self, y: i32) -> bool {
        y >= 0 && y < PANEL_HEIGHT as i32
    }

    fn is_on_dock(&self, x: i32, y: i32) -> bool {
        if let Some((dock_x, dock_y, dock_w, dock_h, _, _, _, _)) = self.dock_layout() {
            x >= dock_x
                && x < dock_x + dock_w as i32
                && y >= dock_y
                && y < dock_y + dock_h as i32
        } else {
            false
        }
    }

    fn handle_right_click(&mut self, x: i32, y: i32) {
        self.context_menu.visible = false;

        if self.wm.window_at(x, y).is_some() {
            return;
        }

        if self.is_on_panel(y) {
            return;
        }

        if self.is_on_dock(x, y) {
            return;
        }

        if self.icon_at(x, y).is_some() {
            return;
        }

        self.context_menu.x = x;
        self.context_menu.y = y;
        self.context_menu.visible = true;
        self.dirty = true;
    }

    fn handle_context_menu_click(&mut self, x: i32, y: i32) -> bool {
        let menu_w = 180u32;
        let item_h = 32u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        if x >= self.context_menu.x && x < self.context_menu.x + menu_w as i32 &&
           y >= self.context_menu.y && y < self.context_menu.y + menu_h as i32 {
            let item_idx = (y - self.context_menu.y) as u32 / item_h;
            if (item_idx as usize) < self.context_menu.items.len() {
                let action = &self.context_menu.items[item_idx as usize];
                if action == "Change Wallpaper" {
                    self.open_display_settings_wallpaper_tab();
                }
                return true;
            }
        }
        false
    }

    fn open_display_settings_wallpaper_tab(&mut self) {
        let port = match libipc::protocol::lookup_service("display_settings") {
            Ok(port) => port,
            Err(_) => {
                self.spawn_display_settings();
                let mut found = None;
                for _ in 0..80 {
                    if let Ok(port) = libipc::protocol::lookup_service("display_settings") {
                        found = Some(port);
                        break;
                    }
                    yield_now();
                }
                match found {
                    Some(port) => port,
                    None => return,
                }
            }
        };

        if let Some(id) = self.wm.windows.iter().find(|w| w.title == "Display Settings").map(|w| w.id) {
            self.wm.focus_window(id);
        }

        let msg = OpenInTabMsg {
            target_app: String::from("display_settings"),
            tab_name: String::from("Wallpaper"),
        };
        let payload = msg.to_bytes();
        let header = MessageHeader::new(MessageType::OpenInTab, payload.len() as u32);
        let mut buf = Vec::new();
        buf.extend_from_slice(&header.to_bytes());
        buf.extend_from_slice(&payload);
        let _ = send(port, &buf);
        self.dirty = true;
    }

    fn send_wallpaper_applied(&mut self) {
        if let Ok(port) = libipc::protocol::lookup_service("display_settings") {
            let msg = WallpaperAppliedMsg {};
            let header = MessageHeader::new(MessageType::WallpaperApplied, WallpaperAppliedMsg::SIZE as u32);
            let mut buf = Vec::new();
            buf.extend_from_slice(&header.to_bytes());
            buf.extend_from_slice(&msg.to_bytes());
            let _ = send(port, &buf);
        }
    }

    fn send_wallpaper_failed(&mut self, error: &str) {
        if let Ok(port) = libipc::protocol::lookup_service("display_settings") {
            let msg = WallpaperFailedMsg { error_message: String::from(error) };
            let payload = msg.to_bytes();
            let header = MessageHeader::new(MessageType::WallpaperFailed, payload.len() as u32);
            let mut buf = Vec::new();
            buf.extend_from_slice(&header.to_bytes());
            buf.extend_from_slice(&payload);
            let _ = send(port, &buf);
        }
    }

    fn handle_apply_wallpaper_msg(&mut self, msg: &ApplyWallpaperMsg) {
        let source_type = WallpaperSourceType::from_ipc(msg.source_type);
        let scaling_mode = ScalingMode::from_ipc(msg.scaling_mode);
        let config = match source_type {
            WallpaperSourceType::SolidColor => DesktopConfig {
                wallpaper: WallpaperConfig {
                    source_type: WallpaperSourceType::SolidColor,
                    image_path: None,
                    color_rgb: msg.color_rgb,
                    scaling_mode,
                },
                resolution: ResolutionConfig { width: self.fb.width(), height: self.fb.height() },
            },
            WallpaperSourceType::Image => DesktopConfig {
                wallpaper: WallpaperConfig {
                    source_type: WallpaperSourceType::Image,
                    image_path: msg.image_path.clone(),
                    color_rgb: None,
                    scaling_mode,
                },
                resolution: ResolutionConfig { width: self.fb.width(), height: self.fb.height() },
            },
        };

        match self.apply_config(config, true) {
            Ok(()) => self.send_wallpaper_applied(),
            Err(error) => {
                self.revert_to_fallback_wallpaper();
                self.send_wallpaper_failed(error);
            }
        }
    }

    fn load_persisted_config(&mut self) {
        let mut config = DesktopConfig::default_config(self.fb.width(), self.fb.height());
        if let Ok(bytes) = fs::read_file("/user/config/desktop.cfg") {
            if let Ok(text) = core::str::from_utf8(&bytes) {
                let trimmed = text.trim();
                if let Ok(parsed) = DesktopConfig::from_json(trimmed) {
                    config = parsed;
                } else if validate_wallpaper_path(trimmed) {
                    config.wallpaper.source_type = WallpaperSourceType::Image;
                    config.wallpaper.image_path = Some(String::from(trimmed));
                    config.wallpaper.color_rgb = None;
                    config.wallpaper.scaling_mode = ScalingMode::Fill;
                }
            }
        }
        if matches!(config.wallpaper.source_type, WallpaperSourceType::SolidColor) {
            if let Ok(bytes) = fs::read_file("/user/config/desktop.json") {
                if let Ok(text) = core::str::from_utf8(&bytes) {
                    let trimmed = text.trim();
                    if let Ok(parsed) = DesktopConfig::from_json(trimmed) {
                        config = parsed;
                    }
                }
            }
        }
        let _ = self.apply_config(config, false);
    }

    fn load_and_decode_image(&mut self, path: &str) -> Result<DecodedImage, &'static str> {
        if !validate_wallpaper_path(path) {
            return Err("Invalid wallpaper path");
        }
        let data = fs::read_file(path).map_err(|_| "Image file not found")?;
        if data.len() > 16 * 1024 * 1024 {
            return Err("Image file too large");
        }
        let img = if let Some(sidecar_path) = wallpaper_png_sidecar_path(path) {
            if let Ok(sidecar_data) = fs::read_file(&sidecar_path) {
                PngDecoder::decode(&sidecar_data).or_else(|_| JpgDecoder::decode(&data))
            } else {
                JpgDecoder::decode(&data)
            }
        } else {
            JpgDecoder::decode(&data)
        }.map_err(|_| "Failed to decode image")?;
        if img.width == 0 || img.height == 0 || img.width > 4096 || img.height > 4096 {
            return Err("Image dimensions unsupported");
        }
        Ok(img)
    }

    fn apply_config(&mut self, config: DesktopConfig, persist: bool) -> Result<(), &'static str> {
        config.validate().map_err(|_| "Invalid desktop configuration")?;

        self.desktop_config = config.clone();
        self.current_scaling_mode = config.wallpaper.scaling_mode;
        match config.wallpaper.source_type {
            WallpaperSourceType::SolidColor => {
                let rgb = config.wallpaper.color_rgb.unwrap_or(0x12141C);
                self.current_wallpaper_source = CurrentWallpaperSource::SolidColor { rgb };
                self.scaled_wallpaper = None;
                self.wallpaper_recompute_pending = false;
                self.desktop_bg = Color::new(((rgb >> 16) & 0xFF) as u8, ((rgb >> 8) & 0xFF) as u8, (rgb & 0xFF) as u8);
            }
            WallpaperSourceType::Image => {
                let path = config.wallpaper.image_path.as_ref().ok_or("Missing image path")?.clone();
                self.current_wallpaper_source = CurrentWallpaperSource::Image { path };
                self.scaled_wallpaper = None;
                self.wallpaper_recompute_pending = true;
            }
        }

        if persist {
            self.config_save_pending = true;
        }
        self.dirty = true;
        Ok(())
    }

    fn save_desktop_config(&mut self) -> Result<(), &'static str> {
        self.desktop_config.validate().map_err(|_| "Invalid config")?;
        let json = self.desktop_config.to_json().map_err(|_| "Serialize failed")?;
        let tmp_path = "/user/config/desktop.tmp";
        let final_path = "/user/config/desktop.cfg";

        if fs::write_file(tmp_path, json.as_bytes()).is_ok() {
            match fs::rename(tmp_path, final_path) {
                Ok(()) => return Ok(()),
                Err(_) => {
                    // FAT32 rename is not implemented yet; fall back to direct overwrite.
                }
            }
        }

        fs::write_file(final_path, json.as_bytes()).map_err(|_| "Failed to write config")
    }

    fn revert_to_fallback_wallpaper(&mut self) {
        let fallback_rgb = match self.current_wallpaper_source {
            CurrentWallpaperSource::SolidColor { rgb } => rgb,
            CurrentWallpaperSource::Image { .. } => 0x12141C,
        };
        self.desktop_bg = Color::new(((fallback_rgb >> 16) & 0xFF) as u8, ((fallback_rgb >> 8) & 0xFF) as u8, (fallback_rgb & 0xFF) as u8);
        self.current_wallpaper_source = CurrentWallpaperSource::SolidColor { rgb: fallback_rgb };
        self.current_scaling_mode = ScalingMode::Fill;
        self.scaled_wallpaper = None;
        self.wallpaper_recompute_pending = false;
        self.desktop_config.wallpaper = WallpaperConfig {
            source_type: WallpaperSourceType::SolidColor,
            image_path: None,
            color_rgb: Some(fallback_rgb),
            scaling_mode: ScalingMode::Fill,
        };
        self.config_save_pending = true;
        self.dirty = true;
    }

    fn flush_deferred_desktop_work(&mut self) {
        if self.wallpaper_recompute_pending {
            let image_path = match &self.current_wallpaper_source {
                CurrentWallpaperSource::Image { path } => Some(path.clone()),
                CurrentWallpaperSource::SolidColor { .. } => None,
            };

            if let Some(path) = image_path {
                match self.load_and_decode_image(&path) {
                    Ok(wallpaper) => {
                        self.scaled_wallpaper = Some(self.scale_wallpaper(&wallpaper, self.current_scaling_mode));
                    }
                    Err(_) => {
                        self.revert_to_fallback_wallpaper();
                    }
                }
            } else {
                self.scaled_wallpaper = None;
            }
            self.wallpaper_recompute_pending = false;
            self.dirty = true;
        }

        if self.config_save_pending {
            let _ = self.save_desktop_config();
            self.config_save_pending = false;
        }
    }

    fn fill_image(width: u32, height: u32, color: Color) -> DecodedImage {
        let mut pixels = alloc::vec![0u8; (width * height * 4) as usize];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = color.r;
            chunk[1] = color.g;
            chunk[2] = color.b;
            chunk[3] = 255;
        }
        DecodedImage::new(width, height, pixels)
    }

    fn sample_bilinear(img: &DecodedImage, src_x_fp: i32, src_y_fp: i32) -> Option<(u8, u8, u8, u8)> {
        if img.width == 0 || img.height == 0 {
            return None;
        }

        let max_x_fp = ((img.width - 1) as i32) << 16;
        let max_y_fp = ((img.height - 1) as i32) << 16;
        let x_fp = src_x_fp.clamp(0, max_x_fp);
        let y_fp = src_y_fp.clamp(0, max_y_fp);

        let x0 = (x_fp >> 16) as u32;
        let y0 = (y_fp >> 16) as u32;
        let x1 = (x0 + 1).min(img.width - 1);
        let y1 = (y0 + 1).min(img.height - 1);

        let tx = (x_fp & 0xFFFF) as u32;
        let ty = (y_fp & 0xFFFF) as u32;

        let (r00, g00, b00, a00) = img.get_pixel(x0, y0)?;
        let (r10, g10, b10, a10) = img.get_pixel(x1, y0)?;
        let (r01, g01, b01, a01) = img.get_pixel(x0, y1)?;
        let (r11, g11, b11, a11) = img.get_pixel(x1, y1)?;

        let lerp = |a: u32, b: u32, t: u32| -> u32 {
            (((a * (65536 - t)) + (b * t)) + 32768) >> 16
        };

        let top_r = lerp(r00 as u32, r10 as u32, tx);
        let top_g = lerp(g00 as u32, g10 as u32, tx);
        let top_b = lerp(b00 as u32, b10 as u32, tx);
        let top_a = lerp(a00 as u32, a10 as u32, tx);
        let bot_r = lerp(r01 as u32, r11 as u32, tx);
        let bot_g = lerp(g01 as u32, g11 as u32, tx);
        let bot_b = lerp(b01 as u32, b11 as u32, tx);
        let bot_a = lerp(a01 as u32, a11 as u32, tx);

        let r = lerp(top_r, bot_r, ty).min(255) as u8;
        let g = lerp(top_g, bot_g, ty).min(255) as u8;
        let b = lerp(top_b, bot_b, ty).min(255) as u8;
        let a = lerp(top_a, bot_a, ty).min(255) as u8;

        Some((r, g, b, a))
    }

    fn blit_scaled(dest: &mut DecodedImage, img: &DecodedImage, dst_x: i32, dst_y: i32, dst_w: u32, dst_h: u32) {
        if dst_w == 0 || dst_h == 0 || img.width == 0 || img.height == 0 {
            return;
        }
        for y in 0..dst_h {
            for x in 0..dst_w {
                let src_x = ((((x as u64) * 2 + 1) * img.width as u64) << 15) / dst_w as u64;
                let src_y = ((((y as u64) * 2 + 1) * img.height as u64) << 15) / dst_h as u64;
                let src_x_fp = src_x as i64 - (1 << 15);
                let src_y_fp = src_y as i64 - (1 << 15);
                let dx = dst_x + x as i32;
                let dy = dst_y + y as i32;
                if dx < 0 || dy < 0 || dx >= dest.width as i32 || dy >= dest.height as i32 {
                    continue;
                }
                if let Some((r, g, b, a)) = Self::sample_bilinear(img, src_x_fp as i32, src_y_fp as i32) {
                    let off = (((dy as u32) * dest.width + dx as u32) * 4) as usize;
                    dest.pixels[off] = r;
                    dest.pixels[off + 1] = g;
                    dest.pixels[off + 2] = b;
                    dest.pixels[off + 3] = a;
                }
            }
        }
    }

    fn scale_fill(&self, img: &DecodedImage, sw: u32, sh: u32) -> DecodedImage {
        let mut out = Self::fill_image(sw, sh, self.desktop_bg);
        let scale_w = (sw as u64 * 1024) / img.width as u64;
        let scale_h = (sh as u64 * 1024) / img.height as u64;
        let scale = scale_w.max(scale_h).max(1);
        let target_w = ((img.width as u64 * scale) / 1024) as u32;
        let target_h = ((img.height as u64 * scale) / 1024) as u32;
        let dx = (sw as i32 - target_w as i32) / 2;
        let dy = (sh as i32 - target_h as i32) / 2;
        Self::blit_scaled(&mut out, img, dx, dy, target_w.max(1), target_h.max(1));
        out
    }

    fn scale_fit(&self, img: &DecodedImage, sw: u32, sh: u32) -> DecodedImage {
        let mut out = Self::fill_image(sw, sh, self.desktop_bg);
        let scale_w = (sw as u64 * 1024) / img.width as u64;
        let scale_h = (sh as u64 * 1024) / img.height as u64;
        let scale = scale_w.min(scale_h).max(1);
        let target_w = ((img.width as u64 * scale) / 1024) as u32;
        let target_h = ((img.height as u64 * scale) / 1024) as u32;
        let dx = (sw as i32 - target_w as i32) / 2;
        let dy = (sh as i32 - target_h as i32) / 2;
        Self::blit_scaled(&mut out, img, dx, dy, target_w.max(1), target_h.max(1));
        out
    }

    fn scale_stretch(&self, img: &DecodedImage, sw: u32, sh: u32) -> DecodedImage {
        let mut out = Self::fill_image(sw, sh, self.desktop_bg);
        Self::blit_scaled(&mut out, img, 0, 0, sw, sh);
        out
    }

    fn scale_center(&self, img: &DecodedImage, sw: u32, sh: u32) -> DecodedImage {
        let mut out = Self::fill_image(sw, sh, self.desktop_bg);
        let dx = (sw as i32 - img.width as i32) / 2;
        let dy = (sh as i32 - img.height as i32) / 2;
        Self::blit_scaled(&mut out, img, dx, dy, img.width, img.height);
        out
    }

    fn scale_tile(&self, img: &DecodedImage, sw: u32, sh: u32) -> DecodedImage {
        let mut out = Self::fill_image(sw, sh, self.desktop_bg);
        for y in 0..sh {
            for x in 0..sw {
                if let Some((r, g, b, a)) = img.get_pixel(x % img.width, y % img.height) {
                    let off = ((y * sw + x) * 4) as usize;
                    out.pixels[off] = r;
                    out.pixels[off + 1] = g;
                    out.pixels[off + 2] = b;
                    out.pixels[off + 3] = a;
                }
            }
        }
        out
    }

    fn scale_wallpaper(&self, img: &DecodedImage, mode: ScalingMode) -> DecodedImage {
        let sw = self.fb.width();
        let sh = self.fb.height();
        match mode {
            ScalingMode::Fill => self.scale_fill(img, sw, sh),
            ScalingMode::Fit => self.scale_fit(img, sw, sh),
            ScalingMode::Stretch => self.scale_stretch(img, sw, sh),
            ScalingMode::Center => self.scale_center(img, sw, sh),
            ScalingMode::Tile => self.scale_tile(img, sw, sh),
        }
    }

    fn get_work_area(&self) -> (i32, i32, u32, u32) {
        let sw = self.fb.width();
        let sh = self.fb.height();

        let x = 0;
        let y = PANEL_HEIGHT as i32;
        let w = sw;
        // Subtract panel height and dock area (bottom margin)
        let h = sh.saturating_sub(PANEL_HEIGHT + DOCK_HEIGHT + 24);

        (x, y, w, h)
    }

    fn send_surface_assignment(&self, window_id: WindowId, port: PortId, region_id: SharedRegionId, width: u32, height: u32, resize_pos: Option<(i32, i32)>) {
        if port == 0 {
            return;
        }

        // 1. Notify client about surface
        let assign = SurfaceAssignMsg {
            window_id,
            region_id,
            width,
            height,
            stride: width,
            bytes_per_pixel: 4,
            compositor_port: self.event_port,
            scale_factor: 1000,
        };

        let header = MessageHeader::new(MessageType::SurfaceAssign, SurfaceAssignMsg::SIZE as u32);
        let mut full_msg = [0u8; 64];
        full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
        full_msg[MessageHeader::SIZE..MessageHeader::SIZE + SurfaceAssignMsg::SIZE].copy_from_slice(&assign.to_bytes());
        let _ = send(port, &full_msg[..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]);

        if let Some((win_x, win_y)) = resize_pos {
            // 2. Notify client about resize event
            let resize_event = libipc::messages::WmWindowEventMsg {
                window_id,
                event_type: libipc::messages::WindowEventType::Resize,
                x: win_x,
                y: win_y,
                width,
                height,
            };

            let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
            let mut full_msg = [0u8; 64];
            full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
            full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&resize_event.to_bytes());
            let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
        }
    }

    fn reallocate_window_surface(&mut self, window_id: WindowId) {
        let (cw, ch, already_correct) = if let Some(window) = self.wm.get_window(window_id) {
            let cw = window.content_width();
            let ch = window.content_height();
            let already_correct = if let Some(ref s) = window.surface {
                s.width() == cw && s.height() == ch
            } else {
                false
            };
            (cw, ch, already_correct)
        } else {
            return;
        };

        if already_correct {
            return;
        }

        // Create new shared surface for the window
        if let Ok(new_surface) = SharedSurface::create(cw, ch) {
            let region_id = new_surface.region_id();

            let result = if let Some(window) = self.wm.get_window_mut(window_id) {
                window.surface = Some(new_surface);
                window.surface_region_id = Some(region_id);
                window.content_dirty = false;
                window.surface_ready = false;
                window.event_port.map(|port| (port, window.x, window.y))
            } else {
                None
            };

            if let Some((port, x, y)) = result {
                self.send_surface_assignment(window_id, port, region_id, cw, ch, Some((x, y)));
            }
        }
    }

    fn handle_mouse_up(&mut self, x: i32, y: i32) {
        let captured = self.captured_window.take();

        match self.drag_op {
            DragOperation::Resize { window_id, .. } => {
                self.reallocate_window_surface(window_id);
                self.drag_op = DragOperation::None;
                return;
            }
            DragOperation::Move { .. } => {
                self.drag_op = DragOperation::None;
                return;
            }
            DragOperation::None => {}
        }

        let target_id = captured.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    let rel_x = x - w.content_x() as i32;
                    let rel_y = y - w.content_y() as i32;
                    if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                        let event = MouseButtonEvent {
                            button: MouseButton::Left,
                            x: rel_x,
                            y: rel_y,
                        };
                        self.send_window_event_async(id, port, MessageType::MouseButtonUp, &event.to_bytes());
                    }
                }
            }
        }

        self.drag_op = DragOperation::None;
    }

    fn handle_wm_request(&mut self, payload: &[u8]) {
        if payload.len() < 4 { return; }
        let req_type_raw = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let req_type = match libipc::messages::WmRequestType::from_u32(req_type_raw) {
            Some(t) => t,
            None => return,
        };

        match req_type {
            libipc::messages::WmRequestType::CreateWindow => {
                if let Some(req) = libipc::messages::WmCreateWindowRequest::from_bytes(payload) {
                    let win_x = 150 + (self.wm.windows.len() as i32 * 30);
                    let win_y = 120 + (self.wm.windows.len() as i32 * 30);

                    let id = self.wm.next_id;
                    self.wm.next_id += 1;

                    // Treat req.width/req.height as the desired *content* area.
                    // Add decorations so the surface returned equals exactly what
                    // the client requested.
                    let outer_w = req.width + WINDOW_BORDER_WIDTH * 2;
                    let outer_h = req.height + WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH;

                    let window = match Window::new_with_process(
                        id, &req.title, win_x, win_y, outer_w, outer_h,
                        req.reply_port as PortId
                    ) {
                        Some(w) => w,
                        None => return,
                    };

                    let resp = libipc::messages::WmCreateWindowResponse {
                        window_id: id,
                        region_id: window.surface_region_id.unwrap_or(0),
                        width: window.content_width(),
                        height: window.content_height(),
                        stride: window.content_width(),
                    };

                    self.wm.windows.push(window);
                    self.wm.focus_window(id);
                    self.dirty = true;

                    let header = MessageHeader::new(MessageType::WmResponse, libipc::messages::WmCreateWindowResponse::SIZE as u32);
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE].copy_from_slice(&resp.to_bytes());
                    if req.reply_port != 0 {
                        let _ = send(req.reply_port as PortId, &full_msg[..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE]);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_close_window(&mut self, id: WindowId) {
        let mut event_port = None;
        if let Some(w) = self.wm.get_window(id) {
            event_port = w.event_port;
        }

        if let Some(port) = event_port {
            if port != 0 {
                let msg = TerminateRequestMsg {
                    window_id: id,
                    reason: 0,
                };
                let header = MessageHeader::new(MessageType::TerminateRequest, TerminateRequestMsg::SIZE as u32);
                let mut full_msg = [0u8; 64];
                full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                full_msg[MessageHeader::SIZE..MessageHeader::SIZE + TerminateRequestMsg::SIZE].copy_from_slice(&msg.to_bytes());
                let _ = send(port, &full_msg[..MessageHeader::SIZE + TerminateRequestMsg::SIZE]);
            }
        }

        // Hide the window immediately so it disappears from the screen,
        // but defer actual destruction (which frees the shared surface) so
        // the client process has time to receive the TerminateRequest and
        // stop writing to the shared memory before it is unmapped.
        if let Some(w) = self.wm.get_window_mut(id) {
            w.visible = false;
        }

        // Update focus away from the closing window
        if self.wm.focused_id == Some(id) {
            self.wm.focused_id = self.wm.windows.iter()
                .filter(|w| w.visible && w.id != id && w.state != WindowState::Minimized)
                .last()
                .map(|w| w.id);
            if let Some(new_focus) = self.wm.focused_id {
                self.wm.focus_window(new_focus);
            }
        }

        self.pending_close.push(PendingClose {
            window_id: id,
            deadline_tick: self.ticks.wrapping_add(CLOSE_GRACE_TICKS),
        });

        self.dirty = true;
    }

    /// Reap windows whose close grace period has expired, destroying their
    /// shared surfaces.  Called every tick from the main loop.
    fn reap_pending_closes(&mut self) {
        let current = self.ticks;
        let mut reaped = false;
        // Collect IDs whose deadline has passed (handle wrapping arithmetic)
        let mut to_close: Vec<WindowId> = Vec::new();
        for pc in self.pending_close.iter() {
            // wrapping_sub: if current >= deadline (mod 2^32), time has elapsed
            if current.wrapping_sub(pc.deadline_tick) < 0x8000_0000 {
                to_close.push(pc.window_id);
            }
        }
        for id in to_close.iter() {
            self.wm.close_window(*id);
            reaped = true;
        }
        self.pending_close.retain(|pc| {
            current.wrapping_sub(pc.deadline_tick) >= 0x8000_0000
        });
        if reaped {
            self.dirty = true;
        }
    }

    fn dock_icon_at(&self, x: i32, y: i32) -> Option<usize> {
        let (_, _, _, _, start_x, icon_y, icon_size, spacing) = self.dock_layout()?;

        if y < icon_y || y >= icon_y + icon_size {
            return None;
        }

        for i in 0..self.dock_apps.len() {
            let ix = start_x + (i as i32 * (icon_size + spacing));
            if x >= ix && x < ix + icon_size {
                return Some(i);
            }
        }

        None
    }

    fn handle_dock_click(&mut self, icon_index: usize) {
        if icon_index < self.dock_apps.len() {
            let executable = self.dock_apps[icon_index].executable.clone();
            self.spawn_app(&executable);
        }
    }

    fn dock_layout(&self) -> Option<(i32, i32, u32, u32, i32, i32, i32, i32)> {
        let count = self.dock_apps.len();
        if count == 0 {
            return None;
        }

        let width = self.fb.width();
        let height = self.fb.height();
        let icon_size   = atom_theme::shell::DOCK_ITEM_SIZE as i32;  // DS: 48 px
        let spacing     = atom_theme::spacing::XL as i32;             // DS: 20 px
        let side_padding = atom_theme::spacing::XXXL as i32;          // DS: 32 px

        let total_icons_width = count as i32 * icon_size + (count as i32 - 1) * spacing;
        let dock_width = (total_icons_width + side_padding * 2).max(140) as u32;
        let dock_x = (width / 2).saturating_sub(dock_width / 2) as i32;
        let dock_y = height.saturating_sub(DOCK_HEIGHT + 12) as i32;
        let start_x = dock_x + ((dock_width as i32 - total_icons_width) / 2);
        let icon_y = dock_y + (DOCK_HEIGHT as i32 - icon_size) / 2;

        Some((dock_x, dock_y, dock_width, DOCK_HEIGHT, start_x, icon_y, icon_size, spacing))
    }

    fn build_dock_apps() -> Vec<DockApp> {
        let candidates: [(&str, &str, Color); 4] = [
            ("fileman",          "Files",    atom_theme::colors::ATOM_COLOR_ACCENT_GLOW),
            ("display_settings", "Settings", atom_theme::colors::ATOM_COLOR_ACCENT),
            ("tinygl_demo",      "TinyGL",   atom_theme::colors::ATOM_COLOR_SUCCESS),
            ("terminal",         "Terminal", atom_theme::colors::ATOM_GRADIENT_PRIMARY_END),
        ];

        let mut apps = Vec::new();

        for (exec, label, color) in candidates.iter() {
            let sys_path = alloc::format!("/apps/system/{}.atxf", exec);
            let user_path = alloc::format!("/apps/user/{}.atxf", exec);

            let exists = fs::stat(&sys_path).is_ok() || fs::stat(&user_path).is_ok();

            if exists {
                let mono = if exec.len() >= 2 {
                    alloc::format!(
                        "{}{}",
                        exec.as_bytes()[0] as char,
                        exec.as_bytes()[1] as char
                    )
                } else {
                    String::from(*exec)
                };

                apps.push(DockApp {
                    label: String::from(*label),
                    executable: String::from(*exec),
                    color: *color,
                    monogram: mono,
                });
            }
        }

        apps
    }

    fn spawn_app(&mut self, name: &str) {
        if name == "terminal" {
            self.spawn_terminal();
            return;
        }
        if name == "fileman" {
            self.spawn_fileman();
            return;
        }
        if name == "display_settings" {
            self.spawn_display_settings();
            return;
        }

        let user_path = alloc::format!("/apps/user/{}.atxf", name);
        if fs::stat(&user_path).is_ok() {
            let _ = spawn_from_path(&user_path);
            return;
        }

        let system_path = alloc::format!("/apps/system/{}.atxf", name);
        if fs::stat(&system_path).is_ok() {
            let _ = spawn_from_path(&system_path);
            return;
        }

        let _ = spawn_process(name);
    }

    fn spawn_fileman(&mut self) {
        let _pid = match spawn_from_path("/apps/user/fileman.atxf") {
            Ok(pid) => pid,
            Err(_) => return,
        };

        let offset = (self.wm.windows.len() as i32) * 30;
        let win_x = 100 + offset;
        let win_y = 80 + offset;
        let win_width = 720u32;
        let win_height = 480u32;

        let window_id = match self.wm.create_window_with_process(
            "File Manager",
            win_x,
            win_y,
            win_width,
            win_height,
            0,
        ) {
            Some(id) => id,
            None => return,
        };

        self.pending_windows.push(PendingWindow {
            window_id,
        });

        self.dirty = true;
    }

    fn spawn_terminal(&mut self) {
        let _pid = match spawn_process("terminal") {
            Ok(pid) => pid,
            Err(_) => return,
        };

        let offset = (self.wm.windows.len() as i32) * 30;
        let win_x = 150 + offset;
        let win_y = 120 + offset;
        let win_width = 640u32;
        let win_height = 420u32;

        let window_id = match self.wm.create_window_with_process(
            "Terminal",
            win_x,
            win_y,
            win_width,
            win_height,
            0,
        ) {
            Some(id) => id,
            None => return,
        };

        self.pending_windows.push(PendingWindow {
            window_id,
        });

        self.dirty = true;
    }

    fn spawn_display_settings(&mut self) {
        let _pid = match spawn_process("display_settings") {
            Ok(pid) => pid,
            Err(_) => return,
        };

        let offset = (self.wm.windows.len() as i32) * 20;
        let win_x = 200 + offset;
        let win_y = 100 + offset;
        let win_width = 480u32;
        let win_height = 420u32;

        let window_id = match self.wm.create_window_with_process(
            "Display Settings",
            win_x,
            win_y,
            win_width,
            win_height,
            0,
        ) {
            Some(id) => id,
            None => return,
        };

        self.pending_windows.push(PendingWindow {
            window_id,
        });

        self.dirty = true;
    }

    fn draw_all(&mut self) {
        if self.scaled_wallpaper.is_some() {
            let stride = self.backbuffer_fb.stride();
            if let Some(ref wp) = self.scaled_wallpaper {
                let ptr = self._backbuffer.as_mut_ptr() as *mut u8;
                let len = self._backbuffer.len() * 4;
                let buf = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
                wp.blit_to(buf, stride, 0, 0);
            }
        } else {
            self.backbuffer_fb.fill_rect(0, 0, self.backbuffer_fb.width(), self.backbuffer_fb.height(), self.desktop_bg);
        }

        self.draw_desktop_icons();
        self.draw_panel();

        for window in self.wm.windows.iter() {
            if window.visible {
                self.draw_window(window);
            }
        }

        self.draw_dock();

        if self.context_menu.visible {
            self.draw_context_menu();
        }

        self.draw_cursor();
        self.backbuffer_fb.blit(&self.fb);
    }

    fn draw_context_menu(&self) {
        let menu_w   = 200u32;
        let item_h   = atom_theme::spacing::XXXL as u32;                         // DS: 32 px row height
        let padding_v = atom_theme::spacing::SM as u32 - 2;                      // 6 px vertical padding
        let menu_r    = atom_theme::radius::SM as u32;                            // DS: 8 px
        let menu_h    = self.context_menu.items.len() as u32 * item_h + padding_v * 2;
        let mx = self.context_menu.x as u32;
        let my = self.context_menu.y as u32;

        // Shadow
        self.backbuffer_fb.fill_rect_rounded_alpha(mx + 2, my + 3, menu_w, menu_h, menu_r, theme::SHADOW, 100);
        // Background
        self.backbuffer_fb.fill_rect_rounded_aa(mx, my, menu_w, menu_h, menu_r, theme::MENU_BG);
        // Border
        self.backbuffer_fb.draw_rect_rounded_aa(mx, my, menu_w, menu_h, menu_r, theme::MENU_BORDER);

        for (i, item) in self.context_menu.items.iter().enumerate() {
            let iy = my + padding_v + (i as u32 * item_h);
            let text_y = iy + (item_h - 8) / 2;
            self.backbuffer_fb.draw_string(mx + atom_theme::spacing::LG as u32, text_y, item, theme::MENU_TEXT, theme::MENU_BG);
        }
    }

    fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        for (i, icon) in self.icons.iter().enumerate() {
            if x >= icon.x && x < icon.x + 60 && y >= icon.y && y < icon.y + 60 {
                return Some(i);
            }
        }
        None
    }

    fn draw_desktop_icons(&self) {
        for icon in &self.icons {
            let ix = icon.x as u32;
            let iy = icon.y as u32;
            let size = 60u32;
            let icon_size = 48u32;
            let icon_x = ix + (size - icon_size) / 2;
            let icon_y = iy + (size - icon_size) / 2 - 2;

            self.backbuffer_fb.fill_rect_rounded_aa(icon_x, icon_y, icon_size, icon_size, 8, icon.color);

            // Label below icon (centered)
            let label_len = icon.label.len() as u32 * 8;
            let lx = (ix as i32 + (size as i32 - label_len as i32) / 2).max(0) as u32;
            let label_y = iy + size + 6;
            self.backbuffer_fb.draw_string(lx, label_y, &icon.label, theme::ICON_LABEL, self.desktop_bg);
        }
    }

    fn draw_panel(&self) {
        let width = self.backbuffer_fb.width();

        // Panel background
        self.backbuffer_fb.fill_rect(0, 0, width, PANEL_HEIGHT, theme::PANEL_BG);
        // Subtle top highlight
        self.backbuffer_fb.fill_rect(0, 0, width, 1, theme::PANEL_BG_ACCENT);
        // Bottom border
        self.backbuffer_fb.fill_rect(0, PANEL_HEIGHT - 1, width, 1, theme::PANEL_BORDER);

        // Branding: Atom logo area
        let brand_y = (PANEL_HEIGHT - 8) / 2;
        // Accent dot — DS: radius::XS, spacing::MD offset
        self.backbuffer_fb.fill_rect_rounded_aa(atom_theme::spacing::MD as u32, brand_y - 1, 10, 10, atom_theme::radius::XS as u32, theme::ACCENT);
        // Brand text
        self.backbuffer_fb.draw_string(atom_theme::spacing::MD as u32 + 14, brand_y, "Atom", theme::PANEL_TEXT, theme::PANEL_BG);

        // Center: focused window title (if any)
        if let Some(focused_id) = self.wm.focused_id {
            if let Some(w) = self.wm.get_window(focused_id) {
                let title_len = w.title.len() as u32 * 8;
                let title_x = (width / 2).saturating_sub(title_len / 2);
                self.backbuffer_fb.draw_string(title_x, brand_y, &w.title, theme::PANEL_TEXT_DIM, theme::PANEL_BG);
            }
        }

        // Right side: clock + status area
        let clock_x = width.saturating_sub(96);
        // Status dot (indicates system running) — DS radius::XS
        self.backbuffer_fb.fill_rect_rounded_aa(clock_x - atom_theme::spacing::LG as u32, brand_y, 8, 8, atom_theme::radius::XS as u32, theme::BTN_MAXIMIZE);
        self.backbuffer_fb.draw_string(clock_x, brand_y, "12:00 PM", theme::PANEL_TEXT, theme::PANEL_BG);
    }

    fn blit_surface_bottom_rounded(&self, surface: &SharedSurface, dest_x: u32, dest_y: u32, radius: u32) {
        let Some(src_addr) = surface.address() else {
            return;
        };

        let fb_bpp = self.backbuffer_fb.bytes_per_pixel();
        if fb_bpp != surface.bytes_per_pixel() {
            surface.blit_to_framebuffer(&self.backbuffer_fb, dest_x, dest_y);
            return;
        }

        let copy_width = surface.width().min(self.backbuffer_fb.width().saturating_sub(dest_x));
        let copy_height = surface.height().min(self.backbuffer_fb.height().saturating_sub(dest_y));
        if copy_width == 0 || copy_height == 0 {
            return;
        }

        let r = radius.min(copy_width / 2).min(copy_height);
        if r == 0 {
            surface.blit_to_framebuffer(&self.backbuffer_fb, dest_x, dest_y);
            return;
        }

        let fb_addr = self.backbuffer_fb.address();
        let fb_stride = self.backbuffer_fb.stride();
        let src_stride = surface.stride();
        let straight_height = copy_height.saturating_sub(r);

        for sy in 0..straight_height {
            let src_offset = (sy * src_stride) as usize * fb_bpp;
            let dst_offset = ((dest_y + sy) * fb_stride + dest_x) as usize * fb_bpp;
            let src_ptr = ((src_addr as usize) + src_offset) as *const u8;
            let dst_ptr = (fb_addr + dst_offset) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_width as usize * fb_bpp);
            }
        }

        for sy in straight_height..copy_height {
            let dy = copy_height - 1 - sy;
            let fy = r - dy;
            let n = r * r - fy * fy;
            let sq = isqrt_helper(n);
            let frac = frac_sqrt_256_helper(n, sq);
            let int_offset = r - sq;
            let row_y = dest_y + sy;
            let row_width = copy_width.saturating_sub(int_offset * 2);

            if row_width > 0 {
                let src_offset = (sy * src_stride + int_offset) as usize * fb_bpp;
                let dst_offset = (row_y * fb_stride + dest_x + int_offset) as usize * fb_bpp;
                let src_ptr = ((src_addr as usize) + src_offset) as *const u8;
                let dst_ptr = (fb_addr + dst_offset) as *mut u8;
                unsafe {
                    core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, row_width as usize * fb_bpp);
                }
            }

            if frac > 0 && int_offset > 0 {
                let alpha = frac as u8;

                let left_src_x = int_offset - 1;
                let left_src_offset = (sy * src_stride + left_src_x) as usize * fb_bpp;
                let left_pixel = unsafe { (((src_addr as usize) + left_src_offset) as *const u32).read_volatile() };
                self.backbuffer_fb.fill_rect_alpha(
                    dest_x + left_src_x,
                    row_y,
                    1,
                    1,
                    rgb32_to_color(left_pixel),
                    alpha,
                );

                let right_src_x = copy_width - int_offset;
                let right_src_offset = (sy * src_stride + right_src_x) as usize * fb_bpp;
                let right_pixel = unsafe { (((src_addr as usize) + right_src_offset) as *const u32).read_volatile() };
                self.backbuffer_fb.fill_rect_alpha(
                    dest_x + right_src_x,
                    row_y,
                    1,
                    1,
                    rgb32_to_color(right_pixel),
                    alpha,
                );
            }
        }
    }

    fn draw_window(&self, window: &Window) {
        if !window.visible || window.state == WindowState::Minimized {
            return;
        }

        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        // Multi-layer soft shadow (only for non-maximized)
        if window.state != WindowState::Maximized {
            self.backbuffer_fb.fill_rect_rounded_alpha(x + 2, y + 4, w + 4, h + 4, 6, theme::SHADOW, 60);
            self.backbuffer_fb.fill_rect_rounded_alpha(x + 1, y + 2, w + 2, h + 2, 4, theme::SHADOW, 90);
        }

        let border_color = if window.focused {
            theme::WINDOW_BORDER_FOCUSED
        } else {
            theme::WINDOW_BORDER
        };

        // Window outer shell — full rounded rect (border color fills entire area first)
        self.backbuffer_fb.fill_rect_rounded_aa(x, y, w, h, atom_theme::shell::WINDOW_RADIUS as u32, border_color);

        // Window header — top corners rounded to match outer border (outer_r - border = inner_r)
        let inner_r = atom_theme::shell::WINDOW_RADIUS as u32 - WINDOW_BORDER_WIDTH;
        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.backbuffer_fb.fill_rect_top_rounded_aa(
            x + WINDOW_BORDER_WIDTH, y + WINDOW_BORDER_WIDTH,
            w - WINDOW_BORDER_WIDTH * 2, WINDOW_HEADER_HEIGHT - WINDOW_BORDER_WIDTH,
            inner_r, header_color,
        );
        // Header bottom separator
        self.backbuffer_fb.fill_rect(x + WINDOW_BORDER_WIDTH, y + WINDOW_HEADER_HEIGHT - 1,
                         w - WINDOW_BORDER_WIDTH * 2, 1, border_color);

        // Window title (vertically centered in header)
        let title_y = y + (WINDOW_HEADER_HEIGHT - 8) / 2;
        let title_color = if window.focused { theme::PANEL_TEXT } else { theme::PANEL_TEXT_DIM };
        self.backbuffer_fb.draw_string(x + 16, title_y, &window.title, title_color, header_color);

        // Traffic light buttons (rounded circles)
        let btn_y = y + (WINDOW_HEADER_HEIGHT - 12) / 2;
        let btn_size = 12u32;
        let btn_radius = 6u32;
        let close_x = x + w - 22;
        let max_x = close_x - 20;
        let min_x = max_x - 20;

        if window.focused {
            self.backbuffer_fb.fill_rect_rounded_aa(close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_CLOSE);
            self.backbuffer_fb.fill_rect_rounded_aa(max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MAXIMIZE);
            self.backbuffer_fb.fill_rect_rounded_aa(min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MINIMIZE);
        } else {
            self.backbuffer_fb.fill_rect_rounded_aa(close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
            self.backbuffer_fb.fill_rect_rounded_aa(max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
            self.backbuffer_fb.fill_rect_rounded_aa(min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
        }

        // Content area background — always paint the rounded body first so the
        // surface AA edge blends against the same shape as the window shell.
        self.backbuffer_fb.fill_rect_bottom_rounded_aa(
            window.content_x(), window.content_y(),
            window.content_width(), window.content_height(),
            inner_r, theme::WINDOW_BG,
        );

        // Blit application surface
        if window.surface_ready {
            if let Some(ref surface) = window.surface {
                self.blit_surface_bottom_rounded(surface, window.content_x(), window.content_y(), inner_r);
            }
        }
    }

    fn draw_dock(&self) {
        let (dock_x_i32, dock_y_i32, dock_width, dock_height, start_x_i32, icon_y_i32, icon_size_i32, spacing_i32) = match self.dock_layout() {
            Some(v) => v,
            None => return,
        };

        let dock_x = dock_x_i32 as u32;
        let dock_y = dock_y_i32 as u32;
        let start_x = start_x_i32 as u32;
        let icon_y = icon_y_i32 as u32;
        let icon_size = icon_size_i32 as u32;
        let spacing = spacing_i32 as u32;

        // Dock shadow
        let dock_r = atom_theme::radius::LG as u32; // DS: 16 px
        self.backbuffer_fb.fill_rect_rounded_alpha(dock_x + 2, dock_y + 3, dock_width, dock_height, dock_r, theme::SHADOW, 80);

        // Dock background (pill shape)
        self.backbuffer_fb.fill_rect_rounded_aa(dock_x, dock_y, dock_width, dock_height, dock_r, theme::DOCK_BG);
        // Dock border
        self.backbuffer_fb.draw_rect_rounded_aa(dock_x, dock_y, dock_width, dock_height, dock_r, theme::DOCK_BORDER);
        // Top highlight line
        self.backbuffer_fb.fill_rect(dock_x + dock_r, dock_y, dock_width - dock_r * 2, 1, theme::DOCK_BORDER);

        for (i, app) in self.dock_apps.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + spacing));

            let label_len = app.monogram.len() as u32 * 8;
            let lx = ix + (icon_size - label_len) / 2;
            let ly = icon_y + (icon_size - 8) / 2;
            self.backbuffer_fb.draw_string(lx, ly, &app.monogram, app.color, theme::DOCK_BG);

            // Active indicator dot for running apps
            if self.wm.windows.iter().any(|w| w.title == app.label && w.visible) {
                let dot_x = ix + icon_size / 2 - 2;
                let dot_y = icon_y + icon_size + 3;
                self.backbuffer_fb.fill_rect_rounded_aa(dot_x, dot_y, 4, 4, 2, theme::ACCENT);
            }
        }
    }

    fn _backbuffer_as_u8_mut(&mut self) -> &mut [u8] {
        let ptr = self._backbuffer.as_mut_ptr() as *mut u8;
        let len = self._backbuffer.len() * 4;
        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    }

    fn draw_cursor(&self) {
        let cursor_shape = [
            [1,0,0,0,0,0,0,0,0,0],
            [1,1,0,0,0,0,0,0,0,0],
            [1,2,1,0,0,0,0,0,0,0],
            [1,2,2,1,0,0,0,0,0,0],
            [1,2,2,2,1,0,0,0,0,0],
            [1,2,2,2,2,1,0,0,0,0],
            [1,2,2,2,2,2,1,0,0,0],
            [1,2,2,2,2,2,2,1,0,0],
            [1,2,2,2,2,2,2,2,1,0],
            [1,2,2,2,2,2,2,2,2,1],
            [1,2,2,2,2,1,1,1,1,1],
            [1,2,1,2,1,0,0,0,0,0],
            [1,1,0,1,2,1,0,0,0,0],
            [0,0,0,1,2,1,0,0,0,0],
            [0,0,0,0,1,2,1,0,0,0],
            [0,0,0,0,1,1,0,0,0,0],
        ];

        for (row, cols) in cursor_shape.iter().enumerate() {
            for (col, &pixel) in cols.iter().enumerate() {
                let px = self.cursor.x as u32 + col as u32;
                let py = self.cursor.y as u32 + row as u32;
                if px < self.backbuffer_fb.width() && py < self.backbuffer_fb.height() {
                    match pixel {
                        1 => self.backbuffer_fb.draw_pixel(px, py, theme::CURSOR_OUTLINE),
                        2 => self.backbuffer_fb.draw_pixel(px, py, theme::CURSOR_FILL),
                        _ => {}
                    }
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[no_mangle]
pub extern "efiapi" fn efi_main(
    _image_handle: *const core::ffi::c_void,
    _system_table: *const core::ffi::c_void,
) -> usize {
    main()
}

fn main() -> ! {
    log("Atom Desktop Environment v1.0");
    log("ui_shell: startup begin");

    let fb_info = match get_framebuffer() {
        Some(info) => info,
        None => {
            log("ui_shell: failed to get framebuffer info");
            exit(1)
        }
    };

    // Reserve enough heap for:
    // - a backbuffer at the largest supported mode (1920×1080×32bpp)
    // - decoded wallpaper source images / PNG scratch buffers
    // - one fully scaled wallpaper plus UI transient allocations
    //
    // The previous 8 MiB headroom was too small for image wallpapers and would
    // hit `alloc_error`, which traps in an infinite loop and looked like a
    // compositor freeze immediately after pressing Apply.
    const MAX_BACKBUFFER_PIXELS: usize = 1920 * 1080;
    const EXTRA_HEAP_HEADROOM: usize = 32 * 1024 * 1024;
    let fb_size = fb_info.stride as usize * fb_info.height as usize * fb_info.bytes_per_pixel as usize;
    let heap_size = (MAX_BACKBUFFER_PIXELS * 4).max(fb_size) + EXTRA_HEAP_HEADROOM;

    let region_id = match shared_region_create(heap_size) {
        Ok(id) => id,
        Err(_) => {
            log("ui_shell: failed to create shared heap region");
            exit(1)
        }
    };

    let heap_start = match shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE) {
        Ok(addr) => addr,
        Err(_) => {
            log("ui_shell: failed to map shared heap region");
            exit(1)
        }
    };

    ALLOCATOR.init(heap_start as usize, heap_size);

    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("ui_shell: failed to create framebuffer handle");
            exit(1)
        }
    };

    let mut compositor = Compositor::new(fb);

    log("ui_shell: registering compositor services");
    if libipc::protocol::register_service("compositor", compositor.event_port).is_err() {
        log("ui_shell: failed to register service 'compositor'");
        exit(1);
    }
    if libipc::protocol::register_service("compositor.register", compositor.register_port).is_err() {
        log("ui_shell: failed to register service 'compositor.register'");
        exit(1);
    }
    if libipc::protocol::register_service("compositor.wm", compositor.register_port).is_err() {
        log("ui_shell: failed to register service 'compositor.wm'");
        exit(1);
    }
    log("ui_shell: compositor services registered");

    compositor.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Desktop: PANIC!");
    exit(0xFF);
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_desktop_config_to_json_solid_color() {
        let config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(1184028),
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig {
                width: 1024,
                height: 768,
            },
        };
        
        let json = config.to_json().unwrap();
        assert!(json.contains("\"source_type\": \"SolidColor\""));
        assert!(json.contains("\"color_rgb\": 1184028"));
        assert!(json.contains("\"scaling_mode\": \"Fill\""));
        assert!(json.contains("\"width\": 1024"));
        assert!(json.contains("\"height\": 768"));
    }
    
    #[test]
    fn test_desktop_config_to_json_image() {
        let config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::Image,
                image_path: Some(String::from("/system/wallpapers/01.jpg")),
                color_rgb: None,
                scaling_mode: ScalingMode::Fit,
            },
            resolution: ResolutionConfig {
                width: 1920,
                height: 1080,
            },
        };
        
        let json = config.to_json().unwrap();
        assert!(json.contains("\"source_type\": \"Image\""));
        assert!(json.contains("\"image_path\": \"/system/wallpapers/01.jpg\""));
        assert!(json.contains("\"scaling_mode\": \"Fit\""));
        assert!(json.contains("\"width\": 1920"));
        assert!(json.contains("\"height\": 1080"));
    }
    
    #[test]
    fn test_desktop_config_from_json_solid_color() {
        let json = r#"{
  "wallpaper": {
    "source_type": "SolidColor",
    "color_rgb": 1184028,
    "scaling_mode": "Fill"
  },
  "resolution": {
    "width": 1024,
    "height": 768
  }
}"#;
        
        let config = DesktopConfig::from_json(json).unwrap();
        assert_eq!(config.wallpaper.source_type, WallpaperSourceType::SolidColor);
        assert_eq!(config.wallpaper.color_rgb, Some(1184028));
        assert_eq!(config.wallpaper.scaling_mode, ScalingMode::Fill);
        assert_eq!(config.resolution.width, 1024);
        assert_eq!(config.resolution.height, 768);
    }
    
    #[test]
    fn test_desktop_config_from_json_image() {
        let json = r#"{
  "wallpaper": {
    "source_type": "Image",
    "image_path": "/system/wallpapers/mountain.jpg",
    "scaling_mode": "Stretch"
  },
  "resolution": {
    "width": 1920,
    "height": 1080
  }
}"#;
        
        let config = DesktopConfig::from_json(json).unwrap();
        assert_eq!(config.wallpaper.source_type, WallpaperSourceType::Image);
        assert_eq!(config.wallpaper.image_path, Some(String::from("/system/wallpapers/mountain.jpg")));
        assert_eq!(config.wallpaper.scaling_mode, ScalingMode::Stretch);
        assert_eq!(config.resolution.width, 1920);
        assert_eq!(config.resolution.height, 1080);
    }
    
    #[test]
    fn test_desktop_config_roundtrip() {
        let original = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::Image,
                image_path: Some(String::from("/system/wallpapers/test.jpg")),
                color_rgb: None,
                scaling_mode: ScalingMode::Center,
            },
            resolution: ResolutionConfig {
                width: 1280,
                height: 720,
            },
        };
        
        let json1 = original.to_json().unwrap();
        let parsed = DesktopConfig::from_json(&json1).unwrap();
        let json2 = parsed.to_json().unwrap();
        
        assert_eq!(json1, json2);
    }
    
    #[test]
    fn test_validate_resolution_bounds() {
        let mut config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(1184028),
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig {
                width: 1024,
                height: 768,
            },
        };
        
        assert!(config.validate().is_ok());
        
        // Test invalid width
        config.resolution.width = 500;
        assert!(config.validate().is_err());
        
        config.resolution.width = 2000;
        assert!(config.validate().is_err());
        
        // Test invalid height
        config.resolution.width = 1024;
        config.resolution.height = 400;
        assert!(config.validate().is_err());
        
        config.resolution.height = 1200;
        assert!(config.validate().is_err());
    }
    
    #[test]
    fn test_validate_image_path() {
        let mut config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::Image,
                image_path: Some(String::from("/system/wallpapers/test.jpg")),
                color_rgb: None,
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig {
                width: 1024,
                height: 768,
            },
        };
        
        assert!(config.validate().is_ok());
        
        // Test missing path
        config.wallpaper.image_path = None;
        assert!(config.validate().is_err());
        
        // Test empty path
        config.wallpaper.image_path = Some(String::from(""));
        assert!(config.validate().is_err());
        
        // Test invalid extension
        config.wallpaper.image_path = Some(String::from("/system/wallpapers/test.png"));
        assert!(config.validate().is_err());
    }
    
    #[test]
    fn test_validate_color_rgb() {
        let mut config = DesktopConfig {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(1184028),
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig {
                width: 1024,
                height: 768,
            },
        };
        
        assert!(config.validate().is_ok());
        
        // Test missing color
        config.wallpaper.color_rgb = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_wallpaper_path_constraints() {
        assert!(validate_wallpaper_path("/system/wallpapers/test.jpg"));
        assert!(validate_wallpaper_path("/system/wallpapers/test.JPEG"));
        assert!(!validate_wallpaper_path("relative/test.jpg"));
        assert!(!validate_wallpaper_path("/system/wallpapers/../test.jpg"));
        assert!(!validate_wallpaper_path("/tmp/test.jpg"));
        assert!(!validate_wallpaper_path("/system/wallpapers/test.png"));
    }

    #[test]
    fn test_parse_invalid_json_missing_fields() {
        let json = r#"{
  "wallpaper": {
    "source_type": "Image"
  }
}"#;
        assert!(matches!(DesktopConfig::from_json(json), Err(ParseError::MissingField(_))));
    }

    #[test]
    fn test_parse_invalid_json_conditional_field() {
        let json = r#"{
  "wallpaper": {
    "source_type": "Image",
    "scaling_mode": "Fill"
  },
  "resolution": {
    "width": 1024,
    "height": 768
  }
}"#;
        assert!(matches!(DesktopConfig::from_json(json), Err(ParseError::MissingField("image_path"))));
    }
    
    #[test]
    fn test_scaling_mode_conversions() {
        assert_eq!(ScalingMode::Fill.to_str(), "Fill");
        assert_eq!(ScalingMode::Fit.to_str(), "Fit");
        assert_eq!(ScalingMode::Stretch.to_str(), "Stretch");
        assert_eq!(ScalingMode::Center.to_str(), "Center");
        assert_eq!(ScalingMode::Tile.to_str(), "Tile");
        
        assert_eq!(ScalingMode::from_str("Fill"), Some(ScalingMode::Fill));
        assert_eq!(ScalingMode::from_str("Fit"), Some(ScalingMode::Fit));
        assert_eq!(ScalingMode::from_str("Stretch"), Some(ScalingMode::Stretch));
        assert_eq!(ScalingMode::from_str("Center"), Some(ScalingMode::Center));
        assert_eq!(ScalingMode::from_str("Tile"), Some(ScalingMode::Tile));
        assert_eq!(ScalingMode::from_str("Invalid"), None);
    }
    
    #[test]
    fn test_wallpaper_source_type_conversions() {
        assert_eq!(WallpaperSourceType::Image.to_str(), "Image");
        assert_eq!(WallpaperSourceType::SolidColor.to_str(), "SolidColor");
        
        assert_eq!(WallpaperSourceType::from_str("Image"), Some(WallpaperSourceType::Image));
        assert_eq!(WallpaperSourceType::from_str("SolidColor"), Some(WallpaperSourceType::SolidColor));
        assert_eq!(WallpaperSourceType::from_str("Invalid"), None);
    }
}
