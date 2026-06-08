//! Atom Browser - basic HTML renderer for userspace.
//!
//! Implements a small, deterministic HTML subset: headings, paragraphs, lists,
//! preformatted text, line breaks, rules, clickable hyperlinks, images
//! (PNG/JPEG decoded via libimage, fetched over HTTP or from data: URIs), and
//! input/search boxes. Mouse is supported: click links to navigate, click the
//! address bar / Go button, click input fields to focus them, and scroll with
//! the wheel.

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::debug::log;
use atom_syscall::thread::{exit, yield_now};
use libgui::application::Application;
use libgui::color::Color;
use libgui::event::{Event, KeyEvent, MouseButton, MouseEvent, WindowEvent};
use libimage::{ImageDecoder, JpgDecoder, PngDecoder};
use libipc::protocol::lookup_service;

// Generous heap: the bump allocator never frees, and decoded images can be
// hundreds of KB. 16 MiB covers several page loads (with images) before reset.
const HEAP_SIZE: usize = 16 * 1024 * 1024;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0u8; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (cur + align - 1) & !(align - 1);
            let end = aligned + size;
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(cur, end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    loop {}
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

const CHAR_W: u32 = 8;
const CHAR_H: u32 = 8;
const LINE_H: u32 = 12;
const TOOLBAR_H: u32 = 42;
const STATUS_H: u32 = 20;
const PADDING: u32 = 12;
const ADDR_Y: u32 = 8;
const ADDR_H: u32 = 24;

// ============================================================================
// Document model
// ============================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum TextKind {
    H1,
    H2,
    H3,
    Paragraph,
    ListItem,
    Pre,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputKind {
    Text,
    Search,
    Submit,
}

/// An inline run of text, optionally part of hyperlink `link` (index into the
/// document's link table).
struct Run {
    text: String,
    link: Option<usize>,
}

enum Block {
    Text { kind: TextKind, runs: Vec<Run> },
    Rule,
    Image { alt: String, img: Option<libimage::DecodedImage>, src: String },
    Input { idx: usize },
}

struct InputMeta {
    kind: InputKind,
    placeholder: String,
    name: String,
    action: String,
}

/// A clickable region in screen coordinates, tied to a link or input index.
#[derive(Clone, Copy)]
struct Hit {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    idx: usize,
}

impl Hit {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

struct Browser {
    url: String,
    status: String,
    title: String,
    blocks: Vec<Block>,
    links: Vec<String>,
    inputs: Vec<InputMeta>,
    input_text: Vec<String>,
    focused_input: Option<usize>,
    link_hits: Vec<Hit>,
    input_hits: Vec<Hit>,
    scroll: u32,
    content_height: u32,
    view_h: u32,
    focused: bool,
    needs_redraw: bool,
}

impl Browser {
    fn new() -> Self {
        let mut browser = Self {
            url: String::from("about:home"),
            status: String::from("Ready"),
            title: String::from("Atom Browser"),
            blocks: Vec::new(),
            links: Vec::new(),
            inputs: Vec::new(),
            input_text: Vec::new(),
            focused_input: None,
            link_hits: Vec::new(),
            input_hits: Vec::new(),
            scroll: 0,
            content_height: 0,
            view_h: 1,
            focused: false,
            needs_redraw: true,
        };
        browser.load_current_url();
        browser
    }

    fn load_current_url(&mut self) {
        self.status = String::from("Loading...");
        self.scroll = 0;
        self.focused_input = None;

        let html = if self.url == "about:home" || self.url.is_empty() {
            self.title = String::from("Atom Browser");
            String::from(ABOUT_HOME)
        } else if self.url == "about:html" {
            self.title = String::from("HTML Demo");
            String::from(ABOUT_HTML)
        } else if let Some(http_url) = normalize_http_url(&self.url) {
            self.url = http_url;
            match fetch_http(&self.url) {
                Ok(body) => {
                    self.status = String::from("Loaded via HTTP");
                    body
                }
                Err(msg) => {
                    self.status = msg.clone();
                    format!(
                        "<h1>Could not load page</h1><p>{}</p><p>Try about:home or about:html.</p>",
                        msg
                    )
                }
            }
        } else {
            self.status = String::from("Unsupported URL");
            format!(
                "<h1>Unsupported URL</h1><p>Atom Browser supports about:home, about:html, and plain http:// URLs.</p><p>Requested: {}</p>",
                escape_text(&self.url)
            )
        };

        let parsed = parse_html(&html);
        if !parsed.title.is_empty() {
            self.title = parsed.title;
        }
        self.blocks = parsed.blocks;
        self.links = parsed.links;
        self.inputs = parsed.inputs;
        self.input_text = self.inputs.iter().map(|_| String::new()).collect();

        // Fetch and decode images now (NOT during render). Cap the number of
        // network fetches so a heavy page can't hang the UI for too long.
        let page_url = self.url.clone();
        let mut fetched = 0u32;
        for block in self.blocks.iter_mut() {
            if let Block::Image { src, img, .. } = block {
                if img.is_some() {
                    continue;
                }
                if src.trim_start().starts_with("data:") {
                    if let Some(decoded) = decode_data_uri(src).and_then(|b| decode_image(&b)) {
                        *img = Some(decoded);
                    }
                } else if fetched < 6 {
                    fetched += 1;
                    if let Some(url) = resolve_url(&page_url, src) {
                        if let Some(bytes) = fetch_url_bytes(&url) {
                            *img = decode_image(&bytes);
                        }
                    }
                }
            }
        }

        if self.status == "Loading..." {
            self.status = format!("Loaded {} blocks", self.blocks.len());
        }
        self.needs_redraw = true;
    }

    fn navigate_to(&mut self, href: &str) {
        let h = href.trim();
        if h.is_empty() || h.starts_with('#') {
            return;
        }
        if h.starts_with("https://") {
            self.status = String::from("HTTPS not supported yet (no TLS)");
            self.needs_redraw = true;
            return;
        }
        if h.starts_with("about:") {
            self.url = String::from(h);
            self.load_current_url();
            return;
        }
        if let Some(u) = resolve_url(&self.url, h) {
            self.url = u;
            self.load_current_url();
        } else if let Some(u) = normalize_http_url(h) {
            self.url = u;
            self.load_current_url();
        } else {
            self.status = format!("Cannot open: {}", h);
            self.needs_redraw = true;
        }
    }

    fn submit_input(&mut self, idx: usize) {
        if idx >= self.inputs.len() {
            return;
        }
        let action = self.inputs[idx].action.clone();
        let name = self.inputs[idx].name.clone();
        let value = self.input_text[idx].clone();
        let base = if action.is_empty() {
            self.url.clone()
        } else {
            action
        };
        let query = if name.is_empty() {
            percent_encode(&value)
        } else {
            format!("{}={}", name, percent_encode(&value))
        };
        let sep = if base.contains('?') { '&' } else { '?' };
        let target = format!("{}{}{}", base, sep, query);
        self.navigate_to(&target);
    }

    fn clamp_scroll(&mut self) {
        let max = self.content_height.saturating_sub(self.view_h);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !key.pressed {
            return;
        }

        // Editing a focused input field.
        if let Some(i) = self.focused_input {
            match key.character {
                b'\n' | b'\r' => self.submit_input(i),
                8 => {
                    self.input_text[i].pop();
                    self.needs_redraw = true;
                }
                ch if ch >= 32 && ch < 127 => {
                    self.input_text[i].push(ch as char);
                    self.needs_redraw = true;
                }
                _ => {}
            }
            return;
        }

        // Otherwise keystrokes edit the address bar.
        match key.character {
            b'\n' | b'\r' => self.load_current_url(),
            8 => {
                self.url.pop();
                self.needs_redraw = true;
            }
            0 => match key.scancode {
                0x48 => self.scroll_by(-32),
                0x50 => self.scroll_by(32),
                0x49 => self.scroll_by(-160),
                0x51 => self.scroll_by(160),
                _ => {}
            },
            ch if ch >= 32 && ch < 127 => {
                self.url.push(ch as char);
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let new = self.scroll as i32 + delta;
        self.scroll = new.max(0) as u32;
        self.clamp_scroll();
        self.needs_redraw = true;
    }

    fn handle_mouse(&mut self, ev: MouseEvent, width: u32) {
        match ev {
            MouseEvent::Scroll { delta, .. } => {
                // Wheel up = positive delta on most mice; scroll content up.
                self.scroll_by(-(delta as i32) * 48);
            }
            MouseEvent::ButtonDown {
                button: MouseButton::Left,
                x,
                y,
            } => {
                // Toolbar: address bar / Go button.
                let (ix, iw, gx, gw) = addr_bar_geom(width);
                if y >= ADDR_Y as i32 && y < (ADDR_Y + ADDR_H) as i32 {
                    if x >= gx as i32 && x < (gx + gw) as i32 {
                        self.focused_input = None;
                        self.load_current_url();
                        return;
                    }
                    if x >= ix as i32 && x < (ix + iw) as i32 {
                        // Focus the address bar (default keyboard target).
                        self.focused_input = None;
                        self.needs_redraw = true;
                        return;
                    }
                }

                // Content: links first, then inputs.
                for h in &self.link_hits {
                    if h.contains(x, y) {
                        let idx = h.idx;
                        if idx < self.links.len() {
                            let href = self.links[idx].clone();
                            self.navigate_to(&href);
                        }
                        return;
                    }
                }
                for h in &self.input_hits {
                    if h.contains(x, y) {
                        if self.inputs.get(h.idx).map(|m| m.kind) == Some(InputKind::Submit) {
                            // Submit button: submit the first text/search field.
                            let target = self
                                .inputs
                                .iter()
                                .position(|m| m.kind != InputKind::Submit)
                                .unwrap_or(h.idx);
                            self.submit_input(target);
                        } else {
                            self.focused_input = Some(h.idx);
                            self.needs_redraw = true;
                        }
                        return;
                    }
                }
                // Clicked empty content area: drop input focus.
                if self.focused_input.is_some() {
                    self.focused_input = None;
                    self.needs_redraw = true;
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, surface: &mut libgui::surface::Surface) {
        let width = surface.width();
        let height = surface.height();
        let bg = Color::rgb(248, 249, 252);
        let chrome = Color::rgb(34, 40, 49);
        let chrome_line = Color::rgb(74, 86, 104);
        let text = Color::rgb(28, 34, 43);
        let muted = Color::rgb(94, 105, 120);
        let accent = Color::rgb(38, 132, 255);

        surface.clear(bg);
        surface.fill_rect(0, 0, width, TOOLBAR_H, chrome);
        surface.draw_hline(0, TOOLBAR_H.saturating_sub(1), width, chrome_line);
        surface.draw_string(10, 8, "Atom", Color::rgb(150, 220, 255), chrome);

        let (input_x, input_w, go_x, go_w) = addr_bar_geom(width);
        let field_bg = Color::rgb(246, 248, 250);
        surface.fill_rect(input_x, ADDR_Y, input_w, ADDR_H, field_bg);
        surface.draw_rect(
            input_x,
            ADDR_Y,
            input_w,
            ADDR_H,
            if self.focused_input.is_none() && self.focused {
                accent
            } else {
                chrome_line
            },
        );
        surface.draw_string(
            input_x + 6,
            ADDR_Y + 8,
            &truncate_for_width(&self.url, input_w.saturating_sub(12)),
            text,
            field_bg,
        );
        surface.fill_rect(go_x, ADDR_Y, go_w, ADDR_H, accent);
        surface.draw_string(go_x + 14, ADDR_Y + 8, "Go", Color::WHITE, accent);

        // Title line under the toolbar.
        surface.draw_string(
            PADDING,
            TOOLBAR_H + 8,
            &truncate_for_width(&self.title, width.saturating_sub(PADDING * 2)),
            accent,
            bg,
        );

        let clip_top = (TOOLBAR_H + 30) as i32;
        let clip_bottom = height.saturating_sub(STATUS_H) as i32;
        let x0 = PADDING;
        let content_w = width.saturating_sub(PADDING * 2);

        let mut link_hits: Vec<Hit> = Vec::new();
        let mut input_hits: Vec<Hit> = Vec::new();

        let mut y = clip_top - self.scroll as i32;
        for block in &self.blocks {
            match block {
                Block::Text { kind, runs } => {
                    y = draw_text_block(
                        surface,
                        *kind,
                        runs,
                        x0,
                        content_w,
                        y,
                        clip_top,
                        clip_bottom,
                        bg,
                        &mut link_hits,
                    );
                }
                Block::Rule => {
                    if y >= clip_top && y <= clip_bottom {
                        surface.draw_hline(x0, y as u32 + 8, content_w, Color::rgb(210, 218, 230));
                    }
                    y += 24;
                }
                Block::Image { alt, img, .. } => {
                    y = draw_image_block(
                        surface,
                        alt,
                        img,
                        x0,
                        content_w,
                        y,
                        clip_top,
                        clip_bottom,
                    );
                }
                Block::Input { idx } => {
                    let meta = &self.inputs[*idx];
                    let value = &self.input_text[*idx];
                    let focused = self.focused_input == Some(*idx);
                    y = draw_input_block(
                        surface,
                        meta,
                        value,
                        focused,
                        *idx,
                        x0,
                        content_w,
                        y,
                        clip_top,
                        clip_bottom,
                        &mut input_hits,
                    );
                }
            }
        }

        self.content_height = (y + self.scroll as i32 - clip_top).max(0) as u32;
        self.view_h = (clip_bottom - clip_top).max(1) as u32;

        // Status bar.
        let status_y = height.saturating_sub(STATUS_H);
        surface.fill_rect(0, status_y, width, STATUS_H, Color::rgb(235, 239, 245));
        surface.draw_hline(0, status_y, width, Color::rgb(210, 218, 230));
        let status = format!("{}  |  {} links", self.status, self.links.len());
        surface.draw_string(
            8,
            status_y + 6,
            &truncate_for_width(&status, width.saturating_sub(16)),
            muted,
            Color::rgb(235, 239, 245),
        );

        surface.present();
        self.link_hits = link_hits;
        self.input_hits = input_hits;
        self.needs_redraw = false;
    }
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
        loop {
            let event = app.poll_event();
            match event {
                Event::None => break,
                Event::Quit => exit(0),
                Event::Key(key) => browser.handle_key(key),
                Event::Mouse(m) => browser.handle_mouse(m, surface.width()),
                Event::Window(WindowEvent::Focus) => {
                    browser.focused = true;
                    browser.needs_redraw = true;
                }
                Event::Window(WindowEvent::Unfocus) => {
                    browser.focused = false;
                    browser.needs_redraw = true;
                }
                Event::Window(WindowEvent::Resize { .. }) => browser.needs_redraw = true,
                _ => {}
            }
        }

        if browser.needs_redraw {
            browser.render(&mut surface);
        }

        yield_now();
    }
}

// ============================================================================
// Drawing helpers
// ============================================================================

fn addr_bar_geom(width: u32) -> (u32, u32, u32, u32) {
    let input_x = 62u32;
    let input_w = width.saturating_sub(input_x + 72);
    let go_w = 48u32;
    let go_x = width.saturating_sub(58);
    (input_x, input_w, go_x, go_w)
}

#[allow(clippy::too_many_arguments)]
fn draw_text_block(
    s: &mut libgui::surface::Surface,
    kind: TextKind,
    runs: &[Run],
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip_top: i32,
    clip_bottom: i32,
    bg: Color,
    link_hits: &mut Vec<Hit>,
) -> i32 {
    let text = Color::rgb(28, 34, 43);
    let accent = Color::rgb(38, 132, 255);

    if kind == TextKind::Pre {
        return draw_pre(s, runs, x0, max_w, y, clip_top, clip_bottom);
    }

    let (fg, top_pad, bottom_pad) = match kind {
        TextKind::H1 => (Color::rgb(17, 24, 39), 12, 8),
        TextKind::H2 => (accent, 10, 6),
        TextKind::H3 => (Color::rgb(83, 96, 116), 8, 4),
        TextKind::ListItem => (text, 4, 4),
        _ => (text, 6, 6),
    };
    y += top_pad;

    let (indent, eff_w) = if kind == TextKind::ListItem {
        // bullet
        if y >= clip_top && y <= clip_bottom && y >= 0 {
            s.draw_string(x0 + 8, y as u32, "*", accent, bg);
        }
        (24u32, max_w.saturating_sub(24))
    } else {
        (0u32, max_w)
    };

    let cols = (eff_w / CHAR_W).max(1);
    let line_x = x0 + indent;
    let mut cx: u32 = 0; // chars used on the current line (incl. inter-word spaces)

    for run in runs {
        let color = if run.link.is_some() { accent } else { fg };
        for word in run.text.split(' ') {
            if word.is_empty() {
                continue;
            }
            let wlen = word.chars().count() as u32;
            if cx != 0 && cx + 1 + wlen > cols {
                y += LINE_H as i32;
                cx = 0;
            }
            if cx != 0 {
                cx += 1; // the separating space
            }
            let px = line_x + cx * CHAR_W;
            let wpx = wlen * CHAR_W;
            let visible = y >= clip_top - CHAR_H as i32 && y <= clip_bottom && y >= 0;
            if visible {
                s.draw_string(px, y as u32, word, color, bg);
                if run.link.is_some() {
                    s.draw_hline(px, y as u32 + CHAR_H, wpx, color);
                }
            }
            if let Some(idx) = run.link {
                link_hits.push(Hit {
                    x: px as i32,
                    y: y - 1,
                    w: wpx as i32,
                    h: (CHAR_H + 3) as i32,
                    idx,
                });
            }
            cx += wlen;
        }
    }

    y + LINE_H as i32 + bottom_pad
}

fn draw_pre(
    s: &mut libgui::surface::Surface,
    runs: &[Run],
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip_top: i32,
    clip_bottom: i32,
) -> i32 {
    let fg = Color::rgb(20, 28, 40);
    let panel = Color::rgb(230, 236, 245);
    y += 8;
    let mut text = String::new();
    for run in runs {
        text.push_str(&run.text);
    }
    let line_count = text.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    let box_h = line_count * LINE_H + 12;
    if y >= clip_top - box_h as i32 && y <= clip_bottom && y >= 0 {
        s.fill_rect(x0, y as u32, max_w, box_h, panel);
        s.draw_rect(x0, y as u32, max_w, box_h, Color::rgb(203, 213, 225));
    }
    let max_cols = (max_w.saturating_sub(16) / CHAR_W).max(1) as usize;
    let mut ly = y + 6;
    for raw in text.split('\n') {
        let visible = &raw[..raw.len().min(max_cols)];
        if ly >= clip_top && ly <= clip_bottom && ly >= 0 {
            s.draw_string(x0 + 8, ly as u32, visible, fg, panel);
        }
        ly += LINE_H as i32;
    }
    y + box_h as i32 + 8
}

#[allow(clippy::too_many_arguments)]
fn draw_image_block(
    s: &mut libgui::surface::Surface,
    alt: &str,
    img: &Option<libimage::DecodedImage>,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip_top: i32,
    clip_bottom: i32,
) -> i32 {
    y += 8;
    match img {
        Some(im) if im.width > 0 && im.height > 0 => {
            let mut tw = im.width;
            let mut th = im.height;
            let maxh = 320u32;
            if tw > max_w {
                th = (th * max_w / tw).max(1);
                tw = max_w;
            }
            if th > maxh {
                tw = (tw * maxh / th).max(1);
                th = maxh;
            }
            if y + th as i32 >= clip_top && y <= clip_bottom {
                for ry in 0..th {
                    let dy = y + ry as i32;
                    if dy < clip_top || dy > clip_bottom || dy < 0 {
                        continue;
                    }
                    let sy = ry * im.height / th;
                    for rx in 0..tw {
                        let sx = rx * im.width / tw;
                        if let Some((r, g, b, a)) = im.get_pixel(sx, sy) {
                            if a >= 16 {
                                s.set_pixel(x0 + rx, dy as u32, Color::rgb(r, g, b));
                            }
                        }
                    }
                }
            }
            y + th as i32 + 10
        }
        _ => {
            let w = max_w.min(260);
            let h = 64u32;
            if y + h as i32 >= clip_top && y <= clip_bottom && y >= 0 {
                let panel = Color::rgb(238, 240, 244);
                s.fill_rect(x0, y as u32, w, h, panel);
                s.draw_rect(x0, y as u32, w, h, Color::rgb(203, 213, 225));
                let label = if alt.is_empty() {
                    String::from("[image]")
                } else {
                    format!("[image] {}", alt)
                };
                s.draw_string(
                    x0 + 8,
                    y as u32 + h / 2 - 4,
                    &truncate_for_width(&label, w.saturating_sub(16)),
                    Color::rgb(94, 105, 120),
                    panel,
                );
            }
            y + h as i32 + 10
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_input_block(
    s: &mut libgui::surface::Surface,
    meta: &InputMeta,
    value: &str,
    focused: bool,
    idx: usize,
    x0: u32,
    max_w: u32,
    mut y: i32,
    clip_top: i32,
    clip_bottom: i32,
    input_hits: &mut Vec<Hit>,
) -> i32 {
    y += 6;
    let h = 26u32;
    let w = match meta.kind {
        InputKind::Submit => 120u32.min(max_w),
        _ => max_w.min(440),
    };
    if y + h as i32 >= clip_top && y <= clip_bottom && y >= 0 {
        let accent = Color::rgb(38, 132, 255);
        match meta.kind {
            InputKind::Submit => {
                s.fill_rect(x0, y as u32, w, h, accent);
                s.draw_rect(x0, y as u32, w, h, accent);
                let label = if meta.placeholder.is_empty() {
                    "Search"
                } else {
                    &meta.placeholder
                };
                s.draw_string(x0 + 10, y as u32 + 9, label, Color::WHITE, accent);
            }
            _ => {
                let field = Color::rgb(255, 255, 255);
                s.fill_rect(x0, y as u32, w, h, field);
                s.draw_rect(
                    x0,
                    y as u32,
                    w,
                    h,
                    if focused { accent } else { Color::rgb(180, 190, 205) },
                );
                if value.is_empty() && !focused {
                    s.draw_string(
                        x0 + 8,
                        y as u32 + 9,
                        &truncate_for_width(&meta.placeholder, w.saturating_sub(16)),
                        Color::rgb(150, 160, 175),
                        field,
                    );
                } else {
                    let shown = if focused {
                        format!("{}_", value)
                    } else {
                        value.to_string()
                    };
                    s.draw_string(
                        x0 + 8,
                        y as u32 + 9,
                        &truncate_for_width(&shown, w.saturating_sub(16)),
                        Color::rgb(28, 34, 43),
                        field,
                    );
                }
            }
        }
    }
    input_hits.push(Hit {
        x: x0 as i32,
        y,
        w: w as i32,
        h: h as i32,
        idx,
    });
    y + h as i32 + 8
}

// ============================================================================
// HTTP / image fetching
// ============================================================================

fn fetch_http(url: &str) -> Result<String, String> {
    let netd = lookup_service("netd").map_err(|_| String::from("netd service not found"))?;

    let mut current_url = String::from(url);
    for _ in 0..4 {
        let (host, path, port) = split_http_url(&current_url)?;
        let response = libnet::http_get(netd, &host, &path, port)
            .map_err(|e| format!("HTTP request failed ({:?})", e))?;

        if response.status >= 300 && response.status < 400 {
            if let Some(location) = response.location {
                if location.trim().starts_with("https://") {
                    return Err(format!(
                        "This site requires HTTPS, but TLS is not implemented yet: {}",
                        location
                    ));
                }
                if let Some(next_url) = normalize_redirect_url(&current_url, &location) {
                    current_url = next_url;
                    continue;
                }
            }
        }

        if response.status == 0 && response.body.is_empty() {
            return Err(String::from("No HTTP response received"));
        }
        if response.body.is_empty() {
            return Err(format!("HTTP {} with empty body", response.status));
        }
        return Ok(String::from_utf8_lossy(&response.body).to_string());
    }
    Err(String::from("Too many HTTP redirects"))
}

/// Fetch raw bytes (used for images), following plain-HTTP redirects.
fn fetch_url_bytes(url: &str) -> Option<Vec<u8>> {
    let netd = lookup_service("netd").ok()?;
    let mut current = String::from(url);
    for _ in 0..3 {
        let (host, path, port) = split_http_url(&current).ok()?;
        let resp = libnet::http_get(netd, &host, &path, port).ok()?;
        if resp.status >= 300 && resp.status < 400 {
            let loc = resp.location?;
            if loc.trim().starts_with("https://") {
                return None;
            }
            current = normalize_redirect_url(&current, &loc)?;
            continue;
        }
        if resp.body.is_empty() {
            return None;
        }
        return Some(resp.body);
    }
    None
}

fn decode_image(bytes: &[u8]) -> Option<libimage::DecodedImage> {
    if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        PngDecoder::decode(bytes).ok()
    } else if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        JpgDecoder::decode(bytes).ok()
    } else {
        None
    }
}

fn decode_data_uri(uri: &str) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let data = &rest[comma + 1..];
    if meta.contains("base64") {
        base64_decode(data)
    } else {
        Some(percent_decode(data))
    }
}

// ============================================================================
// URL handling
// ============================================================================

fn normalize_http_url(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") {
        return Some(String::from(trimmed));
    }
    if trimmed.starts_with("https://") {
        let host_path = &trimmed["https://".len()..];
        return Some(format!("http://{}", host_path));
    }
    if trimmed == "google" || trimmed == "google.com" || trimmed == "www.google.com" {
        return Some(String::from("http://www.google.com/"));
    }
    if trimmed == "neverssl" || trimmed == "neverssl.com" {
        return Some(String::from("http://neverssl.com/"));
    }
    if trimmed == "example" || trimmed == "example.com" {
        return Some(String::from("http://example.com/"));
    }
    if trimmed.bytes().any(|b| b == b'.') {
        return Some(format!("http://{}", trimmed));
    }
    None
}

fn split_http_url(url: &str) -> Result<(String, String, u16), String> {
    if !url.starts_with("http://") {
        return Err(String::from("Only plain HTTP is supported"));
    }
    let rest = &url["http://".len()..];
    let slash = rest.find('/').unwrap_or(rest.len());
    let host_port = &rest[..slash];
    let path = if slash < rest.len() { &rest[slash..] } else { "/" };
    let colon = host_port.find(':');
    let (host, port) = match colon {
        Some(pos) => (
            &host_port[..pos],
            parse_port(&host_port[pos + 1..]).unwrap_or(80),
        ),
        None => (host_port, 80),
    };
    if host.is_empty() {
        return Err(String::from("Invalid HTTP host"));
    }
    Ok((String::from(host), String::from(path), port))
}

fn normalize_redirect_url(base_url: &str, location: &str) -> Option<String> {
    if let Some(url) = normalize_http_url(location) {
        return Some(url);
    }
    if location.starts_with('/') {
        let (host, _, port) = split_http_url(base_url).ok()?;
        if port == 80 {
            return Some(format!("http://{}{}", host, location));
        }
        return Some(format!("http://{}:{}{}", host, port, location));
    }
    None
}

/// Resolve a (possibly relative) URL against the current page URL.
/// Returns None for unsupported schemes (https, data handled separately).
fn resolve_url(base_url: &str, rel: &str) -> Option<String> {
    let rel = rel.trim();
    if rel.starts_with("http://") {
        return Some(String::from(rel));
    }
    if rel.starts_with("https://") {
        return None;
    }
    if let Some(stripped) = rel.strip_prefix("//") {
        return Some(format!("http://{}", stripped));
    }
    let (host, base_path, port) = split_http_url(base_url).ok()?;
    let hostport = if port == 80 {
        host
    } else {
        format!("{}:{}", host, port)
    };
    if rel.starts_with('/') {
        return Some(format!("http://{}{}", hostport, rel));
    }
    // Relative to the base path's directory.
    let dir_end = base_path.rfind('/').map(|i| i + 1).unwrap_or(0);
    let dir = &base_path[..dir_end];
    Some(format!("http://{}{}{}", hostport, dir, rel))
}

fn parse_port(s: &str) -> Option<u16> {
    let mut value: u32 = 0;
    for b in s.bytes() {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (b - b'0') as u32;
        if value > u16::MAX as u32 {
            return None;
        }
    }
    Some(value as u16)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0xF));
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex_digit(v: u8) -> char {
    if v < 10 {
        (b'0' + v) as char
    } else {
        (b'A' + v - 10) as char
    }
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn v(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        if c == b'\n' || c == b'\r' || c == b' ' || c == b'\t' {
            continue;
        }
        let d = v(c)? as u32;
        buf = (buf << 6) | d;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

// ============================================================================
// HTML parsing
// ============================================================================

struct ParseResult {
    title: String,
    blocks: Vec<Block>,
    links: Vec<String>,
    inputs: Vec<InputMeta>,
}

fn parse_html(html: &str) -> ParseResult {
    let mut parser = HtmlParser::new(html);
    parser.parse();
    ParseResult {
        title: parser.title,
        blocks: parser.blocks,
        links: parser.links,
        inputs: parser.inputs,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IgnoredTag {
    None,
    Script,
    Style,
}

struct HtmlParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    blocks: Vec<Block>,
    runs: Vec<Run>,
    cur: String,
    cur_kind: TextKind,
    cur_link: Option<usize>,
    links: Vec<String>,
    inputs: Vec<InputMeta>,
    form_action: String,
    title: String,
    in_title: bool,
    in_pre: bool,
    ignored_tag: IgnoredTag,
    last_was_space: bool,
}

impl<'a> HtmlParser<'a> {
    fn new(html: &'a str) -> Self {
        Self {
            bytes: html.as_bytes(),
            pos: 0,
            blocks: Vec::new(),
            runs: Vec::new(),
            cur: String::new(),
            cur_kind: TextKind::Paragraph,
            cur_link: None,
            links: Vec::new(),
            inputs: Vec::new(),
            form_action: String::new(),
            title: String::new(),
            in_title: false,
            in_pre: false,
            ignored_tag: IgnoredTag::None,
            last_was_space: true,
        }
    }

    fn parse(&mut self) {
        while self.pos < self.bytes.len() {
            if self.ignored_tag != IgnoredTag::None {
                self.skip_ignored_tag();
                continue;
            }
            match self.bytes[self.pos] {
                b'<' => self.parse_tag(),
                b'&' => {
                    let ch = self.parse_entity();
                    self.push_char(ch);
                }
                b => {
                    self.pos += 1;
                    self.push_char(b as char);
                }
            }
        }
        self.flush_block();
        if self.blocks.is_empty() {
            self.blocks.push(Block::Text {
                kind: TextKind::Paragraph,
                runs: alloc::vec![Run {
                    text: String::from("(empty document)"),
                    link: None,
                }],
            });
        }
    }

    fn parse_tag(&mut self) {
        let start = self.pos + 1;
        let Some(end) = self.bytes[start..].iter().position(|&b| b == b'>') else {
            self.pos = self.bytes.len();
            return;
        };
        let tag_bytes = &self.bytes[start..start + end];
        self.pos = start + end + 1;

        if tag_bytes.starts_with(b"!--") {
            return;
        }

        let mut idx = 0;
        while idx < tag_bytes.len() && tag_bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let closing = idx < tag_bytes.len() && tag_bytes[idx] == b'/';
        if closing {
            idx += 1;
        }
        while idx < tag_bytes.len() && tag_bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        let name_start = idx;
        while idx < tag_bytes.len()
            && (tag_bytes[idx].is_ascii_alphanumeric() || tag_bytes[idx] == b'-')
        {
            idx += 1;
        }
        let name = ascii_lower(&tag_bytes[name_start..idx]);
        if name.is_empty() {
            return;
        }
        if closing {
            self.close_tag(&name);
        } else {
            self.open_tag(&name, &tag_bytes[idx..]);
        }
    }

    fn open_tag(&mut self, name: &str, attrs: &[u8]) {
        match name {
            "script" => self.ignored_tag = IgnoredTag::Script,
            "style" => self.ignored_tag = IgnoredTag::Style,
            "title" => {
                self.flush_block();
                self.in_title = true;
                self.cur.clear();
                self.last_was_space = true;
            }
            "h1" => self.start_block(TextKind::H1),
            "h2" => self.start_block(TextKind::H2),
            "h3" | "h4" | "h5" | "h6" => self.start_block(TextKind::H3),
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "center"
            | "td" | "th" | "tr" => self.start_block(TextKind::Paragraph),
            "li" => self.start_block(TextKind::ListItem),
            "pre" => {
                self.start_block(TextKind::Pre);
                self.in_pre = true;
            }
            "br" => self.flush_block(),
            "hr" => {
                self.flush_block();
                self.blocks.push(Block::Rule);
            }
            "a" => {
                self.flush_run();
                let href = find_attr(attrs, b"href").unwrap_or_default();
                let idx = self.links.len();
                self.links.push(href);
                self.cur_link = Some(idx);
            }
            "form" => {
                self.form_action = find_attr(attrs, b"action").unwrap_or_default();
            }
            "img" => {
                self.flush_block();
                let src = find_attr(attrs, b"src").unwrap_or_default();
                let alt = find_attr(attrs, b"alt").unwrap_or_default();
                self.blocks.push(Block::Image {
                    alt,
                    img: None,
                    src,
                });
            }
            "input" => {
                let ty = find_attr(attrs, b"type").unwrap_or_default();
                let kind = match ty.as_str() {
                    "submit" | "button" => InputKind::Submit,
                    "search" => InputKind::Search,
                    "hidden" => return,
                    _ => InputKind::Text,
                };
                let placeholder = find_attr(attrs, b"placeholder")
                    .or_else(|| find_attr(attrs, b"value"))
                    .unwrap_or_default();
                let qname = find_attr(attrs, b"name").unwrap_or_default();
                self.flush_block();
                let idx = self.inputs.len();
                self.inputs.push(InputMeta {
                    kind,
                    placeholder,
                    name: qname,
                    action: self.form_action.clone(),
                });
                self.blocks.push(Block::Input { idx });
            }
            _ => {}
        }
    }

    fn close_tag(&mut self, name: &str) {
        match name {
            "title" => {
                self.title = self.cur.trim().to_string();
                self.cur.clear();
                self.in_title = false;
                self.last_was_space = true;
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "section" | "article"
            | "li" | "center" | "td" | "th" | "tr" => {
                self.flush_block();
                self.cur_kind = TextKind::Paragraph;
            }
            "pre" => {
                self.flush_block();
                self.in_pre = false;
                self.cur_kind = TextKind::Paragraph;
            }
            "form" => self.form_action.clear(),
            "a" => {
                self.flush_run();
                self.cur_link = None;
            }
            _ => {}
        }
    }

    fn start_block(&mut self, kind: TextKind) {
        self.flush_block();
        self.cur_kind = kind;
        self.last_was_space = true;
    }

    fn push_char(&mut self, ch: char) {
        if self.ignored_tag != IgnoredTag::None {
            return;
        }
        if self.in_title {
            self.cur.push(ch);
            return;
        }
        if self.in_pre {
            if ch == '\r' {
                return;
            }
            self.cur.push(ch);
            return;
        }
        if ch.is_whitespace() {
            if !self.last_was_space && !self.cur.is_empty() {
                self.cur.push(' ');
                self.last_was_space = true;
            }
        } else {
            self.cur.push(ch);
            self.last_was_space = false;
        }
    }

    fn parse_entity(&mut self) -> char {
        let start = self.pos + 1;
        let max_end = (start + 12).min(self.bytes.len());
        for end in start..max_end {
            if self.bytes[end] == b';' {
                let entity = &self.bytes[start..end];
                self.pos = end + 1;
                return match entity {
                    b"amp" => '&',
                    b"lt" => '<',
                    b"gt" => '>',
                    b"quot" => '"',
                    b"apos" => '\'',
                    b"nbsp" => ' ',
                    _ => ' ',
                };
            }
        }
        self.pos += 1;
        '&'
    }

    fn flush_run(&mut self) {
        if !self.cur.is_empty() {
            let text = core::mem::take(&mut self.cur);
            self.runs.push(Run {
                text,
                link: self.cur_link,
            });
        }
    }

    fn flush_block(&mut self) {
        self.flush_run();
        if self.in_pre {
            // Preserve pre content verbatim (already in runs).
        }
        let has_text = self
            .runs
            .iter()
            .any(|r| r.text.chars().any(|c| !c.is_whitespace()));
        if has_text {
            let runs = core::mem::take(&mut self.runs);
            self.blocks.push(Block::Text {
                kind: self.cur_kind,
                runs,
            });
        } else {
            self.runs.clear();
        }
        self.cur_kind = TextKind::Paragraph;
        self.last_was_space = true;
    }

    fn skip_ignored_tag(&mut self) {
        let needle = match self.ignored_tag {
            IgnoredTag::Script => b"</script".as_slice(),
            IgnoredTag::Style => b"</style".as_slice(),
            IgnoredTag::None => return,
        };
        let Some(close_rel) = find_ignore_ascii_case(&self.bytes[self.pos..], needle) else {
            self.pos = self.bytes.len();
            return;
        };
        self.pos += close_rel;
        if let Some(end_rel) = self.bytes[self.pos..].iter().position(|&b| b == b'>') {
            self.pos += end_rel + 1;
        } else {
            self.pos = self.bytes.len();
        }
        self.ignored_tag = IgnoredTag::None;
    }
}

fn ascii_lower(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        out.push((b as char).to_ascii_lowercase());
    }
    out
}

fn find_attr(attrs: &[u8], name: &[u8]) -> Option<String> {
    let mut i = 0;
    while i < attrs.len() {
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < attrs.len()
            && (attrs[i].is_ascii_alphanumeric() || attrs[i] == b'-' || attrs[i] == b'_')
        {
            i += 1;
        }
        let attr_name = &attrs[name_start..i];
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < attrs.len() && attrs[i] == b'=' {
            i += 1;
            while i < attrs.len() && attrs[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= attrs.len() {
                return None;
            }
            let value = if attrs[i] == b'\'' || attrs[i] == b'"' {
                let quote = attrs[i];
                i += 1;
                let start = i;
                while i < attrs.len() && attrs[i] != quote {
                    i += 1;
                }
                let v = &attrs[start..i];
                if i < attrs.len() {
                    i += 1;
                }
                v
            } else {
                let start = i;
                while i < attrs.len() && !attrs[i].is_ascii_whitespace() {
                    i += 1;
                }
                &attrs[start..i]
            };
            if eq_ignore_ascii_case(attr_name, name) {
                return core::str::from_utf8(value).ok().map(String::from);
            }
        } else if attr_name.is_empty() {
            i += 1;
        }
    }
    None
}

fn find_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| eq_ignore_ascii_case(w, needle))
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|(&x, &y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn truncate_for_width(text: &str, width: u32) -> String {
    let max_chars = (width / CHAR_W) as usize;
    if text.len() <= max_chars {
        return String::from(text);
    }
    if max_chars <= 3 {
        return String::from("...");
    }
    let mut out = String::new();
    for b in text.bytes().take(max_chars - 3) {
        out.push(b as char);
    }
    out.push_str("...");
    out
}

fn escape_text(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

const ABOUT_HOME: &str = r#"
<!doctype html>
<html>
<head><title>Atom Browser</title></head>
<body>
  <h1>Atom Browser</h1>
  <p>A small userspace browser for Atom OS. Type a URL above and press Enter, or click a link below.</p>
  <h2>Check your connection</h2>
  <p>Try <a href="http://neverssl.com/">neverssl</a> or <a href="http://example.com/">example</a> to load a plain-HTTP page through netd.</p>
  <p>Note: HTTPS sites (such as google.com) are not supported yet because Atom has no TLS stack.</p>
  <p>Open <a href="about:html">about:html</a> for a richer render test (links, lists, images, search box).</p>
</body>
</html>
"#;

const ABOUT_HTML: &str = r#"
<!doctype html>
<html>
<head><title>HTML Demo</title></head>
<body>
  <h1>Render Test</h1>
  <p>This page exercises whitespace collapsing, entity decoding like &amp; and &lt;, and wrapping across the content area.</p>
  <h2>Links</h2>
  <p>Visit <a href="http://example.com/">Example Domain</a> or <a href="http://neverssl.com/">NeverSSL</a>.</p>
  <hr>
  <h2>Search box</h2>
  <form action="http://example.com/search">
    <input type="search" name="q" placeholder="Type a query and press Enter">
    <input type="submit" value="Search">
  </form>
  <h2>List</h2>
  <ul>
    <li>First item with enough words to wrap onto a second line.</li>
    <li>Second item with an entity: Atom &amp; HTML.</li>
  </ul>
</body>
</html>
"#;
