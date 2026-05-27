//! End-to-end routing test: macro-generated predicate evaluator feeds
//! the `QueryClient`'s routing engine. Verifies that one mutation's
//! row changes are visible in matching queries and invisible in
//! non-matching queries — the design's "no active subscription"
//! property.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_sync::{ClientMutation, MutationId, SyncOp};
#[allow(unused_imports)] // referenced by the macro emission
use pocopine_sync_query::params;
use pocopine_sync_query::{QueryClient, RowChange};
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

fn wire_mutation() -> ClientMutation<serde_json::Value> {
    ClientMutation {
        id: MutationId::new("device:1").unwrap(),
        key: None,
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::Value::Null,
        migration_outcome: None,
    }
}

#[test]
fn matching_query_receives_optimistic_overlay() {
    let client = QueryClient::new();
    let q = Issues::query()
        .workspace_id("W1".to_string())
        .status_in([Status::Open, Status::InProgress])
        .unwrap()
        .build();
    let handle = client.subscribe::<Issue>(q.clone());

    let row = Issue {
        id: "issue_1".to_string(),
        workspace_id: "W1".to_string(),
        title: "Auth bug".to_string(),
        status: Status::Open,
    };
    let mutation = wire_mutation();
    let changes = vec![RowChange::Upsert(row.clone())];
    client.route_optimistic_changes::<Issue>(q.stream(), &mutation.id, &mutation, &changes);

    let state = handle.state();
    assert_eq!(state.pending().len(), 1);
    let overlay = &state.pending()[0];
    assert_eq!(overlay.mutation_id.as_str(), "device:1");
    assert!(overlay.optimistic_row.is_some());
    assert_eq!(overlay.optimistic_row.as_ref().unwrap().value, row);
}

#[test]
fn non_matching_query_does_not_receive_overlay() {
    let client = QueryClient::new();
    let q_w1 = Issues::query().workspace_id("W1".to_string()).build();
    let q_w2 = Issues::query().workspace_id("W2".to_string()).build();
    let h_w1 = client.subscribe::<Issue>(q_w1.clone());
    let h_w2 = client.subscribe::<Issue>(q_w2.clone());

    // A W1 mutation should appear in W1's overlay only.
    let row = Issue {
        id: "issue_1".to_string(),
        workspace_id: "W1".to_string(),
        title: "test".to_string(),
        status: Status::Open,
    };
    let mutation = wire_mutation();
    let changes = vec![RowChange::Upsert(row)];
    client.route_optimistic_changes::<Issue>(q_w1.stream(), &mutation.id, &mutation, &changes);

    assert_eq!(h_w1.state().pending().len(), 1);
    assert_eq!(h_w2.state().pending().len(), 0);
}

#[test]
fn canonical_response_dequeues_overlay_and_upserts_canonical() {
    let client = QueryClient::new();
    let q = Issues::query().workspace_id("W1".to_string()).build();
    let handle = client.subscribe::<Issue>(q.clone());

    let row = Issue {
        id: "issue_1".to_string(),
        workspace_id: "W1".to_string(),
        title: "test".to_string(),
        status: Status::Open,
    };
    let mutation = wire_mutation();

    // 1. Optimistic apply: overlay added.
    client.route_optimistic_changes::<Issue>(
        q.stream(),
        &mutation.id,
        &mutation,
        &[RowChange::Upsert(row.clone())],
    );
    assert_eq!(handle.state().pending().len(), 1);
    assert_eq!(handle.state().canonical_len(), 0);

    // 2. Canonical reconciliation: server confirmed; overlay
    //    removed; canonical row inserted.
    let canonical_row = pocopine_sync::SyncRow::new("issue_1", row.clone()).unwrap();
    client.route_canonical_changes::<Issue>(q.stream(), &mutation.id, &[canonical_row]);

    let state = handle.state();
    assert_eq!(state.pending().len(), 0);
    assert_eq!(state.canonical_len(), 1);
}

#[test]
fn canonical_row_no_longer_matching_is_removed() {
    let client = QueryClient::new();
    let q = Issues::query()
        .workspace_id("W1".to_string())
        .status_in([Status::Open])
        .unwrap()
        .build();
    let handle = client.subscribe::<Issue>(q.clone());

    // Seed: optimistic + canonical for an Open issue.
    let row_open = Issue {
        id: "issue_1".to_string(),
        workspace_id: "W1".to_string(),
        title: "test".to_string(),
        status: Status::Open,
    };
    let mutation_id = MutationId::new("device:1").unwrap();
    client.route_canonical_changes::<Issue>(
        q.stream(),
        &mutation_id,
        &[pocopine_sync::SyncRow::new("issue_1", row_open.clone()).unwrap()],
    );
    assert_eq!(handle.state().canonical_len(), 1);

    // Server now confirms the row has moved to status=Closed. It no
    // longer matches our subscription (status_in=[Open]) — should be
    // removed from canonical.
    let row_closed = Issue {
        status: Status::Closed,
        ..row_open
    };
    client.route_canonical_changes::<Issue>(
        q.stream(),
        &mutation_id,
        &[pocopine_sync::SyncRow::new("issue_1", row_closed).unwrap()],
    );

    assert_eq!(handle.state().canonical_len(), 0);
}
