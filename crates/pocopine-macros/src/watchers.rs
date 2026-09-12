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
    format_ident!("{}Field", name(id))
}
fn marker(id: &Ident) -> Ident {
    let mut pascal: String = name(id)
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let mut word = chars.next().unwrap().to_uppercase().collect::<String>();
            word.extend(chars);
            word
        })
        .collect();
    if pascal.starts_with(|c: char| c.is_ascii_digit()) {
        pascal.insert(0, '_');
    }
    if pascal == "Self" {
        pascal.push('_');
    }
    // A Rust field may consist entirely of underscores. Keep its marker a
    // valid identifier and let the owner check for normalization collisions.
    format_ident!(
        "{}",
        if pascal.is_empty() {
            "Underscore"
        } else {
            &pascal
        },
        span = id.span()
    )
}

fn check_duplicates(fields: &[Ident], label: &str) -> syn::Result<()> {
    for (i, field) in fields.iter().enumerate() {
        if fields[..i].iter().any(|f| name(f) == name(field)) {
            return Err(syn::Error::new_spanned(
                field,
                format!("duplicate field `{field}` in the {label} list"),
            ));
        }
    }
    Ok(())
}

fn all_fields_macro(id: &Ident) -> Ident {
    format_ident!("__pocopine_watch_all_{}", name(id))
}

fn single_type_arg<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(path) = ty else { return None };
    let segment = path.path.segments.last()?;
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    if segment.ident != wrapper || args.args.len() != 1 {
        return None;
    }
    match args.args.first()? {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    }
}

enum InputMode {
    Owned,
    Borrowed,
    Change,
}

struct Param {
    field: Ident,
    ty: Type,
    mode: InputMode,
}

pub struct Args {
    pub reads: Vec<Ident>,
    /// Resolved output marker names, shared by explicit and shorthand forms.
    pub writes: Vec<Ident>,
    has_updates: bool,
    pub all: bool,
}

