# RFC 093 - Pocopine Agenkit plan

* **Status:** Draft
* **Author:** Pocopine AI working group
* **Tracking branch:** `pocopine-agenkit`
* **Related:** [RFC 002 (application framework, stores, server functions)](./rfc-002-app-stores-servers.md),
  [RFC 066 (server-function auth and access policy)](./rfc-066-server-function-auth.md),
  [RFC 069 (observability, logging, and analytics)](./rfc-069-observability.md),
  [RFC 076 (app plugin lifecycle)](./rfc-076-app-plugin-lifecycle.md),
  [RFC 077 (server plugin lifecycle)](./rfc-077-server-plugin-lifecycle.md)
* **References:** [Google Genkit](https://github.com/genkit-ai/genkit),
  [Rig](https://github.com/0xPlaygrounds/rig),
  [LangChain agents](https://docs.langchain.com/oss/python/langchain/agents),
  [OpenAI Agents SDK](https://openai.github.io/openai-agents-python/agents/),
  [LlamaIndex agents](https://developers.llamaindex.ai/python/framework/module_guides/deploying/agents/),
  [Semantic Kernel Agent Framework](https://learn.microsoft.com/en-us/semantic-kernel/frameworks/agent/)

## Summary

Add **Pocopine Agenkit**: a Pocopine-native generative-AI kit for
building AI features, AI server functions, local prompt/flow tooling,
and future focused products such as an agent-powered Pocopine debugger.

This RFC now scopes the first implementation to exactly two crates:

* `pocopine-agenkit-core`;
* `pocopine-agenkit`.

The goal is one Pocopine platform surface: app flows, server functions,
tools, model calls, traces, analytics, evals, and future debugging
products all compose through the same framework-owned contracts.

The target is not "an LLM client crate." The target is the Pocopine
equivalent of a batteries-included AI app stack:

* typed model/provider abstraction;
* prompts, tools, agents, threads, and flows;
* structured output and schema validation;
* RAG hooks without binding the framework to one vector database;
* server-function integration so apps call AI flows like normal
  Pocopine app functions;
* local traces, evals, and replayable run records;
* server-only provider credentials.

Google Genkit is the primary product and code/API reference: unified
providers, flows, tools, prompts, structured output, RAG, a local
developer UI, and production monitoring. Pocopine should learn from
Genkit's code organization and authoring ergonomics where they fit, but
not inherit a separate telemetry platform. Rig is the Rust
provider/reference crate to evaluate behind Pocopine-owned traits.

## Motivation

Pocopine already has the hard parts of a full-stack Rust app framework:
typed components, server functions, auth policy, jobs, storage,
observability, deploy adapters, sync, and VS Code integration. AI app
features should use those surfaces instead of becoming detached ad hoc
Node scripts or one-off provider calls inside server handlers.

A Pocopine app should be able to configure AI once:

```rust
use pocopine_agenkit::prelude::*;

pub fn agenkit() -> Agenkit {
    Agenkit::builder()
        .default_model(ModelRef::new("local/default"))
        .provider(local_provider())
        .tool(search_docs)
        .flow(summarize_doc)
        .build()
}
```

and then call an AI flow with the same operational shape as other
Pocopine server work: auth, tracing, typed payloads, local dev
diagnostics, deployability, and replayable errors.

The second motivation is product focus. A general coding-agent harness
is not the first Pocopine product. Pocopine can already use reusable
skill packs, and many agent shells can consume skill-like context. The
first Agenkit work should create the AI runtime contracts that later
products can depend on, especially a possible **Pocopine Debugger** that
uses Pocopine traces, server-function metadata, build output, and RFC
context to help diagnose application failures.

## Decisions

### D1 - Working Name

The product name is **Pocopine Agenkit** (spelling resolved), matching the
branch and the crate names `pocopine-agenkit-core` / `pocopine-agenkit`. The
earlier draft alternatives (`Agentkit`, `Genkit for Pocopine`) are dropped.

### D2 - Two-Crate Scope

Use Pocopine infrastructure crate names, not `pine-*`, because the
feature is server/tooling infrastructure rather than a browser-runtime
component library.

The first implementation has exactly two crates. Keeping the first
slice narrow avoids committing to an agent harness, product UI, proc
macros, or provider crate names before the core contracts are proven.

| Layer | Crate | Reason |
|-------|-------|--------|
| 0 | `pocopine-agenkit-core` | Stable shared contracts: content parts, messages, model refs, schema descriptors, tool descriptors, flow descriptors, run ids, trace events, usage/cost metadata, errors, and client-safe payload structs. This crate must stay dependency-light and wasm-friendly so generated clients, tests, and tooling can use the same types. |
| 1 | `pocopine-agenkit` | The unified runtime facade app authors depend on. It owns `Agenkit`, `Ai`, provider traits, provider registry, flow registry, tool execution, trace recording, server-function helpers, redaction policy, and public re-exports from core. |

Dependency direction:

```text
pocopine-agenkit-core
  <- pocopine-agenkit
```

The facade crate re-exports the stable authoring surface:

```rust
use pocopine_agenkit::prelude::*;
```

Normal apps should depend on `pocopine-agenkit`, not directly on
`pocopine-agenkit-core`. Internal tests, generated clients, and future
tooling can import core when they need stable payload types without the
runtime.

Deferred crates are explicitly out of this RFC: proc macros, provider
adapter crates, a code-agent runtime, a debugger product crate, and a
commercial UI. They should depend on the two crates above once the
runtime proves the trait shape.

### D3 - Unified API Surface

The user-facing API must have one canonical entry point: `Agenkit`.
Apps configure it once and then call `ai()` or registered flow handles
from server functions, jobs, evals, and future debugger products.

Generation:

```rust
let answer = agenkit
    .ai()
    .model(ModelRef::new("local/default"))
    .system("Answer using the project's docs.")
    .prompt("How do uploads work?")
    .generate_text()
    .await?;
```

Structured output:

```rust
let summary: Summary = agenkit
    .ai()
    .prompt(input.prompt())
    .schema::<Summary>()
    .generate_structured()
    .await?;
```

Manual tool registration:

```rust
async fn search_docs(input: SearchDocs, ctx: AiToolContext) -> AgenkitResult<Vec<SearchHit>> {
    ctx.state::<DocIndex>("doc_index")?.search(input.query).await
}
```

Manual flow registration:

```rust
async fn summarize_doc(input: SummarizeDoc, ctx: AiFlowContext) -> AgenkitResult<Summary> {
    ctx.ai()
        .prompt(input.prompt())
        .schema::<Summary>()
        .generate_structured()
        .await
}
```

The same underlying flow descriptor should power all entry points:

* app server calls;
* server jobs;
* eval dataset runs;
* debugger-assisted diagnosis;
* trace replay.

### D4 - App Authoring Concepts

Agenkit exposes six authoring concepts through that unified facade:

| Concept | Pocopine surface | Purpose |
|---------|------------------|---------|
| Model | `ModelRef`, provider config, runtime registry | Select the provider/model without wiring app code to vendor clients. |
| Prompt | typed prompt templates plus runtime variables | Keep prompt text inspectable, reusable, and testable. |
| Tool | manual `Tool` registration | Let models call app-owned functions with typed args and auth policy. |
| Agent | typed `AiAgent` registration | A runnable AI decision unit with instructions, tools, input/output schemas, limits, and optional memory/session policy. |
| Thread | `AgentThread` and thread stores | Optional conversation/session state for an agent across turns. |
| Flow | manual `Flow` registration | The app-callable unit for generation, RAG, tools, and multi-step workflows. |

Flows are the bridge to Pocopine apps. A flow can be exposed as a
server function, but not every flow must be public. Private flows can
back jobs, tools, evals, and internal automation.

`AiAgent` is not the same thing as a flow. A flow is the typed
orchestration and app boundary: auth policy, server-function exposure,
stream contract, trace lifetime, and final input/output shape. An
`AiAgent` is a typed runnable unit inside a flow or future product: it
can choose tool calls, produce structured output, participate in
parallel branches, and later carry memory/session behavior if the
runtime needs it.

`AgentThread` is not the same thing as an agent or a flow. A thread is
optional conversation/session state: message history, safe attachments,
checkpoints, resumable ids, and retention policy. Stateless one-shot
agents do not need a thread. Chat-like products, debugger sessions, and
longer investigations can attach a thread to an agent run.

The separation is:

| Layer | Owns | Strength |
|-------|------|----------|
| `Flow` | App operation, auth, public schema, streaming contract, trace lifetime, orchestration. | Reliable server-function boundary and production behavior. |
| `AiAgent` | AI behavior: instructions, model alias, tools, structured output, guardrails, limits. | Reusable typed AI role that can run sequentially, in parallel, or inside products. |
| `AgentThread` | Conversation/session state across turns. | Continuity for debugger/chat sessions without forcing every flow to become stateful. |

Proc macros such as `#[ai_tool]` and `#[ai_flow]` are future authoring
sugar. Manual registration must work first.

### D5 - Tool And Retrieval Traits

Tools and retrieval need clear separate contracts.

A **tool** is an action an agent or flow may invoke with typed input and
typed output. Tools can be read-only or side-effecting, so they need an
explicit policy boundary.

A **retriever** is a read-oriented context source used for RAG,
debugging evidence, docs lookup, trace lookup, search, or ranking.
Retrievers can be exposed as tools when an agent should decide when to
search, but retrieval is not inherently a tool. Keeping retrieval
separate lets flows perform deterministic context loading, eval replay,
and debugger evidence collection without routing every lookup through a
model tool call.

An **embedder** is an optional provider-backed or local capability that
turns text or multimodal content into vectors. Agenkit should support
embedding-powered retrieval without binding the framework to one vector
database.

Typed tool shape:

```rust
pub trait AiTool: Send + Sync + 'static {
    const ID: &'static str;

    type Input: Send + 'static;
    type Output: Send + 'static;

    fn descriptor() -> ToolDescriptor;

    async fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext<'_>,
    ) -> AgenkitResult<Self::Output>;
}
```

Typed retriever shape:

```rust
pub trait AiRetriever: Send + Sync + 'static {
    const ID: &'static str;

    type Query: Send + 'static;
    type Hit: Send + 'static;

    fn descriptor() -> RetrieverDescriptor;

    async fn retrieve(
        &self,
        query: Self::Query,
        ctx: RetrievalContext<'_>,
    ) -> AgenkitResult<RetrievalSet<Self::Hit>>;
}
```

Typed embedder shape:

```rust
pub trait AiEmbedder: Send + Sync + 'static {
    const ID: &'static str;

    type Input: Send + 'static;

    fn descriptor() -> EmbedderDescriptor;

    async fn embed(
        &self,
        input: Self::Input,
        ctx: EmbedContext<'_>,
    ) -> AgenkitResult<EmbeddingBatch>;
}
```

These traits are author-facing typed APIs. The runtime can erase them
into object-safe registry entries such as `DynTool`, `DynRetriever`,
and `DynEmbedder` for storage, JSON schema validation, tracing, and
agent tool-call dispatch.

Example registration:

```rust
pub fn agenkit() -> Agenkit {
    Agenkit::builder()
        .tool(SearchDocs)
        .retriever(ProjectDocs)
        .embedder(LocalEmbedder)
        .flow(answer_question)
        .build()
}
```

Example deterministic retrieval inside a flow:

```rust
let docs = ctx
    .retrieve::<ProjectDocs>()
    .query(ProjectDocsQuery::new(&input.question))
    .top_k(5)
    .run()
    .await?;
```

Example agent-controlled retrieval:

```rust
impl AiAgent for DocsFirst {
    const ID: &'static str = "docs_first";

    type Input = QuestionInput;
    type Output = CandidateAnswer;

    fn configure(agent: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        agent
            .system("Search project docs before answering.")
            .tools([ProjectDocs::as_tool()])
    }
}
```

Rules:

* `pocopine-agenkit-core` defines shared descriptors and payloads:
  `ToolDescriptor`, `ToolCall`, `ToolResult`,
  `ToolSideEffectPolicy`, `RetrieverDescriptor`, `RetrievalQuery`,
  `RetrievalHit`, `RetrievalSet`, `SourceRef`, `Citation`,
  `EmbedderDescriptor`, `Embedding`, and `EmbeddingBatch`.
* `pocopine-agenkit` owns typed traits and runtime registries:
  `AiTool`, `AiRetriever`, `AiEmbedder`, `ToolRegistry`,
  `RetrieverRegistry`, and `EmbedderRegistry`.
* Tool input is validated before execution and tool output is validated
  before it is sent back to a model or client boundary.
* Tools run under the current flow principal, explicit tool allowlist,
  budget, timeout, and side-effect policy.
* Tool descriptors declare side effects and framework-mediated state or
  resource dependencies. Tool implementations should use
  `AiToolContext` for app resources so those reads/writes are visible
  to traces.
* Side-effecting tools must be marked explicitly. Read-only is the safe
  default.
* Retrieval is read-only by default and must respect auth, tenancy, and
  per-flow source allowlists.
* Retriever descriptors declare source kinds, auth requirements, and
  framework-mediated resource dependencies.
* Retrieval hits carry citations/source refs and privacy labels.
  Traces should record source ids, scores, and redacted metadata, not
  raw document bodies by default.
* Retrievers can be wrapped as tools through an adapter, but flows can
  call retrievers directly when retrieval should be deterministic.
* Embedders expose model/provider aliases and dimensions, not provider
  credentials.

### D6 - Flow Context Best Practices And Diagnostics

Flows should be written as if they were pure at the app boundary and
declared-effectful inside the runtime. This is a best-practice and
diagnostics model, not a hard safety guarantee. Rust code can still
call a global database pool, read a file, or use a network client
directly. What Agenkit can do is make the safe path obvious, traceable,
and documented, then warn when framework-mediated context access was
not declared.

The rule:

```text
flow output = input + declared context + declared AI effects
```

Declared context includes registered tools, retrievers, embedders,
agent threads, app state handles, environment aliases, and model
aliases. Anything outside that set is an undeclared dependency and
should not be invisible to traces when it goes through Agenkit APIs.

Flow descriptors can carry a context manifest:

```rust
Flow::new("answer_question", answer_question)
    .input::<QuestionInput>()
    .output::<Answer>()
    .uses_retriever::<ProjectDocs>()
    .uses_agent::<DocsFirst>()
    .uses_tool::<SearchDocs>()
```

This manifest is documentation and trace metadata, not an enforcement
policy. Agenkit can compare framework-mediated context access against
the manifest and emit advisory diagnostics when something is missing,
but it should not expose a `context_policy(...)` API or pretend to
make arbitrary Rust code pure.

Correct deterministic retrieval:

```rust
let docs = ctx
    .retrieve::<ProjectDocs>()
    .query(ProjectDocsQuery::new(&input.question))
    .top_k(5)
    .run()
    .await?;
```

Avoid hidden context loads:

```rust
// Bad for traceability: hidden dependency on DocIndex that does not
// appear in the flow descriptor unless it was explicitly declared.
let docs = ctx.state::<DocIndex>()?.search(&input.question).await?;
```

If a flow really needs app state, it should be declared:

```rust
Flow::new("debug_failure", debug_failure)
    .input::<DebugInput>()
    .output::<DebugReport>()
    .uses_state::<TraceStore>("trace_store")
    .uses_retriever::<BuildLogs>()
    .uses_agent::<DebuggerAgent>()
```

Rules:

* `pocopine-agenkit-core` defines `ContextManifest`,
  `DeclaredResource`, `ResourceKind`, and `ContextGapDiagnostic`.
* `pocopine-agenkit` records framework-mediated context access: state,
  retrievers, tools, agents, threads, embedders, providers, and model
  aliases.
* Runtime diagnostics are advisory. They should be emitted as typed
  diagnostics and `pocopine.trace` events, not only logs.
* Generated clients and public flow schemas should include the public
  input/output schema, but not secret-bearing resource config.
* Flow traces should record the context manifest hash or version, so a
  replay can detect that the flow's resource set changed.
* Direct app code can still bypass the framework by using globals or
  direct DB clients. The docs, examples, and skills should state this
  plainly: if a flow loads hidden context outside Agenkit APIs, traces
  and replay become less reliable.
* Future proc macros and lints can improve detection, but the first
  implementation should not overpromise enforcement.
* Pocopine Debugger should flag flows with missing manifests or context
  gap diagnostics as lower-confidence because replay and root-cause
  analysis are weaker.

### D7 - Multi-Step, AiAgent, And Parallel Flow Orchestration

Multi-step generation is traced flow orchestration, not a separate
agent harness. A public flow still has one typed input and one typed
output, but internally it can run retrieval, model calls, tool calls,
validation, ranking, and reducer steps under one trace tree.

Model calls and tool calls are traced automatically. Custom app work is
included in the same trace by wrapping it in an explicit step:

```rust
async fn answer_question(input: QuestionInput, ctx: AiFlowContext) -> AgenkitResult<Answer> {
    let docs = ctx
        .retrieve::<ProjectDocs>()
        .query(ProjectDocsQuery::new(&input.question))
        .top_k(5)
        .run()
        .await?;

    let draft: DraftAnswer = ctx
        .ai()
        .system("Answer using the retrieved project docs.")
        .docs(docs)
        .prompt(&input.question)
        .schema::<DraftAnswer>()
        .generate_structured()
        .await?;

    let answer: Answer = ctx
        .ai()
        .system("Verify the draft and return the final answer.")
        .input(draft)
        .schema::<Answer>()
        .generate_structured()
        .await?;

    Ok(answer)
}
```

Parallel agents use the same flow runtime. An `AiAgent` in v1 is not a
separate process or external harness. It is a typed runnable AI unit
containing a stable id, model alias, system prompt, optional tool
allowlist, input/output schemas, limits, redaction/capture policy, and
future memory/session policy.

This matches the common shape in other frameworks while staying
Pocopine-native:

* LangChain describes an agent as a model calling tools in a loop, with
  the harness providing model, prompt, tools, and middleware.
* The OpenAI Agents SDK describes an agent as a model configured with
  instructions, tools, handoffs, guardrails, structured outputs, and
  runtime behavior.
* LlamaIndex describes an agent as a system using an LLM, memory, and
  tools to handle user input.
* Semantic Kernel separates `Agent`, `AgentThread`, and orchestration
  patterns such as concurrent, sequential, handoff, and group chat.

The Rust-first API should be type-driven:

```rust
struct FastResearcher;

impl AiAgent for FastResearcher {
    const ID: &'static str = "fast_researcher";

    type Input = QuestionInput;
    type Output = CandidateAnswer;

    fn configure(agent: AiAgentBuilder<Self>) -> AiAgentBuilder<Self> {
        agent
            .model(ModelRef::new("local/fast"))
            .system("Find likely answers quickly, then cite evidence.")
            .tools([search_docs])
            .max_tokens(700)
    }
}
```

Prefer `ctx.agent::<FastResearcher>()` over string-based lookup. The
typed form lets the compiler carry the agent id, input schema, output
schema, and policy. A dynamic lookup such as
`ctx.agent_by_id("fast_researcher")` can exist later for CLI, config,
debugger UI, or eval runners, but it should not be the main app
authoring API.

Agent threads are explicit:

```rust
let answer = ctx
    .agent::<DebuggerAgent>()
    .thread(existing_thread)
    .input(next_question)
    .run()
    .await?;
```

A threaded run appends safe messages/checkpoints to the thread. A
threadless run uses only the supplied input, flow context, tools, and
provider state. This keeps one-shot server flows cheap and predictable
while giving debugger/chat products durable context.

Fan-out/fan-in flows run several agents concurrently and then combine
their outputs through a reducer:

```rust
async fn answer_with_review(
    input: QuestionInput,
    ctx: AiFlowContext,
) -> AgenkitResult<FinalAnswer> {
    let candidates = ctx
        .parallel("candidate_answers")
        .branch(ctx.agent::<FastResearcher>().input(input.clone()))
        .branch(ctx.agent::<StrictReviewer>().input(input.clone()))
        .branch(ctx.agent::<DocsFirst>().input(input.clone()))
        .run()
        .await?;

    let checked: FinalAnswer = ctx
        .reduce("judge_and_merge", candidates)
        .system("Keep only claims supported by evidence.")
        .schema::<FinalAnswer>()
        .await?;

    Ok(checked)
}
```

Trace shape:

```text
flow: answer_with_review
  parallel group: candidate_answers
    step: fast_researcher
    step: strict_reviewer
    step: docs_first
  step: judge_and_merge
  step: validate_final_answer
```

A debugger-style flow can combine all three layers:

```rust
async fn debug_failure(input: DebugInput, ctx: AiFlowContext) -> AgenkitResult<DebugReport> {
    let thread = ctx
        .thread::<DebuggerAgent>()
        .resume_or_create(input.session_id)
        .await?;

    let trace = ctx
        .retrieve::<TraceStore>()
        .query(TraceQuery::new(input.trace_id))
        .run()
        .await?;

    let findings = ctx
        .parallel("diagnosis_candidates")
        .join(ParallelJoin::AllSettled)
        .min_success(2)
        .branch(ctx.agent::<TraceReviewer>().input(trace.clone()))
        .branch(ctx.agent::<LogReviewer>().input(input.logs.clone()))
        .branch(ctx.agent::<CodeReviewer>().input(input.files.clone()))
        .run()
        .await?;

    ctx.agent::<DebuggerAgent>()
        .thread(thread)
        .input(findings)
        .run()
        .await
}
```

Rules:

* `pocopine-agenkit-core` defines shared orchestration types:
  `StepId`, `StepKind`, `StepStatus`, `ParallelGroupId`,
  `AiAgentDescriptor`, `AgentThreadId`, `AgentThreadDescriptor`,
  `ThreadMessage`, `ThreadCheckpoint`, `ReducerKind`, and the trace
  payloads that describe parent/child step relationships.
* `pocopine-agenkit` owns the `AiAgent` trait, `AiAgentBuilder`,
  `AgentThreadStore`, and execution APIs such as `ctx.step(...)`,
  `ctx.parallel(...)`, `ctx.agent::<T>()`, `ctx.thread::<T>()`, and
  `ctx.reduce(...)`.
* Typed agent calls erase into branch plans for parallel execution, so
  different agent types can run in the same group while preserving each
  agent's descriptor, input schema, output schema, and policy.
* Thread state is opt-in. Flows can attach an existing thread, create a
  new thread, or run agents without any thread.
* Thread storage must be behind a runtime trait so v1 can start with an
  in-memory or local store while future debugger products can use
  durable storage.
* Parallel groups must be bounded by explicit max concurrency, timeout,
  token, and cost budgets. A cancelled flow cancels unfinished child
  steps.
* Branch failures are policy-driven: fail fast, collect partial results,
  or require a minimum number of successful candidates.
* Reducers can be model judges, deterministic Rust functions, scoring
  rubrics, voting strategies, schema validators, or product-specific
  checks.
* Reducers must produce typed outputs and record why an answer was
  accepted, rejected, or merged. For Pocopine Debugger, a reducer should
  prefer concrete evidence such as traces, logs, tests, file paths, and
  generated client contracts over unsupported model claims.
* Each branch still uses model aliases, provider allowlists, tool
  allowlists, auth policy, and redaction settings from the parent flow.
  Parallel execution must not create a bypass around the server-only
  secret boundary.

The first implementation should stay lightweight. Durable DAGs,
long-running resumability, distributed queues, and retry schedulers can
layer on `pocopine-jobs` later if a real product needs them.

### D8 - User-Facing Streams And Parallel Join Policies

Streaming to users is a separate public contract from internal traces.
The runtime may record detailed trace metadata for debugging and evals,
but a browser client should only receive typed, redacted, allowlisted
events.

Do not stream raw model thoughts, hidden chain-of-thought, provider
reasoning tokens, raw prompts, raw tool arguments, or raw provider
payloads to clients. If a product wants to show that the system is
"thinking," it should stream explicit progress events and optional
model-generated reasoning summaries that were requested for display,
redacted, and marked as user-visible output.

User-facing stream events should be shaped as `FlowStreamEvent` values:

| Event | Purpose |
|-------|---------|
| `flow_started` / `flow_completed` / `flow_failed` | Public flow lifecycle. |
| `step_started` / `step_progress` / `step_completed` / `step_failed` | Redacted step status for retrieval, generation, validation, reducers, and custom app work. |
| `output_delta` / `output_completed` | User-visible answer text or structured output fragments. |
| `tool_started` / `tool_completed` / `tool_failed` | Redacted tool status; no raw args by default. |
| `parallel_started` / `branch_started` / `branch_completed` / `branch_failed` / `parallel_completed` | Public progress for fan-out/fan-in flows. |
| `reducer_started` / `reducer_decision` / `reducer_completed` | Redacted checker/merge progress and final decision metadata. |
| `usage_update` | Optional aggregate usage/cost metadata when enabled. |
| `error` | Public error kind and trace id, not provider internals. |

Public streaming modes should be explicit:

| Mode | Client sees |
|------|-------------|
| `FinalOnly` | No progress stream; only the final typed result. |
| `OutputDeltas` | User-visible output deltas plus final typed result. |
| `Progress` | Redacted step/tool/parallel/reducer status plus final typed result. |
| `DebugSafe` | Progress plus safe trace refs for local debugging; never raw secrets or hidden reasoning. |

Parallel groups need Promise-like join policies, but the default should
match the product goal:

| Policy | Behavior | Good for |
|--------|----------|----------|
| `All` | Require every branch to succeed; fail or cancel on policy. | Required independent checks. |
| `AllSettled` | Wait for every branch and return successes plus failures. | Agent review where the reducer should see disagreements and failures. |
| `FirstSuccess` | Return the first successful branch and cancel the rest. | Latency races across equivalent providers/models. |
| `Quorum(n)` | Return when at least `n` branches succeed, optionally cancelling the rest. | Voting or majority-style checks. |

For parallel agents that combine answers, prefer `AllSettled` with a
minimum-success requirement and a reducer. `FirstSuccess` is useful for
latency, but it is the wrong default for quality checking because it
throws away disagreement. `All` is useful when every branch is required,
but it is too brittle for exploratory multi-agent review unless every
agent is mandatory.

Example:

```rust
let candidates = ctx
    .parallel("candidate_answers")
    .join(ParallelJoin::AllSettled)
    .min_success(2)
    .max_concurrency(3)
    .branch(ctx.agent::<FastResearcher>().input(input.clone()))
    .branch(ctx.agent::<StrictReviewer>().input(input.clone()))
    .branch(ctx.agent::<DocsFirst>().input(input.clone()))
    .run()
    .await?;
```

### D9 - Server-Function Integration

AI calls should feel like Firebase/Genkit-style callable app functions,
but through Pocopine's existing server-function and auth contracts.

Rules:

* Public flows expose typed input/output through generated client
  helpers.
* Auth policy is declared at the flow/server-function boundary, not in
  arbitrary prompt code.
* Provider keys never enter browser bundles.
* Flow invocations produce trace ids that can be correlated with
  `pocopine-observe` events.
* Streaming is an explicit mode, not a hidden return-type special case.
* Public streams use `FlowStreamEvent` and stream Pocopine events, not
  raw provider streams.
* Public flow clients see a single typed flow contract even when the
  server runs multiple internal steps or parallel branches.

### D10 - Secret Boundary And Client Security

Provider credentials are server-only. A Pocopine browser client must
never receive an AI provider API key, provider bearer token, OAuth
refresh token, service-account document, signing secret, or provider
request header. The browser calls typed Pocopine flow endpoints; the
server validates auth, resolves configured model/provider aliases, and
performs provider calls from host-side credentials.

Rules:

* `pocopine-agenkit-core` may define `ProviderRef`, `ModelRef`, schema
  metadata, tool descriptors, and client-safe payload shapes, but it
  must not define a client-serializable secret-bearing provider config.
* `pocopine-agenkit` loads provider credentials only from host-side
  sources: environment variables, deploy/runtime secret stores, dev
  `.env` loaded by the server process, or explicit host runtime config.
* Generated client helpers expose public flow names, typed input/output
  schemas, trace ids, and safe model aliases only. They must not expose
  provider base headers, raw provider config, or credential presence.
* Client-supplied model/provider choices are aliases, not raw provider
  ids. The server allowlists aliases per app or per flow before use.
* Public flows run under Pocopine auth policy. Tools execute under the
  current app principal and only from the flow's explicit tool allowlist.
* Retrieval executes under the current app principal and only from the
  flow's explicit source allowlist.
* Parallel branches inherit the parent flow's auth, model alias
  allowlist, tool allowlist, budget, and redaction policy.
* Model output is untrusted. Structured output is schema-validated, tool
  calls are validated before execution, and provider/tool errors are
  mapped to Pocopine errors before they cross the client boundary.
* Streaming flows stream Pocopine events, not raw provider streams.
  Provider chunks are translated server-side so provider headers,
  request ids, and vendor-specific debug payloads cannot leak.
* Hidden reasoning, provider reasoning tokens, and raw chain-of-thought
  are never part of client stream events. User-visible reasoning
  summaries must be explicit outputs with their own capture/redaction
  policy.
* Agent thread state follows the same privacy model as traces. Thread
  messages, attachments, checkpoints, and summaries must be
  privacy-labeled; raw prompts, tool args, provider payloads, and
  secrets are not stored unless explicitly enabled by capture policy.
* Thread ids exposed to clients are opaque Pocopine ids, not provider
  thread ids. Provider-backed thread ids, if any, stay in host runtime
  state.
* Traces and logs redact secrets by default: API keys, Authorization
  headers, cookies, service-account material, `.env` values, provider
  request headers, and configured redaction fields. Prompt and output
  capture must be configurable separately from operational metadata.

This makes the security boundary visible in the type system: client
types can describe *what* flow to call and *what* data shape to send,
but only host runtime types can describe *how* to authenticate with an
AI provider.

### D11 - Product Focus And Deferred Agent Surfaces

This RFC does not define a general coding-agent harness. It defines the
AI app runtime that future focused products can use.

The strongest first product candidate is **Pocopine Debugger**: an
agent-powered debugging assistant that consumes Pocopine traces,
server-function metadata, generated clients, build/test output,
observability events, and project RFCs. Its job would be to diagnose
Pocopine-specific failures, not to compete with every generic coding
agent shell.

That product can be designed after `pocopine-agenkit-core` and
`pocopine-agenkit` prove:

* flows can be registered and invoked safely;
* traces are privacy-labeled and redacted;
* provider credentials remain server-only;
* tools run under explicit app policy;
* run records are structured enough for replay and diagnosis.

Future products may include a debugger UI, VS Code integration, skill
loading, eval runners, or a stdio process. None of those should add new
AI contracts until the core runtime cannot express the need.

### D12 - Unified Pocopine Observability Platform

Agenkit uses Pocopine observability as the platform layer. It must not
grow a parallel Genkit-style telemetry runtime, separate trace exporter,
or AI-only dashboard contract.

The split stays the same as the rest of Pocopine:

| Crate | Agenkit responsibility |
|-------|------------------------|
| `pocopine-observe` | Stable AI event schema, context, privacy labels, redaction metadata, trace ids, session ids, and fixed tracing targets. |
| `pocopine-logging` | Browser/server subscribers, local dev logs, JSON logs, and OTLP export through existing logging setup. |
| `pocopine-analytics` | Redacted telemetry fan-out for production dashboards, usage/cost summaries, eval summaries, and product analytics. |

Runtime code emits `tracing` spans/events or typed `ObservedEvent`s.
It does not install global subscribers, own vendor exporters, or bypass
the existing analytics sinks. App entrypoints and deployment adapters
keep deciding where events go.

AI-specific event families should use the existing targets and privacy
model:

| Event family | Target/class | Notes |
|--------------|--------------|-------|
| Flow lifecycle | `pocopine.trace` | `ai_flow_started`, `ai_flow_completed`, `ai_flow_failed`; includes flow id, trace id, model alias, duration, and privacy-labeled metadata. |
| Flow context diagnostics | `pocopine.trace` | `ai_context_gap`, `ai_context_manifest_changed`; records flow id, resource kind, resource id, and manifest version/hash. |
| Step orchestration | `pocopine.trace` | `ai_step_started`, `ai_step_completed`, `ai_step_failed`; records step id, parent step id, step kind, duration, and redacted input/output refs. |
| Parallel and reducer steps | `pocopine.trace` | `ai_parallel_started`, `ai_parallel_completed`, `ai_reducer_started`, `ai_reducer_completed`; records branch count, join policy, success policy, reducer kind, and redacted decision metadata. |
| Model calls | `pocopine.trace` | `ai_model_request`, `ai_model_response`, `ai_model_failed`; records provider alias, model alias, token/cost metadata when available, and redacted error kind. |
| Tool calls | `pocopine.trace` | `ai_tool_started`, `ai_tool_completed`, `ai_tool_failed`; records tool id, duration, auth principal context, and validated argument schema id, not raw args by default. |
| Retrieval calls | `pocopine.trace` | `ai_retrieval_started`, `ai_retrieval_completed`, `ai_retrieval_failed`; records retriever id, source ids, hit count, scores when available, and redacted metadata. |
| Embedding calls | `pocopine.trace` | `ai_embedding_started`, `ai_embedding_completed`, `ai_embedding_failed`; records embedder alias, input count, dimensions, duration, and redacted error kind. |
| Agent thread lifecycle | `pocopine.trace` | `ai_thread_created`, `ai_thread_resumed`, `ai_thread_checkpointed`, `ai_thread_deleted`; records opaque thread id, agent id, retention class, and redacted checkpoint metadata. |
| Public stream events | client stream | Redacted `FlowStreamEvent` values derived from the runtime, not raw trace events or raw provider streams. |
| Evals | `pocopine.analytics` | Dataset/case summaries, pass/fail scores, cost/token aggregates, and stable eval ids. |
| Operational failures | `pocopine.log` | Configuration, provider, streaming, validation, and secret-boundary failures with redacted fields. |
| Usage/cost summaries | `pocopine.metric` or `pocopine.analytics` | Aggregated token counts, latency, model mix, and queue depth. |

GenAI semantic conventions can inform field names where they map cleanly,
but Pocopine's `ObservedEvent`, privacy labels, redaction rules, and
targets are authoritative. Prompt and output capture must be explicitly
configured and privacy-labeled; operational traces should remain useful
without storing raw prompts, raw outputs, tool arguments, provider
headers, request bodies, or secrets.

The trace model must capture:

* model/provider aliases, not raw provider credentials;
* context manifest version/hash when available;
* token/cost metadata when available;
* tool calls and validated tool results;
* retrieval source refs, hit counts, score metadata, and citations;
* embedding dimensions and batch metadata;
* flow step timing;
* parallel group timing, join policy, branch status, and reducer
  decisions;
* optional agent thread ids and checkpoint refs;
* structured-output validation errors;
* auth/user context at the boundary, without logging secrets;
* replay handles for local evals and future debugger inspection.

Evals are deferred past the first core slice, but the run record should
not block them. A future dataset runner should be able to run a flow
over a dataset and emit per-case results later without changing the
flow API or adding an eval-only telemetry path.

### D13 - Recommended Improvements Before Implementation

These improvements should be designed before writing the first runtime
APIs. They keep the implementation small while avoiding obvious
dead-ends.

* Budget model: define per-flow and per-step limits for timeout,
  concurrency, tokens, retries, and estimated cost. Parallel agents
  become expensive quickly without this.
* Cancellation model: define how cancellation propagates through nested
  steps, provider streams, tool calls, and parallel branches.
* Stream visibility model: define which event classes are client-safe,
  which are trace-only, and which require explicit prompt/output capture
  consent.
* Parallel join model: define `All`, `AllSettled`, `FirstSuccess`, and
  `Quorum(n)` semantics, including cancellation and failure handling.
* Retrieval model: define score semantics, source refs, citations,
  source allowlists, auth filtering, and deterministic replay of
  retrieval outputs.
* Flow context guidance: document the best-practice rule that AI flows
  should load context through declared Agenkit APIs, and clearly explain
  the debugging/replay cost of hidden DB/file/network reads.
* Declared-context model: define how flow descriptors declare tools,
  retrievers, embedders, agents, threads, state handles, provider
  aliases, and model aliases.
* Static lint model: future proc macros or lints should flag direct
  global/database access inside AI flow bodies when possible.
* Evidence model: structured outputs should be able to carry citations,
  source refs, trace refs, test refs, or file refs so reducers can check
  claims instead of merely ranking prose.
* Replay model: traces should store enough safe metadata and output refs
  to reproduce a flow with a mock provider, without storing provider
  credentials or raw sensitive prompts by default.
* Thread storage model: define in-memory/local/durable thread stores,
  retention policy, deletion behavior, and how provider-backed thread
  ids are hidden behind opaque Pocopine ids.
* Capture policy: prompt/output capture should be configured separately
  from operational metadata at flow, step, and branch level.
* Deterministic verification hooks: reducers should be able to call Rust
  validators, schema checks, tests, or Pocopine debugger checks before
  accepting a final answer.
* Local provider first: use a deterministic local/mock provider in tests
  and examples before committing to hosted provider adapters.
* Typed errors: distinguish provider failure, schema failure, tool
  policy failure, budget exhaustion, cancellation, and reducer
  disagreement in `AgenkitError`.

### D14 - Why One Facade Instead Of Many App APIs

The implementation has two crates, but the app authoring API must stay
unified. The facade exists for five reasons:

* Provider churn should not leak into app code. Apps choose
  `ModelRef`s and provider config, not provider crate internals.
* Debugger products, evals, server functions, and jobs need the same
  flow descriptors. Separate APIs would split tracing and replay.
* Observability needs one platform contract. Separate AI telemetry would
  duplicate `pocopine-observe`, weaken redaction guarantees, and make
  future debugger traces drift from production traces.
* Future proc macros need stable core descriptors to generate
  predictable runtime registration.
* Future UI or editor work should layer on the open runtime contracts,
  not fork the core app API.

### D15 - Resolved Implementation Decisions

These decisions were frozen in Phase 0 after grounding the design in the
current framework (cited crates are the integration surfaces Agenkit reuses).

* **DC-1 - Core stays observe-neutral.** `pocopine-agenkit-core` defines
  neutral `TraceEvent`/`TraceSpan` payloads plus stable `&'static str`
  event-name constants and must **not** depend on `pocopine-observe`. The
  facade owns the mapping `TraceEvent -> pocopine_observe::ObservedEvent`
  followed by `emit_tracing`. Rationale: `pocopine-observe` is consumed only
  by facade-tier crates (`pocopine-logging`, `pocopine-analytics`) today;
  keeping core free of it preserves "dependency-light + wasm-friendly" and
  keeps generated clients from transitively pulling `tracing`.
* **DC-1a - Trace trees are fields, not spans.** `pocopine-observe` has no
  span parent/child hierarchy: parent/child is conveyed only by a shared
  `trace_id` in `ObserveContext` plus explicit fields. `TraceEvent`/
  `TraceSpan` therefore carry explicit `step_id` / `parent_step_id` /
  `parallel_group_id`, which the facade copies into `ObservedEvent` fields.
* **DC-2 - `AgenkitError`/`AgenkitResult` live in core**, modelled as a
  manual enum + `Display` + `impl std::error::Error` + helper constructors +
  `impl From` (no `thiserror`, matching `JobError`/`SyncError`/`StorageError`).
  Host-only variants are gated `#[cfg(not(target_arch = "wasm32"))]`. Variants
  distinguish provider, schema/validation, tool-policy, budget-exhaustion,
  cancellation, and reducer-disagreement failures (§D13).
* **DC-3 - `prelude` and root re-exports are facade-only and deferred.** No
  `prelude` exists elsewhere in the workspace; it is a sanctioned new
  convention. Per the workspace's "defer umbrella re-exports until the API is
  stable" rule, the facade does not re-export core while the surface churns;
  the curated `prelude` lands as the final checkpoint of each phase.
* **DC-4 - Async contract traits use native `async fn in trait`** with
  `#[allow(async_fn_in_trait)]` plus a boxed-future typedef
  (`Pin<Box<dyn Future<Output = AgenkitResult<...>> + Send + 'a>>`) for
  object-safe `DynTool`/`DynRetriever`/`DynEmbedder`/`DynProvider` erasure,
  mirroring `pocopine_sync_query::source::Source`. The `async-trait` macro is
  not used for public contract traits. Registries are
  `HashMap<String, Arc<dyn DynX>>`, mirroring `SyncServerBuilder` /
  `StorageServerBuilder` / `Server::new`.
* **DC-5 - Public flows reuse the `#[server]` path; principal access is an
  in-facade adapter.** Exposing a public flow as a `#[server]` function gives
  the typed wasm client for free (the macro emits
  `pocopine::fetch::call::<Args, R>`); no separate Rust client codegen is
  added. Because `#[server]` hands `RequestContext`/`Principal` only to guards,
  not handler bodies, tools/retrieval obtain the caller principal through a
  thin adapter in `pocopine-agenkit` that reads `Principal` from request
  extensions and threads it through `AiFlowContext`. The shared
  `pocopine-macros` `#[server]` macro is **not** modified (macro work is
  deferred, §Deferred Follow-Ups).
* **DC-6 - Streaming ships in v1.** `StreamMode::{FinalOnly, OutputDeltas,
  Progress, DebugSafe}` are all honored. There is no server-function
  streaming today (only `pocopine-live`'s SSE), so the streaming route is
  net-new: it reuses `pocopine-live`'s SSE primitives (replay cursor via
  `last_event_id`, keep-alive, structured event enum) with AI-flow semantics,
  and applies a single per-event redaction chokepoint as the last transform
  before the wire so raw thoughts/reasoning tokens/prompts/tool args/provider
  payloads can never leak (§D8, §D10).
* **DC-7 - In-flight parallel cancellation is net-new.** `pocopine-jobs`
  cancellation is drop/nack/requeue and does not abort running work; parallel
  `ParallelJoin::{FirstSuccess, Quorum}` require real sibling cancellation via
  `tokio::task::JoinSet` + `abort()`. Only the budget *shapes*
  (`RetryPolicy`, `Duration` timeouts) are borrowed from `pocopine-jobs`.

## Non-Goals

* No browser-side provider calls in v1. Secrets stay on the server.
* No client-side provider credential storage. LocalStorage,
  sessionStorage, IndexedDB, browser cookies, generated JS bundles, and
  wasm custom sections must never contain provider API keys or provider
  bearer tokens.
* No provider zoo in the first slice. Prove the Pocopine API first.
* No `pocopine-agenkit-code`, JSONL stdio protocol, VS Code panel, or
  generic coding-agent harness in this RFC.
* No proc-macro crate in the first slice. Manual registration must work
  before authoring sugar.
* No mandatory thread state for every agent run. Stateless agent calls
  must remain the default for one-shot flows.
* No production durable thread database in the first slice. Define the
  trait and prove it with in-memory/local stores first.
* No claim that Agenkit can prevent arbitrary Rust code from using
  globals, direct database clients, filesystem APIs, or network clients.
  The first slice records framework-mediated context access and emits
  advisory diagnostics only; later macros/lints can improve detection.
* No commercial-only runtime contract. Future debugger or UI products
  must layer on the same open core/facade contracts.
* No direct copy of Genkit, Pi, or Rig APIs. They are references; the
  Pocopine surface must fit Pocopine's server functions, auth,
  observability, and deployment model.
* No hand-rolled encoding/crypto helpers. If trace ids, signatures,
  base64 payloads, or percent-encoded fields are needed, use
  `pocopine-crypto` and `pocopine-codec`.

## Implementation Plan

### Phase 0 - Scaffolding And API Freeze

**Checkpoint 0.1** - land the two empty crates wired into the workspace plus
the frozen decisions, with no runtime logic:

* create `pocopine-agenkit-core` (dependency-light, wasm-friendly) and
  `pocopine-agenkit` (host-side facade depending on core), inheriting the
  `[workspace.package]` fields and registered in the root `[workspace]
  members` + `[workspace.dependencies]`;
* model the facade's host-only dependency target-split on
  `crates/pocopine-sync-query/Cargo.toml`: core is serde-only; the facade
  gates `tokio`, `pocopine-observe`, `pocopine-auth`, `pocopine-server` under
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` as the
  checkpoints that need them arrive, so the workspace wasm build keeps
  compiling the facade as an (empty) library;
* freeze the §D15 decisions (DC-1..DC-7), the resolved name (§D1), the
  secret-boundary test plan, the context-manifest/diagnostics model, the
  typed tool/retriever/embedder trait shapes, the multi-step/parallel trace
  schema, the public stream-event schema and visibility rules (§D8), and the
  optional `AgentThread` storage/retention contract;
* provider strategy: a deterministic local/mock provider first; Rig stays a
  reference to evaluate behind Pocopine traits in a deferred adapter crate.

Exit gate: `cargo build` and `cargo build --target wasm32-unknown-unknown`
both pass on the two crates, `cargo fmt --all --check` is clean, and an app
author can read this RFC and know which crate to use, which API to call, and
which future products are intentionally out of scope.

### Phase 1 - `pocopine-agenkit-core`

Add only shared contracts, built bottom-up along the type dependency graph.
Each checkpoint is a reviewable commit; serde round-trip tests and the
"no credential-shaped field" corpus accumulate per checkpoint.

* **Checkpoint 1.1 - identity, leaf enums, error** (roots the whole graph;
  blocks everything else): `ModelRef`, `ProviderRef`, `RunId`, `TraceId`,
  `SessionId`, `AgentThreadId`, `ParallelGroupId`, `StepId`; payload-free
  enums `Role`, `StepKind`, `StepStatus`, `ToolSideEffectPolicy`,
  `ResourceKind`, `ReducerKind`, `StreamMode`, `ParallelJoin`,
  `BranchFailurePolicy`, `ThreadRetention`; `AgenkitError`/`AgenkitResult`
  (§D15 DC-2). Core only *carries* ids; the facade mints them (via
  `pocopine-crypto`/`pocopine-codec`) to keep core dependency-light.
* **Checkpoint 1.2 - content/messaging** (depends on `Role`):
  `Content`, `ContentPart`, `Message`, multimodal attachments.
* **Checkpoint 1.3 - capability descriptors/payloads** (three independent
  families, can be authored in parallel): `ToolDescriptor`, `ToolCall`,
  `ToolResult`; `RetrieverDescriptor`, `RetrievalQuery`, `RetrievalHit`,
  `RetrievalSet`, `SourceRef`, `Citation`; `EmbedderDescriptor`, `Embedding`,
  `EmbeddingBatch`. First home for the "no credential field" invariant tests.
* **Checkpoint 1.4 - flow, manifest, orchestration, trace/stream** (most
  schema-sensitive, lands last): `FlowDescriptor`, `FlowInputSchema`,
  `FlowOutputSchema`; `ContextManifest`, `DeclaredResource`,
  `ContextGapDiagnostic`; `AiAgentDescriptor`, `AgentThreadDescriptor`,
  `ThreadMessage`, `ThreadCheckpoint`, `ReducerDecision`; `FlowStreamEvent`,
  `TraceEvent`, `TraceSpan`, `Usage`, `CostEstimate`; stable event-name
  constants for the AI event families. `TraceEvent`/`TraceSpan` carry explicit
  `step_id`/`parent_step_id`/`parallel_group_id` (§D15 DC-1a).

Rules:

* no provider SDKs;
* no secret-bearing provider config structs that serialize to client
  payloads;
* no proc macros;
* no stdio implementation or agent harness;
* no server runtime dependency; no `pocopine-observe` dependency (§D15 DC-1);
* serde round-trip tests for every public payload;
* tests proving public payloads do not contain credential fields;
* stable event names and trace payload structs for the first AI event
  families;
* wasm-friendly by default where practical.

Exit gate: `pocopine-agenkit-core` can serialize and deserialize the
same client-safe payloads used by runtime tests and future generated
clients; a round-trip test for a nested-parallel `TraceEvent` proves
`parent_step_id`/`parallel_group_id` survive and no secret fields exist.

### Phase 2 - `pocopine-agenkit`

Add the runtime facade. The deterministic mock provider is the critical path
and lands first; nothing downstream is testable without it. The trace-mapping
layer lands early (2.3) so every later checkpoint traces through one tested
mapper.

* **Checkpoint 2.1 - provider trait + mock provider + registry** (first):
  `Provider` trait + `DynProvider` shim (§D15 DC-4), `ProviderRegistry`, and a
  deterministic `MockProvider` (canned text + structured JSON keyed by input,
  for reproducible tests and replay).
* **Checkpoint 2.2 - `Agenkit::builder()` + `Ai` generation**:
  fluent builder + `.build()`; `Ai` with `generate_text` and
  `generate_structured` (structured runs the schema-validation hook — model
  output is untrusted, §D10).
* **Checkpoint 2.3 - trace-mapping layer + observe integration**: the facade's
  `TraceEvent -> ObservedEvent` + `emit_tracing` mapping (§D15 DC-1), one
  function per event family, copying `step_id`/`parent_step_id`/
  `parallel_group_id` into `ObservedEvent` fields with the right
  `FieldPrivacy`; default `RedactionPolicy::public_only`, raw
  prompts/args/outputs `Sensitive` and stripped unless capture policy opts in.
* **Checkpoint 2.4 - tool/retriever/embedder** typed traits (§D15 DC-4),
  `DynTool`/`DynRetriever`/`DynEmbedder` erasure, `ToolRegistry`/
  `RetrieverRegistry`/`EmbedderRegistry`, `AiToolContext`/`RetrievalContext`/
  `EmbedContext`, the retriever-as-tool adapter, and input/output validation.
* **Checkpoint 2.5 - flows + context**: manual `Flow` registration,
  `AiFlowContext`, `AiFlowContext::step(...)` for traced custom work,
  `AiFlowContext::retrieve::<T>()` for deterministic retrieval, context
  manifests and advisory undeclared-context diagnostics.
* **Checkpoint 2.6 - agents, threads, parallel, reduce, stream** (split into
  2.6a/2.6b): `AiAgent` + `AiAgentBuilder`, `AiFlowContext::agent::<T>()`,
  `AgentThreadStore` trait + in-memory impl, `AiFlowContext::thread::<T>()`,
  `AiFlowContext::reduce(...)` (2.6a); then `AiFlowContext::parallel(...)`,
  `ParallelJoin` execution for `All`/`AllSettled`/`FirstSuccess`/`Quorum(n)`
  with real in-flight sibling cancellation (§D15 DC-7), budget + cancellation
  propagation for nested steps, and `FlowStreamEvent` emission into an
  internal channel (2.6b). Optional dynamic `agent_by_id(...)` after typed
  agents work. The curated `prelude` re-exports land as the final commit
  (§D15 DC-3).

Initial public modules:

```rust
pub mod prelude;
pub mod generate;
pub mod flow;
pub mod tool;
pub mod retrieval;
pub mod embed;
pub mod provider;
pub mod step;
pub mod agent;
pub mod thread;
pub mod reduce;
pub mod trace;
pub mod observe;
pub mod server;
```

Exit gate: a small example can register one provider, one tool, and one
flow manually, call the flow from Rust, run at least one traced custom
step, one deterministic retriever, one optional-thread agent run, and
one parallel fan-out/fan-in branch, and emit `pocopine.trace` events
through the existing observability path.

### Phase 3 - Server-Function, Auth, And Secret-Boundary Integration

Wire public flows into Pocopine's existing server stack.

* **Checkpoint 3.1 - public-flow-as-`#[server]` bridge** (FinalOnly path,
  first): expose a public flow through the existing `#[server]`
  request/response path; the macro's wasm stub
  (`pocopine::fetch::call::<Args, R>`) is the typed client (§D15 DC-5). Map
  `AgenkitError -> ServerError` at the boundary. Enforce the server-side model
  alias allowlist, the per-flow tool allowlist, and the per-flow retrieval
  source allowlist. The client sees one typed contract even when the server
  runs multiple internal steps. No new infra.
* **Checkpoint 3.2 - principal injection adapter** (in-facade, §D15 DC-5): a
  thin adapter in `pocopine-agenkit` reads `Principal` from request extensions
  and threads it through `AiFlowContext`, so tools/retrieval run under the
  caller principal. The shared `#[server]` macro is not modified.
* **Checkpoint 3.3 - streaming route** (§D15 DC-6): a dedicated AI-flow SSE
  route reusing `pocopine-live` primitives (replay cursor, keep-alive,
  structured event enum) with AI-flow semantics, consuming the 2.6b
  `FlowStreamEvent` channel. Honor `FinalOnly`/`OutputDeltas`/`Progress`/
  `DebugSafe`. A single per-event redaction chokepoint is the last transform
  before the wire; it strips raw thoughts, reasoning tokens, prompts, tool
  args, and provider payloads. Auth via `RequestContext` as the live route.
* **Checkpoint 3.4 - secret-boundary test suite** (the gate-defining
  checkpoint): tests that fail if a provider key/bearer/refresh/service-account
  /signing-secret/provider-header string appears in the wasm artifact or JS
  bundle; that no `Sensitive` field survives `public_only` redaction in any
  emitted trace payload; that the streaming chokepoint never emits raw
  prompts/args/thoughts/provider payloads (fuzzed with planted fake secrets);
  that a non-allowlisted model/provider alias is rejected before any provider
  call; and that provider/tool errors map to `ServerError` without leaking
  provider internals. Trace id propagation into `pocopine-observe`, and
  server-function telemetry uses the same route/body/header omission rules as
  existing Pocopine server-function observability.

Exit gate: the example app calls an AI flow from UI code like a normal
Pocopine server function (FinalOnly) and consumes a redacted progress stream,
can inspect its trace locally, and has tests that fail if provider credentials
appear in client artifacts, trace payloads, or stream events.

## Deferred Follow-Ups

These are deliberately outside the first implementation series:

* `pocopine-agenkit-macros` for `#[ai_flow]`, `#[ai_tool]`, schema
  derivation, and generated server-function bridges;
* provider adapter crates such as `pocopine-agenkit-rig` or
  `pocopine-agenkit-provider-*`;
* a focused `pocopine-debugger` product that consumes Agenkit flows and
  Pocopine observability events;
* best-practice docs and skills that teach traceable flow design,
  declared context, retriever usage, and when to avoid hidden DB/file
  reads inside flow bodies;
* editor integrations, skill loading, stdio protocols, and commercial
  UI surfaces;
* eval dataset runners and production dashboard extensions.

Each deferred item should prove that it can layer on
`pocopine-agenkit-core` and `pocopine-agenkit` without adding a second
AI runtime contract.

## Acceptance Criteria

This RFC can move to Accepted when it has:

* final name;
* final two-crate split;
* explicit note on whether Rig is only a reference, a future adapter, or
  a direct dependency hidden behind Pocopine traits;
* first unified `pocopine_agenkit::prelude` contents;
* first `Agenkit::builder()` and `Ai` generation API shape;
* explicit tests for the server-only provider secret boundary;
* `ContextManifest` and advisory context-gap diagnostic shape for
  public flows;
* best-practice documentation plan for traceable flow design, including
  guidance to put this in Pocopine skills;
* first `AiTool`, `AiRetriever`, and `AiEmbedder` trait shapes with
  typed inputs/outputs and runtime registry erasure;
* clear rule for when retrieval is called directly by a flow versus
  exposed as an agent tool;
* redaction policy for traces/logs/model/tool/flow events;
* first `Flow`, `AiAgent`, and optional `AgentThread` API split with
  clear ownership boundaries;
* first `ctx.step`, `ctx.parallel`, `ctx.agent::<T>()`, and `ctx.reduce`
  runtime shape;
* first `ctx.thread::<T>()` shape with opaque thread ids, retention
  policy, and redacted thread checkpoints;
* explicit budget, cancellation, and branch-failure policy for
  multi-step and parallel flows;
* public `FlowStreamEvent` schema that does not expose raw thoughts,
  provider reasoning tokens, raw prompts, tool args, or provider
  payloads;
* explicit `ParallelJoin` semantics for `All`, `AllSettled`,
  `FirstSuccess`, and `Quorum(n)`;
* unified observability mapping to `pocopine-observe`,
  `pocopine-logging`, and `pocopine-analytics`;
* a concrete first example app;
* a clear boundary that keeps debugger, agent harness, VS Code, skills,
  proc macros, provider adapters, and commercial UI work deferred until
  the core/facade API is proven.
