//! Synchronous networking for scripts: `XMLHttpRequest` and `fetch`.
//!
//! The engine has no async runtime, so both are synchronous: `xhr.send()`
//! blocks and fills `status`/`responseText`, and `fetch()` returns an
//! already-resolved thenable whose `.then()` callbacks run immediately. The
//! actual transfer is delegated to a [`NetFetch`] hook the browser installs
//! (so this module — and the host engine tests — stay free of the network
//! code). The underlying HTTP stack is GET-only; other methods still fetch.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::cookie::SharedJar;
use super::interp::{EResult, Interp};
use super::value::*;

/// A request handed to the browser's network hook. `method`, `body`, and
/// `headers` are captured for fidelity but the current GET-only HTTP stack
/// does not forward them (a mock hook in the engine tests does read them).
pub struct NetRequest {
    #[allow(dead_code)]
    pub method: String,
    /// URL as written by the script (may be relative to `base_url`).
    pub url: String,
    /// The page URL, for resolving a relative `url`.
    pub base_url: String,
    #[allow(dead_code)]
    pub body: Option<String>,
    #[allow(dead_code)]
    pub headers: Vec<(String, String)>,
}

/// The hook's response.
pub struct NetResponse {
    pub status: u16,
    pub ok: bool,
    pub body: String,
    pub final_url: String,
}

/// Browser-installed synchronous fetch: resolves `req` (using `jar` for
/// cookies) and returns the response. Absent in the host tests.
pub type NetFetch = fn(&NetRequest, &SharedJar) -> NetResponse;

/// Run the request through the installed hook, or return a status-0 failure
/// when no network is available (e.g. host tests, `about:` pages).
fn perform(it: &Interp, req: &NetRequest) -> NetResponse {
    match it.net {
        Some(f) => f(req, &it.cookies),
        None => NetResponse {
            status: 0,
            ok: false,
            body: String::new(),
            final_url: req.url.clone(),
        },
    }
}

// ── XMLHttpRequest ────────────────────────────────────────────────────────────

const XHR_METHODS: &[&str] = &[
    "open",
    "send",
    "setRequestHeader",
    "getAllResponseHeaders",
    "getResponseHeader",
    "abort",
    "addEventListener",
];

/// `new XMLHttpRequest()` — a plain object pre-seeded with state and methods.
pub fn xhr_new(_it: &mut Interp, _this: &Value, _args: &[Value], _name: &'static str) -> EResult {
    let o = new_plain();
    {
        let mut b = o.borrow_mut();
        b.props.insert("readyState".into(), Value::Num(0.0));
        b.props.insert("status".into(), Value::Num(0.0));
        b.props.insert("statusText".into(), str_value(""));
        b.props.insert("responseText".into(), str_value(""));
        b.props.insert("responseURL".into(), str_value(""));
        b.props.insert("_method".into(), str_value("GET"));
        b.props.insert("_url".into(), str_value(""));
        b.props.insert("_headers".into(), new_array(Vec::new()));
        for m in XHR_METHODS {
            b.props.insert((*m).into(), native(xhr_method, m));
        }
    }
    Ok(Value::Obj(o))
}

fn xhr_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let Value::Obj(o) = this else {
        return Ok(Value::Undefined);
    };
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    match name {
        "open" => {
            let mut b = o.borrow_mut();
            b.props.insert(
                "_method".into(),
                str_value(&to_string(&arg(0)).to_uppercase()),
            );
            b.props
                .insert("_url".into(), str_value(&to_string(&arg(1))));
            b.props.insert("readyState".into(), Value::Num(1.0));
            Ok(Value::Undefined)
        }
        "setRequestHeader" => {
            if let Some(Value::Obj(arr)) = o.borrow().props.get("_headers") {
                if let ObjKind::Array(items) = &mut arr.borrow_mut().kind {
                    items.push(new_array(alloc::vec![arg(0), arg(1)]));
                }
            }
            Ok(Value::Undefined)
        }
        "send" => xhr_send(it, o, arg(0)),
        "getResponseHeader" | "getAllResponseHeaders" => Ok(Value::Null),
        "addEventListener" => {
            // Map `load`/`error`/`readystatechange` to the on* property slots.
            let ev = to_string(&arg(0));
            let prop = match ev.as_str() {
                "load" => "onload",
                "error" => "onerror",
                "readystatechange" => "onreadystatechange",
                _ => return Ok(Value::Undefined),
            };
            o.borrow_mut().props.insert(prop.into(), arg(1));
            Ok(Value::Undefined)
        }
        _ => Ok(Value::Undefined),
    }
}

fn xhr_send(it: &mut Interp, o: &Obj, body: Value) -> EResult {
    let (method, url, headers) = {
        let b = o.borrow();
        let method = b.props.get("_method").map(to_string).unwrap_or_default();
        let url = b.props.get("_url").map(to_string).unwrap_or_default();
        let headers = match b.props.get("_headers") {
            Some(Value::Obj(arr)) => header_pairs(&arr.borrow().kind),
            _ => Vec::new(),
        };
        (method, url, headers)
    };
    let body = match body {
        Value::Undefined | Value::Null => None,
        v => Some(to_string(&v)),
    };
    let resp = perform(
        it,
        &NetRequest {
            method,
            url,
            base_url: it.base_url.to_string(),
            body,
            headers,
        },
    );

    {
        let mut b = o.borrow_mut();
        b.props
            .insert("status".into(), Value::Num(resp.status as f64));
        b.props.insert(
            "statusText".into(),
            str_value(if resp.ok { "OK" } else { "" }),
        );
        b.props.insert("responseText".into(), str_value(&resp.body));
        b.props.insert("response".into(), str_value(&resp.body));
        b.props
            .insert("responseURL".into(), str_value(&resp.final_url));
        b.props.insert("readyState".into(), Value::Num(4.0));
    }

    // Fire handlers synchronously, in the usual order, with `this` = the XHR.
    let this_v = Value::Obj(o.clone());
    for prop in ["onreadystatechange", "onload"] {
        let handler = o.borrow().props.get(prop).cloned();
        if let Some(h) = handler.filter(is_callable) {
            it.call(&h, &this_v, &[])?;
        }
    }
    if resp.status == 0 {
        let handler = o.borrow().props.get("onerror").cloned();
        if let Some(h) = handler.filter(is_callable) {
            it.call(&h, &this_v, &[])?;
        }
    }
    Ok(Value::Undefined)
}

