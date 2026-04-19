//! `pocopine-macros` — `#[component]` and `#[handlers]` attribute macros.
//!
//! `#[component]` annotates a plain struct and emits:
//!   * `impl ComponentState` (proxy `get`/`set`/`keys`/`invoke` over public
//!     fields via `serde_wasm_bindgen`)
//!   * `impl Self { pub fn register() { ... } }` that wires component, its
//!     template, and optional stylesheet into the runtime.
//!
//! `#[handlers]` annotates `impl MyStruct { ... }` and emits:
//!   * the user's impl block unchanged
//!   * an `impl HandlerDispatch` whose match-arms dispatch to each method.
//!
//! Defaults when the user passes no arguments to `#[component]`:
//!   * `name = <kebab-case of the struct ident>`
//!   * `template = "<StructIdent>.poco"` (relative to the calling `.rs`)
//!   * `style` is omitted unless explicit
//!
//! A struct ident whose kebab-case matches a known HTML element is rejected
//! at compile time, since a collision would mask a real HTML element in
//! parent templates.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream, Parser},
    parse_macro_input,
    punctuated::Punctuated,
    Expr, ExprLit, FnArg, ImplItem, ItemFn, ItemImpl, ItemStruct, Lit, LitStr, MetaNameValue,
    Pat, PatType, Token,
};

/// HTML Living Standard element names. A struct whose kebab-case ident
/// matches one of these is rejected — its custom-element tag would
/// collide with real HTML markup in parent templates.
const HTML5_ELEMENTS: &[&str] = &[
    "a", "abbr", "address", "area", "article", "aside", "audio",
    "b", "base", "bdi", "bdo", "blockquote", "body", "br", "button",
    "canvas", "caption", "cite", "code", "col", "colgroup",
    "data", "datalist", "dd", "del", "details", "dfn", "dialog",
    "div", "dl", "dt",
    "em", "embed",
    "fieldset", "figcaption", "figure", "footer", "form",
    "h1", "h2", "h3", "h4", "h5", "h6", "head", "header", "hgroup",
    "hr", "html",
    "i", "iframe", "img", "input", "ins",
    "kbd",
    "label", "legend", "li", "link",
    "main", "map", "mark", "math", "menu", "meta", "meter",
    "nav", "noscript",
    "object", "ol", "optgroup", "option", "output",
    "p", "picture", "pre", "progress",
    "q",
    "rp", "rt", "ruby",
    "s", "samp", "script", "search", "section", "select", "slot",
    "small", "source", "span", "strong", "style", "sub", "summary",
    "sup", "svg",
    "table", "tbody", "td", "template", "textarea", "tfoot", "th",
    "thead", "time", "title", "tr", "track",
    "u", "ul",
    "var", "video",
    "wbr",
];

#[derive(Default)]
struct ComponentArgs {
    name: Option<LitStr>,
    template: Option<LitStr>,
    style: Option<LitStr>,
}

impl Parse for ComponentArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs: Punctuated<MetaNameValue, Token![,]> =
            Punctuated::parse_terminated(input)?;
        let mut args = ComponentArgs::default();
        for kv in pairs {
            let lit = match kv.value {
                Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => s,
                other => {
                    return Err(syn::Error::new_spanned(
                        other,
                        "expected a string literal",
                    ));
                }
            };
            if kv.path.is_ident("name") {
                args.name = Some(lit);
            } else if kv.path.is_ident("template") {
                args.template = Some(lit);
            } else if kv.path.is_ident("style") {
                args.style = Some(lit);
            } else {
                return Err(syn::Error::new_spanned(
                    kv.path,
                    "unknown key — expected one of: name, template, style",
                ));
            }
        }
        Ok(args)
    }
}

