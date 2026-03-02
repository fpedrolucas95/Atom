// Display Settings — System Configuration UI for Screen Resolution
//
// This userspace application implements the "Display" panel of the system
// settings, allowing users to:
//   - View the current display resolution and color depth
//   - Browse available resolutions
//   - Apply a new resolution immediately
//   - Restore the default (boot) resolution
//
// ## Architecture
//
// This app follows the layered design mandated by the kernel architecture:
//
//   Rendering layer     — direct framebuffer writes (LFB via SYS_GET_FRAMEBUFFER)
//   Video manager layer — SYS_SET_VIDEO_MODE / SYS_GET_VIDEO_MODES
//   IPC service layer   — registers as "display_settings" with namesvc
//   UI state layer      — manages selection cursor, apply/cancel state
//
// ## Double Buffering Strategy
//
// To avoid tearing during UI redraws, this app uses a software double buffer:
//   1. All drawing goes into `backbuffer` (allocated in our heap).
//   2. When a frame is complete, `blit_to_lfb()` copies the backbuffer
//      to the LFB in a single scanline-by-scanline pass.
//   3. The blit never reads from the LFB (write-only access to VRAM).
//
// This is intentional: VRAM reads are slow (uncached MMIO). The backbuffer
// is in normal RAM and is as fast as any heap allocation.
//
// ## Design Decisions
//
// - The UI is drawn entirely in software using the 8x8 font from libgui.
//   No GPU acceleration is required or assumed.
// - Resolution changes take effect immediately (no "reboot required").
// - The "Restore Default" button recalls the boot resolution, stored in
//   BOOT_MODE at startup.
// - The app registers an IPC port so other processes can request display
//   settings changes (future: settings daemon, app launcher integration).
//
// ## Limitations and Future Work
//
// - EDID is not queried (BGA/VBE DISPI does not expose DDC/CI).
// - Multiple monitors are not supported; only the primary LFB is managed.
// - Resolution changes are not persisted across reboots (no config file yet).
// - Future: read/write settings to /etc/display.conf via filesystem syscalls.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::vec::Vec;
use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::graphics::{
    Color, Framebuffer, VideoModeEntry,
    get_video_modes, get_current_video_mode, set_video_mode, video_mode_count,
};
use atom_syscall::ipc::create_port;
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use libipc::protocol::register_service;

// ============================================================================
// Heap Allocator
// ============================================================================
//
// We need a reasonably large heap for the backbuffer (up to 1920×1080×4 ~8MB)
// plus mode list and UI state. We use 10MB to be safe.
//
// Note: In a production system this would use mmap/brk to dynamically request
// memory from the kernel. For simplicity we use a static bump allocator.

const HEAP_SIZE: usize = 10 * 1024 * 1024; // 10 MB

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
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let next = aligned + layout.size();
            if next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self.next
                .compare_exchange_weak(cur, next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {
        // Bump allocator: no-op deallocation.
        // Acceptable for a settings dialog that runs briefly.
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    log("DisplaySettings: FATAL: out of heap memory");
    exit(0xFE);
}

// ============================================================================
// UI Constants
// ============================================================================

/// Background color (dark navy, consistent with Atom OS theme)
const COLOR_BG:         Color = Color::new(46,  52,  64);
/// Window title bar
const COLOR_TITLEBAR:   Color = Color::new(36,  41,  51);
/// Panel background
const COLOR_PANEL:      Color = Color::new(59,  66,  82);
/// Highlighted item (selected)
const COLOR_SELECTED:   Color = Color::new(94, 129, 172);
/// Highlighted item text
const COLOR_SEL_TEXT:   Color = Color::new(236, 239, 244);
/// Normal item text
const COLOR_TEXT:       Color = Color::new(216, 222, 233);
/// Dimmed / secondary text
const COLOR_DIM_TEXT:   Color = Color::new(129, 140, 152);
/// Apply button (green)
const COLOR_BTN_APPLY:  Color = Color::new(163, 190, 140);
/// Default button (amber)
const COLOR_BTN_DEFAULT: Color = Color::new(235, 203, 139);
/// Button text
const COLOR_BTN_TEXT:   Color = Color::new(46,  52,  64);
/// Border / separator
const COLOR_BORDER:     Color = Color::new(76,  86, 106);
/// Accent (info highlight)
const COLOR_ACCENT:     Color = Color::new(136, 192, 208);
/// Error / warning indicator
const COLOR_WARN:       Color = Color::new(191,  97, 106);

const FONT_W: u32 = 8;
const FONT_H: u32 = 8;

const WINDOW_X: u32 = 80;
const WINDOW_Y: u32 = 60;
const WINDOW_W: u32 = 560;
const WINDOW_H: u32 = 440;

const TITLE_H: u32 = 28;
const PANEL_MARGIN: u32 = 16;

/// Number of modes visible at once in the scroll list
const VISIBLE_MODES: usize = 8;

// ============================================================================
// Double Buffer
// ============================================================================

/// Write-only blit from a raw byte slice to the LFB.
/// Never reads from VRAM; always writes full scanlines.
fn blit_to_lfb(fb: &Framebuffer, buf: &[u8]) {
    let fb_addr = fb.address();
    let w = fb.width() as usize;
    let h = fb.height() as usize;
    let bpp = fb.bytes_per_pixel();
    let src_stride = w * bpp;
    let dst_stride = fb.stride() as usize * bpp;

    for y in 0..h {
        let src_off = y * src_stride;
        let dst_off = y * dst_stride;
        let src_ptr = buf[src_off..].as_ptr();
        let dst_ptr = (fb_addr + dst_off) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, src_stride.min(dst_stride));
        }
    }
}

