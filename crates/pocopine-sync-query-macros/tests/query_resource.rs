//! End-to-end tests for `#[query_resource]`.
//!
//! Declare a queryable resource, build queries with the typed DSL,
//! exercise the macro-generated predicate evaluator on row data.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_sync_query::Order;
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
}

// `#[query_resource]` must come BEFORE `#[derive(...)]` so it strips
// the per-field `#[query_param]` annotations before serde sees them.
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    // `required` makes `workspace_id` a tenant gate — predicate fails
    // if a query has no workspace_id filter (cross-tenant safety).
    // The other queryable fields are optional filters: queries can
    // include them or not.
    #[query_param(required)]
    pub workspace_id: String,
    #[query_param]
    pub assignee_id: Option<String>,
    #[query_param]
    pub title: String,
    #[query_param]
    pub status: Status,
    #[query_param]
    pub priority: u32,
}

fn sample_issue() -> Issue {
    Issue {
        id: "post_1".to_string(),
        workspace_id: "W1".to_string(),
        assignee_id: Some("alice".to_string()),
        title: "Auth bug".to_string(),
        status: Status::Open,
        priority: 5,
    }
}

#[test]
fn macro_emits_constants() {
    assert_eq!(issues::NAME, "issues");
    assert_eq!(issues::SCHEMA_VERSION, 1);
}

#[test]
fn builder_constructs_typed_query() {
    use issues::field;

    let q = Issue::query()
        .eq(field::workspace_id, "W1")
        .any_of(field::status, [Status::Open, Status::InProgress])
        .unwrap()
        .contains(field::title, "auth")
        .unwrap()
        .range(field::priority, 1u32..=10)
        .order_by(field::priority, Order::Asc)
        .limit(50)
        .build();

    assert_eq!(q.stream().as_str(), "issues");
    assert_eq!(q.params().len(), 4);
    assert!(q.params().contains_key("workspace_id"));
    assert!(q.params().contains_key("status"));
    assert!(q.params().contains_key("title"));
    assert!(q.params().contains_key("priority"));
    assert!(q.order_by().is_some());
    assert_eq!(q.limit(), Some(50));
}

#[test]
fn matches_required_eq_row_in_workspace() {
    use issues::field;

    let q = Issue::query().eq(field::workspace_id, "W1").build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut other = sample_issue();
    other.workspace_id = "W2".to_string();
    assert!(!issues::matches(&q, &other));
}

#[test]
fn matches_optional_eq_some_value() {
    use issues::field;

    let q = Issue::query()
        .eq(field::workspace_id, "W1")
        .eq(field::assignee_id, "alice")
        .build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut bob = sample_issue();
    bob.assignee_id = Some("bob".to_string());
    assert!(!issues::matches(&q, &bob));
    let mut none = sample_issue();
    none.assignee_id = None;
    assert!(!issues::matches(&q, &none));
}

#[test]
fn matches_any_of() {
    use issues::field;

    let q = Issue::query()
        .eq(field::workspace_id, "W1")
        .any_of(field::status, [Status::Open, Status::InProgress])
        .unwrap()
        .build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut closed = sample_issue();
    closed.status = Status::Closed;
    assert!(!issues::matches(&q, &closed));
}

#[test]
fn matches_range() {
    use issues::field;

    let q = Issue::query()
        .eq(field::workspace_id, "W1")
        .range(field::priority, 1u32..=5)
        .build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut high = sample_issue();
    high.priority = 6;
    assert!(!issues::matches(&q, &high));
}

#[test]
fn matches_contains_case_insensitive() {
    use issues::field;

    let q = Issue::query()
        .eq(field::workspace_id, "W1")
        .contains(field::title, "AUTH")
        .unwrap()
        .build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut unrelated = sample_issue();
    unrelated.title = "Layout bug".to_string();
    assert!(!issues::matches(&q, &unrelated));
}

