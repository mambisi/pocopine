# agenkit roadmap — from one-shot flows to a long-running generation loop

Agenkit today (RFC-093) drives **single-shot, server-side, redacted flows**: a
`#[server]` fn calls `agenkit.flow(Marker)`, the runtime turns one request into
one typed result, and the `#[server]` boundary drops prompts, reasoning, and
provider internals on the way to the client (§D10). That is the right shape for
a flow — and exactly why the SDK has no model metadata, no cost meter, no
context-window awareness, and only coarse streaming. It never needed them.

The next line of work matures agenkit into an SDK that can drive a
**long-running, multi-turn, interactive generation loop** — the substrate a
conversational agent will sit on — plus the durable-session and credential
pieces that loop will need. Each workstream is independently shippable and
gated. The redaction boundary does not move: everything below adds richness
**server-side** and keeps the wire client-safe.

> Spec base: [RFC-093](../../rfcs/rfc-093-pocopine-agenkit.md). New workstreams below are
> spec'd in this doc; allocate RFC numbers from `git worktree list` (sibling
> branches reserve numbers) before promoting any to its own RFC.

| # | workstream | ships | gate |
|---|---|---|---|
| **W1** | **Model catalog** | A `Model` descriptor (context window, max output, reasoning/vision support, input/output/cache **prices**) + a curated, host-only catalog keyed by `ModelRef`; resolution in `Ai::resolve_model`. | Every built-in alias resolves to a descriptor; an unknown alias degrades to `None` with a one-time warn (never a hard error); catalog stays out of the wasm bundle. |
| **W2** | **Usage · cost · cache metering** | `Usage` gains `cache_read_tokens` / `cache_creation_tokens`; `CostEstimate` is actually computed (`cost(model, usage)` from W1 rates); the already-wired `observe.rs` cost path gets its allow-list keys; the dead `FlowStreamEvent::UsageUpdate` is emitted. | A live Anthropic + OpenAI-compat call yields populated cost **and** cache tokens in the trace; cost within ε of a hand-computed figure. |
| **W3** | **Context-window survival** | An overflow detector — provider-error matcher **+** proactive `usage.input > context_window − headroom` (needs W1) — surfaced as a typed signal (`AgenkitError::ContextOverflow` + a pre-call estimate). | A synthetic overflow from each provider is detected and classified; the proactive trigger fires at the configured headroom; the signal is consumable by W7. |
| **W4** | **Reasoning ("thinking") content** | `ContentPart::Thinking { text, signature }` (server-side, opaque signature passthrough); providers parse + replay it; a thinking-level control on the request. **Redaction-gated at the wire.** | A reasoning model's thinking + signature round-trips across a 2-turn server-side run; the client wire carries **zero** thinking under default `StreamMode` (redaction test green). |
| **W5** | **Richer streaming events** | `StreamChunk::Thinking` (internal) → `FlowStreamEvent::ThinkingDelta` (wire), classified in `stream_filter`; a terminal **error-with-partial** event so an aborted/failed stream yields the partial instead of only a thrown `Result`. | A streamed run emits incremental text + thinking + usage deltas; an aborted stream delivers a terminal partial; `stream_filter` still gates every variant (won't compile otherwise). |
| **W6** | **Pluggable provider credentials + OAuth** | A `ProviderCredentialsStore` trait (resolve per-`(provider, principal)` → key/token, refresh on expiry); env-backed default keeps `from_env`. OAuth-subscription auth as a sibling credential-type crate. | A provider call resolves a credential from a pluggable store (env still the default); an OAuth token refreshes transparently on expiry; keys never logged. |
| **W7** | **`agenkit::server::session`** (host module) | A durable, branchable conversation `Session` + `SessionStore` trait — a single-writer append-only event log (JSONL) + a small KV/SQL index for metadata & parent pointers; resume/fork; compaction driven by the W3 overflow signal. Threads graduate here. Collab is a tool, not the backend. | A session survives process restart; fork mints an independent thread with a parent pointer (parent untouched); compaction triggers from the overflow signal, writes a checkpoint, and does NOT duplicate replaced history or inline large tool outputs. |

**Sequencing.** `W1 → {W2, W3} → W4 → W5 → W6 → W7`. W1–W3 are small and mechanical; **W4 carries the only real design decision** (where reasoning lives vs. what the wire redacts); W5 is a richer event enum; W6 is the credential seam; W7 is the durable layer that consumes the rest.

---

## Workstream detail (touchpoints)

### W1 — Model catalog *(keystone)*

A bare `ModelRef("provider/model")` is fine to hand a provider, but a loop needs each model's **context window** (W3 + a UI meter), **price rates** (W2), and **reasoning/vision support** (W4). Everything else leans on this.

- **New:** `Model` descriptor in `pocopine-agenkit-core` (sibling to `Usage`/`CostEstimate` in `src/trace.rs`, or a new `src/model.rs`): `{ alias, provider, context_window, max_output, supports_reasoning, supports_vision, price: { input, output, cache_read, cache_creation } }` (per-Mtok). Serde + JsonSchema.
- **New:** a curated catalog (small, hand-maintained or codegen'd, **host-only**, never in the wasm bundle) + a lookup `catalog::get(&ModelRef) -> Option<&Model>`.
- **Edit:** `crates/pocopine-agenkit/src/server/generate.rs` — `resolve_model()` (≈L100) resolves the descriptor alongside the `ModelRef`; carry it on `GenerateRequest` (`provider.rs` L79) as `model_meta: Option<Model>` so tracing/cost/streaming can reach it.
- Capability note: per-model `supports_*` supersedes today's provider-wide `ProviderCapabilities` (`provider.rs` L32) for the degrade decision; keep the provider flags as the coarse fallback.

### W2 — Usage · cost · cache metering

Most of the plumbing already exists and is simply unpopulated.

- **Edit (core):** `pocopine-agenkit-core/src/trace.rs` — `Usage` (L15) gains `cache_read_tokens` / `cache_creation_tokens` (`#[serde(default)]` → non-breaking); add `TraceEvent::with_cost()` (the `cost: Option<CostEstimate>` field at L98 already exists).
- **New:** `cost(model: &Model, usage: &Usage) -> CostEstimate` (uses W1 rates; bills cache reads/writes at their own rate — e.g. Anthropic 1h cache-write ≈ 2× input).
- **Edit (runtime):** `generate.rs` `run()`/`run_streamed()` (≈L148–162 / L308–335) — after the provider returns `usage`, compute cost and attach it before emitting `ai_model_response`.
- **Edit (observe):** `server/observe.rs` `to_observed_event()` already maps cost if present (L85–89); add `cost_amount` / `cost_currency` + the cache-token keys to `PUBLIC_FIELD_KEYS` (L116).
- **Edit (providers):** `WireUsage` in `pocopine-agenkit-anthropic/src/wire.rs` (L322) parses `cache_read_input_tokens` / `cache_creation_input_tokens` (sent today, discarded); `pocopine-agenkit-oai/src/wire.rs` (L481) parses `prompt_tokens_details.cached_tokens`.
- **Edit (stream):** emit the dead `FlowStreamEvent::UsageUpdate` (`core/src/stream.rs` L173) so live token/cost meters work.

### W3 — Context-window survival

The signal that lets a long session compact-and-continue instead of hard-erroring.

- **New:** an overflow detector — (a) a provider-error string matcher (each provider's "context length exceeded" shapes), and (b) a proactive pre-call estimate `usage.input + est(messages) > model.context_window − headroom` (needs W1).
- **New:** `AgenkitError::ContextOverflow { .. }` (`core/src/error.rs`) so the loop can branch on it; plus a non-error `context_headroom(req, model) -> Headroom` probe for the proactive path.
- **Edit:** `generate.rs` classifies provider errors at the `Err` arms; the proactive check runs in `build_request`/`run`.
- Consumed by W7 compaction.

### W4 — Reasoning content *(the design decision)*

The strongest coding/reasoning models emit thinking. Two needs the current "drop it" stance breaks for a loop: (1) the `thinkingSignature` must be **replayed next turn** or providers error / lose continuity; (2) the UI wants to show/collapse thinking. The tension: §D10 *redacts* reasoning at the client boundary. Resolution: thinking lives **server-side** in the message model and is **redaction-gated at the wire** — retention and boundary-redaction are different layers.

- **New (core):** `ContentPart::Thinking { text: String, signature: Option<String> }` in `content.rs` (L13) — opaque `signature` is passed back to the provider verbatim next turn, never interpreted.
- **Edit (providers):** Anthropic `wire.rs` — promote `thinking` out of `ResponseBlock::Other` (L276 silently drops it today) into a real block + `StreamEvent` arm; OpenAI `wire.rs` `StreamDelta` (L505) parses `reasoning_content`. Both replay the signature on the next request.
- **New:** a thinking-level control on the request (`off | minimal | low | medium | high`), mapped per-model via W1.
- **Wire:** see W5 for `ThinkingDelta` + its redaction classification.

### W5 — Richer streaming events

- **Edit:** `StreamChunk` (`provider.rs` L54) gains `Thinking(String)` — internal currency only; `generate.rs` stream loop (L245) adds the match arm.
- **Edit:** `FlowStreamEvent` (`core/src/stream.rs`) gains `ThinkingDelta { text }` and a terminal **`Error { partial }`** so a failed/aborted stream carries the partial message (today errors are a thrown `Result`; `FinishReason` has no `Error`/`Aborted`).
- **Edit (redaction chokepoint):** `server/stream_route.rs` `stream_filter()` (L34) — classify `ThinkingDelta` as `Progress` (hidden under default `StreamMode`). The match is exhaustive-by-design: *a new variant won't compile until its wire visibility is decided* (§D10). This is the safety property; keep it.

### W6 — Pluggable provider credentials + OAuth

Today: env-only (`AnthropicProvider::from_env` reads `ANTHROPIC_API_KEY`); no per-user/per-provider resolution, no refresh, no OAuth (`pocopine-auth` is `Principal`/`AuthUser` + email-password only).

- **New:** `ProviderCredentialsStore` trait (host-only, `pocopine-agenkit` or a `pocopine-agenkit-auth` crate): `resolve(provider_alias, principal) -> Future<Credential>` + refresh. Mirrors the `TokenStore` shape from `pocopine-auth-credentials`. Default impl = env (keeps `from_env`); apps implement against their DB.
- **DONE:** OAuth (authorization-code + PKCE + refresh) shipped as the **provider-neutral `pocopine-auth-oauth` crate** (sibling to `pocopine-auth-credentials`) — the flow + `OAuthTokenStore` (keyed on an opaque `subject`) live there, so the same machinery can back a "Sign in with X" login later. agenkit's `OAuthCredentials` is a thin adapter (token → `ProviderCredential::Bearer`). `SecretString` moved to `pocopine-crypto` so tokens + credentials share one secret primitive. Token handling routes through `pocopine-crypto`/`pocopine-codec` as planned.
- Threads the resolved `Principal` (already flowing via the agenkit server plugin) to pick the right credential.

### W7 — conversation sessions (`pocopine-agenkit::server::session`)

> **Naming.** *Not* a separate `pocopine-sessions` crate — that name reads as a
> **web auth session** (a cookie → logged-in user; the `Session`/`SessionStore`
> contracts already in `pocopine-auth`). This is the agent **conversation
> transcript**, and only agenkit consumes it, so it lives **in agenkit's
> host-only `server::session` module** — no separate crate.

The durable, multi-turn conversation layer the loop runs on. agenkit stays stateless; **all conversation state lives here.** Threads (`AgentThreadDescriptor` / `ThreadMessage` / `ThreadCheckpoint` payloads + the in-memory thread store) graduate into a real durable session.

**A session is single-writer by construction.** The runtime that owns a run is its only writer; a sub-agent or a `ctx.parallel` branch gets its *own* session, linked by reference — never a shared concurrent log. So conversation state is a **forest of single-writer append-only logs**, not a collaborative document. There is no multi-writer concurrency on a session, so there is no CRDT and no `CollabStore` here.

- **`pocopine-agenkit::server::session`** (host-only module — **BUILT**):
  - `Session` — an **append-only event log** (JSONL): messages + model/thinking/tool-set changes + compaction checkpoints. Single-writer. Resume = replay the log; undo = move the leaf pointer to an ancestor.
  - **A small index sidecar** (an embedded KV / SQL — `sled` / SQLite / LevelDB) holds thread metadata, token usage, and the **parent/child pointers** that form the branching tree. The log is the source of truth; the index makes "list threads / find children / resume last" cheap, and the two must be kept consistent (handle an orphaned index entry / a missing log).
  - `SessionStore` trait — `append` / `load` / `fork` / `snapshot(checkpoint)` over that backend. Ship **in-memory + JSONL** for dev, KV/SQL-indexed for prod. `AgentThreadStore`'s payloads graduate into the log. **Not** `CollabStore`.
  - **Branching = a new thread with a parent pointer + a split point** (not a file copy, not an in-place mutation): `fork(parent, at)` mints a new log whose index entry points at the parent and a split point ("before the Nth user message", or "as-is / interrupted"). resume / fork / undo fall out of (leaf pointer + parent chain); a fork never touches the parent.
  - **Compaction** — remedial, consumes the **W3** overflow signal: summarize older turns into a **checkpoint** entry, keep recent verbatim, full history retained in the log. Two hard rules from prior art (open-source coding agents whose JSONL sessions ballooned to hundreds of MB–GB): (1) **don't re-persist the replaced history inline on every compaction** — the checkpoint *replaces* in the active context, it doesn't *duplicate* prior history into the log each pass; (2) **store large tool outputs out-of-line** (a blob ref in the log, bytes in a side store), never inline in the event stream. Don't compact so aggressively at a phase boundary (plan→execute) that you nuke the plan you're about to run.

- **DONE:** W7 shipped. `server::session` is the host-only durable layer: `SessionStore` (append-only `Record` log + `ThreadMeta`/`ParentLink` index) with **three** impls — `MemorySessionStore` (dev), `JsonlSessionStore` (cat-able log), `SqliteSessionStore` (indexed prod: `children`/`last_seq` are indexed queries, `append` one txn). `Session` handle gives resume / `fork` (new thread + parent pointer, parent untouched) / `checkpoint` / `history` / `active_context`. `AgentThreadStore` graduated onto it via `SessionThreadStore` (owner persisted in `attributes`, scope survives restart). Compaction is wired into the agent loop (consumes the W3 headroom signal). **Both anti-bloat rules hold:** (1) compaction keeps the recent tail verbatim *inside* the checkpoint payload — no duplicated history; (2) `ExternalizingSessionStore` + `BlobStore` (`Memory`/`Fs`) push oversized payloads out-of-line, content-addressed (dedup), rehydrated on read. Collab stayed a tool, never the backend.

**Collaboration is a tool, not the session backend.** Several agents/humans co-editing one *artifact* (a shared doc) is a different object, reached via a **tool call into `pocopine-collab`** — orthogonal to where the session lives. `CollabStore`/CRDT (genuine multi-writer) belongs there, never under the single-writer session. (Supersedes the earlier "reuse `CollabStore` as the backend / CRDT path" idea.)

**Prior art (validated against open-source coding agents):** OpenAI Codex is the closest match and confirms this shape — sessions are append-only JSONL rollout files (single-writer `RolloutRecorder`, one event/line) with a SQLite `state.db` indexing thread status, token usage, and **parent/child relationships**; forking creates an *independent* thread with a parent pointer + a split snapshot (`TruncateBeforeNthUserMessage(n)` / `Interrupted`), not a copy. Its documented failure mode — single rollout files reaching 700 MB–2 GB because compaction re-persisted replaced history and raw tool output was stored inline — is exactly what rules (1) and (2) above prevent. (Google Antigravity compacts at ~135k tokens but publishes no storage internals; its main lesson is the *timing* complaint — over-aggressive compaction at plan→execute wipes needed context.)

---

## Standing constraints (binding)

- **The §D10 redaction boundary does not move.** Every capability above adds richness *server-side*. The wire only ever speaks `FlowStreamEvent`, and every variant is classified in `stream_filter` (won't compile otherwise). Default `StreamMode` hides thinking and progress. Prompts, reasoning, provider payloads, and credentials never cross to the client.
- **agenkit stays a stateless SDK.** No conversation state in the agenkit crates; durability is W7's job. One request → one streamed result remains the core contract.
- **Additive-first.** New `ContentPart` / `StreamChunk` / `FlowStreamEvent` variants and `#[serde(default)]` `Usage` fields are non-breaking on the wire (the stream already has an `Unknown` fallback). The only exhaustive-match breaks are internal (`generate.rs`) or the *intentional* `stream_filter` gate. The flow/tool/agent authoring surface (`#[ai_tool]`, `#[ai_flow]`, `ctx.*`) is frozen.
- **The catalog is curated, overridable, and host-only.** Never ship a model catalog in the wasm bundle.
- **pocopine ships a credential *trait*, not a vault.** App-runtime secret management stays out of pocopine (the deploy-time `credentials.toml` is unrelated and unchanged).
- **Opinionated.** One canonical way per capability; no parallel mechanisms.

## Out of scope (this roadmap)

- The agent runtime **loop** itself (multi-turn orchestration, tool-loop control, the interactive UI) — that's the layer that *consumes* this SDK; separate roadmap.
- A secret manager / vault.
- Image / audio **generation** (input multimodal already exists via `ContentPart::Media`).
- Long-tail vendor breadth beyond Anthropic + OpenAI-compatible (gateways cover most).
- Auto-compaction policy tuning beyond a sane default (lives in `pocopine-sessions`).
