//! Demo Rects — Atom Design System v1.0 showcase
//!
//! Originally a "moving rectangle" smoke test.  Now migrated to the DS to
//! serve as a live proof that the token/widget stack is wired up correctly.
//!
//! ## What it demonstrates
//! - Colors exclusively from `atom_theme::colors` (zero hard-coded RGB)
//! - `Panel` elevated card with drop-shadow
//! - `Button` (Default / Primary / Danger) with full state machine
//! - Vertical gradient fill (`Surface::fill_rect_gradient_v`)
//! - Rounded gradient + glow (`fill_rect_rounded_aa_gradient_v` + shadow layers)
//! - Colour swatches for the full Luminous Dark palette
//! - Spacing and radius tokens throughout

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

// ── Allocator ────────────────────────────────────────────────────────────────

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

const HEAP_SIZE: usize = 256 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}
unsafe impl Sync for BumpAllocator {}
impl BumpAllocator {
    const fn new() -> Self {
        Self { heap: UnsafeCell::new([0; HEAP_SIZE]), next: AtomicUsize::new(0) }
    }
}
unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let cur  = self.next.load(Ordering::Relaxed);
            let aln  = (cur + align - 1) & !(align - 1);
            let next = aln + layout.size();
            if next > HEAP_SIZE { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(
                cur, next, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return (self.heap.get() as *mut u8).add(aln);
            }
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let base    = self.heap.get() as usize;
        let blk_end = (ptr as usize - base) + layout.size();
        let _ = self.next.compare_exchange(
            blk_end, ptr as usize - base, Ordering::SeqCst, Ordering::Relaxed,
        );
    }
}
#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_l: Layout) -> ! { loop {} }

// ── Imports ──────────────────────────────────────────────────────────────────

use core::panic::PanicInfo;
use atom_syscall::thread::{sleep_ms, exit};
use atom_syscall::debug::log;
use libgui::application::Application;
use libgui::color::Color;
use libgui::event::Event;

// DS tokens — all colours come from here, no raw RGB in this file
use atom_theme::colors::{
    ATOM_COLOR_BG, ATOM_COLOR_SURFACE, ATOM_COLOR_SURFACE_ALT,
    ATOM_COLOR_BORDER,
    ATOM_COLOR_TEXT_MUTED,
    ATOM_COLOR_ACCENT,
    ATOM_GRADIENT_PRIMARY_START, ATOM_GRADIENT_PRIMARY_END,
    ATOM_COLOR_SUCCESS, ATOM_COLOR_WARNING, ATOM_COLOR_ERROR,
    ATOM_COLOR_SHADOW,
};
use atom_theme::shadows;

// Widgets
use atom_ui::panel::Panel;
use atom_ui::button::Button;
use atom_ui::label::TextLabel;
use atom_ui::widget::WidgetEvent;

// ── Entry points ─────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

