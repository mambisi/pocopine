//! Proc-macros for `pocopine-sync-crud`.
//!
//! The first macro slice deliberately preserves the annotated impl unchanged.
//! It establishes the attribute grammar and validation boundary that later
//! code generation can build on.

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, ItemImpl, LitStr, Token};

// Matches the sync protocol's stream and row-key token budget.
const MAX_SYNC_TOKEN_LEN: usize = 1024;

#[derive(Debug)]
struct ResourceArgs {
    name: LitStr,
}

impl Parse for ResourceArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let key: syn::Ident = input.parse()?;
        if key != "name" {
            return Err(syn::Error::new(
                key.span(),
                "expected `name = \"...\"` in CRUD resource attribute",
            ));
        }
        input.parse::<Token![=]>()?;
        let name: LitStr = input.parse()?;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after CRUD resource name"));
        }
        validate_resource_name(&name)?;
        Ok(Self { name })
    }
}

/// Mark a `CrudSource` impl as a typed CRUD resource.
///
/// This initial slice validates the resource name and verifies that the
/// attribute is attached to an impl of a trait named `CrudSource`. It emits the
/// original impl unchanged. Later macro slices can use the same validated
/// boundary to generate resource modules and client helpers.
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

    let _resource_name = args.name.value();
    quote! { #item }.into()
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

fn impl_targets_crud_source(item: &ItemImpl) -> bool {
    // This intentionally accepts any path ending in `CrudSource` for the
    // scaffold slice. Once generation depends on Pocopine-specific associated
    // types, tighten this to the supported crate path.
    item.trait_
        .as_ref()
        .and_then(|(_, path, _)| path.segments.last())
        .is_some_and(|segment| segment.ident == "CrudSource")
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
    }

    #[test]
    fn allows_trailing_comma() {
        let args = parse_args(r#"name = "customers","#).unwrap();

        assert_eq!(args.name.value(), "customers");
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
            .contains("unexpected tokens after CRUD resource name"));
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
}