/// Kebab-case an ident: `TodoItem` → `todo-item`.
fn kebab_case(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    for (i, c) in ident.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

#[proc_macro_attribute]
pub fn component(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match ComponentArgs::parse.parse(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemStruct);

    let struct_ident = input.ident.clone();
    let ident_str = struct_ident.to_string();
    let default_name = kebab_case(&ident_str);
    let name_str = args
        .name
        .as_ref()
        .map(|s| s.value())
        .unwrap_or_else(|| default_name.clone());

    if HTML5_ELEMENTS.binary_search(&name_str.as_str()).is_ok() {
        return syn::Error::new_spanned(
            &struct_ident,
            format!(
                "component tag `<{name_str}>` would collide with a real HTML element. \
                 Rename the struct or pass an explicit `name = \"...\"` override."
            ),
        )
        .to_compile_error()
        .into();
    }

    let template_path: LitStr = match &args.template {
        Some(s) => s.clone(),
        None => LitStr::new(&format!("{ident_str}.poco"), struct_ident.span()),
    };

    let field_idents: Vec<_> = input
        .fields
        .iter()
        .filter_map(|f| f.ident.clone())
        .collect();
    let field_names: Vec<String> = field_idents.iter().map(|i| i.to_string()).collect();

    let get_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
        }
    });

    let set_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => {
                if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(value) {
                    self.#id = v;
                }
            }
        }
    });

    let keys_arr = field_names.iter().map(|n| quote! { #n });

    let register_template_stmt = quote! {
        const _: &str = include_str!(#template_path);
        ::pocopine::__private::register_template(
            #name_str,
            ::pocopine::__private::inject_pp_data(
                include_str!(#template_path),
                #name_str,
            ),
        );
    };

    let register_style_stmt = match args.style.as_ref() {
        Some(style_path) => quote! {
            const _: &str = include_str!(#style_path);
            ::pocopine::__private::inject_style(
                #name_str,
                include_str!(#style_path),
            );
        },
        None => quote! {},
    };

    // Give each registration function a distinct name so multiple components
    // in one module don't trip the `pub fn register()` duplicate.
    let _register_fn = format_ident!("__pocopine_register_{}", struct_ident);

    let out = quote! {
        #input

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    _ => {}
                }
            }
            fn keys(&self) -> &'static [&'static str] {
                &[#(#keys_arr),*]
            }
            fn invoke(
                &mut self,
                key: &str,
                args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                <Self as ::pocopine::__private::HandlerDispatch>::invoke_handler(self, key, args)
            }
            fn mount(&mut self) {
                <Self as ::pocopine::__private::HandlerDispatch>::mount(self);
            }
            fn unmount(&mut self) {
                <Self as ::pocopine::__private::HandlerDispatch>::unmount(self);
            }
        }

        impl #struct_ident {
            /// Register this component (template, stylesheet, constructor)
            /// with the pocopine runtime. Idempotent. Call directly or via
            /// [`pocopine::App::register`].
            pub fn register() {
                ::pocopine::__private::register_component(
                    #name_str,
                    || {
                        let instance: ::std::rc::Rc<::std::cell::RefCell<#struct_ident>> =
                            ::std::rc::Rc::new(::std::cell::RefCell::new(
                                <#struct_ident as ::core::default::Default>::default()
                            ));
                        ::pocopine::__private::Scope::new(instance)
                    },
                );
                #register_template_stmt
                #register_style_stmt
            }
        }

        impl ::pocopine::__private::Component for #struct_ident {
            const NAME: &'static str = #name_str;
            fn register() {
                <#struct_ident>::register();
            }
        }
    };

    out.into()
}

#[proc_macro_attribute]
pub fn handlers(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);
    let ty = input.self_ty.clone();

    let mut arms = Vec::new();
    let mut has_on_mount = false;
    let mut has_on_unmount = false;
    for item in &input.items {
        let ImplItem::Fn(method) = item else { continue };
        let Some(_receiver) = method.sig.receiver() else {
            continue;
        };
        let extra = method.sig.inputs.len().saturating_sub(1);
        if extra > 0 {
            return syn::Error::new_spanned(
                &method.sig,
                "pocopine handlers currently support (&mut self) with no additional parameters; \
                 event args will be wired in a later milestone",
            )
            .to_compile_error()
            .into();
        }
        let ident = method.sig.ident.clone();
        let name = ident.to_string();
        match name.as_str() {
            "on_mount" => has_on_mount = true,
            "on_unmount" => has_on_unmount = true,
            _ => {}
        }
        arms.push(quote! {
            #name => {
                Self::#ident(self);
                ::pocopine::__private::JsValue::UNDEFINED
            }
        });
    }

    // Only override the trait's default if the user actually defined
    // the hook — keeps the "no lifecycle code" path a real no-op.
    let mount_impl = has_on_mount.then(|| {
        quote! {
            fn mount(&mut self) {
                Self::on_mount(self);
            }
        }
    });
    let unmount_impl = has_on_unmount.then(|| {
        quote! {
            fn unmount(&mut self) {
                Self::on_unmount(self);
            }
        }
    });

    let out = quote! {
        #input

        impl ::pocopine::__private::HandlerDispatch for #ty {
            fn invoke_handler(
                &mut self,
                key: &str,
                _args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                match key {
                    #(#arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            #mount_impl
            #unmount_impl
        }
    };

    out.into()
}

