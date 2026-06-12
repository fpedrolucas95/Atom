//! JavaScript standard library subset.
//!
//! Installs the globals page scripts expect — `console`, `Math`, `JSON`,
//! `parseInt`/`parseFloat`, `isNaN`, the `Object`/`Array`/`String`/`Number`/
//! `Boolean`/`Error`/`Date` constructors, `window` (the global object), and
//! `setTimeout`/`alert` stubs — plus the per-type method dispatchers the
//! interpreter routes property access through (string, array, number,
//! function, and plain-object methods).
//!
//! Methods are stateless natives dispatched by name: `"abc".slice` returns a
//! native carrying `"slice"`, and the call supplies the string as `this`.
//! This avoids materialising prototype objects for every primitive.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core::sync::atomic::{AtomicU64, Ordering};

use super::interp::{EResult, Interp};
use super::value::*;

// ── Global installation ──────────────────────────────────────────────────────

pub fn install_globals(global: &EnvRef) {
    let mut g = global.borrow_mut();
    let mut def = |name: &str, v: Value| {
        g.vars.insert(name.into(), v);
    };

    def("undefined", Value::Undefined);
    def("NaN", Value::Num(f64::NAN));
    def("Infinity", Value::Num(f64::INFINITY));

    def("parseInt", native(global_fn, "parseInt"));
    def("parseFloat", native(global_fn, "parseFloat"));
    def("isNaN", native(global_fn, "isNaN"));
    def("isFinite", native(global_fn, "isFinite"));
    def("encodeURIComponent", native(global_fn, "encodeURIComponent"));
    def("decodeURIComponent", native(global_fn, "decodeURIComponent"));
    def("encodeURI", native(global_fn, "encodeURIComponent"));
    def("decodeURI", native(global_fn, "decodeURIComponent"));
    def("alert", native(global_fn, "alert"));
    def("setTimeout", native(global_fn, "setTimeout"));
    def("setInterval", native(global_fn, "setInterval"));
    def("clearTimeout", native(global_fn, "noop"));
    def("clearInterval", native(global_fn, "noop"));
    def("eval", native(global_fn, "noop"));
    def("String", native(global_fn, "String"));
    def("Number", native(global_fn, "Number"));
    def("Boolean", native(global_fn, "Boolean"));
    def("Array", native(global_fn, "Array"));
    def("Error", native(global_fn, "Error"));
    def("TypeError", native(global_fn, "TypeError"));
    def("RangeError", native(global_fn, "RangeError"));
    def("Date", native(global_fn, "Date"));
    def("RegExp", native(global_fn, "RegExp"));

    // console.{log,warn,error,info,debug}
    let console = new_plain();
    for m in ["log", "warn", "error", "info", "debug"] {
        console
            .borrow_mut()
            .props
            .insert(m.into(), native(console_fn, leak_name("console.", m)));
    }
    def("console", Value::Obj(console));

    // Math
    let math = new_plain();
    {
        let mut mb = math.borrow_mut();
        for m in [
            "abs", "floor", "ceil", "round", "trunc", "sqrt", "pow", "min", "max", "random",
            "sign", "cbrt", "hypot", "log", "exp", "sin", "cos", "tan", "atan", "atan2",
        ] {
            mb.props.insert(m.into(), native(math_fn, leak_name("Math.", m)));
        }
        mb.props.insert("PI".into(), Value::Num(core::f64::consts::PI));
        mb.props.insert("E".into(), Value::Num(core::f64::consts::E));
        mb.props.insert("LN2".into(), Value::Num(core::f64::consts::LN_2));
        mb.props.insert("LN10".into(), Value::Num(core::f64::consts::LN_10));
        mb.props.insert("SQRT2".into(), Value::Num(core::f64::consts::SQRT_2));
    }
    def("Math", Value::Obj(math));

    // JSON
    let json = new_plain();
    json.borrow_mut()
        .props
        .insert("parse".into(), native(json_fn, "parse"));
    json.borrow_mut()
        .props
        .insert("stringify".into(), native(json_fn, "stringify"));
    def("JSON", Value::Obj(json));

    // Object (constructor + statics)
    let object = new_obj(ObjKind::Native(global_fn, "Object"));
    for m in ["keys", "values", "assign", "freeze", "create"] {
        object
            .borrow_mut()
            .props
            .insert(m.into(), native(object_static_fn, leak_name("Object.", m)));
    }
    def("Object", Value::Obj(object));

    // Array.isArray
    let array = new_obj(ObjKind::Native(global_fn, "Array"));
    array
        .borrow_mut()
        .props
        .insert("isArray".into(), native(object_static_fn, "Array.isArray"));
    def("Array", Value::Obj(array));

    // Date.now
    let date = new_obj(ObjKind::Native(global_fn, "Date"));
    date.borrow_mut()
        .props
        .insert("now".into(), native(global_fn, "Date.now"));
    def("Date", Value::Obj(date));

    // window / globalThis / self: one shared object; `location`, `navigator`
    // and `screen` are minimal stubs.
    let window = new_plain();
    {
        let mut w = window.borrow_mut();
        let location = new_plain();
        location
            .borrow_mut()
            .props
            .insert("href".into(), str_value(""));
        w.props.insert("location".into(), Value::Obj(location.clone()));
        let navigator = new_plain();
        navigator
            .borrow_mut()
            .props
            .insert("userAgent".into(), str_value("AtomBrowser/0.1 (AtomOS)"));
        w.props.insert("navigator".into(), Value::Obj(navigator));
        w.props
            .insert("innerWidth".into(), Value::Num(crate::css::VIEWPORT_W as f64));
        w.props
            .insert("innerHeight".into(), Value::Num(crate::css::VIEWPORT_H as f64));
        w.props
            .insert("addEventListener".into(), native(window_listen_fn, "add"));
        w.props
            .insert("removeEventListener".into(), native(window_listen_fn, "remove"));
        w.props.insert("document".into(), Value::Document);
        def("location", Value::Obj(location));
    }
    def("window", Value::Obj(window.clone()));
    def("globalThis", Value::Obj(window.clone()));
    def("self", Value::Obj(window));
    def("document", Value::Document);
}

