//! Proc-macros for `pocopine-sync-query`.
//!
//! The headline macro is `#[query_resource]`. It attaches to the row
//! struct that names a queryable resource and emits:
//!
//! * A `Row::query()` builder yielding `pocopine_sync_query::QueryBuilder<Row>`.
//! * Per-field zero-sized markers + sealed comparator-trait impls.
//! * A predicate evaluator: `pub fn matches(query, row) -> bool` (the
//!   builder injects this as the query's `matches_fn`).
//! * Constants: `NAME`, `SCHEMA_VERSION`.
//!
//! Queryable fields are declared in-line on the struct with
//! `#[query_param]`. Every annotated field automatically supports
//! `.eq()` and `.any_of()` at the call site (both apply to any T).
//! `FieldRange` is auto-emitted when the inner type's last path
//! segment is a known orderable (numeric primitives, `String`,
//! common `DateTime`-y types); `FieldContains` is auto-emitted when
//! it's `String` / `str` / `Cow`. The type-name heuristic can be
//! overridden with explicit keywords:
//!
//! * `#[query_param(required)]` — predicate fails if the query has
//!   no value for this field. Use for tenant-gate fields
//!   (`workspace_id`, `tenant_id`) to prevent cross-tenant leaks.
//! * `#[query_param(range)]` — force `FieldRange` emission when the
//!   heuristic missed it (newtypes around numerics / DateTimes).
//! * `#[query_param(contains)]` — force `FieldContains` emission for
//!   newtypes around `String`.
//!
//! Multiple keywords combine: `#[query_param(required, range)]`.
//!
//! The macro strips its own attrs from the emitted struct so
//! downstream derives (`Serialize`, `Deserialize`, …) see a clean shape.
//!
//! **Attribute order matters.** `#[query_resource]` must come BEFORE
//! `#[derive(...)]` so it processes the struct first and strips the
//! per-field annotations.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, Attribute, Fields, FnArg, GenericArgument, Ident, ItemFn, ItemStruct,
    LitInt, LitStr, Meta, Pat, PatIdent, PathArguments, PathSegment, ReturnType, Token, Type,
};

const MAX_SYNC_TOKEN_LEN: usize = 1024;

/// Which comparator traits to emit for a `#[query_param]`-annotated
/// field. `FieldEq` + `FieldInSet` are always emitted (they're
/// universally applicable to any T); `range` and `contains` are gated
/// by the inner-type heuristic OR an explicit opt-in
/// (`#[query_param(range)]` / `#[query_param(contains)]`).
#[derive(Clone, Debug, Default)]
struct FieldCapabilities {
    /// True when the field type is `Option<T>` — gates the
    /// "param absent = no constraint" and "param null = matches None"
    /// branches in the matches body.
    optional: bool,
    /// True when the user opted in with `#[query_param(required)]`.
    /// A required field's predicate FAILS when the query has no
    /// param for it — used for tenant-gate fields (`workspace_id`,
    /// `tenant_id`, …) to prevent accidental cross-tenant leaks from
    /// queries built without the required filter. Default is false
    /// (queryable but not required).
    required: bool,
    /// Emit `FieldRange` + a Range branch in matches().
    range: bool,
    /// Emit `FieldContains` + a Contains branch in matches().
    contains: bool,
}

/// One field declared with `#[query_param]`. `inner_ty` is `T`
/// (unwrapped from any outer `Option`). The macro uses `inner_ty` for
/// every comparator's `Value` / `Item` / `Bound` projection so a
/// `pub assignee_id: Option<String>` field gets `FieldEq<Value = String>`.
#[derive(Clone, Debug)]
struct ParamDef {
    name: Ident,
    inner_ty: Type,
    caps: FieldCapabilities,
}

#[derive(Debug)]
struct QueryResourceArgs {
    name: LitStr,
    schema_version: u32,
}

impl Parse for QueryResourceArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut schema_version: Option<u32> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            if key == "name" {
                if name.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `name`"));
                }
                input.parse::<Token![=]>()?;
                name = Some(input.parse()?);
            } else if key == "schema_version" {
                if schema_version.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `schema_version`"));
                }
                input.parse::<Token![=]>()?;
                let lit: LitInt = input.parse().map_err(|err| {
                    syn::Error::new(
                        err.span(),
                        "`schema_version` must be a u32 integer literal (e.g. `schema_version = 2`)",
                    )
                })?;
                let suffix = lit.suffix();
                if !suffix.is_empty() && suffix != "u32" {
                    return Err(syn::Error::new(
                        lit.span(),
                        format!("`schema_version` must be a bare integer literal or `u32`-suffixed (got: {lit})"),
                    ));
                }
                let value: u32 = lit.base10_parse().map_err(|err| {
                    syn::Error::new(
                        lit.span(),
                        format!("`schema_version` must fit in u32: {err}"),
                    )
                })?;
                if value == 0 {
                    return Err(syn::Error::new(
                        lit.span(),
                        "schema_version must be >= 1 (schema versions start at 1)",
                    ));
                }
                schema_version = Some(value);
            } else if key == "row" {
                return Err(syn::Error::new(
                    key.span(),
                    "`row = Type` is no longer supported — `#[query_resource]` now decorates the row struct itself; the row type is the decorated struct",
                ));
            } else if key == "params" {
                return Err(syn::Error::new(
                    key.span(),
                    "`params(...)` is no longer supported — annotate each queryable field with `#[query_param]` / `#[query_param(any_of|range|contains)]`",
                ));
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `name = \"...\"` or `schema_version = N`",
                ));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else if !input.is_empty() {
                return Err(input.error("expected `,` between query_resource attribute options"));
            }
        }

        let name = name.ok_or_else(|| input.error("expected `name = \"...\"`"))?;
        validate_resource_name(&name)?;
        let schema_version = schema_version.unwrap_or(1);

        Ok(Self {
            name,
            schema_version,
        })
    }
}

/// Capability keyword in `#[query_param(...)]`. `eq` and `any_of` are
/// always on for an annotated field; the explicit keywords are for:
///
/// * `required` — make the field a tenant gate (predicate fails if
///   the query lacks this param).
/// * `range` — force `FieldRange` emission when the type-name
///   heuristic missed it (e.g. a `WorkspaceId(u32)` newtype).
/// * `contains` — force `FieldContains` emission for the same reason
///   (e.g. a `Slug(String)` newtype).
#[derive(Clone, Copy, Debug)]
enum CapabilityKeyword {
    Required,
    Range,
    Contains,
}

