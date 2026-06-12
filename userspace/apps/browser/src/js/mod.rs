//! JavaScript engine: a hand-written, `no_std + alloc` interpreter.
//!
//! Pipeline: [`lexer`] → [`parser`] (recursive descent + precedence climbing,
//! with ASI) → [`interp`] (tree-walking evaluator with a step budget and
//! call-depth cap) over [`value`]'s prototype-based object model, with the
//! standard library in [`builtins`] and DOM access in [`dom_api`].
//!
//! Execution model (phase 1): scripts run **once, in document order, after
//! tree construction** — equivalent to every script being `defer`. They may
//! mutate the DOM (`document.write` output is grafted at the script's
//! position); styling and flattening happen afterwards, so mutations are
//! visible in the rendered page. There is no event loop yet: handlers
//! (`onclick`, `addEventListener`) are accepted but never fire, and timers
//! only run inline at zero delay.
//!
//! A misbehaving script cannot take the browser down: parse errors, uncaught
//! exceptions, and budget/depth aborts are reported on the console and the
//! page still renders.

pub mod ast;
pub mod builtins;
pub mod dom_api;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod value;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use interp::{Control, Interp, WriteCursor};

use crate::domtree::{Dom, DOCUMENT};

/// One script to execute: the element id and its source text.
struct PageScript {
    node: usize,
    source: String,
}

/// Execute every `<script>` in `dom` in document order. External sources are
/// resolved through `fetch_js` (`href` as written → source text, or `None`
/// to skip). Console output and script errors accumulate in `console`.
pub fn run_page_scripts(
    dom: &mut Dom,
    fetch_js: &mut dyn FnMut(&str) -> Option<String>,
    console: &mut Vec<String>,
) {
    let scripts = collect_scripts(dom, fetch_js, console);
    if scripts.is_empty() {
        return;
    }

    let mut it = Interp::new(dom, console);
    for script in scripts {
        it.cursor = Some(write_cursor_for(it.dom, script.node));
        match parser::parse_program(&script.source) {
            Err(e) => it.console.push(format!("[script error] {e}")),
            Ok(stmts) => match it.run_program(&stmts) {
                Ok(()) | Err(Control::Return(_)) => {}
                Err(Control::Throw(v)) => it
                    .console
                    .push(format!("Uncaught {}", value::to_string(&v))),
                Err(Control::Abort(why)) => {
                    it.console.push(format!("[script aborted] {why}"))
                }
                Err(Control::Break) | Err(Control::Continue) => {}
            },
        }
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
