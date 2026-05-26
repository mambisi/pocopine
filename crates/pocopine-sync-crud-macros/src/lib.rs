//! Proc-macros for `pocopine-sync-crud`.
//!
//! The macro reads a server-side `CrudSource` impl and generates the shared
//! resource module that client and server code use.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    parse_macro_input, ExprPath, GenericArgument, Ident, ImplItem, ItemImpl, LitInt, LitStr,
    PathArguments, PathSegment, Token, Type,
};

// Matches the sync protocol's stream and row-key token budget.
const MAX_SYNC_TOKEN_LEN: usize = 1024;

/// Inferred comparator kind for one entry in `params(...)`.
#[derive(Clone, Debug)]
enum ComparatorKind {
    /// Bare `T` — required equality.
    RequiredEq,
    /// `Option<T>` — optional equality. The inner `T` is the value
    /// type used in the builder setter signature.
    OptionalEq { inner: Type },
    /// `params::InSet<T>` — membership in a non-empty set.
    InSet { inner: Type },
    /// `params::Range<T>` — bounded range.
    Range { inner: Type },
    /// `params::Contains` — substring match against a text field.
    Contains,
}

/// One field declared in `#[resource(params(...))]`.
#[derive(Clone, Debug)]
struct ParamDef {
    /// Field identifier as written by the author (e.g. `workspace_id`).
    name: Ident,
    /// Type the author wrote (used for the public `XxxStreamParams`
    /// struct field type).
    ty: Type,
    /// Inferred comparator semantics + the inner value type when
    /// applicable.
    kind: ComparatorKind,
}

#[derive(Debug)]
struct ResourceArgs {
    name: LitStr,
    module: Ident,
    schema_version: u32,
    migrate_with: Option<ExprPath>,
    params: Vec<ParamDef>,
}

/// Pull the single generic-argument type out of `Wrapper<T>`. Returns
/// `None` when the segment carries no angle-bracketed args or carries
/// more than one type arg.
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

/// Map a declared params field type to its comparator semantics.
///
/// Classification is by **last path segment** + generic-arg count:
///
/// | Last segment + arity      | Inferred kind                            |
/// |---------------------------|------------------------------------------|
/// | `Option<T>`               | `OptionalEq { inner: T }`                |
/// | `InSet<T>` (any prefix)   | `InSet { inner: T }`                     |
/// | `Range<T>` (any prefix)   | `Range { inner: T }`                     |
/// | `Contains` (no args)      | `Contains`                               |
/// | any other path / shape    | `RequiredEq`                             |
///
/// Authors who want a `std::ops::Range<T>` as a required-eq value
/// must alias it (`use std::ops::Range as StdRange`) or accept the
/// `Range` classification.
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
        "InSet" => match extract_single_generic_type(last) {
            Some(inner) => Ok(ComparatorKind::InSet { inner }),
            None => Err(syn::Error::new(
                last.span(),
                "`InSet<T>` requires exactly one generic type argument; use `params::InSet<Status>` or similar",
            )),
        },
        "Range" => match extract_single_generic_type(last) {
            Some(inner) => Ok(ComparatorKind::Range { inner }),
            None => Err(syn::Error::new(
                last.span(),
                "`Range<T>` requires exactly one generic type argument; use `params::Range<DateTime>` or similar",
            )),
        },
        "Contains" => {
            if !matches!(last.arguments, PathArguments::None) {
                return Err(syn::Error::new(
                    last.span(),
                    "`Contains` takes no generic arguments — use `params::Contains` directly",
                ));
            }
            Ok(ComparatorKind::Contains)
        }
        _ => Ok(ComparatorKind::RequiredEq),
    }
}

