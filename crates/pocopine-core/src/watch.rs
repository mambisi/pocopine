//! `watch` — imperative change listener.
//!
//! Reads `source()` inside an effect; when the returned value differs from
//! the previous run, calls `cb(new, previous)`. Returns the backing
//! [`EffectId`] so callers can `release` the watcher when they're done
//! with it.

use std::cell::RefCell;
use std::rc::Rc;

use crate::reactive::{effect, EffectId};

/// Watch `source` and call `cb` whenever its value changes.
///
/// `cb` fires once on the initial run (with `previous = None`), then once
/// per distinct subsequent value. Equality is checked with `PartialEq`.
pub fn watch<T, S, C>(source: S, cb: C) -> EffectId
where
    T: Clone + PartialEq + 'static,
    S: Fn() -> T + 'static,
    C: Fn(&T, Option<&T>) + 'static,
{
    let prev: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
    let prev_w = prev.clone();
    effect(move || {
        let next = source();
        let last = prev_w.borrow().clone();
        if last.as_ref() != Some(&next) {
            cb(&next, last.as_ref());
            *prev_w.borrow_mut() = Some(next);
        }
    })
}
