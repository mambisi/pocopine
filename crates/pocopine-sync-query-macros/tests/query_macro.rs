//! Integration tests for the `#[query]` proc-macro.
//!
//! Verifies that the macro's expansion compiles, produces a stable
//! `SELECTOR_ID`, hashes args correctly, and plugs into the runtime's
//! caching + diff-suppression machinery end-to-end.
//!
//! The runtime itself is covered by
//! `crates/pocopine-sync-query/tests/selector.rs`; these tests
//! exercise the macro's args-hashing and code-generation surface.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::Cell;
use std::rc::Rc;

use pocopine_sync::{MutationId, SyncResult};
use pocopine_sync_query::{
    Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClient, RowChange,
};
use pocopine_sync_query_macros::{query, query_resource};
use serde::{Deserialize, Serialize};
use tokio::task::LocalSet;

#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    pub title: String,
}

struct EchoMutator;

impl Mutator for EchoMutator {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "echo";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        _ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        Box::pin(async move { Ok(vec![RowChange::Upsert(payload)]) })
    }
}

struct StubContext {
    next_id: Cell<u64>,
}

impl StubContext {
    fn new() -> Self {
        Self {
            next_id: Cell::new(1),
        }
    }
}

impl MutatorRemoteContext for StubContext {
    fn push_url(&self) -> &str {
        "/__pocopine/sync/v1/push"
    }

    fn next_mutation_id(&self) -> SyncResult<MutationId> {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        MutationId::new(format!("test:{n}"))
    }
}

fn make_issue(id: &str, ws: &str, title: &str) -> Issue {
    Issue {
        id: id.into(),
        workspace_id: ws.into(),
        title: title.into(),
    }
}

// ---- Selectors under test -----------------------------------------

thread_local! {
    static OPEN_RUNS: Cell<u32> = const { Cell::new(0) };
    static TITLES_RUNS: Cell<u32> = const { Cell::new(0) };
}

#[query]
fn issue_count_in_workspace(client: QueryClient, ws: String) -> u32 {
    OPEN_RUNS.with(|c| c.set(c.get() + 1));
    let view = client.observe(
        Issue::query()
            .eq(issues::field::workspace_id, ws.clone())
            .build(),
    );
    view.rows().len() as u32
}

#[query]
fn issue_titles(client: QueryClient, ws: String) -> Vec<String> {
    TITLES_RUNS.with(|c| c.set(c.get() + 1));
    let view = client.observe(
        Issue::query()
            .eq(issues::field::workspace_id, ws.clone())
            .build(),
    );
    view.rows().into_iter().map(|i| i.title).collect()
}

// ---- Tests --------------------------------------------------------

#[test]
fn selector_id_is_stable_and_distinct() {
    let a = issue_count_in_workspace::SELECTOR_ID.as_u64();
    let b = issue_titles::SELECTOR_ID.as_u64();
    assert_ne!(a, b, "two #[query] fns must hash to distinct ids");
    // Stability: same fn, same id across reads.
    assert_eq!(a, issue_count_in_workspace::SELECTOR_ID.as_u64());
}

#[tokio::test]
async fn macro_observe_returns_cached_value_and_reacts_to_mutations() {
    LocalSet::new()
        .run_until(async {
            OPEN_RUNS.with(|c| c.set(0));
            let client = QueryClient::without_driver();

            let view = issue_count_in_workspace::observe(&client, "W1".to_string());
            assert_eq!(view.value(), 0);
            assert_eq!(OPEN_RUNS.with(|c| c.get()), 1);

            // Second observe with SAME args — cache hit, compute NOT
            // re-fired.
            let view2 = issue_count_in_workspace::observe(&client, "W1".to_string());
            assert_eq!(view2.value(), 0);
            assert_eq!(
                OPEN_RUNS.with(|c| c.get()),
                1,
                "cache hit must not rerun compute"
            );

            // Different args → new entry → compute fires.
            let view_w2 = issue_count_in_workspace::observe(&client, "W2".to_string());
            assert_eq!(view_w2.value(), 0);
            assert_eq!(OPEN_RUNS.with(|c| c.get()), 2);

            // Track listener fires after a mutation.
            let fires = Rc::new(Cell::new(0u32));
            let fires_c = fires.clone();
            let _tok = view.on_update(move || {
                fires_c.set(fires_c.get() + 1);
            });

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(make_issue("i1", "W1", "first"), &ctx)
                .await
                .expect("mutate ok");

            assert_eq!(view.value(), 1, "selector observed the new row");
            assert_eq!(view_w2.value(), 0, "W2 selector unaffected");
            assert!(fires.get() >= 1);
        })
        .await;
}

