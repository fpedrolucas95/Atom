// Atom OS – Display Settings
//
// A proper desktop-environment-integrated settings window.
// Renders into the compositor-provided SharedSurface and communicates
// exclusively via IPC — no direct framebuffer access.
//
// Lifecycle:
//   1. Create local IPC port.
//   2. Register with namesvc as "display_settings".
//   3. Look up "compositor.register" and send AppRegister.
//   4. Wait for SurfaceAssign → map the shared region.
//   5. Render UI → SurfacePresent → event loop.
//
// Key events handled:
//   ArrowUp / ArrowDown  — navigate mode list
//   Enter                — apply selected mode
//   R                    — restore default (1024×768)

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
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::thread::{exit, yield_now};
use atom_syscall::debug::log;

use libipc::messages::{
    MessageType, MessageHeader,
    SurfaceAssignMsg, SurfacePresentMsg,
    KeyEvent as IpcKeyEvent,
    MouseButtonEvent,
    MouseMoveEvent,
    MouseButton,
    MouseScrollEvent,
    OpenInTabMsg,
    WallpaperAppliedMsg,
    WallpaperFailedMsg,
    ScalingMode,
    ApplyWallpaperMsg,
    WallpaperSourceType,
};

use alloc::string::String;
use alloc::vec::Vec;

// ============================================================================
// Heap
// ============================================================================

const HEAP_SIZE: usize = 256 * 1024; // 256 KB — uses SharedSurface, no local backbuffer

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
// Theme (matches ui_shell dark palette)
// ============================================================================

mod theme {
    use atom_syscall::graphics::Color;

    pub const BG:           Color = Color::new(18,  20,  28);   // window body
    pub const HEADER_BG:    Color = Color::new(24,  27,  36);   // section headers
    pub const BORDER:       Color = Color::new(42,  48,  64);   // dividers
    pub const TEXT:         Color = Color::new(210, 215, 225);  // primary text
    pub const TEXT_DIM:     Color = Color::new(110, 118, 138);  // secondary / hint text
    pub const ACCENT:       Color = Color::new(86,  182, 245);  // highlight / accent
    pub const ACCENT_DIM:   Color = Color::new(50,  110, 160);  // dimmed accent fill
    pub const SEL_BG:       Color = Color::new(30,  70,  120);  // selected row background
    pub const SEL_TEXT:     Color = Color::new(255, 255, 255);  // selected row text
    pub const BTN_APPLY:    Color = Color::new(86,  182, 245);  // Apply button
    pub const BTN_RESTORE:  Color = Color::new(52,  58,  78);   // Restore button (dark)
    pub const BTN_TEXT:     Color = Color::new(255, 255, 255);
    pub const SUCCESS:      Color = Color::new(72,  199, 142);
    pub const WARNING:      Color = Color::new(245, 189, 65);
    pub const SCROLLBAR:    Color = Color::new(28,  34,  48);
    pub const THUMB:        Color = Color::new(72,  84,  116);
}

// ============================================================================
// Video mode list (queried from the kernel at startup)
// ============================================================================

#[derive(Clone, Copy)]
struct Mode { width: u16, height: u16 }

/// Default resolution used by "Restore Default".
const DEFAULT_W: u16 = 1024;
const DEFAULT_H: u16 = 768;

/// Query available video modes from the kernel.
///
/// Returns the populated mode array and count.  Falls back to an empty list
/// if the syscall reports zero modes (BGA not available in this environment).
fn query_modes() -> ([Mode; VIDEO_MAX_MODES], usize) {
    let count = video_mode_count().min(VIDEO_MAX_MODES);
    if count == 0 {
        return ([Mode { width: 0, height: 0 }; VIDEO_MAX_MODES], 0);
    }

    let mut raw = [VideoModeEntry::default(); VIDEO_MAX_MODES];
    let written = get_video_modes(&mut raw[..count]);

    let mut modes = [Mode { width: 0, height: 0 }; VIDEO_MAX_MODES];
    for i in 0..written {
        modes[i] = Mode { width: raw[i].width as u16, height: raw[i].height as u16 };
    }
    (modes, written)
}

/// Find the index of the default resolution (1024×768) in the mode list,
/// or 0 if it is absent.
fn default_idx(modes: &[Mode], count: usize) -> usize {
    for i in 0..count {
        if modes[i].width == DEFAULT_W && modes[i].height == DEFAULT_H {
            return i;
        }
    }
    0
}

// ============================================================================
// Status message
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum StatusKind { None, Ok, Warn }

struct Status {
    kind:  StatusKind,
    text:  [u8; 48],
    len:   usize,
    ticks: u32,
}

impl Status {
    const fn new() -> Self {
        Self { kind: StatusKind::None, text: [0; 48], len: 0, ticks: 0 }
    }
    fn set(&mut self, kind: StatusKind, msg: &[u8]) {
        self.kind = kind;
        self.len  = msg.len().min(47);
        self.text[..self.len].copy_from_slice(&msg[..self.len]);
        self.ticks = 180; // ~3 s at ~60 polls/sec
    }
    fn tick(&mut self) {
        if self.ticks > 0 { self.ticks -= 1; }
        if self.ticks == 0 { self.kind = StatusKind::None; }
    }
    fn text_str(&self) -> &str {
        core::str::from_utf8(&self.text[..self.len]).unwrap_or("")
    }
}

// ============================================================================
// Wallpaper state structures
// ============================================================================

/// Represents the source of a wallpaper (solid color or image file)
#[derive(Debug, Clone, PartialEq, Eq)]
enum WallpaperSource {
    SolidColor { rgb: u32 },
    Image { path: String },
}

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

/// State for the wallpaper tab
struct WallpaperState {
    solid_colors: [Color; 8],
    discovered_images: Vec<WallpaperInfo>,
    selected_source: Option<WallpaperSource>,
    selected_scaling: ScalingMode,
    discovery_in_progress: bool,
}

impl WallpaperState {
    const MAX_WALLPAPER_COUNT: usize = 100;

    fn new() -> Self {
        Self {
            solid_colors: [
                Color::new(18, 20, 28),   // Dark blue-gray
                Color::new(12, 14, 22),   // Darker blue-gray
                Color::new(24, 28, 42),   // Medium blue-gray
                Color::new(16, 24, 40),   // Deep blue
                Color::new(22, 36, 52),   // Teal-blue
                Color::new(28, 18, 36),   // Purple-gray
                Color::new(20, 30, 24),   // Dark green-gray
                Color::new(34, 20, 20),   // Dark red-brown
            ],
            discovered_images: Vec::new(),
            selected_source: None,
            selected_scaling: ScalingMode::Fill,
            discovery_in_progress: false,
        }
    }

    fn selected_is_image(&self) -> bool {
        matches!(self.selected_source, Some(WallpaperSource::Image { .. }))
    }

