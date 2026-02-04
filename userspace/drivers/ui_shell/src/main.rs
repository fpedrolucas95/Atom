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
#![feature(alloc_error_handler)]

extern crate alloc;

// ============================================================================
// Simple Bump Allocator for Userspace
// ============================================================================

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 1024 * 1024; // 1 MB heap

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }

            if self.next.compare_exchange_weak(
                current, new_next, Ordering::SeqCst, Ordering::Relaxed
            ).is_ok() {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't free - memory is reclaimed when process exits
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use atom_syscall::graphics::{Color, Framebuffer, SharedSurface, SharedRegionId};
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::interrupts::register_irq_handler;
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use atom_syscall::process::{spawn_process, ProcessId};
use atom_syscall::input::{MouseDriver, keyboard_poll, scancode_to_ascii, scancodes};

use libipc::messages::{MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers, MouseMoveEvent, MouseButtonEvent, MouseButton};
use libipc::protocol::send_message_async;
use libipc::well_known;

// ============================================================================
// Theme Colors (Nord-inspired)
// ============================================================================

mod theme {
    use atom_syscall::graphics::Color;

    pub const DESKTOP_BG_DEFAULT: Color = Color::new(46, 52, 64);
    pub const PANEL_BG: Color = Color::new(26, 30, 38);
    pub const PANEL_TEXT: Color = Color::new(216, 222, 233);
    pub const ACCENT: Color = Color::new(136, 192, 208);
    pub const WINDOW_BG: Color = Color::new(43, 48, 59);
    pub const WINDOW_HEADER: Color = Color::new(46, 52, 64);
    pub const WINDOW_HEADER_FOCUSED: Color = Color::new(59, 66, 82);
    pub const WINDOW_BORDER: Color = Color::new(76, 86, 106);
    pub const DOCK_BG: Color = Color::new(26, 30, 38);
    pub const CURSOR_FILL: Color = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;

    pub const SHADOW: Color = Color::new(10, 12, 16);
}

// ============================================================================
// Window Management
// ============================================================================

const WINDOW_HEADER_HEIGHT: u32 = 28;
const WINDOW_BORDER_WIDTH: u32 = 1;
const WINDOW_MIN_WIDTH: u32 = 100;
const WINDOW_MIN_HEIGHT: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
}

/// Window state in the compositor
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
    /// IPC port for sending events to the owning application
    event_port: Option<PortId>,
    /// Process ID of the owning application (if any)
    process_id: Option<ProcessId>,
    /// Shared surface for application rendering (if managed)
    surface: Option<SharedSurface>,
    /// Shared region ID for passing to the application
    surface_region_id: Option<SharedRegionId>,
    /// Whether the content is dirty and needs compositing
    content_dirty: bool,
    /// Previous geometry before maximization
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
            process_id: None,
            surface: None,
            surface_region_id: None,
            content_dirty: false,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        }
    }

    /// Create a window with an associated process and shared surface
    fn new_with_process(
        id: WindowId,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        process_id: ProcessId,
        event_port: PortId,
    ) -> Option<Self> {
        let width = width.max(WINDOW_MIN_WIDTH);
        let height = height.max(WINDOW_MIN_HEIGHT);

        // Calculate content area dimensions (subtract header and borders)
        let content_width = width.saturating_sub(WINDOW_BORDER_WIDTH * 2);
        let content_height = height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH);

        // Create shared surface for the content area
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
            process_id: Some(process_id),
            surface_region_id: Some(region_id),
            surface: Some(surface),
            content_dirty: true,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        })
    }

    /// Get content area position (inside window chrome)
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

/// Window manager state
struct WindowManager {
    /// Windows in z-order (back to front)
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

    /// Create a window with an associated process and shared surface
    fn create_window_with_process(
        &mut self,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        process_id: ProcessId,
        event_port: PortId,
    ) -> Option<WindowId> {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new_with_process(id, title, x, y, width, height, process_id, event_port)?;
        self.windows.push(window);
        self.focus_window(id);
        Some(id)
    }

    /// Get window by ID
    fn get_window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    /// Get mutable window by ID
    fn get_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn focus_window(&mut self, id: WindowId) {
        // Unfocus previous
        if let Some(prev_id) = self.focused_id {
            if prev_id == id {
                // Already focused, move to top anyway
                if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                    let window = self.windows.remove(pos);
                    self.windows.push(window);
                }
                return;
            }
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev_id) {
                w.focused = false;
                let log_msg = alloc::format!("WM: Window Unfocused - ID={}", prev_id);
                log(&log_msg);
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

        // Focus new and move to top (end of vector)
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.focused = true;
            window.visible = true; // Ensure it's visible if focused
            if window.state == WindowState::Minimized {
                window.state = WindowState::Normal;
            }

            let log_msg = alloc::format!("WM: Window Focused - ID={}", id);
            log(&log_msg);
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
        // Check from top to bottom (reverse order)
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

    fn move_window(&mut self, id: WindowId, dx: i32, dy: i32) {
        if let Some(window) = self.get_window_mut(id) {
            if window.state == WindowState::Maximized {
                return; // Can't move maximized window
            }
            window.x += dx;
            window.y += dy;
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

                // Reallocate shared surface if size changed significantly
                // For now, we just mark content dirty and assume app handles resize event
                // In a full implementation, we'd negotiate a new surface here.
                window.content_dirty = true;
            }
        }
    }

