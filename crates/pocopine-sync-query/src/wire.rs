//! Wire-envelope builders.
//!
//! These functions translate a typed `Query<Row>` into the
//! `pocopine_sync` request envelopes that hit `/open`, `/pull`, and
//! `/push`. The wire shape is settled (see `pocopine_sync::protocol`);
//! these helpers just glue the typed query onto the existing
//! envelopes so the runtime can call them without re-deriving the
//! mapping at every call site.
//!
//! The functions are thin enough that callers could open-code them,
//! but having them in one place keeps the order/limit reserved-key
//! convention visible.

use pocopine_sync::{
    StreamParams, SyncOpenRequest, SyncPullRequest, SyncPushRequest, SyncStreamSubscription,
};
use serde_json::Value;

use crate::mutator::Mutator;
use crate::query::Query;

/// Reserved param keys for order/limit. The macro DSL emits these
/// when `.order_by(...)` or `.limit(...)` are set; servers that don't
/// understand them should treat them as opaque (the wire envelope is
/// just `BTreeMap<String, Value>` — unknown keys reject unless the
/// source overrides `validate_params`).
pub const ORDER_BY_KEY: &str = "__order_by";
pub const LIMIT_KEY: &str = "__limit";

/// Encode the order_by / limit metadata into the wire params map.
///
/// The base `params` already carries the comparator entries. This
/// helper folds in the reserved keys so the server sees the full
/// query shape.
pub fn encode_query_to_params<Row>(query: &Query<Row>) -> StreamParams {
    let mut params = query.params().clone();
    if let Some(order) = query.order_by() {
        params.insert(
            ORDER_BY_KEY.to_string(),
            serde_json::json!({
                "field": &order.field,
                "direction": match order.direction {
                    crate::Order::Asc => "asc",
                    crate::Order::Desc => "desc",
                },
            }),
        );
    }
    if let Some(limit) = query.limit() {
        params.insert(LIMIT_KEY.to_string(), serde_json::json!(limit));
    }
    params
}

/// Build the `/open` request envelope for a single typed query.
pub fn build_open_request<Row>(query: &Query<Row>) -> SyncOpenRequest {
    SyncOpenRequest::new([SyncStreamSubscription {
        stream: query.stream().clone(),
        params: encode_query_to_params(query),
    }])
}

/// Build the `/pull` request envelope for a typed query.
pub fn build_pull_request<Row>(
    query: &Query<Row>,
    cursor: Option<pocopine_sync::SyncCursor>,
) -> SyncPullRequest {
    SyncPullRequest::new(query.stream().clone())
        .params(encode_query_to_params(query))
        .cursor(cursor)
}

/// Build the `/push` request envelope for a single mutator-produced
/// `ClientMutation`. Push envelopes carry EMPTY params — mutators are
/// query-agnostic on the wire; the client routes via predicate
/// evaluation in [`crate::QueryClient::route_optimistic_changes`].
///
/// This matches the design's "mutators don't know about queries"
/// property. Server-side, `validate_push_params` accepts the empty
/// case (the default on `SyncStreamSource` matches `validate_params`,
/// which itself accepts empty by default — sources that want strict
/// shape validation on push override accordingly).
pub fn build_push_request<M: Mutator>(
    mutation: pocopine_sync::ClientMutation<M::Payload>,
) -> SyncPushRequest<M::Payload> {
    SyncPushRequest::new(
        pocopine_sync::SyncStreamName::new(M::STREAM)
            .expect("Mutator::STREAM is a valid sync token (compile-time invariant)"),
        [mutation],
    )
    .params(StreamParams::new())
    .with_schema_version(M::SCHEMA_VERSION)
}

/// Silence the unused-Value warning on host. `serde_json::Value` is
/// referenced by the return types of the macro-emitted code; this
/// alias keeps the rust-analyzer hint about it stable across the
/// surface area.
#[doc(hidden)]
pub type WireValue = Value;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Order, Query};
    use pocopine_sync::SyncStreamName;

    fn issues_query() -> Query<()> {
        Query::builder(SyncStreamName::new("issues").unwrap())
            .raw_param("workspace_id", serde_json::json!("W1"))
            .order_by_raw("created_at", Order::Desc)
            .limit(50)
            .build()
    }

    #[test]
    fn encode_includes_order_and_limit() {
        let q = issues_query();
        let params = encode_query_to_params(&q);
        assert!(params.contains_key("workspace_id"));
        assert_eq!(
            params.get(ORDER_BY_KEY),
            Some(&serde_json::json!({"field": "created_at", "direction": "desc"}))
        );
        assert_eq!(params.get(LIMIT_KEY), Some(&serde_json::json!(50)));
    }

    #[test]
    fn open_request_carries_query_params() {
        let q = issues_query();
        let req = build_open_request(&q);
        assert_eq!(req.streams.len(), 1);
        assert_eq!(req.streams[0].stream.as_str(), "issues");
        assert!(req.streams[0].params.contains_key("workspace_id"));
        assert!(req.streams[0].params.contains_key(ORDER_BY_KEY));
        assert!(req.streams[0].params.contains_key(LIMIT_KEY));
    }

    #[test]
    fn pull_request_carries_cursor_and_params() {
        let q = issues_query();
        let cursor = pocopine_sync::SyncCursor::new("cursor_1").ok();
        let req = build_pull_request(&q, cursor.clone());
        assert_eq!(req.stream.as_str(), "issues");
        assert_eq!(req.cursor, cursor);
        assert!(req.params.contains_key("workspace_id"));
    }
}
