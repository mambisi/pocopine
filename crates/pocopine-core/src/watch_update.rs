//! Snapshot inputs and restricted multi-field patches for generated watchers.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;

use crate::reactive::{EffectId, ScopeId};
use crate::{Handle, Scope};

/// A watched field at this invocation and at the previous invocation.
/// `previous` is `None` for the initial notification. Unchanged inputs in a
/// multi-field watch still carry their current value and previous snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change<T> {
    pub current: T,
    pub previous: Option<T>,
}

impl<T: PartialEq> Change<T> {
    pub fn changed(&self) -> bool {
        self.previous.as_ref() != Some(&self.current)
    }
}

/// The generated bundle of field changes for a bare `#[watch]` observer.
/// The macro supplies the policy parameter in `Changes<Self>` signatures.
pub type Changes<C, W> = <W as WatchAllSpec<C>>::Changes;

#[doc(hidden)]
pub trait WatchAllSpec<C> {
    type Changes;
    type History;
    const FIELDS: &'static [&'static str];
    fn read(state: &C, previous: Option<&Self::History>) -> (Self::Changes, Self::History);
}

/// A watcher-specific patch. In `#[watch]` signatures, `Update<Self>` is
/// expanded to include the generated policy. Fluent setters exist only for
/// the fields declared in `writes(...)`; omitted fields remain unchanged.
/// An optional field uses an outer option for presence, so explicitly setting
/// it to `None` remains different from omitting it.
#[must_use = "return the update from the watcher so the runtime can apply it"]
pub struct Update<C, W: WatchSpec<C>> {
    patch: W::Patch,
    component: PhantomData<fn() -> C>,
}

impl<C, W: WatchSpec<C>> Default for Update<C, W> {
    fn default() -> Self {
        Self {
            patch: W::Patch::default(),
            component: PhantomData,
        }
    }
}

impl<C, W: WatchSpec<C>> Update<C, W> {
    pub fn new() -> Self {
        Self::default()
    }

    #[doc(hidden)]
    pub fn __patch_mut(&mut self) -> &mut W::Patch {
        &mut self.patch
    }
}

/// Metadata implemented by generated watcher policies.
#[doc(hidden)]
pub trait WatchSpec<C> {
    type Patch: Default;
    fn is_empty(patch: &Self::Patch) -> bool;
    fn apply(patch: Self::Patch, state: &mut C);
}

/// Typed field metadata shared by `#[component]`/`#[store]` and `#[handlers]`.
#[doc(hidden)]
pub trait WatchField<C> {
    type Value;
    fn set(state: &mut C, value: Self::Value);
}

mod sealed {
    pub trait Sealed<C> {}
    impl<C> Sealed<C> for () {}
    impl<C, W: super::WatchSpec<C>> Sealed<C> for super::Update<C, W> {}
}

#[doc(hidden)]
pub trait WatchResult<C>: sealed::Sealed<C> {
    fn commit(self, handle: &Handle<C>);
}

impl<C: 'static> WatchResult<C> for () {
    fn commit(self, _: &Handle<C>) {}
}

impl<C: 'static, W: WatchSpec<C>> WatchResult<C> for Update<C, W> {
    fn commit(self, handle: &Handle<C>) {
        if !W::is_empty(&self.patch) {
            handle.update(|state| W::apply(self.patch, state));
        }
    }
}

thread_local! {
    static EVALUATING: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn assert_writes_allowed() {
    assert!(
        !EVALUATING.with(Cell::get),
        "#[watch] evaluation cannot mutate reactive state directly; return Update<Self> instead"
    );
}

fn evaluate<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            EVALUATING.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(EVALUATING.with(|v| v.replace(true)));
    f()
}