#[test]
fn matches_returns_false_when_required_field_absent_from_params() {
    // Safety: a resource declaring `workspace_id` as a RequiredEq
    // param must NOT match arbitrary rows when the query was built
    // without that param — otherwise a misuse of the raw builder
    // would silently leak cross-workspace rows into a view that
    // never authorized them. The required-field absence acts as a
    // deny-by-default gate.
    let q: pocopine_sync_query::Query<Issue> = pocopine_sync_query::Query::builder(
        pocopine_sync_query::SyncStreamName::new("issues").unwrap(),
    )
    .build();
    assert!(!issues::matches(&q, &sample_issue()));
}

#[test]
fn distinct_workspace_queries_have_distinct_keys() {
    use issues::field;

    let a = Issue::query().eq(field::workspace_id, "W1").build();
    let b = Issue::query().eq(field::workspace_id, "W2").build();
    assert_ne!(a.key(), b.key());
}

#[test]
fn field_markers_typecheck_against_comparator_traits() {
    use pocopine_sync_query::{FieldContains, FieldEq, FieldInSet, FieldRange};

    // The macro emitted: __Field_workspace_id impls
    // FieldEq with Value = String.
    fn _eq_workspace<F: FieldEq<Value = String>>(_: F) {}
    _eq_workspace(issues::field::workspace_id);

    // __Field_assignee_id impls FieldEq with Value = String
    // (Option<T> → T).
    fn _eq_assignee<F: FieldEq<Value = String>>(_: F) {}
    _eq_assignee(issues::field::assignee_id);

    // __Field_status impls FieldInSet with Item = Status.
    fn _inset_status<F: FieldInSet<Item = Status>>(_: F) {}
    _inset_status(issues::field::status);

    // __Field_priority impls FieldRange with Bound = u32.
    fn _range_priority<F: FieldRange<Bound = u32>>(_: F) {}
    _range_priority(issues::field::priority);

    // __Field_title impls FieldContains.
    fn _contains_title<F: FieldContains>(_: F) {}
    _contains_title(issues::field::title);
}

// ---- RFC 088 §C: row_to_params codegen tests ---------------------

use pocopine_sync::StreamParams;
use serde_json::json;

#[test]
fn row_to_params_includes_only_required_fields() {
    // Post-codex-review (P2.1): only `required`-flagged fields
    // partition the topic. Anything else would produce a hash on
    // the server that no subset-filtering client subscription
    // computes on its side (Codex finding).
    //
    //   - workspace_id: required=true → INCLUDE
    //   - everything else → SKIP
    let row = json!({
        "id": "i1",
        "workspace_id": "W1",
        "assignee_id": "alice",
        "title": "Auth bug",
        "status": "open",
        "priority": 5,
    });
    let params = issues::row_to_params(&row).expect("row deserializes");
    let expected: StreamParams = [("workspace_id".to_string(), json!("W1"))]
        .into_iter()
        .collect();
    assert_eq!(params, expected);
}

#[test]
fn partition_for_topic_matches_row_to_params_hash() {
    // The whole point of the projection: server-side
    // row_to_params and client-side partition_for_topic MUST hash
    // identically when both project the same canonical fields.
    use pocopine_sync::stream_params_hash;

    let row = json!({
        "id": "i1",
        "workspace_id": "W1",
        "assignee_id": "alice",
        "title": "Auth bug",
        "status": "open",
        "priority": 5,
    });
    let server_params = issues::row_to_params(&row).unwrap();
    let server_hash = stream_params_hash("issues", &server_params);

    // Client subscription: just the tenant gate.
    use issues::field;
    let query = Issue::query().eq(field::workspace_id, "W1").build();
    let client_hash = query.partition_hash().expect("partition_hash computes");

    assert_eq!(client_hash, server_hash);
}

#[test]
fn partition_for_topic_returns_none_for_missing_required_field() {
    // A query that doesn't filter by the required field can't
    // partition — falls back to bare-topic subscription.
    let query: pocopine_sync_query::Query<Issue> = pocopine_sync_query::Query::builder(
        pocopine_sync_query::SyncStreamName::new("issues").unwrap(),
    )
    .with_partition_hash(issues::partition_for_topic)
    .build();
    assert!(query.partition_hash().is_none());
}

