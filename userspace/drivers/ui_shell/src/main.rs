// Userspace UI Shell Driver
//
// This is a complete userspace driver that handles:
// - PS/2 mouse input with 1:1 movement (no acceleration)
// - PS/2 keyboard input
// - Framebuffer-based graphics rendering
// - Cursor rendering and movement
//
// This driver runs entirely in Ring 3 (userspace) and communicates with
// the kernel via the atom_syscall library. It is a TRUE userspace binary,
// not code that runs inside the kernel.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// Use the atom_syscall library for all kernel interactions
use atom_syscall::graphics::{Color, Framebuffer};
use atom_syscall::input::{keyboard_poll, MouseDriver};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

// ============================================================================
// Theme Colors
// ============================================================================

struct Theme;
impl Theme {
    const DESKTOP_BG: Color = Color::new(46, 52, 64);
    const BAR_BG: Color = Color::new(36, 41, 51);
    const ACCENT: Color = Color::new(136, 192, 208);
    const TEXT_MAIN: Color = Color::new(236, 239, 244);
    const WINDOW_BG: Color = Color::WHITE;
    const WINDOW_HEADER: Color = Color::new(216, 222, 233);
    const DOCK_BG: Color = Color::new(36, 41, 51);
    const CURSOR_FILL: Color = Color::WHITE;
    const CURSOR_OUTLINE: Color = Color::BLACK;
}

// ============================================================================
// Cursor State (using atom_syscall Framebuffer)
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

    /// Apply mouse delta with 1:1 movement (no acceleration)
    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        // Direct 1:1 mapping - no scaling, no acceleration
        let new_x = (self.x as i32).saturating_add(dx);
        let new_y = (self.y as i32).saturating_sub(dy); // Y is inverted in PS/2

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
// Drawing Functions
// ============================================================================

const TOP_BAR_HEIGHT: u32 = 32;
const DOCK_HEIGHT: u32 = 48;
const DOCK_WIDTH: u32 = 400;
const DOCK_ICON_SIZE: u32 = 32;
const DOCK_ICON_PADDING: u32 = 16;

// Dock icon types
#[derive(Clone, Copy, PartialEq)]
enum DockIcon {
    Files,
    Settings,
    Browser,
    Terminal,
}

impl DockIcon {
    fn color(&self) -> Color {
        match self {
            DockIcon::Files => Color::new(191, 97, 106),
            DockIcon::Settings => Color::new(163, 190, 140),
            DockIcon::Browser => Color::new(94, 129, 172),
            DockIcon::Terminal => Color::new(46, 46, 46), // Dark terminal color
        }
    }
}

const DOCK_ICONS: [DockIcon; 4] = [
    DockIcon::Files,
    DockIcon::Settings,
    DockIcon::Browser,
    DockIcon::Terminal,
];

// Dock position calculation helpers
fn get_dock_bounds(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let x_start = (width / 2).saturating_sub(DOCK_WIDTH / 2);
    let y_start = height.saturating_sub(DOCK_HEIGHT + 10);
    (x_start, y_start, DOCK_WIDTH, DOCK_HEIGHT)
}

fn get_icon_bounds(dock_x: u32, dock_y: u32, icon_index: usize) -> (u32, u32, u32, u32) {
    let ix = dock_x + DOCK_ICON_PADDING + (icon_index as u32 * (DOCK_ICON_SIZE + DOCK_ICON_PADDING));
    let iy = dock_y + ((DOCK_HEIGHT - DOCK_ICON_SIZE) / 2);
    (ix, iy, DOCK_ICON_SIZE, DOCK_ICON_SIZE)
}

fn draw_scene(fb: &Framebuffer) {
    let width = fb.width();
    let height = fb.height();

    // Desktop background
    fb.fill_rect(0, 0, width, height, Theme::DESKTOP_BG);

    // Top bar
    fb.fill_rect(0, 0, width, TOP_BAR_HEIGHT, Theme::BAR_BG);
    fb.draw_string(16, 8, "Atom", Theme::ACCENT, Theme::BAR_BG);
    fb.draw_string(80, 8, "|  Userspace Shell", Theme::TEXT_MAIN, Theme::BAR_BG);

    // Clock
    let clock_x = width.saturating_sub(100);
    fb.draw_string(clock_x, 8, "12:00 PM", Theme::TEXT_MAIN, Theme::BAR_BG);

    // Window
    draw_window(fb, 100, 100, 400, 300, "Welcome to Atom");

    // Dock
    draw_dock(fb, width, height);
}

