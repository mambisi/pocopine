//! Comparator wrapper types for `#[resource(params(...))]` declarations.
//!
//! Authors declare filterable subscription parameters via the macro:
//!
//! ```ignore
//! #[resource(
//!     name = "issues",
//!     params(
//!         workspace_id: WorkspaceId,              // required eq
//!         assignee_id: Option<UserId>,            // optional eq
//!         status: params::InSet<Status>,          // in-set
//!         created_at: params::Range<DateTime>,    // range
//!         title: params::Contains,                // substring match
//!     ),
//! )]
//! impl CrudSource for Issues { ... }
//! ```
//!
//! The macro maps each declared `Type` to one of these comparator
//! kinds at compile time:
//!
//! | Wrapper                  | Semantic              | Wire shape                                                                |
//! |--------------------------|-----------------------|---------------------------------------------------------------------------|
//! | `T` (bare)               | Required equality     | `{ "field": <value> }`                                                    |
//! | `Option<T>`              | Optional equality     | `{ "field": <value> }` (or absent)                                        |
//! | `params::InSet<T>`       | Membership in a set   | `{ "field": { "in": [<v1>, <v2>, ...] } }`                                |
//! | `params::Range<T>`       | Bounded range         | `{ "field": { "from": <lo>, "to": <hi>, "inclusive": [<bool>, <bool>] } }`|
//! | `params::Contains`       | Substring match       | `{ "field": { "contains": <needle>, "case_sensitive": <bool> } }`         |
//!
//! All comparator wrappers are `Serialize + Deserialize` so the same
//! types travel through the wire envelope and the typed `IssuesStreamParams`
//! struct the macro generates.

use serde::{Deserialize, Deserializer, Serialize};

/// Membership predicate: row matches when the declared field's value
/// equals ANY value in this set. The set must be non-empty; empty sets
/// are rejected at deserialization time because an empty `IN ()` is
/// almost always a mistake (selects no rows, but the author probably
/// wanted "no constraint" — they should omit the param entirely).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InSet<T> {
    #[serde(rename = "in")]
    values: Vec<T>,
}

impl<'de, T> Deserialize<'de> for InSet<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Re-run the constructor invariant after deriving the raw shape:
        // an empty `in` list is the bug `InSet::new` rejects, so we
        // must reject it on the wire too — otherwise the doc-promise
        // "empty sets rejected at deserialization time" is a lie and
        // a wire payload `{"in": []}` would silently select no rows.
        #[derive(Deserialize)]
        struct Raw<T> {
            #[serde(rename = "in")]
            values: Vec<T>,
        }
        let raw = Raw::<T>::deserialize(deserializer)?;
        if raw.values.is_empty() {
            return Err(serde::de::Error::custom(EmptyInSetError));
        }
        Ok(Self { values: raw.values })
    }
}

impl<T> InSet<T> {
    /// Build an `InSet` from an iterator of values. Returns `Err` if
    /// the iterator was empty — empty in-sets are rejected because
    /// they always select no rows.
    pub fn new<I>(values: I) -> Result<Self, EmptyInSetError>
    where
        I: IntoIterator<Item = T>,
    {
        let values: Vec<T> = values.into_iter().collect();
        if values.is_empty() {
            return Err(EmptyInSetError);
        }
        Ok(Self { values })
    }

    /// Borrow the set's values in declaration order.
    pub fn values(&self) -> &[T] {
        &self.values
    }

    /// Consume the set and yield its values.
    pub fn into_values(self) -> Vec<T> {
        self.values
    }
}

/// Returned by `InSet::new` when the input iterator was empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyInSetError;

impl core::fmt::Display for EmptyInSetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("InSet must contain at least one value (empty sets select no rows; omit the param to leave it unconstrained)")
    }
}

impl std::error::Error for EmptyInSetError {}

