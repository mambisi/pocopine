//! Proc-macros for `pocopine-sync-query`.
//!
//! The headline macro is `#[query_resource]`. It attaches to a unit
//! struct that names a queryable resource and emits:
//!
//! * A `Resource::query()` builder yielding `pocopine_sync_query::Query<Row>`.
//! * Per-field zero-sized markers + sealed comparator-trait impls.
//! * A predicate evaluator: `impl Query<Row> { fn matches(&self, row: &Row) -> bool }`.
//! * Constants: `NAME`, `SCHEMA_VERSION`, `STREAM`.
//!
//! Unlike `pocopine-sync-crud-macros`, this macro is decoupled from any
//! particular source impl. The author declares the *shape* of queries
//! that can target this resource; how the server actually serves those
//! queries (CrudSource adapter, raw SyncStreamSource impl) is the
//! author's concern.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, GenericArgument, Ident, ItemStruct, LitInt, LitStr, PathArguments,
    PathSegment, Token, Type, TypePath,
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
    row: Type,
    schema_version: u32,
    params: Vec<ParamDef>,
}

impl Parse for QueryResourceArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut row: Option<Type> = None;
        let mut schema_version: Option<u32> = None;
        let mut params: Vec<ParamDef> = Vec::new();
        let mut params_set = false;

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            if key == "name" {
                if name.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `name`"));
                }
                input.parse::<Token![=]>()?;
                name = Some(input.parse()?);
            } else if key == "row" {
                if row.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `row`"));
                }
                input.parse::<Token![=]>()?;
                row = Some(input.parse()?);
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
            } else if key == "params" {
                if params_set {
                    return Err(syn::Error::new(key.span(), "duplicate `params(...)`"));
                }
                params_set = true;
                let content;
                let paren = syn::parenthesized!(content in input);
                while !content.is_empty() {
                    let field_name: Ident = content.parse()?;
                    if params.iter().any(|p| p.name == field_name) {
                        return Err(syn::Error::new(
                            field_name.span(),
                            format!(
                                "duplicate param field `{field_name}` in params(...) — each field name must be unique"
                            ),
                        ));
                    }
                    content.parse::<Token![:]>()?;
                    let field_type: Type = content.parse()?;
                    let kind = classify_param_type(&field_type)?;
                    params.push(ParamDef {
                        name: field_name,
                        ty: field_type,
                        kind,
                    });
                    if content.peek(Token![,]) {
                        content.parse::<Token![,]>()?;
                    } else if !content.is_empty() {
                        return Err(content.error("expected `,` between params(...) entries"));
                    }
                }
                if params.is_empty() {
                    return Err(syn::Error::new(
                        paren.span.join(),
                        "`params(...)` requires at least one field; drop the empty parens to omit shape params",
                    ));
                }
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `name = \"...\"`, `row = Type`, `schema_version = N`, or `params(field: Type, ...)`",
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
        let row =
            row.ok_or_else(|| input.error("expected `row = Type` naming the query row type"))?;
        let schema_version = schema_version.unwrap_or(1);

        Ok(Self {
            name,
            row,
            schema_version,
            params,
        })
    }
}

