// Atom Desktop Environment
//
// This is the userspace desktop environment responsible for:
// - Window management (creating, positioning, focusing windows)
// - Compositing (rendering all windows to the framebuffer)
// - Input routing (receiving input from drivers, dispatching to applications)
// - Focus management
// - UI policy (decorations, layouts, themes)
//
// Architecture:
// - Receives raw input events from keyboard/mouse drivers via IPC
// - Manages surfaces allocated to applications
// - Composites all surfaces + decorations to the screen
// - Routes input events to the focused application
//
// The desktop environment is the SINGLE AUTHORITY for UI policy.
// Applications cannot position or resize their own windows.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

use atom_syscall::graphics::{Color, Framebuffer};
use atom_syscall::input::{keyboard_poll, MouseDriver, clear_keyboard_buffer, clear_mouse_buffer};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use atom_syscall::io;

// ============================================================================
// Theme Colors (Nord)
// ============================================================================

struct Theme;
impl Theme {
    const DESKTOP_BG: Color = Color::new(46, 52, 64);    // Nord0
    const PANEL_BG: Color = Color::new(36, 41, 51);      // Darker
    const DOCK_BG: Color = Color::new(36, 41, 51);
    const ACCENT: Color = Color::new(136, 192, 208);     // Nord8
    const TEXT_MAIN: Color = Color::new(236, 239, 244); // Nord6
    const TEXT_DIM: Color = Color::new(200, 200, 200);
    const WINDOW_BG: Color = Color::WHITE;
    const WINDOW_HEADER: Color = Color::new(67, 76, 94); // Nord2
    const WINDOW_HEADER_UNFOCUSED: Color = Color::new(59, 66, 82); // Nord1
    const WINDOW_BORDER: Color = Color::new(59, 66, 82);
    const WINDOW_SHADOW: Color = Color::new(20, 20, 20);
    const CURSOR_FILL: Color = Color::WHITE;
    const CURSOR_OUTLINE: Color = Color::BLACK;
}

// ============================================================================
// Window Management
// ============================================================================

/// Unique window identifier
type WindowId = u32;

/// Window state
struct Window {
    id: WindowId,
    /// Position on screen
    x: i32,
    y: i32,
    /// Size including decorations
    width: u32,
    height: u32,
    /// Title
    title: &'static str,
    /// Whether window is focused
    focused: bool,
    /// Whether window is visible
    visible: bool,
    /// Client surface buffer (if allocated)
    buffer: Option<*mut u32>,
    /// Client area width
    client_width: u32,
    /// Client area height
    client_height: u32,
}

const TITLE_BAR_HEIGHT: u32 = 24;
const BORDER_WIDTH: u32 = 1;

impl Window {
    fn new(id: WindowId, title: &'static str, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            id,
            x,
            y,
            width,
            height,
            title,
            focused: false,
            visible: true,
            buffer: None,
            client_width: width - BORDER_WIDTH * 2,
            client_height: height - TITLE_BAR_HEIGHT - BORDER_WIDTH,
        }
    }

    /// Get client area bounds (relative to window)
    fn client_rect(&self) -> (u32, u32, u32, u32) {
        (
            BORDER_WIDTH,
            TITLE_BAR_HEIGHT,
            self.client_width,
            self.client_height,
        )
    }

    /// Check if point is in title bar
    fn in_title_bar(&self, x: i32, y: i32) -> bool {
        let local_x = x - self.x;
        let local_y = y - self.y;
        local_x >= 0 && local_x < self.width as i32 &&
        local_y >= 0 && local_y < TITLE_BAR_HEIGHT as i32
    }

    /// Check if point is in close button
    fn in_close_button(&self, x: i32, y: i32) -> bool {
        let local_x = x - self.x;
        let local_y = y - self.y;
        let btn_x = self.width as i32 - 20;
        local_x >= btn_x && local_x < btn_x + 12 &&
        local_y >= 6 && local_y < 18
    }

    /// Check if point is in client area
    fn in_client_area(&self, x: i32, y: i32) -> bool {
        let local_x = x - self.x;
        let local_y = y - self.y;
        local_x >= BORDER_WIDTH as i32 &&
        local_x < (self.width - BORDER_WIDTH) as i32 &&
        local_y >= TITLE_BAR_HEIGHT as i32 &&
        local_y < (self.height - BORDER_WIDTH) as i32
    }

    /// Convert screen coords to client coords
    fn to_client_coords(&self, x: i32, y: i32) -> (i32, i32) {
        (
            x - self.x - BORDER_WIDTH as i32,
            y - self.y - TITLE_BAR_HEIGHT as i32,
        )
    }
}

