// Graphical user interface and cursor handling (User-Space Shell)
//
// This module implements a declarative graphical user interface running in
// USER MODE. It is managed as a system service (ui_shell) and interacts with
// the kernel via capabilities (FrameBufferCap, PointerCap).
//
// Key responsibilities:
// - Render a basic desktop layout (background, top bar, dock, windows)
// - Track and update a software-rendered mouse cursor in userspace
// - Integrate PS/2 mouse events (via syscalls/capabilities) into cursor updates
// - Manage partial redraws using save/restore of framebuffer regions
// - Act as the primary interface for early userspace interaction
//
// Design and implementation (Microkernel Architecture):
// - UI is isolated from the kernel; failures here do not compromise system stability.
// - Access to graphics memory is granted via Shared Memory / Framebuffer Capability.
// - Input events are received through bounded IPC queues or syscall polling.
// - Rendering remains CPU-bound but executes with 'High' thread priority.
//
// Safety and correctness notes:
// - Executes in Ring 3 (User Mode); restricted instructions are unavailable.
// - Memory access is limited to mapped regions (Text, Stack, Framebuffer).
// - Capability enforcement: Sends/Recvs require validated IPC ports.
// - Cooperative multitasking: Yields to the scheduler via `drive_cooperative_tick`.
//
// Limitations and future considerations:
// - Currently a single-tasking shell; needs a true Window Manager/Compositor.
// - Event loop is polling-based; transitioning to event-driven IPC (blocked wait).
// - Font rendering is bitmap-based; vector support planned for Phase 7.
//
// Public interface:
// - `run_userspace_shell` as the service entry point (Ring 3).

use spin::Mutex;

use crate::{graphics, log_warn};
use crate::graphics::Color;
use crate::util::UI_DIRTY;

struct Theme;

impl Theme {
    const DESKTOP_BG: Color = Color::new(46, 52, 64);
    const BAR_BG: Color = Color::new(36, 41, 51);
    const ACCENT: Color = Color::new(136, 192, 208);
    const TEXT_MAIN: Color = Color::new(236, 239, 244);
    const WINDOW_BG: Color = Color::new(255, 255, 255);
    const WINDOW_HEADER: Color = Color::new(216, 222, 233);
    const DOCK_BG: Color = Color::new(36, 41, 51);
    const CURSOR_FILL: Color = Color::new(255, 255, 255);
    const CURSOR_OUTLINE: Color = Color::BLACK;
}

const TOP_BAR_HEIGHT: u32 = 32;
#[allow(dead_code)]
const LOG_ORIGIN: &str = "ui";

#[derive(Copy, Clone)]
struct CursorPosition {
    x: u32,
    y: u32,
}

impl CursorPosition {
    fn centered(width: u32, height: u32) -> Self {
        Self { x: width / 2, y: height / 2 }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        self.x = self.x.saturating_add_signed(dx).clamp(0, width.saturating_sub(1));
        self.y = self.y.saturating_add_signed(-dy).clamp(0, height.saturating_sub(1));
    }
}

static CURSOR: Mutex<CursorPosition> = Mutex::new(CursorPosition { x: 0, y: 0 });

const CURSOR_BUFFER_SIZE: usize = 16 * 16 * 4;
static CURSOR_SAVED_REGION: Mutex<([u8; CURSOR_BUFFER_SIZE], bool, u32, u32)> =
    Mutex::new(([0; CURSOR_BUFFER_SIZE], false, 0, 0));

#[inline(always)]
fn mouse_poll_delta() -> Option<(i32, i32)> {
    // Mouse driver moved to user space
    // TODO: Get mouse delta from user space driver via IPC
    None
}