    fn select_color(&mut self, index: usize) {
        if index >= self.solid_colors.len() {
            return;
        }
        let color = self.solid_colors[index];
        let rgb = ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32);
        self.selected_source = Some(WallpaperSource::SolidColor { rgb });
    }

    fn select_image(&mut self, index: usize) {
        if let Some(info) = self.discovered_images.get(index) {
            self.selected_source = Some(WallpaperSource::Image { path: info.path.clone() });
        }
    }

    fn set_scaling_mode(&mut self, mode: ScalingMode) {
        self.selected_scaling = mode;
    }

    /// Discover wallpaper images from /system/wallpapers/ directory.
    /// Filters for .jpg and .jpeg files (case-insensitive), sorts alphabetically.
    /// Returns error if directory doesn't exist (graceful degradation - show only colors).
    ///
    /// Requirements: 3.1, 3.2, 3.3, 3.6, 3.7
    fn discover_images(&mut self) -> Result<(), &'static str> {
        use atom_syscall::fs::{open, readdir, close, OpenFlags, FileType};
        self.discovery_in_progress = true;
        self.discovered_images.clear();

        // Try to open /system/wallpapers/ directory
        let fd = match open("/system/wallpapers/", OpenFlags::DIRECTORY, 0) {
            Ok(fd) => fd,
            Err(_) => {
                // Directory doesn't exist - graceful degradation (show only colors)
                log("WallpaperState: /system/wallpapers/ not found, showing only colors");
                self.discovery_in_progress = false;
                return Err("Directory not found");
            }
        };

        // Read directory entries
        let entries = match readdir(fd) {
            Ok(entries) => entries,
            Err(_) => {
                let _ = close(fd);
                log("WallpaperState: Failed to read directory");
                self.discovery_in_progress = false;
                return Err("Failed to read directory");
            }
        };

        let _ = close(fd);

        // Filter valid images and add to discovered_images
        for entry in entries {
            // Skip directories
            if entry.file_type == FileType::Directory {
                continue;
            }

            // Validate file extension (case-insensitive .jpg or .jpeg)
            if validate_filename_extension(&entry.name) {
                // Build full path
                let mut path = String::from("/system/wallpapers/");
                path.push_str(&entry.name);

                // Add to discovered images with lazy-loaded thumbnail
                self.discovered_images.push(WallpaperInfo {
                    name: entry.name,
                    path,
                    thumbnail: ThumbnailState::Unloaded,
                });
                if self.discovered_images.len() >= Self::MAX_WALLPAPER_COUNT {
                    break;
                }
            }
        }

        // Sort alphabetically by filename
        self.discovered_images.sort_by(|a, b| a.name.cmp(&b.name));

        log("WallpaperState: Image discovery complete");
        self.discovery_in_progress = false;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabState {
    Wallpaper,
    Resolution,
}

#[derive(Clone, Copy)]
struct Tab {
    label: &'static str,
    state: TabState,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Tab {
    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x as i32
            && py >= self.y as i32
            && px < (self.x + self.width) as i32
            && py < (self.y + self.height) as i32
    }

    fn render(&self, surface: &SharedSurface, active: bool) {
        let bg = if active { theme::SEL_BG } else { theme::HEADER_BG };
        let fg = if active { theme::SEL_TEXT } else { theme::TEXT_DIM };
        surface.fill_rect_rounded_aa(self.x, self.y, self.width, self.height, 4, bg);
        surface.draw_rect_rounded_aa(self.x, self.y, self.width, self.height, 4, theme::BORDER);
        surface.draw_string(
            self.x + (self.width - self.label.len() as u32 * CHAR_W) / 2,
            self.y + (self.height - CHAR_H) / 2,
            self.label,
            fg,
            bg,
        );
    }
}

// ============================================================================
// Layout constants
// ============================================================================

const CHAR_W:       u32 = 8;
const CHAR_H:       u32 = 8;
const HDR_H:        u32 = 34;   // header bar height
const SEC_H:        u32 = 22;   // section label height
const ROW_H:        u32 = 22;   // each mode row height
const VISIBLE_ROWS: usize = 9;  // rows visible without scrolling
const SB_W:         u32 = 8;    // scrollbar width
const BTN_BAR_H:    u32 = 44;   // button bar height
const STATUS_H:     u32 = 20;   // status / hint bar height
const PAD:          u32 = 12;   // horizontal padding

// Wallpaper tab layout constants
const COLOR_TILE_SIZE:    u32 = 48;  // size of each color tile
const COLOR_TILE_SPACING: u32 = 12;  // spacing between color tiles
const TAB_HEIGHT:         u32 = 28;
const TAB_WIDTH:          u32 = 110;
const TAB_SPACING:        u32 = 8;
const IMAGE_TILE_W:       u32 = 116;
const IMAGE_TILE_H:       u32 = 112;
const IMAGE_TILE_SPACING: u32 = 10;
const IMAGE_LIST_ROWS:    usize = 2;
const SCALING_BTN_W:      u32 = 64;
const SCALING_BTN_H:      u32 = 24;
const SCALING_BTN_SPACING:u32 = 8;

// ============================================================================
// App state
// ============================================================================

struct DisplaySettings {
    window_id:       u32,
    compositor_port: PortId,
    local_port:      PortId,
    surface:         Option<SharedSurface>,
    surface_w:       u32,
    surface_h:       u32,
    modes:           [Mode; VIDEO_MAX_MODES],
    mode_count:      usize,
    selected:        usize,
    scroll:          usize,
    scrollbar_dragging: bool,
    scrollbar_drag_offset: i32,
    dirty:           bool,
    status:          Status,
    running:         bool,
    active_tab:      TabState,
    tabs:            [Tab; 2],
    wallpaper_state: WallpaperState,
    wallpaper_scroll: usize,
}

