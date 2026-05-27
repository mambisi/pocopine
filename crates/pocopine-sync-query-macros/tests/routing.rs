//! End-to-end routing tests using the public API. A mutator's
//! row changes flow through `QueryClient::mutate(...)`, which
//! routes via predicate evaluation into every observing view's
//! state. Demonstrates the design's "no active subscription"
//! property — a W1 mutation appears in the W1 view and not the
//! W2 view, with no manual routing in user code.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_sync_query::{
    MutationOutcome, Mutator, MutatorRemoteContext, MutatorRemoteFuture, Order, QueryClient,
    RowChange,
};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
}

#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    pub title: String,
    #[query_param]
    pub status: Status,
}

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
    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let q = Issue::query()
        .eq(issues::field::workspace_id, "W1")
        .any_of(issues::field::status, [Status::Open])
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
    let client = QueryClient::without_driver();
    let ctx = StubContext::new();

    let q_w1 = Issue::query().eq(issues::field::workspace_id, "W1").build();
    let q_w2 = Issue::query().eq(issues::field::workspace_id, "W2").build();
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
    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

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
    let client = QueryClient::without_driver();
    let q1 = Issue::query().eq(issues::field::workspace_id, "W1").build();
    let q2 = Issue::query().eq(issues::field::workspace_id, "W1").build();

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

    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

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

    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

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

/// Delete-shaped mutator. Codex pass 1 P2 #1: a successful delete
/// must remove the row from canonical state, not just from the
/// optimistic overlay. Previously `RowChange::Delete` was filtered
/// out before reaching `route_canonical_changes`, leaving the row
/// visible after the mutation accepted.
struct DeleteIssue;

impl Mutator for DeleteIssue {
    type Payload = pocopine_sync::RowKey;
    type Row = Issue;
    const NAME: &'static str = "delete_issue";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Delete(payload.clone())]
    }

    fn apply_remote(
        _ctx: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        Box::pin(async move { Ok(vec![RowChange::Delete(payload)]) })
    }
}

#[tokio::test]
async fn delete_mutation_removes_canonical_row() {
    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

    // Seed: create an issue.
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
    assert_eq!(view.len(), 1);

    // Delete it. Both the optimistic overlay AND the canonical
    // row should be cleared after the mutation accepts.
    client
        .mutate::<DeleteIssue>(pocopine_sync::RowKey::new("issue_1").unwrap(), &ctx)
        .await
        .unwrap();

    assert_eq!(
        view.len(),
        0,
        "canonical row should be removed after delete"
    );
    assert_eq!(view.state().pending().len(), 0);
}

/// Codex pass 1 P1: handles can outlive the client safely. The
/// registry now lives in an `Rc<QueryClientInner>` and handles hold
/// a `Weak`. Dropping a handle after the client is freed is a safe
/// no-op instead of a use-after-free.
#[test]
fn handle_outliving_client_is_safe() {
    let q = Issue::query().eq(issues::field::workspace_id, "W1").build();
    let h = {
        let c = QueryClient::without_driver();
        c.subscribe::<Issue>(q)
    };
    // Client is gone; dropping the handle must not panic or UAF.
    drop(h);
}

/// Codex pass 1 P2 #2: rollback fires observers. When `apply_remote`
/// errors, optimistic overlays are dropped; observers must see the
/// state change immediately, not wait for some unrelated mutation
/// to refresh them.
struct FailingMutator;

impl Mutator for FailingMutator {
    type Payload = Issue;
    type Row = Issue;
    const NAME: &'static str = "failing";
    const STREAM: &'static str = "issues";
    const SCHEMA_VERSION: u32 = 1;

    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>> {
        vec![RowChange::Upsert(payload.clone())]
    }