/// Map a declared params field type to its comparator semantics.
///
/// Same pattern as the reference CRUD macro: comparator wrappers
/// (`InSet`, `Range`, `Contains`) must come from
/// `pocopine_sync_query::params::*` (or its `params::*` re-export via
/// `use`). Anything else falls through to `RequiredEq`.
fn classify_param_type(ty: &Type) -> syn::Result<ComparatorKind> {
    let Type::Path(type_path) = ty else {
        return Ok(ComparatorKind::RequiredEq);
    };
    let Some(last) = type_path.path.segments.last() else {
        return Ok(ComparatorKind::RequiredEq);
    };
    match last.ident.to_string().as_str() {
        "Option" => match extract_single_generic_type(last) {
            Some(inner) => Ok(ComparatorKind::OptionalEq { inner }),
            None => Err(syn::Error::new(
                last.span(),
                "`Option<T>` requires exactly one generic type argument",
            )),
        },
        "InSet" => {
            ensure_pocopine_params_prefix(type_path, "InSet")?;
            match extract_single_generic_type(last) {
                Some(inner) => Ok(ComparatorKind::InSet { inner }),
                None => Err(syn::Error::new(
                    last.span(),
                    "`InSet<T>` requires exactly one generic type argument",
                )),
            }
        }
        "Range" => {
            ensure_pocopine_params_prefix(type_path, "Range")?;
            match extract_single_generic_type(last) {
                Some(inner) => Ok(ComparatorKind::Range { inner }),
                None => Err(syn::Error::new(
                    last.span(),
                    "`Range<T>` requires exactly one generic type argument",
                )),
            }
        }
        "Contains" => {
            ensure_pocopine_params_prefix(type_path, "Contains")?;
            if !matches!(last.arguments, PathArguments::None) {
                return Err(syn::Error::new(
                    last.span(),
                    "`Contains` takes no generic arguments",
                ));
            }
            Ok(ComparatorKind::Contains)
        }
        _ => Ok(ComparatorKind::RequiredEq),
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

fn ensure_pocopine_params_prefix(type_path: &TypePath, wrapper: &str) -> syn::Result<()> {
    let segments = &type_path.path.segments;
    if segments.len() < 2 {
        return Err(syn::Error::new(
            type_path.path.span(),
            format!(
                "bare `{wrapper}` is ambiguous — write `params::{wrapper}` (with `use pocopine_sync_query::params;`)"
            ),
        ));
    }
    let parent = &segments[segments.len() - 2];
    if parent.ident != "params" {
        return Err(syn::Error::new(
            type_path.path.span(),
            format!(
                "`{}::{wrapper}` is not a pocopine comparator wrapper — write `params::{wrapper}`",
                parent.ident
            ),
        ));
    }
    // Fully-qualified paths must root in pocopine_sync_query.
    if segments.len() >= 3 {
        let root_ident = &segments[0].ident;
        let crate_root = if root_ident == "pocopine_sync_query" {
            true
        } else if segments.len() >= 4 && root_ident == "crate" {
            segments[1].ident == "pocopine_sync_query"
        } else {
            false
        };
        if !crate_root {
            return Err(syn::Error::new(
                type_path.path.span(),
                format!(
                    "fully-qualified `{wrapper}` path must start with `pocopine_sync_query::params::{wrapper}`"
                ),
            ));
        }
    }
    Ok(())
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

/// `#[query_resource(name = "issues", row = Issue, params(...))]` on a
/// unit struct generates the query DSL surface for that resource.
#[proc_macro_attribute]
pub fn query_resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as QueryResourceArgs);
    let item = parse_macro_input!(item as ItemStruct);

    match expand_query_resource(args, item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_query_resource(args: QueryResourceArgs, item: ItemStruct) -> syn::Result<TokenStream2> {
    let resource_ident = item.ident.clone();
    let module_ident = module_ident_from_name(&args.name)?;
    let name_lit = args.name;
    let row_ty = args.row;
    let schema_version = args.schema_version;
    let params = args.params;

    let field_markers = generate_field_markers(&params);
    let matches_body = generate_matches_body(&params, &row_ty);
    let builder_methods = generate_builder_methods(&params);

    Ok(quote! {
        #item

        impl #resource_ident {
            /// Build a typed query for this resource.
            pub fn query() -> #module_ident::QueryBuilder {
                #module_ident::QueryBuilder::new()
            }
        }

        pub mod #module_ident {
            // We pull the caller's scope into this module so the
            // user's row type and per-field comparator types
            // (declared in `params(...)`) resolve here. The glob is
            // OK because Rust resolves locally-defined items in this
            // module ahead of glob imports — so the macro's own
            // `Row` alias, `QueryBuilder`, `field` submodule, and
            // `matches` fn always shadow anything brought in from
            // `super`, regardless of identifier collisions in caller
            // scope. `#[allow(unused_imports)]` silences the lint for
            // callers that don't import anything reusable.
            #[allow(unused_imports)]
            use super::*;

            pub const NAME: &str = #name_lit;
            pub const SCHEMA_VERSION: u32 = #schema_version;
            pub type Row = #row_ty;

            /// Typed query builder. Each `where_*` method requires the
            /// matching `field::*` marker; misuse fails to compile via
            /// the sealed comparator-trait gate in `pocopine_sync_query`.
            pub struct QueryBuilder {
                inner: ::pocopine_sync_query::QueryBuilder<Row>,
            }

            impl QueryBuilder {
                pub fn new() -> Self {
                    let stream = ::pocopine_sync_query::SyncStreamName::new(NAME)
                        .expect("query_resource name passed validation");
                    Self {
                        inner: ::pocopine_sync_query::Query::<Row>::builder(stream)
                            .with_matches(self::matches),
                    }
                }

                #(#builder_methods)*

                pub fn order_by(
                    mut self,
                    field: impl ::std::convert::Into<::std::string::String>,
                    direction: ::pocopine_sync_query::Order,
                ) -> Self {
                    self.inner = self.inner.order_by(field, direction);
                    self
                }

                pub fn limit(mut self, limit: u32) -> Self {
                    self.inner = self.inner.limit(limit);
                    self
                }

                pub fn build(self) -> ::pocopine_sync_query::Query<Row> {
                    self.inner.build()
                }

                /// Subscribe to this query through `client`. Returns a
                /// reactive [`QueryView`] that holds the subscription
                /// alive until dropped. Equivalent to
                /// `client.observe(self.build())` but reads better at
                /// the call site:
                ///
                /// ```ignore
                /// let view = Issues::query()
                ///     .workspace_id(w1)
                ///     .observe(&client);
                /// ```
                pub fn observe(
                    self,
                    client: &::pocopine_sync_query::QueryClient,
                ) -> ::pocopine_sync_query::QueryView<Row> {
                    client.observe(self.build())
                }
            }

            impl Default for QueryBuilder {
                fn default() -> Self {
                    Self::new()
                }
            }

            /// Field markers for the type-safe query DSL. One marker
            /// per declared param; each impls exactly the comparator
            /// trait matching its declared shape.
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
                        impl ::pocopine_sync_query::FieldEq<#ty> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::OptionalEq { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldEq<#inner> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::InSet { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldInSet<#inner> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::Range { inner } => {
                    quote! {
                        impl ::pocopine_sync_query::FieldRange<#inner> for #marker_ident {
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
                #[doc = concat!("Field marker for `", #name_str, "` — used by the query DSL's `where_*` methods.")]
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

fn generate_builder_methods(params: &[ParamDef]) -> Vec<TokenStream2> {
    params
        .iter()
        .map(|param| {
            let name = &param.name;
            let name_str = name.to_string();
            match &param.kind {
                ComparatorKind::RequiredEq => {
                    let ty = &param.ty;
                    quote! {
                        pub fn #name(mut self, value: #ty) -> Self
                        where
                            #ty: ::pocopine_sync_query::__private::serde::Serialize,
                        {
                            let encoded = ::pocopine_sync_query::__private::serde_json::to_value(&value)
                                .expect("RequiredEq field encodes successfully");
                            self.inner = self.inner.raw_param(#name_str, encoded);
                            self
                        }
                    }
                }
                ComparatorKind::OptionalEq { inner } => {
                    quote! {
                        pub fn #name(mut self, value: #inner) -> Self
                        where
                            #inner: ::pocopine_sync_query::__private::serde::Serialize,
                        {
                            let encoded = ::pocopine_sync_query::__private::serde_json::to_value(&value)
                                .expect("OptionalEq field encodes successfully");
                            self.inner = self.inner.raw_param(#name_str, encoded);
                            self
                        }
                    }
                }
                ComparatorKind::InSet { inner } => {
                    let method_name = Ident::new(&format!("{name_str}_in"), name.span());
                    quote! {
                        pub fn #method_name<I>(mut self, values: I) -> ::pocopine_sync::SyncResult<Self>
                        where
                            I: ::std::iter::IntoIterator<Item = #inner>,
                            #inner: ::pocopine_sync_query::__private::serde::Serialize,
                        {
                            let set = ::pocopine_sync_query::params::InSet::<#inner>::new(values)
                                .map_err(|e| ::pocopine_sync::SyncError::client(e.to_string()))?;
                            let encoded = ::pocopine_sync_query::__private::serde_json::to_value(&set)
                                .map_err(|e| ::pocopine_sync::SyncError::client(e.to_string()))?;
                            self.inner = self.inner.raw_param(#name_str, encoded);
                            Ok(self)
                        }
                    }
                }
                ComparatorKind::Range { inner } => {
                    let method_name = Ident::new(&format!("{name_str}_range"), name.span());
                    quote! {
                        pub fn #method_name(
                            mut self,
                            range: ::pocopine_sync_query::params::Range<#inner>,
                        ) -> ::pocopine_sync::SyncResult<Self>
                        where
                            #inner: ::pocopine_sync_query::__private::serde::Serialize,
                        {
                            let encoded = ::pocopine_sync_query::__private::serde_json::to_value(&range)
                                .map_err(|e| ::pocopine_sync::SyncError::client(e.to_string()))?;
                            self.inner = self.inner.raw_param(#name_str, encoded);
                            Ok(self)
                        }
                    }
                }
                ComparatorKind::Contains => {
                    let method_name = Ident::new(&format!("{name_str}_contains"), name.span());
                    quote! {
                        pub fn #method_name(
                            mut self,
                            needle: impl ::std::convert::Into<::std::string::String>,
                        ) -> ::pocopine_sync::SyncResult<Self> {
                            let contains = ::pocopine_sync_query::params::Contains::icontains(needle)
                                .map_err(|e| ::pocopine_sync::SyncError::client(e.to_string()))?;
                            let encoded = ::pocopine_sync_query::__private::serde_json::to_value(&contains)
                                .map_err(|e| ::pocopine_sync::SyncError::client(e.to_string()))?;
                            self.inner = self.inner.raw_param(#name_str, encoded);
                            Ok(self)
                        }
                    }
                }
            }
        })
        .collect()
}

/// Generate the body of `matches(query, row) -> bool`. Each declared
/// param emits a check that returns false on mismatch.
fn generate_matches_body(params: &[ParamDef], _row_ty: &Type) -> TokenStream2 {
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
                    // query built without the setter (e.g.
                    // `Issues::query().build()` with no
                    // `.workspace_id(...)` call) would otherwise
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
