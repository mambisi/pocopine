---
title: "Traceable flow design"
description: "Declare every resource a flow uses in its context manifest, route all access through ctx, and read the advisory context-gap diagnostic — so a flow's behavior is fully visible in one trace tree."
---

# Traceable flow design

Agenkit's value — one correlated trace tree, principal-scoped access, redacted
streams — holds only if a flow does its work **through `ctx`** and **declares**
what it touches. This page is the contract (§D6).

## The two rules

1. **Everything goes through `ctx`.** Generation (`ctx.ai`), custom work
   (`ctx.step`), retrieval (`ctx.retrieve`), agents (`ctx.agent`), app resources
   (`ctx.state`) — each emits a trace event under the run's tree and runs under
   the caller principal. A raw read inside the flow body does neither.
2. **Declare resources in the manifest.** A flow declares the tools, retrievers,
   agents, and state keys it uses; the runtime checks framework-mediated access
   against that declaration and flags anything undeclared.

```mermaid
graph TB
  subgraph good["✅ through ctx — traced + principal-scoped"]
    b1["ctx.retrieve::&lt;Docs&gt;()"] --> t1["ai_retrieval_started/completed"]
    b2["ctx.state::&lt;Db&gt;(\"db\")"] --> t2["recorded vs manifest"]
  end
  subgraph bad["❌ raw read in the body — invisible"]
    r1["sqlx::query(&amp;pool)"] -.->|no trace event| X1["not in the tree"]
    r1 -.->|no identity| X2["not principal-scoped"]
  end
```

## Declare what you use

With the macro, declare in the attribute:

```rust
#[ai_flow(
    public,
    agents("researcher"),
    tools("lookup"),
    retrievers("project_docs"),
    state("rate_limiter"),
)]
async fn answer(input: Question, ctx: AiFlowContext) -> AgenkitResult<Answer> { … }
```

Hand-written, the same declarations are builder calls:

```rust
Flow::new("answer", answer)
    .public()
    .uses_agent("researcher")
    .uses_tool("lookup")
    .uses_retriever("project_docs")
    .uses_state("rate_limiter")
```

These populate the flow's `ContextManifest`, which is part of its public
descriptor — the declared shape of what the flow can reach.

## The context-gap diagnostic

When the flow accesses a framework-mediated resource (`ctx.retrieve::<R>()`,
`ctx.state(key)`) that it did **not** declare, the runtime still runs it — the
check is **advisory** — but emits an `ai_context_gap` trace event naming the
flow and the undeclared resource:

```rust
// flow declared no retrievers, but the body calls one:
let docs = ctx.retrieve::<ProjectDocs>().query(q).run().await?;
// → still runs, and emits:  ai_context_gap { flow_id, resource_id: "project_docs", … }
```

Treat a context gap as a lint: either add the `uses_*` declaration (the resource
*is* intended) or remove the access. A flow whose trace has zero gaps is a flow
whose declared surface matches its real behavior — which is what makes the trace
trustworthy for debugging, evals, and audits.

## Why hidden reads are the wrong path

The tempting shortcut is a raw read inside the flow body — `sqlx::query`,
`std::fs::read`, a global client, `reqwest` to your own API. It compiles and
"works", but:

- **It's invisible.** No `ai_step_*` event, so the trace tree shows a gap in
  reasoning with no explanation — useless for debugging a bad answer or building
  an eval.
- **It's not principal-scoped.** The read runs as the process, not the caller,
  so per-user authorization and rate limits silently don't apply (§D5/§D10).
- **It's not declared.** Nothing records that this flow depends on that
  resource, so the manifest lies about the flow's surface.

Route it through `ctx` instead:

- A database/file/HTTP read your flow needs → register it as app **state**
  (`builder.state("db", pool)`), declare `uses_state("db")`, and fetch with
  `ctx.state::<Pool>("db")`. Wrap the actual query in `ctx.step("load_user", …)`
  so the work itself is a traced step.
- A search/lookup the **model** should be able to invoke → expose it as a tool
  or retriever (see [Retrieval: direct vs. tool](./retrieval.md)).

## Custom work belongs in `ctx.step`

Any non-trivial computation between AI calls — assembling a prompt, scoring,
transforming retrieved hits — goes in a named step so it appears in the tree:

```rust
let context = ctx
    .step("assemble_context", async move {
        Ok::<_, AgenkitError>(docs.hits.iter().map(|h| h.content.as_text()).collect::<Vec<_>>().join("\n"))
    })
    .await?;
```

The result: a flow run emits a single correlated tree —
`ai_flow_started → ai_retrieval_* → ai_step_* → ai_model_request/response →
ai_flow_completed` — that reads like the flow's logic, because it *is* the
flow's logic.