impl Args {
    pub fn all() -> Self {
        Self {
            reads: Vec::new(),
            writes: Vec::new(),
            has_updates: false,
            all: true,
        }
    }
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut reads = Vec::new();
        let mut writes = Vec::new();
        let mut has_updates = false;
        while !input.is_empty() {
            let id: Ident = input.parse().map_err(|_| {
                input.error("#[watch] expects field identifiers and optional updates(field, ...)")
            })?;
            if id == "writes" && input.peek(syn::token::Paren) {
                return Err(syn::Error::new_spanned(
                    id,
                    "writes(...) was renamed to updates(...); or declare outputs in Update<Self, (Self::Field, ...)>",
                ));
            }
            if id == "updates" && input.peek(syn::token::Paren) {
                if has_updates {
                    return Err(syn::Error::new_spanned(
                        id,
                        "only one updates(...) list is allowed",
                    ));
                }
                has_updates = true;
                let content;
                parenthesized!(content in input);
                writes = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?
                    .into_iter()
                    .collect();
            } else {
                if has_updates {
                    return Err(syn::Error::new_spanned(
                        id,
                        "watched fields must precede updates(...)",
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
        check_duplicates(&reads, "watch")?;
        check_duplicates(&writes, "updates")?;
        Ok(Self {
            reads,
            writes: writes.iter().map(marker).collect(),
            has_updates,
            all: false,
        })
    }
}

pub struct Watch {
    pub method: Ident,
    pub args: Args,
    params: Vec<Param>,
    module: Ident,
    fields_module: Ident,
    all_fields_macro: Ident,
    pub cfg: Vec<Attribute>,
}

impl Watch {
    pub fn parse(method: &mut ImplItemFn, mut args: Args, owner: &Type) -> syn::Result<Self> {
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "#[watch] handlers take no self receiver; use named T, &T, or Change<T> inputs, or Changes<Self> for a bare #[watch]",
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
                    "watch inputs must be named parameters without extractors",
                ));
            }
            if args.all {
                if !matches!(single_type_arg(&arg.ty, "Changes"), Some(Type::Path(p)) if p.path.is_ident("Self"))
                {
                    return Err(syn::Error::new_spanned(
                        arg,
                        "bare #[watch] requires one Changes<Self> parameter",
                    ));
                }
                continue;
            }
            if !args.reads.iter().any(|f| name(f) == name(&pat.ident)) {
                return Err(syn::Error::new_spanned(
                    &pat.ident,
                    "watch parameter must name a declared input field",
                ));
            }
            let (mode, ty) = if let Some(ty) = single_type_arg(&arg.ty, "Change") {
                (InputMode::Change, ty.clone())
            } else if let Type::Reference(reference) = arg.ty.as_ref() {
                if reference.mutability.is_some() {
                    return Err(syn::Error::new_spanned(
                        arg,
                        "watch inputs cannot be mutable references; return Update<Self> for state changes",
                    ));
                }
                (InputMode::Borrowed, (*arg.ty).clone())
            } else {
                (InputMode::Owned, (*arg.ty).clone())
            };
            params.push(Param {
                field: pat.ident.clone(),
                ty,
                mode,
            });
        }
        if args.all {
            if method.sig.inputs.len() != 1 {
                return Err(syn::Error::new_spanned(
                    &method.sig,
                    "bare #[watch] requires one Changes<Self> parameter",
                ));
            }
            let FnArg::Typed(arg) = method.sig.inputs.first_mut().unwrap() else {
                unreachable!()
            };
            // Preserve the user's path so an explicit `use pocopine::Changes`
            // stays used after expanding the shorthand.
            let Type::Path(path) = arg.ty.as_mut() else {
                unreachable!()
            };
            let PathArguments::AngleBracketed(generics) =
                &mut path.path.segments.last_mut().unwrap().arguments
            else {
                unreachable!()
            };
            generics.args.push(syn::parse_quote!(#module::Policy));
        } else if params.len() != args.reads.len()
            || args
                .reads
                .iter()
                .any(|r| params.iter().filter(|p| name(&p.field) == name(r)).count() != 1)
        {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "#[watch] requires one named T, &T, or Change<T> parameter for each watched field",
            ));
        }
        let mut explicit = None;
        let patch = match &method.sig.output {
            ReturnType::Default => false,
            ReturnType::Type(_, ty) if matches!(ty.as_ref(), Type::Tuple(tuple) if tuple.elems.is_empty()) => {
                false
            }
            ReturnType::Type(_, ty) => {
                let Type::Path(path) = ty.as_ref() else {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[watch] must return Update<Self, (Self::Field, ...)> or (); updates(...) permits Update<Self>",
                    ));
                };
                let segment = path.path.segments.last().unwrap();
                let PathArguments::AngleBracketed(generics) = &segment.arguments else {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[watch] must return Update<Self, (Self::Field, ...)> or (); updates(...) permits Update<Self>",
                    ));
                };
                if segment.ident != "Update"
                    || !(1..=2).contains(&generics.args.len())
                    || !matches!(generics.args.first(), Some(GenericArgument::Type(Type::Path(p))) if p.path.is_ident("Self"))
                {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[watch] must return Update<Self, (Self::Field, ...)> or (); updates(...) permits Update<Self>",
                    ));
                }
                if generics.args.len() == 2 {
                    let Some(GenericArgument::Type(Type::Tuple(tuple))) = generics.args.last()
                    else {
                        return Err(syn::Error::new_spanned(
                            &generics.args,
                            "output fields must be a tuple: (Self::Field,) for one field",
                        ));
                    };
                    let mut outputs = Vec::new();
                    for field in &tuple.elems {
                        let Type::Path(path) = field else {
                            return Err(syn::Error::new_spanned(
                                field,
                                "output fields must use Self::FieldName markers",
                            ));
                        };
                        if path.qself.is_some()
                            || path.path.leading_colon.is_some()
                            || path.path.segments.len() != 2
                            || path.path.segments[0].ident != "Self"
                            || path
                                .path
                                .segments
                                .iter()
                                .any(|s| !matches!(s.arguments, PathArguments::None))
                        {
                            return Err(syn::Error::new_spanned(
                                field,
                                "output fields must use Self::FieldName markers",
                            ));
                        }
                        outputs.push(path.path.segments[1].ident.clone());
                    }
                    explicit = Some(outputs);
                }
                true
            }
        };
        if args.all && patch {
            return Err(syn::Error::new_spanned(
                &method.sig.output,
                "bare #[watch] observes every watchable field and must return (); patches are not allowed",
            ));
        }
        if !patch && args.has_updates {
            return Err(syn::Error::new_spanned(
                &method.sig,
                "a watcher with updates(...) must return Update<Self>",
            ));
        }
        if let Some(outputs) = explicit {
            if args.has_updates {
                return Err(syn::Error::new_spanned(
                    &method.sig.output,
                    "declare outputs once: use an explicit field tuple or updates(...), not both",
                ));
            }
            args.writes = outputs;
        } else if patch && !args.has_updates {
            return Err(syn::Error::new_spanned(
                &method.sig.output,
                "Update<Self> requires updates(...); declare an explicit field tuple or return () for observation",
            ));
        }
        check_duplicates(&args.writes, "output")?;
        if args.writes.len() > 32 {
            return Err(syn::Error::new_spanned(
                &method.sig.output,
                "an Update field tuple supports at most 32 fields",
            ));
        }
        for field in &args.writes {
            if args.reads.iter().any(|read| marker(read) == *field) {
                return Err(syn::Error::new_spanned(
                    field,
                    "a watcher cannot update a watched input field",
                ));
            }
        }
        let fields_module = fields_module(owner_id);
        if patch {
            let ReturnType::Type(_, ty) = &mut method.sig.output else {
                unreachable!()
            };
            let Type::Path(path) = ty.as_mut() else {
                unreachable!()
            };
            let PathArguments::AngleBracketed(generics) =
                &mut path.path.segments.last_mut().unwrap().arguments
            else {
                unreachable!()
            };
            let outputs = &args.writes;
            // Preserve the author's Update path, and resolve both forms to
            // the same tuple of owner markers rather than a per-watch policy.
            generics.args = syn::parse_quote!(Self, (#(#fields_module::#outputs,)*));
            if !outputs.is_empty() {
                method.block.stmts.insert(
                    0,
                    syn::parse_quote!(#[allow(unused_imports)] use #fields_module::setters::{#(#outputs as _,)*};),
                );
            }
            if outputs.is_empty() {
                let warning = crate::build_warning_tokens(
                    "pocopine::empty_watch_update: this watcher declares no outputs; return () instead of Update<Self, ()> or updates()",
                );
                method.block.stmts.insert(0, syn::parse2(warning)?);
            }
        }
        Ok(Self {
            method: method.sig.ident.clone(),
            args,
            params,
            module,
            fields_module,
            all_fields_macro: all_fields_macro(owner_id),
            cfg: method
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
                .cloned()
                .collect(),
        })
    }

    pub fn definitions(&self) -> TokenStream {
        if self.args.all {
            let Self {
                module,
                all_fields_macro,
                cfg,
                ..
            } = self;
            return quote! { #(#cfg)* self::#all_fields_macro!(#module); };
        }
        quote! {}
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
        if self.args.all {
            let module = &self.module;
            return quote! {
                #(#cfg)*
                {
                    let scope = ::pocopine::current_scope_id().expect("watch installed outside a lifecycle context");
                    ::pocopine::__private::install_snapshot_watch::<#owner, <#module::Policy as ::pocopine::__private::WatchAllSpec<#owner>>::History, _>(
                        scope, <#module::Policy as ::pocopine::__private::WatchAllSpec<#owner>>::FIELDS, #label,
                        |state, previous| {
                            let (changes, history) = <#module::Policy as ::pocopine::__private::WatchAllSpec<#owner>>::read(state, previous);
                            <#owner>::#method(changes);
                            ((), history)
                        },
                    );
                }
            };
        }
        let mut history_fields = Vec::new();
        let mut history_types = Vec::new();
        let mut inputs = Vec::new();
        let mut values = Vec::new();
        let mut checks = Vec::new();
        for (i, Param { field, ty, mode }) in self.params.iter().enumerate() {
            let local = format_ident!("__watch_input_{i}");
            let field_marker = marker(field);
            checks.push(quote! { let _: &<#fields_module::#field_marker as ::pocopine::Field<#owner>>::Value = &state.#field; });
            inputs.push(match mode {
                InputMode::Owned => quote! { let #local: #ty = state.#field.clone(); },
                InputMode::Borrowed => quote! { let #local: #ty = &state.#field; },
                InputMode::Change => {
                    let index = syn::Index::from(history_fields.len());
                    history_fields.push(field);
                    history_types.push(ty);
                    quote! {
                        let #local = ::pocopine::Change::<#ty> {
                            current: state.#field.clone(),
                            previous: _previous.map(|p| p.#index.clone()),
                        };
                    }
                }
            });
            values.push(local);
        }
        quote! {
            #(#cfg)*
            {
                let scope = ::pocopine::current_scope_id().expect("watch installed outside a lifecycle context");
                ::pocopine::__private::install_snapshot_watch::<#owner, (#(#history_types,)*), _>(
                    scope, &[#(#names),*], #label,
                    |state, _previous| {
                        #(#checks)*
                        #(#inputs)*
                        let result = <#owner>::#method(#(#values),*);
                        (result, (#(state.#history_fields.clone(),)*))
                    },
                );
            }
        }
    }

    pub fn graph_node(&self, owner: &Type) -> TokenStream {
        let cfg = &self.cfg;
        if self.args.all {
            // All-fields observers have no outputs, so they cannot create
            // cycles. Their runtime subscription still includes every field.
            return quote! { #(#cfg)* ::pocopine::__private::WatchNode { reads: &[], writes: &[] } };
        }
        let reads: Vec<_> = self.args.reads.iter().map(name).collect();
        let fields_module = &self.fields_module;
        let writes: Vec<_> = self
            .args
            .writes
            .iter()
            .map(|f| quote!(<#fields_module::#f as ::pocopine::Field<#owner>>::NAME))
            .collect();
        quote! { #(#cfg)* ::pocopine::__private::WatchNode { reads: &[#(#reads),*], writes: &[#(#writes),*] } }
    }
}

fn marker_visibility(vis: &syn::Visibility) -> syn::Visibility {
    match vis {
        syn::Visibility::Inherited => syn::parse_quote!(pub(super)),
        syn::Visibility::Restricted(restricted) => {
            let mut vis = restricted.clone();
            let first = &mut vis.path.segments[0].ident;
            if *first == "self" {
                *first = syn::parse_quote!(super);
            } else if *first == "super" {
                let path = &vis.path;
                vis.path = Box::new(syn::parse_quote!(super::#path));
            }
            vis.in_token = Some(Default::default());
            syn::Visibility::Restricted(vis)
        }
        _ => vis.clone(),
    }
}

pub fn field_metadata(
    input: &syn::ItemStruct,
    fields: &[Ident],
    types: &[Type],
    skipped: &[bool],
) -> TokenStream {
    let owner = &input.ident;
    let visibility = &input.vis;
    let module = fields_module(owner);
    let all_macro = all_fields_macro(owner);
    let active: Vec<_> = fields
        .iter()
        .zip(skipped)
        .filter(|(_, skip)| !**skip)
        .map(|(field, _)| field)
        .collect();
    let markers: Vec<_> = active.iter().map(|field| marker(field)).collect();
    if let Err(error) = check_duplicates(&markers, "generated field marker") {
        return error.to_compile_error();
    }
    let names: Vec<_> = active.iter().map(|field| name(field)).collect();
    let active_types: Vec<_> = markers
        .iter()
        .map(|field| quote!(<#module::#field as ::pocopine::Field<#owner>>::Value))
        .collect();
    let mut marker_items = Vec::new();
    let mut setter_items = Vec::new();
    let mut field_impls = Vec::new();
    for (((field, ty), skip), original) in fields
        .iter()
        .zip(types)
        .zip(skipped)
        .zip(input.fields.iter())
    {
        if *skip {
            continue;
        }
        let marker = marker(field);
        let setter = &marker;
        let vis = marker_visibility(&original.vis);
        let setter_vis = marker_visibility(&vis);
        let field_name = name(field);
        let value = quote!(<super::#marker as ::pocopine::Field<super::super::#owner>>::Value);
        marker_items.push(quote! {
            #[doc = concat!("Descriptor for `", stringify!(#owner), "::", stringify!(#field), "`.")]
            #vis enum #marker {}
        });
        setter_items.push(quote! {
            #setter_vis trait #setter<W: ::pocopine::__private::WatchSpec<super::super::#owner>>: Sized {
                fn #field<const INDEX: usize>(self, value: #value) -> Self
                where W: ::pocopine::__private::UpdateField<super::super::#owner, super::#marker, INDEX>;
            }
            impl<W: ::pocopine::__private::WatchSpec<super::super::#owner>> #setter<W> for ::pocopine::Update<super::super::#owner, W> {
                fn #field<const INDEX: usize>(mut self, value: #value) -> Self
                where W: ::pocopine::__private::UpdateField<super::super::#owner, super::#marker, INDEX> {
                    <W as ::pocopine::__private::UpdateField<super::super::#owner, super::#marker, INDEX>>::set_field(self.__patch_mut(), value);
                    self
                }
            }
        });
        // Resolve original field types in the declaration module, where
        // `theme::Theme`, `self::` and `super::` retain their meaning.
        field_impls.push(quote! {
            impl ::pocopine::Field<#owner> for #module::#marker {
                type Value = #ty;
                const NAME: &'static str = #field_name;
                fn get(state: &#owner) -> &Self::Value { &state.#field }
                fn set(state: &mut #owner, value: Self::Value) { state.#field = value; }
            }
        });
    }
    quote! {
        #[doc = concat!("Typed field descriptors for [`", stringify!(#owner), "`]. The setters module contains fluent Update extension traits.")]
        #[allow(non_snake_case, non_camel_case_types, unused_imports, dead_code)]
        #visibility mod #module {
            #(#marker_items)*
            /// Import each selected field's trait as `_` for fluent Update setters.
            pub mod setters { #(#setter_items)* }
        }
        #(#field_impls)*

        // Generate history only when an active bare observer requests it.
        // Components with borrowed-only watches need no Clone implementations.
        #[allow(unused_macros)]
        macro_rules! #all_macro {
            ($watch_module:ident) => {
                #[doc(hidden)]
                #[allow(non_snake_case, unused_imports, unused_variables, dead_code)]
                mod $watch_module {
                    use super::*;
                    pub struct Policy;
                    pub struct Inputs { #(pub(super) #active: ::pocopine::Change<#active_types>,)* }
                    pub struct History { #(#active: #active_types,)* }
                    impl ::pocopine::__private::WatchAllSpec<#owner> for Policy {
                        type Changes = Inputs;
                        type History = History;
                        const FIELDS: &'static [&'static str] = &[#(#names),*];
                        fn read(state: &#owner, previous: Option<&History>) -> (Inputs, History) {
                            (
                                Inputs { #(#active: ::pocopine::Change {
                                    current: state.#active.clone(),
                                    previous: previous.map(|p| p.#active.clone()),
                                },)* },
                                History { #(#active: state.#active.clone(),)* },
                            )
                        }
                    }
                }
            };
        }
        #[doc(hidden)]
        #[allow(unused_imports)]
        pub(crate) use #all_macro;
    }
}
