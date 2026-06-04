//! Integration tests for the `#[query]` selector runtime.
//!
//! Exercises the four moving parts from
//! `docs/internal/sync-query-selector-mechanism.md`:
//!
//! * Thread-local read tracking — `QueryView::rows()` inside a
//!   selector's compute registers the subscription as a dep.
//! * `(SelectorId, args_hash)` caching — repeat `observe_selector`
//!   calls return the same entry.
//! * Invalidation via the existing `notify_listeners` plumbing —
//!   mutating through `QueryClient::mutate` reruns the selector.
//! * `PartialEq` diff suppression — selectors whose output didn't
//!   change do NOT fire their own listeners.
//!
//! Tests drive a no-network `EchoMutator` whose `apply_remote`
//! returns the payload synchronously, so the routing engine's
//! optimistic + canonical paths both fire without a real fetch.

#![cfg(not(target_arch = "wasm32"))]

use std::cell::Cell;
use std::rc::Rc;

use pocopine_sync::{MutationId, SyncResult};
use pocopine_sync_query::{
    selector::SelectorId, Mutator, MutatorRemoteContext, MutatorRemoteFuture, QueryClient,
    RowChange,
};
use pocopine_sync_query_macros::query_resource;
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

/// No-network mutator. `apply_local` and `apply_remote` both echo the
/// payload as a single Upsert so the routing engine fires both
/// optimistic and canonical paths.
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

/// Helper: build a workspace-scoped issues query.
fn workspace_query(ws: &str) -> pocopine_sync_query::Query<Issue> {
    Issue::query()
        .eq(issues::field::workspace_id, ws.to_string())
        .build()
}

fn issue(id: &str, ws: &str, title: &str) -> Issue {
    Issue {
        id: id.into(),
        workspace_id: ws.into(),
        title: title.into(),
    }
}

// ---- Regression: compute() panic doesn't poison the selector -----
//
// If a selector's compute body panics, the RAII RerunGuard inside
// rerun() must still: (1) pop the tracking frame so subsequent
// reads on this thread don't accumulate in a stale frame, and (2)
// reset `running` so future reruns aren't permanently no-op'd at
// the running-guard check. Catch the panic via
// std::panic::catch_unwind and verify a fresh selector on the same
// thread observes normal mutation semantics.

#[tokio::test]
async fn compute_panic_does_not_poison_thread() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();

            // Selector A: compute panics on first run.
            let panic_id = SelectorId::new(0xBADC0FFEE);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = client.observe_selector::<u32, _>(panic_id, 0, Box::new(()), || {
                    panic!("intentional panic for the test")
                });
            }));
            assert!(result.is_err(), "compute panic should propagate");

            // Selector B: a normal selector on the same thread must
            // work — if the guard didn't clean up, this would either
            // panic from a poisoned frame or capture the wrong deps.
            let normal_id = SelectorId::new(0xC0FFEE);
            let client_for = client.clone();
            let view = client.observe_selector(normal_id, 0, Box::new(()), move || {
                let qv = client_for.observe(workspace_query("W1"));
                qv.rows().len() as u32
            });
            assert_eq!(view.value(), 0);

            // Mutate and verify the rerun fires — proves `running`
            // was reset (otherwise B's listener-triggered rerun
            // would no-op at the guard).
            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "after panic"), &ctx)
                .await
                .expect("mutate ok");
            assert_eq!(view.value(), 1, "selector reran after the unrelated panic");
        })
        .await;
}

// ---- Regression: selector tracks via QueryView::version() --------
//
// A selector body that reads `view.version()` (the documented
// reactive integer-changes signal) must register the underlying
// subscription as a dep. Same for `state()` / `shared_state()`.
// Pre-fix, these methods bypassed track_for_selector, leaving the
// selector with zero deps and never rerunning.

#[tokio::test]
async fn selector_tracks_through_version_call() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0xCEED);

            let client_for = client.clone();
            let view = client.observe_selector(id, 0, Box::new(()), move || {
                let qv = client_for.observe(workspace_query("W1"));
                qv.version() // version() must trigger track_for_selector
            });

            let initial = view.value();

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "x"), &ctx)
                .await
                .expect("mutate ok");

            assert_ne!(
                view.value(),
                initial,
                "selector picked up the version bump via version()"
            );
        })
        .await;
}