impl Parse for ResourceArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut module = None;
        let mut schema_version: Option<u32> = None;
        let mut migrate_with: Option<ExprPath> = None;
        let mut params: Vec<ParamDef> = Vec::new();
        let mut params_set = false;

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            if key == "name" {
                if name.is_some() {
                    return Err(syn::Error::new(
                        key.span(),
                        "duplicate `name` in CRUD resource attribute",
                    ));
                }
                input.parse::<Token![=]>()?;
                name = Some(input.parse()?);
            } else if key == "module" {
                if module.is_some() {
                    return Err(syn::Error::new(
                        key.span(),
                        "duplicate `module` in CRUD resource attribute",
                    ));
                }
                input.parse::<Token![=]>()?;
                module = Some(input.parse()?);
            } else if key == "schema_version" {
                if schema_version.is_some() {
                    return Err(syn::Error::new(
                        key.span(),
                        "duplicate `schema_version` in CRUD resource attribute",
                    ));
                }
                input.parse::<Token![=]>()?;
                let lit: LitInt = input.parse().map_err(|err| {
                    syn::Error::new(
                        err.span(),
                        "`schema_version` must be a u32 integer literal (e.g. `schema_version = 2`)",
                    )
                })?;
                // `base10_parse::<u32>` silently strips type suffixes —
                // `schema_version = 3u64` would otherwise parse as `3`.
                // Reject anything that's not bare or explicitly `u32`.
                let suffix = lit.suffix();
                if !suffix.is_empty() && suffix != "u32" {
                    return Err(syn::Error::new(
                        lit.span(),
                        format!(
                            "`schema_version` must be a bare integer literal or `u32`-suffixed (got: {lit})"
                        ),
                    ));
                }
                let value: u32 = lit.base10_parse().map_err(|err| {
                    syn::Error::new(
                        lit.span(),
                        format!("`schema_version` must fit in u32 (got: {lit}): {err}"),
                    )
                })?;
                if value == 0 {
                    return Err(syn::Error::new(
                        lit.span(),
                        "schema_version must be >= 1 (schema versions start at 1)",
                    ));
                }
                schema_version = Some(value);
            } else if key == "migrate_with" {
                if migrate_with.is_some() {
                    return Err(syn::Error::new(
                        key.span(),
                        "duplicate `migrate_with` in CRUD resource attribute",
                    ));
                }
                input.parse::<Token![=]>()?;
                let path: ExprPath = input.parse().map_err(|err| {
                    syn::Error::new(
                        err.span(),
                        "`migrate_with` must be a function path (e.g. `migrate_with = my_app::migrate_post`)",
                    )
                })?;
                migrate_with = Some(path);
            } else if key == "params" {
                if params_set {
                    return Err(syn::Error::new(
                        key.span(),
                        "duplicate `params(...)` in CRUD resource attribute",
                    ));
                }
                params_set = true;
                // `params(field: Type, field: Type, ...)`. Parenthesized
                // list; entries are `Ident: Type` pairs. Comparator
                // semantics are inferred from `Type` via
                // `classify_param_type`.
                let content;
                syn::parenthesized!(content in input);
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
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `name = \"...\"`, `module = ident`, `schema_version = N`, `migrate_with = path`, or `params(field: Type, ...)` in CRUD resource attribute",
                ));
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else if !input.is_empty() {
                return Err(input.error("expected `,` between CRUD resource attribute options"));
            }
        }

        let name = name
            .ok_or_else(|| input.error("expected `name = \"...\"` in CRUD resource attribute"))?;
        validate_resource_name(&name)?;
        let module = match module {
            Some(module) => module,
            None => module_ident_from_name(&name)?,
        };
        // `migrate_with = path` registers a migrator that only fires
        // when the client's wire `schema_version` is strictly LESS
        // than the server's declared `schema_version`. With
        // `schema_version = 1` (the default), they always match and
        // the migrator is dead code. Force the author to either bump
        // `schema_version` or drop `migrate_with`.
        if let Some(path) = migrate_with.as_ref() {
            if schema_version.unwrap_or(1) < 2 {
                return Err(syn::Error::new(
                    path.span(),
                    "`migrate_with` requires `schema_version = N` with N >= 2; a v1 migrator is never invoked because clients also default to v1",
                ));
            }
        }
        let schema_version = schema_version.unwrap_or(1);
        Ok(Self {
            name,
            module,
            schema_version,
            migrate_with,
            params,
        })
    }
}

#[derive(Debug)]
struct ResourceTypes {
    id: Type,
    row: Type,
    draft: Type,
}

/// Mark a `CrudSource` impl as a typed CRUD resource.
#[proc_macro_attribute]
pub fn resource(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ResourceArgs);
    let item = parse_macro_input!(item as ItemImpl);

    if !impl_targets_crud_source(&item) {
        return syn::Error::new(
            item.impl_token.span,
            "`#[pocopine_sync_crud::resource]` must be attached to an impl of `CrudSource`",
        )
        .to_compile_error()
        .into();
    }

    match expand_resource(args, item) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

