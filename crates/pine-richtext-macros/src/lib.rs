//! Proc-macros for target-independent `pine-richtext` semantic node types.

use std::collections::BTreeMap;

use proc_macro::TokenStream;
use quote::quote;
use syn::{Attribute, Data, DeriveInput, Error, Fields, LitStr, Result, Token, parse_macro_input};

/// Derive the closed serde wire-key set for a rich-text node attribute map.
///
/// This deliberately accepts only named-field structs and the serde options
/// whose accepted and emitted keys remain statically knowable.
#[proc_macro_derive(RichTextNodeAttrs, attributes(serde))]
pub fn derive_rich_text_node_attrs(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive(input)
        .unwrap_or_else(Error::into_compile_error)
        .into()
}

fn derive(input: DeriveInput) -> Result<proc_macro2::TokenStream> {
    let container = parse_container_options(&input.attrs)?;
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => &fields.named,
            Fields::Unnamed(_) => {
                return Err(Error::new_spanned(
                    &input.ident,
                    "RichTextNodeAttrs can only be derived for a struct with named fields; tuple structs do not have a closed string-key map",
                ));
            }
            Fields::Unit => {
                return Err(Error::new_spanned(
                    &input.ident,
                    "RichTextNodeAttrs can only be derived for a struct with named fields; unit structs do not have an attribute map",
                ));
            }
        },
        Data::Enum(_) => {
            return Err(Error::new_spanned(
                &input.ident,
                "RichTextNodeAttrs cannot be derived for enums; node attrs must serialize as one closed object map",
            ));
        }
        Data::Union(_) => {
            return Err(Error::new_spanned(
                &input.ident,
                "RichTextNodeAttrs cannot be derived for unions; node attrs must serialize as one closed object map",
            ));
        }
    };

    let mut keys = Vec::with_capacity(fields.len());
    let mut seen = BTreeMap::<String, proc_macro2::Span>::new();
    for field in fields {
        let ident = field.ident.as_ref().expect("named fields have identifiers");
        let rust_name = ident.to_string();
        let rust_name = rust_name.strip_prefix("r#").unwrap_or(&rust_name);
        let options = parse_field_options(&field.attrs)?;
        let key = options.rename.unwrap_or_else(|| {
            container
                .rename_all
                .unwrap_or(RenameRule::None)
                .apply_to_field(rust_name)
        });

        if let Some(previous) = seen.insert(key.clone(), ident.span()) {
            let mut error = Error::new(
                ident.span(),
                format!("duplicate serde wire key `{key}` in RichTextNodeAttrs"),
            );
            error.combine(Error::new(previous, "the same wire key was declared here"));
            return Err(error);
        }
        keys.push(LitStr::new(&key, ident.span()));
    }

    let name = &input.ident;
    let (_, input_ty_generics, _) = input.generics.split_for_impl();
    let self_type = quote!(#name #input_ty_generics);
    let mut generics = input.generics.clone();
    generics
        .make_where_clause()
        .predicates
        .push(syn::parse_quote!(
            #self_type: ::pine_richtext::__private::serde::Serialize
                + ::pine_richtext::__private::serde::de::DeserializeOwned
                + ::core::clone::Clone
                + ::core::cmp::PartialEq
                + ::core::marker::Send
                + ::core::marker::Sync
                + 'static
        ));
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    Ok(quote! {
        impl #impl_generics ::pine_richtext::RichTextNodeAttrs for #name #ty_generics #where_clause {
            const KEYS: &'static [&'static str] = &[#(#keys),*];
        }
    })
}

#[derive(Default)]
struct ContainerOptions {
    rename_all: Option<RenameRule>,
}

#[derive(Default)]
struct FieldOptions {
    rename: Option<String>,
}

fn parse_container_options(attrs: &[Attribute]) -> Result<ContainerOptions> {
    let mut options = ContainerOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                require_equals_form(&meta, "directional serde rename_all")?;
                let value: LitStr = meta.value()?.parse()?;
                let rule = RenameRule::parse(&value)?;
                if options.rename_all.replace(rule).is_some() {
                    return Err(meta.error("duplicate serde rename_all option"));
                }
                return Ok(());
            }
            if meta.path.is_ident("rename") {
                require_equals_form(&meta, "directional serde rename")?;
                let _: LitStr = meta.value()?.parse()?;
                return Ok(());
            }
            if meta.path.is_ident("default") {
                consume_optional_string_value(&meta)?;
                return Ok(());
            }
            if meta.path.is_ident("transparent")
                || meta.path.is_ident("from")
                || meta.path.is_ident("try_from")
                || meta.path.is_ident("into")
            {
                return Err(meta.error(format!(
                    "serde `{}` is not supported by RichTextNodeAttrs because it changes the object-map wire shape",
                    path_name(&meta.path)
                )));
            }
            Err(meta.error(
                "unsupported serde container option for RichTextNodeAttrs; only rename, rename_all, and default are allowed",
            ))
        })?;
    }
    Ok(options)
}