/// Bounded range predicate: row matches when the declared field's
/// value falls within `[from, to]` with inclusivity per the
/// `inclusive` tuple. Either bound may be `None` for half-open
/// ranges. A range with BOTH bounds `None` is rejected at
/// deserialization time — it always selects every row and the
/// author probably wanted to omit the param entirely.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Range<T> {
    /// Lower bound; `None` means unbounded below.
    pub from: Option<T>,
    /// Upper bound; `None` means unbounded above.
    pub to: Option<T>,
    /// Inclusivity of `(from, to)` — `(true, false)` is `[from, to)`,
    /// `(true, true)` is `[from, to]`, etc. Defaults to `(true, true)`
    /// when constructed via the helper constructors.
    #[serde(default = "default_inclusive")]
    pub inclusive: (bool, bool),
}

impl<'de, T> Deserialize<'de> for Range<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Re-run the constructor invariant: a fully-unbounded range
        // (both `from` and `to` None) matches every row and is
        // always a mistake. The doc promise rejects it; on the wire,
        // it would silently disable the filter.
        #[derive(Deserialize)]
        struct Raw<T> {
            #[serde(default = "Option::default")]
            from: Option<T>,
            #[serde(default = "Option::default")]
            to: Option<T>,
            #[serde(default = "default_inclusive")]
            inclusive: (bool, bool),
        }
        let raw = Raw::<T>::deserialize(deserializer)?;
        if raw.from.is_none() && raw.to.is_none() {
            return Err(serde::de::Error::custom(
                "Range must specify at least one of `from` or `to` (a fully-unbounded range matches every row; omit the param to leave it unconstrained)",
            ));
        }
        Ok(Self {
            from: raw.from,
            to: raw.to,
            inclusive: raw.inclusive,
        })
    }
}

fn default_inclusive() -> (bool, bool) {
    (true, true)
}

impl<T> Range<T> {
    /// Closed range `[from, to]`. Errors if both bounds would be None
    /// (would always match; omit the param instead).
    pub fn closed(from: T, to: T) -> Self {
        Self {
            from: Some(from),
            to: Some(to),
            inclusive: (true, true),
        }
    }

    /// Half-open range `[from, to)`.
    pub fn half_open(from: T, to: T) -> Self {
        Self {
            from: Some(from),
            to: Some(to),
            inclusive: (true, false),
        }
    }

    /// Lower-bounded range `[from, ∞)`.
    pub fn at_least(from: T) -> Self {
        Self {
            from: Some(from),
            to: None,
            inclusive: (true, false),
        }
    }

    /// Upper-bounded range `(-∞, to]`.
    pub fn at_most(to: T) -> Self {
        Self {
            from: None,
            to: Some(to),
            inclusive: (false, true),
        }
    }

    /// Strict lower-bounded range `(from, ∞)`.
    pub fn greater_than(from: T) -> Self {
        Self {
            from: Some(from),
            to: None,
            inclusive: (false, false),
        }
    }

    /// Strict upper-bounded range `(-∞, to)`.
    pub fn less_than(to: T) -> Self {
        Self {
            from: None,
            to: Some(to),
            inclusive: (false, false),
        }
    }

    /// True when both bounds are `None`. The framework rejects such
    /// ranges at deserialization time; this is exposed for source
    /// authors who need to validate manually.
    pub fn is_unbounded(&self) -> bool {
        self.from.is_none() && self.to.is_none()
    }
}

// Conversions from the standard library's range syntax so callers
// can write `.range(field::priority, 2..5)` instead of
// `params::Range::half_open(2, 5)`. The builder accepts
// `impl Into<params::Range<M::Bound>>`, so every From impl below is
// directly usable at the call site.
//
// `core::ops::RangeFull` (`..`) is intentionally NOT supported —
// a fully-unbounded range matches every row, which is the
// deserialize invariant the framework rejects. Callers wanting
// "no constraint" should omit the param entirely.

/// `a..b` — half-open `[a, b)`.
impl<T> From<core::ops::Range<T>> for Range<T> {
    fn from(r: core::ops::Range<T>) -> Self {
        Self::half_open(r.start, r.end)
    }
}

