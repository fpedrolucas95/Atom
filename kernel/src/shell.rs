// Atom Desktop Environment
//
// Minimal, modern desktop environment running in Ring 3 (userspace).
// Design: Top panel + Desktop area + Bottom dock
//
// Uses syscalls to communicate with the kernel for:
// - Getting framebuffer information
// - Polling keyboard and mouse input
// - Yielding to other threads

use crate::syscall::{
    ESUCCESS, EWOULDBLOCK,
    SYS_THREAD_YIELD, SYS_GET_FRAMEBUFFER, SYS_KEYBOARD_POLL, SYS_MOUSE_POLL,
};

// ============================================================================
// Theme Colors (Nord-inspired)
// ============================================================================
const COLOR_BG_DARK: u32 = 0x002E3440;      // Desktop background
const COLOR_PANEL: u32 = 0x00242933;         // Panel/Dock background
const COLOR_ACCENT: u32 = 0x0088C0D0;        // Accent (cyan)
const COLOR_TEXT: u32 = 0x00ECEFF4;          // Primary text
const COLOR_TEXT_DIM: u32 = 0x004C566A;      // Dimmed text
const COLOR_DOCK_ICON: u32 = 0x003B4252;     // Dock icon background
const COLOR_DOCK_HOVER: u32 = 0x00434C5E;    // Dock icon hover
const COLOR_TERMINAL_BG: u32 = 0x001E1E1E;   // Terminal window background
const COLOR_TERMINAL_BAR: u32 = 0x002D2D2D;  // Terminal title bar
const COLOR_TERMINAL_TEXT: u32 = 0x00DCDCDC; // Terminal text
const COLOR_TERMINAL_PROMPT: u32 = 0x0088C0D0; // Terminal prompt color
const COLOR_TERMINAL_GREEN: u32 = 0x0000FF00; // Terminal icon green

// ============================================================================
// Layout Constants
// ============================================================================
const PANEL_HEIGHT: u32 = 24;
const DOCK_HEIGHT: u32 = 48;
const DOCK_ICON_SIZE: u32 = 40;
const DOCK_ICON_MARGIN: u32 = 4;