/// Walk a struct's named fields, collect a `ParamDef` for every field
/// that carries `#[query_param(...)]`. `range` / `contains` are
/// auto-detected from the inner type's name (numeric / DateTime-y →
/// range; `String` / `str` / `Cow` → contains). Anything the heuristic
/// misses can be opted in explicitly.
fn extract_field_params(item: &ItemStruct) -> syn::Result<Vec<ParamDef>> {
    // Struct-level `#[serde(rename_all = "...")]` silently changes
    // every field's wire key. The macro keys row_to_params and
    // partition_for_topic on the Rust ident, so a struct-level
    // rename_all would produce a server-side hash on the renamed
    // keys (because `from_value` deserializes from them) but the
    // macro's `params.insert(<rust_ident>, ...)` writes the Rust
    // names — net effect: client and server both use Rust idents
    // (an accidentally-correct outcome), but third-party clients
    // following the wire spec would use the renamed keys and miss
    // every wakeup. Reject up front.
    for serde_attr in item.attrs.iter().filter(|a| a.path().is_ident("serde")) {
        let meta_tokens = match &serde_attr.meta {
            Meta::List(list) => list.tokens.to_string(),
            _ => continue,
        };
        if meta_tokens.contains("rename") {
            return Err(syn::Error::new(
                serde_attr.span(),
                "`#[serde(rename...)]` on a `#[query_resource]` struct is not supported. The \
                 macro keys both the predicate evaluator and the RFC 088 §C topic-hash projection \
                 on the Rust field names (with `r#` stripped), so a serde rename would diverge \
                 from the wire spec — third-party clients implementing the spec would hash \
                 different bytes and miss every per-params wakeup. Remove the rename, or open an \
                 issue if you need serde-rename-aware projection.",
            ));
        }
    }

    let fields = match &item.fields {
        Fields::Named(named) => named,
        Fields::Unit => return Ok(Vec::new()),
        Fields::Unnamed(_) => {
            return Err(syn::Error::new(
                item.ident.span(),
                "#[query_resource] requires a struct with named fields (tuple structs and unit structs cannot carry `#[query_param]` annotations)",
            ));
        }
    };

    let mut params: Vec<ParamDef> = Vec::new();
    for field in &fields.named {
        let Some(field_name) = field.ident.clone() else {
            continue;
        };

        let query_param_attrs: Vec<&Attribute> = field
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("query_param"))
            .collect();
        if query_param_attrs.len() > 1 {
            return Err(syn::Error::new(
                query_param_attrs[1].span(),
                format!(
                    "duplicate `#[query_param]` on field `{field_name}` — at most one per field"
                ),
            ));
        }
        let Some(attr) = query_param_attrs.first() else {
            continue;
        };

        // RFC 088 §C: reject `#[serde(rename)]` / `#[serde(rename_all)]`
        // shaped attrs on `#[query_param]` fields. The macro
        // currently keys both the predicate evaluator (`matches()`)
        // and the topic-hash projection (`row_to_params` /
        // `partition_for_topic`) on the raw Rust ident (with the
        // `r#` prefix stripped), NOT on the serde wire key. A field
        // with a serde rename would silently produce a different
        // wire-shape than the projector expects — third-party
        // language clients implementing the golden-fixture spec
        // would hash a different value and miss every per-params
        // wakeup. Rejecting at compile time turns a silent runtime
        // routing-precision bug into a clear error.
        //
        // Detection is structural: any `#[serde(rename ...)]` or
        // `#[serde(rename_all ...)]` on the same field is fatal.
        // We don't try to parse serde's full attribute grammar —
        // just look for the keyword in the meta list's tokens. A
        // future enhancement could thread the rename through the
        // macro's wire-key derivation; until then, reject.
        for serde_attr in field.attrs.iter().filter(|a| a.path().is_ident("serde")) {
            let meta_tokens = match &serde_attr.meta {
                Meta::List(list) => list.tokens.to_string(),
                _ => continue,
            };
            if meta_tokens.contains("rename") {
                return Err(syn::Error::new(
                    serde_attr.span(),
                    format!(
                        "field `{field_name}`: `#[serde(rename...)]` on a `#[query_param]` field \
                         is not supported. The `#[query_resource]` macro keys both the predicate \
                         evaluator and the RFC 088 §C topic-hash projection on the Rust field \
                         name (with `r#` stripped), NOT the serde-renamed wire key — a third-party \
                         client implementing the wire spec would hash a different value and miss \
                         every per-params wakeup. Remove the rename or omit the `#[query_param]`."
                    ),
                ));
            }
        }

        let extras = parse_query_param_extras(attr)?;
        let (inner_ty, is_option) = unwrap_option_type(&field.ty);

        let auto_range = !is_option && is_ordered_type(&inner_ty);
        let auto_contains = !is_option && is_string_type(&inner_ty);

        let mut caps = FieldCapabilities {
            optional: is_option,
            required: false,
            range: auto_range,
            contains: auto_contains,
        };

        for extra in extras {
            match extra {
                CapabilityKeyword::Required => {
                    if is_option {
                        return Err(syn::Error::new(
                            attr.span(),
                            "`(required)` on `Option<T>` is contradictory — an Option field is intrinsically nullable; drop the keyword or change the type to `T`",
                        ));
                    }
                    caps.required = true;
                }
                CapabilityKeyword::Range => {
                    if is_option {
                        return Err(syn::Error::new(
                            attr.span(),
                            "explicit `(range)` on `Option<T>` is not supported — declare the field as `T`",
                        ));
                    }
                    caps.range = true;
                }
                CapabilityKeyword::Contains => {
                    if is_option {
                        return Err(syn::Error::new(
                            attr.span(),
                            "explicit `(contains)` on `Option<T>` is not supported — declare the field as `T`",
                        ));
                    }
                    caps.contains = true;
                }
            }
        }

        params.push(ParamDef {
            name: field_name,
            inner_ty,
            caps,
        });
    }

    Ok(params)
}

/// Parse the keyword list inside `#[query_param(...)]`. Returns the
/// explicit opt-in capabilities. Bare `#[query_param]` returns an
/// empty list — eq/any_of are still emitted (they're universal);
/// range/contains fall back to the type-name heuristic.
fn parse_query_param_extras(attr: &Attribute) -> syn::Result<Vec<CapabilityKeyword>> {
    match &attr.meta {
        Meta::Path(_) => Ok(Vec::new()),
        Meta::List(list) => {
            let parsed: syn::punctuated::Punctuated<Ident, Token![,]> =
                list.parse_args_with(syn::punctuated::Punctuated::parse_terminated)
                    .map_err(|_| {
                        syn::Error::new(
                            attr.span(),
                            "expected a comma-separated list of capability keywords inside `#[query_param(...)]` — `required`, `range`, and/or `contains` (eq + any_of are automatic)",
                        )
                    })?;
            let mut extras = Vec::new();
            for ident in parsed {
                match ident.to_string().as_str() {
                    "required" => extras.push(CapabilityKeyword::Required),
                    "range" => extras.push(CapabilityKeyword::Range),
                    "contains" => extras.push(CapabilityKeyword::Contains),
                    "eq" | "any_of" => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "`{ident}` is now automatic for every `#[query_param]` field — drop this keyword and use bare `#[query_param]`"
                            ),
                        ));
                    }
                    other => {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "unknown capability `{other}` — expected `required`, `range`, or `contains`"
                            ),
                        ));
                    }
                }
            }
            Ok(extras)
        }
        Meta::NameValue(_) => Err(syn::Error::new(
            attr.span(),
            "expected `#[query_param]` or `#[query_param(required|range|contains|...)]`, not `key = value`",
        )),
    }
}

/// True when the inner type's last path segment is a known orderable
/// stdlib primitive. Used to auto-enable `FieldRange` on a bare
/// `#[query_param]` field.
///
/// The heuristic is intentionally narrow: only types whose last path
/// segment cannot collide with a user-defined ident. Earlier
/// versions included `DateTime`, `Date`, `Time`, `Instant`,
/// `Duration`, `Timestamp`, `Zoned` etc. — but `last_segment_ident`
/// matches by the trailing ident alone, so a user-defined
/// `struct Date(u32)` (no `PartialOrd`) hits the heuristic, the
/// macro auto-emits `FieldRange`, and the generated matches body
/// fails to compile with an error pointing into macro output.
/// Authors who want range on DateTime-y types opt in explicitly:
/// `#[query_param(range)]`.
fn is_ordered_type(ty: &Type) -> bool {
    const ORDERED: &[&str] = &[
        // Integers — stdlib idents; safe to auto-enable.
        "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
        // Floats (NaN aside; `PartialOrd` is enough for the macro).
        "f32", "f64",
        // Strings — stdlib idents; lexicographic ordering is what
        // most users expect on String range filters.
        "String", "str",
    ];
    last_segment_ident(ty)
        .map(|i| ORDERED.contains(&i.to_string().as_str()))
        .unwrap_or(false)
}

/// True when the inner type's last path segment is a known stdlib
/// string-y type. Same narrowing rationale as `is_ordered_type` —
/// `Cow` is excluded (a common newtype name) so users opt in
/// explicitly on non-`String` types with `#[query_param(contains)]`.
fn is_string_type(ty: &Type) -> bool {
    const STRINGS: &[&str] = &["String", "str"];
    last_segment_ident(ty)
        .map(|i| STRINGS.contains(&i.to_string().as_str()))
        .unwrap_or(false)
}

fn last_segment_ident(ty: &Type) -> Option<&Ident> {
    if let Type::Path(tp) = ty {
        tp.path.segments.last().map(|s| &s.ident)
    } else {
        None
    }
}

/// Wire param key for a field ident. Strips the `r#` raw-identifier
/// prefix because serde's row serialization emits the bare name
/// (e.g. a field `r#type: String` serializes as the JSON key
/// `"type"`); we must look up params using the same string.
fn ident_wire_key(name: &Ident) -> String {
    raw_ident_body(name)
}

/// Returns the ident's body sans any leading `r#`. Used for both the
/// wire key (matches serde's row serialization) and the marker
/// type-name (raw `r#` chars are illegal in struct identifiers).
fn raw_ident_body(name: &Ident) -> String {
    let s = name.to_string();
    s.strip_prefix("r#").map(str::to_string).unwrap_or(s)
}

/// If `ty` is `Option<T>`, return `(T, true)`. Otherwise `(ty, false)`.
fn unwrap_option_type(ty: &Type) -> (Type, bool) {
    if let Type::Path(tp) = ty {
        if let Some(seg) = tp.path.segments.last() {
            if seg.ident == "Option" {
                if let Some(inner) = extract_single_generic_type(seg) {
                    return (inner, true);
                }
            }
        }
    }
    (ty.clone(), false)
}