    fn toggle_maximize(&mut self, id: WindowId, screen_width: u32, screen_height: u32) {
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
                    window.x = 0;
                    window.y = 28; // Below panel
                    window.width = screen_width;
                    window.height = screen_height - 28; // Subtract panel height
                }
            }
            window.content_dirty = true;
        }
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

// ============================================================================
// Cursor State
// ============================================================================

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

/// Pending window info waiting for application registration
struct PendingWindow {
    pid: ProcessId,
    window_id: WindowId,
}

struct DesktopIcon {
    label: String,
    executable: String,
    x: i32,
    y: i32,
    color: Color,
}

struct ContextMenu {
    x: i32,
    y: i32,
    visible: bool,
    items: Vec<String>,
}

struct WallpaperPicker {
    visible: bool,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    colors: Vec<Color>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOperation {
    None,
    Move { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_x: i32, start_win_y: i32 },
    Resize { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_w: u32, start_win_h: u32 },
}

struct Compositor {
    fb: Framebuffer,
    wm: WindowManager,
    cursor: CursorState,
    event_port: PortId,
    /// Port for receiving application registration messages
    register_port: PortId,
    /// Windows waiting for application to register
    pending_windows: Vec<PendingWindow>,
    dirty: bool,
    /// Mouse button state
    mouse_left_down: bool,
    mouse_right_down: bool,
    /// Current drag operation
    drag_op: DragOperation,
    /// Window that currently has mouse capture
    captured_window: Option<WindowId>,
    /// Unified input drivers
    mouse_driver: MouseDriver,
    keyboard_shift: bool,
    /// Background color
    desktop_bg: Color,
    /// Desktop icons
    icons: Vec<DesktopIcon>,
    /// Global tick counter
    ticks: u32,
    /// Counter for mouse clicks
    click_counter: u32,
    /// Last click info for double-click
    last_click_tick: u32,
    last_click_icon: Option<usize>,
    /// Context menu
    context_menu: ContextMenu,
    /// Wallpaper picker
    wallpaper_picker: WallpaperPicker,
    /// Last time a terminal was spawned to prevent fork-bomb
    last_terminal_spawn_time: u64,
}

impl Compositor {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();

        // Create IPC port for receiving events
        let event_port = create_port().expect("Failed to create event port");
        // Create registration port for applications
        let register_port = create_port().expect("Failed to create registration port");

        log("Compositor: Ports created successfully");

        let mut mouse_driver = MouseDriver::new();
        mouse_driver.init();

        // Register for IRQ notifications (Keyboard = 1, Mouse = 12)
        // This makes the compositor interrupt-driven instead of polling
        if let Err(e) = register_irq_handler(1, event_port) {
            log("Compositor: Failed to register IRQ 1 handler");
        } else {
            log("Compositor: Registered IRQ 1 (keyboard) successfully");
        }

        if let Err(e) = register_irq_handler(12, event_port) {
            log("Compositor: Failed to register IRQ 12 handler");
        } else {
            log("Compositor: Registered IRQ 12 (mouse) successfully");
        }

        let mut icons = Vec::new();
        icons.push(DesktopIcon {
            label: String::from("Rect Demo"),
            executable: String::from("demo_rects"),
            x: 20,
            y: 48,
            color: Color::new(136, 192, 208),
        });
        icons.push(DesktopIcon {
            label: String::from("Text Demo"),
            executable: String::from("demo_text"),
            x: 20,
            y: 128,
            color: Color::new(163, 190, 140),
        });

        Self {
            fb,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            event_port,
            register_port,
            pending_windows: Vec::new(),
            dirty: true,
            mouse_left_down: false,
            mouse_right_down: false,
            drag_op: DragOperation::None,
            captured_window: None,
            mouse_driver,
            keyboard_shift: false,
            desktop_bg: theme::DESKTOP_BG_DEFAULT,
            icons,
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
            last_terminal_spawn_time: 0,
            wallpaper_picker: WallpaperPicker {
                visible: false,
                x: (width as i32 - 300) / 2,
                y: (height as i32 - 200) / 2,
                width: 300,
                height: 200,
                colors: {
                    let mut v = Vec::new();
                    v.push(Color::new(46, 52, 64));   // Nord Night
                    v.push(Color::new(59, 66, 82));   // Nord Night Light
                    v.push(Color::new(76, 86, 106));  // Nord Grey
                    v.push(Color::new(136, 192, 208)); // Nord Frost Blue
                    v.push(Color::new(143, 188, 187)); // Nord Frost Teal
                    v.push(Color::new(163, 190, 140)); // Nord Green
                    v.push(Color::new(191, 97, 106));  // Nord Red
                    v.push(Color::new(208, 135, 112)); // Nord Orange
                    v
                },
            },
        }
    }

