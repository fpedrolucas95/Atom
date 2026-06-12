//! DOM bindings: `document`, element handles, `style`, and `classList`.
//!
//! Elements are first-class interpreter values carrying the arena node id
//! ([`Value::Node`]); property access lands here. Implemented surface:
//!
//! * `document`: `title`, `body`, `documentElement`, `head`, `readyState`,
//!   `getElementById`, `querySelector(All)` (reusing the CSS selector
//!   engine), `getElementsByTagName/ClassName`, `createElement`,
//!   `createTextNode`, `write`/`writeln` (fragment-grafted at the running
//!   script's position), `cookie` (empty), `addEventListener` (no-op).
//! * elements: `tagName`, `textContent`/`innerText`, `innerHTML` (get
//!   serialises, set reparses), `outerHTML`, tree navigation (`parentNode`,
//!   `children`, `firstElementChild`, …), `style` (mapped onto the `style`
//!   attribute, camelCase ↔ kebab-case), `classList`, `get/set/remove/
//!   hasAttribute`, `appendChild`/`insertBefore`/`removeChild`/`remove`,
//!   `cloneNode`, scoped `querySelector(All)`, and a generic fallback that
//!   reads unknown properties as attributes (so `a.href` just works).
//!
//! Event listeners are accepted but never fire — there is no event loop yet.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::interp::{Control, EResult, Interp, WriteCursor};
use super::value::*;
use crate::css::parse_selector_list;
use crate::domtree::{is_void, Dom, NodeData, DOCUMENT};

// ── document ────────────────────────────────────────────────────────────────

static DOCUMENT_METHODS: &[&str] = &[
    "getElementById", "querySelector", "querySelectorAll", "getElementsByTagName",
    "getElementsByClassName", "createElement", "createTextNode", "write", "writeln",
    "addEventListener", "removeEventListener",
];

pub fn document_get(it: &mut Interp, key: &str) -> EResult {
    Ok(match key {
        "title" => {
            let mut t = String::new();
            if let Some(id) = it.dom.find_first("title") {
                it.dom.text_content(id, &mut t);
            }
            str_value(t.trim())
        }
        "body" => it
            .dom
            .find_first("body")
            .map(Value::Node)
            .unwrap_or(Value::Null),
        "head" => it
            .dom
            .find_first("head")
            .map(Value::Node)
            .unwrap_or(Value::Null),
        "documentElement" => it
            .dom
            .find_first("html")
            .map(Value::Node)
            .unwrap_or(Value::Null),
        "readyState" => str_value("interactive"),
        "cookie" => str_value(""),
        "location" => env_get(&it.global, "location").unwrap_or(Value::Undefined),
        "forms" | "images" | "links" | "scripts" => new_array(Vec::new()),
        m if DOCUMENT_METHODS.contains(&m) => {
            native(document_method, intern_doc_method(m))
        }
        _ => Value::Undefined,
    })
}

pub fn document_set(it: &mut Interp, key: &str, value: Value) -> Result<(), Control> {
    match key {
        "title" => {
            let text = to_string(&value);
            let title = match it.dom.find_first("title") {
                Some(id) => id,
                None => {
                    let head = it
                        .dom
                        .find_first("head")
                        .or_else(|| it.dom.find_first("html"))
                        .unwrap_or(DOCUMENT);
                    let t = it.dom.create_element("title", Vec::new());
                    it.dom.attach(head, t, Some(0));
                    t
                }
            };
            it.dom.set_text_content(title, &text);
        }
        _ => {} // cookie / location / on* assignments are accepted and dropped
    }
    Ok(())
}

/// Method names are a fixed set; return the `'static` spelling.
fn intern_doc_method(m: &str) -> &'static str {
    DOCUMENT_METHODS
        .iter()
        .find(|x| **x == m)
        .copied()
        .unwrap_or("noop")
}

