# RFC-118: Subagent primitives — lineage, fork-as, event kinds

**Status:** Proposal
**Crates:** `pocopine-agenkit` (thread lineage, `fork_as`), `agenkitty-core` (two session event kinds)
**Relates to:** RFC-093 (agenkit), `server/session` doctrine ("forest of single-writer logs"), roadmap-agenkitty M6 (`session.fork` UX gate), the downstream AgenKitty app

## Summary

**Decision: subagent orchestration is app territory.** The `subagent.*` tool family, agent
spec format, budgets, spawn gating, and approval UX are product/policy and live in the
consuming app, not the framework. An app can build all of it on today's public surface: a
registered tool struct may hold an `Arc` to the app's own orchestrator (the same pattern
every agenkitty family uses), and `AgentSessionBuilder`, `abort_handle`, `usage`/`cost`,
`SessionHost::fork_live_session`, `SecretRuntime::export/import_inheritable_grants`, and
`McpRuntime::fork` are all public.

Exactly three things cannot be built from outside. This RFC ships those, plus a short
normative recipe for the safe composition:

1. **Lineage on fresh threads** — create a child thread that carries a
   `ParentLink { parent, fork_at_seq: 0 }` provenance edge *without* inheriting any
   records. Today only `fork()` writes a parent pointer, and it always drags the
   transcript along.
2. **`AgentSession::fork_as(agent_id, config)`** — fork the thread but hand it to a
   *different* agent identity/config. Today `fork()` clones the parent's config and
   agent id, which are private and fixed at builder time.
3. **`SessionEventKind::{SubagentStarted, SubagentFinished}`** — the enum
   (`agenkitty-core/src/sessions.rs:90-104`) is closed and its `Unknown` variant is not
   `#[serde(other)]`, so a downstream app can neither add kinds nor rely on graceful
   fallback. Two additive variants + `#[serde(other)]` on `Unknown`.

This supersedes the earlier full-family draft (briefly `rfc-116-subagents.md`, withdrawn
before commit; 116/117 are now claimed by the inline-`poco!`/fmt RFCs).

## Motivation

Delegation needs a family tree. Whatever surface an app gives the model
(`subagent.run`, spawn/wait handles, anything), the substrate requirement is the same and
was written into the session layer from the start (`server/session/mod.rs:1-49`): *"a
sub-agent or a parallel branch gets its own session, linked by reference — a forest of
single-writer append-only logs."* "Linked by reference" is the part the public API cannot
express today except by full-context fork. Each primitive below closes one such gap; none
of them adds state, policy, or orchestration to the framework.

Keeping the family itself downstream follows standing doctrine: `agenkitty/src/lib.rs`
excludes orchestration by design; pocopine-chat was parked as product-not-core-lib; the
house rule is traits and seams, not bundled behavior. Building host-side delegation in the
app also satisfies the roadmap's precondition for model-facing `session.fork`
("after fork UX is proven host-side" — `roadmap-agenkitty.md:183,211`).

## Primitive 1 — lineage without inheritance

### The gap

Every session thread can carry a parent pointer:
`ParentLink { parent: ThreadId, fork_at_seq: u64 }` (`session/mod.rs:171`), meaning
"I descend from `parent` and inherit its records `[0, fork_at_seq)`". The tree is
navigable both ways (`SessionStore::children`, ancestor materialization) and deletion is
lineage-aware (`HasChildren`).