// ============================================================================
// Pixel Writer (into backbuffer)
// ============================================================================

struct Canvas<'a> {
    buf: &'a mut Vec<u8>,
    width: u32,
    height: u32,
    /// Bytes per pixel (always 4 for BGRA)
    bpp: u32,
}

impl<'a> Canvas<'a> {
    fn new(buf: &'a mut Vec<u8>, width: u32, height: u32) -> Self {
        Self { buf, width, height, bpp: 4 }
    }

    #[inline]
    fn offset(&self, x: u32, y: u32) -> usize {
        (y * self.width + x) as usize * self.bpp as usize
    }

    #[inline]
    fn put_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height { return; }
        let off = self.offset(x, y);
        // BGRA layout (matches what the framebuffer expects in BGR mode)
        self.buf[off]     = color.b;
        self.buf[off + 1] = color.g;
        self.buf[off + 2] = color.r;
        self.buf[off + 3] = 0xFF;
    }

    fn fill_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        let x2 = (x + w).min(self.width);
        let y2 = (y + h).min(self.height);
        for py in y..y2 {
            for px in x..x2 {
                self.put_pixel(px, py, color);
            }
        }
    }

    fn draw_hline(&mut self, x: u32, y: u32, w: u32, color: Color) {
        self.fill_rect(x, y, w, 1, color);
    }

    fn draw_vline(&mut self, x: u32, y: u32, h: u32, color: Color) {
        self.fill_rect(x, y, 1, h, color);
    }

    fn draw_rect_outline(&mut self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        self.draw_hline(x, y, w, color);
        self.draw_hline(x, y + h.saturating_sub(1), w, color);
        self.draw_vline(x, y, h, color);
        self.draw_vline(x + w.saturating_sub(1), y, h, color);
    }

    /// Draw an 8×8 character from the built-in font.
    fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let glyph = FONT_DATA[if ch >= 32 && ch < 128 { (ch - 32) as usize } else { 0 }];
        for row in 0u32..8 {
            for col in 0u32..8 {
                let bit = (glyph[row as usize] >> (7 - col)) & 1;
                let color = if bit != 0 { fg } else { bg };
                self.put_pixel(x + col, y + row, color);
            }
        }
    }

    /// Draw a string (ASCII only, 8×8 font).
    fn draw_str(&mut self, x: u32, y: u32, s: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for ch in s.bytes() {
            if cx + FONT_W > self.width { break; }
            self.draw_char(cx, y, ch, fg, bg);
            cx += FONT_W;
        }
    }

    /// Draw a string, right-aligned to `right_edge`.
    fn draw_str_right(&mut self, right_edge: u32, y: u32, s: &str, fg: Color, bg: Color) {
        let total_w = (s.len() as u32) * FONT_W;
        let x = right_edge.saturating_sub(total_w);
        self.draw_str(x, y, s, fg, bg);
    }

    /// Draw a button: filled rect + centered text.
    fn draw_button(&mut self, x: u32, y: u32, w: u32, h: u32, label: &str, bg: Color, fg: Color, hovered: bool) {
        let btn_bg = if hovered {
            Color::new(
                (bg.r as u16).saturating_add(20) as u8,
                (bg.g as u16).saturating_add(20) as u8,
                (bg.b as u16).saturating_add(20) as u8,
            )
        } else {
            bg
        };
        self.fill_rect(x, y, w, h, btn_bg);
        self.draw_rect_outline(x, y, w, h, COLOR_BORDER);

        let text_w = (label.len() as u32) * FONT_W;
        let text_x = x + (w.saturating_sub(text_w)) / 2;
        let text_y = y + (h.saturating_sub(FONT_H)) / 2;
        self.draw_str(text_x, text_y, label, fg, btn_bg);
    }

    /// Draw a progress bar (used for BPP indicator — future use).
    #[allow(dead_code)]
    fn draw_progress(&mut self, x: u32, y: u32, w: u32, h: u32, value: u32, max: u32, color: Color) {
        self.fill_rect(x, y, w, h, COLOR_PANEL);
        self.draw_rect_outline(x, y, w, h, COLOR_BORDER);
        if max > 0 && value > 0 {
            let fill_w = (value * (w - 2)) / max;
            self.fill_rect(x + 1, y + 1, fill_w, h - 2, color);
        }
    }
}