fn parse_field_options(attrs: &[Attribute]) -> Result<FieldOptions> {
    let mut options = FieldOptions::default();
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                require_equals_form(&meta, "directional serde rename")?;
                let value: LitStr = meta.value()?.parse()?;
                if options.rename.replace(value.value()).is_some() {
                    return Err(meta.error("duplicate serde rename option"));
                }
                return Ok(());
            }
            if meta.path.is_ident("default") {
                consume_optional_string_value(&meta)?;
                return Ok(());
            }

            let rejected = [
                "flatten",
                "alias",
                "skip",
                "skip_serializing",
                "skip_deserializing",
                "skip_serializing_if",
                "serialize_with",
                "deserialize_with",
                "with",
            ];
            if rejected.iter().any(|name| meta.path.is_ident(name)) {
                return Err(meta.error(format!(
                    "serde `{}` is not supported by RichTextNodeAttrs because it makes the accepted and emitted key maps asymmetric or unknowable",
                    path_name(&meta.path)
                )));
            }
            Err(meta.error(
                "unsupported serde field option for RichTextNodeAttrs; only rename and default are allowed",
            ))
        })?;
    }
    Ok(options)
}

fn require_equals_form(meta: &syn::meta::ParseNestedMeta<'_>, feature: &str) -> Result<()> {
    if meta.input.peek(Token![=]) {
        Ok(())
    } else {
        Err(meta.error(format!(
            "{feature} is not supported by RichTextNodeAttrs; serialization and deserialization must use one identical wire key"
        )))
    }
}

fn consume_optional_string_value(meta: &syn::meta::ParseNestedMeta<'_>) -> Result<()> {
    if meta.input.peek(Token![=]) {
        let _: LitStr = meta.value()?.parse()?;
    }
    Ok(())
}

fn path_name(path: &syn::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.ident.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

#[derive(Clone, Copy)]
enum RenameRule {
    None,
    LowerCase,
    UpperCase,
    PascalCase,
    CamelCase,
    SnakeCase,
    ScreamingSnakeCase,
    KebabCase,
    ScreamingKebabCase,
}

impl RenameRule {
    fn parse(value: &LitStr) -> Result<Self> {
        let rule = match value.value().as_str() {
            "lowercase" => Self::LowerCase,
            "UPPERCASE" => Self::UpperCase,
            "PascalCase" => Self::PascalCase,
            "camelCase" => Self::CamelCase,
            "snake_case" => Self::SnakeCase,
            "SCREAMING_SNAKE_CASE" => Self::ScreamingSnakeCase,
            "kebab-case" => Self::KebabCase,
            "SCREAMING-KEBAB-CASE" => Self::ScreamingKebabCase,
            other => {
                return Err(Error::new(
                    value.span(),
                    format!("unsupported serde rename_all rule `{other}`"),
                ));
            }
        };
        Ok(rule)
    }

    fn apply_to_field(self, field: &str) -> String {
        match self {
            // Serde treats Rust struct field identifiers as already lowercase
            // for this rule (unlike enum variants, which it transforms).
            Self::None | Self::LowerCase | Self::SnakeCase => field.to_owned(),
            Self::UpperCase | Self::ScreamingSnakeCase => field.to_ascii_uppercase(),
            Self::PascalCase => to_pascal_case(field),
            Self::CamelCase => {
                let pascal = to_pascal_case(field);
                let mut chars = pascal.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
                    None => pascal,
                }
            }
            Self::KebabCase => field.replace('_', "-"),
            Self::ScreamingKebabCase => field.to_ascii_uppercase().replace('_', "-"),
        }
    }
}

fn to_pascal_case(field: &str) -> String {
    let mut output = String::with_capacity(field.len());
    let mut capitalize = true;
    for ch in field.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            output.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::RenameRule;

    #[test]
    fn field_rename_rules_match_serde() {
        let original = "very_tasty";
        assert_eq!(RenameRule::None.apply_to_field(original), "very_tasty");
        assert_eq!(RenameRule::LowerCase.apply_to_field(original), "very_tasty");
        assert_eq!(RenameRule::UpperCase.apply_to_field(original), "VERY_TASTY");
        assert_eq!(RenameRule::PascalCase.apply_to_field(original), "VeryTasty");
        assert_eq!(RenameRule::CamelCase.apply_to_field(original), "veryTasty");
        assert_eq!(RenameRule::SnakeCase.apply_to_field(original), "very_tasty");
        assert_eq!(
            RenameRule::ScreamingSnakeCase.apply_to_field(original),
            "VERY_TASTY"
        );
        assert_eq!(RenameRule::KebabCase.apply_to_field(original), "very-tasty");
        assert_eq!(
            RenameRule::ScreamingKebabCase.apply_to_field(original),
            "VERY-TASTY"
        );

        // This surprising edge matches serde's struct-field behavior.
        assert_eq!(
            RenameRule::LowerCase.apply_to_field("MixedCase"),
            "MixedCase"
        );
    }
}