/// Entry point for the Atom Desktop Environment (Ring 3)
pub extern "C" fn shell_entry() -> ! {
    // Get framebuffer via syscall
    let mut fb_info = [0u64; 5];
    let fb_result = unsafe { syscall1(SYS_GET_FRAMEBUFFER, fb_info.as_mut_ptr() as u64) };

    if fb_result != ESUCCESS {
        loop { unsafe { syscall0(SYS_THREAD_YIELD); } }
    }

    let fb_addr = fb_info[0] as usize;
    let width = fb_info[1] as u32;
    let height = fb_info[2] as u32;
    let stride = fb_info[3] as u32;
    let bpp = fb_info[4] as usize;

    // Calculate layout
    let desktop_y = PANEL_HEIGHT;
    let desktop_height = height - PANEL_HEIGHT - DOCK_HEIGHT;
    let dock_y = height - DOCK_HEIGHT;

    // ========================================================================
    // Draw initial UI
    // ========================================================================

    // Desktop background
    fill_rect(fb_addr, stride, bpp, 0, desktop_y, width, desktop_height, COLOR_BG_DARK);

    // Top Panel
    draw_panel(fb_addr, stride, bpp, width);

    // Bottom Dock
    draw_dock(fb_addr, stride, bpp, width, dock_y);

    // ========================================================================
    // Cursor State
    // ========================================================================
    let mut cursor_x = width / 2;
    let mut cursor_y = height / 2;

    const CURSOR_WIDTH: u32 = 12;
    const CURSOR_HEIGHT: u32 = 16;
    let mut saved_region: [u32; 192] = [0; 192];
    let mut saved_x = cursor_x;
    let mut saved_y = cursor_y;

    save_cursor_region(fb_addr, stride, bpp, cursor_x, cursor_y, CURSOR_WIDTH, CURSOR_HEIGHT, &mut saved_region);
    draw_cursor(fb_addr, stride, bpp, cursor_x, cursor_y);

    // Mouse packet state
    let mut mouse_cycle = 0u8;
    let mut mouse_packet = [0u8; 3];
    let mut prev_left_button = false;
    let mut terminal_open = false;

    // Simple tick counter for clock updates
    let mut tick_counter: u32 = 0;
    let mut last_clock_update: u32 = 0;

    // Dock layout info for click detection
    let num_icons = 4u32;
    let dock_icon_width = DOCK_ICON_SIZE + DOCK_ICON_MARGIN;
    let dock_total_width = num_icons * dock_icon_width - DOCK_ICON_MARGIN;
    let dock_start_x = (width - dock_total_width) / 2;
    let dock_icon_y = dock_y + (DOCK_HEIGHT - DOCK_ICON_SIZE) / 2;

    // ========================================================================
    // Main Event Loop
    // ========================================================================
    loop {
        tick_counter = tick_counter.wrapping_add(1);

        // Update clock every ~1000 ticks (rough approximation)
        if tick_counter.wrapping_sub(last_clock_update) > 1000 {
            last_clock_update = tick_counter;
            // Clock area refresh - just redraw time placeholder
            fill_rect(fb_addr, stride, bpp, width - 60, 0, 60, PANEL_HEIGHT, COLOR_PANEL);
            draw_string(fb_addr, stride, bpp, width - 52, 8, "12:34", COLOR_TEXT);
        }

        // Poll keyboard
        loop {
            let scancode = unsafe { syscall0(SYS_KEYBOARD_POLL) };
            if scancode == EWOULDBLOCK { break; }

            if scancode == 0x01 { // ESC
                loop { unsafe { syscall0(SYS_THREAD_YIELD); } }
            }
        }

        // Poll mouse
        let mut total_dx: i32 = 0;
        let mut total_dy: i32 = 0;
        let mut mouse_moved = false;
        let mut left_button_pressed = false;

        // Debug counter for framebuffer sync (required for rendering)
        static mut DEBUG_BYTE_X: u32 = 0;

        loop {
            let byte_result = unsafe { syscall0(SYS_MOUSE_POLL) };
            if byte_result == EWOULDBLOCK { break; }

            let byte = byte_result as u8;

            // Framebuffer sync pulse (discreet, in panel area)
            unsafe {
                DEBUG_BYTE_X = (DEBUG_BYTE_X + 1) % 100;
                fill_rect(fb_addr, stride, bpp, DEBUG_BYTE_X, 0, 1, 1, COLOR_PANEL);
            }

            match mouse_cycle {
                0 => {
                    if byte & 0x08 != 0 {
                        mouse_packet[0] = byte;
                        mouse_cycle = 1;
                    }
                }
                1 => {
                    mouse_packet[1] = byte;
                    mouse_cycle = 2;
                }
                2 => {
                    mouse_packet[2] = byte;
                    mouse_cycle = 0;

                    let flags = mouse_packet[0];
                    if flags & 0xC0 != 0 { continue; }

                    let mut dx = mouse_packet[1] as i32;
                    if flags & 0x10 != 0 { dx -= 256; }

                    let mut dy = mouse_packet[2] as i32;
                    if flags & 0x20 != 0 { dy -= 256; }

                    total_dx += dx;
                    total_dy += dy;
                    mouse_moved = true;

                    // Check left button state
                    left_button_pressed = (flags & 0x01) != 0;
                }
                _ => mouse_cycle = 0,
            }
        }

        // Detect left button click (rising edge)
        if left_button_pressed && !prev_left_button {
            // Check if click is on terminal icon (icon index 3)
            let icon_idx = 3u32;
            let icon_x = dock_start_x + icon_idx * dock_icon_width;
            let icon_end_x = icon_x + DOCK_ICON_SIZE;
            let icon_end_y = dock_icon_y + DOCK_ICON_SIZE;

            if cursor_x >= icon_x && cursor_x < icon_end_x &&
               cursor_y >= dock_icon_y && cursor_y < icon_end_y {
                if !terminal_open {
                    // Restore cursor before drawing terminal
                    restore_cursor_region(fb_addr, stride, bpp, saved_x, saved_y, CURSOR_WIDTH, CURSOR_HEIGHT, &saved_region);

                    // Draw terminal window
                    draw_terminal_window(fb_addr, stride, bpp, width, height);
                    terminal_open = true;

                    // Save and redraw cursor
                    save_cursor_region(fb_addr, stride, bpp, cursor_x, cursor_y, CURSOR_WIDTH, CURSOR_HEIGHT, &mut saved_region);
                    saved_x = cursor_x;
                    saved_y = cursor_y;
                    draw_cursor(fb_addr, stride, bpp, cursor_x, cursor_y);
                }
            }
        }
        prev_left_button = left_button_pressed;

        if mouse_moved {
            // Sync pulse
            static mut SYNC_X: u32 = 0;
            unsafe {
                SYNC_X = (SYNC_X + 1) % 50;
                fill_rect(fb_addr, stride, bpp, 100 + SYNC_X, 0, 1, 1, COLOR_PANEL);
            }

            restore_cursor_region(fb_addr, stride, bpp, saved_x, saved_y, CURSOR_WIDTH, CURSOR_HEIGHT, &saved_region);

            let new_x = (cursor_x as i32 + total_dx).clamp(0, (width - CURSOR_WIDTH) as i32) as u32;
            let new_y = (cursor_y as i32 - total_dy).clamp(0, (height - CURSOR_HEIGHT) as i32) as u32;

            cursor_x = new_x;
            cursor_y = new_y;

            save_cursor_region(fb_addr, stride, bpp, cursor_x, cursor_y, CURSOR_WIDTH, CURSOR_HEIGHT, &mut saved_region);
            saved_x = cursor_x;
            saved_y = cursor_y;

            draw_cursor(fb_addr, stride, bpp, cursor_x, cursor_y);
        }

        unsafe { syscall0(SYS_THREAD_YIELD); }
    }
}