#[derive(Default)]
struct StoreArgs {
    name: Option<LitStr>,
}

impl Parse for StoreArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let pairs: Punctuated<MetaNameValue, Token![,]> =
            Punctuated::parse_terminated(input)?;
        let mut args = StoreArgs::default();
        for kv in pairs {
            let lit = match kv.value {
                Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => s,
                other => {
                    return Err(syn::Error::new_spanned(other, "expected a string literal"));
                }
            };
            if kv.path.is_ident("name") {
                args.name = Some(lit);
            } else {
                return Err(syn::Error::new_spanned(
                    kv.path,
                    "unknown key — expected: name",
                ));
            }
        }
        Ok(args)
    }
}

/// `#[store]` — declare a singleton store. Same shape as `#[component]`
/// (emits `ComponentState` + `HandlerDispatch` bridge), plus a per-type
/// `thread_local` holding the singleton, plus an `impl Store`. Unlike
/// `#[component]`, stores are not tied to a DOM element — they're
/// registered once via `App::store::<T>()` and accessed from templates
/// via `$store.<name>` and from Rust via `pocopine::store::<T>()`.
#[proc_macro_attribute]
pub fn store(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = match StoreArgs::parse.parse(attr) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };
    let input = parse_macro_input!(item as ItemStruct);

    let struct_ident = input.ident.clone();
    let ident_str = struct_ident.to_string();
    let default_name = kebab_case(&ident_str);
    let name_str = args
        .name
        .as_ref()
        .map(|s| s.value())
        .unwrap_or(default_name);

    let field_idents: Vec<_> = input
        .fields
        .iter()
        .filter_map(|f| f.ident.clone())
        .collect();
    let field_names: Vec<String> = field_idents.iter().map(|i| i.to_string()).collect();

    let get_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => ::pocopine::__private::serde_wasm_bindgen::to_value(&self.#id)
                .unwrap_or(::pocopine::__private::JsValue::UNDEFINED),
        }
    });
    let set_arms = field_idents.iter().zip(field_names.iter()).map(|(id, name)| {
        quote! {
            #name => {
                if let Ok(v) = ::pocopine::__private::serde_wasm_bindgen::from_value(value) {
                    self.#id = v;
                }
            }
        }
    });
    let keys_arr = field_names.iter().map(|n| quote! { #n });

    let out = quote! {
        #input

        impl ::pocopine::__private::ComponentState for #struct_ident {
            fn get(&self, key: &str) -> ::pocopine::__private::JsValue {
                match key {
                    #(#get_arms)*
                    _ => ::pocopine::__private::JsValue::UNDEFINED,
                }
            }
            fn set(&mut self, key: &str, value: ::pocopine::__private::JsValue) {
                match key {
                    #(#set_arms)*
                    _ => {}
                }
            }
            fn keys(&self) -> &'static [&'static str] {
                &[#(#keys_arr),*]
            }
            fn invoke(
                &mut self,
                key: &str,
                args: &::pocopine::__private::js_sys::Array,
            ) -> ::pocopine::__private::JsValue {
                <Self as ::pocopine::__private::HandlerDispatch>::invoke_handler(self, key, args)
            }
        }

        impl ::pocopine::__private::Store for #struct_ident {
            const STORE_NAME: &'static str = #name_str;

            fn __register_store() {
                // First-registration wins; subsequent calls are no-ops.
                if ::pocopine::__private::store_scope(#name_str).is_some() {
                    return;
                }
                let instance: ::std::rc::Rc<::std::cell::RefCell<#struct_ident>> =
                    ::std::rc::Rc::new(::std::cell::RefCell::new(
                        <#struct_ident as ::core::default::Default>::default()
                    ));
                let scope = ::pocopine::__private::Scope::new(instance);
                ::pocopine::__private::register_store_scope(#name_str, scope);
            }

            fn __handle() -> ::pocopine::__private::Handle<Self> {
                let scope = ::pocopine::__private::store_scope(#name_str)
                    .expect(concat!(
                        "store ", #name_str,
                        " not registered — call App::store::<_>() first"
                    ));
                let inner = scope.typed::<#struct_ident>().expect(
                    "store scope's typed state has a mismatched type",
                );
                ::pocopine::__private::Handle::new(inner, scope.id)
            }
        }
    };

    out.into()
}

