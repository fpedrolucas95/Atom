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
use alloc::string::String;
use atom_syscall::thread::{sleep_ms, exit};
use atom_syscall::debug::log;
use libgui::application::Application;
use libgui::color::Color;
use libgui::event::{Event, WindowEvent};

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
    log("Demo Text: Starting...");

    let mut app = Application::new("Text Demo").expect("Failed to create application");
    let mut surface = app.create_window("Text Editor Pro", 500, 400).expect("Failed to create window");

    log("Demo Text: Window created");

    let mut text = String::from("Type something: ");
    let mut focused = false;
    let mut needs_redraw = true;

    loop {
        // Poll for events
        while let event = app.poll_event() {
            if let Event::None = event { break; }
            needs_redraw = true;
            match event {
                Event::Quit => exit(0),
                Event::Key(key) => {
                    if key.pressed {
                        if key.character == 8 { // Backspace
                            text.pop();
                        } else if key.character >= 32 && key.character < 127 {
                            text.push(key.character as char);
                        }
                    }
                }
                Event::Window(WindowEvent::Focus) => focused = true,
                Event::Window(WindowEvent::Unfocus) => focused = false,
                _ => {}
            }
        }

        if needs_redraw {
            // Render
            let bg_color = if focused { Color::rgb(59, 66, 82) } else { Color::rgb(46, 52, 64) };
            surface.clear(bg_color);

            surface.draw_string(10, 10, "Welcome to Atom OS Window Manager", Color::rgb(136, 192, 208), bg_color);
            surface.draw_string(10, 30, &text, Color::WHITE, bg_color);

            if focused {
                surface.draw_string(10, 380, "Status: FOCUSED", Color::rgb(163, 190, 140), bg_color);
            } else {
                surface.draw_string(10, 380, "Status: Click to focus", Color::rgb(191, 97, 106), bg_color);
            }

            surface.present();
            needs_redraw = false;
        }

        sleep_ms(16);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Demo Text: PANIC!");
    exit(0xFF);
}
