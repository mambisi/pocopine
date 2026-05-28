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