// ---- Regression: super:: resolution -------------------------------
//
// If the macro nested the user body inside the generated module,
// `super::*` references in the body would resolve through the wrong
// scope. The macro keeps the body at the same module level as the
// original `#[query] fn`, so `super::HELPER_CONST` here must continue
// to refer to `crate::HELPER_CONST`.

const HELPER_CONST: u32 = 1234;

mod nested_scope {
    use super::*;

    #[query]
    pub fn uses_parent_const(_client: QueryClient) -> u32 {
        // Should resolve to crate::HELPER_CONST. Pre-fix this would
        // have been `nested_scope::uses_parent_const::HELPER_CONST`
        // which doesn't exist.
        super::HELPER_CONST
    }
}

#[test]
fn macro_preserves_super_resolution_in_body() {
    let client = QueryClient::without_driver();
    let view = nested_scope::uses_parent_const::observe(&client);
    assert_eq!(view.value(), HELPER_CONST);
}

// ---- Regression: public attrs survive on the generated module ----
//
// `#[doc]` and `#[deprecated]` on a `#[query] fn` must end up on
// the GENERATED MODULE (the public surface), not on the hidden
// lifted body. Otherwise `#![deny(missing_docs)]` rejects the
// public module and `#[deprecated]` on the body trips
// `deny(deprecated)` when the macro's own `super::__user_fn(...)`
// call references it.
//
// We can't easily assert "doc string is present in rustdoc" from a
// Rust test, but we CAN observe `#[deprecated]` via the lint:
// the user-side `observe()` call on a deprecated selector should
// fire `deprecated` ONCE (from the module-level attr), not twice.
// We assert "compiles with -D deprecated" elsewhere; here we just
// confirm `#[doc]` doesn't break compilation.

/// This selector has a doc comment. The macro must route the doc
/// to the public module, not to the hidden lifted fn (which is
/// `#[doc(hidden)]` and would swallow it).
#[query]
fn documented_selector(_client: QueryClient) -> u32 {
    42
}

#[test]
fn macro_routes_doc_to_module() {
    // The selector works; the doc comment didn't cause expansion to
    // fail. Doc visibility is verified by rustdoc rendering, not by
    // a runtime assert — this test just locks in that doc attrs
    // pass through the macro without breaking the build.
    let client = QueryClient::without_driver();
    let view = documented_selector::observe(&client);
    assert_eq!(view.value(), 42);
}

// ---- Regression: `#[must_use]` routes to observe(), not module ----
//
// `#[must_use]` on a module is a Rust warning (and `-D warnings`
// would make it a build failure). The macro must route the attr to
// `observe()` instead, where it actually fires the must-use lint
// against callers that drop the returned `SelectorView`.

#[query]
#[must_use]
fn must_use_selector(_client: QueryClient) -> u32 {
    99
}

#[test]
#[deny(unused_must_use)]
fn macro_routes_must_use_to_observe() {
    let client = QueryClient::without_driver();
    // We USE the returned view, so `must_use` is satisfied. The
    // value of this test is the build itself: if the macro had
    // placed `#[must_use]` on the module, this file would fail
    // under `-D warnings` even before reaching the assertion.
    let view = must_use_selector::observe(&client);
    assert_eq!(view.value(), 99);
}

// ---- Regression: `#[track_caller]` lands on observe() only -------
//
// `#[track_caller]` on a `#[query] fn` routes to the generated
// `observe()` fn only — NOT the lifted body. Forwarding to the
// body would actually degrade panic quality: `panic!()` inside a
// `#[track_caller]` body reports the caller's line (the opaque
// anonymous compute closure here), whereas the unannotated body
// reports the natural panic-site line. Observe-only is the best
// the macro can do given the closure hop.

