//! The browser state machine: current document, navigation, input handling,
//! and frame composition. Rendering is split between the chrome drawn here and
//! the per-block painters in [`crate::render`].

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libgui::color::Color;
use libgui::event::{KeyEvent, MouseButton, MouseEvent};
use libgui::surface::Surface;

use atom_syscall::debug::log;

use crate::content::{ABOUT_HOME, ABOUT_HTML, ABOUT_LOADING};
use crate::dom::{Block, Document, Hit, InputKind};
use crate::domtree::Dom;
use crate::html::{flatten_dom, load_page, parse_html};
use crate::js;
use crate::net::{decode_data_uri, decode_image, fetch_http, fetch_url_bytes};
use crate::render::{self, truncate_for_width, Clip, FormCtx};
use crate::text::{escape_text, percent_encode, starts_with_ignore_ascii_case};
use crate::url::{normalize_http_url, resolve_url};

/// Maximum images fetched from the network per page load, bounding how long a
/// heavy page can block the UI (focus: bounded resource use).
const MAX_IMAGE_FETCHES: u32 = 6;
/// Upper bound on total images decoded per page (network + inline `data:`).
const MAX_IMAGE_DECODES: u32 = 12;
/// Maximum external stylesheets (`<link rel=stylesheet>`) fetched per page,
/// bounding network work the same way image fetches are bounded.
const MAX_CSS_FETCHES: u32 = 4;
/// Maximum external scripts (`<script src>`) fetched per page.
const MAX_JS_FETCHES: u32 = 4;
/// Cap on a single external resource's size (stylesheet or script), so one
/// huge file can't blow the bump heap or stall parsing.
const MAX_CSS_BYTES: usize = 512 * 1024;
/// Whether page JavaScript runs. Load-time scripts only — there is no event
/// loop yet, so handlers never fire after the page renders.
const SCRIPTING_ENABLED: bool = true;

/// The live page behind `Browser::doc`: kept so click events can run script
/// handlers against the real DOM and the page can be re-flattened.
struct PageState {
    dom: Dom,
    runtime: Option<js::Runtime>,
}

pub struct Browser {
    url: String,
    loaded_url: String,
    status: String,
    doc: Document,
    page: Option<PageState>,
    /// External stylesheets fetched at load, replayed on re-renders so event
    /// handlers never trigger network traffic.
    css_cache: Vec<(String, String)>,
    pending_load: bool,
    back_stack: Vec<String>,
    forward_stack: Vec<String>,
    input_text: Vec<String>,
    focused_input: Option<usize>,
    open_select: Option<usize>,
    select_scroll: usize,
    link_hits: Vec<Hit>,
    input_hits: Vec<Hit>,
    zone_hits: Vec<Hit>,
    select_option_hits: Vec<SelectOptionHit>,
    scroll: u32,
    content_height: u32,
    view_h: u32,
    dragging_scrollbar: bool,
    drag_start_y: i32,
    drag_start_scroll: u32,
    focused: bool,
    pub needs_redraw: bool,
}

struct SelectOptionHit {
    hit: Hit,
    option: usize,
}

#[derive(Clone, Copy)]
enum NavAction {
    Back,
    Forward,
    Reload,
    Home,
}

