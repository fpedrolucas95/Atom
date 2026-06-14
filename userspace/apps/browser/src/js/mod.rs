//! JavaScript engine: a hand-written, `no_std + alloc` interpreter.
//!
//! Pipeline: [`lexer`] → [`parser`] (recursive descent + precedence climbing,
//! with ASI) → [`interp`] (tree-walking evaluator with a step budget and
//! call-depth cap) over [`value`]'s prototype-based object model, with the
//! standard library in [`builtins`], DOM access in [`dom_api`], and event
//! registration/dispatch in [`events`].
//!
//! Execution model: [`Runtime`] holds the page's global scope and handler
//! table and **stays alive after load**. Load runs every `<script>` once in
//! document order, then fires `DOMContentLoaded` and `load`. Afterwards the
//! browser dispatches `click`, `keydown`, `input`, `change`, and `submit`
//! events through [`Runtime::dispatch`] / [`Runtime::dispatch_keyboard`] as
//! the user interacts, re-flattening the DOM when handlers ran. Each entry
//! gets its own step budget, so a runaway load script cannot starve later
//! events and no script can hang the browser. `setTimeout` with delay > 0
//! and `setInterval` schedule callbacks in a [`events::TimerQueue`] that the
//! main event loop drains each iteration via [`Runtime::tick_timers`].
//!
//! A misbehaving script cannot take the page down: parse errors, uncaught
//! exceptions, and budget/depth aborts are reported on the console and the
//! page still renders.

pub mod ast;
pub mod builtins;
pub mod dom_api;
pub mod events;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod storage;
pub mod value;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use interp::{Control, Interp, WriteCursor};
use value::Value;

pub use events::{DispatchOutcome, Handlers, Target};

use crate::domtree::{Dom, DOCUMENT};

/// Step budget for the load phase (all `<script>`s plus load events).
const LOAD_BUDGET: u64 = 3_000_000;
/// Step budget for each later event dispatch / `javascript:` snippet.
const EVENT_BUDGET: u64 = 1_000_000;

/// The page's persistent script state: global scope plus registered handlers.
/// Lives in the browser alongside the DOM for as long as the page is shown.
pub struct Runtime {
    pub global: value::EnvRef,
    pub handlers: Handlers,
    pub storage: storage::Storage,
}

/// One script to execute: the element id and its source text.
struct PageScript {
    node: usize,
    source: String,
}

impl Runtime {
    pub fn new() -> Self {
        let global = value::Env::new_global();
        builtins::install_globals(&global);
        Self {
            global,
            handlers: Handlers::new(),
            storage: storage::Storage::new(),
        }
    }

    /// Load phase: run every `<script>` in document order, then fire
    /// `DOMContentLoaded` on the document and `load` on the window.
    pub fn run_load_scripts(
        &mut self,
        dom: &mut Dom,
        fetch_js: &mut dyn FnMut(&str) -> Option<String>,
        console: &mut Vec<String>,
    ) {
        let scripts = collect_scripts(dom, fetch_js, console);
        let mut it = Interp::new(
            dom,
            console,
            &mut self.handlers,
            &mut self.storage,
            self.global.clone(),
            LOAD_BUDGET,
        );
        for script in scripts {
            it.cursor = Some(write_cursor_for(it.dom, script.node));
            match parser::parse_program(&script.source) {
                Err(e) => it.console.push(format!("[script error] {e}")),
                Ok(stmts) => match it.run_program(&stmts) {
                    Ok(()) | Err(Control::Return(_)) => {}
                    Err(Control::Throw(v)) => it
                        .console
                        .push(format!("Uncaught {}", value::to_string(&v))),
                    Err(Control::Abort(why)) => it.console.push(format!("[script aborted] {why}")),
                    Err(Control::Break) | Err(Control::Continue) => {}
                },
            }
        }
        it.cursor = None;
        events::dispatch(&mut it, Target::Document, "DOMContentLoaded");
        events::dispatch(&mut it, Target::Window, "load");
    }

    /// Dispatch a user-interaction event (e.g. a click on a node).
    pub fn dispatch(
        &mut self,
        dom: &mut Dom,
        console: &mut Vec<String>,
        target: Target,
        ty: &str,
    ) -> DispatchOutcome {
        let mut it = Interp::new(
            dom,
            console,
            &mut self.handlers,
            &mut self.storage,
            self.global.clone(),
            EVENT_BUDGET,
        );
        events::dispatch(&mut it, target, ty)
    }

    /// Run a `javascript:` URL's body in the page's global scope.
    pub fn run_snippet(&mut self, dom: &mut Dom, console: &mut Vec<String>, src: &str) {
        let mut it = Interp::new(
            dom,
            console,
            &mut self.handlers,
            &mut self.storage,
            self.global.clone(),
            EVENT_BUDGET,
        );
        match parser::parse_program(src) {
            Err(e) => it.console.push(format!("[script error] {e}")),
            Ok(stmts) => match it.run_program(&stmts) {
                Ok(()) | Err(Control::Return(_)) => {}
                Err(Control::Throw(v)) => it
                    .console
                    .push(format!("Uncaught {}", value::to_string(&v))),
                Err(Control::Abort(why)) => it.console.push(format!("[script aborted] {why}")),
                Err(_) => {}
            },
        }
    }

    /// Node ids carrying click handlers (for clickable-region flattening).
    pub fn click_targets(&self) -> Vec<usize> {
        self.handlers.click_targets()
    }