impl DisplaySettings {
    fn new(window_id: u32, compositor_port: PortId, local_port: PortId,
           surface: SharedSurface,
           modes: [Mode; VIDEO_MAX_MODES], mode_count: usize) -> Self {
        let w = surface.width();
        let h = surface.height();
        let sel = default_idx(&modes, mode_count);
        let mut s = Self {
            window_id, compositor_port, local_port,
            surface_w: w, surface_h: h,
            surface: Some(surface),
            modes,
            mode_count,
            selected: sel,
            scroll: 0,
            scrollbar_dragging: false,
            scrollbar_drag_offset: 0,
            dirty: true,
            status: Status::new(),
            running: true,
            active_tab: TabState::Resolution,
            tabs: [
                Tab {
                    label: "Wallpaper",
                    state: TabState::Wallpaper,
                    x: PAD,
                    y: HDR_H + 6,
                    width: TAB_WIDTH,
                    height: TAB_HEIGHT,
                },
                Tab {
                    label: "Resolution",
                    state: TabState::Resolution,
                    x: PAD + TAB_WIDTH + TAB_SPACING,
                    y: HDR_H + 6,
                    width: TAB_WIDTH,
                    height: TAB_HEIGHT,
                },
            ],
            wallpaper_state: WallpaperState::new(),
            wallpaper_scroll: 0,
        };
        s.clamp_scroll();
        let _ = s.wallpaper_state.discover_images();
        s
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Calculate the bounds of a color tile given its index (0-7)
    /// Returns (x, y, width, height)
    fn calculate_color_tile_bounds(&self, index: usize) -> (u32, u32, u32, u32) {
        let col = (index % 4) as u32;
        let row = (index / 4) as u32;
        
        // Position color tiles below the tab bar with some padding
        let color_grid_x = PAD + 8;
        let color_grid_y = HDR_H + TAB_HEIGHT + 40;
        
        let x = color_grid_x + col * (COLOR_TILE_SIZE + COLOR_TILE_SPACING);
        let y = color_grid_y + row * (COLOR_TILE_SIZE + COLOR_TILE_SPACING);
        
        (x, y, COLOR_TILE_SIZE, COLOR_TILE_SIZE)
    }

    fn image_list_origin(&self) -> (u32, u32) {
        (PAD + 272, HDR_H + TAB_HEIGHT + 40)
    }

    fn image_tile_bounds(&self, index: usize) -> (u32, u32, u32, u32) {
        let visible_index = index.saturating_sub(self.wallpaper_scroll);
        let col = (visible_index % 2) as u32;
        let row = (visible_index / 2) as u32;
        let (x0, y0) = self.image_list_origin();
        (
            x0 + col * (IMAGE_TILE_W + IMAGE_TILE_SPACING),
            y0 + row * (IMAGE_TILE_H + IMAGE_TILE_SPACING),
            IMAGE_TILE_W,
            IMAGE_TILE_H,
        )
    }

    fn scaling_button_bounds(&self, index: usize) -> (u32, u32, u32, u32) {
        let row = (index / 3) as u32;
        let col = (index % 3) as u32;
        let x = PAD + col * (SCALING_BTN_W + SCALING_BTN_SPACING);
        let y = HDR_H + TAB_HEIGHT + 190 + row * (SCALING_BTN_H + SCALING_BTN_SPACING);
        (x, y, SCALING_BTN_W, SCALING_BTN_H)
    }

    fn switch_tab(&mut self, tab: TabState) {
        let changed = self.active_tab != tab;
        if changed {
            self.active_tab = tab;
            if tab == TabState::Wallpaper && self.wallpaper_state.discovered_images.is_empty() {
                let _ = self.wallpaper_state.discover_images();
            }
            let msg = match tab {
                TabState::Wallpaper => b"Wallpaper tab selected".as_slice(),
                TabState::Resolution => b"Resolution tab selected".as_slice(),
            };
            self.status.set(StatusKind::Ok, msg);
        }
        self.dirty = true;
    }

    fn handle_tab_click(&mut self, x: i32, y: i32) -> bool {
        for tab in &self.tabs {
            if tab.contains(x, y) {
                self.switch_tab(tab.state);
                log("DisplaySettings: tab clicked");
                return true;
            }
        }
        false
    }

    fn render_tabs(&self, surface: &SharedSurface) {
        for tab in &self.tabs {
            tab.render(surface, self.active_tab == tab.state);
        }
    }

    fn notify_mode_changed(&self) {
        let hdr = MessageHeader::new(MessageType::VideoModeChanged, 0);
        let mut buf = [0u8; MessageHeader::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        let _ = send(self.compositor_port, &buf);
    }

    fn notify_present(&self) {
        let msg = SurfacePresentMsg { window_id: self.window_id };
        let hdr = MessageHeader::new(MessageType::SurfacePresent,
                                     SurfacePresentMsg::SIZE as u32);
        let mut buf = [0u8; MessageHeader::SIZE + SurfacePresentMsg::SIZE];
        buf[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
        buf[MessageHeader::SIZE..].copy_from_slice(&msg.to_bytes());
        let _ = send(self.compositor_port, &buf);
    }

    fn clamp_scroll(&mut self) {
        let max_scroll = self.mode_count.saturating_sub(VISIBLE_ROWS);
        if self.scroll > max_scroll { self.scroll = max_scroll; }
        if self.selected < self.scroll { self.scroll = self.selected; }
        if self.selected >= self.scroll + VISIBLE_ROWS {
            self.scroll = self.selected + 1 - VISIBLE_ROWS;
        }
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Render the wallpaper tab content, including the 8 solid color tiles
    /// arranged in a 4x2 grid layout.
    /// 
    /// Each tile is rendered with:
    /// - Size: 48x48 pixels (COLOR_TILE_SIZE)
    /// - Spacing: 12 pixels between tiles (COLOR_TILE_SPACING)
    /// - Rounded corners (4px radius)
    /// - Visual selection indicator (double accent border when selected)
    /// - Subtle border when not selected
    ///
    /// Requirements: 4.1, 4.2, 4.5, 14.3
    fn render_wallpaper_tab(&self, surface: &SharedSurface) {
        let w = self.surface_w;
        let h = self.surface_h;

        // Section label for color tiles
        let sec_y = HDR_H + TAB_HEIGHT + 14;
        surface.draw_string(PAD, sec_y + 3, "Solid Colors", theme::TEXT_DIM, theme::BG);
        surface.draw_string(PAD + 272, sec_y + 3, "Wallpapers", theme::TEXT_DIM, theme::BG);
        surface.fill_rect(0, sec_y + SEC_H - 1, w, 1, theme::BORDER);

        // Render 8 color tiles in 4x2 grid
        for i in 0..8 {
            let (tx, ty, tw, th) = self.calculate_color_tile_bounds(i);
            let color = self.wallpaper_state.solid_colors[i];

            // Check if this tile is selected
            let is_selected = if let Some(ref source) = self.wallpaper_state.selected_source {
                match source {
                    WallpaperSource::SolidColor { rgb } => {
                        // Convert color to RGB u32 for comparison
                        let tile_rgb = ((color.r as u32) << 16) | ((color.g as u32) << 8) | (color.b as u32);
                        tile_rgb == *rgb
                    }
                    _ => false,
                }
            } else {
                false
            };

            // Render tile with rounded corners
            surface.fill_rect_rounded_aa(tx, ty, tw, th, 4, color);

            // If selected, draw a highlight border
            if is_selected {
                surface.draw_rect_rounded_aa(tx, ty, tw, th, 4, theme::ACCENT);
                surface.draw_rect_rounded_aa(tx + 1, ty + 1, tw - 2, th - 2, 4, theme::ACCENT);
            } else {
                surface.draw_rect_rounded_aa(tx, ty, tw, th, 4, theme::BORDER);
            }
        }

        let visible_start = self.wallpaper_scroll;
        let visible_end = (visible_start + IMAGE_LIST_ROWS * 2).min(self.wallpaper_state.discovered_images.len());
        for i in visible_start..visible_end {
            let (ix, iy, iw, ih) = self.image_tile_bounds(i);
            let info = &self.wallpaper_state.discovered_images[i];
            let selected = matches!(
                self.wallpaper_state.selected_source,
                Some(WallpaperSource::Image { ref path }) if path == &info.path
            );
            let tile_bg = if selected { theme::SEL_BG } else { theme::HEADER_BG };
            surface.fill_rect_rounded_aa(ix, iy, iw, ih, 4, tile_bg);
            surface.draw_rect_rounded_aa(ix, iy, iw, ih, 4, if selected { theme::ACCENT } else { theme::BORDER });

            let preview_x = ix + 14;
            let preview_y = iy + 12;
            let preview_w = iw - 28;
            let preview_h = 64;
            surface.fill_rect_rounded_aa(preview_x, preview_y, preview_w, preview_h, 4, Color::new(60, 66, 80));

            match info.thumbnail {
                ThumbnailState::Loaded { width, height } => {
                    let dims = fit_within(width as u32, height as u32, preview_w - 6, preview_h - 6);
                    let px = preview_x + (preview_w - dims.0) / 2;
                    let py = preview_y + (preview_h - dims.1) / 2;
                    surface.fill_rect_rounded_aa(px, py, dims.0, dims.1, 3, theme::ACCENT_DIM);
                    surface.draw_rect_rounded_aa(px, py, dims.0, dims.1, 3, theme::ACCENT);
                }
                ThumbnailState::Failed => {
                    surface.draw_rect_rounded_aa(preview_x + 10, preview_y + 10, preview_w - 20, preview_h - 20, 3, theme::WARNING);
                }
                ThumbnailState::Unloaded => {
                    surface.fill_rect_rounded_aa(preview_x + 18, preview_y + 18, preview_w - 36, preview_h - 36, 3, theme::THUMB);
                }
            }

            surface.draw_string(ix + 8, iy + 84, &info.name, theme::TEXT, tile_bg);
        }

        if self.wallpaper_state.discovery_in_progress {
            surface.draw_string(PAD + 272, sec_y + SEC_H + IMAGE_TILE_H * 2 + 12, "Loading wallpapers...", theme::TEXT_DIM, theme::BG);
        } else if self.wallpaper_state.discovered_images.is_empty() {
            surface.draw_string(PAD + 272, sec_y + SEC_H + 16, "No JPG wallpapers found", theme::TEXT_DIM, theme::BG);
        }

        surface.draw_string(PAD, HDR_H + TAB_HEIGHT + 168, "Scaling", theme::TEXT_DIM, theme::BG);
        let modes = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch, ScalingMode::Center, ScalingMode::Tile];
        for (i, mode) in modes.iter().enumerate() {
            let (bx, by, bw, bh) = self.scaling_button_bounds(i);
            let enabled = self.wallpaper_state.selected_is_image();
            let active = self.wallpaper_state.selected_scaling == *mode;
            let bg = if !enabled {
                theme::HEADER_BG
            } else if active {
                theme::SEL_BG
            } else {
                theme::BTN_RESTORE
            };
            let fg = if !enabled { theme::TEXT_DIM } else { theme::BTN_TEXT };
            surface.fill_rect_rounded_aa(bx, by, bw, bh, 4, bg);
            surface.draw_rect_rounded_aa(bx, by, bw, bh, 4, if active { theme::ACCENT } else { theme::BORDER });
            surface.draw_string(
                bx + (bw - mode.to_str().len() as u32 * CHAR_W) / 2,
                by + (bh - CHAR_H) / 2,
                mode.to_str(),
                fg,
                bg,
            );
        }

        // ── Apply button for wallpaper tab ──────────────────────────────────
        let div_y = h - BTN_BAR_H - STATUS_H;
        let btn_area_y = div_y + 1;
        let btn_h = 26u32;
        let btn_vy = btn_area_y + (BTN_BAR_H - btn_h) / 2;

        // Apply button (right-aligned)
        let aw = 80u32;
        let ax = w - PAD - aw;
        surface.fill_rect_rounded_aa(ax, btn_vy, aw, btn_h, 4, theme::BTN_APPLY);
        let al = "Apply";
        surface.draw_string(
            ax + (aw - al.len() as u32 * CHAR_W) / 2,
            btn_vy + (btn_h - CHAR_H) / 2,
            al, theme::BTN_TEXT, theme::BTN_APPLY,
        );
    }

    fn render(&mut self) {
        if !self.dirty { return; }
        self.dirty = false;

        let surface = match self.surface { Some(ref s) => s, None => return };
        let w = self.surface_w;
        let h = self.surface_h;

        // Background
        surface.fill_rect(0, 0, w, h, theme::BG);

        // ── Header bar ───────────────────────────────────────────────────────
        surface.fill_rect(0, 0, w, HDR_H, theme::HEADER_BG);
        surface.fill_rect(0, HDR_H - 1, w, 1, theme::BORDER);

        // Small monitor icon (pixel-art style)
        let ix = PAD;
        let iy = (HDR_H - 14) / 2;
        surface.fill_rect(ix,     iy,     18, 12, theme::ACCENT_DIM);
        surface.draw_rect(ix,     iy,     18, 12, theme::ACCENT);
        surface.fill_rect(ix + 7, iy + 12, 4,  2, theme::ACCENT);
        surface.fill_rect(ix + 4, iy + 14, 10, 1, theme::ACCENT);

        let title_y = (HDR_H - CHAR_H) / 2;
        surface.draw_string(PAD + 24, title_y, "Display Settings", theme::TEXT, theme::HEADER_BG);
        self.render_tabs(surface);

        // Render appropriate tab content
        if self.active_tab == TabState::Wallpaper {
            self.render_wallpaper_tab(surface);
        } else {
            self.render_resolution_tab(surface);
        }

        // ── Status / hint bar ─────────────────────────────────────────────────
        let st_y = h - STATUS_H;
        surface.fill_rect(0, st_y, w, STATUS_H, theme::HEADER_BG);
        surface.fill_rect(0, st_y, w, 1, theme::BORDER);

        if self.status.kind != StatusKind::None {
            let col = if self.status.kind == StatusKind::Ok { theme::SUCCESS } else { theme::WARNING };
            surface.draw_string(PAD, st_y + (STATUS_H - CHAR_H) / 2,
                                 self.status.text_str(), col, theme::HEADER_BG);
        } else {
            let hint = if self.active_tab == TabState::Wallpaper {
                "Select color or wallpaper   Click Apply to save"
            } else {
                "\x18\x19 Navigate   Enter Apply   R Restore Default"
            };
            surface.draw_string(PAD, st_y + (STATUS_H - CHAR_H) / 2,
                                 hint, theme::TEXT_DIM, theme::HEADER_BG);
        }

        self.notify_present();
    }

    /// Render the resolution tab (existing functionality)
    fn render_resolution_tab(&self, surface: &SharedSurface) {
        let w = self.surface_w;
        let h = self.surface_h;

        // ── Section label ────────────────────────────────────────────────────
        let sec_y = HDR_H + TAB_HEIGHT + 14;
        surface.draw_string(PAD, sec_y + 3, "Available Resolutions", theme::TEXT_DIM, theme::BG);
        surface.fill_rect(0, sec_y + SEC_H - 1, w, 1, theme::BORDER);

        // ── Mode list ─────────────────────────────────────────────────────────
        let (list_top, list_h, list_w, sb_x) = self.resolution_list_geometry();

        for i in 0..VISIBLE_ROWS {
            let idx = self.scroll + i;
            if idx >= self.mode_count { break; }
            let m = self.modes[idx];
            let ry = list_top + i as u32 * ROW_H;
            let sel = idx == self.selected;

            let (row_bg, fg, dim) = if sel {
                (theme::SEL_BG, theme::SEL_TEXT, theme::ACCENT)
            } else {
                (theme::BG, theme::TEXT, theme::TEXT_DIM)
            };

            surface.fill_rect(PAD, ry, list_w, ROW_H - 1, row_bg);
            if sel {
                surface.fill_rect(PAD, ry, 3, ROW_H - 1, theme::ACCENT);
            }

            // "WWWW x HHHH"
            let mut lbuf = [0u8; 24];
            let label = fmt_resolution(&mut lbuf, m.width, m.height);
            let ty = ry + (ROW_H - CHAR_H) / 2;
            surface.draw_string(PAD + 8, ty, label, fg, row_bg);

            // right-side bpp hint
            let bpp = "32bpp";
            let bx = PAD + list_w - bpp.len() as u32 * CHAR_W - 6;
            surface.draw_string(bx, ty, bpp, dim, row_bg);

            if !sel {
                surface.fill_rect(PAD + 8, ry + ROW_H - 1, list_w - 16, 1, theme::BORDER);
            }
        }

        // ── Scrollbar ─────────────────────────────────────────────────────────
        surface.fill_rect(sb_x, list_top, SB_W, list_h, theme::SCROLLBAR);

        let (thumb_y, thumb_h, _) = self.scrollbar_geometry();
        let thumb_color = if self.scrollbar_dragging {
            theme::ACCENT
        } else {
            theme::THUMB
        };
        surface.fill_rect_rounded_aa(sb_x + 1, thumb_y, SB_W - 2, thumb_h, 3, thumb_color);

        // ── Info line below list ──────────────────────────────────────────────
        let info_y = list_top + list_h + 4;
        if self.mode_count > 0 {
            let m = self.modes[self.selected];
            let mut ibuf = [0u8; 44];
            let info = fmt_info(&mut ibuf, m.width, m.height);
            surface.draw_string(PAD, info_y, info, theme::TEXT_DIM, theme::BG);
        }

        // ── Divider ───────────────────────────────────────────────────────────
        let div_y = h - BTN_BAR_H - STATUS_H;
        surface.fill_rect(0, div_y, w, 1, theme::BORDER);

        // ── Button bar ────────────────────────────────────────────────────────
        let btn_area_y = div_y + 1;
        surface.fill_rect(0, btn_area_y, w, BTN_BAR_H, theme::BG);

        let btn_h  = 26u32;
        let btn_vy = btn_area_y + (BTN_BAR_H - btn_h) / 2;

        // Apply button (right-aligned)
        let aw = 80u32;
        let ax = w - PAD - aw;
        surface.fill_rect_rounded_aa(ax, btn_vy, aw, btn_h, 4, theme::BTN_APPLY);
        let al = "Apply";
        surface.draw_string(
            ax + (aw - al.len() as u32 * CHAR_W) / 2,
            btn_vy + (btn_h - CHAR_H) / 2,
            al, theme::BTN_TEXT, theme::BTN_APPLY,
        );

        // Restore button (left of Apply)
        let rw = 120u32;
        let rx = ax - PAD - rw;
        surface.fill_rect_rounded_aa(rx, btn_vy, rw, btn_h, 4, theme::BTN_RESTORE);
        surface.draw_rect_rounded_aa(rx, btn_vy, rw, btn_h, 4, theme::BORDER);
        let rl = "Restore Default";
        surface.draw_string(
            rx + (rw - rl.len() as u32 * CHAR_W) / 2,
            btn_vy + (btn_h - CHAR_H) / 2,
            rl, theme::BTN_TEXT, theme::BTN_RESTORE,
        );
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    fn apply_selected(&mut self) {
        if self.mode_count == 0 { return; }
        let m = self.modes[self.selected];
        match atom_syscall::graphics::set_video_mode(m.width, m.height, 32) {
            Ok(()) => {
                self.notify_mode_changed();
                self.status.set(StatusKind::Ok, b"Mode applied successfully.");
            }
            Err(_) => self.status.set(StatusKind::Warn,
                          b"BGA unavailable or mode rejected."),
        }
        self.dirty = true;
    }

    fn restore_default(&mut self) {
        if self.mode_count == 0 { return; }
        self.selected = default_idx(&self.modes, self.mode_count);
        self.clamp_scroll();
        let m = self.modes[self.selected];
        match atom_syscall::graphics::set_video_mode(m.width, m.height, 32) {
            Ok(()) => {
                self.notify_mode_changed();
                self.status.set(StatusKind::Ok, b"Restored default 1024x768.");
            }
            Err(_) => self.status.set(StatusKind::Warn, b"BGA unavailable; selection reset."),
        }
        self.dirty = true;
    }

    fn handle_open_in_tab(&mut self, tab_name: &str) {
        match tab_name {
            "Wallpaper" => self.switch_tab(TabState::Wallpaper),
            "Resolution" => self.switch_tab(TabState::Resolution),
            _ => return,
        }
        self.status.set(StatusKind::Ok, b"Display Settings focused");
        self.dirty = true;
    }

    fn handle_wallpaper_applied(&mut self) {
        self.status.set(StatusKind::Ok, b"Wallpaper applied successfully.");
        self.dirty = true;
    }

    fn handle_wallpaper_failed(&mut self, error: &str) {
        let mut msg = String::from("Wallpaper failed: ");
        msg.push_str(error);
        self.status.set(StatusKind::Warn, msg.as_bytes());
        self.dirty = true;
    }

    /// Apply the selected wallpaper by sending ApplyWallpaper IPC message to the compositor.
    /// 
    /// Constructs the message with:
    /// - source_type: Image or SolidColor based on selected_source
    /// - image_path: Full path if Image source
    /// - color_rgb: RGB value if SolidColor source
    /// - scaling_mode: Currently selected scaling mode
    /// 
    /// Displays "Applying wallpaper..." status after sending.
    /// 
    /// Requirements: 7.1, 7.2
    fn apply_wallpaper(&mut self) {
        // Check if a wallpaper source is selected
        let source = match &self.wallpaper_state.selected_source {
            Some(s) => s,
            None => {
                self.status.set(StatusKind::Warn, b"No wallpaper selected");
                self.dirty = true;
                return;
            }
        };

        // Construct ApplyWallpaper message based on selected source
        let msg = match source {
            WallpaperSource::SolidColor { rgb } => {
                ApplyWallpaperMsg {
                    source_type: WallpaperSourceType::SolidColor,
                    image_path: None,
                    color_rgb: Some(*rgb),
                    scaling_mode: self.wallpaper_state.selected_scaling,
                }
            }
            WallpaperSource::Image { path } => {
                ApplyWallpaperMsg {
                    source_type: WallpaperSourceType::Image,
                    image_path: Some(path.clone()),
                    color_rgb: None,
                    scaling_mode: self.wallpaper_state.selected_scaling,
                }
            }
        };

        // Serialize message
        let payload = msg.to_bytes();
        let hdr = MessageHeader::new(MessageType::ApplyWallpaper, payload.len() as u32);
        
        // Build complete message buffer
        let mut buf = Vec::new();
        buf.extend_from_slice(&hdr.to_bytes());
        buf.extend_from_slice(&payload);

        // Send to compositor
        match send(self.compositor_port, &buf) {
            Ok(()) => {
                self.status.set(StatusKind::Ok, b"Applying wallpaper...");
                log("DisplaySettings: ApplyWallpaper message sent");
            }
            Err(_) => {
                self.status.set(StatusKind::Warn, b"Failed to send wallpaper request");
                log("DisplaySettings: Failed to send ApplyWallpaper");
            }
        }
        
        self.dirty = true;
    }

    fn handle_key(&mut self, ev: &IpcKeyEvent) {
        // Extended scancodes (character == 0 → use scancode)
        if ev.character == 0 {
            match ev.scancode & 0x7F {
                0x48 => { // Up
                    if self.selected > 0 { self.selected -= 1; }
                    self.clamp_scroll(); self.dirty = true;
                }
                0x50 => { // Down
                    if self.selected + 1 < self.mode_count { self.selected += 1; }
                    self.clamp_scroll(); self.dirty = true;
                }
                0x49 => { // Page Up
                    self.selected = self.selected.saturating_sub(VISIBLE_ROWS);
                    self.clamp_scroll(); self.dirty = true;
                }
                0x51 => { // Page Down
                    let last = self.mode_count.saturating_sub(1);
                    self.selected = (self.selected + VISIBLE_ROWS).min(last);
                    self.clamp_scroll(); self.dirty = true;
                }
                _ => {}
            }
            return;
        }
        match ev.character {
            b'\n' | b'\r' => {
                if self.active_tab == TabState::Wallpaper {
                    self.apply_wallpaper();
                } else {
                    self.apply_selected();
                }
            }
            b'r' | b'R' if self.active_tab == TabState::Resolution => self.restore_default(),
            b'w' | b'W'   => self.switch_tab(TabState::Wallpaper),
            b't' | b'T'   => self.switch_tab(TabState::Resolution),
            _ => {}
        }
    }

    fn resolution_list_geometry(&self) -> (u32, u32, u32, u32) {
        let list_top = HDR_H + TAB_HEIGHT + 14 + SEC_H + 4;
        let list_h = VISIBLE_ROWS as u32 * ROW_H;
        let list_w = self.surface_w.saturating_sub(PAD * 2 + SB_W + 4);
        let sb_x = self.surface_w.saturating_sub(PAD + SB_W);
        (list_top, list_h, list_w, sb_x)
    }

    fn scrollbar_geometry(&self) -> (u32, u32, u32) {
        let (list_top, list_h, _, _) = self.resolution_list_geometry();
        let total = self.mode_count as u32;
        let visible = VISIBLE_ROWS as u32;
        let thumb_h = if total > 0 {
            ((list_h * visible) / total).max(12).min(list_h)
        } else {
            list_h
        };
        let travel = list_h.saturating_sub(thumb_h);
        let max_scroll = self.mode_count.saturating_sub(VISIBLE_ROWS) as u32;
        let thumb_y = if max_scroll > 0 {
            list_top + travel * self.scroll as u32 / max_scroll
        } else {
            list_top
        };
        (thumb_y, thumb_h, travel)
    }

    fn set_scroll_from_thumb_y(&mut self, thumb_y: i32) {
        let (list_top, _, _, _) = self.resolution_list_geometry();
        let (_, _, travel) = self.scrollbar_geometry();
        let max_scroll = self.mode_count.saturating_sub(VISIBLE_ROWS);
        if travel == 0 || max_scroll == 0 {
            self.scroll = 0;
            return;
        }
        let relative = (thumb_y - list_top as i32).clamp(0, travel as i32) as u32;
        self.scroll = ((relative as u64 * max_scroll as u64 + travel as u64 / 2)
            / travel as u64) as usize;
        self.dirty = true;
    }

    fn handle_scroll(&mut self, dz: i32) {
        let steps = dz.unsigned_abs().max(1) as usize;
        if self.active_tab == TabState::Wallpaper {
            if dz > 0 {
                self.wallpaper_scroll = self.wallpaper_scroll.saturating_sub(steps * 2);
            } else if dz < 0 {
                let max = self.wallpaper_state.discovered_images.len().saturating_sub(IMAGE_LIST_ROWS * 2);
                self.wallpaper_scroll = (self.wallpaper_scroll + steps * 2).min(max);
            }
        } else if dz > 0 {
            self.scroll = self.scroll.saturating_sub(steps);
        } else if dz < 0 {
            let max_scroll = self.mode_count.saturating_sub(VISIBLE_ROWS);
            self.scroll = (self.scroll + steps).min(max_scroll);
        }
        self.dirty = true;
    }

    fn handle_wallpaper_click(&mut self, x: i32, y: i32) -> bool {
        for i in 0..8 {
            let (tx, ty, tw, th) = self.calculate_color_tile_bounds(i);
            if x >= tx as i32 && x < (tx + tw) as i32 && y >= ty as i32 && y < (ty + th) as i32 {
                self.wallpaper_state.select_color(i);
                self.dirty = true;
                return true;
            }
        }

        let visible_end = (self.wallpaper_scroll + IMAGE_LIST_ROWS * 2).min(self.wallpaper_state.discovered_images.len());
        for i in self.wallpaper_scroll..visible_end {
            let (ix, iy, iw, ih) = self.image_tile_bounds(i);
            if x >= ix as i32 && x < (ix + iw) as i32 && y >= iy as i32 && y < (iy + ih) as i32 {
                self.wallpaper_state.select_image(i);
                self.dirty = true;
                return true;
            }
        }

        for i in 0..5 {
            let (bx, by, bw, bh) = self.scaling_button_bounds(i);
            if x >= bx as i32 && x < (bx + bw) as i32 && y >= by as i32 && y < (by + bh) as i32 {
                if self.wallpaper_state.selected_is_image() {
                    let mode = [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Stretch, ScalingMode::Center, ScalingMode::Tile][i];
                    self.wallpaper_state.set_scaling_mode(mode);
                    self.dirty = true;
                }
                return true;
            }
        }
        false
    }

    fn handle_mouse_down(&mut self, x: i32, y: i32) {
        let w = self.surface_w as i32;
        let h = self.surface_h as i32;

        if self.handle_tab_click(x, y) {
            return;
        }

        // Button bar hit test
        let div_y    = h - BTN_BAR_H as i32 - STATUS_H as i32;
        let btn_vy   = div_y + 1 + (BTN_BAR_H as i32 - 26) / 2;
        let ax       = w - PAD as i32 - 80;
        let rx       = ax - PAD as i32 - 120;

        // Check for Apply button click
        if y >= btn_vy && y < btn_vy + 26 {
            if x >= ax && x < ax + 80 {
                if self.active_tab == TabState::Wallpaper {
                    self.apply_wallpaper();
                } else {
                    self.apply_selected();
                }
                return;
            }
            // Restore button (only for resolution tab)
            if self.active_tab == TabState::Resolution && x >= rx && x < rx + 120 {
                self.restore_default(); 
                return; 
            }
        }

        // Wallpaper tab interactions
        if self.active_tab == TabState::Wallpaper {
            self.handle_wallpaper_click(x, y);
            return;
        }

        // Resolution tab interactions
        let (list_top_u, list_h_u, list_w_u, sb_x_u) = self.resolution_list_geometry();
        let (thumb_y_u, thumb_h_u, _) = self.scrollbar_geometry();
        if x >= sb_x_u as i32
            && x < (sb_x_u + SB_W) as i32
            && y >= list_top_u as i32
            && y < (list_top_u + list_h_u) as i32
        {
            if y >= thumb_y_u as i32 && y < (thumb_y_u + thumb_h_u) as i32 {
                self.scrollbar_dragging = true;
                self.scrollbar_drag_offset = y - thumb_y_u as i32;
            } else if y < thumb_y_u as i32 {
                self.scroll = self.scroll.saturating_sub(VISIBLE_ROWS);
            } else {
                let max_scroll = self.mode_count.saturating_sub(VISIBLE_ROWS);
                self.scroll = (self.scroll + VISIBLE_ROWS).min(max_scroll);
            }
            self.dirty = true;
            return;
        }

        // Mode list hit test
        let list_top = list_top_u as i32;
        let list_h = list_h_u as i32;
        let list_w = list_w_u as i32;

        if x >= PAD as i32 && x < PAD as i32 + list_w
            && y >= list_top && y < list_top + list_h
        {
            let row = ((y - list_top) / ROW_H as i32) as usize;
            let idx = self.scroll + row;
            if idx < self.mode_count {
                self.selected = idx;
                self.clamp_scroll();
                self.dirty = true;
            }
        }
    }

    fn handle_mouse_move(&mut self, y: i32) {
        if !self.scrollbar_dragging || self.active_tab != TabState::Resolution {
            return;
        }
        self.set_scroll_from_thumb_y(y - self.scrollbar_drag_offset);
    }

    fn handle_mouse_up(&mut self) {
        if self.scrollbar_dragging {
            self.scrollbar_dragging = false;
            self.dirty = true;
        }
    }

    // ── Event loop ────────────────────────────────────────────────────────────

    fn process_message(&mut self, buf: &[u8], len: usize) {
        if len < MessageHeader::SIZE { return; }
        let hdr = match MessageHeader::from_bytes(buf) { Some(h) => h, None => return };
        let payload = &buf[MessageHeader::SIZE..len.min(buf.len())];

        match hdr.msg_type {
            MessageType::TerminateRequest => {
                log("DisplaySettings: terminate");
                self.running = false;
            }
            MessageType::SurfaceAssign => {
                if let Some(msg) = SurfaceAssignMsg::from_bytes(payload) {
                    if let Ok(s) = SharedSurface::from_region(msg.region_id, msg.width, msg.height) {
                        self.surface_w = msg.width;
                        self.surface_h = msg.height;
                        self.surface   = Some(s);
                        self.dirty     = true;
                    }
                }
            }
            MessageType::KeyPress => {
                if let Some(ev) = IpcKeyEvent::from_bytes(payload) {
                    self.handle_key(&ev);
                }
            }
            MessageType::MouseButtonDown => {
                if let Some(ev) = MouseButtonEvent::from_bytes(payload) {
                    if ev.button == MouseButton::Left {
                        self.handle_mouse_down(ev.x, ev.y);
                    }
                }
            }
            MessageType::MouseMove => {
                if let Some(ev) = MouseMoveEvent::from_bytes(payload) {
                    self.handle_mouse_move(ev.y);
                }
            }
            MessageType::MouseButtonUp => {
                if let Some(ev) = MouseButtonEvent::from_bytes(payload) {
                    if ev.button == MouseButton::Left {
                        self.handle_mouse_up();
                    }
                }
            }
            MessageType::MouseScroll => {
                if let Some(ev) = MouseScrollEvent::from_bytes(payload) {
                    self.handle_scroll(ev.dz);
                }
            }
            MessageType::OpenInTab => {
                if let Some(msg) = OpenInTabMsg::from_bytes(payload) {
                    self.handle_open_in_tab(&msg.tab_name);
                }
            }
            MessageType::WallpaperApplied => {
                if WallpaperAppliedMsg::from_bytes(payload).is_some() {
                    self.handle_wallpaper_applied();
                }
            }
            MessageType::WallpaperFailed => {
                if let Some(msg) = WallpaperFailedMsg::from_bytes(payload) {
                    self.handle_wallpaper_failed(&msg.error_message);
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
                self.process_message(&buf, len);
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
                                let p = &buf[MessageHeader::SIZE..];
                                if let Some(msg) = SurfaceAssignMsg::from_bytes(p) {
                                    return Some(msg);
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

// ============================================================================
// Wallpaper helper functions
// ============================================================================

/// Validate that a filename has a valid image extension (.jpg or .jpeg, case-insensitive).
/// 
/// Requirements: 3.2, 9.6
fn validate_filename_extension(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn fit_within(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }
    let scale_w = (max_w as u64 * 1024) / src_w as u64;
    let scale_h = (max_h as u64 * 1024) / src_h as u64;
    let scale = scale_w.min(scale_h).max(1);
    (
        ((src_w as u64 * scale) / 1024).max(1) as u32,
        ((src_h as u64 * scale) / 1024).max(1) as u32,
    )
}

// ============================================================================
// Number-formatting helpers (no_std, no format!)
// ============================================================================

fn write_u32(buf: &mut [u8], mut n: u32) -> usize {
    if n == 0 { buf[0] = b'0'; return 1; }
    let mut tmp = [0u8; 10];
    let mut i = 0usize;
    while n > 0 { tmp[i] = b'0' + (n % 10) as u8; i += 1; n /= 10; }
    for j in 0..i { buf[j] = tmp[i - 1 - j]; }
    i
}

fn fmt_resolution<'a>(buf: &'a mut [u8; 24], w: u16, h: u16) -> &'a str {
    let mut p = 0usize;
    p += write_u32(&mut buf[p..], w as u32);
    buf[p..p + 3].copy_from_slice(b" x "); p += 3;
    p += write_u32(&mut buf[p..], h as u32);
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

fn fmt_info<'a>(buf: &'a mut [u8; 44], w: u16, h: u16) -> &'a str {
    let pre = b"Selected: ";
    buf[..pre.len()].copy_from_slice(pre);
    let mut p = pre.len();
    p += write_u32(&mut buf[p..], w as u32);
    buf[p..p + 3].copy_from_slice(b" x "); p += 3;
    p += write_u32(&mut buf[p..], h as u32);
    let suf = b" @ 32bpp  60Hz";
    buf[p..p + suf.len()].copy_from_slice(suf); p += suf.len();
    core::str::from_utf8(&buf[..p]).unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_settings() -> DisplaySettings {
        let modes = [Mode { width: 1024, height: 768 }; VIDEO_MAX_MODES];
        DisplaySettings {
            window_id: 1,
            compositor_port: 1,
            local_port: 2,
            surface: None,
            surface_w: 640,
            surface_h: 480,
            modes,
            mode_count: 1,
            selected: 0,
            scroll: 0,
            scrollbar_dragging: false,
            scrollbar_drag_offset: 0,
            dirty: false,
            status: Status::new(),
            running: true,
            active_tab: TabState::Resolution,
            tabs: [
                Tab { label: "Wallpaper", state: TabState::Wallpaper, x: PAD, y: HDR_H + 6, width: TAB_WIDTH, height: TAB_HEIGHT },
                Tab { label: "Resolution", state: TabState::Resolution, x: PAD + TAB_WIDTH + TAB_SPACING, y: HDR_H + 6, width: TAB_WIDTH, height: TAB_HEIGHT },
            ],
            wallpaper_state: WallpaperState::new(),
            wallpaper_scroll: 0,
        }
    }

    #[test]
    fn initial_ui_has_two_tabs() {
        let settings = make_settings();
        assert_eq!(settings.tabs.len(), 2);
        assert_eq!(settings.tabs[0].label, "Wallpaper");
        assert_eq!(settings.tabs[1].label, "Resolution");
    }

    #[test]
    fn switch_tab_updates_active_state() {
        let mut settings = make_settings();
        settings.switch_tab(TabState::Wallpaper);
        assert_eq!(settings.active_tab, TabState::Wallpaper);
    }

    #[test]
    fn clicking_wallpaper_tab_changes_active_tab() {
        let mut settings = make_settings();
        let wallpaper_tab = settings.tabs[0];
        assert!(settings.handle_tab_click(
            wallpaper_tab.x as i32 + 4,
            wallpaper_tab.y as i32 + 4,
        ));
        assert_eq!(settings.active_tab, TabState::Wallpaper);
    }

    #[test]
    fn color_selection_is_mutually_exclusive_with_image_selection() {
        let mut state = WallpaperState::new();
        state.discovered_images.push(WallpaperInfo {
            name: String::from("a.jpg"),
            path: String::from("/system/wallpapers/a.jpg"),
            thumbnail: ThumbnailState::Unloaded,
        });
        state.select_image(0);
        assert!(matches!(state.selected_source, Some(WallpaperSource::Image { .. })));
        state.select_color(2);
        assert!(matches!(state.selected_source, Some(WallpaperSource::SolidColor { .. })));
    }

    #[test]
    fn file_extension_validation_is_case_insensitive() {
        assert!(validate_filename_extension("test.JPG"));
        assert!(validate_filename_extension("test.jpeg"));
        assert!(!validate_filename_extension("test.png"));
    }

    #[test]
    fn fit_within_preserves_bounds() {
        let (w, h) = fit_within(1920, 1080, 64, 64);
        assert!(w <= 64);
        assert!(h <= 64);
        assert!(w > 0);
        assert!(h > 0);
    }
}

// ============================================================================
// Entry point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

fn main() -> ! {
    log("DisplaySettings: starting");

    let port = match create_port() {
        Ok(p) => p,
        Err(_) => { log("DisplaySettings: create_port failed"); exit(1); }
    };

    let _ = libipc::protocol::register_service("display_settings", port);

    log("DisplaySettings: looking up compositor.register");
    let reg_port = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(p) => break p,
            Err(_) => yield_now(),
        }
    };

    // AppRegister: header (16 B) + port (8 B) + pid (8 B)
    let mut rmsg = [0u8; MessageHeader::SIZE + 16];
    let hdr = MessageHeader::new(MessageType::AppRegister, 16);
    rmsg[..MessageHeader::SIZE].copy_from_slice(&hdr.to_bytes());
    rmsg[MessageHeader::SIZE..MessageHeader::SIZE + 8].copy_from_slice(&port.to_le_bytes());
    rmsg[MessageHeader::SIZE + 8..MessageHeader::SIZE + 16].copy_from_slice(&0u64.to_le_bytes());
    let _ = send(reg_port, &rmsg[..MessageHeader::SIZE + 16]);

    log("DisplaySettings: waiting for surface");
    let sa = match DisplaySettings::wait_for_surface(port) {
        Some(sa) => sa,
        None => { log("DisplaySettings: surface timeout"); exit(1); }
    };

    let surface = match SharedSurface::from_region(sa.region_id, sa.width, sa.height) {
        Ok(s) => s,
        Err(_) => { log("DisplaySettings: surface map failed"); exit(1); }
    };

    // Query available video modes from the kernel so the UI always reflects
    // the actual set supported by the current hardware/driver.
    let (modes, mode_count) = query_modes();
    log("DisplaySettings: entering event loop");

    let mut app = DisplaySettings::new(sa.window_id, sa.compositor_port, port, surface,
                                       modes, mode_count);
    app.run();
    exit(0);
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! { log("DisplaySettings: PANIC"); exit(0xFF); }