    fn run(&mut self) -> ! {
        log("Desktop: Starting compositor");

        // Create initial demo windows (no surface/process)
        self.wm.create_window("Welcome to Atom", 100, 100, 400, 300);

        // Initial draw
        self.draw_all();

        log("Desktop: Entering event loop");

        let mut reg_buffer = [0u8; 64];

        loop {
            // Build list of ports to wait on (avoiding Vec to prevent memory leaks in BumpAllocator)
            let ports = [self.register_port, self.event_port];

            // Poll for application registrations and WM requests
            while let Ok(Some(len)) = try_recv(self.register_port, &mut reg_buffer) {
                self.handle_register_message(&reg_buffer[..len]);
            }

            // Poll for application events (e.g., SurfacePresent, Mouse, Keyboard)
            let mut event_buffer = [0u8; 64];
            while let Ok(Some(len)) = try_recv(self.event_port, &mut event_buffer) {
                self.handle_app_event(&event_buffer[..len]);
            }

            // Keyboard and Mouse events are received via IPC from drivers
            // (handled in handle_app_event)

            // Poll for unified input (keyboard and mouse)
            self.poll_input();

            // Redraw if needed
            if self.dirty {
                self.draw_all();
                self.dirty = false;
            }

            // Block waiting for IPC messages instead of busy-wait with yield_now()
            // Timeout reduced to 10ms for more responsive local input polling
            let _ = wait_any(&ports, 10);
            self.ticks = self.ticks.wrapping_add(1);
        }
    }

    /// Poll keyboard and mouse directly from kernel buffers
    fn poll_input(&mut self) {
        // Poll mouse
        while let Some(event) = self.mouse_driver.poll_event() {
            // Update cursor position
            self.cursor.restore_region(&self.fb);
            self.cursor.apply_delta(event.dx, event.dy, self.fb.width(), self.fb.height());
            self.cursor.save_region(&self.fb);
            self.draw_cursor();

            // Handle button states
            if event.left_button {
                if !self.mouse_left_down {
                    self.click_counter = self.click_counter.wrapping_add(1);
                    self.handle_click(self.cursor.x, self.cursor.y);
                } else {
                    self.handle_mouse_drag(self.cursor.x, self.cursor.y);
                }

                // If not dragging shell elements, still dispatch move to app
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
                    // Just mouse movement, route to window under cursor
                    self.dispatch_mouse_move(self.cursor.x, self.cursor.y, event.dx as i16, event.dy as i16);
                }
                self.mouse_left_down = false;
                self.mouse_right_down = false;
            }
        }

