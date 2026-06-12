//! Event registration and dispatch.
//!
//! [`Handlers`] stores every listener a page registers — via
//! `addEventListener` (multiple per target), `on*` property assignment
//! (single, replaceable), or `on*=""` HTML attributes (compiled lazily at
//! dispatch). [`dispatch`] implements the bubbling phase: the event travels
//! from the target up its ancestor chain, then `document`, then `window`,
//! honouring `stopPropagation`; `preventDefault` (or `return false` from a
//! property/attribute handler) cancels the default action, which the browser
//! reads from the outcome to decide whether to navigate/submit.
//!
//! Handler exceptions are reported to the console and never unwind into the
//! caller; aborts (budget/depth) stop the dispatch.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::{FnBody, FnDef, Param};
use super::interp::{Control, Interp};
use super::parser;
use super::value::*;

// ── Timer queue ──────────────────────────────────────────────────────────────

pub struct PendingTimer {
    pub id: u32,
    pub deadline_ms: u64,
    pub callback: Value,
    pub interval_ms: Option<u64>,
}

pub struct TimerQueue {
    pending: Vec<PendingTimer>,
    next_id: u32,
}

impl TimerQueue {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            next_id: 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn schedule(
        &mut self,
        callback: Value,
        now_ms: u64,
        delay_ms: u64,
        interval_ms: Option<u64>,
    ) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending.push(PendingTimer {
            id,
            deadline_ms: now_ms + delay_ms,
            callback,
            interval_ms,
        });
        id
    }

    pub fn cancel(&mut self, id: u32) {
        self.pending.retain(|t| t.id != id);
    }

    pub fn take_expired(&mut self, now_ms: u64) -> Vec<PendingTimer> {
        let mut expired = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].deadline_ms <= now_ms {
                let timer = self.pending.remove(i);
                if let Some(interval) = timer.interval_ms {
                    // Re-arm before returning so cancel() still works.
                    self.pending.push(PendingTimer {
                        id: timer.id,
                        deadline_ms: now_ms + interval,
                        callback: timer.callback.clone(),
                        interval_ms: Some(interval),
                    });
                    expired.push(PendingTimer {
                        id: timer.id,
                        deadline_ms: timer.deadline_ms,
                        callback: timer.callback,
                        interval_ms: Some(interval),
                    });
                } else {
                    expired.push(timer);
                }
            } else {
                i += 1;
            }
        }
        expired
    }
}

/// What an event is aimed at.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Node(usize),
    Document,
    Window,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// `el.onclick = f` / `document.onclick = f`: one per (target, type).
    Prop,
    /// `addEventListener`: any number per (target, type).
    Listener,
}

struct Entry {
    target: Target,
    ty: String,
    slot: Slot,
    f: Value,
}

/// All handlers registered by the page's scripts.
pub struct Handlers {
    entries: Vec<Entry>,
    pub timers: TimerQueue,
}

