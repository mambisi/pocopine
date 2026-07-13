//! Outermost component-callback boundary and post-borrow safe point.
//!
//! Component handlers, typed handle updates, lifecycle hooks, watchers, and
//! component-backed renderers can all synchronously trigger follow-up work.
//! That work must not reconcile or tear down a component while one of those
//! callbacks still owns its `RefCell` borrow. [`ComponentCallbackFrame`]
//! provides one nested RAII boundary, and [`defer_component_callback`] appends
//! work to a single FIFO drained only after the outermost frame unwinds.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::reactive::ScopeId;

type DeferredCallback = Box<dyn FnOnce()>;

struct DeferredWork {
    owner: Option<ScopeId>,
    callback: DeferredCallback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FrameEntry {
    id: u64,
    scope_id: Option<ScopeId>,
}

#[derive(Default)]
struct CallbackState {
    next_frame_id: u64,
    frames: Vec<FrameEntry>,
    deferred: VecDeque<DeferredWork>,
    draining: bool,
}

thread_local! {
    /// One callback scheduler replaces the former per-scope active, pending,
    /// and draining side tables. Frame depth, active scope identities, and the
    /// FIFO live together, so nested cross-scope callbacks share one safe
    /// point and queued work never needs a `ScopeId` hash lookup.
    static CALLBACK_STATE: RefCell<CallbackState> = const { RefCell::new(CallbackState {
        next_frame_id: 1,
        frames: Vec::new(),
        deferred: VecDeque::new(),
        draining: false,
    }) };
}

/// RAII boundary for a callback that may hold a component-state borrow.
///
/// Nested frames share one FIFO. Dropping an inner frame never drains it;
/// dropping the outermost frame drains until stable, after the caller's local
/// `Ref`/`RefMut` values have gone out of scope. A frame entered while the FIFO
/// itself is draining also leaves draining to the existing outer loop.
#[must_use = "the callback frame must remain alive until the component borrow is released"]
pub struct ComponentCallbackFrame {
    id: u64,
}

impl ComponentCallbackFrame {
    /// Enter a callback boundary not tied to one component scope.
    ///
    /// Watch callbacks and renderer-owned callbacks use this form. Prefer
    /// [`Self::for_scope`] where a concrete component scope is known so
    /// same-scope handler re-entry can be deferred safely.
    pub fn enter() -> Self {
        Self::enter_with_scope(None)
    }

    /// Enter a callback boundary for `scope_id`.
    pub fn for_scope(scope_id: ScopeId) -> Self {
        Self::enter_with_scope(Some(scope_id))
    }

    fn enter_with_scope(scope_id: Option<ScopeId>) -> Self {
        let id = CALLBACK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let id = state.next_frame_id;
            state.next_frame_id = state.next_frame_id.wrapping_add(1).max(1);
            state.frames.push(FrameEntry { id, scope_id });
            id
        });
        Self { id }
    }
}

impl Drop for ComponentCallbackFrame {
    fn drop(&mut self) {
        let should_drain = CALLBACK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let popped = state
                .frames
                .pop()
                .expect("component callback frame stack underflow");
            assert_eq!(
                popped.id, self.id,
                "component callback frames must be dropped in LIFO order",
            );
            state.frames.is_empty() && !state.draining && !state.deferred.is_empty()
        });

        if !should_drain {
            return;
        }
        if std::thread::panicking() {
            CALLBACK_STATE.with(|state| state.borrow_mut().deferred.clear());
            return;
        }
        drain_deferred();
    }
}

/// Whether a callback frame currently owns (or may own) the state for
/// `scope_id`.
///
/// Scope dispatch uses this to defer only re-entry into an already-active
/// component. A nested callback for another scope remains synchronous while
/// still participating in the same outer safe point.
pub(crate) fn scope_is_active(scope_id: ScopeId) -> bool {
    CALLBACK_STATE.with(|state| {
        state
            .borrow()
            .frames
            .iter()
            .any(|frame| frame.scope_id == Some(scope_id))
    })
}

/// Whether code is currently inside a component callback or its FIFO drain.
pub fn component_callback_active() -> bool {
    CALLBACK_STATE.with(|state| {
        let state = state.borrow();
        !state.frames.is_empty() || state.draining
    })
}

