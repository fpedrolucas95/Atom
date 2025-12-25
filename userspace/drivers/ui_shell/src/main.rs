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
use atom_syscall::graphics::{Color, Framebuffer, get_framebuffer};
use atom_syscall::input::{mouse_poll, keyboard_poll};
use atom_syscall::thread::{yield_now, exit, get_ticks};
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
    let dock_h = 48;
    let dock_w = 400;
    let x_start = (width / 2).saturating_sub(dock_w / 2);
    let y_start = height.saturating_sub(dock_h + 10);

    fb.fill_rect(x_start, y_start, dock_w, dock_h, Theme::DOCK_BG);

    let colors = [
        Color::new(191, 97, 106),
        Color::new(163, 190, 140),
        Color::new(94, 129, 172),
        Theme::ACCENT,
    ];

    for (i, color) in colors.iter().enumerate() {
        let icon_size = 32;
        let padding = 16;
        let ix = x_start + padding + (i as u32 * (icon_size + padding));
        let iy = y_start + ((dock_h - icon_size) / 2);
        fb.fill_rect(ix, iy, icon_size, icon_size, *color);
    }
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

    // Draw initial scene
    draw_scene(&fb);
    
    // Save initial cursor region and draw cursor
    cursor.save_region(&fb);
    draw_cursor(&fb, cursor.x, cursor.y);

    log("UI Shell: Entering main loop");

    let mut iteration: u64 = 0;

    loop {
        iteration = iteration.wrapping_add(1);

        // Poll for mouse input
        if let Some((dx, dy)) = mouse_poll() {
            // Restore old cursor region
            cursor.restore_region(&fb);

            // Apply delta with 1:1 movement
            cursor.apply_delta(dx, dy, fb.width(), fb.height());

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
