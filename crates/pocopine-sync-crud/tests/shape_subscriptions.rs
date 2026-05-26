#![cfg(not(target_arch = "wasm32"))]

//! Macro `params(...)` integration test for RFC 085 Batch 2.
//!
//! Verifies the macro-generated typed `StreamParams` struct emits
//! the right wire shape, decodes via `extract`, and rejects unknown
//! or malformed params.
//!
//! NOTE: server-side auto-validation via
//! `SyncStreamSource::validate_params` is intentionally NOT wired up
//! in Batch 2 — source authors call `StreamParams::extract` directly
//! from their own `pull`/`push` (or from a manually-overridden
//! `validate_params`). The auto-wire ships in a follow-up Batch 2b.

use pocopine_auth::RequestContext;
use pocopine_sync::{RowVersion, SyncResult};
use pocopine_sync_crud::{
    async_trait, params, resource, CrudRemoveResult, CrudSource, CrudWriteResult,
};
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
    version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct IssueDraft {
    workspace_id: String,
    title: String,
    status: Status,
}

#[derive(Clone, Default)]
struct Issues;

#[resource(
    name = "issues",
    schema_version = 1,
    params(
        workspace_id: String,
        assignee_id: Option<String>,
        status: params::InSet<Status>,
        title: params::Contains,
    ),
)]
#[async_trait]
impl CrudSource for Issues {
    type Id = String;
    type Row = Issue;
    type Draft = IssueDraft;

    async fn list(&self, _ctx: RequestContext, _limit: usize) -> SyncResult<Vec<Self::Row>> {
        Ok(Vec::new())
    }

    async fn get(&self, _ctx: RequestContext, _id: Self::Id) -> SyncResult<Option<Self::Row>> {
        Ok(None)
    }

    async fn create(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _draft: Self::Draft,
    ) -> SyncResult<Self::Row> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }

    async fn save(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _draft: Self::Draft,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Self::Row>> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }

    async fn remove(
        &self,
        _ctx: RequestContext,
        _id: Self::Id,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Self::Row>> {
        Err(pocopine_sync::SyncError::unsupported("test stub"))
    }
}

#[test]
fn typed_stream_params_round_trips_through_wire() {
    let typed = issues::StreamParams {
        workspace_id: "W".to_string(),
        assignee_id: Some("alice".to_string()),
        status: params::InSet::new([Status::Open, Status::InProgress]).unwrap(),
        title: params::Contains::icontains("auth").unwrap(),
    };
    let wire = typed.serialize_params();

    // All non-None fields appear on the wire.
    assert!(wire.contains_key("workspace_id"));
    assert!(wire.contains_key("assignee_id"));
    assert!(wire.contains_key("status"));
    assert!(wire.contains_key("title"));

    let decoded = issues::StreamParams::extract(&wire).expect("decode");
    assert_eq!(decoded.workspace_id, "W");
    assert_eq!(decoded.assignee_id.as_deref(), Some("alice"));
    assert_eq!(decoded.status.values(), &[Status::Open, Status::InProgress]);
    assert_eq!(decoded.title.contains, "auth");
    assert!(!decoded.title.case_sensitive);
}

#[test]
fn typed_stream_params_omits_none_optional_field() {
    let typed = issues::StreamParams {
        workspace_id: "W".to_string(),
        assignee_id: None,
        status: params::InSet::new([Status::Open]).unwrap(),
        title: params::Contains::icontains("auth").unwrap(),
    };
    let wire = typed.serialize_params();

    // None-valued optionals are omitted from the wire shape.
    assert!(!wire.contains_key("assignee_id"));

    let decoded = issues::StreamParams::extract(&wire).expect("decode");
    assert!(decoded.assignee_id.is_none());
}

#[test]
fn extract_rejects_missing_required() {
    let empty = pocopine_sync::StreamParams::new();
    let err = issues::StreamParams::extract(&empty).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("missing required param") && msg.contains("workspace_id"),
        "unexpected: {msg}"
    );
}

#[test]
fn extract_rejects_unknown_param_key() {
    let mut wire = pocopine_sync::StreamParams::new();
    wire.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    wire.insert("status".to_string(), serde_json::json!({ "in": ["open"] }));
    wire.insert(
        "title".to_string(),
        serde_json::json!({ "contains": "auth", "case_sensitive": false }),
    );
    wire.insert(
        "typo_key".to_string(),
        serde_json::Value::String("ignored".to_string()),
    );

    let err = issues::StreamParams::extract(&wire).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown param") && msg.contains("typo_key"),
        "unexpected: {msg}"
    );
}

#[test]
fn extract_rejects_wrong_shape_for_inset() {
    let mut wire = pocopine_sync::StreamParams::new();
    wire.insert(
        "workspace_id".to_string(),
        serde_json::Value::String("W".to_string()),
    );
    // status is declared as InSet<Status>, expects { "in": [...] }
    // but here we send a bare string — must reject.
    wire.insert(
        "status".to_string(),
        serde_json::Value::String("not-an-inset".to_string()),
    );
    wire.insert(
        "title".to_string(),
        serde_json::json!({ "contains": "auth", "case_sensitive": false }),
    );

    let err = issues::StreamParams::extract(&wire).unwrap_err();
    assert!(err.to_string().contains("params.status"));
}
