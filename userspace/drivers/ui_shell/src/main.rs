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
use atom_syscall::input::MouseDriver;
use atom_syscall::ipc::{create_port, send, try_recv, PortId};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use atom_syscall::process::{spawn_process, ProcessId};

use libipc::messages::{MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers};
use libipc::protocol::send_message_async;
use libipc::well_known;

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

/// Window state in the compositor
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
    /// Process ID of the owning application (if any)
    process_id: Option<ProcessId>,
    /// Shared surface for application rendering (if managed)
    surface: Option<SharedSurface>,
    /// Shared region ID for passing to the application
    surface_region_id: Option<SharedRegionId>,
    /// Whether the content is dirty and needs compositing
    content_dirty: bool,
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
            process_id: None,
            surface: None,
            surface_region_id: None,
            content_dirty: false,
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
        // Calculate content area dimensions (subtract header and borders)
        let content_width = width.saturating_sub(2);  // 1px border each side
        let content_height = height.saturating_sub(24 + 1); // 24px header + 1px bottom border

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
            visible: true,
            focused: false,
            event_port: Some(event_port),
            process_id: Some(process_id),
            surface_region_id: Some(region_id),
            surface: Some(surface),
            content_dirty: true,
        })
    }

    /// Get content area position (inside window chrome)
    fn content_x(&self) -> u32 {
        (self.x + 1) as u32 // 1px border
    }

    fn content_y(&self) -> u32 {
        (self.y + 24) as u32 // 24px header
    }

    fn content_width(&self) -> u32 {
        self.width.saturating_sub(2)
    }

    fn content_height(&self) -> u32 {
        self.height.saturating_sub(24 + 1)
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
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.last().map(|w| w.id);
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

struct Compositor {
    fb: Framebuffer,
    wm: WindowManager,
    cursor: CursorState,
    mouse: MouseDriver,
    event_port: PortId,
    /// Port for receiving application registration messages
    register_port: PortId,
    /// Windows waiting for application to register
    pending_windows: Vec<PendingWindow>,
    dirty: bool,
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

        Self {
            fb,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            mouse: MouseDriver::new(),
            event_port,
            register_port,
            pending_windows: Vec::new(),
            dirty: true,
        }
    }

    fn run(&mut self) -> ! {
        log("Desktop: Starting compositor");

        // Create initial demo windows (no surface/process)
        self.wm.create_window("Welcome to Atom", 100, 100, 400, 300);

        // Initial draw
        self.draw_all();

        log("Desktop: Entering event loop");

        let mut prev_left = false;
        let mut reg_buffer = [0u8; 64];

        loop {
            // Poll for application registrations
            while let Ok(Some(len)) = try_recv(self.register_port, &mut reg_buffer) {
                log("Compositor: Received message on register_port");
                self.handle_app_registration(&reg_buffer[..len]);
            }

            // Poll for application events (e.g., SurfacePresent)
            let mut event_buffer = [0u8; 64];
            while let Ok(Some(len)) = try_recv(self.event_port, &mut event_buffer) {
                self.handle_app_event(&event_buffer[..len]);
            }

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

            // Keyboard events are received via IPC from the keyboard driver
            // (handled in handle_app_event when MessageType::KeyPress is received)

            // Redraw if needed
            if self.dirty {
                self.draw_all();
                self.dirty = false;
            }

            yield_now();
        }
    }

    /// Handle an application registration message
    fn handle_app_registration(&mut self, data: &[u8]) {
        log("Compositor: handle_app_registration called");

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

                    let mut full_msg = [0u8; 48];
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

    /// Handle application event messages (e.g., SurfacePresent, KeyPress from keyboard driver)
    fn handle_app_event(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
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
        // Check if clicking on a dock icon first
        if let Some(icon_index) = self.dock_icon_at(x, y) {
            self.handle_dock_click(icon_index);
            return;
        }

        // Check if clicking on a window
        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                self.wm.focus_window(id);
                self.dirty = true;
            }

            // Check for close button click
            let mut should_close = false;
            let mut event_port = None;

            if let Some(w) = self.wm.windows.iter().find(|w| w.id == id) {
                let close_x = w.x + w.width as i32 - 20;
                let close_y = w.y + 6;
                if x >= close_x && x < close_x + 12 && y >= close_y && y < close_y + 12 {
                    should_close = true;
                    event_port = w.event_port;
                }
            }

            if should_close {
                // Send terminate request to the application if it has an IPC port
                if let Some(port) = event_port {
                    let msg = TerminateRequestMsg {
                        window_id: id,
                        reason: 0, // User requested close
                    };
                    let header = MessageHeader::new(MessageType::TerminateRequest, TerminateRequestMsg::SIZE as u32);
                    let header_bytes = header.to_bytes();
                    let payload_bytes = msg.to_bytes();

                    let mut full_msg = [0u8; 24];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + TerminateRequestMsg::SIZE]
                        .copy_from_slice(&payload_bytes);

                    let _ = send(port, &full_msg[..MessageHeader::SIZE + TerminateRequestMsg::SIZE]);
                    log("Compositor: Sent terminate request to application");
                }

                // Close the window (surface will be cleaned up via Drop)
                self.wm.close_window(id);
                self.dirty = true;
            }
        }
    }

    /// Check if a point is on a dock icon and return its index
    fn dock_icon_at(&self, x: i32, y: i32) -> Option<usize> {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 300u32;
        let dock_h = 48u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2) as i32;
        let dock_y = height.saturating_sub(dock_h + 10) as i32;

        let icon_size = 32i32;
        let padding = 16i32;
        let start_x = dock_x + padding;
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
                self.spawn_terminal();
            }
            _ => {}
        }
    }

    /// Spawn terminal process with a managed window and shared surface
    fn spawn_terminal(&mut self) {
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

        // Window content background (only if no surface)
        if window.surface.is_none() {
            self.fb.fill_rect(x + 1, y + 1, w - 2, h - 2, theme::WINDOW_BG);
        }

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

        // Composite shared surface content into window
        if let Some(ref surface) = window.surface {
            surface.blit_to_framebuffer(&self.fb, window.content_x(), window.content_y());
        }
    }

    fn draw_dock(&self) {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_w = 300u32;
        let dock_h = 48u32;
        let dock_x = (width / 2).saturating_sub(dock_w / 2);
        let dock_y = height.saturating_sub(dock_h + 10);

        // Dock background with rounded appearance
        self.fb.fill_rect(dock_x, dock_y, dock_w, dock_h, theme::DOCK_BG);

        // Dock icons
        let icons = [
            (Color::new(191, 97, 106), "F"),  // Files
            (Color::new(163, 190, 140), "S"),  // Settings
            (Color::new(94, 129, 172), "B"),   // Browser
            (Color::new(80, 80, 80), ">_"),    // Terminal
        ];

        let icon_size = 32u32;
        let padding = 16u32;
        let start_x = dock_x + padding;
        let icon_y = dock_y + (dock_h - icon_size) / 2;

        for (i, (color, label)) in icons.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + padding));
            self.fb.fill_rect(ix, icon_y, icon_size, icon_size, *color);
            self.fb.draw_string(ix + 8, icon_y + 10, label, Color::WHITE, *color);
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

/// Register this service with the name service
fn register_with_namesvc(service_name: &str, port: PortId) {
    // Build registration message for name service
    // Format: [msg_type: u32][port: u64][name_len: u32][name: bytes]
    let mut msg = [0u8; 64];
    let msg_type = 600u32; // NsRegister
    msg[0..4].copy_from_slice(&msg_type.to_le_bytes());
    msg[4..12].copy_from_slice(&port.to_le_bytes());
    msg[12..16].copy_from_slice(&(service_name.len() as u32).to_le_bytes());
    msg[16..16 + service_name.len()].copy_from_slice(service_name.as_bytes());

    // Try to send to name service (best effort - name service may not be running yet)
    let _ = send(well_known::NAME_SERVICE, &msg[..16 + service_name.len()]);
    log("Desktop: Registered with name service");
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

    // Register with name service
    register_with_namesvc("compositor", compositor.event_port);

    // Spawn input drivers - keyboard driver sends events to our event_port (port 1)
    log("Desktop: Spawning keyboard driver");
    match spawn_process("keyboard") {
        Ok(_pid) => log("Desktop: Keyboard driver spawned successfully"),
        Err(_) => log("Desktop: Failed to spawn keyboard driver"),
    }

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