// ============================================================================
// UI Drawing Functions
// ============================================================================

fn draw_terminal_window(fb: usize, stride: u32, bpp: usize, screen_width: u32, screen_height: u32) {
    // Window dimensions and position (centered)
    let win_w = 500;
    let win_h = 320;
    let win_x = (screen_width - win_w) / 2;
    let win_y = (screen_height - win_h) / 2 - 20;
    let title_h = 28;

    // Drop shadow
    fill_rect(fb, stride, bpp, win_x + 4, win_y + 4, win_w, win_h, 0x00000000);

    // Window border
    fill_rect(fb, stride, bpp, win_x, win_y, win_w, win_h, 0x00404040);

    // Title bar
    fill_rect(fb, stride, bpp, win_x + 1, win_y + 1, win_w - 2, title_h - 1, COLOR_TERMINAL_BAR);
    draw_string(fb, stride, bpp, win_x + 12, win_y + 9, "Terminal", COLOR_TERMINAL_TEXT);

    // Window control buttons
    let btn_y = win_y + 8;
    let btn_x = win_x + win_w - 20;
    fill_rect(fb, stride, bpp, btn_x, btn_y, 12, 12, 0x00FF5F56);      // Close (red)
    fill_rect(fb, stride, bpp, btn_x - 18, btn_y, 12, 12, 0x00FFBD2E); // Minimize (yellow)
    fill_rect(fb, stride, bpp, btn_x - 36, btn_y, 12, 12, 0x0027C93F); // Maximize (green)

    // Terminal content area
    fill_rect(fb, stride, bpp, win_x + 1, win_y + title_h, win_w - 2, win_h - title_h - 1, COLOR_TERMINAL_BG);

    // Terminal content
    let content_x = win_x + 12;
    let content_y = win_y + title_h + 12;
    let line_h = 16;

    // Welcome message
    draw_string(fb, stride, bpp, content_x, content_y, "Atom Terminal v1.0", COLOR_TERMINAL_TEXT);
    draw_string(fb, stride, bpp, content_x, content_y + line_h, "Type 'help' for available commands.", 0x00808080);

    // Prompt line
    let prompt_y = content_y + line_h * 3;
    draw_string(fb, stride, bpp, content_x, prompt_y, "user", COLOR_TERMINAL_PROMPT);
    draw_string(fb, stride, bpp, content_x + 32, prompt_y, "@", COLOR_TERMINAL_TEXT);
    draw_string(fb, stride, bpp, content_x + 40, prompt_y, "atom", COLOR_TERMINAL_PROMPT);
    draw_string(fb, stride, bpp, content_x + 72, prompt_y, ":", COLOR_TERMINAL_TEXT);
    draw_string(fb, stride, bpp, content_x + 80, prompt_y, "~", 0x00A3BE8C);
    draw_string(fb, stride, bpp, content_x + 88, prompt_y, "$", 0x00B48EAD);

    // Cursor block
    fill_rect(fb, stride, bpp, content_x + 104, prompt_y, 8, 14, COLOR_TERMINAL_TEXT);
}