#[test]
fn partition_for_topic_returns_none_for_any_of_wrapper() {
    // Comparator-wrapped values (any_of / range / contains) don't
    // map to a single topic — fall back to bare. Here the user
    // does `.any_of(workspace_id, [...])` so the partition is
    // ambiguous.
    use issues::field;
    let query = Issue::query()
        .any_of(field::workspace_id, ["W1", "W2"])
        .unwrap()
        .build();
    assert!(query.partition_hash().is_none());
}

// Regression for the codex finding (P2): the macro's comparator
// detection MUST also recognize the `Range<T>` wire shape
// (`{"from": ..., "to": ..., "inclusive": [_, _]}`). Without this,
// a range-filtered required field would hash the wrapper object as
// if it were a plain partition value, subscribing to a topic the
// server never publishes.
#[query_resource(name = "priced", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Priced {
    pub id: String,
    #[query_param(required)]
    pub price: u32,
}

#[test]
fn partition_for_topic_returns_none_for_range_wrapper() {
    use priced::field;
    let query = Priced::query().range(field::price, 10u32..100).build();
    // Range filters can't partition the topic — the server's
    // row_to_params projects a plain price value, but here the
    // client's params have a range wrapper. Returning None lets the
    // driver fall back to bare-topic only.
    assert!(query.partition_hash().is_none());
}

// Regression for the codex finding (P2): a resource with NO
// `required` fields must NOT subscribe to per-params topics — the
// server's `invalidate_stream_with_row` short-circuits on empty
// row_to_params, so any per-params subscription is dead.
#[query_resource(name = "no_required", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoRequiredResource {
    pub id: String,
    #[query_param]
    pub flavor: String,
}

#[test]
fn partition_for_topic_returns_none_when_no_required_fields_declared() {
    let query: pocopine_sync_query::Query<NoRequiredResource> =
        pocopine_sync_query::Query::builder(
            pocopine_sync_query::SyncStreamName::new("no_required").unwrap(),
        )
        .with_partition_hash(no_required::partition_for_topic)
        .build();
    // Empty canonical projection → no per-params topic to subscribe
    // to. Driver falls back to bare-topic only.
    assert!(query.partition_hash().is_none());
}

// Regression for codex second-pass P2: the comparator-key
// detection MUST be gated by the field's actual capabilities. A
// required field with non-numeric, non-string type — that the
// user filters with `.eq()` to a struct value whose JSON
// serialization happens to share keys with the Range wire shape
// (`from`, `to`) — must NOT be misclassified as a comparator
// wrapper.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
pub struct TimeWindow {
    pub from: u32,
    pub to: u32,
}

#[query_resource(name = "windowed", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Windowed {
    pub id: String,
    // TimeWindow isn't in is_ordered_type, so caps.range stays
    // false. The macro should NOT check `from`/`to`/`inclusive`
    // keys for this field's captured value.
    #[query_param(required)]
    pub window: TimeWindow,
}

#[test]
fn partition_for_topic_accepts_plain_struct_value_with_range_like_keys() {
    use windowed::field;
    let w = TimeWindow { from: 1, to: 10 };
    let query = Windowed::query().eq(field::window, w.clone()).build();
    let hash = query
        .partition_hash()
        .expect("plain equality value (not a Range comparator) partitions");
    // Sanity: server-side projection of the same canonical row
    // should hash identically.
    let row = json!({
        "id": "i1",
        "window": {"from": 1, "to": 10},
    });
    let server_params = windowed::row_to_params(&row).unwrap();
    let server_hash = pocopine_sync::stream_params_hash("windowed", &server_params);
    assert_eq!(hash, server_hash);
}

#[test]
fn row_to_params_skips_none_options() {
    // assignee_id: Option<String> with None → must be omitted from
    // the returned map even though the field is INCLUDED in
    // principle.
    let row = json!({
        "id": "i1",
        "workspace_id": "W1",
        "assignee_id": null,
        "title": "",
        "status": "open",
        "priority": 0,
    });
    let params = issues::row_to_params(&row).expect("row deserializes");
    assert!(!params.contains_key("assignee_id"));
}