/// Method-name interning for the handful of `family.method` names we build at
/// startup. Leaked once per process; the set is small and static.
fn leak_name(prefix: &str, name: &str) -> &'static str {
    let s = format!("{prefix}{name}");
    alloc::boxed::Box::leak(s.into_boxed_str())
}

/// `window.addEventListener` / `removeEventListener`.
fn window_listen_fn(it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let ty = to_string(&args.first().cloned().unwrap_or(Value::Undefined));
    let f = args.get(1).cloned().unwrap_or(Value::Undefined);
    use super::events::Target;
    if name == "add" {
        it.handlers.add_listener(Target::Window, &ty, f);
    } else {
        it.handlers.remove_listener(Target::Window, &ty, &f);
    }
    Ok(Value::Undefined)
}

// ── Global functions ─────────────────────────────────────────────────────────

fn global_fn(it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    Ok(match name {
        "parseInt" => {
            let s = to_string(&arg(0));
            let t = s.trim();
            let radix = match to_number(&arg(1)) {
                r if (2.0..=36.0).contains(&r) => r as u32,
                _ => {
                    if t.starts_with("0x") || t.starts_with("0X") {
                        16
                    } else {
                        10
                    }
                }
            };
            let t = if radix == 16 {
                t.trim_start_matches("0x").trim_start_matches("0X")
            } else {
                t
            };
            let (neg, t) = match t.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let mut value: f64 = f64::NAN;
            let mut acc = 0f64;
            let mut any = false;
            for c in t.chars() {
                match c.to_digit(radix) {
                    Some(d) => {
                        acc = acc * radix as f64 + d as f64;
                        any = true;
                    }
                    None => break,
                }
            }
            if any {
                value = if neg { -acc } else { acc };
            }
            Value::Num(value)
        }
        "parseFloat" => {
            let s = to_string(&arg(0));
            let t = s.trim();
            // Longest numeric prefix.
            let mut end = 0;
            let bytes = t.as_bytes();
            let mut seen_dot = false;
            let mut seen_e = false;
            for (i, &b) in bytes.iter().enumerate() {
                let ok = b.is_ascii_digit()
                    || (b == b'.' && !seen_dot && !seen_e)
                    || ((b == b'e' || b == b'E') && !seen_e && i > 0)
                    || ((b == b'+' || b == b'-')
                        && (i == 0 || bytes[i - 1] == b'e' || bytes[i - 1] == b'E'));
                if !ok {
                    break;
                }
                if b == b'.' {
                    seen_dot = true;
                }
                if b == b'e' || b == b'E' {
                    seen_e = true;
                }
                end = i + 1;
            }
            // Trim an incomplete exponent ("2.5e" from "2.5em") before parsing.
            let mut prefix = &t[..end];
            while prefix
                .as_bytes()
                .last()
                .is_some_and(|b| matches!(b, b'e' | b'E' | b'+' | b'-' | b'.'))
            {
                prefix = &prefix[..prefix.len() - 1];
            }
            Value::Num(prefix.parse::<f64>().unwrap_or(f64::NAN))
        }
        "isNaN" => Value::Bool(to_number(&arg(0)).is_nan()),
        "isFinite" => Value::Bool(to_number(&arg(0)).is_finite()),
        "encodeURIComponent" => str_value(&crate::text::percent_encode(&to_string(&arg(0)))),
        "decodeURIComponent" => {
            let bytes = crate::text::percent_decode(&to_string(&arg(0)));
            str_value(&String::from_utf8_lossy(&bytes))
        }
        "alert" => {
            let msg = to_string(&arg(0));
            it.console.push(format!("[alert] {msg}"));
            Value::Undefined
        }
        "setTimeout" | "setInterval" => {
            // No event loop: a zero-delay setTimeout runs inline once; other
            // delays (and all intervals) are dropped.
            if name == "setTimeout" && to_number(&arg(1)) == 0.0 {
                if let f @ Value::Obj(_) = arg(0) {
                    it.call(&f, &Value::Undefined, &[])?;
                }
            }
            Value::Num(0.0)
        }
        "String" => str_value(&to_string(&arg(0))),
        "Number" => Value::Num(to_number(&arg(0))),
        "Boolean" => Value::Bool(truthy(&arg(0))),
        "Array" => {
            // `Array(n)` makes a hole-filled array; `Array(a, b)` lists.
            if args.len() == 1 {
                if let Value::Num(n) = args[0] {
                    let len = if n.is_finite() && n >= 0.0 {
                        (n as usize).min(1 << 20)
                    } else {
                        0
                    };
                    return Ok(new_array(alloc::vec![Value::Undefined; len]));
                }
            }
            new_array(args.to_vec())
        }
        "Error" | "TypeError" | "RangeError" => make_error(name, &to_string(&arg(0))),
        "Date" => {
            // No wall clock yet: a stub fixed at the epoch.
            let o = new_plain();
            for m in ["getTime", "getFullYear", "getMonth", "getDate", "getHours",
                      "getMinutes", "getSeconds", "getDay", "toISOString"] {
                o.borrow_mut()
                    .props
                    .insert((*m).into(), native(date_fn, leak_name("Date#", m)));
            }
            Value::Obj(o)
        }
        "Date.now" => Value::Num(0.0),
        "RegExp" => {
            let o = new_plain();
            o.borrow_mut()
                .props
                .insert("source".into(), str_value(&to_string(&arg(0))));
            Value::Obj(o)
        }
        _ => Value::Undefined, // "noop"
    })
}

