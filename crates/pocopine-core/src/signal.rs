//! Signals — typed reactive cells.
//!
//! A signal is a `(Signal<T>, Setter<T>)` pair. Reads through `Signal::get`
//! subscribe the current effect; writes through `Setter::set` notify
//! subscribers. Internally they share a `Rc<RefCell<T>>` and a single
//! [`SignalId`]; dep tracking rides a dedicated signal-specific table
//! ([`crate::reactive::track_signal`] / [`crate::reactive::trigger_signal`])
//! keyed on `SignalId` directly. Batching, flushing, and cleanup behave
//! exactly as they do for proxy-based scopes — they share the effect
//! engine, just not the key type.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::reactive::{next_signal_id, track_signal, trigger_signal, SignalId};

/// Read handle for a reactive cell. Clone freely; all clones see the same
/// value and share the same id.
///
/// Optionally carries a **drop guard** — a type-erased payload that
/// lives as long as any `Signal` clone exists, and drops with the
/// last one. Used by bridges that wrap an external reactive source
/// (e.g. `QueryView::into_signal()`) — the bridge stows the source
/// handle + the on-update subscription token inside the guard so the
/// subscription is automatically torn down when the last reader of
/// the signal disappears. `signal()` constructs guard-less signals;
/// attach one via [`Signal::with_drop_guard`].
pub struct Signal<T> {
    id: SignalId,
    cell: Rc<RefCell<T>>,
    drop_guard: Option<Rc<dyn Any>>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Signal {
            id: self.id,
            cell: self.cell.clone(),
            drop_guard: self.drop_guard.clone(),
        }
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
        Setter {
            id: self.id,
            cell: self.cell.clone(),
        }
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
        RwSignal {
            id: self.id,
            cell: self.cell.clone(),
        }
    }
}

/// Create a split read/write pair initialized to `initial`.
pub fn signal<T: 'static>(initial: T) -> (Signal<T>, Setter<T>) {
    let id = next_signal_id();
    let cell = Rc::new(RefCell::new(initial));
    (
        Signal {
            id,
            cell: cell.clone(),
            drop_guard: None,
        },
        Setter { id, cell },
    )
}

/// Create a combined read+write handle initialized to `initial`.
pub fn rw_signal<T: 'static>(initial: T) -> RwSignal<T> {
    let id = next_signal_id();
    let cell = Rc::new(RefCell::new(initial));
    RwSignal { id, cell }
}

impl<T: Clone + 'static> Signal<T> {
    /// Subscribe + read. Returns a cloned value; use [`Signal::with`] to
    /// avoid the clone for non-`Clone` `T`.
    pub fn get(&self) -> T {
        track_signal(self.id);
        self.cell.borrow().clone()
    }
}

impl<T: 'static> Signal<T> {
    /// Subscribe + borrow the inner value for the duration of `f`.
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        track_signal(self.id);
        f(&self.cell.borrow())
    }

    /// The signal's stable id — useful for building key strings in
    /// integration tests. Not part of the public reactive surface.
    pub fn id(&self) -> SignalId {
        self.id
    }

    /// Attach a type-erased payload whose lifetime is bound to the
    /// signal — it drops with the last `Signal` clone. Used by
    /// bridges like `QueryView::into_signal()` to keep the external
    /// source + its on-update subscription alive for exactly as long
    /// as any reader of the signal.
    ///
    /// Calling this on a signal that already has a guard replaces
    /// it; the previous guard drops immediately (if no other clone
    /// is still holding it).
    pub fn with_drop_guard<G: 'static>(mut self, guard: G) -> Self {
        self.drop_guard = Some(Rc::new(guard));
        self
    }
}

impl<T: PartialEq + 'static> Setter<T> {
    /// Replace the stored value; notify subscribers only when the
    /// new value differs from the current one. The equality check
    /// eliminates a class of render-loop / watcher-thrash bugs:
    /// an effect that writes back a value it just read (even when
    /// the value is unchanged) used to re-fire every subscriber
    /// and, in a chain, spin.
    ///
    /// For the rare "I want to re-fire even on an identical value"
    /// case, use [`Setter::set_force`].
    pub fn set(&self, value: T) {
        if *self.cell.borrow() == value {
            return;
        }
        *self.cell.borrow_mut() = value;
        trigger_signal(self.id);
    }
}

impl<T: 'static> Setter<T> {
    /// Replace the stored value and unconditionally notify
    /// subscribers — the escape hatch from [`Setter::set`]'s
    /// value-equality guard. Use when a subscriber must re-fire
    /// even for the identical value (rare; e.g. force-replaying
    /// the last effect run).
    pub fn set_force(&self, value: T) {
        *self.cell.borrow_mut() = value;
        trigger_signal(self.id);
    }

    /// Mutate in place via `f`, then notify subscribers. Always
    /// fires — the closure can't prove the value didn't change
    /// without us reading twice (pre-mutation snapshot + post-
    /// mutation compare), which would defeat the point of `update`
    /// for non-`Clone` / non-`PartialEq` `T`.
    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.cell.borrow_mut());
        trigger_signal(self.id);
    }

    pub fn id(&self) -> SignalId {
        self.id
    }
}

impl<T: Clone + 'static> RwSignal<T> {
    pub fn get(&self) -> T {
        track_signal(self.id);
        self.cell.borrow().clone()
    }
}

impl<T: PartialEq + 'static> RwSignal<T> {
    /// See [`Setter::set`] — value-equality guard identical shape.
    pub fn set(&self, value: T) {
        if *self.cell.borrow() == value {
            return;
        }
        *self.cell.borrow_mut() = value;
        trigger_signal(self.id);
    }
}

impl<T: 'static> RwSignal<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        track_signal(self.id);
        f(&self.cell.borrow())
    }

    /// See [`Setter::set_force`].
    pub fn set_force(&self, value: T) {
        *self.cell.borrow_mut() = value;
        trigger_signal(self.id);
    }

    pub fn update(&self, f: impl FnOnce(&mut T)) {
        f(&mut self.cell.borrow_mut());
        trigger_signal(self.id);
    }

    pub fn id(&self) -> SignalId {
        self.id
    }

    /// Split into separate read / write halves. The resulting pair shares
    /// storage with `self`.
    pub fn split(self) -> (Signal<T>, Setter<T>) {
        (
            Signal {
                id: self.id,
                cell: self.cell.clone(),
                drop_guard: None,
            },
            Setter {
                id: self.id,
                cell: self.cell,
            },
        )
    }
}