impl Handlers {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            timers: TimerQueue::new(),
        }
    }

    pub fn add_listener(&mut self, target: Target, ty: &str, f: Value) {
        if !matches!(type_of(&f), "function") {
            return;
        }
        self.entries.push(Entry {
            target,
            ty: ty.to_ascii_lowercase(),
            slot: Slot::Listener,
            f,
        });
    }

    pub fn remove_listener(&mut self, target: Target, ty: &str, f: &Value) {
        let ty = ty.to_ascii_lowercase();
        self.entries.retain(|e| {
            !(e.target == target && e.ty == ty && e.slot == Slot::Listener && strict_eq(&e.f, f))
        });
    }

    /// Assign an `on<type>` property: replaces any previous property handler;
    /// a non-function value clears it.
    pub fn set_prop(&mut self, target: Target, ty: &str, f: Value) {
        let ty = ty.to_ascii_lowercase();
        self.entries
            .retain(|e| !(e.target == target && e.ty == ty && e.slot == Slot::Prop));
        if matches!(type_of(&f), "function") {
            self.entries.push(Entry {
                target,
                ty,
                slot: Slot::Prop,
                f,
            });
        }
    }

    /// Handlers for one target/type, with whether each is property-like
    /// (where `return false` cancels the default action).
    fn collect(&self, target: Target, ty: &str) -> Vec<(Value, bool)> {
        self.entries
            .iter()
            .filter(|e| e.target == target && e.ty == ty)
            .map(|e| (e.f.clone(), e.slot == Slot::Prop))
            .collect()
    }

    /// Node ids carrying any click handler — the flattener marks their text
    /// as clickable so the renderer emits hit regions for them.
    pub fn click_targets(&self) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .entries
            .iter()
            .filter_map(|e| match (e.target, e.ty.as_str()) {
                (Target::Node(id), "click") => Some(id),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

}

/// Result of a dispatch, read by the browser to decide on default actions.
#[derive(Clone, Copy, Default)]
pub struct DispatchOutcome {
    /// At least one handler actually ran (the page may need re-rendering).
    pub fired: bool,
    /// `preventDefault()` was called or a property handler returned `false`.
    pub prevented: bool,
}

/// Dispatch `ty` at `target`, bubbling to document and window.
pub fn dispatch(it: &mut Interp, target: Target, ty: &str) -> DispatchOutcome {
    dispatch_with_props(it, target, ty, &[])
}

/// Dispatch `ty` at `target` with extra key/value pairs injected into the
/// event object (used for keyboard events etc.).
pub fn dispatch_with_props(
    it: &mut Interp,
    target: Target,
    ty: &str,
    extra: &[(&str, Value)],
) -> DispatchOutcome {
    let ty = ty.to_ascii_lowercase();
    let mut outcome = DispatchOutcome::default();

    // The event object handlers receive; preventDefault/stopPropagation set
    // flags on it that we read back between levels.
    let ev = new_plain();
    {
        let mut e = ev.borrow_mut();
        e.props.insert("type".into(), str_value(&ty));
        e.props.insert(
            "target".into(),
            match target {
                Target::Node(id) => Value::Node(id),
                Target::Document => Value::Document,
                Target::Window => env_get(&it.global, "window").unwrap_or(Value::Undefined),
            },
        );
        e.props
            .insert("defaultPrevented".into(), Value::Bool(false));
        e.props.insert("cancelBubble".into(), Value::Bool(false));
        e.props
            .insert("preventDefault".into(), native(event_fn, "preventDefault"));
        e.props
            .insert("stopPropagation".into(), native(event_fn, "stopPropagation"));
        e.props.insert(
            "stopImmediatePropagation".into(),
            native(event_fn, "stopPropagation"),
        );
        // Inject extra properties (e.g. key, keyCode for keyboard events).
        for (k, v) in extra {
            e.props.insert((*k).into(), v.clone());
        }
    }
    let event = Value::Obj(ev.clone());

    for level in bubble_chain(it, target) {
        let (this_v, level_target) = match level {
            Target::Node(id) => (Value::Node(id), level),
            Target::Document => (Value::Document, level),
            Target::Window => (
                env_get(&it.global, "window").unwrap_or(Value::Undefined),
                level,
            ),
        };
        ev.borrow_mut()
            .props
            .insert("currentTarget".into(), this_v.clone());

        let mut handlers = it.handlers.collect(level_target, &ty);
        // `on<type>=""` HTML attribute, compiled on demand (only when no
        // property handler has replaced it).
        if let Target::Node(id) = level {
            let has_prop = handlers.iter().any(|(_, p)| *p);
            if !has_prop {
                if let Some(f) = compile_attr_handler(it, id, &ty) {
                    handlers.push((f, true));
                }
            }
        }
        // `window.onload = …` lands on the plain window object's props.
        if level == Target::Window {
            if let Some(Value::Obj(w)) = env_get(&it.global, "window") {
                let key = format!("on{ty}");
                let f = w.borrow().props.get(&key).cloned();
                if let Some(f) = f {
                    if matches!(type_of(&f), "function") {
                        handlers.push((f, true));
                    }
                }
            }
        }

        for (f, prop_like) in handlers {
            outcome.fired = true;
            match it.call(&f, &this_v, &[event.clone()]) {
                Ok(r) => {
                    if prop_like && matches!(r, Value::Bool(false)) {
                        ev.borrow_mut()
                            .props
                            .insert("defaultPrevented".into(), Value::Bool(true));
                    }
                }
                Err(Control::Throw(v)) => it
                    .console
                    .push(format!("Uncaught (in {ty} handler) {}", to_string(&v))),
                Err(Control::Abort(why)) => {
                    it.console.push(format!("[handler aborted] {why}"));
                    outcome.prevented |= ev_flag(&ev, "defaultPrevented");
                    return outcome;
                }
                Err(_) => {}
            }
        }

        if ev_flag(&ev, "cancelBubble") {
            break;
        }
    }
    outcome.prevented = ev_flag(&ev, "defaultPrevented");
    outcome
}

fn ev_flag(ev: &Obj, key: &str) -> bool {
    ev.borrow()
        .props
        .get(key)
        .map(truthy)
        .unwrap_or(false)
}

fn event_fn(_it: &mut Interp, this: &Value, _args: &[Value], name: &'static str) -> super::interp::EResult {
    if let Value::Obj(o) = this {
        let key = match name {
            "preventDefault" => "defaultPrevented",
            _ => "cancelBubble",
        };
        o.borrow_mut().props.insert(key.into(), Value::Bool(true));
    }
    Ok(Value::Undefined)
}

/// Bubbling path: target, element ancestors, document, window.
fn bubble_chain(it: &Interp, target: Target) -> Vec<Target> {
    let mut chain = Vec::new();
    match target {
        Target::Node(id) => {
            let mut cur = id;
            loop {
                if it.dom.element(cur).is_some() {
                    chain.push(Target::Node(cur));
                }
                let p = it.dom.nodes[cur].parent;
                if p == usize::MAX {
                    break;
                }
                cur = p;
            }
            chain.push(Target::Document);
            chain.push(Target::Window);
        }
        Target::Document => {
            chain.push(Target::Document);
            chain.push(Target::Window);
        }
        Target::Window => chain.push(Target::Window),
    }
    chain
}

/// Compile an `on<type>` attribute's source into a callable, if present.
fn compile_attr_handler(it: &mut Interp, id: usize, ty: &str) -> Option<Value> {
    let src = it
        .dom
        .element(id)
        .and_then(|el| el.attr(&format!("on{ty}")))
        .map(String::from)?;
    match parser::parse_program(&src) {
        Ok(stmts) => {
            let def = FnDef {
                name: None,
                params: alloc::vec![Param {
                    name: String::from("event"),
                    default: None,
                }],
                body: FnBody::Block(stmts),
                is_arrow: false,
            };
            let global = it.global.clone();
            Some(it.make_function(Rc::new(def), &global, None))
        }
        Err(e) => {
            it.console
                .push(format!("[handler error] on{ty}: {e}"));
            None
        }
    }
}