/// Window manager state
struct WindowManager {
    windows: Vec<Window>,
    focused_id: Option<WindowId>,
    next_id: WindowId,
    drag_state: Option<DragState>,
}

/// Window dragging state
struct DragState {
    window_id: WindowId,
    offset_x: i32,
    offset_y: i32,
}

impl WindowManager {
    fn new() -> Self {
        Self {
            windows: Vec::new(),
            focused_id: None,
            next_id: 1,
            drag_state: None,
        }
    }

    fn create_window(&mut self, title: &'static str, x: i32, y: i32, width: u32, height: u32) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new(id, title, x, y, width, height);
        self.windows.push(window);

        self.focus_window(id);
        id
    }

    fn focus_window(&mut self, id: WindowId) {
        // Unfocus previous
        if let Some(old_id) = self.focused_id {
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == old_id) {
                w.focused = false;
            }
        }

        // Focus new and bring to front
        if let Some(idx) = self.windows.iter().position(|w| w.id == id) {
            self.windows[idx].focused = true;
            self.focused_id = Some(id);

            // Move to end (top of z-order)
            let window = self.windows.remove(idx);
            self.windows.push(window);
        }
    }

    fn close_window(&mut self, id: WindowId) {
        if let Some(idx) = self.windows.iter().position(|w| w.id == id) {
            self.windows.remove(idx);

            if self.focused_id == Some(id) {
                self.focused_id = self.windows.last().map(|w| w.id);
                if let Some(new_id) = self.focused_id {
                    self.focus_window(new_id);
                }
            }
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        // Check from top to bottom (reverse order)
        for window in self.windows.iter().rev() {
            if window.visible &&
               x >= window.x && x < window.x + window.width as i32 &&
               y >= window.y && y < window.y + window.height as i32 {
                return Some(window.id);
            }
        }
        None
    }

    fn get_window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn get_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn move_window(&mut self, id: WindowId, x: i32, y: i32) {
        if let Some(w) = self.get_window_mut(id) {
            w.x = x;
            w.y = y;
        }
    }

    fn start_drag(&mut self, id: WindowId, mouse_x: i32, mouse_y: i32) {
        if let Some(w) = self.get_window(id) {
            self.drag_state = Some(DragState {
                window_id: id,
                offset_x: mouse_x - w.x,
                offset_y: mouse_y - w.y,
            });
        }
    }

    fn update_drag(&mut self, mouse_x: i32, mouse_y: i32) {
        if let Some(ref drag) = self.drag_state {
            let new_x = mouse_x - drag.offset_x;
            let new_y = mouse_y - drag.offset_y;
            self.move_window(drag.window_id, new_x, new_y);
        }
    }

    fn end_drag(&mut self) {
        self.drag_state = None;
    }

    fn is_dragging(&self) -> bool {
        self.drag_state.is_some()
    }
}

// ============================================================================
// Cursor State
// ============================================================================

struct CursorState {
    x: u32,
    y: u32,
    saved_region: [u32; 16 * 16],
    saved_x: u32,
    saved_y: u32,
    has_saved: bool,
}

impl CursorState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            x: width / 2,
            y: height / 2,
            saved_region: [0; 16 * 16],
            saved_x: 0,
            saved_y: 0,
            has_saved: false,
        }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        let new_x = (self.x as i32).saturating_add(dx);
        let new_y = (self.y as i32).saturating_sub(dy);

        self.x = new_x.clamp(0, (width - 1) as i32) as u32;
        self.y = new_y.clamp(0, (height - 1) as i32) as u32;
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
                let px = self.x + col;
                let py = self.y + row;

                if px < fb.width() && py < fb.height() {
                    let pixel_offset = (py * stride + px) as usize * bpp;
                    let pixel_ptr = (fb_addr + pixel_offset) as *const u32;
                    self.saved_region[(row * 16 + col) as usize] = unsafe { pixel_ptr.read_volatile() };
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
                let px = self.saved_x + col;
                let py = self.saved_y + row;

                if px < fb.width() && py < fb.height() {
                    let pixel_offset = (py * stride + px) as usize * bpp;
                    let pixel_ptr = (fb_addr + pixel_offset) as *mut u32;
                    unsafe {
                        pixel_ptr.write_volatile(self.saved_region[(row * 16 + col) as usize]);
                    }
                }
            }
        }
    }
}

