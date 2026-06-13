//! Computed — lazy, memoized derivations over signals and proxy fields.
//!
//! A [`Computed`] is a thin wrapper over a lazy [`crate::reactive::effect`]
//! with a custom scheduler. The effect body fills a cache; the scheduler
//! flips a `dirty` bit and re-triggers the computed's own synthetic signal
//! key. Callers of `get()` read the cache on clean reads and re-run the
//! effect on dirty reads, so sources are only re-evaluated when something
//! actually reads the derived value after an input changed.
//!
//! Drop behavior: releasing a `Computed` releases its internal effect, so
//! scopes of parent effects stay tidy without a manual teardown step.

use std::cell::RefCell;
use std::rc::Rc;

use crate::reactive::{
    EffectId, EffectOptions, SignalId, effect_with, next_signal_id, release, run_now, track_signal,
    trigger_signal,
};

struct State<T> {
    cached: Option<T>,
    dirty: bool,
}

/// Memoized derivation. Construct via [`computed`].
pub struct Computed<T: 'static> {
    id: SignalId,
    effect_id: EffectId,
    state: Rc<RefCell<State<T>>>,
}

/// Build a memoized derivation. `f` is run the first time `Computed::get`
/// is called (lazy) and re-run whenever one of its inputs changes *and*
/// someone reads the result again.
pub fn computed<T: Clone + 'static>(f: impl Fn() -> T + 'static) -> Computed<T> {
    let id = next_signal_id();
    let state: Rc<RefCell<State<T>>> = Rc::new(RefCell::new(State {
        cached: None,
        dirty: true,
    }));

    let state_for_body = state.clone();
    let state_for_sched = state.clone();

    let effect_id = effect_with(
        move || {
            let v = f();
            let mut s = state_for_body.borrow_mut();
            s.cached = Some(v);
            s.dirty = false;
        },
        EffectOptions {
            lazy: true,
            scheduler: Some(Rc::new(move |_eid: EffectId| {
                state_for_sched.borrow_mut().dirty = true;
                // Tell our own subscribers the value has (potentially)
                // changed. They'll call back into `get()` and we'll rerun
                // the effect lazily if dirty is still true at that point.
                trigger_signal(id);
            })),
        },
    );

    Computed {
        id,
        effect_id,
        state,
    }
}

impl<T: Clone + 'static> Computed<T> {
    /// Subscribe the current effect and return the latest value. Runs the
    /// derivation only if a dep has changed since the last read.
    pub fn get(&self) -> T {
        let dirty = self.state.borrow().dirty;
        if dirty {
            run_now(self.effect_id);
        }
        track_signal(self.id);
        self.state
            .borrow()
            .cached
            .clone()
            .expect("computed body failed to populate the cache")
    }
}

impl<T: 'static> Computed<T> {
    pub fn id(&self) -> SignalId {
        self.id
    }
}

impl<T: 'static> Drop for Computed<T> {
    fn drop(&mut self) {
        release(self.effect_id);
    }
}
