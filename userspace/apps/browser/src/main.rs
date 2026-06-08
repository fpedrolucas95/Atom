//! Atom Browser — a small, fast HTML renderer for userspace.
//!
//! The browser targets three goals:
//!
//! * **HTML5 / CSS compatibility** — a forgiving parser covering headings,
//!   flow content, ordered/unordered lists, blockquotes, preformatted text,
//!   rules, images (PNG/JPEG/GIF over HTTP or `data:`), inline form controls
//!   (text/search inputs, buttons, and `<select>` drop-downs flowing alongside
//!   text), centered content (`<center>` / `align`), inline formatting
//!   (`b`/`strong`, `i`/`em`, `code`, `u`, `a`, …), HTML entities (named and
//!   numeric), and a practical CSS subset for `color`, `font-weight`,
//!   `text-decoration`, and `display` via inline `style="..."` and `<style>`
//!   blocks.
//! * **Low resource use** — a lazily-`mmap`ed bump heap, zero-allocation tag
//!   tokenisation, redraw only when state changes, and bounded network fetches.
//! * **Clean structure** — the monolith is split into focused modules
//!   (allocator, text, url, net, css, dom, html, render, browser) with single
//!   responsibilities and shared primitives (DRY).
//!
//! Mouse: click links to navigate, click the address bar / Go button, click
//! input fields to focus, and scroll with the wheel.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod allocator;
mod browser;
mod content;
mod css;
mod dom;
mod html;
mod net;
mod render;
mod text;
mod url;

use alloc::format;
use core::alloc::Layout;
use core::panic::PanicInfo;

use atom_syscall::debug::log;
use atom_syscall::thread::{exit, yield_now};
use libgui::application::Application;
use libgui::event::{Event, WindowEvent};

use browser::Browser;

#[global_allocator]
static ALLOCATOR: allocator::MmapBumpAllocator = allocator::MmapBumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    log("browser: out of memory");
    exit(0xFE);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    log(&format!("browser: PANIC - {:?}", info));
    exit(0xFF);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("browser: starting");

    let mut app = match Application::new("Atom Browser") {
        Ok(app) => app,
        Err(_) => {
            log("browser: compositor unavailable");
            exit(1);
        }
    };

    let mut surface = match app.create_window("Atom Browser", 760, 520) {
        Ok(surface) => surface,
        Err(_) => {
            log("browser: failed to create window");
            exit(1);
        }
    };

    let mut browser = Browser::new();

    loop {
        let mut handled_event = false;
        loop {
            match app.poll_event() {
                Event::None => break,
                Event::Quit => exit(0),
                Event::Key(key) => {
                    handled_event = true;
                    browser.handle_key(key);
                }
                Event::Mouse(m) => {
                    handled_event = true;
                    browser.handle_mouse(m, surface.width());
                }
                Event::Window(WindowEvent::Focus) => {
                    handled_event = true;
                    browser.set_focused(true);
                }
                Event::Window(WindowEvent::Unfocus) => {
                    handled_event = true;
                    browser.set_focused(false);
                }
                Event::Window(WindowEvent::Resize { .. }) => {
                    handled_event = true;
                    browser.needs_redraw = true;
                }
                _ => {}
            }
        }

        if browser.needs_redraw {
            browser.render(&mut surface);
        }
        browser.finish_pending_load();

        // Idle without busy-spinning: block for events when nothing happened.
        if handled_event {
            yield_now();
        } else {
            app.wait_for_event(100);
        }
    }
}
