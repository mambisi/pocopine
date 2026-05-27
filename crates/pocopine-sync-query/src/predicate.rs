//! Type-level field tokens for the macro-generated query DSL.
//!
//! `#[resource(params(workspace_id: String, status: params::InSet<Status>))]`
//! emits a `field` module with one zero-sized marker per declared
//! param. Each marker implements exactly ONE of the comparator
//! traits below — the one matching its declared shape:
//!
//! | Param declaration                | Marker impls          |
//! |----------------------------------|------------------------|
//! | `workspace_id: WorkspaceId`      | `FieldEq<WorkspaceId>` |
//! | `assignee_id: Option<UserId>`    | `FieldEq<UserId>`      |
//! | `status: params::InSet<Status>`  | `FieldInSet<Status>`   |
//! | `created_at: params::Range<T>`   | `FieldRange<T>`        |
//! | `title: params::Contains`        | `FieldContains`        |
//!
//! The query DSL methods (`.eq`, `.any_of`, `.range`, `.contains`)
//! are generic over the trait, so misuse fails to compile:
//!
//! ```ignore
//! Issues::query().any_of(field::workspace_id, [...])
//! //                     ^^^^^^^^^^^^^^^^^^^^^^^^^^ workspace_id is
//! //                     declared `WorkspaceId` (required eq), not
//! //                     `InSet<...>`, so this errors at compile time.
//! ```
//!
//! All traits are sealed (private supertrait) so app code cannot
//! implement them for its own types and accidentally widen the
//! comparator vocabulary. The cross-crate seal is implemented via an
//! `unsafe` marker bridge — proc-macros emit `unsafe impl` blocks for
//! each generated field marker, which is a strong syntactic signal
//! that downstream code is violating the library's contract if it
//! reaches for the same trait.

mod sealed {
    pub trait Sealed {}
}

/// Field marker that matches a parameter declared as required `T` or
/// `Option<T>` (i.e. equality predicate). `Value` is the declared
/// inner type — for `Option<T>`, `Value = T`; for required `T`,
/// `Value = T`. Defined as an associated type (rather than a trait
/// generic) so the compiler can pin the type from the marker alone
/// and the builder can accept `impl Into<M::Value>` at the call site
/// — i.e. `.eq(field::workspace_id, "W1")` works without
/// `.to_string()`.
pub trait FieldEq: sealed::Sealed {
    /// The declared T for this field.
    type Value;
    /// Wire param key for this field (e.g. `"workspace_id"`).
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::InSet<T>`. `Item` is the element type — used by
/// `.any_of(field::status, iter)` to accept any `impl IntoIterator`
/// of `impl Into<Item>`.
pub trait FieldInSet: sealed::Sealed {
    /// The declared element T for this field.
    type Item;
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::Range<T>`. `Bound` is the per-bound type.
pub trait FieldRange: sealed::Sealed {
    /// The declared bound T for this field.
    type Bound;
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::Contains`.
pub trait FieldContains: sealed::Sealed {
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Sealed-trait gate marker. Macro-generated field markers
/// (`__Field_<name>`) impl this `unsafe trait` so they pass the seal
/// on the public comparator traits above. **Do not impl this directly.**
///
/// In stable Rust, proc-macros emitting code in DOWNSTREAM crates
/// force the seal-supertrait path to be reachable from outside this
/// crate, so a true private seal is impossible — instead we mark this
/// trait `unsafe`. Downstream code that tries to impl it must write
/// `unsafe impl ...`, which is a strong "you are reaching past the
/// API contract" signal that catches mistakes in code review and
/// rustfmt-stable trees. The `#[resource(...)]` macro is the only
/// legitimate impl'er.
///
/// Impl'ing this trait is not a memory-safety hazard (the `unsafe`
/// here is purely API-stability), but smuggling arbitrary types
/// through the query DSL bypasses the comparator-vocabulary check
/// and may cause silent wire corruption against a server that does
/// not recognize the smuggled key.
#[doc(hidden)]
pub unsafe trait __SealedFieldMarker {}

// Bridge `__SealedFieldMarker` impls into the sealed module. The
// macro emits `unsafe impl __SealedFieldMarker for ...` for each
// marker; this blanket impl forwards them to the actually-sealed
// trait. The blanket is safe — only the supertrait's impl-side is
// unsafe — because `Sealed` carries no methods of its own.
impl<T: ?Sized + __SealedFieldMarker> sealed::Sealed for T {}

// ---------------------------------------------------------------------------
// Comparator runtime helpers used by macro-emitted predicate evaluators.
// Kept here (rather than in `params.rs`) because they're called from
// generated code — the path `pocopine_sync_query::predicate::*` is part
// of the macro's contract.
// ---------------------------------------------------------------------------

use crate::params;

/// True when `value` falls inside `range`, honoring inclusivity.
///
/// A `None` bound on either side means "unbounded in that direction". A
/// fully-unbounded range is rejected at deserialization so we don't have
/// to special-case it here.
pub fn range_contains<T>(range: &params::Range<T>, value: &T) -> bool
where
    T: PartialOrd,
{
    let after_lower = match (&range.from, range.inclusive.0) {
        (Some(lo), true) => value >= lo,
        (Some(lo), false) => value > lo,
        (None, _) => true,
    };
    let before_upper = match (&range.to, range.inclusive.1) {
        (Some(hi), true) => value <= hi,
        (Some(hi), false) => value < hi,
        (None, _) => true,
    };
    after_lower && before_upper
}

/// True when `needle.contains` appears in `haystack`, honoring the
/// `case_sensitive` flag.
pub fn contains_matches(needle: &params::Contains, haystack: &str) -> bool {
    if needle.case_sensitive {
        haystack.contains(&needle.contains)
    } else {
        haystack
            .to_ascii_lowercase()
            .contains(&needle.contains.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_contains_closed_inclusive() {
        let r = params::Range::closed(1, 10);
        assert!(range_contains(&r, &1));
        assert!(range_contains(&r, &5));
        assert!(range_contains(&r, &10));
        assert!(!range_contains(&r, &0));
        assert!(!range_contains(&r, &11));
    }

    #[test]
    fn range_contains_half_open() {
        let r = params::Range::half_open(1, 10); // [1, 10)
        assert!(range_contains(&r, &1));
        assert!(range_contains(&r, &9));
        assert!(!range_contains(&r, &10));
    }

    #[test]
    fn range_contains_at_least() {
        let r = params::Range::at_least(5);
        assert!(range_contains(&r, &5));
        assert!(range_contains(&r, &1000));
        assert!(!range_contains(&r, &4));
    }

    #[test]
    fn range_contains_at_most() {
        let r = params::Range::at_most(5);
        assert!(range_contains(&r, &5));
        assert!(range_contains(&r, &-1000));
        assert!(!range_contains(&r, &6));
    }

    #[test]
    fn contains_matches_case_insensitive_by_default() {
        let needle = params::Contains::icontains("auth").unwrap();
        assert!(contains_matches(&needle, "Authentication"));
        assert!(contains_matches(&needle, "user auth flow"));
        assert!(!contains_matches(&needle, "unrelated"));
    }

    #[test]
    fn contains_matches_case_sensitive_when_flagged() {
        let needle = params::Contains::matches("Auth").unwrap();
        assert!(contains_matches(&needle, "Authentication"));
        assert!(!contains_matches(&needle, "authentication"));
    }
}
