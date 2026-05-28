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
        .order_by("priority", Order::Asc)
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
fn row_to_params_propagates_deserialize_error() {
    // A malformed row (missing required field) bubbles up as
    // SyncError::client, NOT a panic — the server logs + falls back
    // to bare-topic publish.
    let bad_row = json!({"id": "i1"}); // missing workspace_id, etc.
    let result = issues::row_to_params(&bad_row);
    assert!(result.is_err());
}
