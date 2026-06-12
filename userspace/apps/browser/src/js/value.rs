//! JavaScript values, objects, and environments.
//!
//! Values follow the ES model: primitives plus reference-counted objects with
//! a prototype chain. DOM handles are first-class variants (`Node`,
//! `Document`, `StyleOf`) so the interpreter can route property access to the
//! arena-backed DOM without a wrapper-object layer.
//!
//! `Rc` cycles are never collected — the browser heap is a bump allocator
//! that frees nothing until the process exits, so cycles cost no more than
//! any other allocation.
//!
//! The target is soft-float (`x86_64-unknown-none` disables SSE), so the
//! `f64` math helpers std would provide (`floor`, `sqrt`, …) are implemented
//! here with integer/bit manipulation.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use core::cell::RefCell;

use super::ast::FnDef;
use super::interp::{Control, Interp};

pub type Obj = Rc<RefCell<ObjData>>;
pub type EnvRef = Rc<RefCell<Env>>;

/// Native function: `(interp, this, args, name) → value`. The `name` lets one
/// dispatcher serve a whole method family (string methods, Math, …).
pub type NativeFn = fn(&mut Interp, &Value, &[Value], &'static str) -> Result<Value, Control>;

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<str>),
    Obj(Obj),
    /// A DOM element handle (arena node id).
    Node(usize),
    /// The `document` object.
    Document,
    /// `element.style` of a node — property access maps to the style attr.
    StyleOf(usize),
}

pub struct ObjData {
    pub props: BTreeMap<String, Value>,
    pub proto: Option<Obj>,
    pub kind: ObjKind,
}

pub enum ObjKind {
    Plain,
    Array(Vec<Value>),
    Function {
        def: Rc<FnDef>,
        env: EnvRef,
        /// `this` captured at creation for arrow functions.
        this: Option<Value>,
    },
    Native(NativeFn, &'static str),
    /// `Function.prototype.bind` result: forwards to `target` with a fixed
    /// `this` and leading arguments.
    Bound {
        target: Value,
        bound_this: Value,
        bound_args: Vec<Value>,
    },
}

/// A lexical scope. `fn_scope` marks function (and global) boundaries, where
/// `var` declarations land.
pub struct Env {
    pub vars: BTreeMap<String, Value>,
    pub parent: Option<EnvRef>,
    pub fn_scope: bool,
}

impl Env {
    pub fn new_global() -> EnvRef {
        Rc::new(RefCell::new(Env {
            vars: BTreeMap::new(),
            parent: None,
            fn_scope: true,
        }))
    }

    pub fn child(parent: &EnvRef, fn_scope: bool) -> EnvRef {
        Rc::new(RefCell::new(Env {
            vars: BTreeMap::new(),
            parent: Some(parent.clone()),
            fn_scope,
        }))
    }
}

/// Read a variable, walking the scope chain.
pub fn env_get(env: &EnvRef, name: &str) -> Option<Value> {
    let mut cur = env.clone();
    loop {
        if let Some(v) = cur.borrow().vars.get(name) {
            return Some(v.clone());
        }
        let next = cur.borrow().parent.clone();
        match next {
            Some(p) => cur = p,
            None => return None,
        }
    }
}

/// Assign to an existing binding; falls back to defining a global (sloppy
/// mode). Returns false only on internal failure.
pub fn env_set(env: &EnvRef, name: &str, value: Value) {
    let mut cur = env.clone();
    loop {
        if cur.borrow().vars.contains_key(name) {
            cur.borrow_mut().vars.insert(name.into(), value);
            return;
        }
        let next = cur.borrow().parent.clone();
        match next {
            Some(p) => cur = p,
            None => {
                cur.borrow_mut().vars.insert(name.into(), value);
                return;
            }
        }
    }
}

/// Define in the nearest scope (`let`/`const`) or nearest function scope (`var`).
pub fn env_define(env: &EnvRef, name: &str, value: Value, function_scoped: bool) {
    if !function_scoped {
        env.borrow_mut().vars.insert(name.into(), value);
        return;
    }
    let mut cur = env.clone();
    loop {
        if cur.borrow().fn_scope {
            cur.borrow_mut().vars.insert(name.into(), value);
            return;
        }
        let next = cur.borrow().parent.clone();
        match next {
            Some(p) => cur = p,
            None => {
                cur.borrow_mut().vars.insert(name.into(), value);
                return;
            }
        }
    }
}

// ── Constructors ─────────────────────────────────────────────────────────────

pub fn new_obj(kind: ObjKind) -> Obj {
    Rc::new(RefCell::new(ObjData {
        props: BTreeMap::new(),
        proto: None,
        kind,
    }))
}

pub fn new_plain() -> Obj {
    new_obj(ObjKind::Plain)
}

pub fn new_array(items: Vec<Value>) -> Value {
    Value::Obj(new_obj(ObjKind::Array(items)))
}

pub fn native(f: NativeFn, name: &'static str) -> Value {
    Value::Obj(new_obj(ObjKind::Native(f, name)))
}

pub fn str_value(s: &str) -> Value {
    Value::Str(Rc::from(s))
}

/// Build an `Error`-shaped object: `{ name, message }`.
pub fn make_error(kind: &str, msg: &str) -> Value {
    let o = new_plain();
    o.borrow_mut().props.insert("name".into(), str_value(kind));
    o.borrow_mut()
        .props
        .insert("message".into(), str_value(msg));
    Value::Obj(o)
}

// ── Conversions ──────────────────────────────────────────────────────────────

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Undefined | Value::Null => false,
        Value::Bool(b) => *b,
        Value::Num(n) => *n != 0.0 && !n.is_nan(),
        Value::Str(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Undefined => "undefined",
        Value::Null => "object",
        Value::Bool(_) => "boolean",
        Value::Num(_) => "number",
        Value::Str(_) => "string",
        Value::Obj(o) => match o.borrow().kind {
            ObjKind::Function { .. } | ObjKind::Native(..) | ObjKind::Bound { .. } => "function",
            _ => "object",
        },
        Value::Node(_) | Value::Document | Value::StyleOf(_) => "object",
    }
}