// ============================================================================
// UI State
// ============================================================================

/// Identifies which UI element has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    ModeList,
    BtnApply,
    BtnDefault,
}

struct DisplaySettingsState {
    /// Available video modes (from kernel)
    modes: Vec<VideoModeEntry>,
    /// Index of the currently highlighted entry in the list
    selected_idx: usize,
    /// Scroll offset (index of the first visible mode in the list)
    scroll_offset: usize,
    /// Boot-time (default) resolution
    default_mode: VideoModeEntry,
    /// Currently applied resolution (at startup = boot resolution)
    current_mode: VideoModeEntry,
    /// UI focus
    focus: Focus,
    /// Whether an error message should be shown
    show_error: bool,
    /// Whether a success message should be shown
    show_success: bool,
    /// Ticks remaining to display the status message
    status_ticks: u32,
    /// Status message content
    status_msg: &'static str,
}

impl DisplaySettingsState {
    fn new() -> Self {
        Self {
            modes: Vec::new(),
            selected_idx: 0,
            scroll_offset: 0,
            default_mode: VideoModeEntry::default(),
            current_mode: VideoModeEntry::default(),
            focus: Focus::ModeList,
            show_error: false,
            show_success: false,
            status_ticks: 0,
            status_msg: "",
        }
    }

    /// Move selection up
    fn select_up(&mut self) {
        if self.selected_idx > 0 {
            self.selected_idx -= 1;
            if self.selected_idx < self.scroll_offset {
                self.scroll_offset = self.selected_idx;
            }
        }
    }

    /// Move selection down
    fn select_down(&mut self) {
        if self.selected_idx + 1 < self.modes.len() {
            self.selected_idx += 1;
            if self.selected_idx >= self.scroll_offset + VISIBLE_MODES {
                self.scroll_offset = self.selected_idx + 1 - VISIBLE_MODES;
            }
        }
    }

    fn selected_mode(&self) -> Option<&VideoModeEntry> {
        self.modes.get(self.selected_idx)
    }

    /// Apply the currently selected mode.
    fn apply_selected(&mut self) {
        if let Some(&m) = self.selected_mode() {
            match set_video_mode(m.width as u16, m.height as u16, m.bpp as u8) {
                Ok(()) => {
                    self.current_mode = m;
                    self.show_success = true;
                    self.show_error = false;
                    self.status_msg = "Resolution applied successfully.";
                    self.status_ticks = 120; // ~2 seconds at ~60 fps
                }
                Err(_) => {
                    self.show_error = true;
                    self.show_success = false;
                    self.status_msg = "Failed to apply mode. BGA required.";
                    self.status_ticks = 120;
                }
            }
        }
    }

    /// Restore the default (boot) resolution.
    fn restore_default(&mut self) {
        let d = self.default_mode;
        match set_video_mode(d.width as u16, d.height as u16, d.bpp as u8) {
            Ok(()) => {
                self.current_mode = d;
                // Find and select the default mode in the list
                if let Some(idx) = self.modes.iter().position(|m| m.width == d.width && m.height == d.height) {
                    self.selected_idx = idx;
                    if idx < self.scroll_offset || idx >= self.scroll_offset + VISIBLE_MODES {
                        self.scroll_offset = idx.saturating_sub(VISIBLE_MODES / 2);
                    }
                }
                self.show_success = true;
                self.show_error = false;
                self.status_msg = "Default resolution restored.";
                self.status_ticks = 120;
            }
            Err(_) => {
                self.show_error = true;
                self.show_success = false;
                self.status_msg = "Failed to restore default.";
                self.status_ticks = 120;
            }
        }
    }

    fn tick(&mut self) {
        if self.status_ticks > 0 {
            self.status_ticks -= 1;
            if self.status_ticks == 0 {
                self.show_error = false;
                self.show_success = false;
            }
        }
    }
}

// ============================================================================
// Rendering
// ============================================================================

/// Format a u32 as a decimal string into a fixed-size buffer.
/// Returns the slice of the buffer that was written.
fn u32_to_str(buf: &mut [u8; 16], n: u32) -> &str {
    if n == 0 {
        buf[0] = b'0';
        return core::str::from_utf8(&buf[..1]).unwrap_or("0");
    }
    let mut tmp = [0u8; 16];
    let mut len = 0;
    let mut v = n;
    while v > 0 {
        tmp[len] = b'0' + (v % 10) as u8;
        len += 1;
        v /= 10;
    }
    for i in 0..len {
        buf[i] = tmp[len - 1 - i];
    }
    core::str::from_utf8(&buf[..len]).unwrap_or("?")
}

