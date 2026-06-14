//! Web Storage (`localStorage` / `sessionStorage`).
//!
//! Each area is a string→string map owned by the [`super::Runtime`] and kept
//! alive for the page's lifetime, so values survive across script runs and
//! event dispatches. A [`Value::Storage`] handle routes property access here
//! (mirroring how `element.style` routes to [`super::dom_api`]), so both the
//! method API (`getItem`/`setItem`/…) and bracket/dot access (`store.key`,
//! `store.length`) work.

use alloc::collections::BTreeMap;
use alloc::string::String;

use super::interp::{Control, EResult, Interp};
use super::value::{native, str_value, to_number, to_string, StorageArea, Value};

/// Both storage areas, owned by the runtime.
#[derive(Default)]
pub struct Storage {
    pub local: BTreeMap<String, String>,
    pub session: BTreeMap<String, String>,
}

impl Storage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn area(&mut self, area: StorageArea) -> &mut BTreeMap<String, String> {
        match area {
            StorageArea::Local => &mut self.local,
            StorageArea::Session => &mut self.session,
        }
    }

    pub fn area_ref(&self, area: StorageArea) -> &BTreeMap<String, String> {
        match area {
            StorageArea::Local => &self.local,
            StorageArea::Session => &self.session,
        }
    }
}

const METHODS: &[&str] = &["getItem", "setItem", "removeItem", "clear", "key"];

/// `store[key]` / `store.key`: a method, `length`, or the stored item.
pub fn get(it: &mut Interp, area: StorageArea, key: &str) -> EResult {
    if let Some(&m) = METHODS.iter().find(|&&m| m == key) {
        return Ok(native(method, m));
    }
    let map = it.storage.area_ref(area);
    if key == "length" {
        return Ok(Value::Num(map.len() as f64));
    }
    Ok(map.get(key).map(|v| str_value(v)).unwrap_or(Value::Null))
}

/// `store[key] = value` / `store.key = value`: a `setItem`.
pub fn set(it: &mut Interp, area: StorageArea, key: &str, value: Value) -> Result<(), Control> {
    if key == "length" {
        return Ok(());
    }
    let v = to_string(&value);
    it.storage.area(area).insert(String::from(key), v);
    Ok(())
}

/// Dispatcher for the Storage method family; `this` carries the area.
fn method(it: &mut Interp, this: &Value, args: &[Value], name: &'static str) -> EResult {
    let Value::Storage(area) = this else {
        return Ok(Value::Undefined);
    };
    let area = *area;
    let arg = |i: usize| args.get(i).cloned().unwrap_or(Value::Undefined);
    match name {
        "getItem" => {
            let k = to_string(&arg(0));
            Ok(it
                .storage
                .area_ref(area)
                .get(&k)
                .map(|v| str_value(v))
                .unwrap_or(Value::Null))
        }
        "setItem" => {
            it.storage
                .area(area)
                .insert(to_string(&arg(0)), to_string(&arg(1)));
            Ok(Value::Undefined)
        }
        "removeItem" => {
            it.storage.area(area).remove(&to_string(&arg(0)));
            Ok(Value::Undefined)
        }
        "clear" => {
            it.storage.area(area).clear();
            Ok(Value::Undefined)
        }
        "key" => {
            let i = to_number(&arg(0));
            if !i.is_finite() || i < 0.0 {
                return Ok(Value::Null);
            }
            Ok(it
                .storage
                .area_ref(area)
                .keys()
                .nth(i as usize)
                .map(|k| str_value(k))
                .unwrap_or(Value::Null))
        }
        _ => Ok(Value::Undefined),
    }
}
