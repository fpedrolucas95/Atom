//! JavaScript tree-walking interpreter.
//!
//! Evaluates the AST against a scope chain, with:
//!
//! * a **step budget** so `while (true) {}` aborts the script instead of
//!   hanging the browser,
//! * a **call-depth cap** sized for the 512 KiB userspace stack,
//! * `var` hoisting and function-declaration hoisting per function scope,
//! * prototype chains (`new`, `instanceof`, constructor `.prototype`),
//! * exceptions (`throw`/`try`/`catch`/`finally`) carried as [`Control`],
//! * property access dispatch over both script objects and DOM handles
//!   (routed to [`super::dom_api`]).

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::ast::*;
use super::builtins;
use super::dom_api;
use super::events::Handlers;
use super::storage::Storage;
use super::value::*;
use crate::domtree::Dom;

/// Maximum interpreter call depth (JS frames, not Rust frames).
const MAX_CALL_DEPTH: u32 = 64;

/// Non-linear control flow, threaded through `Result::Err`.
pub enum Control {
    Return(Value),
    Break,
    Continue,
    Throw(Value),
    /// Unrecoverable interpreter stop (budget/depth); not catchable.
    Abort(&'static str),
}

pub type EResult = Result<Value, Control>;

/// Where `document.write` output is grafted: `(parent, next_child_index)`.
#[derive(Clone, Copy)]
pub struct WriteCursor {
    pub parent: usize,
    pub index: usize,
}

pub struct Interp<'a> {
    pub dom: &'a mut Dom,
    pub console: &'a mut Vec<String>,
    pub handlers: &'a mut Handlers,
    pub storage: &'a mut Storage,
    pub cookies: super::cookie::SharedJar,
    pub host: Rc<str>,
    pub global: EnvRef,
    pub cursor: Option<WriteCursor>,
    steps: u64,
    depth: u32,
}

