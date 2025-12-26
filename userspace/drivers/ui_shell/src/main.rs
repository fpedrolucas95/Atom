//! Atom Desktop Environment
//!
//! This is the userspace compositor and window manager for Atom OS.
//! It is the sole authority on UI policy, responsible for:
//! - Window management and composition
//! - Focus management
//! - Input routing from drivers to applications
//! - Application launching
//!
//! # Architecture
//!
//! The desktop environment receives input events from userspace drivers
//! (keyboard and mouse) via IPC, routes them to the focused application,
//! and composites window surfaces to the framebuffer.
//!
//! ```text
//! +----------------+     +----------------+
//! | Keyboard       |---->|                |
//! | Driver         |     |                |
//! +----------------+     |    Desktop     |---> Framebuffer
//!                        |  Environment   |
//! +----------------+     |                |
//! | Mouse          |---->|                |
//! | Driver         |     +-------+--------+
//! +----------------+             |
//!                                v
//!                    +----------------------+
//!                    | Applications (IPC)   |
//!                    +----------------------+
//! ```

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use atom_syscall::graphics::{Color, Framebuffer};
use atom_syscall::input::{keyboard_poll, MouseDriver};
use atom_syscall::ipc::{create_port, PortId};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

use libipc::messages::WindowId;

// ============================================================================
// Simple Bump Allocator for userspace
// ============================================================================

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

struct BumpAllocator {
    heap: UnsafeCell<[u8; 1024 * 1024]>, // 1MB heap
    next: UnsafeCell<usize>,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0; 1024 * 1024]),
            next: UnsafeCell::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let next = self.next.get();
        let heap = self.heap.get();
        
        let align = layout.align();
        let size = layout.size();
        
        // Align the next pointer
        let offset = (*next + align - 1) & !(align - 1);
        let new_next = offset + size;
        
        if new_next > (*heap).len() {
            return core::ptr::null_mut();
        }
        
        *next = new_next;
        (*heap).as_mut_ptr().add(offset)
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't support deallocation
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

// ============================================================================
// Theme Colors (Nord-inspired)
// ============================================================================

mod theme {
    use atom_syscall::graphics::Color;

    pub const DESKTOP_BG: Color = Color::new(46, 52, 64);
    pub const PANEL_BG: Color = Color::new(36, 41, 51);
    pub const PANEL_TEXT: Color = Color::new(236, 239, 244);
    pub const ACCENT: Color = Color::new(136, 192, 208);
    pub const WINDOW_BG: Color = Color::new(46, 52, 64);
    pub const WINDOW_HEADER: Color = Color::new(59, 66, 82);
    pub const WINDOW_HEADER_FOCUSED: Color = Color::new(76, 86, 106);
    pub const WINDOW_BORDER: Color = Color::new(67, 76, 94);
    pub const DOCK_BG: Color = Color::new(36, 41, 51);
    pub const CURSOR_FILL: Color = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;
}

// ============================================================================
// Window Management
// ============================================================================

/// Application type for tracking running processes
#[derive(Clone, Copy, PartialEq, Eq)]
enum AppType {
    Static,      // Static window (like Welcome)
    Terminal,    // Terminal application
}

/// Window state in the compositor
#[derive(Clone)]
struct Window {
    id: WindowId,
    title: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    visible: bool,
    focused: bool,
    /// IPC port for sending events to the owning application
    event_port: Option<PortId>,
    /// Application type for lifecycle management
    app_type: AppType,
}

impl Window {
    fn new(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            visible: true,
            focused: false,
            event_port: None,
            app_type: AppType::Static,
        }
    }

    fn new_with_type(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32, app_type: AppType) -> Self {
        Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            visible: true,
            focused: false,
            event_port: None,
            app_type,
        }
    }

    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + self.height as i32
    }

    fn header_contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + 24 // Header height
    }
}

/// Window manager state
struct WindowManager {
    windows: Vec<Window>,
    next_id: WindowId,
    focused_id: Option<WindowId>,
    /// Track if terminal is running
    terminal_window_id: Option<WindowId>,
}

