//! `#[ai_flow]` — turn a flow body into a `FlowHandler` constructor whose
//! context manifest is declared in the attribute.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Ident, ItemFn, LitStr, Meta, Token, parse_quote};

use crate::util;

#[derive(Default)]
struct Args {
    id: Option<String>,
    public: bool,
    agents: Vec<String>,
    tools: Vec<String>,
    retrievers: Vec<String>,
    state: Vec<String>,
    stream: Option<String>,
}

fn string_list(meta: &syn::MetaList) -> syn::Result<Vec<String>> {
    let items = meta.parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)?;
    Ok(items.into_iter().map(|s| s.value()).collect())
}

fn parse_args(attr: TokenStream) -> syn::Result<Args> {
    let mut args = Args::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    for meta in metas {
        match &meta {
            Meta::Path(p) if p.is_ident("public") => args.public = true,
            Meta::NameValue(nv) if nv.path.is_ident("id") => {
                args.id = Some(util::lit_string(&nv.value)?);
            }
            Meta::NameValue(nv) if nv.path.is_ident("stream") => {
                args.stream = Some(util::lit_string(&nv.value)?);
            }
            Meta::List(list) if list.path.is_ident("agents") => args.agents = string_list(list)?,
            Meta::List(list) if list.path.is_ident("tools") => args.tools = string_list(list)?,
            Meta::List(list) if list.path.is_ident("retrievers") => {
                args.retrievers = string_list(list)?;
            }
            Meta::List(list) if list.path.is_ident("state") => args.state = string_list(list)?,
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "unsupported `#[ai_flow]` argument; expected `public`, `id = \"...\"`, \
                     `stream = \"...\"`, or `agents(..)` / `tools(..)` / `retrievers(..)` / `state(..)`",
                ));
            }
        }
    }
    Ok(args)
}

fn stream_variant(stream: &str, span: proc_macro2::Span) -> syn::Result<Ident> {
    let variant = match stream {
        "final_only" => "FinalOnly",
        "output_deltas" => "OutputDeltas",
        "progress" => "Progress",
        "debug_safe" => "DebugSafe",
        other => {
            return Err(syn::Error::new(
                span,
                format!(
                    "unknown stream mode `{other}`; expected one of \
                     final_only, output_deltas, progress, debug_safe"
                ),
            ));
        }
    };
    Ok(Ident::new(variant, span))
}

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args = parse_args(attr)?;
    let func: ItemFn = syn::parse2(item)?;

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "`#[ai_flow]` requires an `async fn`",
        ));
    }
    if func.sig.inputs.len() != 2 {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            "`#[ai_flow]` fn must take exactly two arguments: \
             `(input: YourInput, ctx: AiFlowContext)`",
        ));
    }

    let fn_ident = &func.sig.ident;
    let vis = &func.vis;
    let id = args.id.unwrap_or_else(|| fn_ident.to_string());

    // The body becomes a private, hidden handler fn the constructor references.
    let impl_ident = Ident::new(&format!("__{fn_ident}_handler"), fn_ident.span());
    let mut handler = func.clone();
    handler.sig.ident = impl_ident.clone();
    handler.vis = syn::Visibility::Inherited;
    handler.attrs = vec![parse_quote!(#[doc(hidden)])];

    let public = args.public.then(|| quote!(.public()));
    let agents = &args.agents;
    let tools = &args.tools;
    let retrievers = &args.retrievers;
    let state = &args.state;
    let stream = match &args.stream {
        Some(s) => {
            let variant = stream_variant(s, fn_ident.span())?;
            Some(quote!(.stream_mode(::pocopine_agenkit::prelude::StreamMode::#variant)))
        }
        None => None,
    };

    Ok(quote! {
        #handler

        #vis fn #fn_ident() -> impl ::pocopine_agenkit::server::FlowHandler {
            ::pocopine_agenkit::server::Flow::new(#id, #impl_ident)
                #public
                #stream
                #( .uses_agent(#agents) )*
                #( .uses_tool(#tools) )*
                #( .uses_retriever(#retrievers) )*
                #( .uses_state(#state) )*
        }
    })
}