/// `a..=b` — closed `[a, b]`.
impl<T> From<core::ops::RangeInclusive<T>> for Range<T> {
    fn from(r: core::ops::RangeInclusive<T>) -> Self {
        let (start, end) = r.into_inner();
        Self::closed(start, end)
    }
}

/// `a..` — lower-bounded `[a, ∞)`.
impl<T> From<core::ops::RangeFrom<T>> for Range<T> {
    fn from(r: core::ops::RangeFrom<T>) -> Self {
        Self::at_least(r.start)
    }
}

/// `..b` — strict upper-bounded `(-∞, b)`.
impl<T> From<core::ops::RangeTo<T>> for Range<T> {
    fn from(r: core::ops::RangeTo<T>) -> Self {
        Self::less_than(r.end)
    }
}

/// `..=b` — inclusive upper-bounded `(-∞, b]`.
impl<T> From<core::ops::RangeToInclusive<T>> for Range<T> {
    fn from(r: core::ops::RangeToInclusive<T>) -> Self {
        Self::at_most(r.end)
    }
}

/// Substring-match predicate: row matches when the declared text
/// field contains `needle`. The match is case-insensitive by
/// default; flip `case_sensitive` for exact-case matching. Empty
/// needles are rejected (would always match).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Contains {
    /// Substring to search for. Cannot be empty.
    pub contains: String,
    /// When `true`, match exact case; when `false`, fold both sides
    /// to lowercase before comparing. Defaults to `false`.
    #[serde(default)]
    pub case_sensitive: bool,
}

impl<'de> Deserialize<'de> for Contains {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Re-run the constructor invariant: an empty needle matches
        // every row. The doc promise rejects it; on the wire it would
        // silently disable the filter.
        #[derive(Deserialize)]
        struct Raw {
            contains: String,
            #[serde(default)]
            case_sensitive: bool,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.contains.is_empty() {
            return Err(serde::de::Error::custom(EmptyContainsError));
        }
        Ok(Self {
            contains: raw.contains,
            case_sensitive: raw.case_sensitive,
        })
    }
}

impl Contains {
    /// Case-insensitive substring match.
    pub fn icontains(needle: impl Into<String>) -> Result<Self, EmptyContainsError> {
        Self::build(needle, false)
    }

    /// Case-sensitive substring match.
    pub fn matches(needle: impl Into<String>) -> Result<Self, EmptyContainsError> {
        Self::build(needle, true)
    }

    fn build(needle: impl Into<String>, case_sensitive: bool) -> Result<Self, EmptyContainsError> {
        let contains = needle.into();
        if contains.is_empty() {
            return Err(EmptyContainsError);
        }
        Ok(Self {
            contains,
            case_sensitive,
        })
    }
}

/// Returned by `Contains::contains` / `icontains` when the needle
/// is empty.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyContainsError;

impl core::fmt::Display for EmptyContainsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Contains needle must be non-empty (empty matches every row; omit the param to leave it unconstrained)")
    }
}

impl std::error::Error for EmptyContainsError {}

