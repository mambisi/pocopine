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
//! `#[query_param]` (default `eq`) or `#[query_param(any_of|range|contains)]`.
//! The macro strips those attrs from the emitted struct so downstream
//! derives (`Serialize`, `Deserialize`, …) see a clean shape.
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
    parse_macro_input, Attribute, Fields, GenericArgument, Ident, ItemStruct, LitInt, LitStr, Meta,
    PathArguments, PathSegment, Token, Type,
};

const MAX_SYNC_TOKEN_LEN: usize = 1024;

/// Inferred comparator kind for one entry in `params(...)`.
#[derive(Clone, Debug)]
enum ComparatorKind {
    /// Bare `T` — required equality.
    RequiredEq,
    /// `Option<T>` — optional equality.
    OptionalEq { inner: Type },
    /// `params::InSet<T>` — membership in a non-empty set.
    InSet { inner: Type },
    /// `params::Range<T>` — bounded range.
    Range { inner: Type },
    /// `params::Contains` — substring match against a text field.
    Contains,
}

/// One field declared in `#[query_resource(params(...))]`.
#[derive(Clone, Debug)]
struct ParamDef {
    name: Ident,
    ty: Type,
    kind: ComparatorKind,
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

/// Comparator keyword as written in `#[query_param(...)]`. Bare
/// `#[query_param]` defaults to `Eq`.
#[derive(Clone, Copy, Debug)]
enum ComparatorKeyword {
    Eq,
    AnyOf,
    Range,
    Contains,
}

/// Walk a struct's named fields, collect a `ParamDef` for every field
/// that carries `#[query_param(...)]`. The inner T for `InSet` / `Range`
/// / `Contains` is read from the field's declared type — no
/// `params::InSet<Status>` duplication.
fn extract_field_params(item: &ItemStruct) -> syn::Result<Vec<ParamDef>> {
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

        let keyword = parse_query_param_keyword(attr)?;
        let (inner_ty, is_option) = unwrap_option_type(&field.ty);

        let kind = match keyword {
            ComparatorKeyword::Eq => {
                if is_option {
                    ComparatorKind::OptionalEq { inner: inner_ty }
                } else {
                    ComparatorKind::RequiredEq
                }
            }
            ComparatorKeyword::AnyOf => {
                if is_option {
                    return Err(syn::Error::new(
                        attr.span(),
                        "`#[query_param(any_of)]` on `Option<T>` is not supported — declare the field as `T` (and add a `None` variant to the set if needed)",
                    ));
                }
                ComparatorKind::InSet { inner: inner_ty }
            }
            ComparatorKeyword::Range => {
                if is_option {
                    return Err(syn::Error::new(
                        attr.span(),
                        "`#[query_param(range)]` on `Option<T>` is not supported — declare the field as `T`",
                    ));
                }
                ComparatorKind::Range { inner: inner_ty }
            }
            ComparatorKeyword::Contains => {
                if is_option {
                    return Err(syn::Error::new(
                        attr.span(),
                        "`#[query_param(contains)]` on `Option<T>` is not supported — declare the field as `T`",
                    ));
                }
                ComparatorKind::Contains
            }
        };

        params.push(ParamDef {
            name: field_name,
            ty: field.ty.clone(),
            kind,
        });
    }

    Ok(params)
}

/// Parse the comparator keyword from `#[query_param]` / `#[query_param(eq)]`
/// / `#[query_param(any_of)]` / etc.
fn parse_query_param_keyword(attr: &Attribute) -> syn::Result<ComparatorKeyword> {
    match &attr.meta {
        Meta::Path(_) => Ok(ComparatorKeyword::Eq),
        Meta::List(_) => {
            let ident: Ident = attr.parse_args().map_err(|_| {
                syn::Error::new(
                    attr.span(),
                    "expected a comparator keyword inside `#[query_param(...)]` — one of `eq`, `any_of`, `range`, `contains`",
                )
            })?;
            match ident.to_string().as_str() {
                "eq" => Ok(ComparatorKeyword::Eq),
                "any_of" => Ok(ComparatorKeyword::AnyOf),
                "range" => Ok(ComparatorKeyword::Range),
                "contains" => Ok(ComparatorKeyword::Contains),
                other => Err(syn::Error::new(
                    ident.span(),
                    format!("unknown comparator `{other}` — expected `eq`, `any_of`, `range`, or `contains`"),
                )),
            }
        }
        Meta::NameValue(_) => Err(syn::Error::new(
            attr.span(),
            "expected `#[query_param]` or `#[query_param(eq|any_of|range|contains)]`, not `key = value`",
        )),
    }
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
    Ok(())
}

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
    let row_ident = item.ident.clone();
    let module_ident = module_ident_from_name(&args.name)?;
    let name_lit = args.name;
    let schema_version = args.schema_version;

    let params = extract_field_params(&item)?;
    strip_query_param_attrs(&mut item);

    let field_markers = generate_field_markers(&params);
    let matches_body = generate_matches_body(&params);

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
        }
    })
}

