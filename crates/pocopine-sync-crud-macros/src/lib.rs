//! Proc-macros for `pocopine-sync-crud`.
//!
//! The macro reads a server-side `CrudSource` impl and generates the shared
//! resource module that client and server code use.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Ident, ImplItem, ItemImpl, LitStr, Token, Type};

// Matches the sync protocol's stream and row-key token budget.
const MAX_SYNC_TOKEN_LEN: usize = 1024;

#[derive(Debug)]
struct ResourceArgs {
    name: LitStr,
    module: Ident,
}

impl Parse for ResourceArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut module = None;

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
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `name = \"...\"` or `module = ident` in CRUD resource attribute",
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
        Ok(Self { name, module })
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
    let source = &item.self_ty;
    let id = types.id;
    let row = types.row;
    let draft = types.draft;

    Ok(quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #item

        pub mod #module {
            #[allow(unused_imports)]
            use super::*;

            pub const NAME: &str = #name;

            pub type Id = #id;
            pub type Row = #row;
            pub type Draft = #draft;

            pub type CreateOptions = ::pocopine_sync_crud::CreateOptions<Row>;
            pub type SaveOptions = ::pocopine_sync_crud::SaveOptions<Row>;
            pub type RemoveOptions = ::pocopine_sync_crud::RemoveOptions;
            pub type Outcome = ::pocopine_sync_crud::CrudOutcome<Id, Row>;
            pub type Queued = ::pocopine_sync_crud::Queued<Id>;
            pub type Client<C> = ::pocopine_sync_crud::CrudClientResource<C, Id, Row>;

            #[cfg(not(target_arch = "wasm32"))]
            pub fn resource(
                source: #source,
            ) -> ::pocopine_sync::SyncResult<::pocopine_sync_crud::CrudResourceBuilder<#source>> {
                ::pocopine_sync_crud::resource(NAME, source)
            }

            pub fn new_id() -> ::pocopine_sync::SyncResult<Id> {
                ::pocopine_sync_crud::new_id()
            }

            pub fn view(
                state: &::pocopine_sync::CollectionState<Row>,
            ) -> ::pocopine_sync::SyncResult<::pocopine_sync_crud::LocalResourceView<Id, Row>> {
                ::pocopine_sync_crud::local_resource_view(state)
            }

            pub fn client<C: 'static>(
                collection: ::pocopine_sync::SyncCollection<C, Row>,
                state: &::pocopine_sync::CollectionState<Row>,
            ) -> ::pocopine_sync::SyncResult<Client<C>> {
                let view = view(state)?;
                Ok(::pocopine_sync_crud::client_resource(collection, view))
            }

            #[cfg(target_arch = "wasm32")]
            pub fn collection<C: 'static>(
                sync: &::pocopine_sync::SyncClient,
                handle: ::pocopine::Handle<C>,
                selector: ::pocopine_sync::CollectionSelector<C, Row>,
            ) -> ::pocopine_sync::SyncResult<::pocopine_sync::SyncCollection<C, Row>> {
                sync.collection(handle, selector).stream(NAME)
            }
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