fn expand_resource(args: ResourceArgs, item: ItemImpl) -> syn::Result<TokenStream2> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.span(),
            "`#[pocopine_sync_crud::resource]` does not support generic `CrudSource` impls yet",
        ));
    }

    let types = extract_resource_types(&item)?;
    let module = args.module;
    let name = args.name;
    let schema_version = args.schema_version;
    let source = &item.self_ty;
    let id = types.id;
    let row = types.row;
    let draft = types.draft;
    // `migrate_with = path` -> tokens to chain `.migrate_with(path)` on
    // the builder. When omitted, expand to nothing so the existing
    // `resource(NAME, source)? .schema_version(SCHEMA_VERSION)` chain is
    // unchanged.
    let migrate_with_chain = match args.migrate_with {
        Some(path) => quote! { .map(|b| b.migrate_with(#path)) },
        None => quote! {},
    };
    let params_codegen = generate_params_codegen(&args.params);

    Ok(quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #item

        pub mod #module {
            #[allow(unused_imports)]
            use super::*;

            pub const NAME: &str = #name;
            /// Application-level schema version this resource serves.
            /// Bumped via `#[resource(schema_version = N)]` whenever the
            /// row, draft, or payload shape changes in a way that older
            /// cached data or queued mutations cannot deserialize into.
            pub const SCHEMA_VERSION: u32 = #schema_version;

            pub type Id = #id;
            pub type Row = #row;
            pub type Draft = #draft;

            pub type CreateOptions = ::pocopine_sync_crud::CreateOptions<Row>;
            pub type SaveOptions = ::pocopine_sync_crud::SaveOptions<Row>;
            pub type RemoveOptions = ::pocopine_sync_crud::RemoveOptions;
            pub type View = ::pocopine_sync_crud::LocalResourceView<Id, Row>;
            pub type ViewState = ::pocopine_sync_crud::LocalResourceViewState<Id, Row>;
            pub type Outcome = ::pocopine_sync_crud::CrudOutcome<Id, Row>;
            pub type Queued = ::pocopine_sync_crud::Queued<Id>;
            pub type Client<C> = ::pocopine_sync_crud::CrudClientResource<C, Id, Row>;

            pub struct Resource<C: 'static> {
                sync: ::pocopine_sync::SyncClient,
                handle: ::pocopine_sync::Handle<C>,
                selector: ::pocopine_sync::CollectionSelector<C, Row>,
            }

            impl<C: 'static> Clone for Resource<C> {
                fn clone(&self) -> Self {
                    Self {
                        sync: self.sync.clone(),
                        handle: self.handle.clone(),
                        selector: self.selector,
                    }
                }
            }

            impl<C: 'static> Resource<C>
            where
                Row: 'static,
            {
                pub fn new(
                    sync: &::pocopine_sync::SyncClient,
                    handle: ::pocopine_sync::Handle<C>,
                    selector: ::pocopine_sync::CollectionSelector<C, Row>,
                ) -> Self {
                    Self {
                        sync: sync.clone(),
                        handle,
                        selector,
                    }
                }

                pub fn collection(
                    &self,
                ) -> ::pocopine_sync::SyncResult<::pocopine_sync::SyncCollection<C, Row>> {
                    self.sync
                        .collection(self.handle.clone(), self.selector)
                        .stream(NAME)?
                        .schema_version(SCHEMA_VERSION)
                }

                pub fn open(&self) -> ::pocopine_sync::SyncResult<()>
                where
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.collection()?.open()
                }

                pub fn pull(&self) -> ::pocopine_sync::SyncResult<()>
                where
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.collection()?.pull()
                }

                pub fn view(
                    &self,
                ) -> ::pocopine_sync::SyncResult<View>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone,
                {
                    let mut state = self.handle.try_borrow_mut().map_err(|_| {
                        ::pocopine_sync::SyncError::client(
                            "sync CRUD resource state is already borrowed",
                        )
                    })?;
                    view((self.selector)(&mut state))
                }

                pub fn observe_view<F>(&self, callback: F) -> ::pocopine_sync::SyncResult<()>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone,
                    F: Fn(&ViewState, Option<&ViewState>) + 'static,
                {
                    ::pocopine_sync_crud::observe_local_resource_view::<C, Id, Row, F>(
                        self.handle.clone(),
                        self.selector,
                        callback,
                    )
                }

                pub fn client(&self) -> ::pocopine_sync::SyncResult<Client<C>>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone,
                {
                    let collection = self.collection()?;
                    let view = {
                        let mut state = self.handle.try_borrow_mut().map_err(|_| {
                            ::pocopine_sync::SyncError::client(
                                "sync CRUD resource state is already borrowed",
                            )
                        })?;
                        view((self.selector)(&mut state))?
                    };
                    Ok(::pocopine_sync_crud::client_resource(collection, view))
                }

                pub async fn create(&self, id: Id, draft: Draft) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.create(id, draft).await
                }

                pub async fn create_with_options(
                    &self,
                    id: Id,
                    draft: Draft,
                    options: CreateOptions,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.create_with_options(id, draft, options).await
                }

                pub async fn save(&self, id: Id, draft: Draft) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.save(id, draft).await
                }

                pub async fn save_with_options(
                    &self,
                    id: Id,
                    draft: Draft,
                    options: SaveOptions,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.save_with_options(id, draft, options).await
                }

                pub async fn remove(&self, id: Id) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.client()?.remove(id).await
                }

                pub async fn remove_with_options(
                    &self,
                    id: Id,
                    options: RemoveOptions,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.client()?.remove_with_options(id, options).await
                }

                pub async fn use_server(&self, id: Id) -> ::pocopine_sync::SyncResult<bool>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.client()?.use_server(id).await
                }

                pub async fn discard_local(&self, id: Id) -> ::pocopine_sync::SyncResult<bool>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                {
                    self.client()?.discard_local(id).await
                }

                pub async fn retry_local(
                    &self,
                    id: Id,
                    draft: Draft,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.retry_local(id, draft).await
                }

                pub async fn retry_local_with_options(
                    &self,
                    id: Id,
                    draft: Draft,
                    options: SaveOptions,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.retry_local_with_options(id, draft, options).await
                }

                pub async fn merge_with(
                    &self,
                    id: Id,
                    draft: Draft,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.merge_with(id, draft).await
                }

                pub async fn merge_with_options(
                    &self,
                    id: Id,
                    draft: Draft,
                    options: SaveOptions,
                ) -> ::pocopine_sync::SyncResult<Outcome>
                where
                    Id: ::pocopine_sync_crud::ResourceId,
                    Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                    Draft: ::serde::Serialize + 'static,
                {
                    self.client()?.merge_with_options(id, draft, options).await
                }
            }

            pub fn use_resource<C: 'static>(
                sync: &::pocopine_sync::SyncClient,
                handle: ::pocopine_sync::Handle<C>,
                selector: ::pocopine_sync::CollectionSelector<C, Row>,
            ) -> Resource<C>
            where
                Row: 'static,
            {
                Resource::new(sync, handle, selector)
            }

            #[cfg(not(target_arch = "wasm32"))]
            pub fn resource(
                source: #source,
            ) -> ::pocopine_sync::SyncResult<::pocopine_sync_crud::CrudResourceBuilder<#source>> {
                ::pocopine_sync_crud::resource(NAME, source)?
                    .schema_version(SCHEMA_VERSION)
                    #migrate_with_chain
            }

            pub fn new_id() -> ::pocopine_sync::SyncResult<Id> {
                ::pocopine_sync_crud::new_id()
            }

            pub fn view(
                state: &::pocopine_sync::CollectionState<Row>,
            ) -> ::pocopine_sync::SyncResult<View> {
                ::pocopine_sync_crud::local_resource_view(state)
            }

            pub fn client<C: 'static>(
                collection: ::pocopine_sync::SyncCollection<C, Row>,
                state: &::pocopine_sync::CollectionState<Row>,
            ) -> ::pocopine_sync::SyncResult<Client<C>> {
                let view = view(state)?;
                Ok(::pocopine_sync_crud::client_resource(collection, view))
            }

            pub fn collection<C: 'static>(
                sync: &::pocopine_sync::SyncClient,
                handle: ::pocopine_sync::Handle<C>,
                selector: ::pocopine_sync::CollectionSelector<C, Row>,
            ) -> ::pocopine_sync::SyncResult<::pocopine_sync::SyncCollection<C, Row>>
            where
                Row: 'static,
            {
                sync.collection(handle, selector)
                    .stream(NAME)?
                    .schema_version(SCHEMA_VERSION)
            }

            #params_codegen
        }
    })
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
            "invalid CRUD resource name: expected a non-empty sync token no longer than 1024 bytes without leading/trailing whitespace or control characters",
        ));
    }
    Ok(())
}