/// Render the full display settings window into the backbuffer.
fn render(canvas: &mut Canvas, state: &DisplaySettingsState) {
    // ── Background ─────────────────────────────────────────────────────────
    canvas.fill_rect(0, 0, canvas.width, canvas.height, COLOR_BG);

    // ── Window shadow (simple offset rect) ─────────────────────────────────
    canvas.fill_rect(WINDOW_X + 4, WINDOW_Y + 4, WINDOW_W, WINDOW_H, Color::new(20, 22, 28));

    // ── Window background ──────────────────────────────────────────────────
    canvas.fill_rect(WINDOW_X, WINDOW_Y, WINDOW_W, WINDOW_H, COLOR_PANEL);
    canvas.draw_rect_outline(WINDOW_X, WINDOW_Y, WINDOW_W, WINDOW_H, COLOR_BORDER);

    // ── Title bar ──────────────────────────────────────────────────────────
    canvas.fill_rect(WINDOW_X, WINDOW_Y, WINDOW_W, TITLE_H, COLOR_TITLEBAR);
    canvas.draw_str(WINDOW_X + 12, WINDOW_Y + 10, "Display Settings", COLOR_ACCENT, COLOR_TITLEBAR);
    // Title bar bottom border
    canvas.draw_hline(WINDOW_X, WINDOW_Y + TITLE_H, WINDOW_W, COLOR_BORDER);

    let content_y = WINDOW_Y + TITLE_H + PANEL_MARGIN;
    let content_x = WINDOW_X + PANEL_MARGIN;
    let content_w = WINDOW_W - PANEL_MARGIN * 2;

    // ── Current resolution info ────────────────────────────────────────────
    canvas.draw_str(content_x, content_y, "Current:", COLOR_DIM_TEXT, COLOR_PANEL);

    let cm = &state.current_mode;
    let mut buf1 = [0u8; 16];
    let mut buf2 = [0u8; 16];
    let w_str = u32_to_str(&mut buf1, cm.width);
    let h_str = u32_to_str(&mut buf2, cm.height);

    // Build "WIDTHxHEIGHT" string manually
    let mut cur_label = [0u8; 32];
    let mut cur_len = 0;
    for b in w_str.bytes() { cur_label[cur_len] = b; cur_len += 1; }
    cur_label[cur_len] = b'x'; cur_len += 1;
    for b in h_str.bytes() { cur_label[cur_len] = b; cur_len += 1; }
    let cur_str = core::str::from_utf8(&cur_label[..cur_len]).unwrap_or("?x?");

    canvas.draw_str(content_x + 72, content_y, cur_str, COLOR_SEL_TEXT, COLOR_PANEL);
    canvas.draw_str(content_x + 72 + (cur_len as u32 * FONT_W) + 8,
        content_y, "@ 32bpp", COLOR_DIM_TEXT, COLOR_PANEL);

    // BGA availability indicator
    let bga_label = if state.default_mode.flags != 0 { "BGA active" } else { "GOP only" };
    let bga_color = if state.default_mode.flags != 0 { COLOR_BTN_APPLY } else { COLOR_WARN };
    canvas.draw_str_right(WINDOW_X + WINDOW_W - PANEL_MARGIN,
        content_y, bga_label, bga_color, COLOR_PANEL);

    // ── Separator ─────────────────────────────────────────────────────────
    let sep_y = content_y + FONT_H + 8;
    canvas.draw_hline(content_x, sep_y, content_w, COLOR_BORDER);

    // ── Mode list label ────────────────────────────────────────────────────
    let list_label_y = sep_y + 8;
    canvas.draw_str(content_x, list_label_y, "Available Resolutions:", COLOR_DIM_TEXT, COLOR_PANEL);

    // ── Mode list ──────────────────────────────────────────────────────────
    let list_y = list_label_y + FONT_H + 6;
    let item_h: u32 = 22;
    let list_h: u32 = item_h * VISIBLE_MODES as u32;
    let list_w: u32 = content_w - 24;

    // List background + border
    canvas.fill_rect(content_x, list_y, list_w, list_h, COLOR_TITLEBAR);
    canvas.draw_rect_outline(content_x, list_y, list_w, list_h, COLOR_BORDER);

    let visible_end = (state.scroll_offset + VISIBLE_MODES).min(state.modes.len());

    for (vis_i, mode_i) in (state.scroll_offset..visible_end).enumerate() {
        let m = &state.modes[mode_i];
        let item_y = list_y + vis_i as u32 * item_h;
        let is_selected = mode_i == state.selected_idx;
        let is_current = m.width == state.current_mode.width && m.height == state.current_mode.height;

        let bg = if is_selected { COLOR_SELECTED } else { COLOR_TITLEBAR };
        let fg = if is_selected { COLOR_SEL_TEXT } else { COLOR_TEXT };

        canvas.fill_rect(content_x + 1, item_y + 1, list_w - 2, item_h - 2, bg);

        // Mode label: "1920x1080"
        let (label_bytes, label_len) = m.label_bytes();
        let label = core::str::from_utf8(&label_bytes[..label_len]).unwrap_or("?");
        canvas.draw_str(content_x + 8, item_y + (item_h - FONT_H) / 2, label, fg, bg);

        // "current" badge on the right
        if is_current {
            let badge = "[current]";
            let bx = content_x + list_w - (badge.len() as u32 * FONT_W) - 8;
            canvas.draw_str(bx, item_y + (item_h - FONT_H) / 2, badge, COLOR_ACCENT, bg);
        }

        // Separator line between items
        if vis_i + 1 < VISIBLE_MODES && mode_i + 1 < visible_end {
            canvas.draw_hline(content_x + 1, item_y + item_h - 1, list_w - 2, COLOR_BORDER);
        }
    }

    // Scrollbar
    if state.modes.len() > VISIBLE_MODES {
        let sb_x = content_x + list_w;
        let sb_w: u32 = 12;
        canvas.fill_rect(sb_x, list_y, sb_w, list_h, COLOR_TITLEBAR);
        canvas.draw_rect_outline(sb_x, list_y, sb_w, list_h, COLOR_BORDER);

        let thumb_h = (list_h * VISIBLE_MODES as u32) / state.modes.len() as u32;
        let thumb_h = thumb_h.max(8);
        let thumb_range = list_h.saturating_sub(thumb_h);
        let thumb_y = list_y + (thumb_range * state.scroll_offset as u32)
            / (state.modes.len().saturating_sub(VISIBLE_MODES) as u32).max(1);

        canvas.fill_rect(sb_x + 2, thumb_y, sb_w - 4, thumb_h, COLOR_BORDER);
    }

    // ── Keyboard hints ─────────────────────────────────────────────────────
    let hints_y = list_y + list_h + 10;
    canvas.draw_str(content_x, hints_y,
        "Arrow keys: select   Enter: apply   Esc: cancel   D: default",
        COLOR_DIM_TEXT, COLOR_PANEL);

    // ── Buttons ────────────────────────────────────────────────────────────
    let btn_y = hints_y + FONT_H + 12;
    let btn_h: u32 = 28;
    let btn_w: u32 = 120;
    let btn_gap: u32 = 12;

    // Apply button
    let apply_hovered = state.focus == Focus::BtnApply;
    canvas.draw_button(content_x, btn_y, btn_w, btn_h,
        "Apply", COLOR_BTN_APPLY, COLOR_BTN_TEXT, apply_hovered);

    // Restore Default button
    let default_hovered = state.focus == Focus::BtnDefault;
    canvas.draw_button(content_x + btn_w + btn_gap, btn_y, btn_w + 20, btn_h,
        "Restore Default", COLOR_BTN_DEFAULT, COLOR_BTN_TEXT, default_hovered);

    // Focus indicator (outline on focused button)
    if apply_hovered {
        canvas.draw_rect_outline(content_x - 1, btn_y - 1, btn_w + 2, btn_h + 2, COLOR_ACCENT);
    }
    if default_hovered {
        canvas.draw_rect_outline(content_x + btn_w + btn_gap - 1, btn_y - 1, btn_w + 22, btn_h + 2, COLOR_ACCENT);
    }

    // ── Status message ─────────────────────────────────────────────────────
    if state.show_success || state.show_error {
        let msg_y = btn_y + btn_h + 12;
        let msg_bg = if state.show_success { COLOR_BTN_APPLY } else { COLOR_WARN };
        let msg_x = content_x;
        let msg_w = content_w;

        canvas.fill_rect(msg_x, msg_y, msg_w, FONT_H + 8, msg_bg);
        canvas.draw_str(msg_x + 8, msg_y + 4, state.status_msg, COLOR_BTN_TEXT, msg_bg);
    }

    // ── Window decorations (corner dots) ───────────────────────────────────
    canvas.put_pixel(WINDOW_X, WINDOW_Y, COLOR_BORDER);
    canvas.put_pixel(WINDOW_X + WINDOW_W - 1, WINDOW_Y, COLOR_BORDER);
    canvas.put_pixel(WINDOW_X, WINDOW_Y + WINDOW_H - 1, COLOR_BORDER);
    canvas.put_pixel(WINDOW_X + WINDOW_W - 1, WINDOW_Y + WINDOW_H - 1, COLOR_BORDER);
}