fn document_method(it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    Ok(match name {
        "getElementById" => {
            let id = to_string(&arg(0));
            find_by(it.dom, DOCUMENT, &mut |dom, n| {
                dom.element(n).is_some_and(|e| e.attr("id") == Some(id.as_str()))
            })
            .map(Value::Node)
            .unwrap_or(Value::Null)
        }
        "querySelector" => query_selector(it, DOCUMENT, &to_string(&arg(0)), true)
            .into_iter()
            .next()
            .map(Value::Node)
            .unwrap_or(Value::Null),
        "querySelectorAll" => new_array(
            query_selector(it, DOCUMENT, &to_string(&arg(0)), false)
                .into_iter()
                .map(Value::Node)
                .collect(),
        ),
        "getElementsByTagName" => {
            let tag = to_string(&arg(0)).to_ascii_lowercase();
            new_array(collect_by(it.dom, DOCUMENT, &mut |dom, n| {
                dom.tag(n) == tag || tag == "*"
            }))
        }
        "getElementsByClassName" => {
            let class = to_string(&arg(0));
            new_array(collect_by(it.dom, DOCUMENT, &mut |dom, n| {
                has_class(dom, n, &class)
            }))
        }
        "createElement" => {
            let tag = to_string(&arg(0)).to_ascii_lowercase();
            Value::Node(it.dom.create_element(&tag, Vec::new()))
        }
        "createTextNode" => Value::Node(it.dom.create_text(&to_string(&arg(0)))),
        "write" | "writeln" => {
            let mut html = to_string(&arg(0));
            if name == "writeln" {
                html.push('\n');
            }
            doc_write(it, &html);
            Value::Undefined
        }
        _ => Value::Undefined, // event listeners
    })
}

/// Graft `document.write` output at the running script's cursor, falling back
/// to the end of the body.
fn doc_write(it: &mut Interp, html: &str) {
    let cursor = it.cursor.unwrap_or_else(|| {
        let body = it.dom.find_first("body").unwrap_or(DOCUMENT);
        WriteCursor {
            parent: body,
            index: it.dom.nodes[body].children.len(),
        }
    });
    let inserted = it.dom.graft_fragment(html, cursor.parent, cursor.index);
    it.cursor = Some(WriteCursor {
        parent: cursor.parent,
        index: cursor.index + inserted,
    });
}

// ── Element handles ─────────────────────────────────────────────────────────

static NODE_METHODS: &[&str] = &[
    "getAttribute", "setAttribute", "removeAttribute", "hasAttribute", "appendChild",
    "removeChild", "insertBefore", "remove", "cloneNode", "querySelector", "querySelectorAll",
    "getElementsByTagName", "addEventListener", "removeEventListener", "focus", "blur",
    "click", "contains", "matches",
];

pub fn node_get(it: &mut Interp, id: usize, key: &str) -> EResult {
    let dom = &it.dom;
    Ok(match key {
        "tagName" | "nodeName" => str_value(&dom.tag(id).to_ascii_uppercase()),
        "id" => str_value(attr_of(dom, id, "id").unwrap_or_default().as_str()),
        "className" => str_value(attr_of(dom, id, "class").unwrap_or_default().as_str()),
        "textContent" | "innerText" => {
            let mut t = String::new();
            dom.text_content(id, &mut t);
            str_value(&t)
        }
        "innerHTML" => {
            let mut out = String::new();
            serialize_children(dom, id, &mut out, 0);
            str_value(&out)
        }
        "outerHTML" => {
            let mut out = String::new();
            serialize_node(dom, id, &mut out, 0);
            str_value(&out)
        }
        "nodeType" => Value::Num(match &dom.nodes[id].data {
            NodeData::Element(_) => 1.0,
            NodeData::Text(_) => 3.0,
            NodeData::Document => 9.0,
        }),
        "parentNode" | "parentElement" => {
            let p = dom.nodes[id].parent;
            if p == usize::MAX {
                Value::Null
            } else if matches!(dom.nodes[p].data, NodeData::Document) {
                Value::Document
            } else {
                Value::Node(p)
            }
        }
        "children" | "childNodes" => new_array(
            dom.nodes[id]
                .children
                .iter()
                .filter(|&&c| dom.element(c).is_some())
                .map(|&c| Value::Node(c))
                .collect(),
        ),
        "firstChild" | "firstElementChild" => dom.nodes[id]
            .children
            .iter()
            .find(|&&c| dom.element(c).is_some())
            .map(|&c| Value::Node(c))
            .unwrap_or(Value::Null),
        "lastChild" | "lastElementChild" => dom.nodes[id]
            .children
            .iter()
            .rev()
            .find(|&&c| dom.element(c).is_some())
            .map(|&c| Value::Node(c))
            .unwrap_or(Value::Null),
        "nextSibling" | "nextElementSibling" => sibling(dom, id, 1),
        "previousSibling" | "previousElementSibling" => sibling(dom, id, -1),
        "style" => Value::StyleOf(id),
        "classList" => {
            let o = new_plain();
            o.borrow_mut().props.insert("__node".into(), Value::Num(id as f64));
            for m in ["add", "remove", "toggle", "contains"] {
                o.borrow_mut()
                    .props
                    .insert((*m).into(), native(classlist_fn, intern_classlist(m)));
            }
            Value::Obj(o)
        }
        "value" => str_value(attr_of(dom, id, "value").unwrap_or_default().as_str()),
        "dataset" => {
            // Snapshot of data-* attributes (read-only).
            let o = new_plain();
            if let Some(el) = dom.element(id) {
                for (n, v) in &el.attrs {
                    if let Some(rest) = n.strip_prefix("data-") {
                        o.borrow_mut()
                            .props
                            .insert(kebab_to_camel(rest), str_value(v));
                    }
                }
            }
            Value::Obj(o)
        }
        m if NODE_METHODS.contains(&m) => native(node_method, intern_node_method(m)),
        // Unknown property: fall back to the attribute of the same name, so
        // `a.href`, `img.src`, `input.name`, … work without explicit cases.
        _ => match attr_of(dom, id, &key.to_ascii_lowercase()) {
            Some(v) => str_value(&v),
            None => Value::Undefined,
        },
    })
}