fn generate_field_markers(params: &[ParamDef]) -> Vec<TokenStream2> {
    params
        .iter()
        .map(|param| {
            let name = &param.name;
            let name_str = name.to_string();
            let marker_ident = Ident::new(
                &format!("__Field_{}", name_str),
                proc_macro2::Span::call_site(),
            );
            let comparator_impl = match &param.kind {
                ComparatorKind::RequiredEq => {
                    let ty = &param.ty;
                    quote! {
                        impl ::pocopine_sync_query::FieldEq for #marker_ident {
                            type Value = #ty;
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::OptionalEq { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldEq for #marker_ident {
                            type Value = #inner;
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::InSet { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldInSet for #marker_ident {
                            type Item = #inner;
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::Range { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldRange for #marker_ident {
                            type Bound = #inner;
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::Contains => {
                    quote! {
                        impl ::pocopine_sync_query::FieldContains for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
            };
            quote! {
                #[allow(non_camel_case_types)]
                #[doc = concat!("Field marker for `", #name_str, "` — used by the query DSL methods (`.eq`, `.any_of`, `.range`, `.contains`).")]
                #[derive(::std::marker::Copy, ::std::clone::Clone)]
                pub struct #marker_ident;
                // SAFETY: marker is macro-generated and matches the
                // comparator declared in `params(...)`. The `unsafe`
                // here is API-stability, not memory-safety.
                unsafe impl ::pocopine_sync_query::predicate::__SealedFieldMarker for #marker_ident {}
                #comparator_impl
                #[allow(non_upper_case_globals)]
                pub const #name: #marker_ident = #marker_ident;
            }
        })
        .collect()
}

/// Generate the body of `matches(query, row) -> bool`. Each declared
/// param emits a check that returns false on mismatch.
fn generate_matches_body(params: &[ParamDef]) -> TokenStream2 {
    let checks: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            let name_str = name.to_string();
            let field_access: TokenStream2 = quote! { row.#name };
            match &param.kind {
                ComparatorKind::RequiredEq => {
                    let ty = &param.ty;
                    // Required-equality: the param MUST be set. A
                    // query built without the matching
                    // `.eq(field::<name>, ...)` call would otherwise
                    // permissively match every row in the stream —
                    // a cross-tenant data leak the macro must close.
                    quote! {
                        let raw = match params.get(#name_str) {
                            Some(r) => r,
                            None => return false,
                        };
                        let want: #ty = match ::pocopine_sync_query::__private::serde_json::from_value(raw.clone()) {
                            Ok(v) => v,
                            Err(_) => return false,
                        };
                        if #field_access != want { return false; }
                    }
                }
                ComparatorKind::OptionalEq { inner } => {
                    // For Option<T>: when the query specifies it, row.field
                    // must equal Some(want). When the query has it as null,
                    // accept None. When the query omits the key, no check.
                    quote! {
                        if let Some(raw) = params.get(#name_str) {
                            if raw.is_null() {
                                if #field_access.is_some() { return false; }
                            } else {
                                let want: #inner = match ::pocopine_sync_query::__private::serde_json::from_value(raw.clone()) {
                                    Ok(v) => v,
                                    Err(_) => return false,
                                };
                                match &#field_access {
                                    ::std::option::Option::Some(v) if v == &want => {}
                                    _ => return false,
                                }
                            }
                        }
                    }
                }
                ComparatorKind::InSet { inner } => {
                    quote! {
                        if let Some(raw) = params.get(#name_str) {
                            let set: ::pocopine_sync_query::params::InSet<#inner> =
                                match ::pocopine_sync_query::__private::serde_json::from_value(raw.clone()) {
                                    Ok(v) => v,
                                    Err(_) => return false,
                                };
                            if !set.values().iter().any(|v| v == &#field_access) { return false; }
                        }
                    }
                }
                ComparatorKind::Range { inner } => {
                    quote! {
                        if let Some(raw) = params.get(#name_str) {
                            let range: ::pocopine_sync_query::params::Range<#inner> =
                                match ::pocopine_sync_query::__private::serde_json::from_value(raw.clone()) {
                                    Ok(v) => v,
                                    Err(_) => return false,
                                };
                            if !::pocopine_sync_query::predicate::range_contains(&range, &#field_access) {
                                return false;
                            }
                        }
                    }
                }
                ComparatorKind::Contains => {
                    quote! {
                        if let Some(raw) = params.get(#name_str) {
                            let needle: ::pocopine_sync_query::params::Contains =
                                match ::pocopine_sync_query::__private::serde_json::from_value(raw.clone()) {
                                    Ok(v) => v,
                                    Err(_) => return false,
                                };
                            if !::pocopine_sync_query::predicate::contains_matches(&needle, &#field_access) {
                                return false;
                            }
                        }
                    }
                }
            }
        })
        .collect();

    quote! {
        let _ = params; // silence unused if no params declared
        #(#checks)*
    }
}
