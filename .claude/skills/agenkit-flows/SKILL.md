---
name: agenkit-flows
description: >-
  Use when authoring or touching Pocopine Agenkit AI work — flows, the
  #[ai_flow] / #[ai_tool] macros, AiAgent / AiRetriever, ctx.ai / ctx.step /
  ctx.parallel / ctx.reduce / ctx.retrieve / ctx.agent, agent threads, the
  streaming route, or exposing a flow through a #[server] fn. Keeps work on the
  traceable, principal-scoped path the runtime is designed around (RFC-093) and
  off the easy-but-wrong path (hidden DB/file reads, undeclared resources,
  secrets in trace/stream fields).
---

# Authoring Agenkit flows

Agenkit's guarantees — one correlated trace tree, principal-scoped access,
client-safe streams — hold only when work goes **through `ctx`** and resources
are **declared**. Full guide: `docs/guides/agenkit/`.

## Do this, not that

| Goal | Do | Don't |
| ---- | -- | ----- |
| Read a DB / file / your own API inside a flow | register it as state, `uses_state("db")`, fetch via `ctx.state::<T>("db")`, do the read in `ctx.step(name, …)` | `sqlx::query` / `std::fs` / a global client in the flow body (untraced, not principal-scoped, undeclared) |
| Look something up for the model | a registered `AiRetriever` — `ctx.retrieve::<R>()` (flow decides) or `retriever_as_tool::<R>()` in an agent's `.tools([…])` (model decides) | an ad-hoc vector/search call in the body |
| Define a tool / flow | `#[ai_tool]` / `#[ai_flow]` (or `impl AiTool` / `Flow::new`) | a `.poco` template or a bespoke trait — flows are plain Rust |
| Call a flow | `agenkit.flow(Marker).input(x).run()` (typed); `flow("id")` only for dynamic/runner use | a stringly-typed call when the flow is statically known |
| Expose a flow to clients | a `#[server]` fn that runs `active_plugin::<Agenkit>()…flow(M).input(i).run()` and maps errors with `to_server_error` | giving the flow its own endpoint, or returning the raw `AgenkitError` |
| Thread the caller identity | install `agenkit_server_plugin(agenkit)` (provides the handle + principal layer) | reading `Principal` in the handler and passing it down |
| Stream progress | declare `stream = "progress"` (cap), stream via `.stream(tx)`, mount `ai_flow_stream_router` | inventing a stream type, or raising visibility past the declared `StreamMode` |
| Combine parallel results | `ctx.reduce(name, c).fold(…)` (deterministic) or `.schema::<O>()` (model judge) | a hand-rolled join that drops failures or leaves branches uncancelled |

## Non-negotiables

- **Declare resources.** `#[ai_flow(tools(..), agents(..), retrievers(..), state(..))]`
  (or `.uses_tool/.uses_retriever/.uses_agent/.uses_state`). An `ai_context_gap`
  trace event means an accessed resource wasn't declared — add the declaration
  or remove the access. See `docs/guides/agenkit/traceable-flows.md`.
- **Flows are internal; `#[server]` is the boundary.** `.public()` only governs
  the SSE streaming route; never auto-generate flow endpoints.
- **`FlowStreamEvent` is client-safe by construction** — ids/kinds/counts/output
  only. Never put a prompt, tool args, retrieved content, or a credential in a
  trace `with_field` or anywhere client-facing (§D8/§D10).
- **Errors:** map at the boundary with `to_server_error` — provider/config/etc.
  collapse to a stable kind; internals never cross.
- **Bound cost:** agents `.max_steps(n)`; parallel `.max_concurrency(n)` +
  `.min_success(m)` + `.timeout(..)`; `builder.allow_models([..])`.
- **Threads are owner-scoped** (one principal can't touch another's) and ids are
  opaque; `InMemoryThreadStore` is dev-only — use a real `AgentThreadStore`.
- **Providers:** default to `AnthropicProvider` (recommended). Credentials are
  read from env, server-only. Use `MockProvider` in tests.
- **`#[ai_flow]` marker = PascalCase of the fn name** — it must not collide with
  the flow's input/output type names (rename the fn or set `id = "…"`).

## Verify before pushing

The runtime is host-only, but the shared contracts compile on wasm too — run
both, plus the macro/contract tests (host `cargo test` does **not** catch
wasm32-cfg or fmt issues CI enforces):

```
cargo test -p pocopine-agenkit -p pocopine-agenkit-core \
           -p pocopine-agenkit-oai -p pocopine-agenkit-anthropic -p pocopine-agenkit-macros
cargo clippy -p pocopine-agenkit --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo fmt -p pocopine-agenkit -- --check
```
