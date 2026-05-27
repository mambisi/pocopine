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
//! comparator vocabulary. The cross-crate seal is implemented via an
//! `unsafe` marker bridge — proc-macros emit `unsafe impl` blocks for
//! each generated field marker, which is a strong syntactic signal
//! that downstream code is violating the library's contract if it
//! reaches for the same trait.

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