// ============================================================================
// Main
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("DisplaySettings: Starting...");

    // Register as a service so other processes can find/activate us
    if let Ok(port) = create_port() {
        let _ = register_service("display_settings", port);
    }

    // Acquire framebuffer
    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("DisplaySettings: No framebuffer available");
            exit(1);
        }
    };

    let fb_w = fb.width();
    let fb_h = fb.height();
    let fb_bpp = fb.bytes_per_pixel();

    log("DisplaySettings: Framebuffer acquired");

    // Query current video mode from kernel
    let current_info = get_current_video_mode();
    let initial_mode = VideoModeEntry {
        width:  current_info.as_ref().map_or(fb_w, |m| m.width),
        height: current_info.as_ref().map_or(fb_h, |m| m.height),
        bpp:    32,
        refresh_rate: 60,
        // flags bit 0 = 1 if BGA active; use bga_available from info
        flags: current_info.as_ref().map_or(0, |m| m.bga_available),
    };

    // Build UI state
    let mut state = DisplaySettingsState::new();
    state.default_mode = initial_mode;
    state.current_mode = initial_mode;

    // Load available modes from kernel
    let mode_count = video_mode_count().max(1).min(64);
    let mut mode_buf = alloc::vec![VideoModeEntry::default(); mode_count];
    let actual_count = get_video_modes(&mut mode_buf);
    mode_buf.truncate(actual_count);
    state.modes = mode_buf;

    // Select the entry closest to the current resolution
    if let Some(idx) = state.modes.iter().position(|m|
        m.width == initial_mode.width && m.height == initial_mode.height)
    {
        state.selected_idx = idx;
        state.scroll_offset = idx.saturating_sub(VISIBLE_MODES / 2);
    }

    log("DisplaySettings: Mode list loaded, entering render loop");

    // Allocate backbuffer
    let buf_size = fb_w as usize * fb_h as usize * fb_bpp;
    let mut backbuf: Vec<u8> = alloc::vec![0u8; buf_size];

    // Main render/event loop
    // In a real system, input would come from the keyboard driver via IPC.
    // Here we poll the kernel input buffer directly.
    let mut frame: u64 = 0;

    loop {
        // ── Input handling ────────────────────────────────────────────────
        // Poll for keyboard input (non-blocking)
        let mut key_buf = [0u8; 1];
        let key_opt = poll_key(&mut key_buf);

        if let Some(key) = key_opt {
            handle_key(&mut state, key);
        }

        state.tick();

        // ── Rendering ─────────────────────────────────────────────────────
        // Redraw every frame (simple approach; a real compositor would only
        // redraw on dirty regions).
        {
            let mut canvas = Canvas::new(&mut backbuf, fb_w, fb_h);
            render(&mut canvas, &state);
        }

        // Blit backbuffer → LFB
        blit_to_lfb(&fb, &backbuf);

        frame = frame.wrapping_add(1);

        // Yield to allow other processes to run.
        // Rate limiting: approximately 30 FPS (yield twice per frame).
        yield_now();
        yield_now();
    }
}