pub fn to_number(v: &Value) -> f64 {
    match v {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(b) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Value::Num(n) => *n,
        Value::Str(s) => str_to_number(s),
        Value::Obj(o) => match &o.borrow().kind {
            // [] → 0, [x] → Number(x) via the string round-trip.
            ObjKind::Array(items) => match items.len() {
                0 => 0.0,
                1 => to_number(&items[0]),
                _ => f64::NAN,
            },
            _ => f64::NAN,
        },
        _ => f64::NAN,
    }
}

pub fn str_to_number(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() {
        return 0.0;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return match u64::from_str_radix(hex, 16) {
            Ok(v) => v as f64,
            Err(_) => f64::NAN,
        };
    }
    if t == "Infinity" || t == "+Infinity" {
        return f64::INFINITY;
    }
    if t == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// JS-style number formatting: integral values print without a decimal
/// point; infinities use the JS spelling.
pub fn num_to_string(n: f64) -> String {
    if n.is_nan() {
        return String::from("NaN");
    }
    if n.is_infinite() {
        return String::from(if n > 0.0 { "Infinity" } else { "-Infinity" });
    }
    if n == 0.0 {
        return String::from("0");
    }
    // Rust's shortest-roundtrip Display matches JS for the common range.
    let s = format!("{}", n);
    s
}

pub fn to_string(v: &Value) -> String {
    match v {
        Value::Undefined => String::from("undefined"),
        Value::Null => String::from("null"),
        Value::Bool(b) => String::from(if *b { "true" } else { "false" }),
        Value::Num(n) => num_to_string(*n),
        Value::Str(s) => s.to_string(),
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Array(items) => {
                let mut out = String::new();
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    if !matches!(it, Value::Undefined | Value::Null) {
                        out.push_str(&to_string(it));
                    }
                }
                out
            }
            ObjKind::Function { def, .. } => {
                format!(
                    "function {}() {{ [source hidden] }}",
                    def.name.as_deref().unwrap_or("")
                )
            }
            ObjKind::Native(_, name) => format!("function {name}() {{ [native] }}"),
            ObjKind::Bound { .. } => String::from("function bound() { [bound] }"),
            ObjKind::Plain => {
                // Error-shaped objects stringify as "Name: message".
                let b = o.borrow();
                if let (Some(name), Some(msg)) = (b.props.get("name"), b.props.get("message")) {
                    if b.props.len() <= 3 {
                        return format!("{}: {}", to_string(name), to_string(msg));
                    }
                }
                String::from("[object Object]")
            }
        },
        Value::Node(_) => String::from("[object HTMLElement]"),
        Value::Document => String::from("[object HTMLDocument]"),
        Value::StyleOf(_) => String::from("[object CSSStyleDeclaration]"),
    }
}

/// Debug rendering for console.log: arrays/objects display one level deep.
pub fn inspect(v: &Value) -> String {
    match v {
        Value::Str(s) => s.to_string(),
        Value::Obj(o) => match &o.borrow().kind {
            ObjKind::Array(items) => {
                let mut out = String::from("[");
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(&inspect_shallow(it));
                }
                out.push(']');
                out
            }
            ObjKind::Plain => {
                let b = o.borrow();
                let mut out = String::from("{");
                for (i, (k, pv)) in b.props.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(": ");
                    out.push_str(&inspect_shallow(pv));
                }
                out.push('}');
                out
            }
            _ => to_string(v),
        },
        _ => to_string(v),
    }
}