fn module_ident_from_name(name: &LitStr) -> syn::Result<Ident> {
    syn::parse_str::<Ident>(&name.value()).map_err(|_| {
        syn::Error::new(
            name.span(),
            "CRUD resource name is not a Rust module identifier; add `module = ident`",
        )
    })
}

/// Generate the typed `StreamParams` struct + fluent stream builder
/// for the given `params(...)` declarations. When no params are
/// declared, expand to nothing — non-opt-in resources keep their
/// existing surface unchanged.
fn generate_params_codegen(params: &[ParamDef]) -> TokenStream2 {
    if params.is_empty() {
        return quote! {};
    }

    // Field declarations for the public `StreamParams` struct. Each
    // entry uses the author-declared type verbatim (e.g.
    // `params::InSet<Status>`, `Option<UserId>`, `WorkspaceId`).
    let struct_fields: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            let ty = &param.ty;
            quote! { pub #name: #ty, }
        })
        .collect();

    // `serialize_params` walks every declared field and folds it into
    // the wire `StreamParams` map (`BTreeMap<String, Value>`).
    // Optional fields are omitted when `None`; all other kinds always
    // emit their key.
    let serialize_lines: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            let name_str = name.to_string();
            match &param.kind {
                ComparatorKind::OptionalEq { .. } => {
                    quote! {
                        if let ::std::option::Option::Some(value) = &self.#name {
                            map.insert(
                                #name_str.to_string(),
                                ::serde_json::to_value(value)
                                    .expect("StreamParams::serialize_params: encode field"),
                            );
                        }
                    }
                }
                _ => {
                    quote! {
                        map.insert(
                            #name_str.to_string(),
                            ::serde_json::to_value(&self.#name)
                                .expect("StreamParams::serialize_params: encode field"),
                        );
                    }
                }
            }
        })
        .collect();

    // Known-keys allow-list for `extract`. Unknown keys reject so a
    // client typo can't silently fall through.
    let known_keys_match_arms: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name_str = param.name.to_string();
            quote! { #name_str }
        })
        .collect();

    // Per-field extraction inside `extract`. `OptionalEq` returns the
    // inner `T` wrapped in `Some`; absent → `None`. Required fields
    // (`RequiredEq`, `InSet`, `Range`, `Contains`) return an error
    // when missing.
    let extract_locals: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            let name_str = name.to_string();
            let ty = &param.ty;
            let field_label = format!("params.{name_str}");
            let missing_label = format!("missing required param `{name_str}`");
            match &param.kind {
                ComparatorKind::OptionalEq { inner } => {
                    quote! {
                        let #name: #ty = match params.get(#name_str) {
                            Some(raw) => Some(
                                ::serde_json::from_value::<#inner>(raw.clone()).map_err(|err| {
                                    ::pocopine_sync::SyncError::invalid_value(
                                        #field_label,
                                        err.to_string(),
                                    )
                                })?,
                            ),
                            None => None,
                        };
                    }
                }
                _ => {
                    quote! {
                        let #name: #ty = ::serde_json::from_value(
                            params
                                .get(#name_str)
                                .ok_or_else(|| {
                                    ::pocopine_sync::SyncError::invalid_value(
                                        "params",
                                        #missing_label,
                                    )
                                })?
                                .clone(),
                        )
                        .map_err(|err| {
                            ::pocopine_sync::SyncError::invalid_value(#field_label, err.to_string())
                        })?;
                    }
                }
            }
        })
        .collect();

    let extract_struct_fields: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            quote! { #name, }
        })
        .collect();

    // Fluent builder fields. Every declared field is tracked as
    // `Option<value>` regardless of comparator kind — this lets us
    // detect "required-but-not-set" at `observe()` time.
    let builder_fields: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            let value_ty = builder_value_type(param);
            quote! { #name: ::std::option::Option<#value_ty>, }
        })
        .collect();

    // One setter per field. Method-name shape and argument type
    // depend on the comparator kind:
    let builder_setters: Vec<TokenStream2> = params.iter().map(|param| {
        let name = &param.name;
        let name_str = name.to_string();
        match &param.kind {
            ComparatorKind::RequiredEq => {
                let ty = &param.ty;
                quote! {
                    pub fn #name(mut self, value: #ty) -> Self {
                        self.#name = ::std::option::Option::Some(value);
                        self
                    }
                }
            }
            ComparatorKind::OptionalEq { inner } => {
                quote! {
                    pub fn #name(mut self, value: #inner) -> Self {
                        self.#name = ::std::option::Option::Some(value);
                        self
                    }
                }
            }
            ComparatorKind::InSet { inner } => {
                let setter_name = Ident::new(&format!("{name_str}_in"), name.span());
                quote! {
                    pub fn #setter_name<I>(mut self, values: I) -> ::pocopine_sync::SyncResult<Self>
                    where
                        I: ::std::iter::IntoIterator<Item = #inner>,
                    {
                        let set = ::pocopine_sync_crud::params::InSet::<#inner>::new(values)
                            .map_err(|err| {
                                ::pocopine_sync::SyncError::client(err.to_string())
                            })?;
                        self.#name = ::std::option::Option::Some(set);
                        Ok(self)
                    }
                }
            }
            ComparatorKind::Range { inner } => {
                let setter_name = Ident::new(&format!("{name_str}_range"), name.span());
                quote! {
                    pub fn #setter_name(
                        mut self,
                        range: ::pocopine_sync_crud::params::Range<#inner>,
                    ) -> Self {
                        self.#name = ::std::option::Option::Some(range);
                        self
                    }
                }
            }
            ComparatorKind::Contains => {
                let setter_name = Ident::new(&format!("{name_str}_contains"), name.span());
                quote! {
                    pub fn #setter_name(
                        mut self,
                        needle: impl ::std::convert::Into<::std::string::String>,
                    ) -> ::pocopine_sync::SyncResult<Self> {
                        let needle = ::pocopine_sync_crud::params::Contains::icontains(needle)
                            .map_err(|err| {
                                ::pocopine_sync::SyncError::client(err.to_string())
                            })?;
                        self.#name = ::std::option::Option::Some(needle);
                        Ok(self)
                    }
                }
            }
        }
    })
        .collect();

    // `into_params()` produces the wire `StreamParams` map. Required
    // fields not set return an error; optional fields default to `None`.
    let into_params_lines: Vec<TokenStream2> = params.iter().map(|param| {
        let name = &param.name;
        let name_str = name.to_string();
        let missing = format!("StreamParams::into_params: required field `{name_str}` not set");
        match &param.kind {
            ComparatorKind::OptionalEq { .. } => {
                quote! {
                    if let ::std::option::Option::Some(value) = self.#name {
                        map.insert(
                            #name_str.to_string(),
                            ::serde_json::to_value(value)
                                .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?,
                        );
                    }
                }
            }
            _ => {
                quote! {
                    {
                        let value = self.#name.ok_or_else(|| {
                            ::pocopine_sync::SyncError::client(#missing)
                        })?;
                        map.insert(
                            #name_str.to_string(),
                            ::serde_json::to_value(value)
                                .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?,
                        );
                    }
                }
            }
        }
    })
        .collect();

    let builder_init_fields: Vec<TokenStream2> = params
        .iter()
        .map(|param| {
            let name = &param.name;
            quote! { #name: ::std::option::Option::None, }
        })
        .collect();

    // Field-marker tokens for the type-safe query DSL. Each declared
    // param gets a zero-sized marker struct + a `const` of that type.
    // The marker implements EXACTLY ONE comparator trait — the one
    // matching its declared shape. `.where_eq` / `.where_in` /
    // `.where_range` / `.where_contains` on the query builder are
    // generic over those traits, so calling the wrong setter for a
    // given field fails to compile.
    let field_markers: Vec<TokenStream2> = params
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
                        impl ::pocopine_sync_crud::query::FieldEq<#ty> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::OptionalEq { inner } => {
                    quote! {
                        impl ::pocopine_sync_crud::query::FieldEq<#inner> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::InSet { inner } => {
                    quote! {
                        impl ::pocopine_sync_crud::query::FieldInSet<#inner> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::Range { inner } => {
                    quote! {
                        impl ::pocopine_sync_crud::query::FieldRange<#inner> for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
                ComparatorKind::Contains => {
                    quote! {
                        impl ::pocopine_sync_crud::query::FieldContains for #marker_ident {
                            const NAME: &'static str = #name_str;
                        }
                    }
                }
            };
            quote! {
                #[allow(non_camel_case_types)]
                #[doc = concat!("Field marker for `", #name_str, "` — used by the query DSL's `where_*` methods.")]
                pub struct #marker_ident;
                impl ::pocopine_sync_crud::query::__SealedFieldMarker for #marker_ident {}
                #comparator_impl
                #[allow(non_upper_case_globals)]
                pub const #name: #marker_ident = #marker_ident;
            }
        })
        .collect();

    quote! {
        /// Typed view of the subscription parameters declared via
        /// `#[resource(params(...))]`. Generated by the macro.
        #[derive(Clone, Debug)]
        pub struct StreamParams {
            #(#struct_fields)*
        }

        impl StreamParams {
            /// Encode this typed params struct as the wire
            /// `BTreeMap<String, Value>` envelope used by every
            /// `/open`, `/pull`, and `/push` request.
            pub fn serialize_params(&self) -> ::pocopine_sync::StreamParams {
                let mut map = ::pocopine_sync::StreamParams::new();
                #(#serialize_lines)*
                map
            }

            /// Decode a wire `StreamParams` map into this typed
            /// struct. Used by the macro-generated
            /// `SyncStreamSource::validate_params` override. Returns
            /// `SyncError::InvalidValue` for missing required fields,
            /// wrong-shape values, or unknown keys.
            pub fn extract(
                params: &::pocopine_sync::StreamParams,
            ) -> ::pocopine_sync::SyncResult<Self> {
                for key in params.keys() {
                    match key.as_str() {
                        #(#known_keys_match_arms)|* => {}
                        other => {
                            return Err(::pocopine_sync::SyncError::invalid_value(
                                "params",
                                format!("unknown param `{}`", other),
                            ));
                        }
                    }
                }
                #(#extract_locals)*
                Ok(Self { #(#extract_struct_fields)* })
            }
        }

        /// Fluent stream-subscription builder produced by
        /// `Resource::stream()`. Each declared param gets a setter
        /// (named for its comparator kind); `.observe()` validates
        /// that required fields are set, encodes the wire map, and
        /// returns a `LocalResourceView`.
        pub struct StreamBuilder<C: 'static> {
            resource: Resource<C>,
            #(#builder_fields)*
        }

        impl<C: 'static> StreamBuilder<C>
        where
            Row: 'static,
        {
            fn new(resource: Resource<C>) -> Self {
                Self {
                    resource,
                    #(#builder_init_fields)*
                }
            }

            #(#builder_setters)*

            fn into_params(self) -> ::pocopine_sync::SyncResult<
                (Resource<C>, ::pocopine_sync::StreamParams),
            > {
                let mut map = ::pocopine_sync::StreamParams::new();
                // Per-field moves out of `self`. Each block consumes
                // exactly one field; `self.resource` stays accessible
                // at the end because all prior moves are disjoint.
                #(#into_params_lines)*
                Ok((self.resource, map))
            }

            /// Resolve the subscription's typed params, build a
            /// `SyncCollection` for this `(stream, params)` pair,
            /// open the subscription, and return the current
            /// `LocalResourceView` of the typed rows.
            pub fn observe(self) -> ::pocopine_sync::SyncResult<View>
            where
                Id: ::pocopine_sync_crud::ResourceId,
                Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
            {
                let (resource, params) = self.into_params()?;
                let collection = resource
                    .sync
                    .collection(resource.handle.clone(), resource.selector)
                    .stream(NAME)?
                    .schema_version(SCHEMA_VERSION)?
                    .params(params);
                collection.open()?;
                resource.view()
            }

            /// Like `observe()` but additionally registers `callback`
            /// to fire on every reactive update of this subscription's
            /// view state.
            pub fn observe_with<F>(self, callback: F) -> ::pocopine_sync::SyncResult<()>
            where
                Id: ::pocopine_sync_crud::ResourceId,
                Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                F: Fn(&ViewState, ::std::option::Option<&ViewState>) + 'static,
            {
                let (resource, params) = self.into_params()?;
                let collection = resource
                    .sync
                    .collection(resource.handle.clone(), resource.selector)
                    .stream(NAME)?
                    .schema_version(SCHEMA_VERSION)?
                    .params(params);
                collection.open()?;
                resource.observe_view(callback)
            }
        }

        impl<C: 'static> Resource<C>
        where
            Row: 'static,
        {
            /// Build a typed fluent subscription for this resource's
            /// declared `params(...)`. Returns a builder; call its
            /// per-field setters then `.observe()` to start the
            /// subscription.
            pub fn stream(&self) -> StreamBuilder<C> {
                StreamBuilder::new(self.clone())
            }

            /// Build a typed reactive query for this resource — the
            /// declarative cousin of `stream()`. Same underlying
            /// subscription mechanism; the `.where_*` methods take
            /// `field::*` markers and route through the type system
            /// so misuse (`.where_in` on a required-eq field) fails
            /// to compile.
            pub fn query(&self) -> Query<C> {
                Query::new(self.clone())
            }
        }

        /// Type-safe field markers + comparator-trait impls used by
        /// the query DSL. One marker + one `const` per declared
        /// param; the trait impl that's emitted depends on the
        /// declared comparator kind, so `field::workspace_id` only
        /// works with `.where_eq` (when declared as required `T`),
        /// `field::status` only works with `.where_in` (when declared
        /// as `params::InSet<T>`), etc.
        pub mod field {
            #[allow(unused_imports)]
            use super::*;
            #(#field_markers)*
        }

        /// Declarative reactive query produced by `Resource::query()`.
        /// Use `.where_eq`, `.where_in`, `.where_range`,
        /// `.where_contains` with the `field::*` markers to compose a
        /// subscription. `.observe()` returns the current view;
        /// `.observe_with(callback)` subscribes to updates.
        ///
        /// Identical to a `stream()` builder under the hood: both
        /// produce the same `BTreeMap<String, Value>` so two
        /// subscriptions to logically-equivalent params share the
        /// same local materialization once Batch 2b's subscription
        /// registry lands.
        pub struct Query<C: 'static> {
            resource: Resource<C>,
            params: ::pocopine_sync::StreamParams,
        }

        impl<C: 'static> Query<C>
        where
            Row: 'static,
        {
            fn new(resource: Resource<C>) -> Self {
                Self {
                    resource,
                    params: ::pocopine_sync::StreamParams::new(),
                }
            }

            /// Constrain a field to a single value (equality). Only
            /// compiles for fields declared as bare `T` or
            /// `Option<T>` in `params(...)`.
            pub fn where_eq<F, T>(mut self, _field: F, value: T) -> ::pocopine_sync::SyncResult<Self>
            where
                F: ::pocopine_sync_crud::query::FieldEq<T>,
                T: ::serde::Serialize,
            {
                let encoded = ::serde_json::to_value(value)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                self.params.insert(F::NAME.to_string(), encoded);
                Ok(self)
            }

            /// Constrain a field to a non-empty set of values
            /// (membership). Only compiles for fields declared as
            /// `params::InSet<T>` in `params(...)`.
            pub fn where_in<F, T, I>(
                mut self,
                _field: F,
                values: I,
            ) -> ::pocopine_sync::SyncResult<Self>
            where
                F: ::pocopine_sync_crud::query::FieldInSet<T>,
                T: ::serde::Serialize,
                I: ::std::iter::IntoIterator<Item = T>,
            {
                let set = ::pocopine_sync_crud::params::InSet::<T>::new(values)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                let encoded = ::serde_json::to_value(set)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                self.params.insert(F::NAME.to_string(), encoded);
                Ok(self)
            }

            /// Constrain a field to a bounded range. Only compiles
            /// for fields declared as `params::Range<T>` in
            /// `params(...)`. Use the `params::Range::closed` /
            /// `at_least` / `at_most` / `greater_than` / `less_than`
            /// helpers to build the bound.
            pub fn where_range<F, T>(
                mut self,
                _field: F,
                range: ::pocopine_sync_crud::params::Range<T>,
            ) -> ::pocopine_sync::SyncResult<Self>
            where
                F: ::pocopine_sync_crud::query::FieldRange<T>,
                T: ::serde::Serialize,
            {
                let encoded = ::serde_json::to_value(range)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                self.params.insert(F::NAME.to_string(), encoded);
                Ok(self)
            }

            /// Constrain a text field to a non-empty substring
            /// match. Only compiles for fields declared as
            /// `params::Contains` in `params(...)`. The match is
            /// case-insensitive; use the typed
            /// `params::Contains::matches` builder for case-sensitive
            /// variants and pass through `where_contains_with`.
            pub fn where_contains<F>(
                mut self,
                _field: F,
                needle: impl ::std::convert::Into<::std::string::String>,
            ) -> ::pocopine_sync::SyncResult<Self>
            where
                F: ::pocopine_sync_crud::query::FieldContains,
            {
                let contains = ::pocopine_sync_crud::params::Contains::icontains(needle)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                let encoded = ::serde_json::to_value(contains)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                self.params.insert(F::NAME.to_string(), encoded);
                Ok(self)
            }

            /// Like `where_contains` but takes a pre-built
            /// `params::Contains` so callers can choose
            /// case-sensitivity.
            pub fn where_contains_with<F>(
                mut self,
                _field: F,
                contains: ::pocopine_sync_crud::params::Contains,
            ) -> ::pocopine_sync::SyncResult<Self>
            where
                F: ::pocopine_sync_crud::query::FieldContains,
            {
                let encoded = ::serde_json::to_value(contains)
                    .map_err(|err| ::pocopine_sync::SyncError::client(err.to_string()))?;
                self.params.insert(F::NAME.to_string(), encoded);
                Ok(self)
            }

            /// Returns the encoded wire params built so far. Mostly
            /// useful for tests / introspection — `observe()` /
            /// `observe_with()` consume the query internally.
            pub fn params(&self) -> &::pocopine_sync::StreamParams {
                &self.params
            }

            /// Resolve the subscription's typed params, open the
            /// SyncCollection, and return the current view.
            pub fn observe(self) -> ::pocopine_sync::SyncResult<View>
            where
                Id: ::pocopine_sync_crud::ResourceId,
                Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
            {
                let collection = self
                    .resource
                    .sync
                    .collection(self.resource.handle.clone(), self.resource.selector)
                    .stream(NAME)?
                    .schema_version(SCHEMA_VERSION)?
                    .params(self.params);
                collection.open()?;
                self.resource.view()
            }

            /// Like `observe()` but registers `callback` for reactive
            /// view-state updates.
            pub fn observe_with<F>(self, callback: F) -> ::pocopine_sync::SyncResult<()>
            where
                Id: ::pocopine_sync_crud::ResourceId,
                Row: Clone + ::serde::de::DeserializeOwned + ::serde::Serialize,
                F: Fn(&ViewState, ::std::option::Option<&ViewState>) + 'static,
            {
                let collection = self
                    .resource
                    .sync
                    .collection(self.resource.handle.clone(), self.resource.selector)
                    .stream(NAME)?
                    .schema_version(SCHEMA_VERSION)?
                    .params(self.params);
                collection.open()?;
                self.resource.observe_view(callback)
            }
        }
    }
}