// ---- 0. Hash-collision disambiguation -----------------------------
//
// Regression for the codex finding that `Hash` is not collision-free.
// Two `observe_selector` calls with the SAME `(id, args_hash)` but
// distinct `AnyArgs` values must land in distinct entries — the
// second call must NOT receive the first's cached value.

#[tokio::test]
async fn hash_collision_does_not_alias_distinct_args() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0xC0);

            // Same args_hash, different boxed args. (In practice the
            // macro hashes args, but a colliding hash is rare for
            // String — we force it here to exercise the bucket.)
            let v_a =
                client.observe_selector::<u32, _>(id, 0xCAFE, Box::new("A".to_string()), || 1u32);
            let v_b =
                client.observe_selector::<u32, _>(id, 0xCAFE, Box::new("B".to_string()), || 2u32);

            assert_eq!(v_a.value(), 1, "first entry preserved");
            assert_eq!(v_b.value(), 2, "second entry NOT aliased to first");
        })
        .await;
}

// ---- 1. Caching by (id, args_hash) --------------------------------

#[tokio::test]
async fn observe_selector_caches_by_id_and_args_hash() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0x01);

            let call_count = Rc::new(Cell::new(0u32));
            let client_for_compute = client.clone();
            let counter = call_count.clone();
            let compute = move || {
                counter.set(counter.get() + 1);
                let view = client_for_compute.observe(workspace_query("W1"));
                view.len() as u32
            };

            // First observe — compute fires once, caches.
            let v1 = client.observe_selector(id, 0xAA, Box::new(0xAAu64), compute.clone());
            assert_eq!(v1.value(), 0);
            assert_eq!(call_count.get(), 1);

            // Second observe with SAME (id, args_hash) — cache hit,
            // compute does NOT fire.
            let v2 = client.observe_selector(id, 0xAA, Box::new(0xAAu64), compute.clone());
            assert_eq!(v2.value(), 0);
            assert_eq!(call_count.get(), 1, "compute reused cached entry");

            // Same id, different args_hash — distinct entry, compute fires.
            let v3 = client.observe_selector(id, 0xBB, Box::new(0xBBu64), compute.clone());
            assert_eq!(v3.value(), 0);
            assert_eq!(call_count.get(), 2);
        })
        .await;
}

// ---- 2. Invalidation on subscription change -----------------------

#[tokio::test]
async fn selector_reruns_when_tracked_subscription_changes() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0x02);

            let runs = Rc::new(Cell::new(0u32));
            let client_for_compute = client.clone();
            let runs_for_compute = runs.clone();
            let view = client.observe_selector(id, 0, Box::new(()), move || {
                runs_for_compute.set(runs_for_compute.get() + 1);
                let qv = client_for_compute.observe(workspace_query("W1"));
                qv.rows().len() as u32
            });

            assert_eq!(view.value(), 0);
            assert_eq!(runs.get(), 1);

            // Wire a listener so we can see rerun-with-diff fire.
            let fires = Rc::new(Cell::new(0u32));
            let fires_for_cb = fires.clone();
            let _tok = view.on_update(move || {
                fires_for_cb.set(fires_for_cb.get() + 1);
            });

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "first"), &ctx)
                .await
                .expect("mutate ok");

            assert_eq!(view.value(), 1, "selector picked up the new row");
            assert!(runs.get() >= 2, "selector reran at least once");
            assert!(fires.get() >= 1, "diff-fire fired at least once");
        })
        .await;
}

// ---- 3. Diff suppression ------------------------------------------

#[tokio::test]
async fn diff_suppression_blocks_listener_when_output_unchanged() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0x03);

            // Selector returns a CONSTANT — output never changes
            // even though the tracked subscription does.
            let runs = Rc::new(Cell::new(0u32));
            let client_for_compute = client.clone();
            let runs_for_compute = runs.clone();
            let view = client.observe_selector(id, 0, Box::new(()), move || {
                runs_for_compute.set(runs_for_compute.get() + 1);
                let qv = client_for_compute.observe(workspace_query("W1"));
                let _force_read = qv.rows();
                42u32 // constant
            });

            assert_eq!(view.value(), 42);
            assert_eq!(runs.get(), 1);

            let fires = Rc::new(Cell::new(0u32));
            let fires_for_cb = fires.clone();
            let _tok = view.on_update(move || {
                fires_for_cb.set(fires_for_cb.get() + 1);
            });

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "first"), &ctx)
                .await
                .expect("mutate ok");

            assert!(runs.get() >= 2, "selector reran (was woken)");
            assert_eq!(
                fires.get(),
                0,
                "diff suppression prevented the listener from firing"
            );
            assert_eq!(view.value(), 42);
        })
        .await;
}

