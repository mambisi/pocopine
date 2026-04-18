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
//!   * `template = "<StructIdent>.pcx"` (relative to the calling `.rs`)
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
    Expr, ExprLit, ImplItem, ItemImpl, ItemStruct, Lit, LitStr, MetaNameValue, Token,
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
        None => LitStr::new(&format!("{ident_str}.pcx"), struct_ident.span()),
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
        }

        impl #struct_ident {
            /// Register this component (template, stylesheet, constructor)
            /// with the pocopine runtime. Call from your
            /// `#[wasm_bindgen(start)]` function before `pocopine::run()`.
            pub fn register() {
                ::pocopine::__private::register_component(
                    #name_str,
                    || ::std::rc::Rc::new(::std::cell::RefCell::new(
                        <#struct_ident as ::core::default::Default>::default()
                    )),
                );
                #register_template_stmt
                #register_style_stmt
            }

            /// The component's runtime name (kebab-case of the struct ident
            /// unless overridden). Also the tag name in parent templates.
            pub const NAME: &'static str = #name_str;
        }
    };

    out.into()
}

#[proc_macro_attribute]
pub fn handlers(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemImpl);
    let ty = input.self_ty.clone();

    let mut arms = Vec::new();
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
        arms.push(quote! {
            #name => {
                Self::#ident(self);
                ::pocopine::__private::JsValue::UNDEFINED
            }
        });
    }

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
        }
    };

    out.into()
}
