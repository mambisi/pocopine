//! Per-signal "last changed at" timestamp side-table.
//!
//! The devtools signal graph panel shows each signal's subscriber
//! count + when it was last written. `reactive::trigger_signal`
//! fires `hooks::on_signal_trigger`, which lands here via
//! [`record`]. Lives in a thread-local HashMap so the hot path
//! pays one `performance.now()` call + one HashMap insert per
//! signal trigger — only when devtools is installed and feature
//! is on.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::reactive::SignalId;

use super::ring;

thread_local! {
    static LAST_CHANGED: RefCell<HashMap<SignalId, f64>> =
        RefCell::new(HashMap::new());
}

/// Stamp `id` with the current high-resolution timestamp. Called
/// by the devtools `on_signal_trigger` handler installed at
/// [`super::install`] time.
pub(super) fn record(id: SignalId) {
    let t = ring::now_ms_for_scope();
    LAST_CHANGED.with(|m| {
        m.borrow_mut().insert(id, t);
    });
}

/// Lookup the last-changed timestamp for `id`, if any. Consumed by
/// the signal graph panel (PR D).
#[allow(dead_code)] // used by PR D
pub(super) fn get(id: SignalId) -> Option<f64> {
    LAST_CHANGED.with(|m| m.borrow().get(&id).copied())
}

/// Whole-table snapshot for the signal graph panel.
#[allow(dead_code)] // used by PR D
pub(super) fn snapshot() -> HashMap<SignalId, f64> {
    LAST_CHANGED.with(|m| m.borrow().clone())
}