/// `#[server]` — declare a function that runs on the server and is
/// callable from the client.
///
/// Expands to two `cfg`-gated definitions:
///
/// * **wasm32** — a client stub that POSTs the arguments as JSON to
///   `/_pocopine/<name>` and deserializes the JSON response as
///   `Result<R, ServerError>`. The user-supplied body is discarded on
///   this target.
/// * **non-wasm32** — the original user body, plus a helper
///   `__<name>_route(router) -> axum::Router` that registers the POST
///   route so a server binary can mount it.
///
/// The signature shape this milestone supports:
///
/// * `async fn name(arg1: T1, ..., argN: TN) -> Result<R, ServerError>`
/// * Every arg must be owned (`T`, not `&T` / `&mut T`). Args must
///   `Serialize + Deserialize`.
/// * Return type is ignored by this macro; the user is responsible for
///   having it round-trip through `serde_json`.
#[proc_macro_attribute]
pub fn server(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let vis = &input.vis;
    let sig = &input.sig;
    let body = &input.block;

    let fn_ident = sig.ident.clone();
    let fn_name_str = fn_ident.to_string();
    let route_path = format!("/_pocopine/{fn_name_str}");
    let route_ident = format_ident!("__{fn_name_str}_route");

    // Collect (pat_ident, type) pairs, rejecting self / ref args.
    let mut arg_idents = Vec::new();
    let mut arg_types = Vec::new();
    for input_arg in &sig.inputs {
        match input_arg {
            FnArg::Receiver(r) => {
                return syn::Error::new_spanned(
                    r,
                    "`#[server]` functions cannot take `self` — they are free functions",
                )
                .to_compile_error()
                .into();
            }
            FnArg::Typed(PatType { pat, ty, .. }) => {
                // Reject &T / &mut T.
                if matches!(**ty, syn::Type::Reference(_)) {
                    return syn::Error::new_spanned(
                        ty,
                        "`#[server]` args must be owned — reference types are not supported \
                         (clone-into or take an owned type instead)",
                    )
                    .to_compile_error()
                    .into();
                }
                let Pat::Ident(pat_ident) = &**pat else {
                    return syn::Error::new_spanned(
                        pat,
                        "`#[server]` args must be simple identifiers",
                    )
                    .to_compile_error()
                    .into();
                };
                arg_idents.push(pat_ident.ident.clone());
                arg_types.push((**ty).clone());
            }
        }
    }

    // `(arg1, arg2, ...)` — tuple of idents, with trailing comma for
    // single-element tuples so the macro output is grammatical.
    let args_tuple_value = if arg_idents.is_empty() {
        quote! { () }
    } else if arg_idents.len() == 1 {
        let only = &arg_idents[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_idents),* ) }
    };

    // `(T1, T2, ...)` — matching tuple type.
    let args_tuple_type = if arg_types.is_empty() {
        quote! { () }
    } else if arg_types.len() == 1 {
        let only = &arg_types[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_types),* ) }
    };

    // `(arg1, arg2, ...)` destructuring pattern for the axum handler's
    // `Json<(T1, T2, ...)>` body extractor.
    let destructure = if arg_idents.is_empty() {
        quote! { _args }
    } else if arg_idents.len() == 1 {
        let only = &arg_idents[0];
        quote! { (#only,) }
    } else {
        quote! { ( #(#arg_idents),* ) }
    };

    let sig_without_body = quote! { #vis #sig };

    let client = quote! {
        #[cfg(target_arch = "wasm32")]
        #sig_without_body {
            ::pocopine::fetch::call::<#args_tuple_type, _>(
                #route_path,
                &#args_tuple_value,
            ).await
        }
    };

    // On the server we preserve the user's body, plus emit a route helper.
    // The extractor destructures the JSON body into our ident tuple, then
    // we call the original function by name — Rust method resolution picks
    // the non-wasm32 definition.
    let server = quote! {
        #[cfg(not(target_arch = "wasm32"))]
        #vis #sig #body

        #[cfg(not(target_arch = "wasm32"))]
        #[doc(hidden)]
        pub fn #route_ident(
            router: ::pocopine_server::axum::Router,
        ) -> ::pocopine_server::axum::Router {
            router.route(
                #route_path,
                ::pocopine_server::axum::routing::post(
                    |::pocopine_server::axum::Json(#destructure):
                        ::pocopine_server::axum::Json<#args_tuple_type>| async move {
                        let result = #fn_ident( #(#arg_idents),* ).await;
                        ::pocopine_server::axum::Json(result)
                    },
                ),
            )
        }
    };

    let out = quote! {
        #client
        #server
    };
    out.into()
}