impl WindowManager {
    fn new() -> Self {
        Self {
            windows: Vec::new(),
            next_id: 1,
            focused_id: None,
            terminal_window_id: None,
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

    fn create_window_with_type(&mut self, title: &str, x: i32, y: i32, width: u32, height: u32, app_type: AppType) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new_with_type(id, title, x, y, width, height, app_type);
        self.windows.push(window);
        self.focus_window(id);
        
        if app_type == AppType::Terminal {
            self.terminal_window_id = Some(id);
        }
        
        id
    }

    fn focus_window(&mut self, id: WindowId) {
        // Unfocus previous
        if let Some(prev_id) = self.focused_id {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev_id) {
                w.focused = false;
            }
        }

        // Focus new and move to top
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.focused = true;
            self.windows.push(window);
            self.focused_id = Some(id);
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        // Check from top to bottom (reverse order)
        for window in self.windows.iter().rev() {
            if window.visible && window.contains(x, y) {
                return Some(window.id);
            }
        }
        None
    }

    fn close_window(&mut self, id: WindowId) {
        // If closing terminal, clear terminal window tracking
        if self.terminal_window_id == Some(id) {
            self.terminal_window_id = None;
        }
        
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.last().map(|w| w.id);
        }
    }

    fn is_terminal_running(&self) -> bool {
        self.terminal_window_id.is_some()
    }

    fn get_terminal_window_id(&self) -> Option<WindowId> {
        self.terminal_window_id
    }
}

// ============================================================================
// Cursor State
// ============================================================================

/// Dock icon identifier
#[derive(Clone, Copy, PartialEq, Eq)]
enum DockIcon {
    Files,
    Settings,
    Browser,
    Terminal,
}

/// Dock icon position and metadata
struct DockIconInfo {
    icon: DockIcon,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: Color,
    label: &'static str,
}

impl DockIconInfo {
    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x as i32 && py >= self.y as i32
            && px < (self.x + self.width) as i32
            && py < (self.y + self.height) as i32
    }
}

struct CursorState {
    x: i32,
    y: i32,
    saved_region: [u32; 16 * 16],
    saved_x: i32,
    saved_y: i32,
    has_saved: bool,
    visible: bool,
}

