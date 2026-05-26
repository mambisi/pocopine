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
//! The query DSL methods (`.where_eq`, `.where_in`, `.where_range`,
//! `.where_contains`) are generic over the trait, so misuse fails to
//! compile:
//!
//! ```ignore
//! Issues::query().where_in(field::workspace_id, [...])
//! //                       ^^^^^^^^^^^^^^^^^^^^^^^^^^ workspace_id is
//! //                       declared `WorkspaceId` (required eq), not
//! //                       `InSet<...>`, so this errors at compile time.
//! ```
//!
//! All traits are sealed (private supertrait) so app code cannot
//! implement them for its own types and accidentally widen the
//! comparator vocabulary.

mod sealed {
    pub trait Sealed {}
}

/// Field marker that matches a parameter declared as required `T` or
/// `Option<T>` (i.e. equality predicate). The `Value` associated type
/// is the inner type — for `Option<T>`, `Value = T`; for required
/// `T`, `Value = T`.
pub trait FieldEq<T>: sealed::Sealed {
    /// Wire param key for this field (e.g. `"workspace_id"`).
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::InSet<T>`.
pub trait FieldInSet<T>: sealed::Sealed {
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::Range<T>`.
pub trait FieldRange<T>: sealed::Sealed {
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Field marker that matches a parameter declared as
/// `params::Contains`.
pub trait FieldContains: sealed::Sealed {
    /// Wire param key for this field.
    const NAME: &'static str;
}

/// Sealed-trait gate macro used by `#[resource(...)]` to register
/// macro-generated field markers as `Sealed`. Authors should NOT use
/// this directly — the `#[resource]` macro emits the necessary impls.
#[doc(hidden)]
pub trait __SealedFieldMarker {}

// Bridge `__SealedFieldMarker` impls into the sealed module. The
// macro emits `impl __SealedFieldMarker for ...` for each marker;
// this blanket impl forwards them to the actually-sealed trait.
impl<T: __SealedFieldMarker> sealed::Sealed for T {}
