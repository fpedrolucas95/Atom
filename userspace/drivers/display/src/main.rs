// Display Driver - Userspace Framebuffer Manager
//
// This is a userspace driver that manages the system framebuffer and provides
// a compositing service for other applications. It runs entirely in Ring 3
// and communicates with the kernel via syscalls.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use atom_syscall::graphics::{Color, Framebuffer};
use atom_syscall::ipc::create_port;
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;

use libipc::protocol::register_service;

// ============================================================================
// Display Driver State
// ============================================================================

struct DisplayDriver {
    framebuffer: Framebuffer,
    width: u32,
    height: u32,
}

impl DisplayDriver {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();
        
        Self {
            framebuffer: fb,
            width,
            height,
        }
    }

    fn clear(&self, color: Color) {
        self.framebuffer.fill_rect(0, 0, self.width, self.height, color);
    }

    fn fill_rect(&self, x: u32, y: u32, w: u32, h: u32, color: Color) {
        self.framebuffer.fill_rect(x, y, w, h, color);
    }

    fn draw_text(&self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        self.framebuffer.draw_string(x, y, text, fg, bg);
    }

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

    if let Ok(port) = create_port() {
        let _ = register_service("display", port);
    }

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

    let (width, _) = driver.dimensions();
    driver.clear(Color::new(46, 52, 64));
    driver.fill_rect(0, 0, width, 24, Color::new(36, 41, 51));
    driver.draw_text(8, 4, "Atom Display Driver", Color::new(136, 192, 208), Color::new(36, 41, 51));
    driver.draw_text(16, 40, "Display Driver Active", Color::WHITE, Color::new(46, 52, 64));

    log("Display Driver: Ready");

    loop {
        yield_now();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Display Driver: PANIC!");
    exit(0xFF);
}