But the **only writer** of `ParentLink` is the fork path (`Session::fork`,
`SessionThreadStore::fork`), which always links at the current end — parent pointer and
transcript inheritance are welded together. A subagent child usually wants the opposite
split: an **empty context** (that is the point of delegating — the child burns its own
window, not the parent's) yet a **recorded spawned-by edge** so the host can walk, export,
and audit the tree.

The encoding already exists for free: `fork_at_seq: 0` inherits `[0, 0)` — nothing — and
is purely a provenance edge. What's missing is any public way to create a thread with it:
`AgentThreadStore::create` (`thread.rs:97`) takes no parent, and
`AgentSessionBuilder::open(None)` bottoms out there. An app's only workaround is stuffing
a parent id into the free-form `attributes` JSON — a private convention the session
layer's own tree walk (`children`, materialize, `HasChildren`) never sees.

```text
      fork() today                          primitive 1 (spawn edge)

P: [m1 m2 m3 m4]                        P: [m1 m2 m3 m4]
        │ ParentLink{P, fork_at_seq:4}          │ ParentLink{P, fork_at_seq:0}
        ▼                                       ▼
C: (m1 m2 m3 m4 inherited) + own log    C: own log only — empty start,
   "same history, branched"                but children(P) still finds C
```

### The API

```rust
// pocopine-agenkit — thread.rs
pub trait AgentThreadStore {
    /// Create a thread linked to `parent` by a records-empty ParentLink.
    /// Default: Err(unsupported) — a store that cannot record lineage must
    /// fail loudly, never silently drop the provenance edge.
    fn create_child(&self, agent_id: &str, owner: ThreadOwner<'_>,
                    retention: ThreadRetention, parent: &AgentThreadId)
        -> BoxFuture<'_, AgenkitResult<AgentThreadId>> { /* Err */ }
}
// SessionThreadStore overrides it: create_thread(Some(ParentLink{parent, fork_at_seq: 0}),
// attributes {owner, agent_id}) — owner scoping identical to create().

// runtime.rs
impl AgentSessionBuilder {
    /// Link the (fresh) thread this builder creates to an existing parent thread.
    /// Only applies when open(None) creates; resuming an existing thread ignores it.
    pub fn parent(mut self, parent: AgentThreadId) -> Self;
}
```

Owner rules are unchanged: the parent must be owned by the same principal (checked like
any other owner-scoped read; foreign/missing indistinguishable). `state_changes()`
already reads own-records-only, so a lineage edge can never double-count usage.

## Primitive 2 — `fork_as`: same history, different agent

### The gap

`AgentSession::fork()` (`runtime.rs:932`) exists for **"same agent, alternate
timeline"**: it forks the thread at its current end and returns a new `AgentSession`
that is a clone of the parent's *identity* — same `AgentConfig` (model, system prompt,
tool allowlist, step cap), same `agent_id`, same hooks. That is the right shape for
branching/undo UX.

A fork-*context* subagent is the other quadrant: **"different agent, inherited
knowledge."** It forks precisely because it needs the conversation so far as context —
but it must then run as its own agent: its own instructions as the system prompt, a
narrowed tool list, possibly a cheaper model. Those knobs are private fields on
`AgentSession`, fixed at builder time; nothing on the forked session lets an app swap
them. So today an app choosing fork-context children is forced to run the child as a
full-powered copy of the parent — the opposite of attenuation.

```text
                     inherits transcript?
                       yes           no
   same identity   │ fork()      │ (plain open — new session)
   own identity    │ fork_as()   │ builder + parent()   ← primitives 2 and 1
                   │  MISSING    │      MISSING
```

### The API

```rust
impl AgentSession {
    /// Fork this session's thread (same semantics as fork()) but hand the child
    /// timeline to a different agent identity and config.
    pub async fn fork_as(&self, agent_id: impl Into<String>, config: AgentConfig)
        -> AgenkitResult<Option<AgentSession>>;
}
```

Semantics: thread fork identical to `fork()` (child inherits the full record range;
parent untouched; owner inherited). The child session gets the given `config`/`agent_id`,
fresh `TurnControls`/abort/busy state, and the parent's hooks unless the caller replaces
them. Implementation note: a fork inherits the parent's attributes including `agent_id`,
and `update_attributes` rightly rejects the reserved key — `fork_as` must therefore set
the child's `agent_id` attribute at creation time through the store, not post-hoc.