pub fn node_set(it: &mut Interp, id: usize, key: &str, value: Value) -> Result<(), Control> {
    match key {
        "textContent" | "innerText" => {
            let text = to_string(&value);
            it.dom.set_text_content(id, &text);
        }
        "innerHTML" => {
            let html = to_string(&value);
            it.dom.clear_children(id);
            it.dom.graft_fragment(&html, id, 0);
        }
        "className" => {
            let v = to_string(&value);
            it.dom.set_attr(id, "class", &v);
        }
        "style" => {
            let v = to_string(&value);
            it.dom.set_attr(id, "style", &v);
        }
        k if k.starts_with("on") => {} // event handlers: accepted, never fire
        _ => {
            let v = to_string(&value);
            it.dom.set_attr(id, &key.to_ascii_lowercase(), &v);
        }
    }
    Ok(())
}

fn intern_node_method(m: &str) -> &'static str {
    NODE_METHODS
        .iter()
        .find(|x| **x == m)
        .copied()
        .unwrap_or("noop")
}

fn node_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let Value::Node(id) = this else {
        return Ok(Value::Undefined);
    };
    let id = *id;
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    Ok(match name {
        "getAttribute" => {
            let n = to_string(&arg(0)).to_ascii_lowercase();
            match attr_of(it.dom, id, &n) {
                Some(v) => str_value(&v),
                None => Value::Null,
            }
        }
        "setAttribute" => {
            let n = to_string(&arg(0));
            let v = to_string(&arg(1));
            it.dom.set_attr(id, &n, &v);
            Value::Undefined
        }
        "removeAttribute" => {
            it.dom.remove_attr(id, &to_string(&arg(0)));
            Value::Undefined
        }
        "hasAttribute" => Value::Bool(
            attr_of(it.dom, id, &to_string(&arg(0)).to_ascii_lowercase()).is_some(),
        ),
        "appendChild" => {
            if let Value::Node(c) = arg(0) {
                it.dom.attach(id, c, None);
                return Ok(Value::Node(c));
            }
            Value::Undefined
        }
        "insertBefore" => {
            if let Value::Node(c) = arg(0) {
                let before = match arg(1) {
                    Value::Node(r) => it.dom.nodes[id].children.iter().position(|&x| x == r),
                    _ => None,
                };
                it.dom.attach(id, c, before);
                return Ok(Value::Node(c));
            }
            Value::Undefined
        }
        "removeChild" => {
            if let Value::Node(c) = arg(0) {
                if it.dom.nodes[c].parent == id {
                    it.dom.detach(c);
                }
                return Ok(Value::Node(c));
            }
            Value::Undefined
        }
        "remove" => {
            it.dom.detach(id);
            Value::Undefined
        }
        "cloneNode" => {
            let deep = truthy(&arg(0));
            if deep {
                Value::Node(it.dom.deep_copy(id))
            } else {
                let attrs = it
                    .dom
                    .element(id)
                    .map(|e| e.attrs.clone())
                    .unwrap_or_default();
                let tag = it.dom.tag(id).to_string();
                Value::Node(it.dom.create_element(&tag, attrs))
            }
        }
        "querySelector" => query_selector(it, id, &to_string(&arg(0)), true)
            .into_iter()
            .next()
            .map(Value::Node)
            .unwrap_or(Value::Null),
        "querySelectorAll" => new_array(
            query_selector(it, id, &to_string(&arg(0)), false)
                .into_iter()
                .map(Value::Node)
                .collect(),
        ),
        "getElementsByTagName" => {
            let tag = to_string(&arg(0)).to_ascii_lowercase();
            new_array(collect_by(it.dom, id, &mut |dom, n| {
                n != id && (dom.tag(n) == tag || tag == "*")
            }))
        }
        "contains" => Value::Bool(match arg(0) {
            Value::Node(c) => it.dom.is_ancestor(id, c),
            _ => false,
        }),
        "matches" => {
            let sels = parse_selector_list(&to_string(&arg(0)));
            Value::Bool(sels.iter().any(|s| s.matches(it.dom, id)))
        }
        _ => Value::Undefined, // listeners / focus / blur / click
    })
}

