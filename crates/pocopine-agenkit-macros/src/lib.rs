//! `pocopine-agenkit-macros` — attribute macros that cut the boilerplate of
//! authoring Agenkit tools, flows, and the public-flow server bridge. Sugar
//! over the hand-written `pocopine-agenkit` API (RFC-093 §D14); the hand-written
//! traits stay fully usable.
//!
//! - [`macro@ai_tool`] — turns a typed `async fn` into an `AiTool` impl.
//! - [`macro@ai_flow`] — turns a flow body into a `FlowHandler` constructor with
//!   its context manifest declared in the attribute.
//!
//! Re-exported through `pocopine_agenkit::{ai_tool, ai_flow}`; the generated
//! code references the runtime via absolute `::pocopine_agenkit` paths, so it
//! compiles wherever the host runtime is in scope.

use proc_macro::TokenStream;

mod ai_flow;
mod ai_server_flow;
mod ai_tool;
mod util;

/// Derive an `AiTool` from a typed async function.
///
/// ```ignore
/// /// Look up a fact about a term.       // becomes the tool description
/// #[ai_tool]
/// async fn lookup(input: LookupIn, ctx: AiToolContext) -> AgenkitResult<LookupOut> {
///     Ok(LookupOut { fact: format!("{} use presigned URLs", input.term) })
/// }
/// // register: .tool(Lookup)
/// ```
///
/// The generated unit struct is the PascalCase of the fn name. The tool id
/// defaults to the fn name, the description to the doc comment. Override with
/// `#[ai_tool(id = "lookup", description = "...", side_effecting)]`.
#[proc_macro_attribute]
pub fn ai_tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    ai_tool::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Turn a flow body into a `FlowHandler` constructor, with the context manifest
/// declared in the attribute.
///
/// ```ignore
/// #[ai_flow(public, agents("researcher"), tools("lookup"))]
/// async fn research(input: Question, ctx: AiFlowContext) -> AgenkitResult<Answer> {
///     ctx.agent::<Researcher>().input(input).run().await
/// }
/// // register: .flow(research())
/// ```
///
/// The fn becomes `fn research() -> impl FlowHandler`. The flow id defaults to
/// the fn name (override with `id = "..."`). Declare resources with
/// `agents(..)`, `tools(..)`, `retrievers(..)`, `state(..)`, mark the flow
/// `public`, and optionally set `stream = "progress"`.
#[proc_macro_attribute]
pub fn ai_flow(attr: TokenStream, item: TokenStream) -> TokenStream {
    ai_flow::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Generate the typed public-flow bridge: a `#[server]`-symmetric fn that
/// fetches the principal-aware flow route on wasm and calls `run_public_flow`
/// on the host.
///
/// ```ignore
/// #[ai_server_flow(flow = "summarize", runtime = crate::agenkit)]
/// pub async fn summarize(input: SummarizeInput) -> ServerResult<Summary> {}
/// // browser: summarize(input).await  → POST /__pocopine/agenkit/v1/flow/summarize
/// ```
///
/// `flow` defaults to the fn name; `runtime` is a path to a `fn() -> Agenkit`
/// the host body calls. The author writes the signature (input type + return
/// `ServerResult<Output>`); the macro fills both bodies. The flow runs under the
/// request principal via the route's DC-5 adapter.
#[proc_macro_attribute]
pub fn ai_server_flow(attr: TokenStream, item: TokenStream) -> TokenStream {
    ai_server_flow::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