`fork()` remains and becomes the trivial case (`fork_as(self.agent_id, self.config)`).

## Primitive 3 — subagent event kinds

`SessionEventKind` (`agenkitty-core/src/sessions.rs:90-104`) is a closed enum, and its
`Unknown` variant lacks `#[serde(other)]` — an unrecognized kind string is a
deserialization *error*, not a fallback. A downstream app therefore cannot introduce
subagent lifecycle events; it would have to mislabel them as `NoteCreated` or similar,
which poisons event filtering and policy classification.

Change (all additive):

```rust
pub enum SessionEventKind {
    // …existing…
    SubagentStarted,    // payload: child thread id; source_refs: ToolCall + Thread(child)
    SubagentFinished,   // payload: status + usage/cost rollup; source_refs: Thread(child)
    #[serde(other)]
    Unknown,            // NEW attribute: future kinds degrade gracefully
}
```

The inverse edge on the child side needs no new kind — the child's `Started` event
already carries `SessionSourceRef::Thread { parent }` via the existing provenance
vocabulary. `SessionEventKindFilter` and the event-policy classification gain the two
kinds (`SubagentStarted`/`SubagentFinished` classify as side-effecting/read-only
respectively at the model-visible policy layer).

## The recipe (normative for app implementors, shipped as a guide)

A short guide — `docs/guides/agenkitty/subagents.md` — documents the canonical safe
composition instead of the framework coding it. Checklist:

1. **Attenuation:** `child_tools = (spec ∩ parent ∩ call_args) − subagent-family`;
   widening or an empty intersection is a loud spawn error. Recursion off by default.
2. **Fail-closed children:** wire the same `PolicyEvaluator` into the child's
   `before_tool_call`; give the child **no approver** unless explicitly opted in, so
   every `Ask` resolves to `Deny`.
3. **Spawn gate:** mirror `SecretRequestPolicy` (`mode` + preapproved set; `Ask` requires
   the shared `ToolApprover`; absent approver ⇒ deny).
4. **Fork boundaries:** per child, `McpRuntime::fork()`;
   `import_inherited_grants(export_inheritable_grants(..))` for secrets (grants, never
   values); memory context under the child's `agent_id` (its `Agent`-scope namespace
   isolates automatically).
5. **Budgets:** cap child count, depth, wall-clock (`tokio::time::timeout` +
   `abort_handle`), and report bytes; cost ceiling from the child's own recorded
   `UsageRecord`s.
6. **Report flow:** return the child's final assistant text as ordinary tool output —
   the session redactor's transitive coverage then applies with no extra work.

## Non-goals

- The `subagent.*` tool family, `AgentSpec` activation, spawn host, and budget
  enforcement in the framework — app territory. Revisit only if a second consumer
  demands a shared shape (`ToolKind::SubAgent` and `supervisor::AgentSpec` stay reserved
  for that day).
- Per-child workspace/sandbox isolation, agent-to-agent messaging, remote subagents,
  model-facing `session.fork` — unchanged from the prior analysis, all out of scope.

## Phasing

| phase | ships | gate |
|---|---|---|
| **P1** | primitive 1 (`create_child` + `builder.parent()`), primitive 3 (event kinds + `serde(other)`), the guide | fresh child thread carries `ParentLink{…, 0}`; `children(parent)` finds it; child starts with empty history; unsupported store errs loudly; old persisted logs still decode |
| **P2** | primitive 2 (`fork_as`) | forked child sees parent history verbatim, runs under its own config/agent_id; parent log untouched; `fork()` behavior unchanged |

## Open questions

1. Does the guide live upstream (`docs/guides/agenkitty/`) or in the downstream app repo
   next to its implementation? Upstream proposed, since the invariants it documents are
   framework invariants.
2. Should `builder.parent()` on a resume (`open(Some(id))`) be an error rather than
   silently ignored? Proposed: error — mixing lineage intent with resume is a bug.