fn date_fn(_it: &mut Interp, _this: &Value, _args: &[Value], name: &'static str) -> EResult {
    Ok(match name {
        "Date#getFullYear" => Value::Num(1970.0),
        "Date#getDay" | "Date#getDate" => Value::Num(if name.ends_with("Date") { 1.0 } else { 4.0 }),
        "Date#toISOString" => str_value("1970-01-01T00:00:00.000Z"),
        _ => Value::Num(0.0),
    })
}

fn console_fn(it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let level = name.rsplit('.').next().unwrap_or("log");
    let mut line = String::new();
    if level != "log" {
        line.push('[');
        line.push_str(level);
        line.push_str("] ");
    }
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            line.push(' ');
        }
        line.push_str(&inspect(a));
    }
    it.console.push(line);
    Ok(Value::Undefined)
}

// ── Math ─────────────────────────────────────────────────────────────────────

static RNG_STATE: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn math_fn(_it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let n = |i: usize| to_number(&args.get(i).cloned().unwrap_or(Value::Undefined));
    let m = name.rsplit('.').next().unwrap_or(name);
    Ok(Value::Num(match m {
        "abs" => f_abs(n(0)),
        "floor" => f_floor(n(0)),
        "ceil" => f_ceil(n(0)),
        "round" => f_round(n(0)),
        "trunc" => f_trunc(n(0)),
        "sqrt" => f_sqrt(n(0)),
        "cbrt" => {
            // Newton iteration for the cube root.
            let x = n(0);
            if x == 0.0 {
                0.0
            } else {
                let neg = x < 0.0;
                let a = f_abs(x);
                let mut g = if a > 1.0 { a / 3.0 } else { a };
                for _ in 0..40 {
                    g = (2.0 * g + a / (g * g)) / 3.0;
                }
                if neg {
                    -g
                } else {
                    g
                }
            }
        }
        "pow" => f_pow(n(0), n(1)),
        "hypot" => f_sqrt(n(0) * n(0) + n(1) * n(1)),
        "min" => {
            let mut best = f64::INFINITY;
            for a in args {
                let v = to_number(a);
                if v.is_nan() {
                    return Ok(Value::Num(f64::NAN));
                }
                if v < best {
                    best = v;
                }
            }
            best
        }
        "max" => {
            let mut best = f64::NEG_INFINITY;
            for a in args {
                let v = to_number(a);
                if v.is_nan() {
                    return Ok(Value::Num(f64::NAN));
                }
                if v > best {
                    best = v;
                }
            }
            best
        }
        "sign" => {
            let v = n(0);
            if v.is_nan() {
                f64::NAN
            } else if v > 0.0 {
                1.0
            } else if v < 0.0 {
                -1.0
            } else {
                v
            }
        }
        "random" => {
            // xorshift64*; good enough for page scripts, no clock needed.
            let mut s = RNG_STATE.load(Ordering::Relaxed);
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            RNG_STATE.store(s, Ordering::Relaxed);
            let bits = s.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
            bits as f64 / (1u64 << 53) as f64
        }
        // Transcendentals: low-precision series adequate for layout scripts.
        "log" => poor_ln(n(0)),
        "exp" => poor_exp(n(0)),
        "sin" => poor_sin(n(0)),
        "cos" => poor_sin(core::f64::consts::FRAC_PI_2 - n(0)),
        "tan" => poor_sin(n(0)) / poor_sin(core::f64::consts::FRAC_PI_2 - n(0)),
        "atan" => poor_atan(n(0)),
        "atan2" => {
            let (y, x) = (n(0), n(1));
            if x > 0.0 {
                poor_atan(y / x)
            } else if x < 0.0 && y >= 0.0 {
                poor_atan(y / x) + core::f64::consts::PI
            } else if x < 0.0 {
                poor_atan(y / x) - core::f64::consts::PI
            } else if y > 0.0 {
                core::f64::consts::FRAC_PI_2
            } else if y < 0.0 {
                -core::f64::consts::FRAC_PI_2
            } else {
                0.0
            }
        }
        _ => f64::NAN,
    }))
}