// ── style and classList ─────────────────────────────────────────────────────

pub fn style_get(it: &mut Interp, id: usize, key: &str) -> EResult {
    let style = attr_of(it.dom, id, "style").unwrap_or_default();
    if key == "cssText" {
        return Ok(str_value(&style));
    }
    let prop = camel_to_kebab(key);
    for decl in style.split(';') {
        if let Some((p, v)) = decl.split_once(':') {
            if p.trim().eq_ignore_ascii_case(&prop) {
                return Ok(str_value(v.trim()));
            }
        }
    }
    Ok(str_value(""))
}

pub fn style_set(it: &mut Interp, id: usize, key: &str, value: Value) -> Result<(), Control> {
    let v = to_string(&value);
    if key == "cssText" {
        it.dom.set_attr(id, "style", &v);
        return Ok(());
    }
    let prop = camel_to_kebab(key);
    let style = attr_of(it.dom, id, "style").unwrap_or_default();
    let mut out = String::new();
    for decl in style.split(';') {
        let Some((p, val)) = decl.split_once(':') else {
            continue;
        };
        if p.trim().eq_ignore_ascii_case(&prop) {
            continue; // replaced below
        }
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(p.trim());
        out.push_str(": ");
        out.push_str(val.trim());
    }
    if !v.is_empty() {
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(&prop);
        out.push_str(": ");
        out.push_str(&v);
    }
    it.dom.set_attr(id, "style", &out);
    Ok(())
}

fn intern_classlist(m: &str) -> &'static str {
    match m {
        "add" => "add",
        "remove" => "remove",
        "toggle" => "toggle",
        _ => "contains",
    }
}

fn classlist_fn(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let Value::Obj(o) = this else {
        return Ok(Value::Undefined);
    };
    let id = match o.borrow().props.get("__node") {
        Some(Value::Num(n)) => *n as usize,
        _ => return Ok(Value::Undefined),
    };
    let class = to_string(&args.first().cloned().unwrap_or(Value::Undefined));
    let current = attr_of(it.dom, id, "class").unwrap_or_default();
    let mut classes: Vec<&str> = current.split_ascii_whitespace().collect();
    let present = classes.contains(&class.as_str());
    let result = match name {
        "contains" => return Ok(Value::Bool(present)),
        "add" => {
            if !present {
                classes.push(&class);
            }
            Value::Undefined
        }
        "remove" => {
            classes.retain(|c| *c != class);
            Value::Undefined
        }
        _ => {
            // toggle
            if present {
                classes.retain(|c| *c != class);
            } else {
                classes.push(&class);
            }
            Value::Bool(!present)
        }
    };
    let joined = classes.join(" ");
    it.dom.set_attr(id, "class", &joined);
    Ok(result)
}

