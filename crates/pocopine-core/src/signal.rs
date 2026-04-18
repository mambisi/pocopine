//! Signals — typed reactive cells.
//!
//! A signal is a `(Signal<T>, Setter<T>)` pair. Reads through `Signal::get`
//! subscribe the current effect; writes through `Setter::set` notify
//! subscribers. Internally they share a `Rc<RefCell<T>>` and a single
//! [`SignalId`]; dep tracking rides the effect engine via the synthetic
//! [`crate::reactive::SIGNAL_SCOPE`], so batching, flushing, and cleanup
//! behave exactly as they do for proxy-based scopes.

use std::cell::RefCell;
use std::rc::Rc;

use crate::reactive::{next_signal_id, track, trigger, SignalId, SIGNAL_SCOPE};

/// Read handle for a reactive cell. Clone freely; all clones see the same
/// value and share the same id.
pub struct Signal<T> {
    id: SignalId,
    cell: Rc<RefCell<T>>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Signal { id: self.id, cell: self.cell.clone() }
    }
}

/// Write handle for a reactive cell. Split from [`Signal`] so functions
/// that only need read access can advertise that in their type.
pub struct Setter<T> {
    id: SignalId,
    cell: Rc<RefCell<T>>,
}

impl<T> Clone for Setter<T> {
    fn clone(&self) -> Self {
        Setter { id: self.id, cell: self.cell.clone() }
    }
}

/// Combined read+write handle. Prefer `signal()` for APIs that want to
/// enforce read-only access; reach for `rw_signal()` only when ergonomics
/// win out (tight loops, short-lived locals).
pub struct RwSignal<T> {
    id: SignalId,
    cell: Rc<RefCell<T>>,
}

impl<T> Clone for RwSignal<T> {
    fn clone(&self) -> Self {
        RwSignal { id: self.id, cell: self.cell.clone() }
    }
}

/// Create a split read/write pair initialized to `initial`.
pub fn signal<T: 'static>(initial: T) -> (Signal<T>, Setter<T>) {
    let id = next_signal_id();
    let cell = Rc::new(RefCell::new(initial));
    (
        Signal { id, cell: cell.clone() },
        Setter { id, cell },
    )
}

/// Create a combined read+write handle initialized to `initial`.
pub fn rw_signal<T: 'static>(initial: T) -> RwSignal<T> {
    let id = next_signal_id();
    let cell = Rc::new(RefCell::new(initial));
    RwSignal { id, cell }
}

fn key_of(id: SignalId) -> String {
    // IDs stringified at the dep-map boundary. Measured as the cheapest
    // shape for v0; can swap for a non-string key kind once benchmarks
    // tell us it matters.
    id.0.to_string()
}

impl<T: Clone + 'static> Signal<T> {
    /// Subscribe + read. Returns a cloned value; use [`Signal::with`] to
    /// avoid the clone for non-`Clone` `T`.
    pub fn get(&self) -> T {
        track(SIGNAL_SCOPE, &key_of(self.id));
        self.cell.borrow().clone()
    }
}

impl<T: 'static> Signal<T> {
    /// Subscribe + borrow the inner value for the duration of `f`.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        track(SIGNAL_SCOPE, &key_of(self.id));
        f(&self.cell.borrow())
    }

    /// The signal's stable id — useful for building key strings in
    /// integration tests. Not part of the public reactive surface.
    pub fn id(&self) -> SignalId {
        self.id
    }
}

impl<T: 'static> Setter<T> {
    /// Replace the stored value and notify subscribers.
    pub fn set(&self, value: T) {
        *self.cell.borrow_mut() = value;
        trigger(SIGNAL_SCOPE, &key_of(self.id));
    }

    /// Mutate in place via `f`, then notify subscribers.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.cell.borrow_mut());
        trigger(SIGNAL_SCOPE, &key_of(self.id));
    }

    pub fn id(&self) -> SignalId {
        self.id
    }
}

impl<T: Clone + 'static> RwSignal<T> {
    pub fn get(&self) -> T {
        track(SIGNAL_SCOPE, &key_of(self.id));
        self.cell.borrow().clone()
    }
}

impl<T: 'static> RwSignal<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        track(SIGNAL_SCOPE, &key_of(self.id));
        f(&self.cell.borrow())
    }

    pub fn set(&self, value: T) {
        *self.cell.borrow_mut() = value;
        trigger(SIGNAL_SCOPE, &key_of(self.id));
    }

    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.cell.borrow_mut());
        trigger(SIGNAL_SCOPE, &key_of(self.id));
    }

    pub fn id(&self) -> SignalId {
        self.id
    }

    /// Split into separate read / write halves. The resulting pair shares
    /// storage with `self`.
    pub fn split(self) -> (Signal<T>, Setter<T>) {
        (
            Signal { id: self.id, cell: self.cell.clone() },
            Setter { id: self.id, cell: self.cell },
        )
    }
}