/// Remove every `#[query_param(...)]` attr from every named field of
/// `item`. Called before emitting the struct so downstream derives
/// (`Serialize`, `Deserialize`, …) never see the macro's per-field
/// annotations.
fn strip_query_param_attrs(item: &mut ItemStruct) {
    if let Fields::Named(fields) = &mut item.fields {
        for field in &mut fields.named {
            field.attrs.retain(|a| !a.path().is_ident("query_param"));
        }
    }
}

fn extract_single_generic_type(segment: &PathSegment) -> Option<Type> {
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut types = args.args.iter().filter_map(|arg| {
        if let GenericArgument::Type(ty) = arg {
            Some(ty.clone())
        } else {
            None
        }
    });
    let first = types.next()?;
    if types.next().is_some() {
        return None;
    }
    Some(first)
}

fn validate_resource_name(name: &LitStr) -> syn::Result<()> {
    let value = name.value();
    let trimmed = value.trim();
    if value.len() > MAX_SYNC_TOKEN_LEN
        || trimmed.is_empty()
        || trimmed != value
        || value.chars().any(char::is_control)
    {
        return Err(syn::Error::new(
            name.span(),
            "invalid query_resource name: must be a non-empty sync token without leading/trailing whitespace or control chars (max 1024 bytes)",
        ));
    }
    // RFC 088 §C: `:` is the live-wakeup wire delimiter. A resource
    // name containing `:` would produce a topic that collides with
    // the per-`(stream, params_hash)` format. `SyncStreamName::new`
    // also rejects this at runtime, but catching it at compile time
    // turns the `.expect("query_resource name passed validation")`
    // panic in the generated `Resource::query()` into a clear
    // proc-macro error.
    if value.contains(':') {
        return Err(syn::Error::new(
            name.span(),
            "invalid query_resource name: must not contain `:` — the live-wakeup wire \
             protocol uses `:` as a structural delimiter (RFC 088 §C)",
        ));
    }
    Ok(())
}

/// Convert the resource's `name = "..."` literal into a Rust ident
/// used as the generated module name. The wire stream name and the
/// module name share one string by design — the resource module
/// (`pub mod issues`) holds the field markers and predicate
/// evaluator that map to wire keys for `name = "issues"`.
///
/// **Collision note.** The macro emits `pub mod <name>` in the same
/// scope as the decorated struct. If the caller already has a
/// module / type / const with that name in scope, the duplicate
/// produces a Rust `defined multiple times` error. Pick a `name`
/// that doesn't collide with existing items, or move the
/// `#[query_resource]` declaration into a dedicated submodule.
/// (We can't detect the collision at expansion time — the macro
/// doesn't see the caller's other items — so this is documented
/// rather than enforced.)
fn module_ident_from_name(name: &LitStr) -> syn::Result<Ident> {
    syn::parse_str::<Ident>(&name.value()).map_err(|_| {
        syn::Error::new(
            name.span(),
            "query_resource name is not a Rust module identifier; the macro generates a module of the same name",
        )
    })
}