// ============================================================================
// Desktop Environment State
// ============================================================================

struct Desktop {
    fb: Framebuffer,
    width: u32,
    height: u32,
    cursor: CursorState,
    wm: WindowManager,
    mouse: MouseDriver,
    left_button_down: bool,
    needs_redraw: bool,
}

impl Desktop {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();

        Self {
            fb,
            width,
            height,
            cursor: CursorState::new(width, height),
            wm: WindowManager::new(),
            mouse: MouseDriver::new(),
            left_button_down: false,
            needs_redraw: true,
        }
    }

    fn handle_input(&mut self) -> bool {
        let mut need_redraw = false;
        let mut cursor_moved = false;

        // Handle mouse input
        while let Some(event) = self.mouse.poll_event() {
            self.cursor.apply_delta(event.dx, event.dy, self.width, self.height);
            cursor_moved = true;

            // Handle dragging
            if self.wm.is_dragging() {
                if event.left_button {
                    self.wm.update_drag(self.cursor.x as i32, self.cursor.y as i32);
                    need_redraw = true;
                } else {
                    self.wm.end_drag();
                }
            }

            // Handle left button press
            if event.left_button && !self.left_button_down {
                self.left_button_down = true;

                let x = self.cursor.x as i32;
                let y = self.cursor.y as i32;

                // Check which window was clicked
                if let Some(id) = self.wm.window_at(x, y) {
                    let window = self.wm.get_window(id).unwrap();

                    if window.in_close_button(x, y) {
                        // Close window
                        self.wm.close_window(id);
                        need_redraw = true;
                    } else if window.in_title_bar(x, y) {
                        // Start dragging
                        self.wm.focus_window(id);
                        self.wm.start_drag(id, x, y);
                        need_redraw = true;
                    } else {
                        // Click in client area - focus and deliver event
                        if self.wm.focused_id != Some(id) {
                            self.wm.focus_window(id);
                            need_redraw = true;
                        }
                    }
                } else {
                    // Clicked on desktop - check dock
                    if let Some(icon) = self.check_dock_click(x as u32, y as u32) {
                        self.handle_dock_click(icon);
                        need_redraw = true;
                    }
                }
            }

            // Handle left button release
            if !event.left_button && self.left_button_down {
                self.left_button_down = false;
                self.wm.end_drag();
            }
        }

        // Handle keyboard input
        while let Some(scancode) = keyboard_poll() {
            // Escape key to exit
            if scancode == 0x01 {
                return false;
            }

            // Deliver to focused window (if any)
            // In a real implementation, this would send via IPC
        }

        if cursor_moved || need_redraw {
            self.needs_redraw = true;
        }

        true
    }

    fn render(&mut self) {
        if !self.needs_redraw {
            return;
        }

        // Restore cursor background
        self.cursor.restore_region(&self.fb);

        // Draw desktop background
        self.fb.fill_rect(0, 0, self.width, self.height, Theme::DESKTOP_BG);

        // Draw top panel
        self.draw_top_panel();

        // Draw dock
        self.draw_dock();

        // Draw windows (bottom to top)
        for window in &self.wm.windows {
            if window.visible {
                self.draw_window(window);
            }
        }

        // Save cursor background and draw cursor
        self.cursor.save_region(&self.fb);
        self.draw_cursor(self.cursor.x, self.cursor.y);

        self.needs_redraw = false;
    }

    fn draw_top_panel(&self) {
        let panel_height = 32;
        self.fb.fill_rect(0, 0, self.width, panel_height, Theme::PANEL_BG);

        // Logo
        self.fb.draw_string(16, 8, "Atom", Theme::ACCENT, Theme::PANEL_BG);
        self.fb.draw_string(56, 8, "|  Desktop Environment", Theme::TEXT_DIM, Theme::PANEL_BG);

        // Clock (right side)
        let clock_x = self.width.saturating_sub(100);
        self.fb.draw_string(clock_x, 8, "12:00 PM", Theme::TEXT_MAIN, Theme::PANEL_BG);
    }

    fn draw_dock(&self) {
        let dock_height = 48;
        let dock_width = 300;
        let dock_x = (self.width / 2).saturating_sub(dock_width / 2);
        let dock_y = self.height.saturating_sub(dock_height + 10);

        // Dock background
        self.fb.fill_rect(dock_x, dock_y, dock_width, dock_height, Theme::DOCK_BG);

        // Draw dock icons
        let icons = [
            (Color::new(191, 97, 106), "F"),   // Files
            (Color::new(163, 190, 140), "S"),  // Settings
            (Color::new(94, 129, 172), "B"),   // Browser
            (Color::new(46, 46, 46), ">"),     // Terminal
        ];

        let icon_size = 32;
        let padding = 16;
        let mut ix = dock_x + padding;

        for (color, label) in icons.iter() {
            let iy = dock_y + (dock_height - icon_size) / 2;
            self.fb.fill_rect(ix, iy, icon_size, icon_size, *color);
            // Draw label centered
            self.fb.draw_string(ix + 12, iy + 12, label, Color::WHITE, *color);
            ix += icon_size + padding;
        }
    }

    fn draw_window(&self, window: &Window) {
        let x = window.x.max(0) as u32;
        let y = window.y.max(0) as u32;
        let w = window.width;
        let h = window.height;

        // Shadow
        self.fb.fill_rect(x + 4, y + 4, w, h, Theme::WINDOW_SHADOW);

        // Border
        self.fb.fill_rect(x, y, w, h, Theme::WINDOW_BORDER);

        // Title bar
        let header_color = if window.focused {
            Theme::WINDOW_HEADER
        } else {
            Theme::WINDOW_HEADER_UNFOCUSED
        };
        self.fb.fill_rect(x + 1, y + 1, w - 2, TITLE_BAR_HEIGHT - 1, header_color);
        self.fb.draw_string(x + 10, y + 6, window.title, Theme::TEXT_MAIN, header_color);

        // Window control buttons
        let btn_y = y + 6;
        let btn_x = x + w - 18;
        self.fb.fill_rect(btn_x, btn_y, 12, 12, Color::new(255, 95, 86));      // Close
        self.fb.fill_rect(btn_x - 18, btn_y, 12, 12, Color::new(255, 189, 46)); // Minimize
        self.fb.fill_rect(btn_x - 36, btn_y, 12, 12, Color::new(39, 201, 63));  // Maximize

        // Client area
        self.fb.fill_rect(
            x + BORDER_WIDTH,
            y + TITLE_BAR_HEIGHT,
            w - BORDER_WIDTH * 2,
            h - TITLE_BAR_HEIGHT - BORDER_WIDTH,
            Theme::WINDOW_BG,
        );
    }

    fn draw_cursor(&self, x: u32, y: u32) {
        let cursor_map = [
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

        for (row, cols) in cursor_map.iter().enumerate() {
            for (col, &px) in cols.iter().enumerate() {
                let cx = x + col as u32;
                let cy = y + row as u32;
                match px {
                    1 => self.fb.draw_pixel(cx, cy, Theme::CURSOR_OUTLINE),
                    2 => self.fb.draw_pixel(cx, cy, Theme::CURSOR_FILL),
                    _ => {}
                }
            }
        }
    }

    fn check_dock_click(&self, x: u32, y: u32) -> Option<usize> {
        let dock_height = 48;
        let dock_width = 300;
        let dock_x = (self.width / 2).saturating_sub(dock_width / 2);
        let dock_y = self.height.saturating_sub(dock_height + 10);

        // Check bounds
        if x < dock_x || x >= dock_x + dock_width ||
           y < dock_y || y >= dock_y + dock_height {
            return None;
        }

        // Check which icon
        let icon_size = 32;
        let padding = 16;
        let relative_x = x - dock_x - padding;

        let icon_spacing = icon_size + padding;
        let icon_index = relative_x / icon_spacing;

        if icon_index < 4 && (relative_x % icon_spacing) < icon_size {
            Some(icon_index as usize)
        } else {
            None
        }
    }

    fn handle_dock_click(&mut self, icon: usize) {
        match icon {
            0 => { // Files
                log("Desktop: Files icon clicked");
                self.wm.create_window("Files", 100, 100, 400, 300);
            }
            1 => { // Settings
                log("Desktop: Settings icon clicked");
                self.wm.create_window("Settings", 150, 150, 350, 280);
            }
            2 => { // Browser
                log("Desktop: Browser icon clicked");
                self.wm.create_window("Browser", 200, 120, 600, 400);
            }
            3 => { // Terminal
                log("Desktop: Terminal icon clicked");
                self.launch_terminal();
            }
            _ => {}
        }
    }

    fn launch_terminal(&mut self) {
        let id = self.wm.create_window("Terminal", 120, 80, 560, 360);

        // Draw terminal content (in a real implementation, this would be
        // handled by the terminal application via IPC)
        if let Some(window) = self.wm.get_window(id) {
            let x = window.x.max(0) as u32;
            let y = window.y.max(0) as u32;

            // Terminal background
            let content_x = x + BORDER_WIDTH;
            let content_y = y + TITLE_BAR_HEIGHT;
            let content_w = window.width - BORDER_WIDTH * 2;
            let content_h = window.height - TITLE_BAR_HEIGHT - BORDER_WIDTH;

            self.fb.fill_rect(content_x, content_y, content_w, content_h, Color::new(30, 30, 30));

            // Terminal text
            let text_x = content_x + 10;
            let text_y = content_y + 10;
            let bg = Color::new(30, 30, 30);
            let fg = Color::new(220, 220, 220);

            self.fb.draw_string(text_x, text_y, "Atom Terminal v1.0", fg, bg);
            self.fb.draw_string(text_x, text_y + 16, "Type 'help' for available commands.", Color::new(128, 128, 128), bg);

            // Prompt
            let prompt_y = text_y + 48;
            self.fb.draw_string(text_x, prompt_y, "user", Theme::ACCENT, bg);
            self.fb.draw_string(text_x + 32, prompt_y, "@", fg, bg);
            self.fb.draw_string(text_x + 40, prompt_y, "atom", Theme::ACCENT, bg);
            self.fb.draw_string(text_x + 72, prompt_y, ":~$", fg, bg);

            // Cursor
            self.fb.fill_rect(text_x + 104, prompt_y, 8, 14, fg);
        }
    }
}