/// Natural log via `ln(m·2^e) = e·ln2 + ln(m)`, atanh series on the mantissa.
fn poor_ln(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }
    if x.is_infinite() {
        return x;
    }
    let bits = x.to_bits();
    let e = ((bits >> 52) & 0x7FF) as i64 - 1023;
    let m = f64::from_bits((bits & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000);
    // ln(m) = 2·atanh((m-1)/(m+1)), m ∈ [1,2)
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    let mut term = t;
    let mut sum = 0.0;
    for k in 0..20 {
        sum += term / (2 * k + 1) as f64;
        term *= t2;
    }
    e as f64 * core::f64::consts::LN_2 + 2.0 * sum
}

/// exp via argument reduction `e^x = 2^k · e^r` and a Taylor series.
fn poor_exp(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if x > 709.0 {
        return f64::INFINITY;
    }
    if x < -709.0 {
        return 0.0;
    }
    let k = f_round(x / core::f64::consts::LN_2);
    let r = x - k * core::f64::consts::LN_2;
    let mut term = 1.0;
    let mut sum = 1.0;
    for i in 1..20 {
        term *= r / i as f64;
        sum += term;
    }
    // 2^k by exponent assembly.
    let two_k = f64::from_bits((((k as i64) + 1023).clamp(1, 2046) as u64) << 52);
    sum * two_k
}

fn poor_sin(x: f64) -> f64 {
    if !x.is_finite() {
        return f64::NAN;
    }
    // Reduce to [-π, π].
    let tau = core::f64::consts::TAU;
    let mut r = x % tau;
    if r > core::f64::consts::PI {
        r -= tau;
    }
    if r < -core::f64::consts::PI {
        r += tau;
    }
    let r2 = r * r;
    let mut term = r;
    let mut sum = r;
    for k in 1..12 {
        term *= -r2 / ((2 * k) as f64 * (2 * k + 1) as f64);
        sum += term;
    }
    sum
}

fn poor_atan(x: f64) -> f64 {
    if x.is_nan() {
        return f64::NAN;
    }
    if f_abs(x) > 1.0 {
        let half_pi = core::f64::consts::FRAC_PI_2;
        return if x > 0.0 {
            half_pi - poor_atan(1.0 / x)
        } else {
            -half_pi - poor_atan(1.0 / x)
        };
    }
    // atan(x) = x / (1 + x²/(3 + …)) — use the series with Euler transform.
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    for k in 1..40 {
        term *= -x2;
        sum += term / (2 * k + 1) as f64;
    }
    sum
}

// ── Object / Array statics ──────────────────────────────────────────────────

fn object_static_fn(_it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    Ok(match name {
        "Object.keys" | "Object.values" => {
            let mut out = Vec::new();
            if let Value::Obj(o) = arg(0) {
                let b = o.borrow();
                match &b.kind {
                    ObjKind::Array(items) => {
                        for (i, v) in items.iter().enumerate() {
                            out.push(if name.ends_with("keys") {
                                str_value(&i.to_string())
                            } else {
                                v.clone()
                            });
                        }
                    }
                    _ => {
                        for (k, v) in b.props.iter() {
                            out.push(if name.ends_with("keys") {
                                str_value(k)
                            } else {
                                v.clone()
                            });
                        }
                    }
                }
            }
            new_array(out)
        }
        "Object.assign" => {
            let target = arg(0);
            if let Value::Obj(t) = &target {
                for src in &args[1.min(args.len())..] {
                    if let Value::Obj(s) = src {
                        let entries: Vec<(String, Value)> = s
                            .borrow()
                            .props
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        for (k, v) in entries {
                            t.borrow_mut().props.insert(k, v);
                        }
                    }
                }
            }
            target
        }
        "Object.freeze" => arg(0), // no-op: we don't enforce immutability
        "Object.create" => {
            let o = new_plain();
            if let Value::Obj(p) = arg(0) {
                o.borrow_mut().proto = Some(p);
            }
            Value::Obj(o)
        }
        "Array.isArray" => Value::Bool(matches!(
            &arg(0),
            Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_))
        )),
        _ => Value::Undefined,
    })
}

// ── JSON ─────────────────────────────────────────────────────────────────────

fn json_fn(it: &mut Interp, _this: &Value, args: &[Value], name: &'static str) -> EResult {
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    match name {
        "parse" => {
            let s = to_string(&arg(0));
            let mut pos = 0;
            let v = json_parse(&s, &mut pos)
                .ok_or_else(|| Interp::throw_str("SyntaxError", "invalid JSON"))?;
            Ok(v)
        }
        "stringify" => {
            let mut out = String::new();
            json_stringify(it, &arg(0), &mut out, 0);
            if out.is_empty() {
                Ok(Value::Undefined)
            } else {
                Ok(str_value(&out))
            }
        }
        _ => Ok(Value::Undefined),
    }
}