/// Compute the value type stored inside the builder's `Option<…>` field
/// for one declared param. The shape mirrors the public StreamParams
/// struct field type — bare `T`, `Option<T>` becomes `T` (we wrap in
/// Option<Option<T>> wouldn't make sense, so the builder collapses the
/// declared `Option<T>` into `T` and uses `None` to mean "absent"),
/// `InSet<T>` / `Range<T>` / `Contains` stay as their wrapper types.
fn builder_value_type(param: &ParamDef) -> TokenStream2 {
    match &param.kind {
        ComparatorKind::OptionalEq { inner } => quote! { #inner },
        _ => {
            let ty = &param.ty;
            quote! { #ty }
        }
    }
}

fn impl_targets_crud_source(item: &ItemImpl) -> bool {
    item.trait_
        .as_ref()
        .and_then(|(_, path, _)| path.segments.last())
        .is_some_and(|segment| segment.ident == "CrudSource")
}

fn extract_resource_types(item: &ItemImpl) -> syn::Result<ResourceTypes> {
    let mut id = None;
    let mut row = None;
    let mut draft = None;

    for item in &item.items {
        let ImplItem::Type(associated) = item else {
            continue;
        };
        match associated.ident.to_string().as_str() {
            "Id" => id = Some(associated.ty.clone()),
            "Row" => row = Some(associated.ty.clone()),
            "Draft" => draft = Some(associated.ty.clone()),
            _ => {}
        }
    }

    Ok(ResourceTypes {
        id: id.ok_or_else(|| missing_associated_type(item, "Id"))?,
        row: row.ok_or_else(|| missing_associated_type(item, "Row"))?,
        draft: draft.ok_or_else(|| missing_associated_type(item, "Draft"))?,
    })
}

fn missing_associated_type(item: &ItemImpl, name: &str) -> syn::Error {
    syn::Error::new(
        item.impl_token.span,
        format!("`#[pocopine_sync_crud::resource]` requires associated type `{name}`"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(input: &str) -> syn::Result<ResourceArgs> {
        syn::parse_str(input)
    }

    #[test]
    fn parses_resource_name() {
        let args = parse_args(r#"name = "customers""#).unwrap();

        assert_eq!(args.name.value(), "customers");
        assert_eq!(args.module.to_string(), "customers");
        assert_eq!(args.schema_version, 1);
    }

    #[test]
    fn parses_schema_version_attribute() {
        let args = parse_args(r#"name = "customers", schema_version = 3"#).unwrap();

        assert_eq!(args.name.value(), "customers");
        assert_eq!(args.schema_version, 3);
    }

    #[test]
    fn rejects_zero_schema_version() {
        let err = parse_args(r#"name = "customers", schema_version = 0"#).unwrap_err();

        // Matches the message used by the runtime builder error too.
        assert!(err.to_string().contains("schema_version must be >= 1"));
    }

    #[test]
    fn rejects_non_integer_schema_version() {
        let err = parse_args(r#"name = "customers", schema_version = "two""#).unwrap_err();

        assert!(err.to_string().contains("must be a u32 integer literal"));
    }

    #[test]
    fn rejects_duplicate_schema_version() {
        let err = parse_args(r#"name = "customers", schema_version = 1, schema_version = 2"#)
            .unwrap_err();

        assert!(err.to_string().contains("duplicate `schema_version`"));
    }

    #[test]
    fn rejects_suffixed_non_u32_schema_version() {
        // `base10_parse::<u32>` strips suffixes silently, so a user
        // writing `3u64` would otherwise parse as `3`. Flag explicitly.
        let err = parse_args(r#"name = "customers", schema_version = 3u64"#).unwrap_err();
        assert!(err
            .to_string()
            .contains("must be a bare integer literal or `u32`-suffixed"));

        let err = parse_args(r#"name = "customers", schema_version = 3i32"#).unwrap_err();
        assert!(err
            .to_string()
            .contains("must be a bare integer literal or `u32`-suffixed"));
    }

    #[test]
    fn accepts_u32_suffixed_schema_version() {
        let args = parse_args(r#"name = "customers", schema_version = 5u32"#).unwrap();
        assert_eq!(args.schema_version, 5);
    }

    #[test]
    fn allows_trailing_comma() {
        let args = parse_args(r#"name = "customers","#).unwrap();

        assert_eq!(args.name.value(), "customers");
        assert_eq!(args.module.to_string(), "customers");
    }

    #[test]
    fn allows_module_override() {
        let args = parse_args(r#"name = "tenant-customers", module = customers"#).unwrap();

        assert_eq!(args.name.value(), "tenant-customers");
        assert_eq!(args.module.to_string(), "customers");
    }

    #[test]
    fn rejects_unknown_attribute_key() {
        let err = parse_args(r#"stream = "customers""#).unwrap_err();

        assert!(err.to_string().contains("expected `name = \"...\"`"));
    }

    #[test]
    fn rejects_invalid_resource_name() {
        let err = parse_args(r#"name = " customers ""#).unwrap_err();

        assert!(err.to_string().contains("invalid CRUD resource name"));
    }

    #[test]
    fn rejects_empty_resource_name() {
        let err = parse_args(r#"name = """#).unwrap_err();

        assert!(err.to_string().contains("invalid CRUD resource name"));
    }

    #[test]
    fn rejects_control_character_resource_name() {
        let err = parse_args("name = \"customers\\n\"").unwrap_err();

        assert!(err.to_string().contains("invalid CRUD resource name"));
    }

    #[test]
    fn rejects_extra_tokens() {
        let err = parse_args(r#"name = "customers" extra"#).unwrap_err();

        assert!(err
            .to_string()
            .contains("expected `,` between CRUD resource attribute options"));
    }

    #[test]
    fn rejects_resource_name_that_cannot_be_module_ident() {
        let err = parse_args(r#"name = "tenant-customers""#).unwrap_err();

        assert!(err.to_string().contains("add `module = ident`"));
    }

    #[test]
    fn detects_crud_source_trait_impl() {
        let item: ItemImpl = syn::parse_quote! {
            impl pocopine_sync_crud::CrudSource for Customers {}
        };

        assert!(impl_targets_crud_source(&item));
    }

    #[test]
    fn rejects_inherent_impl_target() {
        let item: ItemImpl = syn::parse_quote! {
            impl Customers {}
        };

        assert!(!impl_targets_crud_source(&item));
    }

    #[test]
    fn extracts_required_associated_types() {
        let item: ItemImpl = syn::parse_quote! {
            impl pocopine_sync_crud::CrudSource for Customers {
                type Id = uuid::Uuid;
                type Row = Customer;
                type Draft = CustomerDraft;
            }
        };

        let types = extract_resource_types(&item).unwrap();

        assert!(matches!(types.id, Type::Path(_)));
        assert!(matches!(types.row, Type::Path(_)));
        assert!(matches!(types.draft, Type::Path(_)));
    }

    #[test]
    fn rejects_missing_associated_type() {
        let item: ItemImpl = syn::parse_quote! {
            impl pocopine_sync_crud::CrudSource for Customers {
                type Id = uuid::Uuid;
                type Row = Customer;
            }
        };

        let err = extract_resource_types(&item).unwrap_err();

        assert!(err.to_string().contains("associated type `Draft`"));
    }
}