/// `#[query_resource(name = "issues", schema_version = 1)]` on a row
/// struct generates the query DSL surface for that resource. Queryable
/// fields opt in with `#[query_param]` (defaults to `eq`) or
/// `#[query_param(any_of|range|contains)]`. `Option<T>` is auto-detected
/// and emits `OptionalEq` rather than `RequiredEq`.
///
/// **Attribute order matters.** This attribute must come BEFORE
/// `#[derive(...)]` so it strips the per-field `#[query_param]` attrs
/// from the emitted struct before downstream derives see them.
#[proc_macro_attribute]
pub fn query_resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as QueryResourceArgs);
    let item = parse_macro_input!(item as ItemStruct);

    match expand_query_resource(args, item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_query_resource(
    args: QueryResourceArgs,
    mut item: ItemStruct,
) -> syn::Result<TokenStream2> {
    // Reject generic row structs. Propagating `item.generics` +
    // `item.generics.where_clause` through every emitted item
    // (`impl Row { … }`, `pub type Row = super::Row<…>`, `fn matches`)
    // is doable but adds complexity the current users don't need;
    // we'd rather fail loudly than silently emit broken code that
    // looks like `impl Issue { … }` for a `struct Issue<'a, T>`.
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.params.span(),
            "#[query_resource] does not support generic row structs (type or lifetime parameters). Open an issue if you need this — the macro needs to propagate generics through the generated `impl`, type alias, and `matches` fn.",
        ));
    }
    if let Some(where_clause) = &item.generics.where_clause {
        return Err(syn::Error::new(
            where_clause.span(),
            "#[query_resource] does not support row structs with `where` clauses (same limitation as type/lifetime parameters).",
        ));
    }

    let row_ident = item.ident.clone();
    let module_ident = module_ident_from_name(&args.name)?;
    let name_lit = args.name;
    let schema_version = args.schema_version;

    let params = extract_field_params(&item)?;
    strip_query_param_attrs(&mut item);

    // Reject a #[query_resource] struct with no #[query_param]
    // fields. Without any predicate the generated matches() body
    // returns `true` unconditionally — every routed change on the
    // stream lands in every subscription, including cross-tenant
    // rows. Treat this as a programming error so the macro fails at
    // compile time rather than silently shipping a stream-wide
    // wildcard.
    if params.is_empty() {
        return Err(syn::Error::new(
            row_ident.span(),
            "#[query_resource] requires at least one `#[query_param]`-annotated field. \
             A struct with no query params would generate a predicate that matches every row, \
             routing every cross-tenant mutation into the subscription. \
             Annotate your tenant-gate field with `#[query_param(required)]` (and other filter \
             fields with `#[query_param]`).",
        ));
    }

    let field_markers = generate_field_markers(&params);
    let matches_body = generate_matches_body(&params);
    let row_to_params_body = generate_row_to_params_body(&params);
    let row_to_params_typed_body = generate_row_to_params_typed_body(&params);
    let partition_for_topic_body = generate_partition_for_topic_body(&params);

    // RFC 088 §C / code-review fix #10: per-(stream, params_hash)
    // live wakeups depend on at least one `#[query_param(required)]`
    // field to partition the topic space. A `#[query_resource]` with
    // zero required fields generates a `partition_for_topic` that
    // ALWAYS returns `None`, silently downgrading every live
    // subscription to the bare stream tag — which means every routed
    // change on the stream wakes every subscriber regardless of
    // tenant. That's correct (the matches() predicate still gates
    // delivery), but loses §C's fanout-reduction benefit.
    //
    // Surface the trade-off via:
    //   1. A public const `HAS_PER_PARAMS_LIVE_ROUTING: bool` users
    //      can introspect or assert against in their own builds.
    //   2. When the count is zero, a one-shot `tracing::warn!` from
    //      `partition_for_topic` the first time it returns `None`
    //      due to the missing-required-field cause. Uses
    //      `std::sync::Once` so production logs see one line per
    //      process, not one per call. We chose the runtime channel
    //      over a compile-time `#[deprecated]` because the macro-
    //      generated lint surfaces in the user's crate's
    //      `#[query_resource(...)]` site and CAN'T be suppressed
    //      from outside the macro (lint attrs don't reach into
    //      attr-macro-emitted sibling items in stable Rust).
    let required_count = params.iter().filter(|p| p.caps.required).count();
    let has_per_params_live_routing = required_count > 0;
    let zero_required_runtime_warning = if required_count == 0 {
        let name_for_warning = name_lit.clone();
        quote! {
            // One-shot warning the first time partition_for_topic
            // returns `None` because no #[query_param(required)]
            // field was declared. Cf. RFC 088 §C / code-review
            // finding #10.
            static __PER_PARAMS_ROUTING_DISABLED_WARNING: ::std::sync::Once =
                ::std::sync::Once::new();
            __PER_PARAMS_ROUTING_DISABLED_WARNING.call_once(|| {
                ::pocopine_sync_query::__private::tracing::warn!(
                    target: "pocopine.log",
                    resource = #name_for_warning,
                    "RFC 088 §C live routing disabled: `#[query_resource]` `{}` declares no \
                     `#[query_param(required)]` field, so partition_for_topic always returns \
                     None. Every routed change on this stream will wake every subscriber. \
                     Annotate the tenant-gate field (e.g. workspace_id) with \
                     `#[query_param(required)]` to enable precise live wakeups.",
                    #name_for_warning,
                );
            });
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        #item

        impl #row_ident {
            /// Build a typed query for this resource. Returns the
            /// generic [`pocopine_sync_query::QueryBuilder`] pre-wired
            /// with the resource's stream name and predicate
            /// evaluator. Apply filters with the trait-gated DSL —
            /// `.eq(field::name, value)`, `.any_of(field::name,
            /// values)?`, `.range(field::name, range)`,
            /// `.contains(field::name, "needle")?` — then `.build()`
            /// (or `.observe(&client)` to subscribe directly).
            pub fn query() -> ::pocopine_sync_query::QueryBuilder<Self> {
                let stream = ::pocopine_sync_query::SyncStreamName::new(#name_lit)
                    .expect("query_resource name passed validation");
                ::pocopine_sync_query::Query::<Self>::builder(stream)
                    .with_matches(#module_ident::matches)
                    .with_partition_hash(#module_ident::partition_for_topic)
            }
        }

        pub mod #module_ident {
            // We pull the caller's scope into this module so the
            // user's row type and per-field comparator types resolve
            // here. Local items in this module shadow anything
            // brought in from `super`, so identifier collisions in
            // caller scope can't break the macro-generated items
            // (`Row`, `field`, `matches`).
            #[allow(unused_imports)]
            use super::*;

            pub const NAME: &str = #name_lit;
            pub const SCHEMA_VERSION: u32 = #schema_version;
            /// `true` iff this resource declares at least one
            /// `#[query_param(required)]` field — the gate that
            /// enables RFC 088 §C per-(stream, params_hash) live
            /// wakeups. When `false`, `partition_for_topic` always
            /// returns `None` and live subscriptions fall back to
            /// the bare stream tag (fanout-reduction disabled). The
            /// macro also emits a `#[deprecated]` build warning
            /// when this is `false`.
            pub const HAS_PER_PARAMS_LIVE_ROUTING: bool = #has_per_params_live_routing;
            pub type Row = super::#row_ident;

            /// Field markers for the type-safe query DSL. One marker
            /// per `#[query_param]`-annotated field; each impls exactly
            /// the comparator trait matching the keyword on the attr —
            /// so `.eq(field::workspace_id, ...)` requires a `FieldEq`
            /// marker and `.any_of(field::status, ...)` requires a
            /// `FieldInSet` marker. Misuse fails to compile.
            pub mod field {
                #[allow(unused_imports)]
                use super::*;
                #(#field_markers)*
            }

            /// Predicate evaluator: returns `true` when the row matches
            /// every declared field's comparator constraint in the
            /// query's params. Used by the routing engine to decide
            /// which queries should see a row change. The macro
            /// injects this function as the query's `matches_fn` via
            /// `QueryBuilder::with_matches`, so callers can use
            /// `query.matches(&row)` directly.
            #[allow(clippy::manual_let_else)]
            pub fn matches(
                query: &::pocopine_sync_query::Query<Row>,
                row: &Row,
            ) -> bool {
                let params = query.params();
                #matches_body
                true
            }

            /// Project a canonical row into the `StreamParams` that
            /// identify which (RFC 088 §C) per-`(stream, params_hash)`
            /// live topic should be invalidated when this row changes.
            ///
            /// Plug this into your `SyncStreamSource::row_to_params`
            /// impl so the server can route precise wakeups to
            /// matching subscribers:
            ///
            /// ```ignore
            /// impl SyncStreamSource for MyIssuesSource {
            ///     fn row_to_params(&self, row: &serde_json::Value)
            ///         -> SyncResult<StreamParams>
            ///     {
            ///         issues::row_to_params(row)
            ///     }
            ///     // ... other methods ...
            /// }
            /// ```
            ///
            /// Field selection: includes every `#[query_param]` field
            /// EXCEPT those whose only emitted comparator is
            /// `FieldContains` (substring search). Substring filters
            /// can't partition a topic space — a partial-text match
            /// doesn't tell the server which audience cares. Required-
            /// flagged fields are ALWAYS included; explicit
            /// `#[query_param(range)]` / equality-shaped types are
            /// included automatically.
            ///
            /// `None` values from `Option<T>` fields are omitted from
            /// the returned map (a null in the topic key would
            /// fragment unnecessarily; the client's params filter
            /// already handles null match semantics).
            pub fn row_to_params(
                row: &::pocopine_sync_query::__private::serde_json::Value,
            ) -> ::pocopine_sync_query::__private::pocopine_sync::SyncResult<
                ::pocopine_sync_query::__private::pocopine_sync::StreamParams,
            > {
                // RFC 088 §C / code-review fix #12: project required
                // fields directly from the JSON `row` rather than
                // round-tripping through `serde_json::from_value::<Row>`.
                // The typed-roundtrip path forced the full row payload
                // through serde even when the partition projection only
                // cares about the (typically 1–2) `required`-flagged
                // fields, and failed the whole publish if any unrelated
                // field on `Row` was non-deserializable. Direct
                // projection only touches the keys we actually need and
                // never fails — a missing/null required field yields an
                // empty `StreamParams` and the caller treats that as
                // "skip per-params publish".
                let mut params =
                    ::pocopine_sync_query::__private::pocopine_sync::StreamParams::new();
                #row_to_params_body
                ::std::result::Result::Ok(params)
            }

            /// Typed sibling of [`row_to_params`] — projects a typed
            /// `&Row` directly into `StreamParams` without the
            /// `serde_json::Value` round-trip.
            ///
            /// This is what `CrudResource::params_of` consumes — the
            /// CRUD adapter has `&Row` in hand at write time, so the
            /// JSON form would only force a needless deserialize:
            ///
            /// ```ignore
            /// let issues = resource("issues", IssueSource::default())?
            ///     .id(|r| r.id.clone())
            ///     .params_of(issues::row_to_params_typed);
            /// ```
            ///
            /// Selection rules and skip behavior match
            /// [`row_to_params`] verbatim — only `#[query_param(
            /// required)]` fields contribute, `Option::None` is
            /// skipped, `contains`-only fields are excluded.
            ///
            /// Per-field `serde_json::to_value` calls are infallible
            /// for idiomatic field types (primitives, `String`,
            /// small enums) — the function panics with a clear
            /// message naming the failing field if a custom
            /// `Serialize` impl returns an error.
            pub fn row_to_params_typed(
                row: &Row,
            ) -> ::pocopine_sync_query::__private::pocopine_sync::StreamParams {
                let mut params =
                    ::pocopine_sync_query::__private::pocopine_sync::StreamParams::new();
                #row_to_params_typed_body
                params
            }

            /// Project the caller's captured query params into a hash
            /// of the SAME canonical projection that `row_to_params`
            /// projects on the server. Used by the client driver to
            /// subscribe to the per-`(stream, params_hash)` live
            /// topic — must agree byte-for-byte with the server's
            /// hash for routing to work (RFC 088 §C).
            ///
            /// Returns `None` if the captured params can't be
            /// projected unambiguously:
            ///   * a required field is missing (no tenant gate → no
            ///     partition possible)
            ///   * a required field's value is a comparator wrapper
            ///     (`any_of` / `range` / `contains` — partition isn't
            ///     well-defined for a single topic)
            /// In either case the client should fall back to the
            /// bare topic subscription only.
            pub fn partition_for_topic(
                captured: &::pocopine_sync_query::__private::pocopine_sync::StreamParams,
            ) -> ::std::option::Option<u64> {
                let mut canonical =
                    ::pocopine_sync_query::__private::pocopine_sync::StreamParams::new();
                #partition_for_topic_body
                // Symmetric to the server: an empty canonical
                // projection means there's no per-`(stream, params)`
                // topic to subscribe to (the server's
                // `invalidate_stream_with_row` short-circuits on
                // empty params). Falling back to bare-topic only
                // also avoids requesting a topic that exact-match
                // allowlists (`allow_topics(sync.live_topics())`)
                // would reject.
                if canonical.is_empty() {
                    #zero_required_runtime_warning
                    return ::std::option::Option::None;
                }
                ::std::option::Option::Some(
                    ::pocopine_sync_query::__private::pocopine_sync::stream_params_hash(
                        NAME, &canonical,
                    ),
                )
            }
        }
    })
}

