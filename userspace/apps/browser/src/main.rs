//! Atom Browser - basic HTML renderer for userspace.
//!
//! This app intentionally implements a small, deterministic HTML subset:
//! headings, paragraphs, lists, links, preformatted text, line breaks, and
//! horizontal rules. It can render bundled `about:` pages without networking
//! and can fetch `http://` pages through `netd` when the service is available.

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
use libgui::event::{Event, KeyEvent, WindowEvent};
use libipc::protocol::lookup_service;

const HEAP_SIZE: usize = 2 * 1024 * 1024;

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
const TOOLBAR_H: u32 = 42;
const STATUS_H: u32 = 20;
const PADDING: u32 = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    H1,
    H2,
    H3,
    Paragraph,
    ListItem,
    Pre,
    Rule,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IgnoredTag {
    None,
    Script,
    Style,
}

struct Block {
    kind: BlockKind,
    text: String,
}

impl Block {
    fn new(kind: BlockKind, text: String) -> Self {
        Self { kind, text }
    }
}

struct Browser {
    url: String,
    status: String,
    title: String,
    blocks: Vec<Block>,
    scroll: u32,
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
            scroll: 0,
            focused: false,
            needs_redraw: true,
        };
        browser.load_current_url();
        browser
    }

    fn load_current_url(&mut self) {
        self.status = String::from("Loading...");
        self.scroll = 0;

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

        let (title, blocks) = parse_html(&html);
        if !title.is_empty() {
            self.title = title;
        }
        self.blocks = blocks;
        if self.status == "Loading..." {
            self.status = format!("Loaded {} blocks", self.blocks.len());
        }
        self.needs_redraw = true;
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !key.pressed {
            return;
        }

        match key.character {
            b'\n' | b'\r' => self.load_current_url(),
            8 => {
                self.url.pop();
                self.needs_redraw = true;
            }
            0 => match key.scancode {
                0x48 => {
                    self.scroll = self.scroll.saturating_sub(32);
                    self.needs_redraw = true;
                }
                0x50 => {
                    self.scroll = self.scroll.saturating_add(32);
                    self.needs_redraw = true;
                }
                0x49 => {
                    self.scroll = self.scroll.saturating_sub(160);
                    self.needs_redraw = true;
                }
                0x51 => {
                    self.scroll = self.scroll.saturating_add(160);
                    self.needs_redraw = true;
                }
                _ => {}
            },
            ch if ch >= 32 && ch < 127 => {
                self.url.push(ch as char);
                self.needs_redraw = true;
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
        let page_y = TOOLBAR_H;
        let page_h = height.saturating_sub(TOOLBAR_H + STATUS_H);

        surface.clear(bg);
        surface.fill_rect(0, 0, width, TOOLBAR_H, chrome);
        surface.draw_hline(0, TOOLBAR_H.saturating_sub(1), width, chrome_line);

        surface.draw_string(10, 8, "Atom", Color::rgb(150, 220, 255), chrome);
        let input_x = 62;
        let input_y = 8;
        let input_w = width.saturating_sub(input_x + 72);
        surface.fill_rect(input_x, input_y, input_w, 24, Color::rgb(246, 248, 250));
        surface.draw_rect(
            input_x,
            input_y,
            input_w,
            24,
            if self.focused { accent } else { chrome_line },
        );
        surface.draw_string(
            input_x + 6,
            input_y + 8,
            &truncate_for_width(&self.url, input_w.saturating_sub(12)),
            text,
            Color::rgb(246, 248, 250),
        );
        surface.fill_rect(width.saturating_sub(58), input_y, 48, 24, accent);
        surface.draw_string(
            width.saturating_sub(46),
            input_y + 8,
            "Go",
            Color::WHITE,
            accent,
        );

        surface.draw_string(
            PADDING,
            TOOLBAR_H + 8,
            &truncate_for_width(&self.title, width.saturating_sub(PADDING * 2)),
            accent,
            bg,
        );

        let clip_top = page_y + 30;
        let clip_bottom = TOOLBAR_H + page_h;
        let mut y = clip_top as i32 - self.scroll as i32;
        let content_w = width.saturating_sub(PADDING * 2);

        for block in &self.blocks {
            if y > clip_bottom as i32 {
                break;
            }

            let block_h = estimated_block_height(block, content_w);
            if y + block_h as i32 >= clip_top as i32 {
                y = draw_block(surface, block, PADDING, y, content_w, bg);
            } else {
                y += block_h as i32;
            }
        }

        let status_y = height.saturating_sub(STATUS_H);
        surface.fill_rect(0, status_y, width, STATUS_H, Color::rgb(235, 239, 245));
        surface.draw_hline(0, status_y, width, Color::rgb(210, 218, 230));
        let status = format!("{}  |  scroll {}", self.status, self.scroll);
        surface.draw_string(
            8,
            status_y + 6,
            &truncate_for_width(&status, width.saturating_sub(16)),
            muted,
            Color::rgb(235, 239, 245),
        );

        surface.present();
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
    // Plain-HTTP test pages that never force a redirect to HTTPS — useful as a
    // "can the browser reach the internet?" check, since Atom has no TLS yet.
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

fn fetch_http(url: &str) -> Result<String, String> {
    let netd = lookup_service("netd").map_err(|_| String::from("netd service not found"))?;

    let mut current_url = String::from(url);
    for _ in 0..4 {
        let (host, path, port) = split_http_url(&current_url)?;
        let response = libnet::http_get(netd, &host, &path, port)
            .map_err(|_| String::from("HTTP request failed"))?;

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

        log(&format!(
            "browser: HTTP {} {} bytes from {}",
            response.status,
            response.body.len(),
            current_url
        ));

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

fn split_http_url(url: &str) -> Result<(String, String, u16), String> {
    if !url.starts_with("http://") {
        return Err(String::from("Only plain HTTP is supported"));
    }

    let rest = &url["http://".len()..];
    let slash = rest.find('/').unwrap_or(rest.len());
    let host_port = &rest[..slash];
    let path = if slash < rest.len() {
        &rest[slash..]
    } else {
        "/"
    };
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

fn parse_html(html: &str) -> (String, Vec<Block>) {
    let mut parser = HtmlParser::new(html);
    parser.parse();
    (parser.title, parser.blocks)
}

struct HtmlParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    blocks: Vec<Block>,
    current: String,
    current_kind: BlockKind,
    title: String,
    in_title: bool,
    in_pre: bool,
    ignored_tag: IgnoredTag,
    last_was_space: bool,
    link_href: Option<String>,
}

impl<'a> HtmlParser<'a> {
    fn new(html: &'a str) -> Self {
        Self {
            bytes: html.as_bytes(),
            pos: 0,
            blocks: Vec::new(),
            current: String::new(),
            current_kind: BlockKind::Paragraph,
            title: String::new(),
            in_title: false,
            in_pre: false,
            ignored_tag: IgnoredTag::None,
            last_was_space: true,
            link_href: None,
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
        self.flush();
        if self.blocks.is_empty() {
            self.blocks.push(Block::new(
                BlockKind::Paragraph,
                String::from("(empty document)"),
            ));
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
                self.flush();
                self.in_title = true;
                self.current.clear();
                self.last_was_space = true;
            }
            "h1" => self.start_block(BlockKind::H1),
            "h2" => self.start_block(BlockKind::H2),
            "h3" => self.start_block(BlockKind::H3),
            "p" | "div" | "section" | "article" | "main" | "header" | "footer" | "center"
            | "td" | "th" => self.start_block(BlockKind::Paragraph),
            "li" => self.start_block(BlockKind::ListItem),
            "pre" => {
                self.start_block(BlockKind::Pre);
                self.in_pre = true;
            }
            "br" => self.flush(),
            "hr" => {
                self.flush();
                self.blocks.push(Block::new(BlockKind::Rule, String::new()));
            }
            "a" => self.link_href = find_href(attrs),
            _ => {}
        }
    }

    fn close_tag(&mut self, name: &str) {
        match name {
            "title" => {
                self.title = self.current.trim().to_string();
                self.current.clear();
                self.in_title = false;
                self.last_was_space = true;
            }
            "h1" | "h2" | "h3" | "p" | "div" | "section" | "article" | "li" | "center" | "td"
            | "th" => {
                self.flush();
                self.current_kind = BlockKind::Paragraph;
            }
            "pre" => {
                self.flush();
                self.in_pre = false;
                self.current_kind = BlockKind::Paragraph;
            }
            "a" => {
                if let Some(href) = self.link_href.take() {
                    if !href.is_empty() {
                        self.current.push_str(" [");
                        self.current.push_str(&href);
                        self.current.push(']');
                        self.last_was_space = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn start_block(&mut self, kind: BlockKind) {
        self.flush();
        self.current_kind = kind;
        self.last_was_space = true;
    }

    fn push_char(&mut self, ch: char) {
        if self.ignored_tag != IgnoredTag::None {
            return;
        }

        if self.in_title {
            self.current.push(ch);
            return;
        }

        if self.in_pre {
            if ch == '\r' {
                return;
            }
            self.current.push(ch);
            return;
        }

        if ch.is_whitespace() {
            if !self.last_was_space && !self.current.is_empty() {
                self.current.push(' ');
                self.last_was_space = true;
            }
        } else {
            self.current.push(ch);
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

    fn flush(&mut self) {
        let text = if self.in_pre {
            self.current.trim_matches('\n').to_string()
        } else {
            self.current.trim().to_string()
        };

        if !text.is_empty() {
            self.blocks.push(Block::new(self.current_kind, text));
        }

        self.current.clear();
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

fn find_href(attrs: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 4 <= attrs.len() {
        while i < attrs.len() && attrs[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 4 <= attrs.len() && eq_ignore_ascii_case(&attrs[i..i + 4], b"href") {
            i += 4;
            while i < attrs.len() && attrs[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= attrs.len() || attrs[i] != b'=' {
                continue;
            }
            i += 1;
            while i < attrs.len() && attrs[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= attrs.len() {
                return None;
            }
            let quote = attrs[i];
            if quote == b'\'' || quote == b'"' {
                i += 1;
                let start = i;
                while i < attrs.len() && attrs[i] != quote {
                    i += 1;
                }
                return core::str::from_utf8(&attrs[start..i])
                    .ok()
                    .map(String::from);
            }
            let start = i;
            while i < attrs.len() && !attrs[i].is_ascii_whitespace() {
                i += 1;
            }
            return core::str::from_utf8(&attrs[start..i])
                .ok()
                .map(String::from);
        }
        i += 1;
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

fn draw_block(
    surface: &mut libgui::surface::Surface,
    block: &Block,
    x: u32,
    y: i32,
    max_w: u32,
    bg: Color,
) -> i32 {
    let text = Color::rgb(28, 34, 43);
    let muted = Color::rgb(83, 96, 116);
    let accent = Color::rgb(38, 132, 255);

    match block.kind {
        BlockKind::H1 => {
            draw_wrapped(
                surface,
                x,
                y + 12,
                max_w,
                &block.text,
                Color::rgb(17, 24, 39),
                bg,
                0,
            ) + 16
        }
        BlockKind::H2 => draw_wrapped(surface, x, y + 10, max_w, &block.text, accent, bg, 0) + 12,
        BlockKind::H3 => draw_wrapped(surface, x, y + 8, max_w, &block.text, muted, bg, 0) + 10,
        BlockKind::Paragraph => {
            draw_wrapped(surface, x, y + 8, max_w, &block.text, text, bg, 0) + 8
        }
        BlockKind::ListItem => {
            let bullet_x = x + 8;
            if y >= 0 {
                surface.draw_string(bullet_x, y as u32 + 8, "*", accent, bg);
            }
            draw_wrapped(
                surface,
                x + 24,
                y + 8,
                max_w.saturating_sub(24),
                &block.text,
                text,
                bg,
                0,
            ) + 6
        }
        BlockKind::Pre => {
            let lines = draw_pre(
                surface,
                x,
                y + 8,
                max_w,
                &block.text,
                Color::rgb(20, 28, 40),
                Color::rgb(230, 236, 245),
            );
            lines + 12
        }
        BlockKind::Rule => {
            if y >= 0 {
                surface.draw_hline(x, y as u32 + 14, max_w, Color::rgb(210, 218, 230));
            }
            y + 28
        }
    }
}

fn estimated_block_height(block: &Block, max_w: u32) -> u32 {
    let chars_per_line = ((max_w / CHAR_W).max(1)) as usize;
    let len = block.text.len().max(1);
    match block.kind {
        BlockKind::Rule => 28,
        BlockKind::Pre => {
            let lines = block.text.bytes().filter(|&b| b == b'\n').count() + 1;
            (lines as u32 * (CHAR_H + 4)) + 20
        }
        BlockKind::H1
        | BlockKind::H2
        | BlockKind::H3
        | BlockKind::Paragraph
        | BlockKind::ListItem => {
            let lines = (len + chars_per_line - 1) / chars_per_line;
            (lines as u32 * (CHAR_H + 4)) + 28
        }
    }
}

fn draw_wrapped(
    surface: &mut libgui::surface::Surface,
    x: u32,
    mut y: i32,
    max_w: u32,
    text: &str,
    fg: Color,
    bg: Color,
    indent: u32,
) -> i32 {
    let max_cols = ((max_w / CHAR_W).max(1)) as usize;
    let mut line = String::new();
    let mut line_cols = 0usize;

    for word in text.split(' ') {
        let word_cols = word.len();
        let extra = if line.is_empty() { 0 } else { 1 };

        if line_cols + extra + word_cols > max_cols && !line.is_empty() {
            if y >= 0 {
                surface.draw_string(x + indent, y as u32, &line, fg, bg);
            }
            y += (CHAR_H + 4) as i32;
            line.clear();
            line_cols = 0;
        }

        if !line.is_empty() {
            line.push(' ');
            line_cols += 1;
        }
        line.push_str(word);
        line_cols += word_cols;
    }

    if !line.is_empty() {
        if y >= 0 {
            surface.draw_string(x + indent, y as u32, &line, fg, bg);
        }
        y += (CHAR_H + 4) as i32;
    }

    y
}

fn draw_pre(
    surface: &mut libgui::surface::Surface,
    x: u32,
    mut y: i32,
    max_w: u32,
    text: &str,
    fg: Color,
    bg: Color,
) -> i32 {
    let max_cols = ((max_w.saturating_sub(16) / CHAR_W).max(1)) as usize;
    let start_y = y.max(0) as u32;
    let line_count = text.bytes().filter(|&b| b == b'\n').count() as u32 + 1;
    surface.fill_rect(x, start_y, max_w, line_count * (CHAR_H + 4) + 12, bg);
    surface.draw_rect(
        x,
        start_y,
        max_w,
        line_count * (CHAR_H + 4) + 12,
        Color::rgb(203, 213, 225),
    );

    for raw in text.split('\n') {
        let mut visible = raw;
        if visible.len() > max_cols {
            visible = &visible[..max_cols];
        }
        if y >= 0 {
            surface.draw_string(x + 8, y as u32 + 6, visible, fg, bg);
        }
        y += (CHAR_H + 4) as i32;
    }

    y
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
  <p>A small userspace browser for Atom OS. Type a URL above and press Enter.</p>
  <h2>Supported HTML</h2>
  <ul>
    <li>Headings: h1, h2, h3</li>
    <li>Text blocks: p, div, section, article</li>
    <li>Lists: ul, ol, li</li>
    <li>Links: a href</li>
    <li>Preformatted text: pre</li>
    <li>Line breaks and rules: br, hr</li>
  </ul>
  <h2>Check your connection</h2>
  <p>Type <a href="http://neverssl.com/">neverssl</a> or <a href="http://example.com/">example</a> and press Enter to load a plain-HTTP page through netd.</p>
  <p>Note: HTTPS sites (such as google.com) are not supported yet because Atom has no TLS stack, so they cannot be displayed.</p>
  <p>Try about:html for a richer render test.</p>
</body>
</html>
"#;

const ABOUT_HTML: &str = r#"
<!doctype html>
<html>
<head><title>HTML Demo</title></head>
<body>
  <h1>Basic HTML Render Test</h1>
  <p>This page exercises text extraction, whitespace collapsing, entity decoding like &amp; and &lt;, and wrapping across the content area.</p>
  <h2>Links</h2>
  <p>Visit <a href="http://example.com/">Example Domain</a> to test plain HTTP loading.</p>
  <hr>
  <h2>List Rendering</h2>
  <ul>
    <li>First item with enough words to wrap onto a second line.</li>
    <li>Second item with an entity: Atom &amp; HTML.</li>
    <li>Third item.</li>
  </ul>
  <h2>Preformatted Text</h2>
  <pre>
fn render(html: &str) {
    parse(html);
    paint();
}
  </pre>
  <h3>Notes</h3>
  <p>This is not a CSS engine yet. It is the small, useful beginning: networking glue, HTML tokenization, layout blocks, and a windowed renderer.</p>
</body>
</html>
"#;