/// One coalesced, lifecycle-bound subscription. Read inputs and evaluate under
/// one shared borrow, then release it before committing the result. Generated
/// history contains only inputs declared as `Change<T>` (or all fields for a
/// bare observer). No retained history or result outlives the scope.
#[doc(hidden)]
pub fn install_snapshot_watch<C, S, R>(
    scope_id: ScopeId,
    fields: &'static [&'static str],
    label: &'static str,
    callback: impl Fn(&C, Option<&S>) -> (R, S) + 'static,
) -> EffectId
where
    C: 'static,
    S: 'static,
    R: WatchResult<C> + 'static,
{
    let previous = Rc::new(RefCell::new(None::<S>));
    let id = crate::watch::watch_snapshot_fields(scope_id, fields, label, move || {
        let Some(scope) = Scope::find(scope_id) else {
            return;
        };
        let Some(state) = scope.typed::<C>() else {
            return;
        };
        // Only the declared inputs subscribe. Incidental reads in author
        // code or model writeback must not extend the checked graph.
        crate::reactive::without_tracking(|| {
            let _frame = crate::ComponentCallbackFrame::for_scope(scope_id);
            crate::scope::with_current_scope_id(scope_id, || {
                let (result, next) = {
                    let inputs = state.borrow();
                    let prev = previous.borrow();
                    evaluate(|| callback(&inputs, prev.as_ref()))
                };
                *previous.borrow_mut() = Some(next);
                // Evaluation can invoke browser APIs that remove the owner.
                if Scope::find(scope_id).is_some() {
                    result.commit(&Handle::new(state, scope_id));
                }
            });
        });
    });
    crate::on_scope_unmount_for(scope_id, move || crate::reactive::release(id));
    id
}

/// Generated const graph nodes. Conditional compilation is applied to each
/// node by the macro, so only active handlers participate in cycle checks.
#[doc(hidden)]
pub struct WatchNode {
    pub reads: &'static [&'static str],
    pub writes: &'static [&'static str],
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[doc(hidden)]
pub const fn assert_watch_graph<const N: usize>(nodes: [WatchNode; N]) {
    let mut edges = [[false; N]; N];
    let mut i = 0;
    while i < N {
        let mut j = 0;
        while j < N {
            let mut w = 0;
            while w < nodes[i].writes.len() {
                let mut r = 0;
                while r < nodes[j].reads.len() {
                    if same(nodes[i].writes[w], nodes[j].reads[r]) {
                        edges[i][j] = true;
                    }
                    r += 1;
                }
                w += 1;
            }
            j += 1;
        }
        i += 1;
    }
    let mut k = 0;
    while k < N {
        let mut i = 0;
        while i < N {
            let mut j = 0;
            while j < N {
                edges[i][j] = edges[i][j] || (edges[i][k] && edges[k][j]);
                j += 1;
            }
            i += 1;
        }
        k += 1;
    }
    let mut i = 0;
    while i < N {
        assert!(!edges[i][i], "#[watch] dependency graph contains a cycle");
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluation_guard_blocks_writes_and_restores_after_unwind() {
        let result = std::panic::catch_unwind(|| evaluate(assert_writes_allowed));
        assert!(result.is_err());
        assert_writes_allowed();
    }

    #[test]
    fn direct_handle_and_signal_writes_are_rejected_before_mutation() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let cell = Rc::new(RefCell::new(1_u32));
        let handle = Handle::new(cell.clone(), ScopeId(999_999));
        for write in [
            Box::new(|| handle.update(|s| *s = 2)) as Box<dyn Fn()>,
            Box::new(|| *handle.borrow_mut() = 3),
            Box::new(|| *handle.try_borrow_mut().unwrap() = 4),
            Box::new(|| handle.defer_update(|s| *s = 5)),
        ] {
            assert!(catch_unwind(AssertUnwindSafe(|| evaluate(write))).is_err());
            assert_eq!(*cell.borrow(), 1);
        }
        let field = crate::handle::FieldHandle::<u32>::__new(ScopeId(999_999), "value");
        for write in [
            Box::new(|| field.set(2)) as Box<dyn Fn()>,
            Box::new(|| field.update(|v| *v = 3)),
        ] {
            assert!(catch_unwind(AssertUnwindSafe(|| evaluate(write))).is_err());
        }
        let signal = crate::rw_signal(10_u32);
        for write in [
            Box::new(|| signal.set(11)) as Box<dyn Fn()>,
            Box::new(|| signal.set_force(12)),
            Box::new(|| signal.update(|v| *v = 13)),
        ] {
            assert!(catch_unwind(AssertUnwindSafe(|| evaluate(write))).is_err());
            assert_eq!(signal.get(), 10);
        }
    }

    #[test]
    fn graph_rejects_indirect_cycles_but_accepts_fanout() {
        assert_watch_graph([
            WatchNode {
                reads: &["a"],
                writes: &["b", "c"],
            },
            WatchNode {
                reads: &["b", "c"],
                writes: &["d"],
            },
        ]);
        assert!(
            std::panic::catch_unwind(|| assert_watch_graph([
                WatchNode {
                    reads: &["a"],
                    writes: &["b"]
                },
                WatchNode {
                    reads: &["b"],
                    writes: &["c"]
                },
                WatchNode {
                    reads: &["c"],
                    writes: &["a"]
                },
            ]))
            .is_err()
        );
    }
}