/// Poll for a keyboard scancode. Returns the ASCII character if one is available.
/// This is a simplified implementation; a production version would decode
/// scancodes fully and handle modifiers (Shift, Ctrl, Alt).
fn poll_key(buf: &mut [u8; 1]) -> Option<u8> {
    // Use the atom_syscall input::poll_scancode function
    // which calls SYS_KEYBOARD_POLL internally.
    let raw = atom_syscall::input::keyboard_poll()?;

    // Only process key-press events (bit 7 clear = press; bit 7 set = release)
    if raw & 0x80 != 0 { return None; }

    // Convert common PS/2 scancodes to ASCII-like values for our input handler
    let ascii = match raw {
        0x48 => b'U',   // Up arrow   → move selection up
        0x50 => b'D',   // Down arrow → move selection down
        0x1C => b'\r',  // Enter      → apply selected
        0x01 => b'\x1B',// Escape     → exit
        0x20 => b'd',   // 'd' key    → restore default
        0x21 => b'f',   // 'f' key    → cycle focus
        0x0F => b'\t',  // Tab        → cycle focus
        _ => return None,
    };

    buf[0] = ascii;
    Some(ascii)
}

/// Handle a decoded input character.
fn handle_key(state: &mut DisplaySettingsState, key: u8) {
    match key {
        b'U' => {
            // Up arrow
            match state.focus {
                Focus::ModeList => state.select_up(),
                Focus::BtnApply | Focus::BtnDefault => state.focus = Focus::ModeList,
            }
        }
        b'D' => {
            // Down arrow (also 'd' for default is handled separately)
            match state.focus {
                Focus::ModeList => state.select_down(),
                _ => {}
            }
        }
        b'\r' => {
            // Enter
            match state.focus {
                Focus::ModeList | Focus::BtnApply => state.apply_selected(),
                Focus::BtnDefault => state.restore_default(),
            }
        }
        b'\x1B' => {
            // Escape — exit the app
            log("DisplaySettings: User closed settings");
            exit(0);
        }
        b'd' => {
            // 'd' key — restore default
            state.restore_default();
        }
        b'\t' | b'f' => {
            // Tab or 'f' — cycle focus
            state.focus = match state.focus {
                Focus::ModeList   => Focus::BtnApply,
                Focus::BtnApply   => Focus::BtnDefault,
                Focus::BtnDefault => Focus::ModeList,
            };
        }
        _ => {}
    }
}