fn generate_field_markers(params: &[ParamDef]) -> Vec<TokenStream2> {
    params
        .iter()
        .map(|param| {
            let name = &param.name;
            // Wire key matches serde's serialization (which strips the
            // `r#` raw-identifier prefix). Without this strip, a row
            // field declared as `r#type: String` would have wire key
            // `"r#type"` here but `"type"` in the serialized row body
            // — params.get would never find the value.
            let name_str = ident_wire_key(name);
            // Marker ident still uses the raw form (no `r#` allowed in
            // type names but unique per field).
            let marker_ident = Ident::new(
                &format!("__Field_{}", raw_ident_body(name)),
                proc_macro2::Span::call_site(),
            );
            let inner = &param.inner_ty;

            // Always emit FieldEq + FieldInSet. Inner type is T (after
            // unwrapping any Option<T>), so `.eq(field::assignee_id,
            // "alice")` on a `Option<String>` field still typechecks
            // with `&str → String` via `Into<M::Value>`.
            let mut trait_impls = vec![
                quote! {
                    impl ::pocopine_sync_query::FieldEq for #marker_ident {
                        type Value = #inner;
                        const NAME: &'static str = #name_str;
                    }
                },
                quote! {
                    impl ::pocopine_sync_query::FieldInSet for #marker_ident {
                        type Item = #inner;
                        const NAME: &'static str = #name_str;
                    }
                },
            ];

            if param.caps.range {
                trait_impls.push(quote! {
                    impl ::pocopine_sync_query::FieldRange for #marker_ident {
                        type Bound = #inner;
                        const NAME: &'static str = #name_str;
                    }
                });
            }
            if param.caps.contains {
                trait_impls.push(quote! {
                    impl ::pocopine_sync_query::FieldContains for #marker_ident {
                        const NAME: &'static str = #name_str;
                    }
                });
            }

            quote! {
                #[allow(non_camel_case_types)]
                #[doc = concat!("Field marker for `", #name_str, "` — used by the query DSL methods (`.eq`, `.any_of`, `.range`, `.contains`).")]
                #[derive(::std::marker::Copy, ::std::clone::Clone)]
                pub struct #marker_ident;
                // SAFETY: marker is macro-generated and matches the
                // comparator declared on the row struct. The `unsafe`
                // here is API-stability, not memory-safety.
                unsafe impl ::pocopine_sync_query::predicate::__SealedFieldMarker for #marker_ident {}
                #(#trait_impls)*
                #[allow(non_upper_case_globals)]
                pub const #name: #marker_ident = #marker_ident;
            }
        })
        .collect()
}

/// Generate the body of `matches(query, row) -> bool`. For each
/// `#[query_param]` field, emits a labeled block that:
/// * fetches `params[field_name]`,
/// * fails closed if absent and the field is required (non-Option),
/// * dispatches on the wire value's JSON shape (InSet object → set
///   membership; Range object → range_contains; Contains object →
///   contains_matches; bare T → equality),
/// * the dispatch order tries the most specific structured shapes
///   before the bare-value fallback.
fn generate_matches_body(params: &[ParamDef]) -> TokenStream2 {
    let checks: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            // Same wire-key strip as `generate_field_markers` —
            // matches the row's serde serialization, which strips
            // the raw-identifier `r#` prefix.
            let name_str = ident_wire_key(name);
            let inner = &param.inner_ty;
            let field_access: TokenStream2 = quote! { row.#name };
            let block_label = syn::Lifetime::new(
                &format!("'__pp_check_{}", raw_ident_body(name)),
                proc_macro2::Span::call_site(),
            );

            // Per-branch handlers. Each `if let Ok(...) = parse {
            // predicate; break 'label }` block; if predicate fails
            // we `return false`; if predicate passes we exit the
            // labeled block (continue to next field check).
            let any_of_predicate = if param.caps.optional {
                quote! {
                    match &#field_access {
                        ::std::option::Option::Some(v) if set.values().iter().any(|x| x == v) => {}
                        _ => return false,
                    }
                }
            } else {
                quote! {
                    if !set.values().iter().any(|v| v == &#field_access) {
                        return false;
                    }
                }
            };
            let any_of_branch = quote! {
                if let ::std::result::Result::Ok(set) =
                    ::pocopine_sync_query::__private::serde_json::from_value::<
                        ::pocopine_sync_query::params::InSet<#inner>
                    >(raw.clone())
                {
                    #any_of_predicate
                    break #block_label;
                }
            };

            let range_branch = if param.caps.range {
                quote! {
                    if let ::std::result::Result::Ok(range) =
                        ::pocopine_sync_query::__private::serde_json::from_value::<
                            ::pocopine_sync_query::params::Range<#inner>
                        >(raw.clone())
                    {
                        if !::pocopine_sync_query::predicate::range_contains(&range, &#field_access) {
                            return false;
                        }
                        break #block_label;
                    }
                }
            } else {
                quote! {}
            };

            let contains_branch = if param.caps.contains {
                quote! {
                    if let ::std::result::Result::Ok(needle) =
                        ::pocopine_sync_query::__private::serde_json::from_value::<
                            ::pocopine_sync_query::params::Contains
                        >(raw.clone())
                    {
                        if !::pocopine_sync_query::predicate::contains_matches(&needle, &#field_access) {
                            return false;
                        }
                        break #block_label;
                    }
                }
            } else {
                quote! {}
            };

            let eq_predicate = if param.caps.optional {
                quote! {
                    match &#field_access {
                        ::std::option::Option::Some(v) if v == &want => {}
                        _ => return false,
                    }
                }
            } else {
                quote! {
                    if #field_access != want { return false; }
                }
            };
            let eq_branch = quote! {
                if let ::std::result::Result::Ok(want) =
                    ::pocopine_sync_query::__private::serde_json::from_value::<#inner>(raw.clone())
                {
                    #eq_predicate
                    break #block_label;
                }
            };

            if param.caps.optional {
                // Option<T>: param-absent = no constraint;
                // param-null = field MUST be None.
                quote! {
                    if let ::std::option::Option::Some(raw) = params.get(#name_str) {
                        #block_label: {
                            if raw.is_null() {
                                if #field_access.is_some() { return false; }
                                break #block_label;
                            }
                            #any_of_branch
                            #range_branch
                            #contains_branch
                            #eq_branch
                            // No known wire shape matched.
                            return false;
                        }
                    }
                }
            } else if param.caps.required {
                // `#[query_param(required)]`: param MUST be in
                // params or the predicate fails (cross-tenant leak
                // guard for fields like `workspace_id`). Then
                // dispatch on the wire shape.
                quote! {
                    {
                        let raw = match params.get(#name_str) {
                            ::std::option::Option::Some(r) => r,
                            ::std::option::Option::None => return false,
                        };
                        #block_label: {
                            #any_of_branch
                            #range_branch
                            #contains_branch
                            #eq_branch
                            // No known wire shape matched.
                            return false;
                        }
                    }
                }
            } else {
                // Bare `#[query_param]` on a non-Option field:
                // queryable but not required. Param-absent = no
                // constraint (skip the check). Dispatch on wire
                // shape only when set.
                quote! {
                    if let ::std::option::Option::Some(raw) = params.get(#name_str) {
                        #block_label: {
                            #any_of_branch
                            #range_branch
                            #contains_branch
                            #eq_branch
                            // No known wire shape matched.
                            return false;
                        }
                    }
                }
            }
        })
        .collect();

    quote! {
        let _ = params; // silence unused if no #[query_param] fields declared
        #(#checks)*
    }
}