fn draw_panel(fb: usize, stride: u32, bpp: usize, width: u32) {
    // Panel background
    fill_rect(fb, stride, bpp, 0, 0, width, PANEL_HEIGHT, COLOR_PANEL);

    // Logo/Brand
    draw_string(fb, stride, bpp, 8, 8, "Atom", COLOR_ACCENT);

    // Clock (right side)
    draw_string(fb, stride, bpp, width - 52, 8, "12:34", COLOR_TEXT);
}

fn draw_dock(fb: usize, stride: u32, bpp: usize, width: u32, y: u32) {
    // Dock background
    fill_rect(fb, stride, bpp, 0, y, width, DOCK_HEIGHT, COLOR_PANEL);

    // Dock separator line
    fill_rect(fb, stride, bpp, 0, y, width, 1, COLOR_TEXT_DIM);

    // Center the dock icons
    let num_icons = 4;  // H, F, S, T (Terminal)
    let dock_width = num_icons * (DOCK_ICON_SIZE + DOCK_ICON_MARGIN) - DOCK_ICON_MARGIN;
    let dock_start_x = (width - dock_width) / 2;
    let icon_y = y + (DOCK_HEIGHT - DOCK_ICON_SIZE) / 2;

    // Draw dock icons
    for i in 0..num_icons {
        let icon_x = dock_start_x + i * (DOCK_ICON_SIZE + DOCK_ICON_MARGIN);
        draw_dock_icon(fb, stride, bpp, icon_x, icon_y, i);
    }
}

fn draw_dock_icon(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, icon_type: u32) {
    // Icon background (rounded appearance with solid rect for now)
    fill_rect(fb, stride, bpp, x, y, DOCK_ICON_SIZE, DOCK_ICON_SIZE, COLOR_DOCK_ICON);

    // Icon symbol (centered)
    let (symbol, color) = match icon_type {
        0 => ("H", COLOR_ACCENT),   // Home
        1 => ("F", COLOR_ACCENT),   // Files
        2 => ("S", COLOR_ACCENT),   // Settings
        3 => (">", COLOR_TERMINAL_GREEN),  // Terminal (green prompt)
        _ => ("?", COLOR_ACCENT),
    };

    let text_x = x + (DOCK_ICON_SIZE - 8) / 2;
    let text_y = y + (DOCK_ICON_SIZE - 8) / 2;
    draw_string(fb, stride, bpp, text_x, text_y, symbol, color);

    // Draw underscore cursor for terminal icon
    if icon_type == 3 {
        fill_rect(fb, stride, bpp, text_x + 8, text_y + 6, 6, 2, COLOR_TERMINAL_GREEN);
    }
}