impl<'a> Interp<'a> {
    /// Build an interpreter over a persistent runtime's global scope and
    /// handler table, with a fresh step budget for this run.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        dom: &'a mut Dom,
        console: &'a mut Vec<String>,
        handlers: &'a mut Handlers,
        storage: &'a mut Storage,
        cookies: super::cookie::SharedJar,
        host: Rc<str>,
        global: EnvRef,
        budget: u64,
    ) -> Self {
        Self {
            dom,
            console,
            handlers,
            storage,
            cookies,
            host,
            global,
            cursor: None,
            steps: budget,
            depth: 0,
        }
    }

    pub fn throw_str(kind: &str, msg: &str) -> Control {
        Control::Throw(make_error(kind, msg))
    }

    fn tick(&mut self) -> Result<(), Control> {
        self.steps = self.steps.saturating_sub(1);
        if self.steps == 0 {
            Err(Control::Abort("step budget exhausted"))
        } else {
            Ok(())
        }
    }

    /// Run a top-level script body in the global scope.
    pub fn run_program(&mut self, stmts: &[Stmt]) -> Result<(), Control> {
        let global = self.global.clone();
        self.hoist(stmts, &global)?;
        let this = env_get(&global, "window").unwrap_or(Value::Undefined);
        for s in stmts {
            self.stmt(s, &global, &this)?;
        }
        Ok(())
    }

    // ── Hoisting ────────────────────────────────────────────────────────────

    /// Predeclare `var` names (as undefined) and function declarations in
    /// `env`, recursing into nested statements but not nested functions.
    fn hoist(&mut self, stmts: &[Stmt], env: &EnvRef) -> Result<(), Control> {
        for s in stmts {
            self.hoist_stmt(s, env)?;
        }
        Ok(())
    }

    fn hoist_stmt(&mut self, s: &Stmt, env: &EnvRef) -> Result<(), Control> {
        match s {
            Stmt::VarDecl {
                function_scoped: true,
                decls,
            } => {
                for (name, _) in decls {
                    if env_get(env, name).is_none() {
                        env_define(env, name, Value::Undefined, true);
                    }
                }
            }
            Stmt::FuncDecl(def) => {
                let f = self.make_function(def.clone(), env, None);
                if let Some(name) = &def.name {
                    env_define(env, name, f, true);
                }
            }
            Stmt::Block(b) => self.hoist(b, env)?,
            Stmt::If { then, els, .. } => {
                self.hoist_stmt(then, env)?;
                if let Some(e) = els {
                    self.hoist_stmt(e, env)?;
                }
            }
            Stmt::While { body, .. } | Stmt::DoWhile { body, .. } => self.hoist_stmt(body, env)?,
            Stmt::For { init, body, .. } => {
                if let Some(i) = init {
                    self.hoist_stmt(i, env)?;
                }
                self.hoist_stmt(body, env)?;
            }
            Stmt::ForIn {
                decl, var, body, ..
            } => {
                if *decl && env_get(env, var).is_none() {
                    env_define(env, var, Value::Undefined, true);
                }
                self.hoist_stmt(body, env)?;
            }
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                self.hoist(body, env)?;
                if let Some(c) = catch {
                    self.hoist(c, env)?;
                }
                if let Some(f) = finally {
                    self.hoist(f, env)?;
                }
            }
            Stmt::Switch { cases, .. } => {
                for (_, body) in cases {
                    self.hoist(body, env)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ── Statements ──────────────────────────────────────────────────────────

    fn stmt(&mut self, s: &Stmt, env: &EnvRef, this: &Value) -> Result<(), Control> {
        self.tick()?;
        match s {
            Stmt::Empty | Stmt::FuncDecl(_) => Ok(()), // decls handled by hoisting
            Stmt::Expr(e) => {
                self.expr(e, env, this)?;
                Ok(())
            }
            Stmt::VarDecl {
                function_scoped,
                decls,
            } => {
                for (name, init) in decls {
                    let v = match init {
                        Some(e) => self.expr(e, env, this)?,
                        None => Value::Undefined,
                    };
                    env_define(env, name, v, *function_scoped);
                }
                Ok(())
            }
            Stmt::Block(body) => {
                let scope = Env::child(env, false);
                self.hoist(body, &scope)?;
                for s in body {
                    self.stmt(s, &scope, this)?;
                }
                Ok(())
            }
            Stmt::If { cond, then, els } => {
                if truthy(&self.expr(cond, env, this)?) {
                    self.stmt(then, env, this)
                } else if let Some(e) = els {
                    self.stmt(e, env, this)
                } else {
                    Ok(())
                }
            }
            Stmt::While { cond, body } => {
                while truthy(&self.expr(cond, env, this)?) {
                    match self.stmt(body, env, this) {
                        Err(Control::Break) => break,
                        Err(Control::Continue) => continue,
                        r => r?,
                    }
                }
                Ok(())
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    match self.stmt(body, env, this) {
                        Err(Control::Break) => break,
                        Err(Control::Continue) => {}
                        r => r?,
                    }
                    if !truthy(&self.expr(cond, env, this)?) {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::For {
                init,
                cond,
                step,
                body,
            } => {
                let scope = Env::child(env, false);
                if let Some(i) = init {
                    self.stmt(i, &scope, this)?;
                }
                loop {
                    if let Some(c) = cond {
                        if !truthy(&self.expr(c, &scope, this)?) {
                            break;
                        }
                    }
                    match self.stmt(body, &scope, this) {
                        Err(Control::Break) => break,
                        Err(Control::Continue) => {}
                        r => r?,
                    }
                    if let Some(s) = step {
                        self.expr(s, &scope, this)?;
                    }
                }
                Ok(())
            }
            Stmt::ForIn {
                decl,
                var,
                of,
                obj,
                body,
            } => {
                let subject = self.expr(obj, env, this)?;
                let items = self.enumerate(&subject, *of);
                let scope = Env::child(env, false);
                if *decl {
                    env_define(&scope, var, Value::Undefined, false);
                }
                for item in items {
                    env_set(&scope, var, item);
                    match self.stmt(body, &scope, this) {
                        Err(Control::Break) => break,
                        Err(Control::Continue) => continue,
                        r => r?,
                    }
                }
                Ok(())
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.expr(e, env, this)?,
                    None => Value::Undefined,
                };
                Err(Control::Return(v))
            }
            Stmt::Break => Err(Control::Break),
            Stmt::Continue => Err(Control::Continue),
            Stmt::Throw(e) => {
                let v = self.expr(e, env, this)?;
                Err(Control::Throw(v))
            }
            Stmt::Try {
                body,
                catch_var,
                catch,
                finally,
            } => {
                let scope = Env::child(env, false);
                let mut result: Result<(), Control> = (|| {
                    self.hoist(body, &scope)?;
                    for s in body {
                        self.stmt(s, &scope, this)?;
                    }
                    Ok(())
                })();
                if let Err(Control::Throw(err)) = result {
                    result = Ok(());
                    if let Some(cbody) = catch {
                        let cscope = Env::child(env, false);
                        if let Some(var) = catch_var {
                            env_define(&cscope, var, err, false);
                        }
                        result = (|| {
                            self.hoist(cbody, &cscope)?;
                            for s in cbody {
                                self.stmt(s, &cscope, this)?;
                            }
                            Ok(())
                        })();
                    }
                }
                if let Some(fbody) = finally {
                    let fscope = Env::child(env, false);
                    for s in fbody {
                        self.stmt(s, &fscope, this)?;
                    }
                }
                result
            }
            Stmt::Switch { disc, cases } => {
                let d = self.expr(disc, env, this)?;
                let scope = Env::child(env, false);
                // Find the matching case (or default), then fall through.
                let mut start = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let tv = self.expr(t, &scope, this)?;
                        if strict_eq(&d, &tv) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                if start.is_none() {
                    start = cases.iter().position(|(t, _)| t.is_none());
                }
                if let Some(start) = start {
                    'outer: for (_, body) in &cases[start..] {
                        for s in body {
                            match self.stmt(s, &scope, this) {
                                Err(Control::Break) => break 'outer,
                                r => r?,
                            }
                        }
                    }
                }
                Ok(())
            }
        }
    }

    /// Keys/values produced by `for-in` (`of == false`) or `for-of`.
    fn enumerate(&mut self, subject: &Value, of: bool) -> Vec<Value> {
        match subject {
            Value::Obj(o) => match &o.borrow().kind {
                ObjKind::Array(items) => {
                    if of {
                        items.clone()
                    } else {
                        (0..items.len())
                            .map(|i| str_value(&i.to_string()))
                            .collect()
                    }
                }
                _ => {
                    let b = o.borrow();
                    if of {
                        b.props.values().cloned().collect()
                    } else {
                        b.props.keys().map(|k| str_value(k)).collect()
                    }
                }
            },
            Value::Str(s) => {
                if of {
                    s.chars().map(|c| str_value(&c.to_string())).collect()
                } else {
                    (0..s.chars().count())
                        .map(|i| str_value(&i.to_string()))
                        .collect()
                }
            }
            _ => Vec::new(),
        }
    }

    // ── Expressions ─────────────────────────────────────────────────────────

    pub fn expr(&mut self, e: &Expr, env: &EnvRef, this: &Value) -> EResult {
        self.tick()?;
        match e {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::This => Ok(this.clone()),
            Expr::Ident(name) => env_get(env, name).ok_or_else(|| {
                Self::throw_str("ReferenceError", &format!("{name} is not defined"))
            }),
            Expr::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.expr(it, env, this)?);
                }
                Ok(new_array(out))
            }
            Expr::Object(props) => {
                let o = new_plain();
                for (k, v) in props {
                    let val = self.expr(v, env, this)?;
                    o.borrow_mut().props.insert(k.clone(), val);
                }
                Ok(Value::Obj(o))
            }
            Expr::Func(def) => Ok(self.make_function(
                def.clone(),
                env,
                if def.is_arrow {
                    Some(this.clone())
                } else {
                    None
                },
            )),
            Expr::Template(parts) => {
                let mut out = String::new();
                for p in parts {
                    match p {
                        TplPart::Str(s) => out.push_str(s),
                        TplPart::Expr(e) => {
                            let v = self.expr(e, env, this)?;
                            out.push_str(&to_string(&v));
                        }
                    }
                }
                Ok(str_value(&out))
            }
            Expr::Regex(src) => {
                // Inert stand-in: `{ source: "…" }`. Matching is unsupported.
                let o = new_plain();
                o.borrow_mut()
                    .props
                    .insert("source".into(), Value::Str(src.clone()));
                Ok(Value::Obj(o))
            }
            Expr::Unary(op, operand) => self.unary(*op, operand, env, this),
            Expr::Update {
                inc,
                prefix,
                target,
            } => {
                let old = to_number(&self.expr(target, env, this)?);
                let new = if *inc { old + 1.0 } else { old - 1.0 };
                self.assign_to(target, Value::Num(new), env, this)?;
                Ok(Value::Num(if *prefix { new } else { old }))
            }
            Expr::Binary(op, l, r) => {
                let lv = self.expr(l, env, this)?;
                let rv = self.expr(r, env, this)?;
                self.binary(*op, lv, rv)
            }
            Expr::Logical(op, l, r) => {
                let lv = self.expr(l, env, this)?;
                let take_right = match op {
                    LogOp::And => truthy(&lv),
                    LogOp::Or => !truthy(&lv),
                    LogOp::Nullish => matches!(lv, Value::Undefined | Value::Null),
                };
                if take_right {
                    self.expr(r, env, this)
                } else {
                    Ok(lv)
                }
            }
            Expr::Cond(c, t, f) => {
                if truthy(&self.expr(c, env, this)?) {
                    self.expr(t, env, this)
                } else {
                    self.expr(f, env, this)
                }
            }
            Expr::Assign { op, target, value } => {
                let rhs = self.expr(value, env, this)?;
                let final_v = match op {
                    None => rhs,
                    Some(op) => {
                        let cur = self.expr(target, env, this)?;
                        self.binary(*op, cur, rhs)?
                    }
                };
                self.assign_to(target, final_v.clone(), env, this)?;
                Ok(final_v)
            }
            Expr::Sequence(seq) => {
                let mut last = Value::Undefined;
                for e in seq {
                    last = self.expr(e, env, this)?;
                }
                Ok(last)
            }
            Expr::Member {
                obj,
                prop,
                optional,
            } => {
                let base = self.expr(obj, env, this)?;
                if *optional && matches!(base, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let key = self.member_key(prop, env, this)?;
                self.get_member(&base, &key)
            }
            Expr::Call { callee, args } => {
                // Member calls bind `this` to the receiver.
                let (this_v, f) = match callee.as_ref() {
                    Expr::Member {
                        obj,
                        prop,
                        optional,
                    } => {
                        let base = self.expr(obj, env, this)?;
                        if *optional && matches!(base, Value::Undefined | Value::Null) {
                            return Ok(Value::Undefined);
                        }
                        let key = self.member_key(prop, env, this)?;
                        let f = self.get_member(&base, &key)?;
                        (base, f)
                    }
                    _ => (Value::Undefined, self.expr(callee, env, this)?),
                };
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.expr(a, env, this)?);
                }
                self.call(&f, &this_v, &argv)
            }
            Expr::New { callee, args } => {
                let f = self.expr(callee, env, this)?;
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.expr(a, env, this)?);
                }
                self.construct(&f, &argv)
            }
        }
    }

    fn member_key(
        &mut self,
        prop: &MemberProp,
        env: &EnvRef,
        this: &Value,
    ) -> Result<String, Control> {
        match prop {
            MemberProp::Dot(name) => Ok(name.clone()),
            MemberProp::Index(e) => {
                let v = self.expr(e, env, this)?;
                Ok(to_string(&v))
            }
        }
    }

    fn assign_to(
        &mut self,
        target: &Expr,
        value: Value,
        env: &EnvRef,
        this: &Value,
    ) -> Result<(), Control> {
        match target {
            Expr::Ident(name) => {
                env_set(env, name, value);
                Ok(())
            }
            Expr::Member { obj, prop, .. } => {
                let base = self.expr(obj, env, this)?;
                let key = self.member_key(prop, env, this)?;
                self.set_member(&base, &key, value)
            }
            _ => Err(Self::throw_str("SyntaxError", "invalid assignment target")),
        }
    }

    fn unary(&mut self, op: UnOp, operand: &Expr, env: &EnvRef, this: &Value) -> EResult {
        if op == UnOp::TypeOf {
            // typeof tolerates undeclared identifiers.
            if let Expr::Ident(name) = operand {
                if env_get(env, name).is_none() {
                    return Ok(str_value("undefined"));
                }
            }
        }
        if op == UnOp::Delete {
            if let Expr::Member { obj, prop, .. } = operand {
                let base = self.expr(obj, env, this)?;
                let key = self.member_key(prop, env, this)?;
                if let Value::Obj(o) = &base {
                    let mut b = o.borrow_mut();
                    if let ObjKind::Array(items) = &mut b.kind {
                        if let Ok(i) = key.parse::<usize>() {
                            if i < items.len() {
                                items[i] = Value::Undefined;
                                return Ok(Value::Bool(true));
                            }
                        }
                    }
                    b.props.remove(&key);
                }
                return Ok(Value::Bool(true));
            }
            return Ok(Value::Bool(true));
        }
        let v = self.expr(operand, env, this)?;
        Ok(match op {
            UnOp::Neg => Value::Num(-to_number(&v)),
            UnOp::Plus => Value::Num(to_number(&v)),
            UnOp::Not => Value::Bool(!truthy(&v)),
            UnOp::BitNot => Value::Num(!to_int32(&v) as f64),
            UnOp::TypeOf => str_value(type_of(&v)),
            UnOp::Void => Value::Undefined,
            UnOp::Delete => Value::Bool(true),
        })
    }

    fn binary(&mut self, op: BinOp, l: Value, r: Value) -> EResult {
        use BinOp::*;
        Ok(match op {
            Add => {
                // String concatenation wins if either side is stringish.
                let l_str = matches!(l, Value::Str(_)) || matches!(l, Value::Obj(_));
                let r_str = matches!(r, Value::Str(_)) || matches!(r, Value::Obj(_));
                if l_str || r_str {
                    let mut s = to_string(&l);
                    s.push_str(&to_string(&r));
                    str_value(&s)
                } else {
                    Value::Num(to_number(&l) + to_number(&r))
                }
            }
            Sub => Value::Num(to_number(&l) - to_number(&r)),
            Mul => Value::Num(to_number(&l) * to_number(&r)),
            Div => Value::Num(to_number(&l) / to_number(&r)),
            Rem => Value::Num(to_number(&l) % to_number(&r)),
            Pow => Value::Num(f_pow(to_number(&l), to_number(&r))),
            Eq => Value::Bool(loose_eq(&l, &r)),
            Ne => Value::Bool(!loose_eq(&l, &r)),
            StrictEq => Value::Bool(strict_eq(&l, &r)),
            StrictNe => Value::Bool(!strict_eq(&l, &r)),
            Lt | Gt | Le | Ge => {
                let res = if let (Value::Str(a), Value::Str(b)) = (&l, &r) {
                    match op {
                        Lt => a < b,
                        Gt => a > b,
                        Le => a <= b,
                        _ => a >= b,
                    }
                } else {
                    let (a, b) = (to_number(&l), to_number(&r));
                    match op {
                        Lt => a < b,
                        Gt => a > b,
                        Le => a <= b,
                        _ => a >= b,
                    }
                };
                Value::Bool(res)
            }
            Shl => Value::Num(((to_int32(&l)) << (to_uint32(&r) & 31)) as f64),
            Shr => Value::Num(((to_int32(&l)) >> (to_uint32(&r) & 31)) as f64),
            UShr => Value::Num(((to_uint32(&l)) >> (to_uint32(&r) & 31)) as f64),
            BitAnd => Value::Num((to_int32(&l) & to_int32(&r)) as f64),
            BitOr => Value::Num((to_int32(&l) | to_int32(&r)) as f64),
            BitXor => Value::Num((to_int32(&l) ^ to_int32(&r)) as f64),
            In => {
                let key = to_string(&l);
                match &r {
                    Value::Obj(o) => {
                        let b = o.borrow();
                        let found = b.props.contains_key(&key)
                            || matches!(&b.kind, ObjKind::Array(items)
                                if key.parse::<usize>().is_ok_and(|i| i < items.len()));
                        Value::Bool(found)
                    }
                    _ => Value::Bool(false),
                }
            }
            InstanceOf => {
                let proto = match &r {
                    Value::Obj(f) => f.borrow().props.get("prototype").cloned(),
                    _ => None,
                };
                let Some(Value::Obj(proto)) = proto else {
                    return Ok(Value::Bool(false));
                };
                let mut cur = match &l {
                    Value::Obj(o) => o.borrow().proto.clone(),
                    _ => None,
                };
                let mut hops = 0;
                while let Some(p) = cur {
                    if Rc::ptr_eq(&p, &proto) {
                        return Ok(Value::Bool(true));
                    }
                    cur = p.borrow().proto.clone();
                    hops += 1;
                    if hops > 64 {
                        break;
                    }
                }
                Value::Bool(false)
            }
        })
    }

    // ── Property access ─────────────────────────────────────────────────────

    pub fn get_member(&mut self, base: &Value, key: &str) -> EResult {
        match base {
            Value::Undefined | Value::Null => Err(Self::throw_str(
                "TypeError",
                &format!("cannot read property `{key}` of {}", to_string(base)),
            )),
            Value::Str(s) => builtins::string_member(s, key),
            Value::Num(_) => builtins::number_member(key),
            Value::Bool(_) => Ok(Value::Undefined),
            Value::Obj(o) => {
                // Arrays: length and indices first.
                {
                    let b = o.borrow();
                    if let ObjKind::Array(items) = &b.kind {
                        if key == "length" {
                            return Ok(Value::Num(items.len() as f64));
                        }
                        if let Ok(i) = key.parse::<usize>() {
                            return Ok(items.get(i).cloned().unwrap_or(Value::Undefined));
                        }
                    }
                    if let Some(v) = b.props.get(key) {
                        return Ok(v.clone());
                    }
                }
                // Prototype chain.
                let mut cur = o.borrow().proto.clone();
                let mut hops = 0;
                while let Some(p) = cur {
                    if let Some(v) = p.borrow().props.get(key) {
                        return Ok(v.clone());
                    }
                    cur = p.borrow().proto.clone();
                    hops += 1;
                    if hops > 64 {
                        break;
                    }
                }
                // Built-in methods by object kind.
                let kind_method = {
                    let b = o.borrow();
                    match &b.kind {
                        ObjKind::Array(_) => builtins::array_member(key),
                        ObjKind::Function { .. } | ObjKind::Native(..) | ObjKind::Bound { .. } => {
                            builtins::function_member(key)
                        }
                        ObjKind::Plain => builtins::object_member(key),
                    }
                };
                Ok(kind_method.unwrap_or(Value::Undefined))
            }
            Value::Node(id) => dom_api::node_get(self, *id, key),
            Value::Document => dom_api::document_get(self, key),
            Value::StyleOf(id) => dom_api::style_get(self, *id, key),
            Value::Storage(area) => super::storage::get(self, *area, key),
        }
    }

    pub fn set_member(&mut self, base: &Value, key: &str, value: Value) -> Result<(), Control> {
        match base {
            Value::Obj(o) => {
                let mut b = o.borrow_mut();
                if let ObjKind::Array(items) = &mut b.kind {
                    if key == "length" {
                        let n = to_number(&value);
                        let n = if n.is_finite() && n >= 0.0 {
                            n as usize
                        } else {
                            0
                        };
                        items.resize(n.min(1 << 20), Value::Undefined);
                        return Ok(());
                    }
                    if let Ok(i) = key.parse::<usize>() {
                        if i < (1 << 20) {
                            if i >= items.len() {
                                items.resize(i + 1, Value::Undefined);
                            }
                            items[i] = value;
                            return Ok(());
                        }
                        return Ok(());
                    }
                }
                b.props.insert(key.to_string(), value);
                Ok(())
            }
            Value::Node(id) => dom_api::node_set(self, *id, key, value),
            Value::Document => dom_api::document_set(self, key, value),
            Value::StyleOf(id) => dom_api::style_set(self, *id, key, value),
            Value::Storage(area) => super::storage::set(self, *area, key, value),
            Value::Undefined | Value::Null => Err(Self::throw_str(
                "TypeError",
                &format!("cannot set property `{key}` of {}", to_string(base)),
            )),
            _ => Ok(()), // assignments to primitives are silently dropped
        }
    }

    // ── Calls ───────────────────────────────────────────────────────────────

    pub fn make_function(&mut self, def: Rc<FnDef>, env: &EnvRef, this: Option<Value>) -> Value {
        let o = new_obj(ObjKind::Function {
            def,
            env: env.clone(),
            this,
        });
        // Constructor functions get a fresh `.prototype` object.
        o.borrow_mut()
            .props
            .insert("prototype".into(), Value::Obj(new_plain()));
        Value::Obj(o)
    }

    pub fn call(&mut self, f: &Value, this: &Value, args: &[Value]) -> EResult {
        let Value::Obj(o) = f else {
            return Err(Self::throw_str(
                "TypeError",
                &format!("{} is not a function", to_string(f)),
            ));
        };
        enum Kind {
            Js(Rc<FnDef>, EnvRef, Option<Value>),
            Native(NativeFn, &'static str),
            Bound(Value, Value, Vec<Value>),
        }
        let kind = {
            let b = o.borrow();
            match &b.kind {
                ObjKind::Function { def, env, this } => {
                    Kind::Js(def.clone(), env.clone(), this.clone())
                }
                ObjKind::Native(f, name) => Kind::Native(*f, name),
                ObjKind::Bound {
                    target,
                    bound_this,
                    bound_args,
                } => Kind::Bound(target.clone(), bound_this.clone(), bound_args.clone()),
                _ => {
                    return Err(Self::throw_str(
                        "TypeError",
                        &format!("{} is not a function", to_string(f)),
                    ))
                }
            }
        };
        match kind {
            Kind::Bound(target, bound_this, bound_args) => {
                let mut all = bound_args;
                all.extend_from_slice(args);
                self.call(&target, &bound_this, &all)
            }
            Kind::Native(nf, name) => {
                self.depth += 1;
                if self.depth > MAX_CALL_DEPTH {
                    self.depth -= 1;
                    return Err(Control::Abort("call depth exceeded"));
                }
                let r = nf(self, this, args, name);
                self.depth -= 1;
                r
            }
            Kind::Js(def, captured, captured_this) => {
                self.depth += 1;
                if self.depth > MAX_CALL_DEPTH {
                    self.depth -= 1;
                    return Err(Control::Abort("call depth exceeded"));
                }
                let scope = Env::child(&captured, true);
                // Bind parameters (defaults fill missing arguments).
                let effective_this = captured_this.unwrap_or_else(|| this.clone());
                for (i, p) in def.params.iter().enumerate() {
                    let mut v = args.get(i).cloned().unwrap_or(Value::Undefined);
                    if matches!(v, Value::Undefined) {
                        if let Some(d) = &p.default {
                            v = match self.expr(d, &scope, &effective_this) {
                                Ok(v) => v,
                                Err(e) => {
                                    self.depth -= 1;
                                    return Err(e);
                                }
                            };
                        }
                    }
                    env_define(&scope, &p.name, v, false);
                }
                env_define(&scope, "arguments", new_array(args.to_vec()), false);
                let result = match &def.body {
                    FnBody::Expr(e) => self.expr(e, &scope, &effective_this),
                    FnBody::Block(stmts) => {
                        let r = self.hoist(stmts, &scope).and_then(|_| {
                            for s in stmts {
                                self.stmt(s, &scope, &effective_this)?;
                            }
                            Ok(())
                        });
                        match r {
                            Ok(()) => Ok(Value::Undefined),
                            Err(Control::Return(v)) => Ok(v),
                            Err(e) => Err(e),
                        }
                    }
                };
                self.depth -= 1;
                result
            }
        }
    }

    /// `new F(args)`: construct an object with `F.prototype`, call F with it.
    pub fn construct(&mut self, f: &Value, args: &[Value]) -> EResult {
        // Native constructors (Array, Object, Error, Date) handled here.
        if let Value::Obj(o) = f {
            let native = match &o.borrow().kind {
                ObjKind::Native(nf, name) => Some((*nf, *name)),
                _ => None,
            };
            if let Some((nf, name)) = native {
                return nf(self, &Value::Undefined, args, name);
            }
        }
        let this_obj = new_plain();
        if let Value::Obj(fo) = f {
            if let Some(Value::Obj(proto)) = fo.borrow().props.get("prototype") {
                this_obj.borrow_mut().proto = Some(proto.clone());
            }
        }
        let this_v = Value::Obj(this_obj);
        let r = self.call(f, &this_v, args)?;
        // A constructor returning an object overrides `this`.
        Ok(match r {
            Value::Obj(_) => r,
            _ => this_v,
        })
    }
}
