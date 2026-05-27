//! `Query<Row>` — the unit of identity for `pocopine-sync-query`.
//!
//! A `Query<Row>` is a declarative description of "what data do I want?".
//! Two queries with the same canonical form share a [`QueryKey`] and (in
//! the runtime) share one underlying `QuerySubscription`. The macro
//! generates per-resource builders that produce `Query<Row>` instances
//! and emit the typed predicate evaluator.

use std::marker::PhantomData;

use pocopine_sync::{StreamParams, SyncStreamName};

/// A declarative query.
///
/// Built via [`Query::builder`] or via the macro-generated `Resource::query()`
/// helper. Holds:
///
/// * `stream` — the server-registered stream name.
/// * `params` — the canonical comparator map (sorted, blake3 / FNV-1a-hashable).
/// * `order_by` — optional ordering directive (applied by the source server-side
///   and re-applied client-side on the rendered overlay).
/// * `limit` — optional row cap.
///
/// `Query<Row>` is `Clone`, `Eq`, and yields a stable [`QueryKey`] via
/// [`Query::key`]. The `Row` type parameter is erased on the wire (everything
/// goes through `serde_json::Value`) but typed at the API.
///
/// `Row` is held in a `PhantomData<fn() -> Row>`, so neither `Clone` nor
/// `Debug` for `Query<Row>` require `Row: Clone + Debug`.
#[derive(Debug)]
pub struct Query<Row> {
    pub(crate) stream: SyncStreamName,
    pub(crate) params: StreamParams,
    pub(crate) order_by: Option<OrderBy>,
    pub(crate) limit: Option<u32>,
    /// Function pointer to the macro-generated predicate evaluator.
    /// `None` for queries built without a `#[query_resource]` macro
    /// declaration (which then can't participate in routing — they're
    /// useful as a raw envelope but won't receive optimistic updates).
    pub(crate) matches_fn: Option<MatchFn<Row>>,
    pub(crate) _row: PhantomData<fn() -> Row>,
}

/// Signature of a query's predicate evaluator. The macro emits one
/// `fn(&Query<Row>, &Row) -> bool` per declared `#[query_resource]`
/// and the builder injects it via [`QueryBuilder::with_matches`].
pub type MatchFn<Row> = fn(query: &Query<Row>, row: &Row) -> bool;

impl<Row> Query<Row> {
    /// Build a query from its parts. Most callers go through the macro-
    /// generated `Resource::query()` builder, which constructs the params
    /// map via typed `where_*` setters and injects the macro-generated
    /// predicate evaluator via [`QueryBuilder::with_matches`].
    pub fn builder(stream: SyncStreamName) -> QueryBuilder<Row> {
        QueryBuilder {
            stream,
            params: StreamParams::new(),
            order_by: None,
            limit: None,
            matches_fn: None,
            _row: PhantomData,
        }
    }

    /// True when the row matches every declared param's comparator
    /// constraint. Delegates to the macro-generated predicate
    /// evaluator stored in `matches_fn`. For queries built without
    /// a `#[query_resource]` macro declaration (no predicate
    /// registered), returns `true` — caller should treat that as
    /// "this query doesn't participate in routing."
    pub fn matches(&self, row: &Row) -> bool {
        match self.matches_fn {
            Some(f) => f(self, row),
            None => true,
        }
    }

    /// The query's server-registered stream name.
    pub fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    /// The canonical comparator map. Sorted by key.
    pub fn params(&self) -> &StreamParams {
        &self.params
    }

    /// Optional ordering directive.
    pub fn order_by(&self) -> Option<&OrderBy> {
        self.order_by.as_ref()
    }

    /// Optional row cap.
    pub fn limit(&self) -> Option<u32> {
        self.limit
    }

    /// Stable canonical identity. Two queries with the same stream, params,
    /// order, and limit yield equal `QueryKey`s and (in the runtime) share
    /// one underlying subscription.
    ///
    /// The key folds in the FULL stream name + canonical params JSON +
    /// order_by + limit. FNV-1a 64-bit — same scheme as
    /// [`pocopine_sync::local_stream_key`], but with a wider domain.
    pub fn key(&self) -> QueryKey {
        let mut hasher_input: Vec<u8> = Vec::with_capacity(256);
        hasher_input.extend_from_slice(self.stream.as_str().as_bytes());
        hasher_input.push(0u8);
        // serde_json::to_vec on a BTreeMap is canonical (sorted by key).
        let params_bytes = serde_json::to_vec(&self.params).unwrap_or_default();
        hasher_input.extend_from_slice(&params_bytes);
        hasher_input.push(0u8);
        if let Some(ob) = &self.order_by {
            hasher_input.extend_from_slice(ob.field.as_bytes());
            hasher_input.push(0u8);
            hasher_input.push(match ob.direction {
                Order::Asc => 1,
                Order::Desc => 2,
            });
        }
        hasher_input.push(0u8);
        if let Some(limit) = self.limit {
            hasher_input.extend_from_slice(&limit.to_le_bytes());
        }
        QueryKey(fnv1a_64(&hasher_input))
    }
}