fn json_skip_ws(s: &str, pos: &mut usize) {
    let b = s.as_bytes();
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn json_parse(s: &str, pos: &mut usize) -> Option<Value> {
    json_skip_ws(s, pos);
    let b = s.as_bytes();
    match b.get(*pos)? {
        b'{' => {
            *pos += 1;
            let o = new_plain();
            json_skip_ws(s, pos);
            if b.get(*pos) == Some(&b'}') {
                *pos += 1;
                return Some(Value::Obj(o));
            }
            loop {
                json_skip_ws(s, pos);
                let Value::Str(key) = json_parse(s, pos)? else {
                    return None;
                };
                json_skip_ws(s, pos);
                if b.get(*pos) != Some(&b':') {
                    return None;
                }
                *pos += 1;
                let v = json_parse(s, pos)?;
                o.borrow_mut().props.insert(key.to_string(), v);
                json_skip_ws(s, pos);
                match b.get(*pos) {
                    Some(b',') => *pos += 1,
                    Some(b'}') => {
                        *pos += 1;
                        return Some(Value::Obj(o));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *pos += 1;
            let mut items = Vec::new();
            json_skip_ws(s, pos);
            if b.get(*pos) == Some(&b']') {
                *pos += 1;
                return Some(new_array(items));
            }
            loop {
                items.push(json_parse(s, pos)?);
                json_skip_ws(s, pos);
                match b.get(*pos) {
                    Some(b',') => *pos += 1,
                    Some(b']') => {
                        *pos += 1;
                        return Some(new_array(items));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => {
            *pos += 1;
            let mut out = String::new();
            while *pos < b.len() {
                match b[*pos] {
                    b'"' => {
                        *pos += 1;
                        return Some(str_value(&out));
                    }
                    b'\\' => {
                        *pos += 1;
                        match b.get(*pos)? {
                            b'n' => out.push('\n'),
                            b't' => out.push('\t'),
                            b'r' => out.push('\r'),
                            b'b' => out.push('\u{8}'),
                            b'f' => out.push('\u{c}'),
                            b'/' => out.push('/'),
                            b'\\' => out.push('\\'),
                            b'"' => out.push('"'),
                            b'u' => {
                                let mut cp = 0u32;
                                for _ in 0..4 {
                                    *pos += 1;
                                    cp = cp * 16 + (*b.get(*pos)? as char).to_digit(16)? ;
                                }
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                            _ => return None,
                        }
                        *pos += 1;
                    }
                    _ => {
                        let c = s[*pos..].chars().next()?;
                        out.push(c);
                        *pos += c.len_utf8();
                    }
                }
            }
            None
        }
        b't' if s[*pos..].starts_with("true") => {
            *pos += 4;
            Some(Value::Bool(true))
        }
        b'f' if s[*pos..].starts_with("false") => {
            *pos += 5;
            Some(Value::Bool(false))
        }
        b'n' if s[*pos..].starts_with("null") => {
            *pos += 4;
            Some(Value::Null)
        }
        _ => {
            let start = *pos;
            if b.get(*pos) == Some(&b'-') {
                *pos += 1;
            }
            while *pos < b.len()
                && (b[*pos].is_ascii_digit()
                    || matches!(b[*pos], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                *pos += 1;
            }
            s[start..*pos].parse::<f64>().ok().map(Value::Num)
        }
    }
}

fn json_stringify(it: &mut Interp, v: &Value, out: &mut String, depth: u32) {
    if depth > 32 {
        out.push_str("null");
        return;
    }
    match v {
        Value::Null | Value::Undefined => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Num(n) => {
            if n.is_finite() {
                out.push_str(&num_to_string(*n));
            } else {
                out.push_str("null");
            }
        }
        Value::Str(s) => json_quote(s, out),
        Value::Obj(o) => {
            let is_fn = matches!(
                o.borrow().kind,
                ObjKind::Function { .. } | ObjKind::Native(..) | ObjKind::Bound { .. }
            );
            if is_fn {
                out.push_str("null");
                return;
            }
            let b = o.borrow();
            match &b.kind {
                ObjKind::Array(items) => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        json_stringify(it, item, out, depth + 1);
                    }
                    out.push(']');
                }
                _ => {
                    out.push('{');
                    for (i, (k, pv)) in b.props.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        json_quote(k, out);
                        out.push(':');
                        json_stringify(it, pv, out, depth + 1);
                    }
                    out.push('}');
                }
            }
        }
        _ => out.push_str("null"),
    }
}

fn json_quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ── Method dispatchers ───────────────────────────────────────────────────────

static STRING_METHODS: &[&str] = &[
    "charAt", "charCodeAt", "indexOf", "lastIndexOf", "includes", "startsWith", "endsWith",
    "slice", "substring", "substr", "toUpperCase", "toLowerCase", "trim", "split", "replace",
    "replaceAll", "repeat", "concat", "padStart", "padEnd", "toString", "valueOf", "at",
];

pub fn string_member(s: &Rc<str>, key: &str) -> EResult {
    if key == "length" {
        return Ok(Value::Num(s.chars().count() as f64));
    }
    if let Ok(i) = key.parse::<usize>() {
        return Ok(s
            .chars()
            .nth(i)
            .map(|c| str_value(&c.to_string()))
            .unwrap_or(Value::Undefined));
    }
    if let Some(m) = STRING_METHODS.iter().find(|m| **m == key) {
        return Ok(native(string_method, m));
    }
    Ok(Value::Undefined)
}

fn string_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let s = to_string(this);
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len() as i64;
    // Normalise a JS index (negative counts from the end).
    let norm = |v: f64, len: i64| -> i64 {
        if v.is_nan() {
            0
        } else {
            let v = v as i64;
            if v < 0 {
                (len + v).max(0)
            } else {
                v.min(len)
            }
        }
    };
    Ok(match name {
        "charAt" => {
            let i = to_number(&arg(0)) as i64;
            chars
                .get(i.max(0) as usize)
                .map(|c| str_value(&c.to_string()))
                .unwrap_or_else(|| str_value(""))
        }
        "at" => {
            let mut i = to_number(&arg(0)) as i64;
            if i < 0 {
                i += len;
            }
            chars
                .get(i.max(0) as usize)
                .map(|c| str_value(&c.to_string()))
                .unwrap_or(Value::Undefined)
        }
        "charCodeAt" => {
            let i = to_number(&arg(0)) as i64;
            chars
                .get(i.max(0) as usize)
                .map(|c| Value::Num(*c as u32 as f64))
                .unwrap_or(Value::Num(f64::NAN))
        }
        "indexOf" | "lastIndexOf" | "includes" | "startsWith" | "endsWith" => {
            let needle = to_string(&arg(0));
            match name {
                "indexOf" => Value::Num(
                    s.find(&needle)
                        .map(|b| s[..b].chars().count() as f64)
                        .unwrap_or(-1.0),
                ),
                "lastIndexOf" => Value::Num(
                    s.rfind(&needle)
                        .map(|b| s[..b].chars().count() as f64)
                        .unwrap_or(-1.0),
                ),
                "includes" => Value::Bool(s.contains(&needle)),
                "startsWith" => Value::Bool(s.starts_with(&needle)),
                _ => Value::Bool(s.ends_with(&needle)),
            }
        }
        "slice" | "substring" => {
            let mut a = norm(to_number(&arg(0)), len);
            let mut b = if matches!(arg(1), Value::Undefined) {
                len
            } else if name == "substring" {
                // substring clamps negatives to 0 rather than wrapping.
                (to_number(&arg(1)).max(0.0) as i64).min(len)
            } else {
                norm(to_number(&arg(1)), len)
            };
            if name == "substring" {
                a = a.max(0);
                if a > b {
                    core::mem::swap(&mut a, &mut b);
                }
            }
            let out: String = chars
                .get(a as usize..(b.max(a)) as usize)
                .unwrap_or(&[])
                .iter()
                .collect();
            str_value(&out)
        }
        "substr" => {
            let a = norm(to_number(&arg(0)), len);
            let n = if matches!(arg(1), Value::Undefined) {
                len - a
            } else {
                (to_number(&arg(1)).max(0.0)) as i64
            };
            let out: String = chars
                .iter()
                .skip(a as usize)
                .take(n.max(0) as usize)
                .collect();
            str_value(&out)
        }
        "toUpperCase" => str_value(&s.to_uppercase()),
        "toLowerCase" => str_value(&s.to_lowercase()),
        "trim" => str_value(s.trim()),
        "split" => {
            let sep = arg(0);
            let parts: Vec<Value> = match &sep {
                Value::Undefined => alloc::vec![str_value(&s)],
                _ => {
                    let sep = to_string(&sep);
                    if sep.is_empty() {
                        chars.iter().map(|c| str_value(&c.to_string())).collect()
                    } else {
                        s.split(sep.as_str()).map(str_value).collect()
                    }
                }
            };
            new_array(parts)
        }
        "replace" | "replaceAll" => {
            // String patterns only; regex stand-ins leave the string as-is.
            let pat = arg(0);
            let Value::Str(pat) = pat else {
                it.console
                    .push(String::from("[warn] regex replace is not supported"));
                return Ok(str_value(&s));
            };
            let with = to_string(&arg(1));
            if name == "replace" {
                str_value(&s.replacen(pat.as_ref(), &with, 1))
            } else {
                str_value(&s.replace(pat.as_ref(), &with))
            }
        }
        "repeat" => {
            let n = (to_number(&arg(0)).max(0.0) as usize).min(10_000);
            str_value(&s.repeat(n))
        }
        "concat" => {
            let mut out = s.clone();
            for a in args {
                out.push_str(&to_string(a));
            }
            str_value(&out)
        }
        "padStart" | "padEnd" => {
            let target = to_number(&arg(0)).max(0.0) as usize;
            let pad = match arg(1) {
                Value::Undefined => String::from(" "),
                v => to_string(&v),
            };
            let mut out = s.clone();
            if pad.is_empty() {
                return Ok(str_value(&out));
            }
            while out.chars().count() < target.min(10_000) {
                let need = target - out.chars().count();
                let chunk: String = pad.chars().take(need).collect();
                if name == "padStart" {
                    out = format!("{chunk}{out}");
                } else {
                    out.push_str(&chunk);
                }
            }
            str_value(&out)
        }
        _ => str_value(&s), // toString / valueOf
    })
}

pub fn number_member(key: &str) -> EResult {
    Ok(match key {
        "toFixed" | "toString" | "valueOf" => native(number_method, leak_static(key)),
        _ => Value::Undefined,
    })
}

/// The three number method names are static; map them without leaking.
fn leak_static(key: &str) -> &'static str {
    match key {
        "toFixed" => "toFixed",
        "toString" => "toString",
        _ => "valueOf",
    }
}

fn number_method(_it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let n = to_number(this);
    Ok(match name {
        "toFixed" => {
            let digits = (to_number(&args.first().cloned().unwrap_or(Value::Undefined))
                .max(0.0) as usize)
                .min(20);
            if !n.is_finite() {
                return Ok(str_value(&num_to_string(n)));
            }
            // Scale, round, and render with manual decimal placement.
            let neg = n < 0.0;
            let mut scale = 1.0f64;
            for _ in 0..digits {
                scale *= 10.0;
            }
            let scaled = f_round(f_abs(n) * scale);
            let int_part = f_trunc(scaled / scale);
            let frac_part = (scaled - int_part * scale) as u64;
            let mut out = String::new();
            if neg {
                out.push('-');
            }
            out.push_str(&num_to_string(int_part));
            if digits > 0 {
                out.push('.');
                let frac_str = format!("{:0width$}", frac_part, width = digits);
                out.push_str(&frac_str);
            }
            str_value(&out)
        }
        "toString" => str_value(&num_to_string(n)),
        _ => Value::Num(n),
    })
}

static ARRAY_METHODS: &[&str] = &[
    "push", "pop", "shift", "unshift", "indexOf", "lastIndexOf", "includes", "join", "slice",
    "splice", "concat", "reverse", "map", "filter", "forEach", "reduce", "some", "every",
    "find", "findIndex", "sort", "fill", "flat", "keys", "toString",
];

pub fn array_member(key: &str) -> Option<Value> {
    ARRAY_METHODS
        .iter()
        .find(|m| **m == key)
        .map(|m| native(array_method, m))
}

fn array_items(this: &Value) -> Option<Obj> {
    match this {
        Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_)) => Some(o.clone()),
        _ => None,
    }
}

fn array_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let Some(arr) = array_items(this) else {
        return Ok(Value::Undefined);
    };
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    let snapshot = || -> Vec<Value> {
        match &arr.borrow().kind {
            ObjKind::Array(items) => items.clone(),
            _ => Vec::new(),
        }
    };
    let with_items = |f: &mut dyn FnMut(&mut Vec<Value>) -> Value| -> Value {
        let mut b = arr.borrow_mut();
        match &mut b.kind {
            ObjKind::Array(items) => f(items),
            _ => Value::Undefined,
        }
    };
    Ok(match name {
        "push" => with_items(&mut |items| {
            for a in args {
                items.push(a.clone());
            }
            Value::Num(items.len() as f64)
        }),
        "pop" => with_items(&mut |items| items.pop().unwrap_or(Value::Undefined)),
        "shift" => with_items(&mut |items| {
            if items.is_empty() {
                Value::Undefined
            } else {
                items.remove(0)
            }
        }),
        "unshift" => with_items(&mut |items| {
            for (i, a) in args.iter().enumerate() {
                items.insert(i, a.clone());
            }
            Value::Num(items.len() as f64)
        }),
        "indexOf" | "lastIndexOf" | "includes" => {
            let needle = arg(0);
            let items = snapshot();
            let pos = if name == "lastIndexOf" {
                items.iter().rposition(|v| strict_eq(v, &needle))
            } else {
                items.iter().position(|v| strict_eq(v, &needle))
            };
            match name {
                "includes" => Value::Bool(pos.is_some()),
                _ => Value::Num(pos.map(|p| p as f64).unwrap_or(-1.0)),
            }
        }
        "join" => {
            let sep = match arg(0) {
                Value::Undefined => String::from(","),
                v => to_string(&v),
            };
            let items = snapshot();
            let mut out = String::new();
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(&sep);
                }
                if !matches!(v, Value::Undefined | Value::Null) {
                    out.push_str(&to_string(v));
                }
            }
            str_value(&out)
        }
        "slice" => {
            let items = snapshot();
            let len = items.len() as i64;
            let norm = |v: f64| -> i64 {
                let v = v as i64;
                if v < 0 {
                    (len + v).max(0)
                } else {
                    v.min(len)
                }
            };
            let a = if matches!(arg(0), Value::Undefined) {
                0
            } else {
                norm(to_number(&arg(0)))
            };
            let b = if matches!(arg(1), Value::Undefined) {
                len
            } else {
                norm(to_number(&arg(1)))
            };
            new_array(items.get(a as usize..b.max(a) as usize).unwrap_or(&[]).to_vec())
        }
        "splice" => with_items(&mut |items| {
            let len = items.len() as i64;
            let start = {
                let v = to_number(&arg(0)) as i64;
                if v < 0 {
                    (len + v).max(0) as usize
                } else {
                    (v as usize).min(items.len())
                }
            };
            let count = if matches!(arg(1), Value::Undefined) {
                items.len() - start
            } else {
                (to_number(&arg(1)).max(0.0) as usize).min(items.len() - start)
            };
            let removed: Vec<Value> = items.splice(start..start + count, args[2.min(args.len())..].iter().cloned()).collect();
            new_array(removed)
        }),
        "concat" => {
            let mut out = snapshot();
            for a in args {
                match a {
                    Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_)) => {
                        if let ObjKind::Array(items) = &o.borrow().kind {
                            out.extend(items.iter().cloned());
                        }
                    }
                    v => out.push(v.clone()),
                }
            }
            new_array(out)
        }
        "reverse" => {
            with_items(&mut |items| {
                items.reverse();
                Value::Undefined
            });
            this.clone()
        }
        "fill" => {
            with_items(&mut |items| {
                for slot in items.iter_mut() {
                    *slot = arg(0);
                }
                Value::Undefined
            });
            this.clone()
        }
        "flat" => {
            let items = snapshot();
            let mut out = Vec::new();
            for v in items {
                match &v {
                    Value::Obj(o) if matches!(o.borrow().kind, ObjKind::Array(_)) => {
                        if let ObjKind::Array(inner) = &o.borrow().kind {
                            out.extend(inner.iter().cloned());
                        }
                    }
                    _ => out.push(v),
                }
            }
            new_array(out)
        }
        "keys" => new_array(
            (0..snapshot().len())
                .map(|i| Value::Num(i as f64))
                .collect(),
        ),
        "map" | "filter" | "forEach" | "some" | "every" | "find" | "findIndex" => {
            let f = arg(0);
            let items = snapshot();
            let mut out = Vec::new();
            for (i, v) in items.iter().enumerate() {
                let r = it.call(&f, &Value::Undefined, &[v.clone(), Value::Num(i as f64), this.clone()])?;
                match name {
                    "map" => out.push(r),
                    "filter" => {
                        if truthy(&r) {
                            out.push(v.clone());
                        }
                    }
                    "some" => {
                        if truthy(&r) {
                            return Ok(Value::Bool(true));
                        }
                    }
                    "every" => {
                        if !truthy(&r) {
                            return Ok(Value::Bool(false));
                        }
                    }
                    "find" => {
                        if truthy(&r) {
                            return Ok(v.clone());
                        }
                    }
                    "findIndex" => {
                        if truthy(&r) {
                            return Ok(Value::Num(i as f64));
                        }
                    }
                    _ => {}
                }
            }
            match name {
                "map" | "filter" => new_array(out),
                "some" => Value::Bool(false),
                "every" => Value::Bool(true),
                "find" => Value::Undefined,
                "findIndex" => Value::Num(-1.0),
                _ => Value::Undefined, // forEach
            }
        }
        "reduce" => {
            let f = arg(0);
            let items = snapshot();
            let mut iter = items.into_iter().enumerate();
            let mut acc = if args.len() > 1 {
                arg(1)
            } else {
                match iter.next() {
                    Some((_, v)) => v,
                    None => {
                        return Err(Interp::throw_str(
                            "TypeError",
                            "reduce of empty array with no initial value",
                        ))
                    }
                }
            };
            for (i, v) in iter {
                acc = it.call(
                    &f,
                    &Value::Undefined,
                    &[acc, v, Value::Num(i as f64), this.clone()],
                )?;
            }
            acc
        }
        "sort" => {
            let cmp = arg(0);
            let mut items = snapshot();
            // Insertion sort: stable, allocation-free, and re-entrant with a
            // user comparator without aliasing the array borrow.
            for i in 1..items.len() {
                let mut j = i;
                while j > 0 {
                    let swap = match &cmp {
                        Value::Undefined => to_string(&items[j - 1]) > to_string(&items[j]),
                        f => {
                            let r = it.call(
                                f,
                                &Value::Undefined,
                                &[items[j - 1].clone(), items[j].clone()],
                            )?;
                            to_number(&r) > 0.0
                        }
                    };
                    if !swap {
                        break;
                    }
                    items.swap(j - 1, j);
                    j -= 1;
                }
            }
            with_items(&mut |slot| {
                *slot = items.clone();
                Value::Undefined
            });
            this.clone()
        }
        _ => str_value(&to_string(this)), // toString
    })
}