// ============================================================================
// Panic Handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("DisplaySettings: PANIC");
    exit(0xFF);
}

// ============================================================================
// Embedded 8×8 Font (96 printable ASCII glyphs, indices 0..95 = ' '..'~')
// ============================================================================
// This is the same PC BIOS 8×8 font used throughout Atom OS.
// Each glyph is 8 bytes; bit 7 of each byte is the leftmost pixel.

const FONT_DATA: [[u8; 8]; 96] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // ' '
    [0x18, 0x3C, 0x3C, 0x18, 0x18, 0x00, 0x18, 0x00], // '!'
    [0x36, 0x36, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // '"'
    [0x36, 0x36, 0x7F, 0x36, 0x7F, 0x36, 0x36, 0x00], // '#'
    [0x0C, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x0C, 0x00], // '$'
    [0x00, 0x63, 0x33, 0x18, 0x0C, 0x66, 0x63, 0x00], // '%'
    [0x1C, 0x36, 0x1C, 0x6E, 0x3B, 0x33, 0x6E, 0x00], // '&'
    [0x06, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00], // '''
    [0x18, 0x0C, 0x06, 0x06, 0x06, 0x0C, 0x18, 0x00], // '('
    [0x06, 0x0C, 0x18, 0x18, 0x18, 0x0C, 0x06, 0x00], // ')'
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00], // '*'
    [0x00, 0x0C, 0x0C, 0x3F, 0x0C, 0x0C, 0x00, 0x00], // '+'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ','
    [0x00, 0x00, 0x00, 0x3F, 0x00, 0x00, 0x00, 0x00], // '-'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C, 0x00], // '.'
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // '/'
    [0x3E, 0x63, 0x73, 0x7B, 0x6F, 0x67, 0x3E, 0x00], // '0'
    [0x0C, 0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x3F, 0x00], // '1'
    [0x1E, 0x33, 0x30, 0x1C, 0x06, 0x33, 0x3F, 0x00], // '2'
    [0x1E, 0x33, 0x30, 0x1C, 0x30, 0x33, 0x1E, 0x00], // '3'
    [0x38, 0x3C, 0x36, 0x33, 0x7F, 0x30, 0x78, 0x00], // '4'
    [0x3F, 0x03, 0x1F, 0x30, 0x30, 0x33, 0x1E, 0x00], // '5'
    [0x1C, 0x06, 0x03, 0x1F, 0x33, 0x33, 0x1E, 0x00], // '6'
    [0x3F, 0x33, 0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x00], // '7'
    [0x1E, 0x33, 0x33, 0x1E, 0x33, 0x33, 0x1E, 0x00], // '8'
    [0x1E, 0x33, 0x33, 0x3E, 0x30, 0x18, 0x0E, 0x00], // '9'
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x00], // ':'
    [0x00, 0x0C, 0x0C, 0x00, 0x00, 0x0C, 0x0C, 0x06], // ';'
    [0x18, 0x0C, 0x06, 0x03, 0x06, 0x0C, 0x18, 0x00], // '<'
    [0x00, 0x00, 0x3F, 0x00, 0x00, 0x3F, 0x00, 0x00], // '='
    [0x06, 0x0C, 0x18, 0x30, 0x18, 0x0C, 0x06, 0x00], // '>'
    [0x1E, 0x33, 0x30, 0x18, 0x0C, 0x00, 0x0C, 0x00], // '?'
    [0x3E, 0x63, 0x7B, 0x7B, 0x7B, 0x03, 0x1E, 0x00], // '@'
    [0x0C, 0x1E, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x00], // 'A'
    [0x3F, 0x66, 0x66, 0x3E, 0x66, 0x66, 0x3F, 0x00], // 'B'
    [0x3C, 0x66, 0x03, 0x03, 0x03, 0x66, 0x3C, 0x00], // 'C'
    [0x1F, 0x36, 0x66, 0x66, 0x66, 0x36, 0x1F, 0x00], // 'D'
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x46, 0x7F, 0x00], // 'E'
    [0x7F, 0x46, 0x16, 0x1E, 0x16, 0x06, 0x0F, 0x00], // 'F'
    [0x3C, 0x66, 0x03, 0x03, 0x73, 0x66, 0x7C, 0x00], // 'G'
    [0x33, 0x33, 0x33, 0x3F, 0x33, 0x33, 0x33, 0x00], // 'H'
    [0x1E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // 'I'
    [0x78, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E, 0x00], // 'J'
    [0x67, 0x66, 0x36, 0x1E, 0x36, 0x66, 0x67, 0x00], // 'K'
    [0x0F, 0x06, 0x06, 0x06, 0x46, 0x66, 0x7F, 0x00], // 'L'
    [0x63, 0x77, 0x7F, 0x7F, 0x6B, 0x63, 0x63, 0x00], // 'M'
    [0x63, 0x67, 0x6F, 0x7B, 0x73, 0x63, 0x63, 0x00], // 'N'
    [0x1C, 0x36, 0x63, 0x63, 0x63, 0x36, 0x1C, 0x00], // 'O'
    [0x3F, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x0F, 0x00], // 'P'
    [0x1E, 0x33, 0x33, 0x33, 0x3B, 0x1E, 0x38, 0x00], // 'Q'
    [0x3F, 0x66, 0x66, 0x3E, 0x36, 0x66, 0x67, 0x00], // 'R'
    [0x1E, 0x33, 0x07, 0x0E, 0x38, 0x33, 0x1E, 0x00], // 'S'
    [0x3F, 0x2D, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // 'T'
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x3F, 0x00], // 'U'
    [0x33, 0x33, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // 'V'
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // 'W'
    [0x63, 0x63, 0x36, 0x1C, 0x1C, 0x36, 0x63, 0x00], // 'X'
    [0x33, 0x33, 0x33, 0x1E, 0x0C, 0x0C, 0x1E, 0x00], // 'Y'
    [0x7F, 0x63, 0x31, 0x18, 0x4C, 0x66, 0x7F, 0x00], // 'Z'
    [0x1E, 0x06, 0x06, 0x06, 0x06, 0x06, 0x1E, 0x00], // '['
    [0x03, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x40, 0x00], // '\'
    [0x1E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x1E, 0x00], // ']'
    [0x08, 0x1C, 0x36, 0x63, 0x00, 0x00, 0x00, 0x00], // '^'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF], // '_'
    [0x0C, 0x0C, 0x18, 0x00, 0x00, 0x00, 0x00, 0x00], // '`'
    [0x00, 0x00, 0x1E, 0x30, 0x3E, 0x33, 0x6E, 0x00], // 'a'
    [0x07, 0x06, 0x06, 0x3E, 0x66, 0x66, 0x3B, 0x00], // 'b'
    [0x00, 0x00, 0x1E, 0x33, 0x03, 0x33, 0x1E, 0x00], // 'c'
    [0x38, 0x30, 0x30, 0x3e, 0x33, 0x33, 0x6E, 0x00], // 'd'
    [0x00, 0x00, 0x1E, 0x33, 0x3f, 0x03, 0x1E, 0x00], // 'e'
    [0x1C, 0x36, 0x06, 0x0f, 0x06, 0x06, 0x0F, 0x00], // 'f'
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x1F], // 'g'
    [0x07, 0x06, 0x36, 0x6E, 0x66, 0x66, 0x67, 0x00], // 'h'
    [0x0C, 0x00, 0x0E, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // 'i'
    [0x30, 0x00, 0x30, 0x30, 0x30, 0x33, 0x33, 0x1E], // 'j'
    [0x07, 0x06, 0x66, 0x36, 0x1E, 0x36, 0x67, 0x00], // 'k'
    [0x0E, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x1E, 0x00], // 'l'
    [0x00, 0x00, 0x33, 0x7F, 0x7F, 0x6B, 0x63, 0x00], // 'm'
    [0x00, 0x00, 0x1F, 0x33, 0x33, 0x33, 0x33, 0x00], // 'n'
    [0x00, 0x00, 0x1E, 0x33, 0x33, 0x33, 0x1E, 0x00], // 'o'
    [0x00, 0x00, 0x3B, 0x66, 0x66, 0x3E, 0x06, 0x0F], // 'p'
    [0x00, 0x00, 0x6E, 0x33, 0x33, 0x3E, 0x30, 0x78], // 'q'
    [0x00, 0x00, 0x3B, 0x6E, 0x66, 0x06, 0x0F, 0x00], // 'r'
    [0x00, 0x00, 0x3E, 0x03, 0x1E, 0x30, 0x1F, 0x00], // 's'
    [0x08, 0x0C, 0x3E, 0x0C, 0x0C, 0x2C, 0x18, 0x00], // 't'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x33, 0x6E, 0x00], // 'u'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x1E, 0x0C, 0x00], // 'v'
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x7F, 0x36, 0x00], // 'w'
    [0x00, 0x00, 0x63, 0x36, 0x1C, 0x36, 0x63, 0x00], // 'x'
    [0x00, 0x00, 0x33, 0x33, 0x33, 0x3E, 0x30, 0x1F], // 'y'
    [0x00, 0x00, 0x3F, 0x19, 0x0C, 0x26, 0x3F, 0x00], // 'z'
    [0x38, 0x0C, 0x0C, 0x07, 0x0C, 0x0C, 0x38, 0x00], // '{'
    [0x18, 0x18, 0x18, 0x00, 0x18, 0x18, 0x18, 0x00], // '|'
    [0x07, 0x0C, 0x0C, 0x38, 0x0C, 0x0C, 0x07, 0x00], // '}'
    [0x6E, 0x3B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // '~'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // DEL (unused)
];
