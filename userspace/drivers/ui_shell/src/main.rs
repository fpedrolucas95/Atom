// Userspace UI Shell Driver
//
// This is a complete userspace driver that handles:
// - PS/2 mouse input with 1:1 movement (no acceleration)
// - PS/2 keyboard input
// - Framebuffer-based graphics rendering
// - Cursor rendering and movement
//
// This driver runs entirely in Ring 3 (userspace) and communicates with
// the kernel via syscalls for IO port access and framebuffer information.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// ============================================================================
// Syscall Numbers (must match kernel/src/syscall/mod.rs)
// ============================================================================

const SYS_THREAD_YIELD: u64 = 0;
const SYS_THREAD_EXIT: u64 = 1;
const SYS_MOUSE_POLL: u64 = 33;
const SYS_IO_PORT_READ: u64 = 34;
const SYS_IO_PORT_WRITE: u64 = 35;
const SYS_KEYBOARD_POLL: u64 = 36;
const SYS_GET_FRAMEBUFFER: u64 = 37;
const SYS_GET_TICKS: u64 = 38;
const SYS_DEBUG_LOG: u64 = 39;

const EWOULDBLOCK: u64 = u64::MAX - 8;

// ============================================================================
// PS/2 Controller Constants
// ============================================================================

const PS2_DATA_PORT: u16 = 0x60;
const PS2_STATUS_PORT: u16 = 0x64;
const PS2_COMMAND_PORT: u16 = 0x64;

// ============================================================================
// Theme Colors
// ============================================================================

#[derive(Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
}

impl Color {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
    
    const BLACK: Color = Color::new(0, 0, 0);
    const WHITE: Color = Color::new(255, 255, 255);
}

struct Theme;
impl Theme {
    const DESKTOP_BG: Color = Color::new(46, 52, 64);
    const BAR_BG: Color = Color::new(36, 41, 51);
    const ACCENT: Color = Color::new(136, 192, 208);
    const TEXT_MAIN: Color = Color::new(236, 239, 244);
    const WINDOW_BG: Color = Color::new(255, 255, 255);
    const WINDOW_HEADER: Color = Color::new(216, 222, 233);
    const DOCK_BG: Color = Color::new(36, 41, 51);
    const CURSOR_FILL: Color = Color::WHITE;
    const CURSOR_OUTLINE: Color = Color::BLACK;
}

// ============================================================================
// Syscall Interface
// ============================================================================

#[inline(always)]
unsafe fn syscall0(num: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline(always)]
unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

#[inline(always)]
unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "syscall",
        inout("rax") num => ret,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

fn thread_yield() {
    unsafe { syscall0(SYS_THREAD_YIELD); }
}

fn thread_exit(code: u64) -> ! {
    unsafe { 
        syscall1(SYS_THREAD_EXIT, code);
        loop { core::arch::asm!("hlt"); }
    }
}

fn mouse_poll() -> Option<(i32, i32)> {
    let result = unsafe { syscall0(SYS_MOUSE_POLL) };
    if result == EWOULDBLOCK {
        None
    } else {
        let dx = (result >> 32) as i32;
        let dy = result as i32;
        Some((dx, dy))
    }
}

fn keyboard_poll() -> Option<u8> {
    let result = unsafe { syscall0(SYS_KEYBOARD_POLL) };
    if result == EWOULDBLOCK {
        None
    } else {
        Some(result as u8)
    }
}

fn get_ticks() -> u64 {
    unsafe { syscall0(SYS_GET_TICKS) }
}

fn debug_log(msg: &str) {
    unsafe {
        syscall2(SYS_DEBUG_LOG, msg.as_ptr() as u64, msg.len() as u64);
    }
}

// ============================================================================
// Framebuffer
// ============================================================================

struct FramebufferInfo {
    address: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
    bytes_per_pixel: u32,
}

fn get_framebuffer_info() -> Option<FramebufferInfo> {
    let mut info: [u64; 5] = [0; 5];
    let result = unsafe { syscall1(SYS_GET_FRAMEBUFFER, info.as_mut_ptr() as u64) };
    
    if result == 0 {
        Some(FramebufferInfo {
            address: info[0] as *mut u8,
            width: info[1] as u32,
            height: info[2] as u32,
            stride: info[3] as u32,
            bytes_per_pixel: info[4] as u32,
        })
    } else {
        None
    }
}

