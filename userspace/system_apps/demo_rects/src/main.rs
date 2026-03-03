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

const HEAP_SIZE: usize = 128 * 1024; // 128 KB heap

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

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

use core::panic::PanicInfo;
use atom_syscall::thread::{sleep_ms, exit};
use atom_syscall::debug::log;
use libgui::application::Application;
use libgui::color::Color;
use libgui::event::Event;

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
    log("Demo Rects: Starting...");

    let mut app = Application::new("Rect Demo").expect("Failed to create application");
    let mut surface = app.create_window("Rectangle Mania", 400, 300).expect("Failed to create window");

    log("Demo Rects: Window created");

    let mut x = 0;
    let mut y = 0;
    let mut dx = 2;
    let mut dy = 2;

    loop {
        // Poll for events
        loop {
            match app.poll_event() {
                Event::Quit => exit(0),
                Event::None => break,
                _ => {}
            }
        }

        // Render
        surface.clear(Color::rgb(46, 52, 64));
        surface.fill_rect(x, y, 50, 50, Color::rgb(136, 192, 208));
        surface.draw_string(10, 10, "Moving Rectangle", Color::WHITE, Color::rgb(46, 52, 64));

        surface.present();

        // Move
        x = (x as i32 + dx) as u32;
        y = (y as i32 + dy) as u32;

        if x == 0 || x + 50 >= surface.width() { dx = -dx; }
        if y == 0 || y + 50 >= surface.height() { dy = -dy; }

        sleep_ms(16);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Demo Rects: PANIC!");
    exit(0xFF);
}
