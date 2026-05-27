//! End-to-end routing tests using the public API. A mutator's
//! row changes flow through `QueryClient::mutate(...)`, which
//! routes via predicate evaluation into every observing view's
//! state. Demonstrates the design's "no active subscription"
//! property — a W1 mutation appears in the W1 view and not the
//! W2 view, with no manual routing in user code.

#![cfg(not(target_arch = "wasm32"))]

#[allow(unused_imports)] // referenced by the macro emission
use pocopine_sync_query::params;
use pocopine_sync_query::{
    MutationOutcome, Mutator, MutatorRemoteContext, MutatorRemoteFuture, Order, QueryClient,
    RowChange,
};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
enum Status {
    Open,
    InProgress,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Issue {
    id: String,
    workspace_id: String,
    title: String,
    status: Status,
}

#[query_resource(
    name = "issues",
    row = Issue,
    schema_version = 1,
    params(
        workspace_id: String,
        status: params::InSet<Status>,
    ),
)]
pub struct Issues;

/// Test mutator: takes a full `Issue` payload and surfaces it as both
/// the optimistic and the canonical row change. A real mutator would
/// generate an id, talk to a server, etc — this is a self-contained
/// loopback so we can exercise the routing engine.
struct CreateIssue;

impl Mutator for CreateIssue {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "create_issue";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        _ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        // Loopback: server "confirms" the same row.
        Box::pin(async move { Ok(vec![RowChange::Upsert(payload)]) })
    }
}

struct StubContext {
    next_id: std::cell::Cell<u64>,
}

impl StubContext {
    fn new() -> Self {
        Self {
            next_id: std::cell::Cell::new(1),
        }
    }
}

impl MutatorRemoteContext for StubContext {
    fn push_url(&self) -> &str {
        "/__pocopine/sync/v1/push"
    }

    fn next_mutation_id(&self) -> pocopine_sync::SyncResult<pocopine_sync::MutationId> {
        let n = self.next_id.get();
        self.next_id.set(n + 1);
        pocopine_sync::MutationId::new(format!("test:{n}"))
    }
}

#[tokio::test]
async fn matching_view_receives_mutation_canonically() {
    let client = QueryClient::new();
    let ctx = StubContext::new();
    let q = Issues::query()
        .workspace_id("W1".to_string())
        .status_in([Status::Open])
        .unwrap()
        .build();
    let view = client.observe::<Issue>(q);

    assert_eq!(view.len(), 0);
    let version_before = view.version();

    let outcome = client
        .mutate::<CreateIssue>(
            Issue {
                id: "issue_1".to_string(),
                workspace_id: "W1".to_string(),
                title: "Auth bug".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    assert!(matches!(outcome, MutationOutcome::Accepted(_)));

    // Canonical row landed; optimistic was dequeued.
    assert_eq!(view.len(), 1);
    assert_eq!(view.state().pending().len(), 0);
    assert!(view.version() > version_before);

    let rows = view.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "issue_1");
}

#[tokio::test]
async fn non_matching_view_is_untouched() {
    let client = QueryClient::new();
    let ctx = StubContext::new();

    let q_w1 = Issues::query().workspace_id("W1".to_string()).build();
    let q_w2 = Issues::query().workspace_id("W2".to_string()).build();
    let v_w1 = client.observe::<Issue>(q_w1);
    let v_w2 = client.observe::<Issue>(q_w2);

    // A W1 mutation: only W1's view should change.
    client
        .mutate::<CreateIssue>(
            Issue {
                id: "issue_1".to_string(),
                workspace_id: "W1".to_string(),
                title: "test".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();

    assert_eq!(v_w1.len(), 1);
    assert_eq!(v_w2.len(), 0);
}

#[tokio::test]
async fn version_counter_bumps_on_state_changes() {
    let client = QueryClient::new();
    let ctx = StubContext::new();
    let view = client.observe::<Issue>(Issues::query().workspace_id("W1".to_string()).build());

    let v0 = view.version();
    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i1".to_string(),
                workspace_id: "W1".to_string(),
                title: "a".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    let v1 = view.version();
    assert!(v1 > v0, "version should bump on canonical apply");

    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i2".to_string(),
                workspace_id: "W1".to_string(),
                title: "b".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    let v2 = view.version();
    assert!(v2 > v1, "version should bump again");
}

#[tokio::test]
async fn observe_dedupes_via_refcount() {
    let client = QueryClient::new();
    let q1 = Issues::query().workspace_id("W1".to_string()).build();
    let q2 = Issues::query().workspace_id("W1".to_string()).build();

    let v1 = client.observe::<Issue>(q1.clone());
    assert_eq!(client.active_subscription_count(), 1);
    let v2 = client.observe::<Issue>(q2);
    assert_eq!(client.active_subscription_count(), 1);
    assert_eq!(client.refcount_of(&q1), Some(2));

    drop(v1);
    assert_eq!(client.refcount_of(&q1), Some(1));
    drop(v2);
    assert_eq!(client.refcount_of(&q1), None);
}

#[tokio::test]
async fn on_update_listener_fires_after_state_changes() {
    use std::cell::Cell;
    use std::rc::Rc;

    let client = QueryClient::new();
    let ctx = StubContext::new();
    let view = client.observe::<Issue>(Issues::query().workspace_id("W1".to_string()).build());

    let fired = Rc::new(Cell::new(0u32));
    let fired_clone = fired.clone();
    let _token = view.on_update(move || {
        fired_clone.set(fired_clone.get() + 1);
    });

    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i1".to_string(),
                workspace_id: "W1".to_string(),
                title: "test".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();

    // Optimistic apply + canonical reconcile both bumped the
    // subscription's listener list, so the callback ran at least
    // twice. The exact count is an implementation detail — we just
    // require monotonic non-zero firing.
    assert!(fired.get() >= 1, "listener should fire on state changes");
}

#[tokio::test]
async fn on_update_listener_unregisters_on_token_drop() {
    use std::cell::Cell;
    use std::rc::Rc;

    let client = QueryClient::new();
    let ctx = StubContext::new();
    let view = client.observe::<Issue>(Issues::query().workspace_id("W1".to_string()).build());

    let fired = Rc::new(Cell::new(0u32));
    let fired_clone = fired.clone();
    let token = view.on_update(move || {
        fired_clone.set(fired_clone.get() + 1);
    });

    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i1".to_string(),
                workspace_id: "W1".to_string(),
                title: "a".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    let first = fired.get();
    assert!(first >= 1);

    drop(token);

    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i2".to_string(),
                workspace_id: "W1".to_string(),
                title: "b".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    // No additional fires after token drop.
    assert_eq!(fired.get(), first);
}

// Sanity: order_by + limit still build correctly through the macro.
#[test]
fn builder_supports_order_and_limit() {
    let q = Issues::query()
        .workspace_id("W1".to_string())
        .order_by("status", Order::Asc)
        .limit(10)
        .build();
    assert!(q.order_by().is_some());
    assert_eq!(q.limit(), Some(10));
}
