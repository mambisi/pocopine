//! End-to-end tests for `#[query_resource]`.
//!
//! Declare a queryable resource, build queries with the typed DSL,
//! exercise the macro-generated predicate evaluator on row data.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_sync_query::{params, Order};
use pocopine_sync_query_macros::query_resource;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    id: String,
    workspace_id: String,
    assignee_id: Option<String>,
    title: String,
    status: Status,
    priority: u32,
}

#[query_resource(
    name = "issues",
    row = Issue,
    schema_version = 1,
    params(
        workspace_id: String,
        assignee_id: Option<String>,
        status: params::InSet<Status>,
        title: params::Contains,
        priority: params::Range<u32>,
    ),
)]
pub struct Issues;

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

    let q = Issues::query()
        .eq(field::workspace_id, "W1")
        .any_of(field::status, [Status::Open, Status::InProgress])
        .unwrap()
        .contains(field::title, "auth")
        .unwrap()
        .range(field::priority, params::Range::closed(1u32, 10))
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

    let q = Issues::query().eq(field::workspace_id, "W1").build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut other = sample_issue();
    other.workspace_id = "W2".to_string();
    assert!(!issues::matches(&q, &other));
}

#[test]
fn matches_optional_eq_some_value() {
    use issues::field;

    let q = Issues::query()
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

    let q = Issues::query()
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

    let q = Issues::query()
        .eq(field::workspace_id, "W1")
        .range(field::priority, params::Range::closed(1u32, 5))
        .build();
    assert!(issues::matches(&q, &sample_issue()));
    let mut high = sample_issue();
    high.priority = 6;
    assert!(!issues::matches(&q, &high));
}

#[test]
fn matches_contains_case_insensitive() {
    use issues::field;

    let q = Issues::query()
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

    let a = Issues::query().eq(field::workspace_id, "W1").build();
    let b = Issues::query().eq(field::workspace_id, "W2").build();
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