// Tests rely on `serde_json` which is a host-only dep; the wasm
// build of pocopine-sync-crud doesn't pull it in (the wasm bundle
// would be larger for no benefit — wasm consumers serialize via
// the framework's existing serde paths).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn in_set_rejects_empty_iterator() {
        let result: Result<InSet<u32>, _> = InSet::new(Vec::<u32>::new());
        assert!(result.is_err());
    }

    #[test]
    fn in_set_round_trips_through_serde() {
        let set = InSet::new(["open", "in_progress"]).unwrap();
        let json = serde_json::to_value(&set).unwrap();
        // Transparent representation: serializes as the inner struct.
        assert_eq!(json["in"], serde_json::json!(["open", "in_progress"]));
        let decoded: InSet<String> = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.values(), &["open", "in_progress"]);
    }

    #[test]
    fn range_helpers_set_inclusivity() {
        let closed = Range::closed(1, 10);
        assert_eq!(closed.inclusive, (true, true));
        let half_open = Range::half_open(1, 10);
        assert_eq!(half_open.inclusive, (true, false));
        let at_least = Range::at_least(1);
        assert_eq!(at_least.from, Some(1));
        assert_eq!(at_least.to, None);
    }

    #[test]
    fn range_from_std_ops_range_is_half_open() {
        let r: Range<i32> = (1..10).into();
        assert_eq!(r.from, Some(1));
        assert_eq!(r.to, Some(10));
        assert_eq!(r.inclusive, (true, false));
    }

    #[test]
    fn range_from_std_ops_range_inclusive_is_closed() {
        let r: Range<i32> = (1..=10).into();
        assert_eq!(r.from, Some(1));
        assert_eq!(r.to, Some(10));
        assert_eq!(r.inclusive, (true, true));
    }

    #[test]
    fn range_from_std_ops_range_from_is_at_least() {
        let r: Range<i32> = (5..).into();
        assert_eq!(r.from, Some(5));
        assert_eq!(r.to, None);
        assert_eq!(r.inclusive, (true, false));
    }

    #[test]
    fn range_from_std_ops_range_to_is_less_than() {
        let r: Range<i32> = (..5).into();
        assert_eq!(r.from, None);
        assert_eq!(r.to, Some(5));
        assert_eq!(r.inclusive, (false, false));
    }

    #[test]
    fn range_from_std_ops_range_to_inclusive_is_at_most() {
        let r: Range<i32> = (..=5).into();
        assert_eq!(r.from, None);
        assert_eq!(r.to, Some(5));
        assert_eq!(r.inclusive, (false, true));
    }

    #[test]
    fn range_round_trips_with_default_inclusivity() {
        // Old wire payloads without `inclusive` deserialize as
        // `(true, true)` per the `#[serde(default)]`.
        let json = serde_json::json!({
            "from": 1,
            "to": 10,
        });
        let range: Range<i32> = serde_json::from_value(json).unwrap();
        assert_eq!(range.from, Some(1));
        assert_eq!(range.to, Some(10));
        assert_eq!(range.inclusive, (true, true));
    }

    #[test]
    fn contains_rejects_empty_needle() {
        assert!(Contains::icontains("").is_err());
        assert!(Contains::matches("").is_err());
    }

    #[test]
    fn in_set_rejects_empty_in_array_on_wire() {
        // The constructor invariant must hold on the wire too —
        // otherwise a malicious / typo'd client could send
        // `{"in": []}` and silently select no rows from a filter
        // that the source assumes always has at least one value.
        let json = serde_json::json!({"in": []});
        let err = serde_json::from_value::<InSet<String>>(json).unwrap_err();
        assert!(err.to_string().contains("InSet"));
    }

    #[test]
    fn range_rejects_fully_unbounded_on_wire() {
        // A fully-unbounded range matches every row; on the wire
        // this is identical to "no filter", which is the same as
        // omitting the param. Reject so the doc promise holds.
        let json = serde_json::json!({"from": null, "to": null});
        let err = serde_json::from_value::<Range<i32>>(json).unwrap_err();
        assert!(err.to_string().contains("Range must specify"));
    }

    #[test]
    fn contains_rejects_empty_needle_on_wire() {
        let json = serde_json::json!({"contains": ""});
        let err = serde_json::from_value::<Contains>(json).unwrap_err();
        assert!(err.to_string().contains("Contains"));
    }

    #[test]
    fn contains_defaults_case_sensitive_false() {
        // Old wire payloads without `case_sensitive` default to false.
        let json = serde_json::json!({
            "contains": "alice",
        });
        let needle: Contains = serde_json::from_value(json).unwrap();
        assert_eq!(needle.contains, "alice");
        assert!(!needle.case_sensitive);
    }
}
