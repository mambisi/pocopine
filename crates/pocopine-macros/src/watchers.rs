//! Typed snapshot watcher parsing, field metadata, and restricted patch codegen.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Attribute, FnArg, GenericArgument, Ident, ImplItemFn, Pat, PathArguments, ReturnType, Token,
    Type, parenthesized,
    parse::{Parse, ParseStream},
    punctuated::Punctuated,
};

fn name(id: &Ident) -> String {
    id.to_string().trim_start_matches("r#").to_string()
}
fn fields_module(id: &Ident) -> Ident {
    format_ident!("__pocopine_watch_fields_{}", name(id))
}

pub struct Args {
    pub reads: Vec<Ident>,
    pub writes: Vec<Ident>,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let mut saw_writes = false;
        while !input.is_empty() {
            let id: Ident = input.parse().map_err(|_| {
                input.error("#[watch] expects field identifiers and optional writes(field, ...)")
            })?;
            if id == "writes" && input.peek(syn::token::Paren) {
                if saw_writes {
                    return Err(syn::Error::new_spanned(
                        id,
                        "only one writes(...) list is allowed",
                    ));
                }
                saw_writes = true;
                let content;
                parenthesized!(content in input);
                writes = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?
                    .into_iter()
                    .collect();
            } else {
                if saw_writes {
                    return Err(syn::Error::new_spanned(
                        id,
                        "watched fields must precede writes(...)",
                    ));
                }
                reads.push(id);
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        if reads.is_empty() {
            return Err(input.error("#[watch] requires at least one input field"));
        }
        for (fields, label) in [(&reads, "watch"), (&writes, "writes")] {
            for (i, field) in fields.iter().enumerate() {
                if fields[..i].iter().any(|f| name(f) == name(field)) {
                    return Err(syn::Error::new_spanned(
                        field,
                        format!("duplicate field `{field}` in the {label} list"),
                    ));
                }
            }
        }
        for field in &writes {
            if reads.iter().any(|f| name(f) == name(field)) {
                return Err(syn::Error::new_spanned(
                    field,
                    "a watcher cannot write a watched input field",
                ));
            }
        }
        Ok(Self { reads, writes })
    }
}

pub struct Watch {
    pub method: Ident,
    pub args: Args,
    params: Vec<(Ident, Type)>,
    module: Ident,
    fields_module: Ident,
    pub cfg: Vec<Attribute>,
    patch: bool,
}

impl Watch {
    pub fn parse(method: &mut ImplItemFn, args: Args, owner: &Type) -> syn::Result<Self> {
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "#[watch] handlers take no self receiver; declare each input as field: FieldUpdate<T> and return Update<Self> for state changes",
            ));
        }
        if method.sig.asyncness.is_some()
            || method.sig.unsafety.is_some()
            || !method.sig.generics.params.is_empty()
        {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "#[watch] handlers must be synchronous, safe, and non-generic",
            ));
        }
        if matches!(
            name(&method.sig.ident).as_str(),
            "on_setup" | "on_mount" | "on_ready" | "on_unmount"
        ) {
            return Err(syn::Error::new_spanned(
                &method.sig.ident,
                "lifecycle methods cannot be #[watch] handlers",
            ));
        }
        let Type::Path(owner_path) = owner else {
            return Err(syn::Error::new_spanned(
                owner,
                "#[watch] requires a named component or store",
            ));
        };
        let owner_id = &owner_path.path.segments.last().unwrap().ident;
        let module = format_ident!(
            "__pocopine_watch_{}_{}",
            name(owner_id),
            name(&method.sig.ident)
        );
        let mut params = Vec::new();
        for arg in &method.sig.inputs {
            let FnArg::Typed(arg) = arg else {
                unreachable!()
            };
            let Pat::Ident(pat) = arg.pat.as_ref() else {
                return Err(syn::Error::new_spanned(
                    arg,
                    "watch inputs must use simple field names",
                ));
            };
            if pat.by_ref.is_some() || pat.subpat.is_some() || !arg.attrs.is_empty() {
                return Err(syn::Error::new_spanned(
                    arg,
                    "watch inputs must be named FieldUpdate<T> parameters without extractors",
                ));
            }
            let Type::Path(path) = arg.ty.as_ref() else {
                return Err(syn::Error::new_spanned(
                    &arg.ty,
                    "watch inputs must have type FieldUpdate<T>",
                ));
            };
            let segment = path.path.segments.last().unwrap();
            let PathArguments::AngleBracketed(generics) = &segment.arguments else {
                return Err(syn::Error::new_spanned(
                    &arg.ty,
                    "watch inputs must have type FieldUpdate<T>",
                ));
            };
            if segment.ident != "FieldUpdate" || generics.args.len() != 1 {
                return Err(syn::Error::new_spanned(
                    &arg.ty,
                    "watch inputs must have type FieldUpdate<T>",
                ));
            }
            let Some(GenericArgument::Type(value_type)) = generics.args.first() else {
                return Err(syn::Error::new_spanned(
                    &arg.ty,
                    "watch inputs must have type FieldUpdate<T>",
                ));
            };
            if !args.reads.iter().any(|f| name(f) == name(&pat.ident)) {
                return Err(syn::Error::new_spanned(
                    &pat.ident,
                    "watch parameter must name a declared input field",
                ));
            }
            params.push((pat.ident.clone(), value_type.clone()));
        }
        if params.len() != args.reads.len()
            || args
                .reads
                .iter()
                .any(|r| params.iter().filter(|(p, _)| name(p) == name(r)).count() != 1)
        {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "#[watch] requires one named FieldUpdate<T> parameter for each watched field",
            ));
        }
        let patch = match &method.sig.output {
            ReturnType::Default => false,
            ReturnType::Type(_, ty) if matches!(ty.as_ref(), Type::Tuple(tuple) if tuple.elems.is_empty()) => {
                false
            }
            ReturnType::Type(_, ty) => {
                let valid = match ty.as_ref() {
                    Type::Path(path) => path.path.segments.last().is_some_and(|segment| {
                        segment.ident == "Update" && matches!(&segment.arguments,
                            PathArguments::AngleBracketed(a) if a.args.len() == 1 && matches!(a.args.first(), Some(GenericArgument::Type(Type::Path(p))) if p.path.is_ident("Self")))
                    }),
                    _ => false,
                };
                if !valid {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[watch] must return Update<Self> or ()",
                    ));
                }
                true
            }
        };
        if !patch && !args.writes.is_empty() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a watcher with writes(...) must return Update<Self>",
            ));
        }
        if patch {
            method.sig.output = syn::parse_quote!(-> ::pocopine::Update<Self, #module::Policy>);
            method.block.stmts.insert(
                0,
                syn::parse_quote!(#[allow(unused_imports)] use #module::Setters as _;),
            );
        }
        Ok(Self {
            method: method.sig.ident.clone(),
            args,
            params,
            module,
            fields_module: fields_module(owner_id),
            cfg: method
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
                .cloned()
                .collect(),
            patch,
        })
    }

    pub fn definitions(&self, owner: &Type) -> TokenStream {
        if !self.patch {
            return quote! {};
        }
        let Self {
            module,
            fields_module,
            cfg,
            ..
        } = self;
        let fields = &self.args.writes;
        let types: Vec<_> = fields.iter().map(|f| quote!(<#fields_module::#f as ::pocopine::__private::WatchField<#owner>>::Value)).collect();
        quote! {
            #(#cfg)*
            #[doc(hidden)]
            #[allow(non_snake_case, unused_imports, unused_variables, dead_code)]
            mod #module {
                use super::*;
                pub struct Policy;
                #[derive(Default)]
                pub struct Patch { #(#fields: Option<#types>,)* }
                impl ::pocopine::__private::WatchSpec<#owner> for Policy {
                    type Patch = Patch;
                    fn is_empty(patch: &Patch) -> bool { true #(&& patch.#fields.is_none())* }
                    fn apply(patch: Patch, state: &mut #owner) {
                        #(if let Some(value) = patch.#fields {
                            <#fields_module::#fields as ::pocopine::__private::WatchField<#owner>>::set(state, value);
                        })*
                    }
                }
                pub trait Setters: Sized { #(fn #fields(self, value: #types) -> Self;)* }
                impl Setters for ::pocopine::Update<#owner, Policy> {
                    #(fn #fields(mut self, value: #types) -> Self {
                        self.__patch_mut().#fields = Some(value);
                        self
                    })*
                }
            }
        }
    }

    pub fn install(&self, owner: &Type) -> TokenStream {
        let Self {
            method,
            cfg,
            fields_module,
            ..
        } = self;
        let names: Vec<_> = self.args.reads.iter().map(name).collect();
        let label = format!("{}::{}", quote!(#owner), method);
        let (fields, types): (Vec<_>, Vec<_>) = self.params.iter().map(|(f, t)| (f, t)).unzip();
        let indices: Vec<_> = (0..fields.len()).map(syn::Index::from).collect();
        quote! {
            #(#cfg)*
            {
                let scope = ::pocopine::current_scope_id().expect("watch installed outside a lifecycle context");
                ::pocopine::__private::install_snapshot_watch::<#owner, (#(#types,)*), _>(
                    scope, &[#(#names),*], #label,
                    |state| {
                        #(let _: &<#fields_module::#fields as ::pocopine::__private::WatchField<#owner>>::Value = &state.#fields;)*
                        (#(state.#fields.clone(),)*)
                    },
                    |next, previous| <#owner>::#method(#(
                        ::pocopine::FieldUpdate {
                            current: next.#indices,
                            previous: previous.as_ref().map(|p| p.#indices.clone()),
                        }
                    ),*),
                );
            }
        }
    }

    pub fn graph_node(&self) -> TokenStream {
        let cfg = &self.cfg;
        let reads: Vec<_> = self.args.reads.iter().map(name).collect();
        let writes: Vec<_> = self.args.writes.iter().map(name).collect();
        quote! { #(#cfg)* ::pocopine::__private::WatchNode { reads: &[#(#reads),*], writes: &[#(#writes),*] } }
    }
}

pub fn field_metadata(
    owner: &Ident,
    fields: &[Ident],
    types: &[Type],
    skipped: &[bool],
) -> TokenStream {
    let module = fields_module(owner);
    let items = fields
        .iter()
        .zip(types)
        .zip(skipped)
        .filter(|(_, skip)| !**skip)
        .map(|((field, ty), _)| {
            quote! {
                pub(super) enum #field {}
                impl ::pocopine::__private::WatchField<#owner> for #field {
                    type Value = #ty;
                    fn set(state: &mut #owner, value: Self::Value) { state.#field = value; }
                }
            }
        });
    quote! {
        #[doc(hidden)]
        #[allow(non_snake_case, non_camel_case_types, unused_imports, dead_code)]
        mod #module { use super::*; #(#items)* }
    }
}