fn inspect_shallow(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("\"{}\"", s),
        Value::Obj(o) => match o.borrow().kind {
            ObjKind::Array(_) => String::from("[…]"),
            ObjKind::Plain => String::from("{…}"),
            _ => to_string(v),
        },
        _ => to_string(v),
    }
}

/// Strict equality (`===`).
pub fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => Rc::ptr_eq(x, y),
        (Value::Node(x), Value::Node(y)) => x == y,
        (Value::Document, Value::Document) => true,
        (Value::StyleOf(x), Value::StyleOf(y)) => x == y,
        _ => false,
    }
}

/// Abstract equality (`==`) with the usual coercions.
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
        (Value::Num(_), Value::Num(_))
        | (Value::Str(_), Value::Str(_))
        | (Value::Bool(_), Value::Bool(_)) => strict_eq(a, b),
        (Value::Num(x), Value::Str(s)) | (Value::Str(s), Value::Num(x)) => {
            *x == str_to_number(s)
        }
        (Value::Bool(_), _) => loose_eq(&Value::Num(to_number(a)), b),
        (_, Value::Bool(_)) => loose_eq(a, &Value::Num(to_number(b))),
        (Value::Obj(_), Value::Num(_) | Value::Str(_)) => {
            loose_eq(&str_value(&to_string(a)), b)
        }
        (Value::Num(_) | Value::Str(_), Value::Obj(_)) => {
            loose_eq(a, &str_value(&to_string(b)))
        }
        _ => strict_eq(a, b),
    }
}

// ── Soft-float math (no std, no SSE) ─────────────────────────────────────────

pub fn f_abs(x: f64) -> f64 {
    f64::from_bits(x.to_bits() & !(1u64 << 63))
}

pub fn f_trunc(x: f64) -> f64 {
    if !x.is_finite() || f_abs(x) >= 9.007_199_254_740_992e15 {
        return x; // beyond 2^53 every f64 is integral
    }
    (x as i64) as f64
}

pub fn f_floor(x: f64) -> f64 {
    let t = f_trunc(x);
    if x < 0.0 && t != x {
        t - 1.0
    } else {
        t
    }
}

pub fn f_ceil(x: f64) -> f64 {
    let t = f_trunc(x);
    if x > 0.0 && t != x {
        t + 1.0
    } else {
        t
    }
}

pub fn f_round(x: f64) -> f64 {
    // JS rounds half-up (toward +∞).
    f_floor(x + 0.5)
}

/// Newton–Raphson square root.
pub fn f_sqrt(x: f64) -> f64 {
    if x.is_nan() || x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 || x.is_infinite() {
        return x;
    }
    // Initial estimate from halving the exponent.
    let bits = x.to_bits();
    let mut guess = f64::from_bits((bits >> 1) + (0x3FF0_0000_0000_0000u64 >> 1));
    for _ in 0..6 {
        guess = 0.5 * (guess + x / guess);
    }
    guess
}

/// `x^y` for integral `y` (binary exponentiation); non-integral exponents
/// approximate via sqrt for halves and otherwise return NaN.
pub fn f_pow(x: f64, y: f64) -> f64 {
    if y == 0.0 {
        return 1.0;
    }
    if y.is_nan() || x.is_nan() {
        return f64::NAN;
    }
    let yi = f_trunc(y);
    if yi == y && f_abs(y) <= 1024.0 {
        let mut base = x;
        let mut n = f_abs(y) as u64;
        let mut acc = 1.0f64;
        while n > 0 {
            if n & 1 == 1 {
                acc *= base;
            }
            base *= base;
            n >>= 1;
        }
        return if y < 0.0 { 1.0 / acc } else { acc };
    }
    // Halves: x^(k + 0.5) = x^k * sqrt(x).
    if f_abs(y - yi) == 0.5 && x >= 0.0 {
        return f_pow(x, yi) * f_sqrt(x) * if y < yi { 1.0 / x } else { 1.0 };
    }
    f64::NAN
}

/// ToInt32 (spec-ish): truncate toward zero, wrap modulo 2^32.
pub fn to_int32(v: &Value) -> i32 {
    let n = to_number(v);
    if !n.is_finite() {
        return 0;
    }
    let t = f_trunc(n);
    // Wrap via i128 to survive the full f64 integral range.
    (t as i128 as u32) as i32
}

pub fn to_uint32(v: &Value) -> u32 {
    to_int32(v) as u32
}
