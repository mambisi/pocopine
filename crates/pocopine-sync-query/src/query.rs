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
    /// Function pointer to the macro-generated partition projector.
    /// Returns `Some(hash)` when the captured params project cleanly
    /// onto the resource's canonical partition keys (RFC 088 §C);
    /// `None` when a required field is missing or wraps a comparator
    /// (`any_of`, `range`, …). Driver uses this to decide whether to
    /// subscribe to the per-`(stream, params_hash)` live topic.
    pub(crate) partition_hash_fn: Option<PartitionHashFn>,
    pub(crate) _row: PhantomData<fn() -> Row>,
}

/// Signature of a query's predicate evaluator. The macro emits one
/// `fn(&Query<Row>, &Row) -> bool` per declared `#[query_resource]`
/// and the builder injects it via [`QueryBuilder::with_matches`].
pub type MatchFn<Row> = fn(query: &Query<Row>, row: &Row) -> bool;

/// Signature of a query's partition-hash projector. Macro-emitted;
/// reads from the caller's captured params and produces the hash
/// that matches what the server publishes against the row (RFC 088
/// §C). Independent of `Row` because the projection operates on the
/// already-typed params map.
pub type PartitionHashFn = fn(captured: &StreamParams) -> Option<u64>;

impl<Row> Query<Row> {
    /// Build a query from its parts. Most callers go through the macro-
    /// generated `Resource::query()` builder, which constructs the params
    /// map via the typed DSL (`.eq` / `.any_of` / `.range` / `.contains`)
    /// and injects the macro-generated predicate evaluator via
    /// [`QueryBuilder::with_matches`].
    pub fn builder(stream: SyncStreamName) -> QueryBuilder<Row> {
        QueryBuilder {
            stream,
            params: StreamParams::new(),
            order_by: None,
            limit: None,
            matches_fn: None,
            partition_hash_fn: None,
            _row: PhantomData,
        }
    }