/// Run `callback` after the outermost component callback releases its borrow.
///
/// With no active callback frame this runs immediately. During a callback or
/// an existing drain it appends to the shared FIFO. The scheduler removes each
/// closure from its `RefCell` before invoking it, so queued work may freely
/// open new frames or enqueue more work without a scheduler borrow conflict.
pub fn defer_component_callback(callback: impl FnOnce() + 'static) {
    defer_component_callback_inner(None, Box::new(callback));
}

/// Scope-owned form of [`defer_component_callback`].
///
/// Deferred work is skipped if `scope_id` has been torn down before its turn.
/// This is the right form for queued handler/event dispatch; editor-wide
/// reconciliation normally uses the unscoped form or its editor-root scope.
pub fn defer_component_callback_for(scope_id: ScopeId, callback: impl FnOnce() + 'static) {
    defer_component_callback_inner(Some(scope_id), Box::new(callback));
}

fn defer_component_callback_inner(owner: Option<ScopeId>, callback: DeferredCallback) {
    let mut work = Some(DeferredWork { owner, callback });
    let run_now = CALLBACK_STATE.with(|state| {
        let mut state = state.borrow_mut();
        if state.frames.is_empty() && !state.draining {
            true
        } else {
            state
                .deferred
                .push_back(work.take().expect("deferred work is present"));
            false
        }
    });
    if run_now {
        let work = work.expect("immediate callback is present");
        if work
            .owner
            .is_some_and(|scope_id| crate::scope::Scope::find(scope_id).is_none())
        {
            return;
        }
        (work.callback)();
    }
}

struct DrainGuard {
    finished: bool,
}

impl DrainGuard {
    fn begin() -> Option<Self> {
        CALLBACK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.draining || !state.frames.is_empty() {
                return None;
            }
            state.draining = true;
            Some(Self { finished: false })
        })
    }

    fn finish(mut self) {
        CALLBACK_STATE.with(|state| state.borrow_mut().draining = false);
        self.finished = true;
    }
}

impl Drop for DrainGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        CALLBACK_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.draining = false;
            // A callback panic aborts this safe point. Replaying leftover work
            // at an unrelated future boundary would apply stale mutations.
            state.deferred.clear();
        });
    }
}

fn drain_deferred() {
    let Some(guard) = DrainGuard::begin() else {
        return;
    };
    loop {
        let next = CALLBACK_STATE.with(|state| state.borrow_mut().deferred.pop_front());
        let Some(work) = next else {
            break;
        };
        if work
            .owner
            .is_some_and(|scope_id| crate::scope::Scope::find(scope_id).is_none())
        {
            continue;
        }
        (work.callback)();
    }
    guard.finish();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::{ComponentCallbackFrame, defer_component_callback};

    #[test]
    fn nested_frames_drain_once_in_fifo_order() {
        let order = Rc::new(RefCell::new(Vec::new()));
        {
            let _outer = ComponentCallbackFrame::enter();
            let order_for_first = Rc::clone(&order);
            defer_component_callback(move || order_for_first.borrow_mut().push(1));
            {
                let _inner = ComponentCallbackFrame::enter();
                let order_for_second = Rc::clone(&order);
                defer_component_callback(move || order_for_second.borrow_mut().push(2));
            }
            assert!(order.borrow().is_empty(), "inner drop must not drain");
            let order_for_third = Rc::clone(&order);
            defer_component_callback(move || order_for_third.borrow_mut().push(3));
        }
        assert_eq!(*order.borrow(), [1, 2, 3]);
    }

    #[test]
    fn work_enqueued_while_draining_appends_without_recursive_drain() {
        let order = Rc::new(RefCell::new(Vec::new()));
        {
            let _frame = ComponentCallbackFrame::enter();
            let order_for_first = Rc::clone(&order);
            defer_component_callback(move || {
                order_for_first.borrow_mut().push(1);
                let _nested_drain_frame = ComponentCallbackFrame::enter();
                let order_for_last = Rc::clone(&order_for_first);
                defer_component_callback(move || order_for_last.borrow_mut().push(3));
            });
            let order_for_second = Rc::clone(&order);
            defer_component_callback(move || order_for_second.borrow_mut().push(2));
        }
        assert_eq!(*order.borrow(), [1, 2, 3]);
    }

    #[test]
    fn no_frame_runs_work_immediately() {
        let ran = Rc::new(RefCell::new(false));
        let ran_for_callback = Rc::clone(&ran);
        defer_component_callback(move || *ran_for_callback.borrow_mut() = true);
        assert!(*ran.borrow());
    }
}