/// Generate the body of the `row_to_params(row: &Value) ->
/// SyncResult<StreamParams>` function inside the generated module
/// (see RFC 088 §C). Iterates the same `#[query_param]` fields the
/// matches() body iterates, but:
///
/// 1. **Skips contains-only fields.** A field whose only emitted
///    comparator is `FieldContains` (substring search) can't
///    partition the topic space — a `.contains(field::title,
///    "auth")?` filter matches an unbounded set of row values, so
///    including the row's title in the params map would generate a
///    distinct topic per row and defeat the fanout-reduction goal.
///    Fields that ALSO carry `range`/`required`/or-are-non-String
///    stay in the map.
///
/// 2. **Drops `None` from `Option<T>` fields.** Inserting a JSON
///    null into params would partition `Option::None` rows into a
///    distinct topic — almost always not what the user wants. The
///    routing engine's predicate evaluator (matches() body) already
///    handles `None` symmetry.
///
/// 3. **Uses each field's wire-key** (raw-ident-stripped) so the
///    emitted topic keys match `serde_json::to_value(row)`'s field
///    names. If the user later adds `#[serde(rename = "…")]`, the
///    macro doesn't currently follow it — same limitation as the
///    existing matches() body and the existing macro tests.
fn generate_row_to_params_body(params: &[ParamDef]) -> TokenStream2 {
    let inserts: Vec<TokenStream2> = params
        .iter()
        .filter(|p| include_in_row_params(&p.caps))
        .map(|param| {
            let name = &param.name;
            let name_str = ident_wire_key(name);
            // Direct JSON projection (code-review fix #12). For both
            // required and Option-wrapped required fields the semantics
            // are the same: insert only if the JSON has the key and
            // it's non-null. A missing or null required field
            // short-circuits the partition (the surrounding caller
            // treats an empty `StreamParams` as "skip per-params
            // publish"), which matches what the typed-roundtrip path
            // produced via the `Option::None` skip and serde's
            // `default` handling.
            quote! {
                if let ::std::option::Option::Some(v) = row.get(#name_str) {
                    if !v.is_null() {
                        params.insert(#name_str.to_string(), v.clone());
                    }
                }
            }
        })
        .collect();

    quote! {
        #(#inserts)*
    }
}

/// Generate the body of the **typed** `row_to_params_typed(row: &Row)
/// -> StreamParams` function inside the generated module. Mirrors
/// `generate_row_to_params_body` but reads directly off the typed
/// `&Row` instead of a JSON `Value` — `CrudResource::params_of` plugs
/// the typed projector in so its `SyncStreamSource::row_to_params`
/// override doesn't pay the serialize-then-deserialize cost when it
/// already holds the typed row.
///
/// `serde_json::to_value` IS called per field, but only on the
/// `required` field's value (typically `String` or a small enum),
/// not the whole row. For idiomatic field types (primitives, owned
/// strings, lightweight enums) the serialize is infallible — we
/// `.expect()` on the impossible-in-practice failure path with a
/// clear panic message that names the field.
fn generate_row_to_params_typed_body(params: &[ParamDef]) -> TokenStream2 {
    let inserts: Vec<TokenStream2> = params
        .iter()
        .filter(|p| include_in_row_params(&p.caps))
        .map(|param| {
            let name = &param.name;
            let name_str = ident_wire_key(name);
            let field_access: TokenStream2 = quote! { row.#name };
            if param.caps.optional {
                quote! {
                    if let ::std::option::Option::Some(inner) = &#field_access {
                        let v = ::pocopine_sync_query::__private::serde_json::to_value(inner)
                            .expect(concat!(
                                "row_to_params_typed: field `",
                                #name_str,
                                "` must be serde_json-serializable — \
                                 #[query_param(required)] field types are constrained to \
                                 Serialize, and idiomatic primitive / String / small-enum \
                                 fields never fail to serialize"
                            ));
                        params.insert(#name_str.to_string(), v);
                    }
                }
            } else {
                quote! {
                    let v = ::pocopine_sync_query::__private::serde_json::to_value(&#field_access)
                        .expect(concat!(
                            "row_to_params_typed: field `",
                            #name_str,
                            "` must be serde_json-serializable — \
                             #[query_param(required)] field types are constrained to \
                             Serialize, and idiomatic primitive / String / small-enum \
                             fields never fail to serialize"
                        ));
                    params.insert(#name_str.to_string(), v);
                }
            }
        })
        .collect();

    quote! {
        #(#inserts)*
    }
}

/// Decide whether a `#[query_param]` field contributes to the
/// per-(stream, params_hash) topic key (RFC 088 §C).
///
/// **Only `required`-flagged fields partition the topic.** This
/// keeps the server-side `row_to_params` and the client-side
/// `partition_for_topic` projections aligned: a client subscription
/// like `Issue::query().eq(workspace_id, "W1")` (just the tenant
/// gate) hashes to the same value as `row_to_params` extracts from a
/// row in that workspace, regardless of what other filters the
/// query carries (status, assignee, title, etc.).
///
/// Optional / non-required fields are NOT in the topic key because:
///   1. Different queries on the same stream filter on different
///      subsets — including all optional fields in the row's hash
///      would produce a hash that no subset-filtering subscriber
///      ever computes on its side. (Codex finding.)
///   2. Free-text (`contains`) fields naturally explode topic
///      cardinality.
///   3. Range / any_of comparator wrappers can't be hashed as plain
///      values; they don't fit the "exact-match hash" contract.
///
/// Users who want finer routing (e.g. per-(workspace, status)) mark
/// `status` as `required` too. The routing engine's existing
/// matches predicate then handles the cross-tenant gate AND the
/// status filter on the client side.
fn include_in_row_params(caps: &FieldCapabilities) -> bool {
    caps.required
}

/// Generate the body of `partition_for_topic` — the client-side
/// projection that mirrors `row_to_params` for the topic hash.
/// Reads each REQUIRED `#[query_param]` field from the caller's
/// captured `StreamParams` and inserts it (verbatim) into the
/// canonical map. Returns `None` from the surrounding function if
/// any required field is missing or is a comparator wrapper.
fn generate_partition_for_topic_body(params: &[ParamDef]) -> TokenStream2 {
    let extracts: Vec<TokenStream2> = params
        .iter()
        .filter(|p| include_in_row_params(&p.caps))
        .map(|param| {
            let name = &param.name;
            let name_str = ident_wire_key(name);

            // Detect comparator-wrapped values by FULL-shape match,
            // not key-overlap. The three comparator wire shapes
            // (defined in `pocopine_sync_query::params`) all use
            // `deny_unknown_fields`, so a user type that happens to
            // contain a `"contains"` or `"in"` or `"from"` key
            // AMONG OTHER KEYS isn't a comparator and must be
            // hashed as a plain partition value.
            //
            //   * InSet wire shape: keys == {"in"}
            //   * Contains wire shape (if caps.contains):
            //       keys ⊆ {"contains","case_sensitive"} AND "contains"∈keys
            //   * Range wire shape (if caps.range):
            //       keys ⊆ {"from","to","inclusive"} AND (from|to)∈keys
            //
            // Capability gating ensures a `(range)`-only-on-paper
            // field (e.g. a `Range`-impl'd numeric) doesn't reject
            // a TimeWindow-shaped equality value if the field
            // wasn't marked range-capable. Plus the FULL-shape
            // match means even marking the field `(range)` doesn't
            // false-positive on a user struct like `{from, to,
            // label, status}` — the extra keys disqualify the
            // match.
            let mut shape_checks: Vec<TokenStream2> = Vec::new();
            // InSet always — every `#[query_param]` field gets `FieldInSet`.
            shape_checks.push(quote! {
                if obj.len() == 1 && obj.contains_key("in") {
                    return ::std::option::Option::None;
                }
            });
            if param.caps.contains {
                shape_checks.push(quote! {
                    if obj.contains_key("contains")
                        && obj.keys().all(|k| matches!(k.as_str(), "contains" | "case_sensitive"))
                    {
                        return ::std::option::Option::None;
                    }
                });
            }
            if param.caps.range {
                shape_checks.push(quote! {
                    if (obj.contains_key("from") || obj.contains_key("to"))
                        && obj.keys().all(|k| matches!(k.as_str(), "from" | "to" | "inclusive"))
                    {
                        return ::std::option::Option::None;
                    }
                });
            }

            quote! {
                {
                    // Required field absent → no partition; `?` on
                    // Option<&Value> yields None from the outer fn.
                    let value = captured.get(#name_str)?;
                    // Reject comparator-wrapped values. See the
                    // FULL-shape rationale above.
                    if let ::pocopine_sync_query::__private::serde_json::Value::Object(obj) = value {
                        #(#shape_checks)*
                    }
                    canonical.insert(#name_str.to_string(), value.clone());
                }
            }
        })
        .collect();

    quote! {
        let _ = captured; // silence unused if no required fields declared
        #(#extracts)*
    }
}

// ---- #[query] selector macro --------------------------------------

/// `#[query]` — declare a memoized selector function over
/// `pocopine-sync-query` reactive state.
///
/// Wraps a Rust function so that its return value is cached by
/// `(SelectorId, args_hash)`, its reads of `QueryView::rows()` are
/// tracked, and the cached value is automatically refreshed when a
/// tracked subscription changes. The user-facing return type
/// MUST implement `PartialEq + Clone + 'static`; each argument MUST
/// implement `Clone + std::hash::Hash + 'static`.
///
/// The macro replaces the annotated `fn name(args...) -> Ret { ... }`
/// with a sibling module `name` containing:
///
/// * `pub const SELECTOR_ID: pocopine_sync_query::SelectorId` —
///   `FNV-1a 64(concat!(module_path!(), "::", "name"))`.
/// * `pub fn observe(client: &QueryClient, args...) -> SelectorView<Ret>`.
/// * a private `__user_fn(args) -> Ret` carrying the original body.
///
/// `observe()` hashes the args via `std::hash::Hash`, looks up or
/// creates the cached entry, and returns a fresh `SelectorView`. The
/// compute closure clones the args and the client into a `'static`
/// `Fn` so it can be invoked on every rerun.
///
/// See `docs/sync-query-selector-mechanism.md` for the runtime
/// design and the four-moving-parts walk-through.
#[proc_macro_attribute]
pub fn query(attr: TokenStream, item: TokenStream) -> TokenStream {
    // No selector attributes are accepted in v1. `#[query(no_diff)]`
    // is deferred — keep the parser strict so misuse fails loud
    // instead of silently being ignored.
    if !attr.is_empty() {
        let attr_ts: TokenStream2 = attr.into();
        return syn::Error::new_spanned(
            attr_ts,
            "`#[query]` does not accept any arguments yet (the `no_diff` opt-out is \
             deferred — track via RFC follow-up)",
        )
        .to_compile_error()
        .into();
    }

    let func = parse_macro_input!(item as ItemFn);
    match expand_query(func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_query(func: ItemFn) -> syn::Result<TokenStream2> {
    let ItemFn {
        attrs,
        vis,
        sig,
        block,
    } = func;

    // Reject anything that doesn't fit the selector contract.
    if let Some(asyncness) = sig.asyncness {
        return Err(syn::Error::new(
            asyncness.span,
            "`#[query]` selectors must be synchronous — the compute closure runs inside the \
             reactive-rerun path",
        ));
    }
    if let Some(unsafety) = sig.unsafety {
        return Err(syn::Error::new(
            unsafety.span,
            "`#[query]` selectors must not be `unsafe`",
        ));
    }
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &sig.generics,
            "`#[query]` selectors must not be generic — the cache key is keyed by the args' \
             runtime hash, not by type parameters",
        ));
    }
    if let Some(where_clause) = &sig.generics.where_clause {
        return Err(syn::Error::new_spanned(
            where_clause,
            "`#[query]` selectors must not have a `where` clause",
        ));
    }
    if let Some(variadic) = &sig.variadic {
        return Err(syn::Error::new_spanned(
            variadic,
            "`#[query]` selectors must not be variadic",
        ));
    }
    if sig.constness.is_some() {
        return Err(syn::Error::new_spanned(
            sig.constness,
            "`#[query]` selectors cannot be `const fn`",
        ));
    }

    let fn_name = sig.ident.clone();
    let fn_name_str = fn_name.to_string();
    let ret_type = match &sig.output {
        ReturnType::Type(_, t) => (**t).clone(),
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                &sig.ident,
                "`#[query]` selectors must declare an explicit return type — the default `()` \
                 return is rarely useful and trips up `PartialEq + Clone` inference",
            ));
        }
    };

    // Extract typed args. Reject `self` receivers and patterns more
    // complex than a simple ident (the compute closure needs to
    // capture each by name). Also reject argument-position
    // `impl Trait`: it makes the user fn generic over the type at
    // the call site, but the cache key only carries
    // `(SelectorId, args_hash)` — two instantiations with the same
    // hash would alias each other's cached entry across different
    // monomorphized return types.
    let mut arg_idents: Vec<Ident> = Vec::with_capacity(sig.inputs.len());
    let mut arg_mutabilities: Vec<Option<Token![mut]>> = Vec::with_capacity(sig.inputs.len());
    let mut arg_types: Vec<Type> = Vec::with_capacity(sig.inputs.len());
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(rec) => {
                return Err(syn::Error::new_spanned(
                    rec,
                    "`#[query]` selectors must be free functions — no `self` parameter",
                ));
            }
            FnArg::Typed(pt) => {
                let (ident, mutability) = match &*pt.pat {
                    Pat::Ident(PatIdent {
                        ident,
                        mutability,
                        subpat: None,
                        by_ref: None,
                        ..
                    }) => (ident.clone(), *mutability),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "`#[query]` selector args must use simple identifier patterns \
                             (e.g. `ws_id: String`); destructuring and `ref` are not supported",
                        ));
                    }
                };
                if contains_impl_trait(&pt.ty) {
                    return Err(syn::Error::new_spanned(
                        &pt.ty,
                        "`#[query]` selectors cannot use argument-position `impl Trait` — \
                         not even nested (e.g. `Box<impl Trait>`, `Option<impl Trait>`). \
                         Argument-position `impl Trait` makes the fn generic over the \
                         call-site type, but the cache key only covers \
                         `(SelectorId, args_hash)`, so two concrete instantiations could \
                         alias each other's cached entries. Use a concrete type.",
                    ));
                }
                arg_idents.push(ident);
                arg_mutabilities.push(mutability);
                arg_types.push((*pt.ty).clone());
            }
        }
    }

    // Convention: if the user fn's first arg is typed `QueryClient`
    // (any path ending in that segment), the macro treats it as the
    // selector's client handle — NOT hashed, NOT replicated in the
    // generated `observe()`'s arg list (which always takes
    // `&QueryClient` as its first param). The user's body sees the
    // client as that first param so it can call `client.observe(q)`
    // ergonomically. If the user fn has no `QueryClient` arg, the
    // body must reach a client some other way (thread_local, etc.).
    let client_arg_present = arg_types.first().is_some_and(is_query_client_type);

    // Hashable (user-visible) args: everything except the recognized
    // client arg. These are what `observe()` exposes publicly and
    // what `args_hash` covers.
    let (user_arg_idents, user_arg_types): (Vec<Ident>, Vec<Type>) = if client_arg_present {
        (arg_idents[1..].to_vec(), arg_types[1..].to_vec())
    } else {
        (arg_idents.clone(), arg_types.clone())
    };
    // Per-arg helper idents for the captured + per-rerun clones.
    // Named by position (`__pq_arg_0`, `__pq_arg_1`, …) so a raw
    // identifier user-arg (`r#type: String`) doesn't produce an
    // invalid `__pq_arg_r#type` and panic during macro expansion.
    let captured_user_idents: Vec<Ident> = (0..user_arg_idents.len())
        .map(|i| Ident::new(&format!("__pq_arg_{i}"), proc_macro2::Span::call_site()))
        .collect();

    // `let __pq_client_for_compute = client.clone();` — emitted only
    // when the user declared a client arg, so the closure has
    // something to clone into each rerun's `__user_fn` call.
    let client_capture_let: TokenStream2 = if client_arg_present {
        quote! {
            let __pq_client_for_compute =
                ::core::clone::Clone::clone(__pq_client);
        }
    } else {
        quote! {}
    };

    // Args passed into `__user_fn` per rerun. When the user
    // declared a client param, the first arg is a cloned client;
    // the rest are cloned hashed args.
    let user_fn_call_args: Vec<TokenStream2> = {
        let mut out: Vec<TokenStream2> = Vec::with_capacity(arg_idents.len());
        if client_arg_present {
            out.push(quote! { ::core::clone::Clone::clone(&__pq_client_for_compute) });
        }
        for captured in &captured_user_idents {
            out.push(quote! { ::core::clone::Clone::clone(&#captured) });
        }
        out
    };

    // Visibility for the generated module. Forward the user's
    // visibility so a `pub fn` selector becomes a `pub mod`.
    let mod_vis = &vis;

    // Split the user-written attrs by audience:
    //
    // * Lint / behavior attrs (`#[allow]`, `#[deny]`, `#[warn]`,
    //   `#[forbid]`, `#[expect]`, `#[inline]`, `#[cold]`, `#[cfg]`,
    //   `#[cfg_attr]`) follow the body to the lifted inner fn so
    //   lints emitted inside the body see them.
    // * Public-API attrs (`#[doc]` / `///`, `#[deprecated]`,
    //   `#[must_use]`) go on the public module — they describe the
    //   selector's outward surface. Putting `#[deprecated]` on the
    //   inner fn would cause the macro's own `super::<inner>()`
    //   call to trigger `deny(deprecated)`.
    // * `#[cfg]` and `#[cfg_attr]` end up on BOTH so a cfg-disabled
    //   selector compiles consistently (the module and the body
    //   need to disappear together).
    let (module_attrs, observe_attrs, inner_attrs): (
        Vec<&Attribute>,
        Vec<&Attribute>,
        Vec<&Attribute>,
    ) = partition_selector_attrs(&attrs);

    // Mangled name for the carried-over user body. Lives at the
    // SAME module level as the original `#[query] fn` so any
    // `super::*` references in the body keep their original
    // resolution. The generated module's `observe()` calls it via
    // `super::<this_ident>`.
    //
    // Strip any `r#` raw-ident prefix via `raw_ident_body`. Without
    // this, `#[query] fn r#type(...)` would expand into
    // `Ident::new("__pq_user_fn__r#type", …)`, which panics — `#`
    // is not a valid identifier character. The raw form is unique
    // among rustc-accepted fn names because keyword-collisioning
    // names HAVE to be written `r#kw`, so `raw_ident_body("r#kw")`
    // == "kw" is unambiguous in the surrounding mod scope.
    let inner_fn_ident = Ident::new(
        &format!("__pq_user_fn__{}", raw_ident_body(&fn_name)),
        fn_name.span(),
    );

    // Reconstruct each parameter for the inner fn, preserving any
    // `mut` binding the user wrote.
    let inner_fn_params: Vec<TokenStream2> = arg_idents
        .iter()
        .zip(arg_mutabilities.iter())
        .zip(arg_types.iter())
        .map(|((ident, mutability), ty)| {
            quote! { #mutability #ident: #ty }
        })
        .collect();

    Ok(quote! {
        // Inner-fn attrs: lint and behavior attrs that need to
        // follow the body. `#[doc(hidden)]` keeps any user docs out
        // of rustdoc (the public module already carries them).
        #(#inner_attrs)*
        #[doc(hidden)]
        #[allow(non_snake_case, clippy::too_many_arguments)]
        fn #inner_fn_ident( #( #inner_fn_params ),* ) -> #ret_type #block

        // Module attrs: doc comments, `#[deprecated]`,
        // `#[must_use]` — they describe the selector's public API.
        // `#[cfg(...)]` attrs are duplicated here so the module
        // disappears in lockstep with the body.
        #(#module_attrs)*
        #[allow(non_camel_case_types, non_snake_case, dead_code)]
        #mod_vis mod #fn_name {
            // Bring the parent module's items into scope so `observe`
            // can resolve `QueryClient` / `SelectorId` if the parent
            // already has them in scope. Doesn't pollute the user
            // body's resolution, since the body lives in the parent
            // module (see `super::#inner_fn_ident`).
            use super::*;

            /// Stable identity for this selector. FNV-1a 64 of
            /// `concat!(module_path!(), "::", "<fn_name>")` evaluated
            /// at the user-crate compile time. Uniqueness collapses
            /// to "no two `#[query]` fns share the same module path
            /// AND name" — a conflict rustc already rejects.
            pub const SELECTOR_ID: ::pocopine_sync_query::SelectorId =
                ::pocopine_sync_query::SelectorId::new(
                    ::pocopine_sync_query::__private::fnv1a64(
                        ::core::concat!(::core::module_path!(), "::", #fn_name_str).as_bytes()
                    )
                );

            /// Observe this selector. Hashes the args, looks up or
            /// creates the cached entry, returns a fresh
            /// [`SelectorView`](::pocopine_sync_query::SelectorView).
            ///
            /// On a cache hit, the user body is NOT called — the
            /// cached value is reused. On a cache miss, the body
            /// runs inside the selector's tracking frame so any
            /// `QueryView::rows()` reads register as dependencies.
            #(#observe_attrs)*
            pub fn observe(
                __pq_client: &::pocopine_sync_query::QueryClient,
                #( #user_arg_idents : #user_arg_types ),*
            ) -> ::pocopine_sync_query::SelectorView<#ret_type> {
                // Hash hashable args. `DefaultHasher` is fine for
                // bucketing speed; the actual cache equality check
                // uses `AnyArgs::dyn_eq` on the boxed tuple below,
                // so hash collisions cannot cause cross-args
                // aliasing.
                let mut __pq_hasher =
                    ::std::collections::hash_map::DefaultHasher::new();
                #(
                    ::std::hash::Hash::hash(&#user_arg_idents, &mut __pq_hasher);
                )*
                let __pq_args_hash = ::std::hash::Hasher::finish(&__pq_hasher);

                // Boxed args tuple — used by the runtime to
                // disambiguate hash collisions via PartialEq.
                let __pq_args_boxed: ::std::boxed::Box<
                    dyn ::pocopine_sync_query::AnyArgs,
                > = ::std::boxed::Box::new((
                    #( ::core::clone::Clone::clone(&#user_arg_idents), )*
                ));

                // Capture owned clones of args (and a clone of the
                // client, if the user declared a client arg) into
                // the `'static` `Fn` closure. Each rerun further
                // clones them to pass into the user body (which
                // takes args by value, matching the user-written
                // signature).
                #client_capture_let
                #(
                    let #captured_user_idents =
                        ::core::clone::Clone::clone(&#user_arg_idents);
                )*
                let __pq_compute = move || -> #ret_type {
                    super::#inner_fn_ident( #( #user_fn_call_args ),* )
                };
                __pq_client.observe_selector(
                    SELECTOR_ID,
                    __pq_args_hash,
                    __pq_args_boxed,
                    __pq_compute,
                )
            }
        }
    })
}

/// Split user-supplied attrs on a `#[query] fn` into three groups —
/// `(module_attrs, observe_attrs, inner_attrs)` — by audience. See
/// the call site in `expand_query` for the rationale.
///
/// * `#[doc]` / `///`, `#[deprecated]` → module (public API surface).
/// * `#[must_use]` → `observe()` (Rust rejects `#[must_use]` on
///   modules; the lint fires against the callsite of `observe()`,
///   which is what the user intended).
/// * `#[track_caller]` → `observe()` only. The user body is invoked
///   through an anonymous compute closure that breaks the
///   `#[track_caller]` chain regardless of where the attr lives, so
///   putting it on the lifted body would actually DEGRADE panic
///   location quality: `panic!()` inside the body without the attr
///   reports the body's panic line; with the attr it reports the
///   (opaque) closure call site. Observe-only is the best we can
///   do; `Location::caller()` reads inside the body are a
///   documented unsupported case.
/// * `#[allow]` / `#[deny]` / `#[warn]` / `#[forbid]` / `#[expect]`,
///   `#[inline]` / `#[cold]` / `#[no_mangle]` → inner fn (body
///   audience).
/// * `#[cfg]` / `#[cfg_attr]` → all three (module, observe, and
///   body must compile or disappear together).
/// * Unknown attrs default to the inner fn (it carries the body, so
///   most behavior-y attrs belong there).
fn partition_selector_attrs(
    attrs: &[Attribute],
) -> (Vec<&Attribute>, Vec<&Attribute>, Vec<&Attribute>) {
    let mut module = Vec::new();
    let mut observe = Vec::new();
    let mut inner = Vec::new();
    for attr in attrs {
        let ident = attr.path().segments.last().map(|s| s.ident.to_string());
        match ident.as_deref() {
            Some("doc" | "deprecated") => module.push(attr),
            Some("must_use" | "track_caller") => observe.push(attr),
            Some("cfg" | "cfg_attr") => {
                module.push(attr);
                observe.push(attr);
                inner.push(attr);
            }
            _ => inner.push(attr),
        }
    }
    (module, observe, inner)
}

/// Detect whether a type is `QueryClient` by last path segment.
/// Matches `QueryClient`, `pocopine_sync_query::QueryClient`, etc;
/// does NOT match `&QueryClient` or `Rc<QueryClient>` (the macro's
/// contract requires an owned client param, mirroring the
/// generated compute closure's capture).
fn is_query_client_type(ty: &Type) -> bool {
    let Type::Path(p) = ty else { return false };
    p.path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == "QueryClient")
}