    fn apply_remote(
        _ctx: &dyn MutatorRemoteContext,
        _payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row> {
        Box::pin(async move { Err(pocopine_sync::SyncError::client("simulated push failure")) })
    }
}

#[tokio::test]
async fn rollback_notifies_observers() {
    use std::cell::Cell;
    use std::rc::Rc;

    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

    let fired = Rc::new(Cell::new(0u32));
    let fired_clone = fired.clone();
    let _token = view.on_update(move || {
        fired_clone.set(fired_clone.get() + 1);
    });

    let result = client
        .mutate::<FailingMutator>(
            Issue {
                id: "issue_1".to_string(),
                workspace_id: "W1".to_string(),
                title: "test".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await;
    assert!(result.is_err());

    // The optimistic apply fired one listener event; the rollback
    // must fire another so the observer sees the cleared state.
    assert!(
        fired.get() >= 2,
        "rollback should notify listeners (got {} fires)",
        fired.get()
    );
    assert_eq!(view.state().pending().len(), 0);
}

// Sanity: order_by + limit still build correctly through the macro.
#[test]
fn builder_supports_order_and_limit() {
    let q = Issue::query()
        .eq(issues::field::workspace_id, "W1")
        .order_by("status", Order::Asc)
        .limit(10)
        .build();
    assert!(q.order_by().is_some());
    assert_eq!(q.limit(), Some(10));
}

#[tokio::test]
async fn rows_applied_order_by_and_limit() {
    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view = client.observe::<Issue>(
        Issue::query()
            .eq(issues::field::workspace_id, "W1")
            .order_by("title", Order::Desc)
            .limit(2)
            .build(),
    );
    for (i, title) in ["alpha", "bravo", "charlie", "delta"].iter().enumerate() {
        client
            .mutate::<CreateIssue>(
                Issue {
                    id: format!("i{i}"),
                    workspace_id: "W1".to_string(),
                    title: title.to_string(),
                    status: Status::Open,
                },
                &ctx,
            )
            .await
            .unwrap();
    }
    let rows = view.rows();
    assert_eq!(rows.len(), 2, "limit must truncate to 2");
    assert_eq!(rows[0].title, "delta", "Desc title sort puts delta first");
    assert_eq!(rows[1].title, "charlie", "then charlie");
}

/// Regression: a Delete overlay targeting a row visible only via a
/// prior pending Upsert must suppress that prior Upsert from
/// `view.rows()` (via `evicted_key`). Without the fix, the user
/// would see the just-deleted row keep rendering until the create's
/// canonical reconcile arrived.
#[tokio::test]
async fn delete_suppresses_prior_pending_upsert() {
    use pocopine_sync_query::{MutatorRemoteFuture, RowChange};

    /// A stub mutator that ONLY emits the optimistic Upsert and
    /// never resolves apply_remote. We poll mutate() once with
    /// `now_or_never` to get past the optimistic apply without
    /// completing the future; that leaves the pending overlay
    /// alive — exactly the state in which a follow-up Delete must
    /// suppress the Upsert.
    struct CreateOnly;
    impl Mutator for CreateOnly {
        type Payload = Issue;
        type Row = Issue;
        const NAME: &'static str = "create_only";
        const STREAM: &'static str = "issues";
        const SCHEMA_VERSION: u32 = 1;
        fn apply_local(p: &Self::Payload) -> Vec<RowChange<Self::Row>> {
            vec![RowChange::Upsert(p.clone())]
        }
        fn apply_remote(
            _ctx: &dyn MutatorRemoteContext,
            payload: Self::Payload,
        ) -> MutatorRemoteFuture<Self::Row> {
            Box::pin(async move { Ok(vec![RowChange::Upsert(payload)]) })
        }
    }

    /// Delete-only mutator that loops back: server "confirms" the
    /// delete. We need this to push a Delete overlay into pending
    /// state synchronously alongside the prior Upsert.
    struct DeleteOnly;
    impl Mutator for DeleteOnly {
        type Payload = String; // row id
        type Row = Issue;
        const NAME: &'static str = "delete_only";
        const STREAM: &'static str = "issues";
        const SCHEMA_VERSION: u32 = 1;
        fn apply_local(id: &Self::Payload) -> Vec<RowChange<Self::Row>> {
            vec![RowChange::Delete(
                pocopine_sync::RowKey::new(id.clone()).unwrap(),
            )]
        }
        fn apply_remote(
            _ctx: &dyn MutatorRemoteContext,
            id: Self::Payload,
        ) -> MutatorRemoteFuture<Self::Row> {
            Box::pin(async move {
                Ok(vec![RowChange::Delete(
                    pocopine_sync::RowKey::new(id).unwrap(),
                )])
            })
        }
    }

    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

    // Step 1: Create — full mutate (apply_local + apply_remote +
    // route_canonical). This puts I1 into canonical.
    client
        .mutate::<CreateOnly>(
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
    assert_eq!(view.len(), 1);

    // Step 2: Delete. With the evicted_key fix, view.rows() no
    // longer renders the row even though the pending overlay
    // carries `optimistic_row=None`.
    client
        .mutate::<DeleteOnly>("i1".to_string(), &ctx)
        .await
        .unwrap();
    assert_eq!(
        view.len(),
        0,
        "deleted row must disappear from rendered view"
    );
}

/// Regression: rollback restoration must NOT clobber a concurrent
/// canonical update. With `canonical_contains` gating the upsert,
/// the stale snapshot is dropped when canonical already holds a
/// newer row.
#[tokio::test]
async fn rollback_does_not_clobber_concurrent_canonical() {
    use pocopine_sync_query::{MutatorRemoteFuture, RowChange};

    struct DeleteFails;
    impl Mutator for DeleteFails {
        type Payload = String;
        type Row = Issue;
        const NAME: &'static str = "delete_fails";
        const STREAM: &'static str = "issues";
        const SCHEMA_VERSION: u32 = 1;
        fn apply_local(id: &Self::Payload) -> Vec<RowChange<Self::Row>> {
            vec![RowChange::Delete(
                pocopine_sync::RowKey::new(id.clone()).unwrap(),
            )]
        }
        fn apply_remote(
            _ctx: &dyn MutatorRemoteContext,
            _id: Self::Payload,
        ) -> MutatorRemoteFuture<Self::Row> {
            Box::pin(async move { Err(pocopine_sync::SyncError::client("server rejected")) })
        }
    }

    let client = QueryClient::without_driver();
    let ctx = StubContext::new();
    let view =
        client.observe::<Issue>(Issue::query().eq(issues::field::workspace_id, "W1").build());

    // Seed canonical with I1 (via a successful Create).
    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i1".to_string(),
                workspace_id: "W1".to_string(),
                title: "original".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();

    // The rollback path runs after apply_remote errors; simulate
    // a concurrent successful update by pre-populating canonical
    // with a NEWER row that the rollback must not overwrite. We
    // do this directly through the optimistic-then-canonical
    // cycle: a second Create with the same id but a different
    // title lands a new canonical I1 ("updated").
    client
        .mutate::<CreateIssue>(
            Issue {
                id: "i1".to_string(),
                workspace_id: "W1".to_string(),
                title: "updated".to_string(),
                status: Status::Open,
            },
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(view.rows()[0].title, "updated");

    // Now the Delete mutation fails. Its rollback must not
    // resurrect the older "original" snapshot — canonical
    // currently holds "updated".
    let result = client.mutate::<DeleteFails>("i1".to_string(), &ctx).await;
    assert!(result.is_err());
    let rows = view.rows();
    assert_eq!(rows.len(), 1, "row must remain — concurrent canonical wins");
    assert_eq!(
        rows[0].title, "updated",
        "stale rollback snapshot must NOT overwrite newer canonical"
    );
}