fn draw_window(fb: &Framebuffer, x: u32, y: u32, w: u32, h: u32, title: &str) {
    // Shadow
    fb.fill_rect(x + 4, y + 4, w, h, Color::new(20, 20, 20));
    
    // Window body
    fb.fill_rect(x, y, w, h, Theme::WINDOW_BG);

    // Header
    let header_h = 24;
    fb.fill_rect(x, y, w, header_h, Theme::WINDOW_HEADER);
    fb.draw_string(x + 8, y + 6, title, Color::BLACK, Theme::WINDOW_HEADER);

    // Close button
    fb.fill_rect(x + w - 20, y + 6, 12, 12, Color::new(255, 90, 90));
}

fn draw_dock(fb: &Framebuffer, width: u32, height: u32) {
    let (x_start, y_start, dock_w, dock_h) = get_dock_bounds(width, height);

    fb.fill_rect(x_start, y_start, dock_w, dock_h, Theme::DOCK_BG);

    for (i, icon) in DOCK_ICONS.iter().enumerate() {
        let (ix, iy, icon_size, _) = get_icon_bounds(x_start, y_start, i);

        // Draw icon background
        fb.fill_rect(ix, iy, icon_size, icon_size, icon.color());

        // Draw special terminal icon with ">" prompt
        if *icon == DockIcon::Terminal {
            // Draw a simple terminal prompt ">" on the icon
            let prompt_color = Color::new(0, 255, 0); // Green terminal color
            // Draw a simple ">" character manually
            let px = ix + 8;
            let py = iy + 10;
            // Draw ">" shape
            for i in 0..6u32 {
                fb.draw_pixel(px + i, py + i, prompt_color);
                fb.draw_pixel(px + i, py + 12 - i, prompt_color);
            }
            // Draw underscore cursor
            for i in 0..8u32 {
                fb.draw_pixel(px + 10 + i, py + 10, prompt_color);
            }
        }
    }
}

/// Check if a click at (x, y) hits a dock icon
fn check_dock_click(x: u32, y: u32, screen_width: u32, screen_height: u32) -> Option<DockIcon> {
    let (dock_x, dock_y, _, _) = get_dock_bounds(screen_width, screen_height);

    for (i, icon) in DOCK_ICONS.iter().enumerate() {
        let (ix, iy, icon_w, icon_h) = get_icon_bounds(dock_x, dock_y, i);

        if x >= ix && x < ix + icon_w && y >= iy && y < iy + icon_h {
            return Some(*icon);
        }
    }

    None
}

// ============================================================================
// Terminal Window
// ============================================================================

struct TerminalTheme;
impl TerminalTheme {
    const WINDOW_BG: Color = Color::new(30, 30, 30);
    const TITLE_BAR: Color = Color::new(45, 45, 45);
    const TITLE_TEXT: Color = Color::new(200, 200, 200);
    const TEXT: Color = Color::new(220, 220, 220);
    const PROMPT: Color = Color::new(136, 192, 208);
    const PATH: Color = Color::new(163, 190, 140);
    const CURSOR: Color = Color::new(200, 200, 200);
}

