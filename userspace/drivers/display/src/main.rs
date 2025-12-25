// Display Driver - Userspace Framebuffer Manager
//
// This is a userspace driver that manages the system framebuffer and provides
// a compositing service for other applications. It runs entirely in Ring 3
// and communicates with the kernel via syscalls.
//
// Key responsibilities:
// - Acquire and manage the framebuffer from the kernel
// - Provide double buffering for smooth rendering
// - Expose a compositing API via IPC for other processes
// - Handle display resolution and format queries
//
// Architecture:
// - Uses atom_syscall library for kernel interaction
// - Exposes IPC ports for client applications
// - Manages a software back buffer for composition

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use atom_syscall::graphics::{Color, Framebuffer};
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

// ============================================================================
// Display Driver State
// ============================================================================

struct DisplayDriver {
    framebuffer: Framebuffer,
    back_buffer: Option<&'static mut [u32]>,
    width: u32,
    height: u32,
    stride: u32,
    dirty: bool,
}

impl DisplayDriver {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();
        let stride = fb.stride();
        
        Self {
            framebuffer: fb,
            back_buffer: None, // TODO: Allocate via shared memory
            width,
            height,
            stride,
            dirty: false,
        }
    }

    /// Clear the display to a solid color
    fn clear(&self, color: Color) {
        self.framebuffer.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Draw a rectangle
    fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        self.framebuffer.fill_rect(x, y, w, h, color);
    }

    /// Draw text
    fn draw_text(&self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        self.framebuffer.draw_string(x, y, text, fg, bg);
    }

    /// Get display dimensions
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
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
    log("Display Driver: Starting...");

    // Acquire framebuffer from kernel
    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("Display Driver: Failed to acquire framebuffer");
            exit(1);
        }
    };

    let driver = DisplayDriver::new(fb);
    
    log("Display Driver: Framebuffer acquired");

    // Display driver info
    let (width, height) = driver.dimensions();
    
    // Clear to dark theme background
    driver.clear(Color::new(46, 52, 64));

    // Draw status bar
    driver.fill_rect(0, 0, width, 24, Color::new(36, 41, 51));
    driver.draw_text(8, 4, "Atom Display Driver", Color::new(136, 192, 208), Color::new(36, 41, 51));

    // Draw driver info
    driver.draw_text(16, 40, "Display Driver Active", Color::WHITE, Color::new(46, 52, 64));
    driver.draw_text(16, 60, "Waiting for IPC clients...", Color::new(200, 200, 200), Color::new(46, 52, 64));

    log("Display Driver: Ready for IPC connections");

    // Main driver loop - wait for IPC messages
    loop {
        // TODO: Implement IPC message handling
        // - CreateSurface(width, height) -> surface_id
        // - DestroySurface(surface_id)
        // - BlitSurface(surface_id, x, y)
        // - Present() - flip back buffer to front

        yield_now();
    }
}

// ============================================================================
// Panic Handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Display Driver: PANIC!");
    exit(0xFF);
}