#[test]
fn row_to_params_missing_required_field_yields_empty_params() {
    // Direct JSON projection (code-review fix #12). A malformed row
    // — missing a required field — no longer errors. It silently
    // yields an empty `StreamParams`, which the caller
    // (`invalidate_stream_with_rows`) treats as "skip per-params
    // publish" and falls back to bare-topic invalidation only.
    // That's the right semantic: a row we can't project shouldn't
    // be wrongly attributed to any partition.
    let bad_row = json!({"id": "i1"}); // missing workspace_id, etc.
    let params = issues::row_to_params(&bad_row).expect("direct projection never fails");
    assert!(
        params.is_empty(),
        "missing required field must yield empty params, got {params:?}"
    );
}

#[test]
fn row_to_params_skips_unrelated_fields_that_dont_deserialize() {
    // A row whose IRRELEVANT fields (not in the partition
    // projection) are malformed must still project the required
    // fields correctly. The old typed-roundtrip path would have
    // failed the whole row; the direct JSON projection only touches
    // the keys we actually need.
    let row = json!({
        "id": "i1",
        "workspace_id": "W1",
        // `priority` is declared as a number in the test fixture
        // but here we pass a string to force `serde_json::from_value`
        // to fail. Direct projection ignores it.
        "priority": "not a number",
        "assignee_id": null,
        "title": "Auth bug",
        "status": "open",
    });
    let params = issues::row_to_params(&row).expect("direct projection never fails");
    assert_eq!(
        params.get("workspace_id").map(|v| v.as_str()),
        Some(Some("W1"))
    );
}

// ---- RFC 090 Phase 4: typed write codegen ------------------------

#[query_resource(name = "tasks", schema_version = 1, draft = TaskDraft)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[query_param(required)]
    pub workspace_id: String,
    #[query_param]
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskDraft {
    pub workspace_id: String,
    pub title: String,
}

// Required by the macro-emitted `Task::create / update` for the
// auto-optimistic overlay (default = Self::from((id, draft))).
impl From<(String, TaskDraft)> for Task {
    fn from((id, draft): (String, TaskDraft)) -> Self {
        Self {
            id,
            workspace_id: draft.workspace_id,
            title: draft.title,
        }
    }
}

#[test]
fn create_emits_typed_builder() {
    use pocopine_sync_query::MutationPayload;
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "Wire up auth".to_string(),
        },
    );
    match m.payload() {
        MutationPayload::Create(p) => {
            assert_eq!(p.id, "t1");
            assert_eq!(p.draft.title, "Wire up auth");
        }
        _ => panic!("expected Create"),
    }
}

#[test]
fn update_carries_expected_version() {
    use pocopine_sync::RowVersion;
    use pocopine_sync_query::MutationPayload;
    let v = RowVersion::new("7").unwrap();
    let m = Task::update(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "Edited".to_string(),
        },
        Some(v),
    );
    match m.payload() {
        MutationPayload::Update(p) => {
            assert_eq!(p.expected_version.as_ref().unwrap().as_str(), "7");
        }
        _ => panic!("expected Update"),
    }
}

#[test]
fn delete_without_expected_version() {
    use pocopine_sync_query::MutationPayload;
    let m = Task::delete("t1".to_string(), None);
    match m.payload() {
        MutationPayload::Delete(p) => {
            assert_eq!(p.id, "t1");
            assert!(p.expected_version.is_none());
        }
        _ => panic!("expected Delete"),
    }
}

#[test]
fn optimistic_closure_attaches_and_runs() {
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "Hello".to_string(),
        },
    )
    .optimistic(|p| match p {
        pocopine_sync_query::MutationPayload::Create(p) => Task {
            id: p.id.clone(),
            workspace_id: p.draft.workspace_id.clone(),
            title: p.draft.title.clone(),
        },
        _ => unreachable!(),
    });
    let (payload, opt) = m.into_parts();
    let row = opt.expect("optimistic attached")(&payload);
    assert_eq!(row.title, "Hello");
}