struct Framebuffer {
    address: *mut u8,
    width: u32,
    height: u32,
    stride: u32,
    bytes_per_pixel: u32,
}

impl Framebuffer {
    fn new(info: FramebufferInfo) -> Self {
        Self {
            address: info.address,
            width: info.width,
            height: info.height,
            stride: info.stride,
            bytes_per_pixel: info.bytes_per_pixel,
        }
    }

    fn draw_pixel(&mut self, x: u32, y: u32, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }

        let pixel_offset = (y * self.stride + x) as usize * self.bytes_per_pixel as usize;
        let pixel_ptr = unsafe { self.address.add(pixel_offset) as *mut u32 };

        // Assume BGR format (common for UEFI)
        let pixel_value = ((color.b as u32) << 16) | ((color.g as u32) << 8) | (color.r as u32);

        unsafe {
            pixel_ptr.write_volatile(pixel_value);
        }
    }

    fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        for dy in 0..height {
            for dx in 0..width {
                self.draw_pixel(x + dx, y + dy, color);
            }
        }
    }

    fn draw_string(&mut self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        let mut offset_x = x;
        for byte in text.bytes() {
            if offset_x + 8 > self.width {
                break;
            }
            self.draw_char(offset_x, y, byte, fg, bg);
            offset_x += 8;
        }
    }

    fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let glyph = get_font_glyph(ch);
        for row in 0..8u32 {
            for col in 0..8u32 {
                let bit = (glyph[row as usize] >> col) & 1;
                let color = if bit == 1 { fg } else { bg };
                self.draw_pixel(x + col, y + row, color);
            }
        }
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

        for row in 0..16u32 {
            for col in 0..16u32 {
                let px = self.x + col;
                let py = self.y + row;

                if px < fb.width && py < fb.height {
                    let pixel_offset = (py * fb.stride + px) as usize * fb.bytes_per_pixel as usize;
                    let pixel_ptr = unsafe { fb.address.add(pixel_offset) as *const u32 };
                    self.saved_region[(row * 16 + col) as usize] = unsafe { pixel_ptr.read_volatile() };
                }
            }
        }
    }

    fn restore_region(&self, fb: &mut Framebuffer) {
        if !self.has_saved {
            return;
        }

        for row in 0..16u32 {
            for col in 0..16u32 {
                let px = self.saved_x + col;
                let py = self.saved_y + row;

                if px < fb.width && py < fb.height {
                    let pixel_offset = (py * fb.stride + px) as usize * fb.bytes_per_pixel as usize;
                    let pixel_ptr = unsafe { fb.address.add(pixel_offset) as *mut u32 };
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

fn draw_scene(fb: &mut Framebuffer) {
    let width = fb.width;
    let height = fb.height;

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

fn draw_window(fb: &mut Framebuffer, x: u32, y: u32, w: u32, h: u32, title: &str) {
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

fn draw_dock(fb: &mut Framebuffer, width: u32, height: u32) {
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

fn draw_cursor(fb: &mut Framebuffer, x: u32, y: u32) {
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

// ============================================================================
// Simple 8x8 Font
// ============================================================================

fn get_font_glyph(ch: u8) -> [u8; 8] {
    match ch {
        b'A' => [0x18, 0x24, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x00],
        b'B' => [0x7C, 0x42, 0x7C, 0x42, 0x42, 0x42, 0x7C, 0x00],
        b'C' => [0x3C, 0x42, 0x40, 0x40, 0x40, 0x42, 0x3C, 0x00],
        b'D' => [0x78, 0x44, 0x42, 0x42, 0x42, 0x44, 0x78, 0x00],
        b'E' => [0x7E, 0x40, 0x7C, 0x40, 0x40, 0x40, 0x7E, 0x00],
        b'F' => [0x7E, 0x40, 0x7C, 0x40, 0x40, 0x40, 0x40, 0x00],
        b'G' => [0x3C, 0x42, 0x40, 0x4E, 0x42, 0x42, 0x3C, 0x00],
        b'H' => [0x42, 0x42, 0x7E, 0x42, 0x42, 0x42, 0x42, 0x00],
        b'I' => [0x3E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x3E, 0x00],
        b'J' => [0x1E, 0x04, 0x04, 0x04, 0x04, 0x44, 0x38, 0x00],
        b'K' => [0x42, 0x44, 0x78, 0x44, 0x42, 0x42, 0x42, 0x00],
        b'L' => [0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x7E, 0x00],
        b'M' => [0x42, 0x66, 0x5A, 0x42, 0x42, 0x42, 0x42, 0x00],
        b'N' => [0x42, 0x62, 0x52, 0x4A, 0x46, 0x42, 0x42, 0x00],
        b'O' => [0x3C, 0x42, 0x42, 0x42, 0x42, 0x42, 0x3C, 0x00],
        b'P' => [0x7C, 0x42, 0x42, 0x7C, 0x40, 0x40, 0x40, 0x00],
        b'Q' => [0x3C, 0x42, 0x42, 0x42, 0x4A, 0x44, 0x3A, 0x00],
        b'R' => [0x7C, 0x42, 0x42, 0x7C, 0x44, 0x42, 0x42, 0x00],
        b'S' => [0x3C, 0x42, 0x40, 0x3C, 0x02, 0x42, 0x3C, 0x00],
        b'T' => [0x7F, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x00],
        b'U' => [0x42, 0x42, 0x42, 0x42, 0x42, 0x42, 0x3C, 0x00],
        b'V' => [0x42, 0x42, 0x42, 0x42, 0x24, 0x24, 0x18, 0x00],
        b'W' => [0x42, 0x42, 0x42, 0x5A, 0x5A, 0x66, 0x42, 0x00],
        b'X' => [0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x42, 0x00],
        b'Y' => [0x41, 0x22, 0x14, 0x08, 0x08, 0x08, 0x08, 0x00],
        b'Z' => [0x7E, 0x04, 0x08, 0x10, 0x20, 0x40, 0x7E, 0x00],
        b'a' => [0x00, 0x00, 0x3C, 0x02, 0x3E, 0x42, 0x3E, 0x00],
        b'b' => [0x40, 0x40, 0x5C, 0x62, 0x42, 0x42, 0x7C, 0x00],
        b'c' => [0x00, 0x00, 0x3C, 0x42, 0x40, 0x42, 0x3C, 0x00],
        b'd' => [0x02, 0x02, 0x3A, 0x46, 0x42, 0x42, 0x3E, 0x00],
        b'e' => [0x00, 0x00, 0x3C, 0x42, 0x7E, 0x40, 0x3C, 0x00],
        b'f' => [0x0C, 0x12, 0x10, 0x7C, 0x10, 0x10, 0x10, 0x00],
        b'g' => [0x00, 0x00, 0x3E, 0x42, 0x3E, 0x02, 0x3C, 0x00],
        b'h' => [0x40, 0x40, 0x5C, 0x62, 0x42, 0x42, 0x42, 0x00],
        b'i' => [0x08, 0x00, 0x18, 0x08, 0x08, 0x08, 0x1C, 0x00],
        b'j' => [0x04, 0x00, 0x0C, 0x04, 0x04, 0x44, 0x38, 0x00],
        b'k' => [0x40, 0x40, 0x44, 0x48, 0x70, 0x48, 0x44, 0x00],
        b'l' => [0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x1C, 0x00],
        b'm' => [0x00, 0x00, 0x76, 0x49, 0x49, 0x49, 0x49, 0x00],
        b'n' => [0x00, 0x00, 0x5C, 0x62, 0x42, 0x42, 0x42, 0x00],
        b'o' => [0x00, 0x00, 0x3C, 0x42, 0x42, 0x42, 0x3C, 0x00],
        b'p' => [0x00, 0x00, 0x7C, 0x42, 0x7C, 0x40, 0x40, 0x00],
        b'q' => [0x00, 0x00, 0x3E, 0x42, 0x3E, 0x02, 0x02, 0x00],
        b'r' => [0x00, 0x00, 0x5C, 0x62, 0x40, 0x40, 0x40, 0x00],
        b's' => [0x00, 0x00, 0x3E, 0x40, 0x3C, 0x02, 0x7C, 0x00],
        b't' => [0x10, 0x10, 0x7C, 0x10, 0x10, 0x12, 0x0C, 0x00],
        b'u' => [0x00, 0x00, 0x42, 0x42, 0x42, 0x46, 0x3A, 0x00],
        b'v' => [0x00, 0x00, 0x42, 0x42, 0x42, 0x24, 0x18, 0x00],
        b'w' => [0x00, 0x00, 0x41, 0x49, 0x49, 0x49, 0x36, 0x00],
        b'x' => [0x00, 0x00, 0x42, 0x24, 0x18, 0x24, 0x42, 0x00],
        b'y' => [0x00, 0x00, 0x42, 0x42, 0x3E, 0x02, 0x3C, 0x00],
        b'z' => [0x00, 0x00, 0x7E, 0x04, 0x18, 0x20, 0x7E, 0x00],
        b'0' => [0x3C, 0x42, 0x46, 0x5A, 0x62, 0x42, 0x3C, 0x00],
        b'1' => [0x08, 0x18, 0x08, 0x08, 0x08, 0x08, 0x1C, 0x00],
        b'2' => [0x3C, 0x42, 0x02, 0x0C, 0x30, 0x40, 0x7E, 0x00],
        b'3' => [0x3C, 0x42, 0x02, 0x1C, 0x02, 0x42, 0x3C, 0x00],
        b'4' => [0x04, 0x0C, 0x14, 0x24, 0x7E, 0x04, 0x04, 0x00],
        b'5' => [0x7E, 0x40, 0x7C, 0x02, 0x02, 0x42, 0x3C, 0x00],
        b'6' => [0x1C, 0x20, 0x40, 0x7C, 0x42, 0x42, 0x3C, 0x00],
        b'7' => [0x7E, 0x02, 0x04, 0x08, 0x10, 0x10, 0x10, 0x00],
        b'8' => [0x3C, 0x42, 0x42, 0x3C, 0x42, 0x42, 0x3C, 0x00],
        b'9' => [0x3C, 0x42, 0x42, 0x3E, 0x02, 0x04, 0x38, 0x00],
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        b'|' => [0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x00],
        _ => [0xFF, 0x81, 0x81, 0x81, 0x81, 0x81, 0xFF, 0x00],
    }
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    debug_log("UI Shell: Starting userspace shell driver");

    // Get framebuffer info
    let fb_info = match get_framebuffer_info() {
        Some(info) => info,
        None => {
            debug_log("UI Shell: Failed to get framebuffer info");
            thread_exit(1);
        }
    };

    debug_log("UI Shell: Framebuffer acquired");

    let mut fb = Framebuffer::new(fb_info);
    let mut cursor = CursorState::new(fb.width, fb.height);

    // Draw initial scene
    draw_scene(&mut fb);
    
    // Save initial cursor region and draw cursor
    cursor.save_region(&fb);
    draw_cursor(&mut fb, cursor.x, cursor.y);

    debug_log("UI Shell: Entering main loop");

    let mut iteration: u64 = 0;

    loop {
        iteration = iteration.wrapping_add(1);

        // Poll for mouse input
        if let Some((dx, dy)) = mouse_poll() {
            // Restore old cursor region
            cursor.restore_region(&mut fb);

            // Apply delta with 1:1 movement
            cursor.apply_delta(dx, dy, fb.width, fb.height);

            // Save new region and draw cursor
            cursor.save_region(&fb);
            draw_cursor(&mut fb, cursor.x, cursor.y);
        }

        // Poll for keyboard input
        while let Some(scancode) = keyboard_poll() {
            // Handle Escape key to exit (scancode 0x01)
            if scancode == 0x01 {
                debug_log("UI Shell: Escape pressed, exiting");
                thread_exit(0);
            }
        }

        // Yield to scheduler
        thread_yield();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    debug_log("UI Shell: PANIC!");
    thread_exit(0xFF)
}
