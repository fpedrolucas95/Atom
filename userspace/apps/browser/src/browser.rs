//! The browser state machine: current document, navigation, input handling,
//! and frame composition. Rendering is split between the chrome drawn here and
//! the per-block painters in [`crate::render`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;
use libgui::event::{KeyEvent, MouseButton, MouseEvent};
use libgui::surface::Surface;

use crate::content::{ABOUT_HOME, ABOUT_HTML};
use crate::dom::{Block, Document, Hit, InputKind};
use crate::html::parse_html;
use crate::net::{decode_data_uri, decode_image, fetch_http, fetch_url_bytes};
use crate::render::{self, Clip, FormCtx};
use crate::text::{escape_text, percent_encode, starts_with_ignore_ascii_case, truncate_for_width};
use crate::url::{normalize_http_url, resolve_url};

/// Maximum images fetched from the network per page load, bounding how long a
/// heavy page can block the UI (focus: bounded resource use).
const MAX_IMAGE_FETCHES: u32 = 6;
/// Upper bound on total images decoded per page (network + inline `data:`).
const MAX_IMAGE_DECODES: u32 = 12;

pub struct Browser {
    url: String,
    status: String,
    doc: Document,
    input_text: Vec<String>,
    focused_input: Option<usize>,
    link_hits: Vec<Hit>,
    input_hits: Vec<Hit>,
    scroll: u32,
    content_height: u32,
    view_h: u32,
    focused: bool,
    pub needs_redraw: bool,
}

impl Browser {
    pub fn new() -> Self {
        let mut browser = Self {
            url: String::from("about:home"),
            status: String::from("Ready"),
            doc: Document {
                title: String::from("Atom Browser"),
                blocks: Vec::new(),
                links: Vec::new(),
                inputs: Vec::new(),
            },
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

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.needs_redraw = true;
    }

    // ── Navigation ──────────────────────────────────────────────────────────

    fn load_current_url(&mut self) {
        self.status = String::from("Loading...");
        self.scroll = 0;
        self.focused_input = None;

        let html = self.resolve_page_source();
        self.doc = parse_html(&html);
        if self.doc.title.is_empty() {
            self.doc.title = String::from("Atom Browser");
        }
        self.input_text = self.doc.inputs.iter().map(|_| String::new()).collect();
        self.load_images();

        if self.status == "Loading..." {
            self.status = format!("Loaded {} blocks", self.doc.blocks.len());
        }
        self.needs_redraw = true;
    }

    /// Resolve the HTML source for the current URL, updating status/title.
    fn resolve_page_source(&mut self) -> String {
        if self.url == "about:home" || self.url.is_empty() {
            return String::from(ABOUT_HOME);
        }
        if self.url == "about:html" {
            return String::from(ABOUT_HTML);
        }
        if let Some(http_url) = normalize_http_url(&self.url) {
            self.url = http_url;
            return match fetch_http(&self.url) {
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
            };
        }
        self.status = String::from("Unsupported URL");
        format!(
            "<h1>Unsupported URL</h1><p>Atom Browser supports about:home, about:html, and plain http:// URLs.</p><p>Requested: {}</p>",
            escape_text(&self.url)
        )
    }

    /// Fetch and decode images up front (never during render), capping network
    /// fetches so a heavy page can't hang the UI.
    fn load_images(&mut self) {
        let page_url = self.url.clone();
        let mut fetched = 0u32;
        let mut decoded = 0u32;
        for block in self.doc.blocks.iter_mut() {
            let Block::Image { src, img, .. } = block else {
                continue;
            };
            if img.is_some() {
                continue;
            }
            // Decoding is CPU-heavy (inflate + per-pixel work); bound it so a
            // page full of inline images can't freeze the UI for seconds.
            if decoded >= MAX_IMAGE_DECODES {
                continue;
            }
            if starts_with_ignore_ascii_case(src.trim_start().as_bytes(), b"data:") {
                decoded += 1;
                *img = decode_data_uri(src).and_then(|b| decode_image(&b));
            } else if fetched < MAX_IMAGE_FETCHES {
                fetched += 1;
                if let Some(url) = resolve_url(&page_url, src) {
                    if let Some(bytes) = fetch_url_bytes(&url) {
                        decoded += 1;
                        *img = decode_image(&bytes);
                    }
                }
            }
        }
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
        if let Some(u) = resolve_url(&self.url, h).or_else(|| normalize_http_url(h)) {
            self.url = u;
            self.load_current_url();
        } else {
            self.status = format!("Cannot open: {}", h);
            self.needs_redraw = true;
        }
    }

    fn submit_input(&mut self, idx: usize) {
        let Some(meta) = self.doc.inputs.get(idx) else {
            return;
        };
        let value = self.input_text[idx].clone();
        let base = if meta.action.is_empty() {
            self.url.clone()
        } else {
            meta.action.clone()
        };
        let query = if meta.name.is_empty() {
            percent_encode(&value)
        } else {
            format!("{}={}", meta.name, percent_encode(&value))
        };
        let sep = if base.contains('?') { '&' } else { '?' };
        self.navigate_to(&format!("{}{}{}", base, sep, query));
    }

    // ── Scrolling ───────────────────────────────────────────────────────────

    fn scroll_by(&mut self, delta: i32) {
        let new = (self.scroll as i32 + delta).max(0) as u32;
        let max = self.content_height.saturating_sub(self.view_h);
        self.scroll = new.min(max);
        self.needs_redraw = true;
    }

    // ── Input handling ──────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) {
        if !key.pressed {
            return;
        }
        if let Some(i) = self.focused_input {
            self.edit_field(i, key);
            return;
        }
        self.edit_address_bar(key);
    }