fn save_cursor_region(
    fb: usize,
    stride: u32,
    bpp: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    buffer: &mut [u32; 192],
) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            let idx = (row * w + col) as usize;
            if idx < 192 {
                unsafe {
                    let ptr = (fb + offset) as *const u32;
                    buffer[idx] = ptr.read_volatile();
                }
            }
        }
    }
}

fn restore_cursor_region(
    fb: usize,
    stride: u32,
    bpp: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    buffer: &[u32; 192],
) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            let idx = (row * w + col) as usize;
            if idx < 192 {
                unsafe {
                    let ptr = (fb + offset) as *mut u32;
                    ptr.write_volatile(buffer[idx]);
                }
            }
        }
    }
}

fn fill_rect(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, w: u32, h: u32, color: u32) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            unsafe {
                let ptr = (fb + offset) as *mut u32;
                ptr.write_volatile(color);
            }
        }
    }
}

fn draw_cursor(fb: usize, stride: u32, bpp: usize, x: u32, y: u32) {
    let cursor = [
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
    
    for (row, cols) in cursor.iter().enumerate() {
        for (col, &px) in cols.iter().enumerate() {
            let color = match px {
                1 => 0x00000000, // Black outline
                2 => 0x00FFFFFF, // White fill
                _ => continue,
            };
            let offset = ((y + row as u32) * stride + (x + col as u32)) as usize * bpp;
            unsafe {
                let ptr = (fb + offset) as *mut u32;
                ptr.write_volatile(color);
            }
        }
    }
}

fn draw_string(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, s: &str, color: u32) {
    let mut cx = x;
    for ch in s.bytes() {
        draw_char(fb, stride, bpp, cx, y, ch, color);
        cx += 8;
    }
}

fn draw_char(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, ch: u8, color: u32) {
    let glyph = get_glyph(ch);
    for row in 0..8 {
        let bits = glyph[row];
        for col in 0..8 {
            if bits & (0x80 >> col) != 0 {
                let offset = ((y + row as u32) * stride + (x + col as u32)) as usize * bpp;
                unsafe {
                    let ptr = (fb + offset) as *mut u32;
                    ptr.write_volatile(color);
                }
            }
        }
    }
}

fn get_glyph(ch: u8) -> [u8; 8] {
    // Minimal 8x8 font (ASCII 32-127)
    match ch {
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'A' => [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'D' => [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00],
        b'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'M' => [0xC6, 0xEE, 0xFE, 0xD6, 0xC6, 0xC6, 0xC6, 0x00],
        b'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'W' => [0xC6, 0xC6, 0xC6, 0xD6, 0xFE, 0xEE, 0xC6, 0x00],
        b'a' => [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00],
        b'c' => [0x00, 0x00, 0x3C, 0x60, 0x60, 0x60, 0x3C, 0x00],
        b'd' => [0x06, 0x06, 0x3E, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'e' => [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00],
        b'f' => [0x1C, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x30, 0x00],
        b'g' => [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'i' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'l' => [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'm' => [0x00, 0x00, 0x6C, 0xFE, 0xD6, 0xC6, 0xC6, 0x00],
        b'n' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        b'o' => [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'p' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60],
        b'r' => [0x00, 0x00, 0x6E, 0x70, 0x60, 0x60, 0x60, 0x00],
        b's' => [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00],
        b't' => [0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x1C, 0x00],
        b'u' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'v' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'y' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'-' => [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00],
        b'(' => [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00],
        b')' => [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00],
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'2' => [0x3C, 0x66, 0x06, 0x1C, 0x30, 0x60, 0x7E, 0x00],
        b'3' => [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00],
        b'4' => [0x0C, 0x1C, 0x3C, 0x6C, 0x7E, 0x0C, 0x0C, 0x00],
        b'5' => [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00],
        b'6' => [0x3C, 0x60, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00],
        b'7' => [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x00],
        b'8' => [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00],
        b'9' => [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x06, 0x3C, 0x00],
        b':' => [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

// Syscall helpers
#[inline(always)]
unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}