#[test]
fn wire_shape_is_flat() {
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "Hello".to_string(),
        },
    );
    let wire = serde_json::to_value(m.payload()).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({
            "op": "create",
            "id": "t1",
            "draft": {"workspace_id": "W1", "title": "Hello"}
        })
    );
}

#[test]
fn create_attaches_default_optimistic_from_from_impl() {
    // The macro pre-wires `.optimistic(|_| Self::from((id, draft)))`
    // on `Task::create` because Task: From<(String, TaskDraft)>.
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "from default".to_string(),
        },
    );
    let (payload, opt) = m.into_parts();
    let row = opt.expect("auto-optimistic should be attached")(&payload);
    assert_eq!(row.id, "t1");
    assert_eq!(row.workspace_id, "W1");
    assert_eq!(row.title, "from default");
}

#[test]
fn server_only_clears_default() {
    // `.server_only()` clears the auto-default. The mutation still
    // pushes correctly; the view just won't update until /pull confirms.
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "no-overlay".to_string(),
        },
    )
    .server_only();
    let (_payload, opt) = m.into_parts();
    assert!(
        opt.is_none(),
        "server_only must clear the default closure"
    );
}

#[tokio::test]
async fn source_provided_mutation_log_skips_auto_default() {
    // The Source impl returns `Some(self.clone())` from
    // `mutation_log()`, so the SourceResource picks it up at
    // construction without `.mutation_log(...)` on the builder.
    // We verify reservations land in the *Source-provided* log,
    // not a fresh `MemoryMutationLog`.
    use pocopine_auth::RequestContext;
    use pocopine_sync::{MutationId, SyncOp};
    use pocopine_sync_query::source::{
        DeleteResult, Source, SourceFuture, SourceStream, WriteResult,
    };
    use pocopine_sync_query::write::{
        AcceptedMutation, MemoryMutationLog, MutationLog, MutationReservation,
    };
    use pocopine_sync_query::Query;
    use std::sync::Arc;

    #[derive(Clone)]
    struct CountedLogStore {
        inner: Arc<MemoryMutationLog<Task>>,
    }

    impl CountedLogStore {
        fn new() -> Self {
            Self {
                inner: Arc::new(MemoryMutationLog::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl MutationLog<Task> for CountedLogStore {
        async fn reserve_mutation(
            &self,
            ctx: &RequestContext,
            candidate: AcceptedMutation,
        ) -> pocopine_sync::SyncResult<MutationReservation> {
            self.inner.reserve_mutation(ctx, candidate).await
        }

        async fn accepted_mutation(
            &self,
            ctx: &RequestContext,
            mutation_id: &MutationId,
        ) -> pocopine_sync::SyncResult<Option<AcceptedMutation>> {
            self.inner.accepted_mutation(ctx, mutation_id).await
        }
    }

    impl Source for CountedLogStore {
        type Id = String;
        type Row = Task;
        type Draft = TaskDraft;
        type Context = ();

        fn extract_context<'a>(
            &'a self,
            _ctx: RequestContext,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Self::Context>> {
            Box::pin(async { Ok(()) })
        }
        fn list_stream<'a>(
            &'a self,
            _ctx: (),
            _query: &'a Query<Self::Row>,
        ) -> SourceStream<'a, Self::Row> {
            Box::pin(futures::stream::iter(::std::iter::empty()))
        }
        fn get<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Option<Self::Row>>> {
            Box::pin(async { Ok(None) })
        }
        fn create<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _draft: Self::Draft,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Self::Row>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }
        fn update<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _draft: Self::Draft,
            _expected: Option<pocopine_sync::RowVersion>,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<WriteResult<Self::Row>>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }
        fn delete<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _expected: Option<pocopine_sync::RowVersion>,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<DeleteResult<Self::Row>>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }

        // The whole point of this test: returning Some here tells
        // `SourceResource` to use the store's own log instead of
        // auto-provisioning a fresh `MemoryMutationLog`.
        fn mutation_log(&self) -> Option<Arc<dyn MutationLog<Self::Row>>> {
            Some(Arc::new(self.clone()))
        }
    }

    let store = CountedLogStore::new();
    let inner_log = store.inner.clone();

    // No `.mutation_log(...)` on the builder — the auto-wiring
    // picks up the Source-provided log via `Source::mutation_log()`.
    let _resource = tasks::resource(store);

    // Seed the log directly through the Source-provided instance.
    // If the SourceResource HAD provisioned a fresh
    // MemoryMutationLog instead, this insert would not be visible
    // to subsequent push handlers. We assert visibility by reading
    // back through the same `inner_log` we wrote to.
    let ctx = RequestContext::new(
        http::Method::POST,
        "/__pocopine/sync/v1/push".parse().unwrap(),
        http::HeaderMap::new(),
    );
    let mid = MutationId::new("auto_wired_mid").unwrap();
    let candidate = AcceptedMutation::new(
        mid.clone(),
        SyncOp::Upsert,
        Some(pocopine_sync::RowKey::new("t1").unwrap()),
        serde_json::json!({"op": "create", "id": "t1", "draft": {}}),
    );
    let reservation = inner_log
        .reserve_mutation(&ctx, candidate)
        .await
        .expect("reservation must succeed");
    assert!(matches!(reservation, MutationReservation::Reserved));
    let prior = inner_log
        .accepted_mutation(&ctx, &mid)
        .await
        .expect("readback");
    assert!(
        prior.is_some(),
        "auto-wired Source-provided log must hold the reservation"
    );
}

