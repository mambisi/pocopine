//! `#[ai_flow]` — turn a flow body into a `FlowHandler` constructor whose
//! context manifest is declared in the attribute.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{FnArg, Ident, ItemFn, LitStr, Meta, Token, parse_quote};

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
    let inputs: Vec<&FnArg> = func.sig.inputs.iter().collect();
    if inputs.len() != 2 {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            "`#[ai_flow]` fn must take exactly two arguments: \
             `(input: YourInput, ctx: AiFlowContext)`",
        ));
    }
    let FnArg::Typed(input_pat) = inputs[0] else {
        return Err(syn::Error::new_spanned(
            inputs[0],
            "`#[ai_flow]` fn cannot take `self`",
        ));
    };
    // The flow's typed input/output, lifted onto the marker's `FlowDef` so
    // `agenkit.flow(Marker).input(..).run()` is type-checked and schema-derivable.
    let input_ty = &input_pat.ty;
    let output_ty = util::result_ok_type(&func.sig.output)?;

    let fn_ident = &func.sig.ident;
    let vis = &func.vis;
    let id = args.id.unwrap_or_else(|| fn_ident.to_string());
    // The marker is the PascalCase of the fn name (like `#[ai_tool]`'s struct).
    let struct_ident = format_ident!("{}", util::pascal_case(&fn_ident.to_string()));

    // The body becomes a private, hidden handler fn the impl references.
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

        #vis struct #struct_ident;

        impl ::pocopine_agenkit::server::FlowDef for #struct_ident {
            const ID: &'static str = #id;
            type Input = #input_ty;
            type Output = #output_ty;
        }

        impl ::pocopine_agenkit::server::FlowKey for #struct_ident {
            type Call = ::pocopine_agenkit::server::TypedFlowCall<Self>;
            fn into_call(
                self,
                agenkit: ::pocopine_agenkit::server::Agenkit,
            ) -> Self::Call {
                ::pocopine_agenkit::server::TypedFlowCall::from_handle(agenkit)
            }
        }

        impl ::pocopine_agenkit::server::FlowHandler for #struct_ident {
            fn id(&self) -> &str {
                #id
            }

            fn descriptor(&self) -> ::pocopine_agenkit::server::FlowDescriptor {
                // Reuse `Flow::new` to derive the I/O schemas + manifest, then
                // take its descriptor — the schema-derivation logic lives in
                // one place.
                ::pocopine_agenkit::server::FlowHandler::descriptor(
                    &::pocopine_agenkit::server::Flow::new(#id, #impl_ident)
                        #public
                        #stream
                        #( .uses_agent(#agents) )*
                        #( .uses_tool(#tools) )*
                        #( .uses_retriever(#retrievers) )*
                        #( .uses_state(#state) )*,
                )
            }

            fn run_json<'flow>(
                &'flow self,
                input: ::pocopine_agenkit::serde_json::Value,
                ctx: ::pocopine_agenkit::server::AiFlowContext,
            ) -> ::pocopine_agenkit::server::BoxFuture<
                'flow,
                ::pocopine_agenkit::prelude::AgenkitResult<::pocopine_agenkit::serde_json::Value>,
            > {
                ::std::boxed::Box::pin(async move {
                    let input: #input_ty = ::pocopine_agenkit::serde_json::from_value(input)
                        .map_err(|err| ::pocopine_agenkit::prelude::AgenkitError::validation(
                            ::std::format!("flow `{}` input: {err}", #id),
                        ))?;
                    let output: #output_ty = #impl_ident(input, ctx).await?;
                    ::pocopine_agenkit::serde_json::to_value(output)
                        .map_err(|err| ::pocopine_agenkit::prelude::AgenkitError::validation(
                            ::std::format!("flow `{}` output: {err}", #id),
                        ))
                })
            }
        }
    })
}