fn header_pairs(kind: &ObjKind) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let ObjKind::Array(items) = kind {
        for it in items {
            if let Value::Obj(p) = it {
                if let ObjKind::Array(kv) = &p.borrow().kind {
                    out.push((
                        to_string(kv.first().unwrap_or(&Value::Undefined)),
                        to_string(kv.get(1).unwrap_or(&Value::Undefined)),
                    ));
                }
            }
        }
    }
    out
}

// ── fetch ─────────────────────────────────────────────────────────────────────

/// `fetch(url, options?)` — performs the request synchronously and returns an
/// already-resolved promise of a Response.
pub fn fetch_fn(it: &mut Interp, _this: &Value, args: &[Value], _name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    let url = to_string(&arg(0));
    let opts = arg(1);
    let (method, headers, body) = parse_fetch_options(&opts);
    let resp = perform(
        it,
        &NetRequest {
            method,
            url,
            base_url: it.base_url.to_string(),
            body,
            headers,
        },
    );
    Ok(resolved_promise(make_response(&resp)))
}

fn parse_fetch_options(opts: &Value) -> (String, Vec<(String, String)>, Option<String>) {
    let mut method = String::from("GET");
    let mut headers = Vec::new();
    let mut body = None;
    if let Value::Obj(o) = opts {
        let b = o.borrow();
        if let Some(m) = b.props.get("method") {
            method = to_string(m).to_uppercase();
        }
        if let Some(bv) = b
            .props
            .get("body")
            .filter(|v| !matches!(v, Value::Undefined | Value::Null))
        {
            body = Some(to_string(bv));
        }
        if let Some(Value::Obj(h)) = b.props.get("headers") {
            for (k, v) in &h.borrow().props {
                headers.push((k.clone(), to_string(v)));
            }
        }
    }
    (method, headers, body)
}

/// Build a `Response`-like object with `text()`/`json()` and status fields.
fn make_response(resp: &NetResponse) -> Value {
    let o = new_plain();
    {
        let mut b = o.borrow_mut();
        b.props
            .insert("status".into(), Value::Num(resp.status as f64));
        b.props.insert("ok".into(), Value::Bool(resp.ok));
        b.props.insert(
            "statusText".into(),
            str_value(if resp.ok { "OK" } else { "" }),
        );
        b.props.insert("url".into(), str_value(&resp.final_url));
        b.props.insert("_body".into(), str_value(&resp.body));
        b.props
            .insert("text".into(), native(response_method, "text"));
        b.props
            .insert("json".into(), native(response_method, "json"));
    }
    Value::Obj(o)
}

fn response_method(it: &mut Interp, this: &Value, _args: &[Value], name: &'static str) -> EResult {
    let body = match this {
        Value::Obj(o) => o
            .borrow()
            .props
            .get("_body")
            .map(to_string)
            .unwrap_or_default(),
        _ => String::new(),
    };
    let _ = it;
    let value = if name == "json" {
        super::builtins::parse_json(&body)?
    } else {
        str_value(&body)
    };
    Ok(resolved_promise(value))
}

// ── Minimal synchronous promise ────────────────────────────────────────────────

/// A pre-resolved thenable: `.then(cb)` runs `cb(value)` immediately and
/// returns a promise of the result, so `fetch(...).then(r => r.json())
/// .then(data => …)` chains work without an event loop.
pub fn resolved_promise(value: Value) -> Value {
    let o = new_plain();
    {
        let mut b = o.borrow_mut();
        b.props.insert("_value".into(), value);
        b.props.insert("_isPromise".into(), Value::Bool(true));
        b.props
            .insert("then".into(), native(promise_method, "then"));
        b.props
            .insert("catch".into(), native(promise_method, "catch"));
        b.props
            .insert("finally".into(), native(promise_method, "finally"));
    }
    Value::Obj(o)
}

fn promise_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    let value = match this {
        Value::Obj(o) => o
            .borrow()
            .props
            .get("_value")
            .cloned()
            .unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    };
    match name {
        "then" => {
            let cb = arg(0);
            if is_callable(&cb) {
                let r = it.call(&cb, &Value::Undefined, &[value])?;
                // A handler returning a promise flattens into the chain.
                Ok(if is_promise(&r) {
                    r
                } else {
                    resolved_promise(r)
                })
            } else {
                Ok(this.clone())
            }
        }
        "catch" => Ok(this.clone()), // never rejected
        "finally" => {
            let cb = arg(0);
            if is_callable(&cb) {
                it.call(&cb, &Value::Undefined, &[])?;
            }
            Ok(this.clone())
        }
        _ => Ok(Value::Undefined),
    }
}

fn is_promise(v: &Value) -> bool {
    matches!(v, Value::Obj(o) if o.borrow().props.contains_key("_isPromise"))
}

fn is_callable(v: &Value) -> bool {
    matches!(v, Value::Obj(o)
        if matches!(o.borrow().kind, ObjKind::Function { .. } | ObjKind::Native(..) | ObjKind::Bound { .. }))
}