impl CursorState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            x: (width / 2) as i32,
            y: (height / 2) as i32,
            saved_region: [0; 16 * 16],
            saved_x: 0,
            saved_y: 0,
            has_saved: false,
            visible: true,
        }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        self.x = (self.x + dx).clamp(0, (width - 1) as i32);
        self.y = (self.y - dy).clamp(0, (height - 1) as i32); // Y inverted in PS/2
    }

    fn save_region(&mut self, fb: &Framebuffer) {
        self.saved_x = self.x;
        self.saved_y = self.y;
        self.has_saved = true;

        let fb_addr = fb.address();
        let stride = fb.stride();
        let bpp = fb.bytes_per_pixel();

        for row in 0..16u32 {
            for col in 0..16u32 {
                let px = (self.x as u32).wrapping_add(col);
                let py = (self.y as u32).wrapping_add(row);

                if px < fb.width() && py < fb.height() {
                    let offset = (py * stride + px) as usize * bpp;
                    let ptr = (fb_addr + offset) as *const u32;
                    self.saved_region[(row * 16 + col) as usize] = unsafe { ptr.read_volatile() };
                }
            }
        }
    }

    fn restore_region(&self, fb: &Framebuffer) {
        if !self.has_saved {
            return;
        }

        let fb_addr = fb.address();
        let stride = fb.stride();
        let bpp = fb.bytes_per_pixel();

        for row in 0..16u32 {
            for col in 0..16u32 {
                let px = (self.saved_x as u32).wrapping_add(col);
                let py = (self.saved_y as u32).wrapping_add(row);

                if px < fb.width() && py < fb.height() {
                    let offset = (py * stride + px) as usize * bpp;
                    let ptr = (fb_addr + offset) as *mut u32;
                    unsafe {
                        ptr.write_volatile(self.saved_region[(row * 16 + col) as usize]);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Compositor
// ============================================================================

struct Compositor {
    fb: Framebuffer,
    wm: WindowManager,
    cursor: CursorState,
    mouse: MouseDriver,
    event_port: PortId,
    dirty: bool,
    dock_icons: Vec<DockIconInfo>,
}

impl Compositor {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();

        // Create IPC port for receiving events
        let event_port = create_port().expect("Failed to create event port");

        Self {
            fb,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            mouse: MouseDriver::new(),
            event_port,
            dirty: true,
            dock_icons: Vec::new(),
        }
    }

    fn run(&mut self) -> ! {
        log("Desktop: Starting compositor");

        // Create initial windows - only Welcome, not Terminal (it will be launched from dock)
        self.wm.create_window("Welcome to Atom", 100, 100, 400, 300);

        // Initialize dock icons
        self.init_dock_icons();

        // Initial draw
        self.draw_all();

        log("Desktop: Entering event loop");

        let mut prev_left = false;

        loop {
            // Process mouse events
            while let Some(event) = self.mouse.poll_event() {
                self.cursor.restore_region(&self.fb);
                self.cursor.apply_delta(event.dx, event.dy, self.fb.width(), self.fb.height());

                // Handle click
                if event.left_button && !prev_left {
                    self.handle_click(self.cursor.x, self.cursor.y);
                }
                prev_left = event.left_button;

                self.cursor.save_region(&self.fb);
                self.draw_cursor();
            }

            // Process keyboard events
            while let Some(scancode) = keyboard_poll() {
                self.handle_key(scancode);
            }

            // Redraw if needed
            if self.dirty {
                self.draw_all();
                self.dirty = false;
            }

            yield_now();
        }
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        // Check if clicking on dock icon
        for icon_info in &self.dock_icons {
            if icon_info.contains(x, y) {
                self.handle_dock_click(icon_info.icon);
                return;
            }
        }

        // Check if clicking on a window
        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                self.wm.focus_window(id);
                self.dirty = true;
            }

            // Check for close button click
            if let Some(w) = self.wm.windows.iter().find(|w| w.id == id) {
                let close_x = w.x + w.width as i32 - 20;
                let close_y = w.y + 6;
                if x >= close_x && x < close_x + 12 && y >= close_y && y < close_y + 12 {
                    self.handle_window_close(id);
                    self.dirty = true;
                }
            }
        }
    }

    fn handle_dock_click(&mut self, icon: DockIcon) {
        match icon {
            DockIcon::Terminal => {
                if self.wm.is_terminal_running() {
                    // Terminal already running - focus it
                    if let Some(id) = self.wm.get_terminal_window_id() {
                        log("Desktop: Focusing existing terminal window");
                        self.wm.focus_window(id);
                        self.dirty = true;
                    }
                } else {
                    // Launch terminal
                    log("Desktop: Launching terminal");
                    self.launch_terminal();
                }
            }
            DockIcon::Files => {
                log("Desktop: Files app not yet implemented");
            }
            DockIcon::Settings => {
                log("Desktop: Settings app not yet implemented");
            }
            DockIcon::Browser => {
                log("Desktop: Browser app not yet implemented");
            }
        }
    }

    fn handle_window_close(&mut self, id: WindowId) {
        log("Desktop: Closing window");
        // TODO: Send close event to application via IPC
        // For now, just remove the window
        self.wm.close_window(id);
    }

    fn launch_terminal(&mut self) {
        // Create a window for the terminal
        let _id = self.wm.create_window_with_type("Terminal", 150, 150, 640, 400, AppType::Terminal);
        log("Desktop: Terminal window created");
        self.dirty = true;
        
        // TODO: Actually spawn the terminal process via service manager
        // For now, we just create the window placeholder
    }

    fn handle_key(&mut self, scancode: u8) {
        // Handle escape to quit
        if scancode == 0x01 {
            log("Desktop: Escape pressed, exiting");
            exit(0);
        }

        // Route to focused window
        if let Some(focused_id) = self.wm.focused_id {
            // Send keyboard event to focused window
            // TODO: Implement IPC message sending to application
            // For now, just log
            if let Some(window) = self.wm.windows.iter().find(|w| w.id == focused_id) {
                if window.app_type == AppType::Terminal {
                    // TODO: Send scancode to terminal via IPC
                }
            }
        }
    }

    fn draw_all(&mut self) {
        self.cursor.restore_region(&self.fb);

        // Desktop background
        self.fb.fill_rect(0, 0, self.fb.width(), self.fb.height(), theme::DESKTOP_BG);

        // Top panel
        self.draw_panel();

        // Windows (bottom to top)
        for window in self.wm.windows.iter() {
            if window.visible {
                self.draw_window(window);
            }
        }

        // Bottom dock
        self.draw_dock();

        // Cursor
        self.cursor.save_region(&self.fb);
        self.draw_cursor();
    }

    fn draw_panel(&self) {
        let width = self.fb.width();

        // Panel background
        self.fb.fill_rect(0, 0, width, 28, theme::PANEL_BG);

        // Logo
        self.fb.draw_string(12, 6, "Atom", theme::ACCENT, theme::PANEL_BG);

        // Status
        self.fb.draw_string(70, 6, "|  Desktop Environment", theme::PANEL_TEXT, theme::PANEL_BG);

        // Clock (right side)
        let clock_x = width.saturating_sub(80);
        self.fb.draw_string(clock_x, 6, "12:00", theme::PANEL_TEXT, theme::PANEL_BG);
    }

    fn draw_window(&self, window: &Window) {
        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        // Shadow
        self.fb.fill_rect(x + 3, y + 3, w, h, Color::new(20, 20, 30));

        // Border
        self.fb.fill_rect(x, y, w, h, theme::WINDOW_BORDER);

        // Window content
        self.fb.fill_rect(x + 1, y + 1, w - 2, h - 2, theme::WINDOW_BG);

        // Header
        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.fb.fill_rect(x + 1, y + 1, w - 2, 22, header_color);

        // Title
        self.fb.draw_string(x + 8, y + 5, &window.title, theme::PANEL_TEXT, header_color);

        // Window controls
        let btn_x = x + w - 18;
        let btn_y = y + 6;
        self.fb.fill_rect(btn_x, btn_y, 10, 10, Color::new(255, 95, 86)); // Close
        self.fb.fill_rect(btn_x - 14, btn_y, 10, 10, Color::new(255, 189, 46)); // Minimize
        self.fb.fill_rect(btn_x - 28, btn_y, 10, 10, Color::new(39, 201, 63)); // Maximize
    }

    fn draw_dock(&mut self) {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 300u32;
        let dock_h = 48u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2);
        let dock_y = height.saturating_sub(dock_h + 10);

        // Dock background with semi-transparency effect (solid for now)
        self.fb.fill_rect(dock_x, dock_y, dock_w, dock_h, theme::DOCK_BG);

        // Calculate and store dock icon positions if not already initialized
        if self.dock_icons.is_empty() {
            self.init_dock_icons();
        }

        // Draw dock icons
        for icon_info in &self.dock_icons {
            self.fb.fill_rect(icon_info.x, icon_info.y, icon_info.width, icon_info.height, icon_info.color);
            
            // Calculate label position to center it
            let label_x = icon_info.x + 8;
            let label_y = icon_info.y + 10;
            self.fb.draw_string(label_x, label_y, icon_info.label, Color::WHITE, icon_info.color);
        }
    }

    fn init_dock_icons(&mut self) {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 300u32;
        let dock_h = 48u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2);
        let dock_y = height.saturating_sub(dock_h + 10);

        let icon_size = 32u32;
        let padding = 16u32;
        let start_x = dock_x + padding;
        let icon_y = dock_y + (dock_h - icon_size) / 2;

        let icons = [
            (DockIcon::Files, Color::new(191, 97, 106), "F"),
            (DockIcon::Settings, Color::new(163, 190, 140), "S"),
            (DockIcon::Browser, Color::new(94, 129, 172), "B"),
            (DockIcon::Terminal, Color::new(80, 80, 80), ">_"),
        ];

        self.dock_icons.clear();
        for (i, (icon, color, label)) in icons.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + padding));
            self.dock_icons.push(DockIconInfo {
                icon: *icon,
                x: ix,
                y: icon_y,
                width: icon_size,
                height: icon_size,
                color: *color,
                label,
            });
        }
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
                if px < self.fb.width() && py < self.fb.height() {
                    match pixel {
                        1 => self.fb.draw_pixel(px, py, theme::CURSOR_OUTLINE),
                        2 => self.fb.draw_pixel(px, py, theme::CURSOR_FILL),
                        _ => {}
                    }
                }
            }
        }
    }
}

// ============================================================================
// Entry Points
// ============================================================================

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
    log("Microkernel architecture - all UI in userspace");

    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("Desktop: Failed to acquire framebuffer");
            exit(1);
        }
    };

    log("Desktop: Framebuffer acquired");

    let mut compositor = Compositor::new(fb);
    compositor.run()
}

// ============================================================================
// Panic Handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Desktop: PANIC!");
    exit(0xFF);
}