    /// Dispatch a keyboard event with full key properties on `target`.
    /// `ctrl`, `alt`, `shift` come from the hardware modifier state.
    pub fn dispatch_keyboard(
        &mut self,
        dom: &mut Dom,
        console: &mut Vec<String>,
        target: Target,
        ty: &str,
        character: u8,
        scancode: u8,
        ctrl: bool,
        alt: bool,
        shift: bool,
    ) -> DispatchOutcome {
        // Map character/scancode to a JS key name and keyCode.
        let (key_name, key_code): (&str, u32) = if character == 0 {
            match scancode {
                0x48 => ("ArrowUp", 38),
                0x50 => ("ArrowDown", 40),
                0x4B => ("ArrowLeft", 37),
                0x4D => ("ArrowRight", 39),
                0x49 => ("PageUp", 33),
                0x51 => ("PageDown", 34),
                0x01 => ("Escape", 27),
                0x0F => ("Tab", 9),
                _ => ("Unidentified", 0),
            }
        } else {
            match character {
                8 => ("Backspace", 8),
                13 => ("Enter", 13),
                27 => ("Escape", 27),
                32..=126 => {
                    // Leak one &'static str per unique printable char — tiny fixed set.
                    let ch = character as char;
                    let s: &'static str = {
                        let owned = alloc::string::String::from(ch);
                        alloc::boxed::Box::leak(owned.into_boxed_str())
                    };
                    (s, character as u32)
                }
                _ => ("Unidentified", 0),
            }
        };

        let extra: alloc::vec::Vec<(&str, Value)> = alloc::vec![
            ("key", value::str_value(key_name)),
            ("keyCode", Value::Num(key_code as f64)),
            ("which", Value::Num(key_code as f64)),
            (
                "charCode",
                Value::Num(if character >= 32 && character < 127 {
                    character as f64
                } else {
                    0.0
                })
            ),
            ("ctrlKey", Value::Bool(ctrl)),
            ("altKey", Value::Bool(alt)),
            ("shiftKey", Value::Bool(shift)),
            ("metaKey", Value::Bool(false)),
        ];

        let mut it = Interp::new(
            dom,
            console,
            &mut self.handlers,
            &mut self.storage,
            self.global.clone(),
            EVENT_BUDGET,
        );
        events::dispatch_with_props(&mut it, target, ty, &extra)
    }

    /// Fire any timers whose deadlines have passed. Returns `true` when at
    /// least one timer ran (the page may need re-rendering).
    pub fn tick_timers(&mut self, dom: &mut Dom, console: &mut Vec<String>, now_ms: u64) -> bool {
        let expired = self.handlers.timers.take_expired(now_ms);
        if expired.is_empty() {
            return false;
        }
        let mut it = Interp::new(
            dom,
            console,
            &mut self.handlers,
            &mut self.storage,
            self.global.clone(),
            EVENT_BUDGET,
        );
        for timer in expired {
            match it.call(&timer.callback, &Value::Undefined, &[]) {
                Ok(_) | Err(Control::Return(_)) => {}
                Err(Control::Throw(v)) => {
                    let s = format!("Uncaught (in timer) {}", value::to_string(&v));
                    it.console.push(s);
                }
                Err(Control::Abort(why)) => {
                    it.console.push(format!("[timer aborted] {why}"));
                    break;
                }
                Err(_) => {}
            }
        }
        true
    }
}

/// Gather runnable scripts in tree order, resolving external sources.
fn collect_scripts(
    dom: &Dom,
    fetch_js: &mut dyn FnMut(&str) -> Option<String>,
    console: &mut Vec<String>,
) -> Vec<PageScript> {
    let mut out = Vec::new();
    let mut stack = alloc::vec![DOCUMENT];
    // DFS preserving document order via reversed child pushes.
    while let Some(id) = stack.pop() {
        if dom.tag(id) == "noscript" {
            continue; // scripting is on: noscript content is inert
        }
        if dom.tag(id) == "script" {
            if let Some(el) = dom.element(id) {
                let ty = el.attr("type").unwrap_or("").trim().to_ascii_lowercase();
                let runnable = ty.is_empty()
                    || ty == "text/javascript"
                    || ty == "application/javascript"
                    || ty == "text/ecmascript";
                if !runnable {
                    if ty == "module" {
                        console.push(String::from(
                            "[script skipped] ES modules are not supported",
                        ));
                    }
                    continue;
                }
                let source = match el.attr("src").filter(|s| !s.trim().is_empty()) {
                    Some(src) => match fetch_js(src) {
                        Some(text) => text,
                        None => {
                            console.push(format!("[script skipped] failed to load {src}"));
                            continue;
                        }
                    },
                    None => {
                        let mut text = String::new();
                        dom.text_content(id, &mut text);
                        text
                    }
                };
                if !source.trim().is_empty() {
                    out.push(PageScript { node: id, source });
                }
            }
            continue;
        }
        for &c in dom.nodes[id].children.iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// Where this script's `document.write` output goes: right after the script
/// element — unless the script lives in `<head>`, in which case writes land
/// at the start of the body so they remain visible.
fn write_cursor_for(dom: &Dom, script: usize) -> WriteCursor {
    let mut anc = dom.nodes[script].parent;
    let mut in_head = false;
    while anc != usize::MAX {
        if dom.tag(anc) == "head" {
            in_head = true;
            break;
        }
        anc = dom.nodes[anc].parent;
    }
    if in_head {
        if let Some(body) = dom.find_first("body") {
            return WriteCursor {
                parent: body,
                index: 0,
            };
        }
    }
    let parent = dom.nodes[script].parent;
    if parent == usize::MAX {
        return WriteCursor {
            parent: DOCUMENT,
            index: dom.nodes[DOCUMENT].children.len(),
        };
    }
    let index = dom.nodes[parent]
        .children
        .iter()
        .position(|&c| c == script)
        .map(|i| i + 1)
        .unwrap_or(dom.nodes[parent].children.len());
    WriteCursor { parent, index }
}