fn save_cursor_region(x: u32, y: u32) {
    graphics::with_framebuffer(|fb| {
        let mut saved = CURSOR_SAVED_REGION.lock();
        saved.1 = true;
        saved.2 = x;
        saved.3 = y;

        let fb_addr = fb.address() as *const u8;
        let stride = fb.stride() as usize;
        let bpp = fb.bytes_per_pixel();

        for row in 0..16 {
            for col in 0..16 {
                let px = x + col;
                let py = y + row;

                if px >= fb.width() || py >= fb.height() {
                    continue;
                }

                let pixel_offset = (py * stride as u32 + px) as usize * bpp;
                let buffer_offset = (row * 16 + col) as usize * 4;

                unsafe {
                    for i in 0..4.min(bpp) {
                        saved.0[buffer_offset + i] = *fb_addr.add(pixel_offset + i);
                    }
                }
            }
        }
    });
}

fn restore_cursor_region() {
    let saved = CURSOR_SAVED_REGION.lock();
    if !saved.1 {
        return; 
    }

    let (ref buffer, _, x, y) = *saved;

    graphics::with_framebuffer(|fb| {
        let fb_addr = fb.address();
        let stride = fb.stride() as usize;
        let bpp = fb.bytes_per_pixel();

        for row in 0..16 {
            for col in 0..16 {
                let px = x + col;
                let py = y + row;

                if px >= fb.width() || py >= fb.height() {
                    continue;
                }

                let pixel_offset = (py * stride as u32 + px) as usize * bpp;
                let buffer_offset = (row * 16 + col) as usize * 4;

                unsafe {
                    for i in 0..4.min(bpp) {
                        let ptr = fb_addr.add(pixel_offset + i);
                        ptr.write_volatile(buffer[buffer_offset + i]);
                    }
                }
            }
        }
    });
}

fn draw_cursor_only(x: u32, y: u32) {
    graphics::with_framebuffer(|fb| {
        draw_arrow_cursor(fb, x, y);
    });
}

pub extern "C" fn run_userspace_shell() -> ! {
    use core::sync::atomic::{AtomicU64, Ordering};

    crate::log_info!("ui", "=== UI SHELL ENTRY POINT REACHED ===");

    static ITERATIONS: AtomicU64 = AtomicU64::new(0);
    static LAST_FULL_REDRAW_TIME: AtomicU64 = AtomicU64::new(0);
    const FULL_REDRAW_INTERVAL_MS: u64 = 16;

    crate::log_info!("ui", "Attempting to get framebuffer dimensions...");

    let (screen_w, screen_h) = match graphics::get_dimensions() {
        Some(d) => {
            crate::log_info!("ui", "Framebuffer ready: {}x{}", d.0, d.1);
            d
        }
        None => {
            crate::log_error!("ui", "CRITICAL: Framebuffer not available, entering fallback loop");
            loop {
                crate::sched::drive_cooperative_tick();
            }
        }
    };

    crate::log_info!("ui", "Initializing cursor at center: ({}, {})", screen_w / 2, screen_h / 2);

    {
        let mut cursor = CURSOR.lock();
        *cursor = CursorPosition::centered(screen_w, screen_h);
    }

    crate::log_info!("ui", "Drawing initial scene...");
    redraw_scene_with_heartbeat(0);

    {
        let cursor = CURSOR.lock();
        save_cursor_region(cursor.x, cursor.y);
        draw_cursor_only(cursor.x, cursor.y);
    }

    crate::log_info!("ui", "Initial scene drawn successfully");

    crate::log_info!("ui", "Entering main UI loop");

    loop {
        let iteration = ITERATIONS.fetch_add(1, Ordering::Relaxed);

        if let Some((dx, dy)) = mouse_poll_delta() {
            if iteration < 10 {
                crate::log_info!("ui", "Mouse delta: dx={}, dy={}", dx, dy);
            }

            restore_cursor_region();

            {
                let mut cursor = CURSOR.lock();
                cursor.apply_delta(dx, dy, screen_w, screen_h);
            }

            let cursor_pos = {
                let cursor = CURSOR.lock();
                (cursor.x, cursor.y)
            };

            save_cursor_region(cursor_pos.0, cursor_pos.1);
            draw_cursor_only(cursor_pos.0, cursor_pos.1);
        }

        let current_time_ms = crate::interrupts::handlers::get_ticks() * 10;

        if UI_DIRTY.load(Ordering::Acquire) {
            let last_full_redraw = LAST_FULL_REDRAW_TIME.load(Ordering::Relaxed);

            if current_time_ms.saturating_sub(last_full_redraw) >= FULL_REDRAW_INTERVAL_MS {
                UI_DIRTY.store(false, Ordering::Release);
                LAST_FULL_REDRAW_TIME.store(current_time_ms, Ordering::Relaxed);

                redraw_scene_with_heartbeat(iteration);

                let cursor_pos = {
                    let cursor = CURSOR.lock();
                    (cursor.x, cursor.y)
                };

                save_cursor_region(cursor_pos.0, cursor_pos.1);
                draw_cursor_only(cursor_pos.0, cursor_pos.1);

                if iteration % 500 == 0 {
                    crate::log_debug!("ui", "Full redraw #{}", iteration);
                }
            }
        }

        crate::sched::drive_cooperative_tick();
    }
}