// ── Shared helpers ──────────────────────────────────────────────────────────

fn attr_of(dom: &Dom, id: usize, name: &str) -> Option<String> {
    dom.element(id)
        .and_then(|e| e.attr(name))
        .map(String::from)
}

fn has_class(dom: &Dom, id: usize, class: &str) -> bool {
    dom.element(id)
        .and_then(|e| e.attr("class"))
        .is_some_and(|cs| cs.split_ascii_whitespace().any(|c| c == class))
}

/// First node under `root` (tree order) matching the predicate.
fn find_by(dom: &Dom, root: usize, pred: &mut dyn FnMut(&Dom, usize) -> bool) -> Option<usize> {
    let mut stack = alloc::vec![root];
    while let Some(id) = stack.pop() {
        if id != root && pred(dom, id) {
            return Some(id);
        }
        for &c in dom.nodes[id].children.iter().rev() {
            stack.push(c);
        }
    }
    None
}

fn collect_by(dom: &Dom, root: usize, pred: &mut dyn FnMut(&Dom, usize) -> bool) -> Vec<Value> {
    let mut out = Vec::new();
    let mut stack = alloc::vec![root];
    while let Some(id) = stack.pop() {
        if id != root && dom.element(id).is_some() && pred(dom, id) {
            out.push(Value::Node(id));
        }
        for &c in dom.nodes[id].children.iter().rev() {
            stack.push(c);
        }
    }
    out
}

/// Run a selector against every element under `root` in tree order.
fn query_selector(it: &Interp, root: usize, selector: &str, first_only: bool) -> Vec<usize> {
    let sels = parse_selector_list(selector);
    if sels.is_empty() {
        return Vec::new();
    }
    let dom = &it.dom;
    let mut out = Vec::new();
    let mut stack = alloc::vec![root];
    while let Some(id) = stack.pop() {
        if id != root && dom.element(id).is_some() && sels.iter().any(|s| s.matches(dom, id)) {
            out.push(id);
            if first_only {
                return out;
            }
        }
        for &c in dom.nodes[id].children.iter().rev() {
            stack.push(c);
        }
    }
    out
}

fn sibling(dom: &Dom, id: usize, dir: i32) -> Value {
    let parent = dom.nodes[id].parent;
    if parent == usize::MAX {
        return Value::Null;
    }
    let siblings = &dom.nodes[parent].children;
    let Some(pos) = siblings.iter().position(|&c| c == id) else {
        return Value::Null;
    };
    let mut i = pos as i64 + dir as i64;
    while i >= 0 && (i as usize) < siblings.len() {
        let c = siblings[i as usize];
        if dom.element(c).is_some() {
            return Value::Node(c);
        }
        i += dir as i64;
    }
    Value::Null
}

fn camel_to_kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn kebab_to_camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

// ── Serialisation (innerHTML / outerHTML getters) ───────────────────────────

fn serialize_children(dom: &Dom, id: usize, out: &mut String, depth: u32) {
    if depth > 200 {
        return;
    }
    for &c in &dom.nodes[id].children {
        serialize_node(dom, c, out, depth + 1);
    }
}

fn serialize_node(dom: &Dom, id: usize, out: &mut String, depth: u32) {
    match &dom.nodes[id].data {
        NodeData::Document => serialize_children(dom, id, out, depth),
        NodeData::Text(t) => {
            for c in t.chars() {
                match c {
                    '&' => out.push_str("&amp;"),
                    '<' => out.push_str("&lt;"),
                    '>' => out.push_str("&gt;"),
                    c => out.push(c),
                }
            }
        }
        NodeData::Element(el) => {
            out.push('<');
            out.push_str(el.tag());
            for (n, v) in &el.attrs {
                out.push(' ');
                out.push_str(n);
                out.push_str("=\"");
                for c in v.chars() {
                    match c {
                        '&' => out.push_str("&amp;"),
                        '"' => out.push_str("&quot;"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            out.push('>');
            if !is_void(el.tag()) {
                serialize_children(dom, id, out, depth);
                out.push_str("</");
                out.push_str(el.tag());
                out.push('>');
            }
        }
    }
}
