//! `#[ai_server_flow]` — generate the typed public-flow bridge.
//!
//! `#[server]`-symmetric: on wasm the body fetches the generic flow route (the
//! DC-5 principal-aware endpoint); on the host it calls `run_public_flow` via
//! an app-provided runtime accessor. The author writes the signature; the macro
//! fills both bodies.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Expr, FnArg, ItemFn, Meta, Pat, Token};

use crate::util;

#[derive(Default)]
struct Args {
    flow: Option<String>,
    runtime: Option<Expr>,
}

fn parse_args(attr: TokenStream) -> syn::Result<Args> {
    let mut args = Args::default();
    if attr.is_empty() {
        return Ok(args);
    }
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated.parse2(attr)?;
    for meta in metas {
        match &meta {
            Meta::NameValue(nv) if nv.path.is_ident("flow") => {
                args.flow = Some(util::lit_string(&nv.value)?);
            }
            Meta::NameValue(nv) if nv.path.is_ident("runtime") => {
                args.runtime = Some(nv.value.clone());
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    meta,
                    "unsupported `#[ai_server_flow]` argument; expected \
                     `flow = \"...\"` and `runtime = <path to a fn returning Agenkit>`",
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
            "`#[ai_server_flow]` requires an `async fn`",
        ));
    }

    let inputs: Vec<&FnArg> = func.sig.inputs.iter().collect();
    if inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &func.sig.inputs,
            "`#[ai_server_flow]` fn must take exactly one input argument: `(input: YourInput)`",
        ));
    }
    let FnArg::Typed(pat_ty) = inputs[0] else {
        return Err(syn::Error::new_spanned(
            inputs[0],
            "`#[ai_server_flow]` fn cannot take `self`",
        ));
    };
    let Pat::Ident(pat_ident) = pat_ty.pat.as_ref() else {
        return Err(syn::Error::new_spanned(
            &pat_ty.pat,
            "the input argument must be a simple identifier, e.g. `input: YourInput`",
        ));
    };
    let arg_ident = &pat_ident.ident;
    let input_ty = &pat_ty.ty;
    let output_ty = util::result_ok_type(&func.sig.output)?;

    let runtime = args.runtime.ok_or_else(|| {
        syn::Error::new_spanned(
            &func.sig,
            "`#[ai_server_flow]` requires `runtime = <path to a fn returning Agenkit>`",
        )
    })?;

    let fn_ident = &func.sig.ident;
    let flow_id = args.flow.unwrap_or_else(|| fn_ident.to_string());
    let url = format!("/__pocopine/agenkit/v1/flow/{flow_id}");
    let vis = &func.vis;
    let attrs = &func.attrs;
    let sig = &func.sig;

    Ok(quote! {
        // Browser: POST the input to the generic principal-aware flow route and
        // decode the `Result<O, ServerError>` envelope (matches `#[server]`).
        #[cfg(target_arch = "wasm32")]
        #(#attrs)*
        #vis #sig {
            ::pocopine::fetch::call::<#input_ty, #output_ty>(#url, &#arg_ident).await
        }

        // Host: call the runtime directly. The caller scopes the principal with
        // `pocopine_agenkit::server::with_principal` when one is needed.
        #[cfg(not(target_arch = "wasm32"))]
        #(#attrs)*
        #vis #sig {
            (#runtime)()
                .run_public_flow::<#input_ty, #output_ty>(#flow_id, #arg_ident)
                .await
        }
    })
}