#[no_mangle]
pub extern "efiapi" fn efi_main(
    _image: *const core::ffi::c_void,
    _table: *const core::ffi::c_void,
) -> usize {
    main()
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() -> ! {
    log("Demo DS: starting Atom Design System v1.0 showcase");

    let mut app     = Application::new("DS Demo").expect("app");
    let mut surface = app.create_window("Atom Design System v1.0", 520, 420)
                         .expect("window");

    // ── Widget setup ──────────────────────────────────────────────────────

    // Background card
    let card = Panel::elevated(16, 16, 488, 388);

    // Title / subtitle labels
    let lbl_title = TextLabel::new(32, 28, "Atom Design System v1.0")
        .with_style(atom_theme::typography::WINDOW_TITLE);
    let lbl_sub   = TextLabel::new(32, 42, "Luminous Dark  |  tokens  |  gradients  |  glow")
        .secondary();
    let lbl_hint  = TextLabel::new(32, 56, "Zero hard-coded RGB.  All colors from atom_theme.")
        .muted();

    // Buttons (row 1)
    let mut btn_default = Button::new(    32,  80, 130, 28, "Secondary");
    let mut btn_primary = Button::primary(174,  80, 130, 28, "Apply");
    let mut btn_danger  = Button::danger( 316,  80, 120, 28, "Delete");

    // Bouncing gradient ball
    let ball_d:  u32 = 48;
    let ball_r:  u32 = 24;
    let mut bx: i32  = 200;
    let mut by: i32  = 220;
    let mut bdx: i32 = 3;
    let mut bdy: i32 = 2;

    // ── Render / event loop ───────────────────────────────────────────────

    loop {
        // Drain event queue
        loop {
            match app.poll_event() {
                Event::Quit => exit(0),
                Event::Mouse(ref me) => {
                    if btn_default.handle_mouse(me) == WidgetEvent::Clicked {
                        log("Demo DS: Secondary clicked");
                    }
                    if btn_primary.handle_mouse(me) == WidgetEvent::Clicked {
                        log("Demo DS: Apply clicked");
                    }
                    if btn_danger.handle_mouse(me) == WidgetEvent::Clicked {
                        log("Demo DS: Delete clicked");
                    }
                }
                Event::None => break,
                _ => {}
            }
        }

        let w = surface.width();
        let h = surface.height();

        // ── Draw ──────────────────────────────────────────────────────────

        // 1. Desktop gradient background
        let bg_s: Color = ATOM_COLOR_BG.into();
        let bg_e: Color = ATOM_COLOR_SURFACE.into();
        surface.fill_rect_gradient_v(0, 0, w, h, bg_s, bg_e);

        // 2. Elevated panel card
        card.draw(&mut surface);

        // 3. Text labels
        lbl_title.draw(&mut surface);
        lbl_sub.draw(&mut surface);
        lbl_hint.draw(&mut surface);

        // 4. Colour swatches (proof of tokens)
        draw_swatches(&mut surface, 32, 120);

        // 5. Buttons
        btn_default.draw(&mut surface);
        btn_primary.draw(&mut surface);
        btn_danger.draw(&mut surface);

        // 6. Bouncing gradient ball + shadow
        let spec = shadows::SOFT;
        let sc: Color = ATOM_COLOR_SHADOW.into();
        surface.draw_shadow_layers(
            bx, by, ball_d, ball_d, ball_r,
            sc, spec.offset_y, spec.layers, spec.alpha,
        );
        let gs: Color = ATOM_GRADIENT_PRIMARY_START.into();
        let ge: Color = ATOM_GRADIENT_PRIMARY_END.into();
        surface.fill_rect_rounded_aa_gradient_v(
            bx as u32, by as u32, ball_d, ball_d, ball_r, gs, ge,
        );

        // Label under ball
        let fl: Color = ATOM_COLOR_TEXT_MUTED.into();
        let fb: Color = ATOM_COLOR_BG.into();
        surface.draw_string(bx as u32, (by as u32).saturating_add(ball_d + 2),
                            "DS gradient", fl, fb);

        surface.present();

        // Animate ball — bounce inside the card (y >= 170 to stay below swatches)
        bx += bdx;
        by += bdy;
        let margin = 20i32;
        if bx < margin || bx + ball_d as i32 > w as i32 - margin { bdx = -bdx; }
        if by < 170 || by + ball_d as i32 > h as i32 - margin { bdy = -bdy; }

        sleep_ms(16);
    }
}

/// Draw a row of Luminous Dark palette swatches with labels below.
fn draw_swatches(surface: &mut libgui::surface::Surface, x: u32, y: u32) {
    const SW: u32 = 22;
    const SH: u32 = 14;
    const G:  u32 = 3;
    const R:  u32 = atom_theme::radius::XS;

    let entries: &[(atom_syscall::graphics::Color, &str)] = &[
        (ATOM_COLOR_BG,          "BG"),
        (ATOM_COLOR_SURFACE,     "SFC"),
        (ATOM_COLOR_SURFACE_ALT, "ALT"),
        (ATOM_COLOR_BORDER,      "BDR"),
        (ATOM_COLOR_ACCENT,      "ACC"),
        (ATOM_COLOR_SUCCESS,     "OK"),
        (ATOM_COLOR_WARNING,     "WRN"),
        (ATOM_COLOR_ERROR,       "ERR"),
    ];

    let mut cx = x;
    for (col, name) in entries {
        let c: Color = (*col).into();
        // Swatch
        surface.fill_rect_rounded_aa(cx, y, SW, SH, R, c);
        // Border so pure-black swatches remain visible
        let bc: Color = ATOM_COLOR_BORDER.into();
        surface.draw_rect_rounded_aa(cx, y, SW, SH, R, bc);
        // Name label below
        let fg: Color = ATOM_COLOR_TEXT_MUTED.into();
        let bg: Color = ATOM_COLOR_SURFACE.into();
        surface.draw_string(cx, y + SH + 1, name, fg, bg);
        cx += SW + G;
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    atom_syscall::debug::log("Demo DS: PANIC!");
    exit(0xFF);
}
