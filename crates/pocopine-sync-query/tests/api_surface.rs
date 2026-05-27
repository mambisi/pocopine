//! Smoke tests that exercise the public API surface of
//! `pocopine-sync-query`. These don't drive the runtime (which is TBD)
//! — they verify the types compose, the traits are object-safe where
//! they need to be, and the macro-target surface is reachable.

#![cfg(not(target_arch = "wasm32"))]

use pocopine_sync::{MutationId, RowKey, SyncResult, SyncStreamName};
use pocopine_sync_query::{
    params, FieldEq, FieldInSet, MutationOutcome, Mutator, MutatorRemoteContext,
    MutatorRemoteFuture, Order, OrderBy, Query, RowChange,
};

// ---- Query building ----------------------------------------------------

#[test]
fn query_builder_constructs_typed_query() {
    let stream = SyncStreamName::new("issues").unwrap();
    let q: Query<()> = Query::builder(stream.clone())
        .raw_param("workspace_id", serde_json::json!("W1"))
        .raw_param("status", serde_json::json!({"in": ["open"]}))
        .order_by("created_at", Order::Desc)
        .limit(50)
        .build();

    assert_eq!(q.stream(), &stream);
    assert_eq!(q.params().len(), 2);
    assert_eq!(
        q.order_by(),
        Some(&OrderBy {
            field: "created_at".to_string(),
            direction: Order::Desc,
        })
    );
    assert_eq!(q.limit(), Some(50));
}

#[test]
fn distinct_queries_have_distinct_keys() {
    let stream = SyncStreamName::new("issues").unwrap();
    let a: Query<()> = Query::builder(stream.clone())
        .raw_param("workspace_id", serde_json::json!("W1"))
        .build();
    let b: Query<()> = Query::builder(stream)
        .raw_param("workspace_id", serde_json::json!("W2"))
        .build();
    assert_ne!(a.key(), b.key());
    assert_eq!(a, a.clone());
    assert_ne!(a, b);
}

// ---- Comparator wrappers -----------------------------------------------

#[test]
fn comparator_wrappers_round_trip() {
    let set = params::InSet::new(["open", "in_progress"]).unwrap();
    let json = serde_json::to_value(&set).unwrap();
    let decoded: params::InSet<String> = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.values(), &["open", "in_progress"]);

    let range = params::Range::closed(1u32, 10);
    let json = serde_json::to_value(&range).unwrap();
    let decoded: params::Range<u32> = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.from, Some(1));
    assert_eq!(decoded.to, Some(10));

    let contains = params::Contains::icontains("auth").unwrap();
    let json = serde_json::to_value(&contains).unwrap();
    let decoded: params::Contains = serde_json::from_value(json).unwrap();
    assert_eq!(decoded.contains, "auth");
    assert!(!decoded.case_sensitive);
}

#[test]
fn comparator_constructors_reject_invariant_violations() {
    assert!(params::InSet::<i32>::new(Vec::<i32>::new()).is_err());
    assert!(params::Contains::icontains("").is_err());
    let unbounded: params::Range<i32> = params::Range {
        from: None,
        to: None,
        inclusive: (true, true),
    };
    // Range construction via struct literal is allowed; the wire
    // deserializer rejects fully-unbounded ranges.
    let json = serde_json::to_value(&unbounded).unwrap();
    assert!(serde_json::from_value::<params::Range<i32>>(json).is_err());
}

// ---- Sealed comparator traits ------------------------------------------
//
// We can't easily test the COMPILE-FAIL property without trybuild, but we
// can verify the traits exist and are referenceable.

#[test]
fn sealed_traits_exist() {
    // Just a sanity check that the trait paths resolve. The macro
    // (TBD) emits impls of these. The traits are NOT object-safe
    // by design — the sealed-trait gate uses an unsized marker.
    fn _accept_field_eq<F: FieldEq<Value = String>>(_f: F) {}
    fn _accept_field_inset<F: FieldInSet<Item = String>>(_f: F) {}
}

// ---- Mutator trait -----------------------------------------------------

/// Toy mutator for surface testing. Adds an Upsert row.
struct ToyCreate;

impl Mutator for ToyCreate {
    type Payload = serde_json::Value;
    type Row = serde_json::Value;
    const NAME: &'static str = "toy_create";
    const STREAM: &'static str = "toy";
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

#[test]
fn mutator_can_be_implemented() {
    let payload = serde_json::json!({"id": "row_1", "title": "test"});
    let changes = ToyCreate::apply_local(&payload);
    assert_eq!(changes.len(), 1);
    assert!(matches!(changes[0], RowChange::Upsert(ref v) if v == &payload));
}

#[test]
fn row_change_variants() {
    let up: RowChange<i32> = RowChange::Upsert(42);
    let del: RowChange<i32> = RowChange::Delete(RowKey::new("row_1").unwrap());
    match up {
        RowChange::Upsert(n) => assert_eq!(n, 42),
        _ => panic!(),
    }
    match del {
        RowChange::Delete(k) => assert_eq!(k.as_str(), "row_1"),
        _ => panic!(),
    }
}

// ---- MutatorRemoteContext is object-safe -------------------------------

struct StubContext;
impl MutatorRemoteContext for StubContext {
    fn push_url(&self) -> &str {
        "/__pocopine/sync/v1/push"
    }
    fn next_mutation_id(&self) -> SyncResult<MutationId> {
        MutationId::new("test:1")
    }
}

#[tokio::test]
async fn mutator_remote_runs_through_context() {
    let ctx = StubContext;
    let payload = serde_json::json!({"id": "row_2"});
    let changes = ToyCreate::apply_remote(&ctx, payload.clone())
        .await
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert!(matches!(changes[0], RowChange::Upsert(ref v) if v == &payload));
}

// ---- MutationOutcome shape ---------------------------------------------

#[test]
fn mutation_outcome_variants() {
    let _accepted: MutationOutcome<i32> = MutationOutcome::Accepted(vec![RowChange::Upsert(1)]);
    let _rejected: MutationOutcome<i32> = MutationOutcome::Rejected {
        reason: "nope".to_string(),
    };
    let _conflict: MutationOutcome<i32> = MutationOutcome::Conflict {
        server_rows: vec![],
    };
}