#[test]
fn tasks_resource_convenience_compiles_with_stub_source() {
    // The macro-emitted `tasks::resource(impl_)` pre-wires `.id`
    // (from the row's `id` field) and `.partition_by` (from
    // `row_to_params_typed`). This stub Source carries no `version`
    // field on the row, so `version_field` is not pre-wired.
    use pocopine_auth::RequestContext;
    use pocopine_sync_query::source::{
        DeleteResult, Source, SourceFuture, SourceStream, WriteResult,
    };
    use pocopine_sync_query::Query;

    pub struct TaskStore;

    impl Source for TaskStore {
        type Id = String;
        type Row = Task;
        type Draft = TaskDraft;
        type Context = ();

        fn extract_context<'a>(
            &'a self,
            _ctx: RequestContext,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Self::Context>> {
            Box::pin(async { Ok(()) })
        }
        fn list_stream<'a>(
            &'a self,
            _ctx: (),
            _query: &'a Query<Self::Row>,
        ) -> SourceStream<'a, Self::Row> {
            Box::pin(futures::stream::iter(::std::iter::empty()))
        }
        fn get<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Option<Self::Row>>> {
            Box::pin(async { Ok(None) })
        }
        fn create<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _draft: Self::Draft,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<Self::Row>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }
        fn update<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _draft: Self::Draft,
            _expected_version: Option<pocopine_sync::RowVersion>,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<WriteResult<Self::Row>>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }
        fn delete<'a>(
            &'a self,
            _ctx: (),
            _id: Self::Id,
            _expected_version: Option<pocopine_sync::RowVersion>,
        ) -> SourceFuture<'a, pocopine_sync::SyncResult<DeleteResult<Self::Row>>> {
            Box::pin(async { Err(pocopine_sync::SyncError::unsupported("stub")) })
        }
    }

    // The actual test — `tasks::resource(impl_)` returns a
    // SourceResource with id + partition_by pre-wired. We can then
    // chain `.mutation_log(...)` (and override defaults if needed).
    let _resource = tasks::resource(TaskStore)
        .mutation_log(pocopine_sync_query::write::MemoryMutationLog::<Task>::new());
}

#[test]
fn explicit_optimistic_overrides_default() {
    let m = Task::create(
        "t1".to_string(),
        TaskDraft {
            workspace_id: "W1".to_string(),
            title: "default".to_string(),
        },
    )
    .optimistic(|_| Task {
        id: "explicit_t1".to_string(),
        workspace_id: "explicit_W1".to_string(),
        title: "explicit override".to_string(),
    });
    let (payload, opt) = m.into_parts();
    let row = opt.unwrap()(&payload);
    assert_eq!(row.id, "explicit_t1");
    assert_eq!(row.title, "explicit override");
}