impl Browser {
    pub fn new() -> Self {
        let mut browser = Self {
            url: String::from("about:home"),
            loaded_url: String::new(),
            status: String::from("Ready"),
            doc: Document {
                title: String::from("Home"),
                background: None,
                blocks: Vec::new(),
                links: Vec::new(),
                link_nodes: Vec::new(),
                inputs: Vec::new(),
                input_nodes: Vec::new(),
                click_nodes: Vec::new(),
            },
            page: None,
            css_cache: Vec::new(),
            pending_load: false,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            input_text: Vec::new(),
            focused_input: None,
            open_select: None,
            select_scroll: 0,
            link_hits: Vec::new(),
            input_hits: Vec::new(),
            zone_hits: Vec::new(),
            select_option_hits: Vec::new(),
            scroll: 0,
            content_height: 0,
            view_h: 1,
            dragging_scrollbar: false,
            drag_start_y: 0,
            drag_start_scroll: 0,
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

    pub fn finish_pending_load(&mut self) {
        if self.pending_load {
            self.pending_load = false;
            self.load_current_url();
        }
    }

    fn load_current_url(&mut self) {
        self.status = String::from("Loading...");
        self.scroll = 0;
        self.focused_input = None;
        self.open_select = None;
        self.select_scroll = 0;

        let html = self.resolve_page_source();
        // `resolve_page_source` has settled `self.url` to the final document
        // URL, so relative `<link href>` / `<script src>` resolve against it.
        // Only http resources are fetched (about: pages are self-contained).
        let base = self.url.clone();
        let allow_net = normalize_http_url(&base).is_some();
        let fetch_resource = |budget: &mut u32, max: u32, href: &str| -> Option<String> {
            if !allow_net || *budget >= max {
                return None;
            }
            *budget += 1;
            let url = resolve_url(&base, href)?;
            let bytes = fetch_url_bytes(&url)?;
            if bytes.is_empty() || bytes.len() > MAX_CSS_BYTES {
                return None;
            }
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        let mut css_fetches = 0u32;
        let mut js_fetches = 0u32;
        // Stylesheets are cached so later re-renders (after click handlers
        // mutate the page) replay them without touching the network.
        let mut css_cache: Vec<(String, String)> = Vec::new();
        let page = load_page(
            &html,
            &mut |href| {
                if let Some((_, css)) = css_cache.iter().find(|(h, _)| h == href) {
                    return Some(css.clone());
                }
                let css = fetch_resource(&mut css_fetches, MAX_CSS_FETCHES, href)?;
                css_cache.push((String::from(href), css.clone()));
                Some(css)
            },
            &mut |src| fetch_resource(&mut js_fetches, MAX_JS_FETCHES, src),
            SCRIPTING_ENABLED,
        );
        self.doc = page.doc;
        self.page = Some(PageState {
            dom: page.dom,
            runtime: page.runtime,
        });
        self.css_cache = css_cache;
        for line in &page.console {
            log(&format!("browser/js: {}", line));
        }
        if self.doc.title.is_empty() {
            self.doc.title = String::from("Untitled");
        }
        self.input_text = self
            .doc
            .inputs
            .iter()
            .map(|meta| {
                if matches!(meta.kind, InputKind::Select) {
                    meta.placeholder.clone()
                } else {
                    String::new()
                }
            })
            .collect();
        self.load_images();
        self.loaded_url = self.url.clone();

        if self.status == "Loading..." {
            self.status = format!("Loaded {} blocks", self.doc.blocks.len());
        }
        self.needs_redraw = true;
    }

    fn show_loading(&mut self) {
        self.status = String::from("Loading...");
        self.doc = parse_html(ABOUT_LOADING);
        self.page = None;
        self.css_cache.clear();
        self.input_text.clear();
        self.focused_input = None;
        self.open_select = None;
        self.link_hits.clear();
        self.input_hits.clear();
        self.zone_hits.clear();
        self.select_option_hits.clear();
        self.scroll = 0;
        self.content_height = 0;
        self.pending_load = true;
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
                Ok(page) => {
                    self.url = page.final_url;
                    self.status = String::from("Loaded via HTTP");
                    page.body
                }
                Err(msg) => {
                    self.status = msg.clone();
                    self.error_page("Could not load page", &msg)
                }
            };
        }
        self.status = String::from("Unsupported URL");
        self.error_page(
            "Unsupported URL",
            &format!(
                "Only about: pages and plain http:// URLs are supported. Requested: {}",
                escape_text(&self.url)
            ),
        )
    }

    fn error_page(&self, title: &str, detail: &str) -> String {
        format!(
            "<!doctype html><html><head><title>{}</title><style>body {{ background: #0a0e15; color: #e2e8f0; }} h1 {{ color: #ff6b6b; }} h2 {{ color: #58a6ff; }} .muted {{ color: gray; }}</style></head><body><h1>{}</h1><p>{}</p><h2>Try next</h2><p><a href=\"about:home\">Home</a>   <a href=\"http://neverssl.com/\">NeverSSL</a>   <a href=\"http://example.com/\">Example</a></p><p class=\"muted\">HTTPS and compressed image resources are still limited in this build.</p></body></html>",
            escape_text(title),
            escape_text(title),
            escape_text(detail)
        )
    }

    /// Fetch and decode images up front (never during render), capping network
    /// fetches so a heavy page can't hang the UI.
    fn load_images(&mut self) {
        let page_url = self.url.clone();
        let mut fetched = 0u32;
        let mut decoded = 0u32;
        let mut ok = 0u32;
        let mut failed = 0u32;
        for block in self.doc.blocks.iter_mut() {
            let Block::Image { src, img, error, .. } = block else {
                continue;
            };
            if img.is_some() {
                continue;
            }
            // Decoding is CPU-heavy (inflate + per-pixel work); bound it so a
            // page full of inline images can't freeze the UI for seconds.
            if decoded >= MAX_IMAGE_DECODES {
                *error = Some(String::from("image limit reached"));
                failed += 1;
                continue;
            }
            if starts_with_ignore_ascii_case(src.trim_start().as_bytes(), b"data:") {
                decoded += 1;
                match decode_data_uri(src).and_then(|b| decode_image(&b)) {
                    Some(decoded_img) => {
                        *img = Some(decoded_img);
                        *error = None;
                        ok += 1;
                    }
                    None => {
                        *error = Some(String::from("decode failed"));
                        failed += 1;
                    }
                }
            } else if fetched < MAX_IMAGE_FETCHES {
                fetched += 1;
                if let Some(url) = resolve_url(&page_url, src) {
                    match fetch_url_bytes(&url) {
                        Some(bytes) => {
                            decoded += 1;
                            match decode_image(&bytes) {
                                Some(decoded_img) => {
                                    *img = Some(decoded_img);
                                    *error = None;
                                    ok += 1;
                                }
                                None => {
                                    *error = Some(String::from("decode failed"));
                                    failed += 1;
                                }
                            }
                        }
                        None => {
                            *error = Some(String::from("fetch failed"));
                            failed += 1;
                        }
                    }
                } else {
                    *error = Some(String::from("unsupported image URL"));
                    failed += 1;
                }
            } else {
                *error = Some(String::from("image fetch limit reached"));
                failed += 1;
            }
        }
        if ok > 0 || failed > 0 {
            self.status = format!("Loaded {} image(s), {} failed", ok, failed);
        }
    }

    // ── Script events ───────────────────────────────────────────────────────

    /// Core event dispatch helper. Returns `true` when `preventDefault` was
    /// called; re-flattens the page whenever any handler ran.
    fn fire_event(&mut self, target: js::Target, ty: &str) -> bool {
        let Some(mut page) = self.page.take() else {
            return false;
        };
        let mut outcome = js::DispatchOutcome::default();
        if let Some(rt) = &mut page.runtime {
            let mut console = Vec::new();
            outcome = rt.dispatch(&mut page.dom, &mut console, target, ty);
            for line in &console {
                log(&format!("browser/js: {}", line));
            }
        }
        self.page = Some(page);
        if outcome.fired {
            self.refresh_page();
        }
        outcome.prevented
    }

    /// Dispatch a `click` on the DOM node. Returns `true` when `preventDefault`
    /// was called.
    fn fire_click(&mut self, node: usize) -> bool {
        self.fire_event(js::Target::Node(node), "click")
    }

    /// Dispatch a keyboard event on `target` carrying full key properties.
    /// Returns `true` when `preventDefault` was called.
    fn fire_key_event(&mut self, target: js::Target, ty: &str, key: libgui::event::KeyEvent) -> bool {
        let Some(mut page) = self.page.take() else {
            return false;
        };
        let mut outcome = js::DispatchOutcome::default();
        if let Some(rt) = &mut page.runtime {
            let mut console = Vec::new();
            outcome = rt.dispatch_keyboard(
                &mut page.dom,
                &mut console,
                target,
                ty,
                key.character,
                key.scancode,
                key.modifiers.ctrl,
                key.modifiers.alt,
                key.modifiers.shift,
            );
            for line in &console {
                log(&format!("browser/js: {}", line));
            }
        }
        self.page = Some(page);
        if outcome.fired {
            self.refresh_page();
        }
        outcome.prevented
    }

    /// Fire `input` on the DOM node of the given input control.
    fn fire_input_on(&mut self, idx: usize) {
        if let Some(&node) = self.doc.input_nodes.get(idx) {
            self.fire_event(js::Target::Node(node), "input");
        }
    }

    /// Fire `change` on the DOM node of the given input control (on blur/Enter).
    fn blur_field(&mut self, idx: usize) {
        if let Some(&node) = self.doc.input_nodes.get(idx) {
            self.fire_event(js::Target::Node(node), "change");
        }
    }

    /// Walk the DOM up from `input_node` looking for a `<form>` ancestor.
    fn find_form_ancestor(&self, input_node: usize) -> Option<usize> {
        self.page.as_ref()?.dom.find_ancestor_tag(input_node, "form")
    }

    /// Fire any timers whose deadlines have passed; re-renders if any ran.
    pub fn tick_timers(&mut self, now_ms: u64) {
        let Some(mut page) = self.page.take() else {
            return;
        };
        if let Some(rt) = &mut page.runtime {
            let mut console = Vec::new();
            if rt.tick_timers(&mut page.dom, &mut console, now_ms) {
                for line in &console {
                    log(&format!("browser/js: {}", line));
                }
                self.page = Some(page);
                self.refresh_page();
                return;
            }
        }
        self.page = Some(page);
    }

    /// Run a `javascript:` URL body in the page's scope, then re-render.
    fn run_snippet(&mut self, code: &str) {
        let Some(mut page) = self.page.take() else {
            return;
        };
        if let Some(rt) = &mut page.runtime {
            let mut console = Vec::new();
            rt.run_snippet(&mut page.dom, &mut console, code);
            for line in &console {
                log(&format!("browser/js: {}", line));
            }
        }
        self.page = Some(page);
        self.refresh_page();
    }

    /// Re-flatten the live DOM (cached CSS only — no network) and swap in the
    /// new document, carrying user-typed input values across by DOM node.
    fn refresh_page(&mut self) {
        let Some(page) = self.page.take() else {
            return;
        };
        let clickable = page
            .runtime
            .as_ref()
            .map(|rt| rt.click_targets())
            .unwrap_or_default();
        let cache = core::mem::take(&mut self.css_cache);
        let doc = flatten_dom(
            &page.dom,
            &mut |href| cache.iter().find(|(h, _)| h == href).map(|(_, c)| c.clone()),
            SCRIPTING_ENABLED,
            &clickable,
        );
        self.css_cache = cache;

        // Map typed values and focus from old input indices to new ones via
        // the underlying DOM node ids (indices shift when the DOM changes).
        let old_values: Vec<(usize, String)> = self
            .doc
            .input_nodes
            .iter()
            .copied()
            .zip(self.input_text.iter().cloned())
            .collect();
        let focused_node = self
            .focused_input
            .and_then(|i| self.doc.input_nodes.get(i).copied());
        self.input_text = doc
            .input_nodes
            .iter()
            .zip(doc.inputs.iter())
            .map(|(node, meta)| {
                old_values
                    .iter()
                    .find(|(n, _)| n == node)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_else(|| {
                        if matches!(meta.kind, InputKind::Select) {
                            meta.placeholder.clone()
                        } else {
                            String::new()
                        }
                    })
            })
            .collect();
        self.focused_input = focused_node
            .and_then(|node| doc.input_nodes.iter().position(|&n| n == node));
        self.open_select = None;

        let title = if doc.title.is_empty() {
            self.doc.title.clone()
        } else {
            doc.title.clone()
        };
        let mut doc = doc;
        self.carry_images(&mut doc);
        self.doc = doc;
        self.doc.title = title;
        // Only genuinely new images (script-inserted) are fetched/decoded.
        self.load_images();
        self.page = Some(page);
        self.needs_redraw = true;
    }

    /// Move already-decoded images from the outgoing document into `doc`,
    /// matched by source URL, so re-renders never refetch or redecode.
    fn carry_images(&mut self, doc: &mut Document) {
        let mut old: Vec<(String, Option<libimage::DecodedImage>, Option<String>)> = Vec::new();
        for block in core::mem::take(&mut self.doc.blocks) {
            if let Block::Image { src, img, error, .. } = block {
                if img.is_some() || error.is_some() {
                    old.push((src, img, error));
                }
            }
        }
        for block in doc.blocks.iter_mut() {
            let Block::Image { src, img, error, .. } = block else {
                continue;
            };
            if let Some(pos) = old.iter().position(|(s, _, _)| s == src) {
                let (_, oimg, oerr) = old.remove(pos);
                *img = oimg;
                *error = oerr;
            }
        }
    }

    fn load_url(&mut self, url: String, record_history: bool) {
        let current = if self.loaded_url.is_empty() {
            self.url.clone()
        } else {
            self.loaded_url.clone()
        };
        if record_history && url != current {
            self.back_stack.push(current);
            self.forward_stack.clear();
        }
        self.url = url;
        self.show_loading();
    }

    fn navigate_to(&mut self, href: &str) {
        let h = href.trim();
        if h.is_empty() || h.starts_with('#') {
            return;
        }
        if let Some(code) = h.strip_prefix("javascript:") {
            let code = String::from(code);
            self.run_snippet(&code);
            return;
        }
        if h.starts_with("https://") {
            self.status = String::from("HTTPS not supported yet (no TLS)");
            self.needs_redraw = true;
            return;
        }
        if h.starts_with("about:") {
            self.load_url(String::from(h), true);
            return;
        }
        if let Some(u) = resolve_url(&self.url, h).or_else(|| normalize_http_url(h)) {
            self.load_url(u, true);
        } else {
            self.status = format!("Cannot open: {}", h);
            self.needs_redraw = true;
        }
    }

    fn reload(&mut self) {
        self.show_loading();
    }

    fn go_home(&mut self) {
        self.load_url(String::from("about:home"), true);
    }

    fn go_back(&mut self) {
        if let Some(url) = self.back_stack.pop() {
            self.forward_stack.push(self.loaded_url.clone());
            self.url = url;
            self.show_loading();
        }
    }

    fn go_forward(&mut self) {
        if let Some(url) = self.forward_stack.pop() {
            self.back_stack.push(self.loaded_url.clone());
            self.url = url;
            self.show_loading();
        }
    }

    fn submit_input(&mut self, idx: usize) {
        // Fire submit on the enclosing <form>; preventDefault cancels navigation.
        let form_node = self
            .doc
            .input_nodes
            .get(idx)
            .copied()
            .and_then(|n| self.find_form_ancestor(n));
        if let Some(form) = form_node {
            if self.fire_event(js::Target::Node(form), "submit") {
                return;
            }
        }

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
        self.scroll = new.min(self.max_scroll());
        self.needs_redraw = true;
    }

    fn max_scroll(&self) -> u32 {
        self.content_height.saturating_sub(self.view_h)
    }

    // ── Input handling ──────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) {
        if !key.pressed {
            return;
        }
        // Fire keydown on the focused element (or document). preventDefault
        // suppresses the default editing action.
        let kd_target = self
            .focused_input
            .and_then(|i| self.doc.input_nodes.get(i).copied())
            .map(js::Target::Node)
            .unwrap_or(js::Target::Document);
        let prevented = self.fire_key_event(kd_target, "keydown", key);
        if prevented {
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
            b'\n' | b'\r' => {
                self.blur_field(i); // fire change before submit
                self.submit_input(i);
            }
            8 => {
                self.input_text[i].pop();
                self.needs_redraw = true;
                self.fire_input_on(i);
            }
            ch if (32..127).contains(&ch) => {
                self.input_text[i].push(ch as char);
                self.needs_redraw = true;
                self.fire_input_on(i);
            }
            _ => {}
        }
    }