impl<Row> Clone for Query<Row> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            params: self.params.clone(),
            order_by: self.order_by.clone(),
            limit: self.limit,
            matches_fn: self.matches_fn,
            _row: PhantomData,
        }
    }
}

impl<Row> PartialEq for Query<Row> {
    fn eq(&self, other: &Self) -> bool {
        self.stream == other.stream
            && self.params == other.params
            && self.order_by == other.order_by
            && self.limit == other.limit
    }
}

impl<Row> Eq for Query<Row> {}

/// Stable identity for a [`Query`]. Used as the hash key in the
/// subscription registry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Ord, PartialOrd)]
pub struct QueryKey(u64);

impl QueryKey {
    /// Returns the raw 64-bit identifier. Mostly useful for logging.
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for QueryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// Ordering directive on a query.
///
/// The framework expects the source's `/pull` response to return rows in
/// the requested order. The client re-applies the same ordering to pending
/// optimistic overlays when rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderBy {
    pub field: String,
    pub direction: Order,
}

/// Ascending or descending.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Order {
    Asc,
    Desc,
}

/// Builder produced by [`Query::builder`] and the macro-generated
/// `Resource::query()` helper. Provides typed `.where_*` / `.order_by` /
/// `.limit` setters; finalize with `.build()`.
///
/// The macro generates wrappers over this that constrain the `where_*`
/// methods to the resource's declared fields via the sealed comparator
/// traits in [`crate::predicate`].
#[derive(Debug)]
pub struct QueryBuilder<Row> {
    stream: SyncStreamName,
    params: StreamParams,
    order_by: Option<OrderBy>,
    limit: Option<u32>,
    matches_fn: Option<MatchFn<Row>>,
    _row: PhantomData<fn() -> Row>,
}

impl<Row> Clone for QueryBuilder<Row> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            params: self.params.clone(),
            order_by: self.order_by.clone(),
            limit: self.limit,
            matches_fn: self.matches_fn,
            _row: PhantomData,
        }
    }
}

impl<Row> QueryBuilder<Row> {
    /// Untyped param insert. The macro-generated `.where_*` helpers
    /// route through this after typing-checking the field marker.
    pub fn raw_param(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.params.insert(key.into(), value);
        self
    }

    /// Set the ordering.
    pub fn order_by(mut self, field: impl Into<String>, direction: Order) -> Self {
        self.order_by = Some(OrderBy {
            field: field.into(),
            direction,
        });
        self
    }

    /// Set the row cap.
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Borrow the params map being built. Useful for tests + introspection.
    pub fn params(&self) -> &StreamParams {
        &self.params
    }

    /// Inject the macro-generated predicate evaluator. Called by
    /// the macro's `QueryBuilder::new()` so that queries built via
    /// `#[query_resource]` participate in routing automatically.
    pub fn with_matches(mut self, matches_fn: MatchFn<Row>) -> Self {
        self.matches_fn = Some(matches_fn);
        self
    }

    /// Finalize into a [`Query<Row>`].
    pub fn build(self) -> Query<Row> {
        Query {
            stream: self.stream,
            params: self.params,
            order_by: self.order_by,
            limit: self.limit,
            matches_fn: self.matches_fn,
            _row: PhantomData,
        }
    }
}

/// FNV-1a 64-bit. Inline because we don't want a hash-crate dep for the
/// few bytes of work this does. Same scheme as
/// `pocopine_sync::protocol::fnv1a_64`.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> SyncStreamName {
        SyncStreamName::new("issues").unwrap()
    }

    #[test]
    fn empty_query_has_stable_key() {
        let q1: Query<()> = Query::builder(stream()).build();
        let q2: Query<()> = Query::builder(stream()).build();
        assert_eq!(q1.key(), q2.key());
    }

    #[test]
    fn distinct_params_yield_distinct_keys() {
        let q1: Query<()> = Query::builder(stream())
            .raw_param("workspace_id", serde_json::json!("W1"))
            .build();
        let q2: Query<()> = Query::builder(stream())
            .raw_param("workspace_id", serde_json::json!("W2"))
            .build();
        assert_ne!(q1.key(), q2.key());
    }

    #[test]
    fn distinct_streams_yield_distinct_keys() {
        let q1: Query<()> = Query::builder(stream()).build();
        let q2: Query<()> = Query::builder(SyncStreamName::new("posts").unwrap()).build();
        assert_ne!(q1.key(), q2.key());
    }

    #[test]
    fn order_changes_key() {
        let q1: Query<()> = Query::builder(stream())
            .order_by("created_at", Order::Asc)
            .build();
        let q2: Query<()> = Query::builder(stream())
            .order_by("created_at", Order::Desc)
            .build();
        assert_ne!(q1.key(), q2.key());
    }

    #[test]
    fn limit_changes_key() {
        let q1: Query<()> = Query::builder(stream()).limit(50).build();
        let q2: Query<()> = Query::builder(stream()).limit(100).build();
        assert_ne!(q1.key(), q2.key());
    }

    #[test]
    fn query_key_displays_as_16_hex() {
        let q: Query<()> = Query::builder(stream()).build();
        let s = format!("{}", q.key());
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