// ============================================================================
// PS/2 Controller Initialization (Userspace)
// ============================================================================

fn init_ps2_controller() {
    log("Desktop: Initializing PS/2 controller...");

    // Clear any stale data from kernel buffers
    clear_keyboard_buffer();
    clear_mouse_buffer();

    // Flush output buffer with timeout (max 16 bytes to avoid infinite loop)
    for _ in 0..16 {
        if io::ps2_read_status().unwrap_or(0) & 0x01 == 0 {
            break;
        }
        let _ = io::ps2_read_data();
    }

    // Enable auxiliary device (mouse) - kernel may have already done this
    let _ = io::ps2_write_command(0xA8);

    // Enable keyboard
    let _ = io::ps2_write_command(0xAE);

    // Enable mouse data reporting
    let _ = io::ps2_write_aux_command(0xF4);
    // Brief wait for ACK (non-blocking)
    for _ in 0..100 {
        if io::ps2_read_status().unwrap_or(0) & 0x01 != 0 {
            let _ = io::ps2_read_data(); // consume ACK
            break;
        }
        core::hint::spin_loop();
    }

    log("Desktop: PS/2 controller initialized");
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[no_mangle]
pub extern "efiapi" fn efi_main(_: *const core::ffi::c_void, _: *const core::ffi::c_void) -> usize {
    main()
}

fn main() -> ! {
    log("Desktop Environment: Starting...");

    // Initialize PS/2 controller from userspace
    init_ps2_controller();

    // Get framebuffer
    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("Desktop: Failed to get framebuffer");
            exit(1);
        }
    };

    log("Desktop Environment: Framebuffer acquired");

    // Create desktop environment
    let mut desktop = Desktop::new(fb);

    log("Desktop Environment: Entering main loop");

    // Main event loop
    loop {
        // Handle input events
        if !desktop.handle_input() {
            break;
        }

        // Render scene
        desktop.render();

        // Yield to other processes
        yield_now();
    }

    log("Desktop Environment: Shutting down");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Desktop Environment: PANIC!");
    exit(0xFF);
}

// Allocator for alloc crate
use core::alloc::{GlobalAlloc, Layout};

struct SimpleAllocator;

unsafe impl GlobalAlloc for SimpleAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Simple bump allocator from a static buffer
        static mut HEAP: [u8; 65536] = [0; 65536];
        static mut OFFSET: usize = 0;

        let align = layout.align();
        let size = layout.size();

        let offset = (OFFSET + align - 1) & !(align - 1);
        let new_offset = offset + size;

        if new_offset > HEAP.len() {
            core::ptr::null_mut()
        } else {
            OFFSET = new_offset;
            HEAP.as_mut_ptr().add(offset)
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // No deallocation in this simple allocator
    }
}

#[global_allocator]
static ALLOCATOR: SimpleAllocator = SimpleAllocator;