/// Walk `ty` recursively and return `true` if any embedded type is
/// argument-position `impl Trait`. Catches `Box<impl T>`,
/// `Option<impl T>`, `(impl T, …)`, `&impl T`, etc. Rust desugars
/// these to anonymous generics; with the selector cache key only
/// covering `(SelectorId, args_hash)`, two concrete instantiations
/// of the same `observe()` could alias their cached entries — that's
/// the unsoundness the recursive check guards against.
fn contains_impl_trait(ty: &Type) -> bool {
    match ty {
        Type::ImplTrait(_) => true,
        Type::Reference(r) => contains_impl_trait(&r.elem),
        Type::Slice(s) => contains_impl_trait(&s.elem),
        Type::Array(a) => contains_impl_trait(&a.elem),
        Type::Ptr(p) => contains_impl_trait(&p.elem),
        Type::Paren(p) => contains_impl_trait(&p.elem),
        Type::Group(g) => contains_impl_trait(&g.elem),
        Type::Tuple(t) => t.elems.iter().any(contains_impl_trait),
        Type::Path(p) => p
            .path
            .segments
            .iter()
            .any(|seg| path_args_contain_impl_trait(&seg.arguments)),
        // Walk `dyn Trait1 + Trait2<Item = impl X>` — the bound's
        // path can carry assoc-type bindings that contain
        // `impl Trait`.
        Type::TraitObject(t) => t.bounds.iter().any(|b| {
            if let syn::TypeParamBound::Trait(tb) = b {
                tb.path
                    .segments
                    .iter()
                    .any(|seg| path_args_contain_impl_trait(&seg.arguments))
            } else {
                false
            }
        }),
        _ => false,
    }
}

fn path_args_contain_impl_trait(args: &PathArguments) -> bool {
    let PathArguments::AngleBracketed(args) = args else {
        return false;
    };
    args.args.iter().any(|arg| match arg {
        GenericArgument::Type(t) => contains_impl_trait(t),
        // `Trait<Item = impl X>` — the assoc-type's RHS can be
        // `impl Trait`.
        GenericArgument::AssocType(at) => contains_impl_trait(&at.ty),
        // `Trait<Item: impl X>` — the constraint's bounds can carry
        // trait paths whose segments embed `impl Trait`.
        GenericArgument::Constraint(c) => c.bounds.iter().any(|b| {
            if let syn::TypeParamBound::Trait(tb) = b {
                tb.path
                    .segments
                    .iter()
                    .any(|seg| path_args_contain_impl_trait(&seg.arguments))
            } else {
                false
            }
        }),
        _ => false,
    })
}