    /// Compute the RFC 088 §C partition hash for this query's
    /// captured params, if the macro registered a projector AND the
    /// projection succeeds (all required fields present as plain
    /// values, no comparator wrappers).
    ///
    /// `None` means the client should NOT subscribe to a per-params
    /// topic for this query — fall back to the bare-topic
    /// subscription (which the routing engine's params filter still
    /// gates correctly).
    pub fn partition_hash(&self) -> Option<u64> {
        self.partition_hash_fn.and_then(|f| f(&self.params))
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
            partition_hash_fn: self.partition_hash_fn,
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
/// `Resource::query()` helper. Provides the trait-gated query DSL
/// (`.eq` / `.any_of` / `.range` / `.contains` / `.contains_exact`)
/// plus `.order_by` and `.limit` setters; finalize with `.build()`
/// or subscribe directly with `.observe(&client)`.
///
/// The DSL methods take a field marker (e.g. `field::workspace_id`)
/// as their first argument and route through the sealed comparator
/// traits in [`crate::predicate`]. Misuse — passing a field marker
/// whose declared comparator doesn't match the method called —
/// fails at compile time, not at the wire boundary.
#[derive(Debug)]
pub struct QueryBuilder<Row> {
    stream: SyncStreamName,
    params: StreamParams,
    order_by: Option<OrderBy>,
    limit: Option<u32>,
    matches_fn: Option<MatchFn<Row>>,
    partition_hash_fn: Option<PartitionHashFn>,
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
            partition_hash_fn: self.partition_hash_fn,
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

    /// Inject the macro-generated partition-hash projector (RFC 088
    /// §C). Called by the macro's `Resource::query()` so the client
    /// driver can compute the per-`(stream, params_hash)` topic
    /// matching the server's `row_to_params` projection.
    pub fn with_partition_hash(mut self, partition_hash_fn: PartitionHashFn) -> Self {
        self.partition_hash_fn = Some(partition_hash_fn);
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
            partition_hash_fn: self.partition_hash_fn,
            _row: PhantomData,
        }
    }

    /// Equality predicate. Adds `(field, value)` to the param map iff
    /// `field` is a marker that implements [`crate::FieldEq`]. The
    /// `value` argument is `impl Into<M::Value>` so the caller can
    /// pass anything convertible to the declared field type — e.g.
    /// `&str` where the field was declared as `String`. The sealed
    /// associated-type trait gates compile-time correctness:
    /// misnaming a field or passing a value that doesn't convert to
    /// the declared param's type fails at compile time, not at the
    /// wire boundary.
    ///
    /// ```ignore
    /// Issues::query()
    ///     // &str → String via the standard library's Into impl
    ///     .eq(field::workspace_id, "W1")
    ///     // field::status is FieldInSet, not FieldEq —
    ///     // .eq(field::status, ...) fails to compile.
    ///     .build();
    /// ```
    pub fn eq<M, V>(self, _field: M, value: V) -> Self
    where
        M: crate::FieldEq,
        V: Into<M::Value>,
        M::Value: serde::Serialize,
    {
        let value: M::Value = value.into();
        let encoded = serde_json::to_value(&value)
            .expect("FieldEq value must be JSON-serializable — contract violation");
        self.raw_param(<M as crate::FieldEq>::NAME, encoded)
    }

    /// Set-membership predicate. Matches rows whose `field` is any
    /// of `values`. Adds `(field, InSet<M::Item>)` to the param map
    /// iff `field` implements [`crate::FieldInSet`]. Iterator items
    /// are `impl Into<M::Item>` so `.any_of(field::status, ["open",
    /// "closed"])` works when `Status` has `From<&str>`. Returns an
    /// error if `values` is empty (an empty set matches no rows;
    /// the caller likely meant to omit the param entirely).
    ///
    /// Reads naturally on the call site as "status is any of these":
    ///
    /// ```ignore
    /// Issues::query()
    ///     .any_of(field::status, [Status::Open, Status::InProgress])?
    ///     .build();
    /// ```
    pub fn any_of<M, V, I>(self, _field: M, values: I) -> pocopine_sync::SyncResult<Self>
    where
        M: crate::FieldInSet,
        M::Item: serde::Serialize,
        V: Into<M::Item>,
        I: IntoIterator<Item = V>,
    {
        let set = crate::params::InSet::<M::Item>::new(values.into_iter().map(Into::into))
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        let encoded = serde_json::to_value(&set)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        Ok(self.raw_param(<M as crate::FieldInSet>::NAME, encoded))
    }

    /// Range predicate. Adds `(field, Range<M::Bound>)` to the param
    /// map iff `field` implements [`crate::FieldRange`]. Accepts
    /// either a pre-built [`crate::params::Range`] or any of the
    /// standard library's range expressions:
    ///
    /// | Rust syntax | Wire shape                       |
    /// |-------------|----------------------------------|
    /// | `a..b`      | half-open `[a, b)`               |
    /// | `a..=b`     | closed `[a, b]`                  |
    /// | `a..`       | lower-bounded `[a, ∞)`           |
    /// | `..b`       | strict upper-bounded `(-∞, b)`   |
    /// | `..=b`      | inclusive upper-bounded `(-∞, b]`|
    ///
    /// `..` (`RangeFull`) is intentionally not supported — a
    /// fully-unbounded range matches every row; omit the param.
    ///
    /// ```ignore
    /// Issues::query()
    ///     .range(field::priority, 2..5)              // a..b
    ///     .range(field::priority, 2..=5)             // a..=b
    ///     .range(field::priority, params::Range::greater_than(0))
    ///     .build();
    /// ```
    pub fn range<M, R>(self, _field: M, range: R) -> Self
    where
        M: crate::FieldRange,
        M::Bound: serde::Serialize,
        R: Into<crate::params::Range<M::Bound>>,
    {
        let range: crate::params::Range<M::Bound> = range.into();
        let encoded = serde_json::to_value(&range)
            .expect("Range<T> is always JSON-serializable when T: Serialize");
        self.raw_param(<M as crate::FieldRange>::NAME, encoded)
    }

    /// Substring-match predicate (case-insensitive). Adds
    /// `(field, Contains { contains: needle, case_sensitive: false })`
    /// to the param map iff `field` implements [`crate::FieldContains`].
    /// Returns an error on empty needle (empty matches every row —
    /// likely a misuse; omit the param instead).
    ///
    /// For case-sensitive matching, use [`Self::contains_exact`].
    ///
    /// ```ignore
    /// Issues::query()
    ///     .contains(field::title, "google")?
    ///     .build();
    /// ```
    pub fn contains<M, S>(self, _field: M, needle: S) -> pocopine_sync::SyncResult<Self>
    where
        M: crate::FieldContains,
        S: Into<String>,
    {
        let contains = crate::params::Contains::icontains(needle)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        let encoded =
            serde_json::to_value(&contains).expect("Contains is always JSON-serializable");
        Ok(self.raw_param(<M as crate::FieldContains>::NAME, encoded))
    }

    /// Substring-match predicate (case-sensitive). Like
    /// [`Self::contains`] but enforces exact-case matching.
    pub fn contains_exact<M, S>(self, _field: M, needle: S) -> pocopine_sync::SyncResult<Self>
    where
        M: crate::FieldContains,
        S: Into<String>,
    {
        let contains = crate::params::Contains::matches(needle)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        let encoded =
            serde_json::to_value(&contains).expect("Contains is always JSON-serializable");
        Ok(self.raw_param(<M as crate::FieldContains>::NAME, encoded))
    }

    /// Subscribe to this query through `client` and return a reactive
    /// view. Sugar for `client.observe(self.build())`.
    pub fn observe(self, client: &crate::QueryClient) -> crate::QueryView<Row>
    where
        Row: Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        client.observe(self.build())
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