        // Poll keyboard
        while let Some(scancode) = keyboard_poll() {
            let pressed = (scancode & 0x80) == 0;
            let code = scancode & 0x7F;

            match code {
                scancodes::LEFT_SHIFT | scancodes::RIGHT_SHIFT => {
                    self.keyboard_shift = pressed;
                }
                scancodes::ESCAPE if pressed => {
                    log("Desktop: Escape pressed, exiting");
                    exit(0);
                }
                _ if pressed => {
                    if let Some(ascii) = scancode_to_ascii(code, self.keyboard_shift) {
                        self.dispatch_key_event(scancode, ascii as u8);
                    }
                }
                _ => {}
            }
        }
    }

    fn dispatch_mouse_move(&mut self, x: i32, y: i32, dx: i16, dy: i16) {
        // If we have a captured window, use it. Otherwise find window under cursor (hover)
        let target_id = self.captured_window.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    if port != 0 {
                        // Relative coordinates to content area
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;

                        let event = MouseMoveEvent {
                            x: rel_x,
                            y: rel_y,
                            dx,
                            dy,
                        };
                        let _ = send_message_async(port, MessageType::MouseMove, &event.to_bytes());
                    }
                }
            }
        }
    }

    fn dispatch_key_event(&mut self, scancode: u8, ascii: u8) {
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

        let event_port = self.wm.focused_id
            .and_then(|id| self.wm.get_window(id))
            .and_then(|w| w.event_port);

        if let Some(port) = event_port {
            if port != 0 {
                let _ = send_message_async(port, MessageType::KeyPress, &event.to_bytes());
            }
        }
    }

    /// Handle messages received on the registration port (AppRegister, WmRequest)
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
                    let log_msg = alloc::format!("WM: Frame Committed - ID={}", msg.window_id);
                    log(&log_msg);
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + libipc::messages::SurfacePresentMsg::SIZE {
                    if let Some(msg) = libipc::messages::SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        log("Compositor: Received SurfacePresent, triggering redraw");
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {
                log("Compositor: Unexpected message type on register_port");
            }
        }
    }

    /// Handle an application registration message
    fn handle_app_registration(&mut self, data: &[u8]) {
        let log_msg = alloc::format!("Compositor: handle_app_registration called, len={}", data.len());
        log(&log_msg);

        if data.len() < MessageHeader::SIZE {
            log("Compositor: Message too small for header");
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => {
                log("Compositor: Failed to parse message header");
                return;
            }
        };

        if header.msg_type != MessageType::AppRegister {
            log("Compositor: Message type is not AppRegister");
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

        log("Compositor: Received app registration");

        // Match to first pending window (FIFO order)
        // In a more sophisticated implementation, we'd match by PID
        if !self.pending_windows.is_empty() {
            let pending = self.pending_windows.remove(0);

            // Get the window and update its event port
            if let Some(window) = self.wm.get_window_mut(pending.window_id) {
                window.event_port = Some(reg_msg.app_port);

                // Send surface assignment to the application
                if let Some(region_id) = window.surface_region_id {
                    let msg = SurfaceAssignMsg {
                        window_id: pending.window_id,
                        region_id,
                        width: window.content_width(),
                        height: window.content_height(),
                        stride: window.content_width(),
                        bytes_per_pixel: 4,
                        compositor_port: self.event_port,
                    };

                    let header = MessageHeader::new(MessageType::SurfaceAssign, SurfaceAssignMsg::SIZE as u32);
                    let header_bytes = header.to_bytes();
                    let payload_bytes = msg.to_bytes();

                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]
                        .copy_from_slice(&payload_bytes);

                    if let Err(_) = send(reg_msg.app_port, &full_msg[..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]) {
                        log("Compositor: Failed to send surface assignment");
                    } else {
                        log("Compositor: Sent surface assignment to application");
                    }
                }
            }
        }
    }

    /// Handle application event messages (e.g., SurfacePresent, KeyPress, MouseMove)
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
                // IRQ notification from kernel (Keyboard = 1, Mouse = 12)
                self.poll_input();
            }
            MessageType::WmRequest => {
                self.handle_wm_request(&data[MessageHeader::SIZE..]);
            }
            MessageType::WmCommitFrame => {
                if let Some(msg) = libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    let log_msg = alloc::format!("WM: Frame Committed - ID={}", msg.window_id);
                    log(&log_msg);
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::MouseMove => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseMoveEvent::from_bytes(&data[payload_start..]) {
                    self.cursor.restore_region(&self.fb);
                    self.cursor.apply_delta(event.dx as i32, event.dy as i32, self.fb.width(), self.fb.height());
                    self.cursor.save_region(&self.fb);
                    self.draw_cursor();
                }
            }
            MessageType::SurfacePresent => {
                // Application has finished rendering, trigger redraw
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + SurfacePresentMsg::SIZE {
                    if let Some(msg) = SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        log("Compositor: Received SurfacePresent, triggering redraw");
                        // Mark the specific window as dirty
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                        }
                        // Trigger compositor redraw
                        self.dirty = true;
                    }
                }
            }
            MessageType::KeyPress => {
                log("Compositor: Received KeyPress from keyboard driver");
                // Keyboard event from keyboard driver - route to focused window
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + 3 {
                    if let Some(key_event) = KeyEvent::from_bytes(&data[payload_start..]) {
                        log("Compositor: Parsed key event successfully");
                        // Check for compositor shortcuts (Escape to quit - debug feature)
                        let scancode = key_event.scancode & 0x7F;
                        if scancode == 0x01 {
                            log("Desktop: Escape pressed, exiting");
                            exit(0);
                        }

                        // Get focused window's event port
                        let event_port = if let Some(focused_id) = self.wm.focused_id {
                            if let Some(window) = self.wm.get_window(focused_id) {
                                log("Compositor: Focused window found");
                                window.event_port
                            } else {
                                log("Compositor: Focused window ID not found in windows list");
                                None
                            }
                        } else {
                            log("Compositor: No focused window ID set");
                            None
                        };

                        // Forward to focused window
                        if let Some(port) = event_port {
                            if port == 0 {
                                log("Compositor: ERROR - event_port is 0 (not registered yet)");
                            } else {
                                log("Compositor: Forwarding KeyPress to window");
                                let _ = send_message_async(port, MessageType::KeyPress, &key_event.to_bytes());
                            }
                        } else {
                            log("Compositor: No event_port for focused window");
                        }
                    }
                }
            }
            _ => {
                // Ignore other message types for now
            }
        }
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        // 1. If wallpaper picker is visible, check for clicks on it (highest priority modal)
        if self.wallpaper_picker.visible {
            if self.handle_wallpaper_picker_click(x, y) {
                self.wallpaper_picker.visible = false;
                self.dirty = true;
                return;
            }
            // If click was outside the picker, close it
            if x < self.wallpaper_picker.x || x >= self.wallpaper_picker.x + self.wallpaper_picker.width as i32 ||
               y < self.wallpaper_picker.y || y >= self.wallpaper_picker.y + self.wallpaper_picker.height as i32 {
                self.wallpaper_picker.visible = false;
                self.dirty = true;
                return;
            }
            return; // Click was inside but didn't hit a color or close button
        }

        // 2. If context menu is visible, check for clicks on it
        if self.context_menu.visible {
            if self.handle_context_menu_click(x, y) {
                self.context_menu.visible = false;
                self.dirty = true;
                return;
            }
            self.context_menu.visible = false;
            self.dirty = true;
        }

        // 3. Check if clicking on a window (z-order priority)
        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                self.wm.focus_window(id);
                self.dirty = true;
            }

            // Capture this window for subsequent moves and release
            self.captured_window = Some(id);

            if let Some(w) = self.wm.get_window(id) {
                // Check for window controls
                let btn_y = w.y + 8;
                let btn_size = 12;
                let close_x = w.x + w.width as i32 - 22;
                let max_x = close_x - 20;
                let min_x = max_x - 20;

                if y >= btn_y && y < btn_y + btn_size {
                    if x >= close_x && x < close_x + btn_size {
                        self.handle_close_window(id);
                        return;
                    } else if x >= max_x && x < max_x + btn_size {
                        let sw = self.fb.width();
                        let sh = self.fb.height();
                        self.wm.toggle_maximize(id, sw, sh);
                        self.dirty = true;
                        return;
                    } else if x >= min_x && x < min_x + btn_size {
                        self.wm.minimize_window(id);
                        self.dirty = true;
                        return;
                    }
                }

                // Check for title bar drag
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

                // Check for resize (bottom-right corner)
                if x >= w.x + w.width as i32 - 12 && y >= w.y + w.height as i32 - 12 {
                    self.drag_op = DragOperation::Resize {
                        window_id: id,
                        start_mouse_x: x,
                        start_mouse_y: y,
                        start_win_w: w.width,
                        start_win_h: w.height,
                    };
                    return;
                }

                // If not decorations, forward to application
                if let Some(port) = w.event_port {
                    if port != 0 {
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;
                        if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                            let event = MouseButtonEvent {
                                button: MouseButton::Left,
                                x: rel_x,
                                y: rel_y,
                            };
                            let _ = send_message_async(port, MessageType::MouseButtonDown, &event.to_bytes());
                        }
                    }
                }
            }
            return;
        }

        // 4. Check if clicking on a dock icon
        if let Some(icon_index) = self.dock_icon_at(x, y) {
            self.handle_dock_click(icon_index);
            return;
        }

        // 5. Check if clicking on a desktop icon
        if let Some(icon_idx) = self.icon_at(x, y) {
            let current = self.click_counter;
            if self.last_click_icon == Some(icon_idx) && current.wrapping_sub(self.last_click_tick) < 2 {
                // Double click! (2 consecutive clicks)
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

    fn is_on_panel(&self, y: i32) -> bool {
        y >= 0 && y < 28
    }

    fn is_on_dock(&self, x: i32, y: i32) -> bool {
        self.dock_icon_at(x, y).is_some()
            || {
                let width = self.fb.width();
                let height = self.fb.height();
                let dock_h = 56;
                y >= (height as i32 - dock_h - 12)
            }
    }

    fn handle_right_click(&mut self, x: i32, y: i32) {
        // Hide context menu if already visible
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

        // ✅ somente desktop vazio
        self.context_menu.x = x;
        self.context_menu.y = y;
        self.context_menu.visible = true;
        self.dirty = true;
    }

    fn handle_context_menu_click(&mut self, x: i32, y: i32) -> bool {
        let menu_w = 150u32;
        let item_h = 24u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        if x >= self.context_menu.x && x < self.context_menu.x + menu_w as i32 &&
           y >= self.context_menu.y && y < self.context_menu.y + menu_h as i32 {
            let item_idx = (y - self.context_menu.y) as u32 / item_h;
            if (item_idx as usize) < self.context_menu.items.len() {
                let action = &self.context_menu.items[item_idx as usize];
                if action == "Change Wallpaper" {
                    log("Compositor: Change Wallpaper clicked");
                    self.show_wallpaper_picker();
                }
                return true;
            }
        }
        false
    }

    fn show_wallpaper_picker(&mut self) {
        self.wallpaper_picker.visible = true;
        self.dirty = true;
    }

    fn handle_wallpaper_picker_click(&mut self, x: i32, y: i32) -> bool {
        let px = self.wallpaper_picker.x;
        let py = self.wallpaper_picker.y;

        // Close button check
        if x >= px + self.wallpaper_picker.width as i32 - 24 && x < px + self.wallpaper_picker.width as i32 - 8 &&
           y >= py + 8 && y < py + 24 {
            return true;
        }

        // Color tiles check
        let start_x = px + 20;
        let start_y = py + 48;
        let tile_size = 40u32;
        let spacing = 16u32;

        for (i, color) in self.wallpaper_picker.colors.iter().enumerate() {
            let tx = start_x + (i as i32 % 4) * (tile_size as i32 + spacing as i32);
            let ty = start_y + (i as i32 / 4) * (tile_size as i32 + spacing as i32);

            if x >= tx && x < tx + tile_size as i32 && y >= ty && y < ty + tile_size as i32 {
                self.desktop_bg = *color;
                self.dirty = true;
                return true;
            }
        }
        false
    }

    fn handle_mouse_up(&mut self, x: i32, y: i32) {
        let captured = self.captured_window.take();

        // If we were dragging, just end it
        if !matches!(self.drag_op, DragOperation::None) {
            self.drag_op = DragOperation::None;
            return;
        }

        // Use captured window if any, otherwise find one
        let target_id = captured.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    if port != 0 {
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;
                        if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                            let event = MouseButtonEvent {
                                button: MouseButton::Left,
                                x: rel_x,
                                y: rel_y,
                            };
                            let _ = send_message_async(port, MessageType::MouseButtonUp, &event.to_bytes());
                        }
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
                    log("Compositor: Received WmCreateWindowRequest");

                    let win_x = 150 + (self.wm.windows.len() as i32 * 30);
                    let win_y = 120 + (self.wm.windows.len() as i32 * 30);

                    let id = self.wm.next_id;
                    self.wm.next_id += 1;

                    let window = match Window::new_with_process(
                        id, &req.title, win_x, win_y, req.width, req.height,
                        0 as ProcessId, req.reply_port as PortId
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

                    let log_msg = alloc::format!("WM: Window Created - ID={} Title='{}'", id, req.title);
                    log(&log_msg);
                    self.wm.windows.push(window);
                    self.wm.focus_window(id);
                    self.dirty = true;

                    // Send response back to the application
                    let header = MessageHeader::new(MessageType::WmResponse, libipc::messages::WmCreateWindowResponse::SIZE as u32);
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE].copy_from_slice(&resp.to_bytes());
                    let _ = send(req.reply_port as PortId, &full_msg[..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE]);
                    log("Compositor: Sent WmCreateWindowResponse");
                }
            }
            _ => {}
        }
    }

    fn handle_close_window(&mut self, id: WindowId) {
        let log_msg = alloc::format!("WM: Window Closing - ID={}", id);
        log(&log_msg);
        let mut event_port = None;
        if let Some(w) = self.wm.get_window(id) {
            event_port = w.event_port;
        }

        if let Some(port) = event_port {
            if port == 0 {
                self.wm.close_window(id);
                self.dirty = true;
                return;
            }
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

        self.wm.close_window(id);
        self.dirty = true;
    }

    /// Check if a point is on a dock icon and return its index
    fn dock_icon_at(&self, x: i32, y: i32) -> Option<usize> {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 320u32;
        let dock_h = 56u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2) as i32;
        let dock_y = height.saturating_sub(dock_h + 12) as i32;

        let icon_size = 36i32;
        let padding = 20i32;
        let num_icons = 4; // Matches draw_dock
        let total_icons_width = num_icons * icon_size + (num_icons - 1) * padding;
        let start_x = dock_x + (dock_w as i32 - total_icons_width) / 2;
        let icon_y = dock_y + (dock_h as i32 - icon_size) / 2;

        // Check if click is within dock vertical bounds
        if y < icon_y || y >= icon_y + icon_size {
            return None;
        }

        // Check each icon
        for i in 0..4 {
            let ix = start_x + (i as i32 * (icon_size + padding));
            if x >= ix && x < ix + icon_size {
                return Some(i);
            }
        }

        None
    }

    /// Handle click on a dock icon
    fn handle_dock_click(&mut self, icon_index: usize) {
        match icon_index {
            0 => {
                // Files - not implemented yet
                log("Dock: Files icon clicked (not implemented)");
            }
            1 => {
                // Settings - not implemented yet
                log("Dock: Settings icon clicked (not implemented)");
            }
            2 => {
                // Browser - not implemented yet
                log("Dock: Browser icon clicked (not implemented)");
            }
            3 => {
                // Terminal - spawn terminal process with managed window
                self.spawn_app("terminal");
            }
            _ => {}
        }
    }

    fn spawn_app(&mut self, name: &str) {
        log(&alloc::format!("Compositor: Spawning {}", name));

        // Special case for terminal which uses managed surface protocol
        if name == "terminal" {
            self.spawn_terminal();
            return;
        }

        match spawn_process(name) {
            Ok(_) => log(&alloc::format!("Compositor: Successfully spawned {}", name)),
            Err(_) => log(&alloc::format!("Compositor: Failed to spawn {}", name)),
        }
    }

    /// Spawn terminal process with a managed window and shared surface
    fn spawn_terminal(&mut self) {
        // Anti-loop: Prevent spawning terminals too fast (cooldown of 1 second)
        let now = atom_syscall::thread::get_time_ms();
        if now.saturating_sub(self.last_terminal_spawn_time) < 1000 {
            log("Dock: Ignoring terminal spawn request (cooldown)");
            return;
        }
        self.last_terminal_spawn_time = now;

        log("Dock: Terminal icon clicked - spawning terminal");

        // Spawn the terminal process
        let pid = match spawn_process("terminal") {
            Ok(pid) => pid,
            Err(_) => {
                log("Dock: Failed to spawn terminal process");
                return;
            }
        };

        log("Dock: Terminal spawned successfully");

        // Calculate window position with offset from existing windows
        let offset = (self.wm.windows.len() as i32) * 30;
        let win_x = 150 + offset;
        let win_y = 120 + offset;
        let win_width = 600u32;
        let win_height = 400u32;

        // Create window with associated process and shared surface
        // Use dummy port 0 for now - will be updated on registration
        let window_id = match self.wm.create_window_with_process(
            "Terminal",
            win_x,
            win_y,
            win_width,
            win_height,
            pid,
            0, // Port will be set when app registers
        ) {
            Some(id) => id,
            None => {
                log("Dock: Failed to create window with surface");
                return;
            }
        };

        // Store as pending - surface assignment will be sent when terminal registers
        self.pending_windows.push(PendingWindow {
            pid,
            window_id,
        });

        log("Dock: Window created, waiting for terminal to register");

        self.dirty = true;
    }

    fn draw_all(&mut self) {
        self.cursor.restore_region(&self.fb);

        // Desktop background
        self.fb.fill_rect(0, 0, self.fb.width(), self.fb.height(), self.desktop_bg);

        // Desktop Icons
        self.draw_desktop_icons();

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

        // Context Menu
        if self.context_menu.visible {
            self.draw_context_menu();
        }

        // Wallpaper Picker
        if self.wallpaper_picker.visible {
            self.draw_wallpaper_picker();
        }

        // Cursor
        self.cursor.save_region(&self.fb);
        self.draw_cursor();
    }

    fn draw_wallpaper_picker(&self) {
        let px = self.wallpaper_picker.x as u32;
        let py = self.wallpaper_picker.y as u32;
        let pw = self.wallpaper_picker.width;
        let ph = self.wallpaper_picker.height;

        // Shadow
        self.fb.fill_rect(px + 4, py + 4, pw, ph, theme::SHADOW);

        // Background
        self.fb.fill_rect(px, py, pw, ph, theme::WINDOW_BG);
        self.fb.draw_rect(px, py, pw, ph, theme::WINDOW_BORDER);

        // Header
        self.fb.fill_rect(px + 1, py + 1, pw - 2, 32, theme::WINDOW_HEADER_FOCUSED);
        self.fb.draw_string(px + 12, py + 8, "Change Wallpaper", theme::PANEL_TEXT, theme::WINDOW_HEADER_FOCUSED);

        // Close button (X)
        self.fb.fill_rect(px + pw - 24, py + 8, 16, 16, Color::new(191, 97, 106));
        self.fb.draw_string(px + pw - 20, py + 8, "x", Color::WHITE, Color::new(191, 97, 106));

        // Color tiles
        let start_x = px + 20;
        let start_y = py + 48;
        let tile_size = 40u32;
        let spacing = 16u32;

        for (i, color) in self.wallpaper_picker.colors.iter().enumerate() {
            let tx = start_x + (i as u32 % 4) * (tile_size + spacing);
            let ty = start_y + (i as u32 / 4) * (tile_size + spacing);

            self.fb.fill_rect(tx, ty, tile_size, tile_size, *color);
            self.fb.draw_rect(tx, ty, tile_size, tile_size, theme::WINDOW_BORDER);
        }
    }

    fn draw_context_menu(&self) {
        let menu_w = 150u32;
        let item_h = 24u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        // Shadow
        self.fb.fill_rect(self.context_menu.x as u32 + 2, self.context_menu.y as u32 + 2, menu_w, menu_h, theme::SHADOW);

        // Background
        self.fb.fill_rect(self.context_menu.x as u32, self.context_menu.y as u32, menu_w, menu_h, theme::PANEL_BG);
        self.fb.draw_rect(self.context_menu.x as u32, self.context_menu.y as u32, menu_w, menu_h, theme::WINDOW_BORDER);

        for (i, item) in self.context_menu.items.iter().enumerate() {
            let iy = self.context_menu.y as u32 + (i as u32 * item_h);
            self.fb.draw_string(self.context_menu.x as u32 + 8, iy + 4, item, theme::PANEL_TEXT, theme::PANEL_BG);
        }
    }

    fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        for (i, icon) in self.icons.iter().enumerate() {
            if x >= icon.x && x < icon.x + 48 && y >= icon.y && y < icon.y + 48 {
                return Some(i);
            }
        }
        None
    }

    fn draw_desktop_icons(&self) {
        for icon in &self.icons {
            let ix = icon.x as u32;
            let iy = icon.y as u32;
            let size = 48u32;

            // Icon box shadow
            self.fb.fill_rect(ix + 2, iy + 2, size, size, theme::SHADOW);
            // Icon box
            self.fb.fill_rect(ix, iy, size, size, icon.color);
            self.fb.draw_rect(ix, iy, size, size, theme::WINDOW_BORDER);

            // Icon symbol (generic square for now)
            self.fb.draw_rect(ix + 12, iy + 12, 24, 24, Color::WHITE);

            // Label
            let label_len = icon.label.len() as i32 * 8;
            let lx = (ix as i32 + (size as i32 - label_len) / 2).max(0) as u32;
            self.fb.draw_string(lx as u32, iy + size + 8, &icon.label, theme::PANEL_TEXT, self.desktop_bg);
        }
    }

    fn draw_panel(&self) {
        let width = self.fb.width();

        // Panel background
        self.fb.fill_rect(0, 0, width, 28, theme::PANEL_BG);
        // Bottom border for panel
        self.fb.fill_rect(0, 27, width, 1, theme::WINDOW_BORDER);

        // Logo
        self.fb.draw_string(16, 6, "ATOM", theme::ACCENT, theme::PANEL_BG);

        // Status
        self.fb.draw_string(70, 6, "|  OS", theme::PANEL_TEXT, theme::PANEL_BG);

        // Clock (right side)
        let clock_x = width.saturating_sub(64);
        self.fb.draw_string(clock_x, 6, "12:00", theme::PANEL_TEXT, theme::PANEL_BG);
    }

    fn draw_window(&self, window: &Window) {
        if !window.visible || window.state == WindowState::Minimized {
            return;
        }

        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        // Shadow (only if not maximized)
        if window.state != WindowState::Maximized {
            self.fb.fill_rect(x + 2, y + 2, w + 2, h + 2, theme::SHADOW);
        }

        // Outer Border
        self.fb.fill_rect(x, y, w, h, theme::WINDOW_BORDER);

        // Header (Title Bar)
        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.fb.fill_rect(x + WINDOW_BORDER_WIDTH, y + WINDOW_BORDER_WIDTH,
                         w - WINDOW_BORDER_WIDTH * 2, WINDOW_HEADER_HEIGHT - WINDOW_BORDER_WIDTH,
                         header_color);

        // Title
        self.fb.draw_string(x + 12, y + 8, &window.title, theme::PANEL_TEXT, header_color);

        // Window controls (buttons)
        let btn_y = y + 8;
        let btn_size = 12;
        let close_x = x + w - 24;
        let max_x = close_x - 22;
        let min_x = max_x - 22;

        // Close button (Red)
        self.fb.fill_rect(close_x, btn_y, btn_size, btn_size, Color::new(191, 97, 106));
        // Maximize button (Green)
        self.fb.fill_rect(max_x, btn_y, btn_size, btn_size, Color::new(163, 190, 140));
        // Minimize button (Yellow)
        self.fb.fill_rect(min_x, btn_y, btn_size, btn_size, Color::new(235, 203, 139));

        // Window content area background
        if window.surface.is_none() {
            self.fb.fill_rect(window.content_x(), window.content_y(),
                             window.content_width(), window.content_height(),
                             theme::WINDOW_BG);
        }

        // Composite shared surface content into window
        if let Some(ref surface) = window.surface {
            surface.blit_to_framebuffer(&self.fb, window.content_x(), window.content_y());
        }

        // Inner border around content
        self.fb.draw_rect(window.content_x() - 1, window.content_y() - 1,
                         window.content_width() + 2, window.content_height() + 2,
                         theme::WINDOW_BORDER);
    }

    fn draw_dock(&self) {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 320u32;
        let dock_h = 56u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2);
        let dock_y = height.saturating_sub(dock_h + 12);

        // Dock shadow
        self.fb.fill_rect(dock_x + 2, dock_y + 2, dock_w, dock_h, theme::SHADOW);

        // Dock background
        self.fb.fill_rect(dock_x, dock_y, dock_w, dock_h, theme::DOCK_BG);
        self.fb.draw_rect(dock_x, dock_y, dock_w, dock_h, theme::WINDOW_BORDER);

        // Dock icons
        let icons = [
            (Color::new(136, 192, 208), "FL"),  // Files
            (Color::new(129, 161, 193), "ST"),  // Settings
            (Color::new(94, 129, 172), "BR"),   // Browser
            (Color::new(76, 86, 106), ">_"),    // Terminal
        ];

        let icon_size = 36u32;
        let padding = 20u32;
        let total_icons_width = icons.len() as u32 * icon_size + (icons.len() as u32 - 1) * padding;
        let start_x = dock_x + (dock_w - total_icons_width) / 2;
        let icon_y = dock_y + (dock_h - icon_size) / 2;

        for (i, (color, label)) in icons.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + padding));

            // Icon background
            self.fb.fill_rect(ix, icon_y, icon_size, icon_size, *color);

            // Icon label (centered)
            let label_len = label.len() as u32 * 8;
            let lx = ix + (icon_size - label_len) / 2;
            let ly = icon_y + (icon_size - 16) / 2;
            self.fb.draw_string(lx, ly, label, Color::WHITE, *color);
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

    // Register with name service so other processes can find us
    let _ = libipc::protocol::register_service("compositor", compositor.event_port);
    let _ = libipc::protocol::register_service("compositor.register", compositor.register_port);
    let _ = libipc::protocol::register_service("compositor.wm", compositor.register_port);

    // NOTE: Drivers (keyboard, mouse, display) are spawned by the init process.
    // ui_shell only receives events via IPC from already-running drivers.

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
