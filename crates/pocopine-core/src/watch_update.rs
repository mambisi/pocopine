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

/// A patch restricted to a tuple of field markers. In `#[watch]` signatures,
/// `Self::FieldName` resolves to an owner field marker; `updates(...)` supplies
/// the tuple for the `Update<Self>` shorthand. Only fields in that tuple can
/// be set; omitted fields remain unchanged.
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
            patch: W::empty(),
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

/// Storage and commit operations for a tuple of output field markers.
#[doc(hidden)]
pub trait WatchSpec<C> {
    type Patch;
    fn empty() -> Self::Patch;
    fn is_empty(patch: &Self::Patch) -> bool;
    fn apply(patch: Self::Patch, state: &mut C);
}

/// A generated, reusable descriptor for one component or store field.
/// `EditorField::Draft`, for example, implements `Field<Editor>` and retains
/// the field's value type and Rust name. `get` borrows the value without a
/// reactive handle or a mutation capability.
pub trait Field<C> {
    type Value;
    const NAME: &'static str;
    fn get(state: &C) -> &Self::Value;
    #[doc(hidden)]
    fn set(state: &mut C, value: Self::Value);
}

/// A statically checked slot in a field tuple. The generated fluent setter
/// infers INDEX from the selected field, so it cannot address another slot.
#[doc(hidden)]
pub trait UpdateField<C, F: Field<C>, const INDEX: usize>: WatchSpec<C> {
    fn set_field(patch: &mut Self::Patch, value: F::Value);
}

impl<C> WatchSpec<C> for () {
    type Patch = ();
    fn empty() {}
    fn is_empty(_: &()) -> bool {
        true
    }
    fn apply(_: (), _: &mut C) {}
}

macro_rules! tuple_slots {
    ([$($all:ident),+];) => {};
    ([$($all:ident),+]; $field:ident:$index:tt $(, $rest:ident:$rest_index:tt)*) => {
        impl<C, $($all: Field<C>),+> UpdateField<C, $field, $index> for ($($all,)+) {
            fn set_field(patch: &mut Self::Patch, value: $field::Value) {
                patch.$index = Some(value);
            }
        }
        tuple_slots!([$($all),+]; $($rest:$rest_index),*);
    };
}

macro_rules! field_tuples {
    () => {};
    ($field:ident:$index:tt $(, $rest:ident:$rest_index:tt)*) => {
        impl<C, $field: Field<C>, $($rest: Field<C>),*> WatchSpec<C> for ($field, $($rest,)*) {
            type Patch = (Option<$field::Value>, $(Option<$rest::Value>,)*);
            fn empty() -> Self::Patch {
                (None::<$field::Value>, $(None::<$rest::Value>,)*)
            }
            fn is_empty(patch: &Self::Patch) -> bool {
                patch.0.is_none() $(&& patch.$rest_index.is_none())*
            }
            fn apply(patch: Self::Patch, state: &mut C) {
                if let Some(value) = patch.0 { $field::set(state, value); }
                $(if let Some(value) = patch.$rest_index { $rest::set(state, value); })*
            }
        }
        tuple_slots!([$field $(, $rest)*]; $field:0 $(, $rest:$rest_index)*);
    };
}

field_tuples!(F0:0);
field_tuples!(F0:0, F1:1);
field_tuples!(F0:0, F1:1, F2:2);
field_tuples!(F0:0, F1:1, F2:2, F3:3);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26, F27:27);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26, F27:27, F28:28);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26, F27:27, F28:28, F29:29);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26, F27:27, F28:28, F29:29, F30:30);
field_tuples!(F0:0, F1:1, F2:2, F3:3, F4:4, F5:5, F6:6, F7:7, F8:8, F9:9, F10:10, F11:11, F12:12, F13:13, F14:14, F15:15, F16:16, F17:17, F18:18, F19:19, F20:20, F21:21, F22:22, F23:23, F24:24, F25:25, F26:26, F27:27, F28:28, F29:29, F30:30, F31:31);

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

pub(crate) fn is_evaluating() -> bool {
    EVALUATING.with(Cell::get)
}

pub(crate) fn assert_writes_allowed() {
    assert!(
        !is_evaluating(),
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

    #[derive(Default)]
    struct State {
        draft: String,
        issue: Option<String>,
    }
    enum Draft {}
    enum Issue {}
    impl Field<State> for Draft {
        type Value = String;
        const NAME: &'static str = "draft";
        fn get(state: &State) -> &String {
            &state.draft
        }
        fn set(state: &mut State, value: String) {
            state.draft = value;
        }
    }
    impl Field<State> for Issue {
        type Value = Option<String>;
        const NAME: &'static str = "issue";
        fn get(state: &State) -> &Option<String> {
            &state.issue
        }
        fn set(state: &mut State, value: Option<String>) {
            state.issue = value;
        }
    }

    #[test]
    fn tuple_patch_preserves_omitted_fields_and_clears_optional_values() {
        type Outputs = (Issue, Draft);
        let mut patch = Update::<State, Outputs>::new();
        assert!(Outputs::is_empty(&patch.patch));
        <Outputs as UpdateField<State, Issue, 0>>::set_field(&mut patch.patch, None);
        assert!(!Outputs::is_empty(&patch.patch));
        let mut state = State {
            draft: "keep".into(),
            issue: Some("clear".into()),
        };
        Outputs::apply(patch.patch, &mut state);
        assert_eq!(Draft::get(&state), "keep");
        assert_eq!(Issue::get(&state), &None);

        let mut patch = Update::<State, Outputs>::new();
        <Outputs as UpdateField<State, Draft, 1>>::set_field(&mut patch.patch, "first".into());
        <Outputs as UpdateField<State, Draft, 1>>::set_field(&mut patch.patch, "last".into());
        Outputs::apply(patch.patch, &mut state);
        assert_eq!(state.draft, "last");
    }

    #[test]
    fn empty_tuple_patch_has_no_storage_or_mutation() {
        let patch = Update::<State, ()>::new();
        assert_eq!(std::mem::size_of_val(&patch), 0);
        assert!(<() as WatchSpec<State>>::is_empty(&patch.patch));
        let mut state = State {
            draft: "keep".into(),
            issue: Some("keep".into()),
        };
        <() as WatchSpec<State>>::apply(patch.patch, &mut state);
        assert_eq!(state.draft, "keep");
        assert_eq!(state.issue.as_deref(), Some("keep"));
    }

    #[test]
    fn evaluation_guard_blocks_writes_and_restores_after_unwind() {
        let result = std::panic::catch_unwind(|| {
            evaluate(|| {
                assert!(is_evaluating());
                let nested = std::panic::catch_unwind(|| evaluate(assert_writes_allowed));
                assert!(nested.is_err());
                assert!(
                    is_evaluating(),
                    "inner unwind must preserve the outer guard"
                );
                assert_writes_allowed();
            });
        });
        assert!(result.is_err());
        assert!(!is_evaluating());
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