pub fn function_member(key: &str) -> Option<Value> {
    match key {
        "call" => Some(native(function_method, "call")),
        "apply" => Some(native(function_method, "apply")),
        "bind" => Some(native(function_method, "bind")),
        _ => None,
    }
}

fn function_method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    match name {
        "call" => {
            let bound_this = args.first().cloned().unwrap_or(Value::Undefined);
            it.call(this, &bound_this, &args[1.min(args.len())..])
        }
        "apply" => {
            let bound_this = args.first().cloned().unwrap_or(Value::Undefined);
            let list = match args.get(1) {
                Some(Value::Obj(o)) => match &o.borrow().kind {
                    ObjKind::Array(items) => items.clone(),
                    _ => Vec::new(),
                },
                _ => Vec::new(),
            };
            it.call(this, &bound_this, &list)
        }
        "bind" => {
            let bound_this = args.first().cloned().unwrap_or(Value::Undefined);
            let bound_args = args.get(1..).unwrap_or(&[]).to_vec();
            Ok(Value::Obj(new_obj(ObjKind::Bound {
                target: this.clone(),
                bound_this,
                bound_args,
            })))
        }
        _ => Ok(Value::Undefined),
    }
}

pub fn object_member(key: &str) -> Option<Value> {
    match key {
        "hasOwnProperty" => Some(native(object_method, "hasOwnProperty")),
        "toString" => Some(native(object_method, "toString")),
        _ => None,
    }
}

fn object_method(_it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    Ok(match name {
        "hasOwnProperty" => {
            let key = to_string(&args.first().cloned().unwrap_or(Value::Undefined));
            match this {
                Value::Obj(o) => {
                    let b = o.borrow();
                    let own = b.props.contains_key(&key)
                        || matches!(&b.kind, ObjKind::Array(items)
                            if key.parse::<usize>().is_ok_and(|i| i < items.len()));
                    Value::Bool(own)
                }
                _ => Value::Bool(false),
            }
        }
        _ => str_value(&to_string(this)),
    })
}