#[allow(dead_code)]
pub fn prepare_boot_surface() {
    if !graphics::is_initialized() {
        log_warn!(LOG_ORIGIN, "Framebuffer not initialized; UI shell unavailable");
        return;
    }
    redraw_scene_with_heartbeat(0);
}

fn redraw_scene_with_heartbeat(redraws: u64) {
    crate::log_debug!("ui", "redraw_scene_with_heartbeat called (redraws={})", redraws);

    graphics::with_framebuffer(|fb| {
        crate::log_debug!("ui", "Inside framebuffer closure");

        let width = fb.width();
        let height = fb.height();

        fb.fill_rect(0, 0, width, height, Theme::DESKTOP_BG);
        draw_mock_window(fb, 100, 100, 400, 300, "Welcome to Atom");
        draw_dock(fb, width, height);
        draw_top_bar(fb, width);

        let debug_x = 10;
        let debug_y = TOP_BAR_HEIGHT + 10;
        let blink = (redraws & 1) == 0;
        fb.fill_rect(
            debug_x,
            debug_y,
            120,
            12,
            if blink { Color::new(255, 255, 0) } else { Color::new(80, 80, 0) },
        );
        crate::log_debug!("ui", "All drawing completed");
    });

    crate::log_debug!("ui", "redraw_scene_with_heartbeat exiting");
}

fn draw_top_bar(fb: &mut graphics::Framebuffer, width: u32) {
    fb.fill_rect(0, 0, width, TOP_BAR_HEIGHT, Theme::BAR_BG);
    fb.draw_string(16, 8, "Atom", Theme::ACCENT, Theme::BAR_BG);
    fb.draw_string(80, 8, "|  System Ready", Theme::TEXT_MAIN, Theme::BAR_BG);
    let clock_x = width.saturating_sub(100);
    fb.draw_string(clock_x, 8, "12:00 PM", Theme::TEXT_MAIN, Theme::BAR_BG);
}

fn draw_dock(fb: &mut graphics::Framebuffer, width: u32, height: u32) {
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

fn draw_mock_window(
    fb: &mut graphics::Framebuffer,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    title: &str,
) {
    fb.fill_rect(x + 4, y + 4, w, h, Color::new(20, 20, 20));
    fb.fill_rect(x, y, w, h, Theme::WINDOW_BG);

    let header_h = 24;
    fb.fill_rect(x, y, w, header_h, Theme::WINDOW_HEADER);
    fb.draw_string(x + 8, y + 6, title, Color::BLACK, Theme::WINDOW_HEADER);

    fb.fill_rect(x + w - 20, y + 6, 12, 12, Color::new(255, 90, 90));
}

fn draw_arrow_cursor(fb: &mut graphics::Framebuffer, x: u32, y: u32) {
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