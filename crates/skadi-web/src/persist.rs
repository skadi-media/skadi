//! Page-lifetime state that survives a re-render (SKADI-T-0596).
//!
//! The detail pages poll every few seconds and rebuild their body from the
//! fresh payload, so any component inside — a release search's results, an
//! opened History panel, the "grab manually" disclosure — was recreated with
//! blank state on every tick. Rather than restructure every page around keyed
//! rows, a component can keep the bits that matter here, keyed by the item
//! they belong to, and pick them up again when it is recreated. The cache is
//! per page load (it lives in the wasm module), never persisted.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;

use leptos::prelude::*;

thread_local! {
    static CACHE: RefCell<HashMap<String, Box<dyn Any>>> = RefCell::new(HashMap::new());
}

/// Store `value` under `key`, replacing anything there.
pub fn remember<T: Clone + 'static>(key: &str, value: T) {
    CACHE.with(|c| {
        c.borrow_mut().insert(key.to_string(), Box::new(value));
    });
}

/// The value stored under `key`, if one of this type is there.
#[must_use]
pub fn recall<T: Clone + 'static>(key: &str) -> Option<T> {
    CACHE.with(|c| {
        c.borrow()
            .get(key)
            .and_then(|b| b.downcast_ref::<T>())
            .cloned()
    })
}

/// Drop everything stored — for tests.
pub fn clear() {
    CACHE.with(|c| c.borrow_mut().clear());
}

/// A signal whose value is restored from the cache on creation and written
/// back on every change, so a component recreated by a re-render resumes
/// where it left off. `default` applies the first time only.
pub fn persisted<T: Clone + Send + Sync + 'static>(key: String, default: T) -> RwSignal<T> {
    let initial = recall::<T>(&key).unwrap_or(default);
    let sig = RwSignal::new(initial);
    Effect::new(move |_| {
        let v = sig.get();
        remember(&key, v);
    });
    sig
}
