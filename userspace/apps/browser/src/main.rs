//! Atom Browser — a small, fast HTML renderer for userspace.
//!
//! The browser targets three goals:
//!
//! * **HTML5 / CSS compatibility** — a real engine pipeline: a spec-shaped
//!   HTML5 tokenizer (tags, attributes, comments, CDATA, RCDATA/RAWTEXT/
//!   PLAINTEXT/script-data content models, full character-reference rules),
//!   tree construction with implied end tags, scope-aware closing,
//!   formatting-element reconstruction (tag-soup recovery), and foster
//!   parenting of table-misnested content, then a CSS engine with selectors
//!   (compound, combinators, attributes, structural pseudo-classes),
//!   specificity + `!important` cascade, inheritance, `@media` evaluation,
//!   `font-size`, and presentational-attribute support — fed by inline
//!   `style`, `<style>` blocks, external `<link rel=stylesheet>` sheets, and
//!   `@import`ed sheets.
//!   The styled DOM is flattened into renderer blocks: headings, flow content,
//!   nested lists, tables (one row per line), blockquotes, preformatted text,
//!   rules, images (PNG/JPEG/GIF over HTTP or `data:`), inline form controls,
//!   `linear-gradient` backgrounds, `display: flex` containers (row/column with `justify-content`,
//!   `align-items`, `gap`, `flex-wrap` multi-line layout, and content-aware
//!   `flex-grow`/`flex-basis` sizing), and decorated
//!   box containers (`background`, `padding`, `border`, `border-radius`,
//!   `box-shadow`, `box-sizing`, `margin` incl. `margin: auto` centring,
//!   `width`/`max-width`/`height`/`min-height` — pixel or percentage, resolved
//!   against the container at layout time — `overflow` clipping, `position`
//!   (`relative` offsets in flow; `absolute`/`fixed` boxes hoisted out of flow
//!   against the content area), and `z-index` paint ordering). The painter synthesises bold, italic, and font scaling the 8x8
//!   bitmap font has no native faces for. A hand-written JavaScript
//!   interpreter (`js/`) runs page scripts after tree construction — DOM
//!   mutation (`getElementById`, `querySelector`, `innerHTML`,
//!   `document.write`, `createElement`, `style`), `console`, `Math`, `JSON`,
//!   and the core language (closures, prototypes, try/catch, arrows) — under
//!   a step budget so a runaway script can never hang the browser. The page
//!   stays live after load: `click` events dispatch through registered
//!   handlers (`addEventListener`, `onclick` properties and attributes) with
//!   bubbling and `preventDefault`, `DOMContentLoaded`/`load` fire on load,
//!   `javascript:` links run in the page scope, and the document re-renders
//!   after handlers mutate it.
//! * **Low resource use** — a lazily-`mmap`ed bump heap, redraw only when
//!   state changes, and bounded network fetches.
//! * **Clean structure** — focused modules (allocator, text, entities,
//!   tokenizer, domtree, css, style, dom, html, render, browser) with single
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
mod domtree;
mod entities;
mod html;
mod js;
mod net;
mod render;
mod style;
mod text;
mod tokenizer;
mod url;

use alloc::format;
use core::alloc::Layout;
use core::panic::PanicInfo;

use atom_syscall::debug::log;
use atom_syscall::thread::{exit, get_time_ms, yield_now};
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
        // Update the JS engine's notion of time before any dispatch so
        // setTimeout deadlines and Date.now() reflect the real clock.
        let now_ms = get_time_ms();
        js::builtins::set_browser_time(now_ms);

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

        // Fire any expired timers; re-render if they mutated the DOM.
        browser.tick_timers(now_ms);

        // Idle without busy-spinning: block for events when nothing happened.
        if handled_event {
            yield_now();
        } else {
            app.wait_for_event(100);
        }
    }
}
