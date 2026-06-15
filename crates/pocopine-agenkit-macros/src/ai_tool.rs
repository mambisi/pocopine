//! `#[ai_tool]` — derive an `AiTool` impl from a typed async function.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{FnArg, ItemFn, Meta, Token};

use crate::util;

#[derive(Default)]
struct Args {
    id: Option<String>,
    description: Option<String>,
    side_effecting: bool,
}

fn parse_args(attr: TokenStream) -> syn::Result<Args> {
    let mut args = Args::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    for meta in metas {
        match &meta {
            Meta::Path(p) if p.is_ident("side_effecting") => args.side_effecting = true,
            Meta::NameValue(nv) if nv.path.is_ident("id") => {
                args.id = Some(util::lit_string(&nv.value)?);
            }
            Meta::NameValue(nv) if nv.path.is_ident("description") => {
                args.description = Some(util::lit_string(&nv.value)?);
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "unsupported `#[ai_tool]` argument; expected `id = \"...\"`, \
                     `description = \"...\"`, or `side_effecting`",
                ));
            }
        }
    }
    Ok(args)
}

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args = parse_args(attr)?;
    let func: ItemFn = syn::parse2(item)?;

    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "`#[ai_tool]` requires an `async fn`",
        ));
    }

    let inputs: Vec<&FnArg> = func.sig.inputs.iter().collect();
    if inputs.len() != 2 {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            "`#[ai_tool]` fn must take exactly two arguments: \
             `(input: YourInput, ctx: AiToolContext)`",
        ));
    }
    let input_arg = &inputs[0];
    let ctx_arg = &inputs[1];
    let FnArg::Typed(input_pat) = input_arg else {
        return Err(syn::Error::new_spanned(
            input_arg,
            "`#[ai_tool]` fn cannot take `self`",
        ));
    };
    let input_ty = &input_pat.ty;
    let output_ty = util::result_ok_type(&func.sig.output)?;

    let fn_ident = &func.sig.ident;
    let id = args.id.unwrap_or_else(|| fn_ident.to_string());
    let description = args
        .description
        .unwrap_or_else(|| util::doc_string(&func.attrs));
    let struct_ident = format_ident!("{}", util::pascal_case(&fn_ident.to_string()));
    let vis = &func.vis;
    let body = &func.block;
    let side_effect = if args.side_effecting {
        quote!(.side_effecting())
    } else {
        quote!()
    };

    Ok(quote! {
        #vis struct #struct_ident;

        impl ::pocopine_agenkit::server::AiTool for #struct_ident {
            const ID: &'static str = #id;
            type Input = #input_ty;
            type Output = #output_ty;

            fn descriptor() -> ::pocopine_agenkit::prelude::ToolDescriptor {
                ::pocopine_agenkit::prelude::ToolDescriptor::new(#id, #description) #side_effect
            }

            fn call(
                &self,
                #input_arg,
                #ctx_arg,
            ) -> ::pocopine_agenkit::server::BoxFuture<
                '_,
                ::pocopine_agenkit::prelude::AgenkitResult<#output_ty>,
            > {
                ::std::boxed::Box::pin(async move #body)
            }
        }
    })
}