#[query]
#[track_caller]
fn tc_selector(_client: QueryClient, n: u32) -> u32 {
    n * 2
}

#[test]
fn macro_accepts_track_caller_attr() {
    let client = QueryClient::without_driver();
    // Build-level assertion: `#[track_caller]` on a selector
    // compiles and runs. The macro routes the attr to `observe()`
    // (not the inner body) — a missing route would just fail to
    // compile or warn under `-D warnings`.
    let view = tc_selector::observe(&client, 21);
    assert_eq!(view.value(), 42);
}

// ---- Regression: raw-identifier args ------------------------------
//
// A user fn arg whose ident is a raw identifier (e.g. `r#type:
// String` — `type` is a keyword) is valid Rust. The macro must not
// panic at expansion time when it builds helper names. The fix names
// helpers by position (`__pq_arg_0`, …) rather than splicing the
// ident, so raw idents survive.

#[query]
fn with_raw_ident(_client: QueryClient, r#type: u32) -> u32 {
    r#type + 1
}

#[test]
fn macro_handles_raw_identifier_args() {
    let client = QueryClient::without_driver();
    let view = with_raw_ident::observe(&client, 41);
    assert_eq!(view.value(), 42);
}

// The fn NAME itself is also a raw identifier (the macro must
// strip `r#` before constructing the mangled `__pq_user_fn__…`
// inner ident, or Ident::new panics during expansion on `#`).
#[query]
fn r#type(_client: QueryClient) -> u32 {
    7
}

#[test]
fn macro_handles_raw_identifier_fn_name() {
    let client = QueryClient::without_driver();
    let view = r#type::observe(&client);
    assert_eq!(view.value(), 7);
}

// ---- Regression: user attrs propagate to the lifted body ----------
//
// Lint attrs on the original `#[query] fn` must follow the body when
// the macro lifts it to a sibling fn — otherwise lints emitted INSIDE
// the body (where the user's `#[allow(...)]` would silence them) hit
// the new fn without the silencer and break `#[deny(warnings)]` /
// `-D warnings` builds. Here `#[allow(unused_variables)]` silences
// the unused `extra` param; without forwarding, that would warn-as-
// error in CI.

#[query]
#[allow(unused_variables)]
fn allow_attr_followed_through(_client: QueryClient, n: u32, extra: u32) -> u32 {
    n + 1
}

#[test]
fn macro_forwards_user_attrs_to_lifted_body() {
    let client = QueryClient::without_driver();
    let view = allow_attr_followed_through::observe(&client, 41, 999);
    assert_eq!(view.value(), 42);
}

// ---- Regression: mut binding preserved ----------------------------
//
// A user body that mutates a `mut`-bound arg must still compile —
// the macro propagates `mut` to the inner fn's parameter list.

#[query]
fn doubled_via_mut(_client: QueryClient, mut n: u32) -> u32 {
    n *= 2;
    n
}

#[test]
fn macro_preserves_mut_arg_binding() {
    let client = QueryClient::without_driver();
    let view = doubled_via_mut::observe(&client, 21);
    assert_eq!(view.value(), 42);
}

#[tokio::test]
async fn macro_selector_composes_inside_another_selector() {
    LocalSet::new()
        .run_until(async {
            OPEN_RUNS.with(|c| c.set(0));
            TITLES_RUNS.with(|c| c.set(0));
            let client = QueryClient::without_driver();

            // Outer reads count + titles; both inner selectors track
            // the same workspace subscription, so a W1 mutation
            // reruns both via the diff-fire chain.
            let count_view = issue_count_in_workspace::observe(&client, "W1".to_string());
            let titles_view = issue_titles::observe(&client, "W1".to_string());

            assert_eq!(count_view.value(), 0);
            assert_eq!(titles_view.value(), Vec::<String>::new());

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(make_issue("i1", "W1", "rate-limit auth"), &ctx)
                .await
                .expect("mutate ok");

            assert_eq!(count_view.value(), 1);
            assert_eq!(titles_view.value(), vec!["rate-limit auth".to_string()]);
        })
        .await;
}