    fn edit_address_bar(&mut self, key: KeyEvent) {
        match key.character {
            b'\n' | b'\r' => self.load_url(self.url.clone(), true),
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
            MouseEvent::Scroll { delta, .. } => {
                if self.scroll_open_select(-(delta as i32)) {
                    return;
                }
                self.scroll_by(-(delta as i32) * 48);
            }
            MouseEvent::Move { y, .. } => self.handle_mouse_move(y),
            MouseEvent::ButtonDown {
                button: MouseButton::Left,
                x,
                y,
            } => self.handle_click(x, y, width),
            MouseEvent::ButtonUp {
                button: MouseButton::Left,
                ..
            } => {
                if self.dragging_scrollbar {
                    self.dragging_scrollbar = false;
                    self.needs_redraw = true;
                }
            }
            _ => {}
        }
    }

    fn handle_click(&mut self, x: i32, y: i32, width: u32) {
        if self.handle_scrollbar_down(x, y, width) {
            return;
        }

        if let Some(action) = self.nav_action_at(x, y, width) {
            self.focused_input = None;
            self.open_select = None;
            match action {
                NavAction::Back => self.go_back(),
                NavAction::Forward => self.go_forward(),
                NavAction::Reload => self.reload(),
                NavAction::Home => self.go_home(),
            }
            return;
        }

        if let Some(option) = self
            .select_option_hits
            .iter()
            .find(|h| h.hit.contains(x, y))
            .map(|h| (h.hit.idx, h.option))
        {
            self.choose_select_option(option.0, option.1);
            return;
        }

        // Toolbar: address bar / Go button.
        let (ix, iw, gx, gw) = render::addr_bar_geom(width);
        if y >= render::ADDR_Y as i32 && y < (render::ADDR_Y + render::ADDR_H) as i32 {
            if x >= gx as i32 && x < (gx + gw) as i32 {
                self.focused_input = None;
                self.open_select = None;
                self.load_url(self.url.clone(), true);
                return;
            }
            if x >= ix as i32 && x < (ix + iw) as i32 {
                self.focused_input = None;
                self.needs_redraw = true;
                return;
            }
        }

        // Content: links first, then form controls, then script click zones.
        // Each dispatches a `click` event through the page's handlers (with
        // bubbling); `preventDefault` suppresses the default action.
        if let Some(idx) = self.link_hits.iter().find(|h| h.contains(x, y)).map(|h| h.idx) {
            let prevented = match self.doc.link_nodes.get(idx).copied() {
                Some(node) => self.fire_click(node),
                None => false,
            };
            if !prevented {
                if let Some(href) = self.doc.links.get(idx).cloned() {
                    self.navigate_to(&href);
                }
            }
            return;
        }
        if let Some(idx) = self.input_hits.iter().find(|h| h.contains(x, y)).map(|h| h.idx) {
            let prevented = match self.doc.input_nodes.get(idx).copied() {
                Some(node) => self.fire_click(node),
                None => false,
            };
            if prevented {
                return;
            }
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
                Some(InputKind::Select) => {
                    self.focused_input = None;
                    self.open_select = Some(idx);
                    self.select_scroll = 0;
                    self.needs_redraw = true;
                }
                _ => {
                    // Blur the previously focused field (fires change) when
                    // switching focus to a different input.
                    if let Some(prev) = self.focused_input {
                        if prev != idx {
                            self.blur_field(prev);
                        }
                    }
                    self.focused_input = Some(idx);
                    self.open_select = None;
                    self.needs_redraw = true;
                }
            }
            return;
        }
        if let Some(idx) = self.zone_hits.iter().find(|h| h.contains(x, y)).map(|h| h.idx) {
            if let Some(node) = self.doc.click_nodes.get(idx).copied() {
                self.fire_click(node);
                return;
            }
        }
        if self.open_select.take().is_some() {
            self.needs_redraw = true;
        }
        // Fire change when the user clicks away from a focused text input.
        if let Some(prev) = self.focused_input.take() {
            self.blur_field(prev);
            self.needs_redraw = true;
        }
    }

    fn choose_select_option(&mut self, idx: usize, option: usize) {
        let Some(meta) = self.doc.inputs.get(idx) else {
            return;
        };
        let Some(next) = meta.options.get(option) else {
            return;
        };
        if let Some(value) = self.input_text.get_mut(idx) {
            *value = next.clone();
        }
        self.focused_input = None;
        self.open_select = None;
        self.needs_redraw = true;
    }

    fn scroll_open_select(&mut self, direction: i32) -> bool {
        let Some(idx) = self.open_select else {
            return false;
        };
        let Some(meta) = self.doc.inputs.get(idx) else {
            return false;
        };
        let max = meta.options.len().saturating_sub(6);
        if max == 0 {
            return true;
        }
        if direction > 0 {
            self.select_scroll = (self.select_scroll + 1).min(max);
        } else if direction < 0 {
            self.select_scroll = self.select_scroll.saturating_sub(1);
        }
        self.needs_redraw = true;
        true
    }

    fn nav_action_at(&self, x: i32, y: i32, _width: u32) -> Option<NavAction> {
        if y < render::ADDR_Y as i32 || y >= (render::ADDR_Y + render::ADDR_H) as i32 {
            return None;
        }
        for (idx, action) in [
            NavAction::Back,
            NavAction::Forward,
            NavAction::Reload,
            NavAction::Home,
        ]
        .iter()
        .enumerate()
        {
            let bx = render::NAV_X + idx as u32 * (render::NAV_BTN_W + render::NAV_GAP);
            if x >= bx as i32 && x < (bx + render::NAV_BTN_W) as i32 {
                return Some(*action);
            }
        }
        None
    }

    fn handle_mouse_move(&mut self, y: i32) {
        if !self.dragging_scrollbar {
            return;
        }
        if self.max_scroll() == 0 || self.view_h <= 1 {
            return;
        }
        let track_h = self.view_h;
        let thumb_h = self.scrollbar_thumb_h(track_h);
        let travel = track_h.saturating_sub(thumb_h);
        if travel == 0 {
            return;
        }
        let dy = y - self.drag_start_y;
        let delta = (dy as i64 * self.max_scroll() as i64) / travel as i64;
        self.scroll = (self.drag_start_scroll as i64 + delta)
            .max(0)
            .min(self.max_scroll() as i64) as u32;
        self.needs_redraw = true;
    }

    fn handle_scrollbar_down(&mut self, x: i32, y: i32, width: u32) -> bool {
        let Some((track_x, track_y, track_w, track_h)) = self.scrollbar_track_rect(width) else {
            return false;
        };
        if x < track_x as i32
            || x >= (track_x + track_w) as i32
            || y < track_y as i32
            || y >= (track_y + track_h) as i32
        {
            return false;
        }

        let (_, thumb_y, _, thumb_h) = self.scrollbar_thumb_rect(width).unwrap();
        if y >= thumb_y as i32 && y < (thumb_y + thumb_h) as i32 {
            self.dragging_scrollbar = true;
            self.drag_start_y = y;
            self.drag_start_scroll = self.scroll;
        } else if y < thumb_y as i32 {
            self.scroll_by(-(self.view_h as i32 * 4 / 5));
        } else {
            self.scroll_by(self.view_h as i32 * 4 / 5);
        }
        self.needs_redraw = true;
        true
    }

    fn scrollbar_track_rect(&self, width: u32) -> Option<(u32, u32, u32, u32)> {
        if self.max_scroll() == 0 || self.view_h <= 1 {
            return None;
        }
        let track_h = self.view_h;
        let track_y = render::TOOLBAR_H + 30;
        let track_x = width.saturating_sub(render::SCROLLBAR_W + 4);
        Some((track_x, track_y, render::SCROLLBAR_W, track_h))
    }

    fn scrollbar_thumb_h(&self, track_h: u32) -> u32 {
        let total = self.content_height.max(self.view_h);
        let thumb = self.view_h as u64 * track_h as u64 / total as u64;
        (thumb as u32).max(18).min(track_h)
    }

    fn scrollbar_thumb_rect(&self, width: u32) -> Option<(u32, u32, u32, u32)> {
        let (track_x, track_y, track_w, track_h) = self.scrollbar_track_rect(width)?;
        let thumb_h = self.scrollbar_thumb_h(track_h);
        let travel = track_h.saturating_sub(thumb_h);
        let y = if self.max_scroll() == 0 {
            track_y
        } else {
            track_y + (self.scroll as u64 * travel as u64 / self.max_scroll() as u64) as u32
        };
        Some((track_x, y, track_w, thumb_h))
    }

    // ── Rendering ───────────────────────────────────────────────────────────

    pub fn render(&mut self, surface: &mut Surface) {
        let width = surface.width();
        let height = surface.height();
        let page_bg = self.doc.background.unwrap_or(Color::WHITE);

        surface.clear(page_bg);
        self.draw_chrome(surface, width, page_bg);

        let clip = Clip {
            top: (render::TOOLBAR_H + 30) as i32,
            bottom: height.saturating_sub(render::STATUS_H) as i32,
        };
        let x0 = render::PADDING;
        let content_w = width.saturating_sub(render::PADDING * 2 + render::SCROLLBAR_W + 6);

        let mut link_hits: Vec<Hit> = Vec::new();
        let mut input_hits: Vec<Hit> = Vec::new();
        let mut zone_hits: Vec<Hit> = Vec::new();
        let mut select_option_hits: Vec<SelectOptionHit> = Vec::new();
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
                    page_bg,
                    &mut link_hits,
                    &form,
                    &mut input_hits,
                    &mut zone_hits,
                ),
                Block::Rule => render::draw_rule(surface, x0, content_w, y, clip),
                Block::Image { alt, img, error, align, .. } => {
                    render::draw_image_block(surface, alt, img, error.as_deref(), *align, x0, content_w, y, clip)
                }
            };
        }

        self.content_height = (y + self.scroll as i32 - clip.top).max(0) as u32;
        self.view_h = (clip.bottom - clip.top).max(1) as u32;
        self.scroll = self.scroll.min(self.max_scroll());

        self.draw_select_popup(surface, &input_hits, &mut select_option_hits, clip.bottom);
        self.draw_scrollbar(surface, width);
        self.draw_status_bar(surface, width, height);
        surface.present();

        self.link_hits = link_hits;
        self.input_hits = input_hits;
        self.zone_hits = zone_hits;
        self.select_option_hits = select_option_hits;
        self.needs_redraw = false;
    }

    fn draw_chrome(&self, surface: &mut Surface, width: u32, page_bg: Color) {
        surface.fill_rect(0, 0, width, render::TOOLBAR_H, render::CHROME);
        surface.draw_hline(
            0,
            render::TOOLBAR_H.saturating_sub(1),
            width,
            render::CHROME_LINE,
        );

        let (input_x, input_w, go_x, go_w) = render::addr_bar_geom(width);
        let field_bg = render::FIELD_BG;
        self.draw_nav_buttons(surface);
        surface.fill_rect_rounded_aa(input_x, render::ADDR_Y, input_w, render::ADDR_H, 7, field_bg);
        let border = if self.focused_input.is_none() && self.focused {
            render::ACCENT
        } else {
            render::CHROME_LINE
        };
        surface.draw_rect_rounded_aa(input_x, render::ADDR_Y, input_w, render::ADDR_H, 7, border);
        surface.draw_string(
            input_x + 10,
            render::ADDR_Y + 8,
            &truncate_for_width(&self.url, input_w.saturating_sub(12)),
            render::TEXT,
            field_bg,
        );
        surface.fill_rect_rounded_aa(go_x, render::ADDR_Y, go_w, render::ADDR_H, 7, render::ACCENT);
        surface.draw_string(go_x + 14, render::ADDR_Y + 8, "Go", Color::WHITE, render::ACCENT);

        surface.draw_string(
            render::PADDING,
            render::TOOLBAR_H + 8,
            &truncate_for_width(&self.doc.title, width.saturating_sub(render::PADDING * 2)),
            Color::rgb(96, 165, 250),
            page_bg,
        );
    }

    fn draw_nav_buttons(&self, surface: &mut Surface) {
        let buttons = [
            ("<", self.back_stack.is_empty(), NavAction::Back),
            (">", self.forward_stack.is_empty(), NavAction::Forward),
            ("R", false, NavAction::Reload),
            ("H", false, NavAction::Home),
        ];
        for (idx, (label, disabled, _)) in buttons.iter().enumerate() {
            let x = render::NAV_X + idx as u32 * (render::NAV_BTN_W + render::NAV_GAP);
            let bg = if *disabled {
                Color::rgb(15, 23, 36)
            } else {
                Color::rgb(38, 52, 72)
            };
            let fg = if *disabled {
                Color::rgb(121, 134, 154)
            } else {
                Color::rgb(240, 245, 252)
            };
            surface.fill_rect_rounded_aa(x, render::ADDR_Y, render::NAV_BTN_W, render::ADDR_H, 7, bg);
            surface.draw_string(x + 10, render::ADDR_Y + 8, label, fg, bg);
        }
    }

    fn draw_status_bar(&self, surface: &mut Surface, width: u32, height: u32) {
        let status_y = height.saturating_sub(render::STATUS_H);
        let bg = Color::rgb(15, 23, 36);
        surface.fill_rect(0, status_y, width, render::STATUS_H, bg);
        surface.draw_hline(0, status_y, width, render::CHROME_LINE);
        let status = format!("{}  |  {} links", self.status, self.doc.links.len());
        surface.draw_string(
            8,
            status_y + 6,
            &truncate_for_width(&status, width.saturating_sub(16)),
            render::MUTED,
            bg,
        );
    }

    fn draw_scrollbar(&self, surface: &mut Surface, width: u32) {
        let Some((track_x, track_y, track_w, track_h)) = self.scrollbar_track_rect(width) else {
            return;
        };
        let track = Color::rgb(226, 232, 240);
        let thumb = if self.dragging_scrollbar {
            render::ACCENT
        } else {
            Color::rgb(148, 163, 184)
        };
        surface.fill_rect(track_x, track_y, track_w, track_h, track);
        if let Some((thumb_x, thumb_y, thumb_w, thumb_h)) = self.scrollbar_thumb_rect(width) {
            surface.fill_rect(thumb_x, thumb_y, thumb_w, thumb_h, thumb);
        }
    }

    fn draw_select_popup(
        &self,
        surface: &mut Surface,
        input_hits: &[Hit],
        option_hits: &mut Vec<SelectOptionHit>,
        clip_bottom: i32,
    ) {
        let Some(idx) = self.open_select else {
            return;
        };
        let Some(meta) = self.doc.inputs.get(idx) else {
            return;
        };
        let Some(anchor) = input_hits.iter().find(|h| h.idx == idx) else {
            return;
        };
        let row_h = 20i32;
        let max_rows = 6usize;
        let start = self.select_scroll.min(meta.options.len().saturating_sub(1));
        let rows = meta.options.len().saturating_sub(start).min(max_rows);
        if rows == 0 {
            return;
        }
        let x = anchor.x.max(0) as u32;
        let y = (anchor.y + anchor.h).max(0) as u32;
        let w = anchor.w.max(80) as u32;
        let h = rows as u32 * row_h as u32;
        surface.fill_rect(x, y, w, h, render::FIELD_BG);
        surface.draw_rect(x, y, w, h, render::CHROME_LINE);

        for (row, label) in meta.options.iter().enumerate().skip(start).take(rows) {
            let oy = anchor.y + anchor.h + (row - start) as i32 * row_h;
            if oy + row_h > clip_bottom {
                break;
            }
            let selected = self.input_text.get(idx).map(String::as_str) == Some(label.as_str());
            let bg = if selected {
                Color::rgb(219, 234, 254)
            } else {
                render::FIELD_BG
            };
            let uy = oy.max(0) as u32;
            surface.fill_rect(x + 1, uy + 1, w.saturating_sub(2), row_h as u32 - 2, bg);
            surface.draw_string(
                x + 6,
                uy + 6,
                &truncate_for_width(label, w.saturating_sub(12)),
                render::TEXT,
                bg,
            );
            option_hits.push(SelectOptionHit {
                hit: Hit {
                    x: anchor.x,
                    y: oy,
                    w: anchor.w.max(80),
                    h: row_h,
                    idx,
                },
                option: row,
            });
        }
    }
}