/// Launch and draw a terminal window
fn launch_terminal(fb: &Framebuffer) {
    let x = 120;
    let y = 80;
    let w = 560;
    let h = 360;
    let title_h = 24;

    // Drop shadow
    fb.fill_rect(x + 4, y + 4, w, h, Color::new(0, 0, 0));

    // Window border
    fb.fill_rect(x, y, w, h, Color::new(60, 60, 60));

    // Title bar
    fb.fill_rect(x + 1, y + 1, w - 2, title_h - 1, TerminalTheme::TITLE_BAR);
    fb.draw_string(x + 10, y + 6, "Terminal", TerminalTheme::TITLE_TEXT, TerminalTheme::TITLE_BAR);

    // Window control buttons
    let btn_y = y + 6;
    let btn_x = x + w - 18;
    fb.fill_rect(btn_x, btn_y, 12, 12, Color::new(255, 95, 86));      // Close (red)
    fb.fill_rect(btn_x - 18, btn_y, 12, 12, Color::new(255, 189, 46)); // Minimize (yellow)
    fb.fill_rect(btn_x - 36, btn_y, 12, 12, Color::new(39, 201, 63));  // Maximize (green)

    // Terminal content area
    fb.fill_rect(x + 1, y + title_h, w - 2, h - title_h - 1, TerminalTheme::WINDOW_BG);

    // Draw terminal content
    let content_x = x + 10;
    let content_y = y + title_h + 10;
    let line_h = 16;

    // Welcome message
    fb.draw_string(content_x, content_y, "Atom Terminal v1.0", TerminalTheme::TEXT, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x, content_y + line_h, "Type 'help' for available commands.", Color::new(128, 128, 128), TerminalTheme::WINDOW_BG);

    // Empty line
    let prompt_y = content_y + line_h * 3;

    // Prompt: user@atom:~$
    fb.draw_string(content_x, prompt_y, "user", TerminalTheme::PROMPT, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x + 32, prompt_y, "@", TerminalTheme::TEXT, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x + 40, prompt_y, "atom", TerminalTheme::PROMPT, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x + 72, prompt_y, ":", TerminalTheme::TEXT, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x + 80, prompt_y, "~", TerminalTheme::PATH, TerminalTheme::WINDOW_BG);
    fb.draw_string(content_x + 88, prompt_y, "$", Color::new(180, 142, 173), TerminalTheme::WINDOW_BG);

    // Cursor (block cursor after prompt)
    fb.fill_rect(content_x + 104, prompt_y, 8, 14, TerminalTheme::CURSOR);
}

fn draw_cursor(fb: &Framebuffer, x: u32, y: u32) {
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
                1 => fb.draw_pixel(cx, cy, Theme::CURSOR_OUTLINE),
                2 => fb.draw_pixel(cx, cy, Theme::CURSOR_FILL),
                _ => {}
            }
        }
    }
}

// Font is now provided by the atom_syscall graphics library

// ============================================================================
// Main Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

/// UEFI entry point (required by x86_64-unknown-uefi target)
#[no_mangle]
pub extern "efiapi" fn efi_main(_image_handle: *const core::ffi::c_void, _system_table: *const core::ffi::c_void) -> usize {
    main()
}

fn main() -> ! {
    log("UI Shell: Starting userspace shell driver");

    // Get framebuffer from atom_syscall library
    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("UI Shell: Failed to get framebuffer");
            exit(1);
        }
    };

    log("UI Shell: Framebuffer acquired");

    let mut cursor = CursorState::new(fb.width(), fb.height());
    let mut mouse_driver = MouseDriver::new();
    let mut prev_left_button = false;
    let mut terminal_launched = false;

    // Draw initial scene
    draw_scene(&fb);
    
    // Save initial cursor region and draw cursor
    cursor.save_region(&fb);
    draw_cursor(&fb, cursor.x, cursor.y);

    log("UI Shell: Entering main loop");

    let mut iteration: u64 = 0;

    loop {
        iteration = iteration.wrapping_add(1);

        // Poll for mouse input with button states
        while let Some(event) = mouse_driver.poll_event() {
            // Restore old cursor region
            cursor.restore_region(&fb);

            // Apply delta with 1:1 movement
            cursor.apply_delta(event.dx, event.dy, fb.width(), fb.height());

            // Check for left button click (rising edge)
            if event.left_button && !prev_left_button {
                // Check if clicked on a dock icon
                if let Some(icon) = check_dock_click(cursor.x, cursor.y, fb.width(), fb.height()) {
                    match icon {
                        DockIcon::Terminal => {
                            if !terminal_launched {
                                log("UI Shell: Terminal icon clicked - launching terminal");
                                launch_terminal(&fb);
                                terminal_launched = true;
                            }
                        }
                        _ => {
                            log("UI Shell: Dock icon clicked");
                        }
                    }
                }
            }
            prev_left_button = event.left_button;

            // Save new region and draw cursor
            cursor.save_region(&fb);
            draw_cursor(&fb, cursor.x, cursor.y);
        }

        // Poll for keyboard input
        while let Some(scancode) = keyboard_poll() {
            // Handle Escape key to exit (scancode 0x01)
            if scancode == 0x01 {
                log("UI Shell: Escape pressed, exiting");
                exit(0);
            }
        }

        // Yield to scheduler
        yield_now();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("UI Shell: PANIC!");
    exit(0xFF);
}