// ---- 4. Nested selector composition -------------------------------

#[tokio::test]
async fn nested_selector_propagates_through_outer() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let inner_id = SelectorId::new(0x10);
            let outer_id = SelectorId::new(0x11);

            // Inner: count of rows in W1.
            let client_for_inner = client.clone();
            let inner_runs = Rc::new(Cell::new(0u32));
            let inner_runs_c = inner_runs.clone();
            let inner_compute = move || {
                inner_runs_c.set(inner_runs_c.get() + 1);
                let qv = client_for_inner.observe(workspace_query("W1"));
                qv.rows().len() as u32
            };
            let _inner = client.observe_selector(inner_id, 0, Box::new(()), inner_compute.clone());

            // Outer: wraps inner, multiplies by 2.
            let client_for_outer = client.clone();
            let outer_runs = Rc::new(Cell::new(0u32));
            let outer_runs_c = outer_runs.clone();
            let outer = client.observe_selector(outer_id, 0, Box::new(()), move || {
                outer_runs_c.set(outer_runs_c.get() + 1);
                let inner_view = client_for_outer.observe_selector(
                    inner_id,
                    0,
                    Box::new(()),
                    inner_compute.clone(),
                );
                inner_view.value() * 2
            });

            assert_eq!(outer.value(), 0);
            assert_eq!(inner_runs.get(), 1);
            assert_eq!(outer_runs.get(), 1);

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "x"), &ctx)
                .await
                .expect("mutate ok");

            // Inner reruns (output 0 → 1), outer reruns because its
            // tracked inner-selector entry's diff-fire propagated.
            assert_eq!(outer.value(), 2);
            assert!(inner_runs.get() >= 2);
            assert!(outer_runs.get() >= 2);
        })
        .await;
}

#[tokio::test]
async fn nested_diff_suppression_stops_cascade() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let inner_id = SelectorId::new(0x20);
            let outer_id = SelectorId::new(0x21);

            // Inner returns a constant — its output never changes.
            let client_for_inner = client.clone();
            let inner_compute = move || {
                let qv = client_for_inner.observe(workspace_query("W1"));
                let _r = qv.rows();
                7u32 // constant
            };

            let outer_runs = Rc::new(Cell::new(0u32));
            let outer_runs_c = outer_runs.clone();
            let client_for_outer = client.clone();
            let outer = client.observe_selector(outer_id, 0, Box::new(()), move || {
                outer_runs_c.set(outer_runs_c.get() + 1);
                let iv = client_for_outer.observe_selector(
                    inner_id,
                    0,
                    Box::new(()),
                    inner_compute.clone(),
                );
                iv.value()
            });

            assert_eq!(outer.value(), 7);
            let initial_outer_runs = outer_runs.get();

            let ctx = StubContext::new();
            client
                .mutate::<EchoMutator>(issue("i1", "W1", "x"), &ctx)
                .await
                .expect("mutate ok");

            // Outer must NOT have reran: inner's diff-suppression
            // prevented it from notifying outer.
            assert_eq!(
                outer_runs.get(),
                initial_outer_runs,
                "inner's diff suppression must stop the cascade"
            );
        })
        .await;
}

// ---- 5. Lifecycle / GC --------------------------------------------

#[tokio::test]
async fn drop_view_removes_entry_from_registry() {
    LocalSet::new()
        .run_until(async {
            let client = QueryClient::without_driver();
            let id = SelectorId::new(0x30);

            let client_for_compute = client.clone();
            let compute = move || {
                let qv = client_for_compute.observe(workspace_query("W1"));
                qv.rows().len() as u32
            };

            {
                let _view = client.observe_selector(id, 0, Box::new(()), compute);
                // Underlying subscription is alive while the view holds.
                assert!(client.refcount_of(&workspace_query("W1")).is_some());
            }
            // After view drops, the selector entry should be gone,
            // and so should the subscription it was holding.
            assert_eq!(
                client.refcount_of(&workspace_query("W1")),
                None,
                "subscription GC'd after selector dropped"
            );
        })
        .await;
}