    fn edit_field(&mut self, i: usize, key: KeyEvent) {
        match key.character {
            b'\n' | b'\r' => self.submit_input(i),
            8 => {
                self.input_text[i].pop();
                self.needs_redraw = true;
            }
            ch if (32..127).contains(&ch) => {
                self.input_text[i].push(ch as char);
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    fn edit_address_bar(&mut self, key: KeyEvent) {
        match key.character {
            b'\n' | b'\r' => self.load_current_url(),
            8 => {
                self.url.pop();
                self.needs_redraw = true;
            }
            0 => match key.scancode {
                0x48 => self.scroll_by(-32),  // Up
                0x50 => self.scroll_by(32),   // Down
                0x49 => self.scroll_by(-160), // PageUp
                0x51 => self.scroll_by(160),  // PageDown
                _ => {}
            },
            ch if (32..127).contains(&ch) => {
                self.url.push(ch as char);
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    pub fn handle_mouse(&mut self, ev: MouseEvent, width: u32) {
        match ev {
            MouseEvent::Scroll { delta, .. } => self.scroll_by(-(delta as i32) * 48),
            MouseEvent::ButtonDown {
                button: MouseButton::Left,
                x,
                y,
            } => self.handle_click(x, y, width),
            _ => {}
        }
    }

    fn handle_click(&mut self, x: i32, y: i32, width: u32) {
        // Toolbar: address bar / Go button.
        let (ix, iw, gx, gw) = render::addr_bar_geom(width);
        if y >= render::ADDR_Y as i32 && y < (render::ADDR_Y + render::ADDR_H) as i32 {
            if x >= gx as i32 && x < (gx + gw) as i32 {
                self.focused_input = None;
                self.load_current_url();
                return;
            }
            if x >= ix as i32 && x < (ix + iw) as i32 {
                self.focused_input = None;
                self.needs_redraw = true;
                return;
            }
        }

        // Content: links first, then form controls.
        if let Some(idx) = self.link_hits.iter().find(|h| h.contains(x, y)).map(|h| h.idx) {
            if let Some(href) = self.doc.links.get(idx).cloned() {
                self.navigate_to(&href);
            }
            return;
        }
        if let Some(idx) = self.input_hits.iter().find(|h| h.contains(x, y)).map(|h| h.idx) {
            match self.doc.inputs.get(idx).map(|m| m.kind) {
                Some(InputKind::Submit) => {
                    let target = self
                        .doc
                        .inputs
                        .iter()
                        .position(|m| matches!(m.kind, InputKind::Text | InputKind::Search))
                        .unwrap_or(idx);
                    self.submit_input(target);
                }
                // Selects are read-only here; text fields take focus.
                Some(InputKind::Select) => {}
                _ => {
                    self.focused_input = Some(idx);
                    self.needs_redraw = true;
                }
            }
            return;
        }
        if self.focused_input.take().is_some() {
            self.needs_redraw = true;
        }
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    pub fn render(&mut self, surface: &mut Surface) {
        let width = surface.width();
        let height = surface.height();

        surface.clear(render::BG);
        self.draw_chrome(surface, width);

        let clip = Clip {
            top: (render::TOOLBAR_H + 30) as i32,
            bottom: height.saturating_sub(render::STATUS_H) as i32,
        };
        let x0 = render::PADDING;
        let content_w = width.saturating_sub(render::PADDING * 2);

        let mut link_hits: Vec<Hit> = Vec::new();
        let mut input_hits: Vec<Hit> = Vec::new();
        let mut y = clip.top - self.scroll as i32;

        let form = FormCtx {
            inputs: &self.doc.inputs,
            values: &self.input_text,
            focused: self.focused_input,
        };
        for block in &self.doc.blocks {
            y = match block {
                Block::Text {
                    kind,
                    items,
                    align,
                    marker,
                } => render::draw_text_block(
                    surface,
                    *kind,
                    items,
                    *align,
                    marker.as_deref(),
                    x0,
                    content_w,
                    y,
                    clip,
                    &mut link_hits,
                    &form,
                    &mut input_hits,
                ),
                Block::Rule => render::draw_rule(surface, x0, content_w, y, clip),
                Block::Image { alt, img, align, .. } => {
                    render::draw_image_block(surface, alt, img, *align, x0, content_w, y, clip)
                }
            };
        }

        self.content_height = (y + self.scroll as i32 - clip.top).max(0) as u32;
        self.view_h = (clip.bottom - clip.top).max(1) as u32;

        self.draw_status_bar(surface, width, height);
        surface.present();

        self.link_hits = link_hits;
        self.input_hits = input_hits;
        self.needs_redraw = false;
    }

    fn draw_chrome(&self, surface: &mut Surface, width: u32) {
        surface.fill_rect(0, 0, width, render::TOOLBAR_H, render::CHROME);
        surface.draw_hline(
            0,
            render::TOOLBAR_H.saturating_sub(1),
            width,
            render::CHROME_LINE,
        );
        surface.draw_string(10, 8, "Atom", Color::rgb(150, 220, 255), render::CHROME);

        let (input_x, input_w, go_x, go_w) = render::addr_bar_geom(width);
        let field_bg = Color::rgb(246, 248, 250);
        surface.fill_rect(input_x, render::ADDR_Y, input_w, render::ADDR_H, field_bg);
        let border = if self.focused_input.is_none() && self.focused {
            render::ACCENT
        } else {
            render::CHROME_LINE
        };
        surface.draw_rect(input_x, render::ADDR_Y, input_w, render::ADDR_H, border);
        surface.draw_string(
            input_x + 6,
            render::ADDR_Y + 8,
            &truncate_for_width(&self.url, input_w.saturating_sub(12)),
            render::TEXT,
            field_bg,
        );
        surface.fill_rect(go_x, render::ADDR_Y, go_w, render::ADDR_H, render::ACCENT);
        surface.draw_string(go_x + 14, render::ADDR_Y + 8, "Go", Color::WHITE, render::ACCENT);

        surface.draw_string(
            render::PADDING,
            render::TOOLBAR_H + 8,
            &truncate_for_width(&self.doc.title, width.saturating_sub(render::PADDING * 2)),
            render::ACCENT,
            render::BG,
        );
    }

    fn draw_status_bar(&self, surface: &mut Surface, width: u32, height: u32) {
        let status_y = height.saturating_sub(render::STATUS_H);
        let bg = Color::rgb(235, 239, 245);
        surface.fill_rect(0, status_y, width, render::STATUS_H, bg);
        surface.draw_hline(0, status_y, width, Color::rgb(210, 218, 230));
        let status = format!("{}  |  {} links", self.status, self.doc.links.len());
        surface.draw_string(
            8,
            status_y + 6,
            &truncate_for_width(&status, width.saturating_sub(16)),
            render::MUTED,
            bg,
        );
    }
}
