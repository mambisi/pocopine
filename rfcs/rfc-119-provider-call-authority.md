# RFC-119: Provider-call authority that internal calls cannot drop

**Status:** Draft
**Crates:** `pocopine-agenkit` (`ProviderContext`, `AgentConfig`, `compact_thread`), `pocopine-agenkit-core`
**Relates to:** `fix(agenkit): carry explicit options into compaction` (b4a5f9b9), RFC-073 §10.1 (re-authorize per frame — the same principle, one layer up)

## Summary

A host attaches per-call authority — billing identity, a tenant, a trace id — to
an agent today by putting it in `provider_options`. Those options reach the
provider on the turn's own request and nowhere else. Any call the runtime makes
on the host's behalf builds its own request and starts from empty, so the
authority silently disappears.

This RFC moves host-owned, non-wire authority off `provider_options` and onto
`ProviderContext`, which the runtime already threads through every provider
call. Inheriting becomes the default; dropping it becomes the deliberate act.

## The defect, as it actually happened

AgenKitty meters every model call: a durable budget hold is reserved before the
provider is dispatched and settled after. Identity travels as a private provider
option that the host's `Provider` decorator strips before Qwen sees the request.

Transcript compaction builds its own `GenerateRequest`:

```rust
// crates/pocopine-agenkit/src/server/agent.rs — before b4a5f9b9
    // Compaction is an internal summarization call, not an agent turn — it
    // doesn't inherit the agent's provider options.
    provider_options: serde_json::Map::new(),
```

The comment is correct and the consequence was not intended: the summarizer
reached the provider with no billing context, so the decorator saw an
unattributed call and passed it straight through. A long-running Thread could
spend on compaction outside both the account's weekly allowance and the global
cost ceiling, indefinitely, with no error and no log line.

The failure mode is what makes this worth an RFC. Dropping authority does not
break the call. It *succeeds* — just unmetered, unattributed, untraced. Nothing
surfaces until someone reconciles a provider invoice against their own ledger.

## Why b4a5f9b9 is not the end of it

That commit added a second, parallel map:

```rust
AgentConfig::compaction_provider_option(key, value)   // …alongside provider_option
```

It unblocks the immediate leak, and it is the right shape for genuinely
compaction-specific settings. As the mechanism for *authority* it has three
problems:

1. **Correctness is opt-in per internal call site.** A host must know that
   compaction exists, that it is a separate provider call, and that it needs the
   same context. Nothing in the type system says so.
2. **It does not generalise.** The next internal call — a title generator, a
   tool-result summarizer, a re-ranking pass — needs a third map, and every host
   that already shipped is silently wrong again the day it lands.
3. **A host cannot verify it complied.** `AgentConfig::provider_options` and
   `compaction_provider_options` are both private. AgenKitty tried to write a
   test asserting the context is on both lanes and could not: there is no
   accessor. The test was deleted rather than left broken.

There is a fourth smell. `provider_options` is a *wire* field — it is serialized
into the provider request. Server-only authority therefore has to be inserted,
carried, and then removed again before delegating:

```rust
// agenkitty: server/assistant/runtime/metered_provider.rs
const BILLING_CONTEXT_OPTION: &str = "__agenkitty_server_billing_context_v1";
fn take_billing_context(request: &mut GenerateRequest) -> …  // strip before the wire
```

A double-underscore key and a strip-before-send step are what a codebase does
when a value is in the wrong container.

## Proposal

`ProviderContext` is already the per-call, non-wire channel, it is already
passed to every provider call including compaction's, and its own documentation
anticipates exactly this:

```rust
// crates/pocopine-agenkit/src/server/provider.rs
/// is `#[non_exhaustive]` so future per-call inputs (deadline, request id) can be
/// added without breaking the trait again.
#[non_exhaustive]
pub struct ProviderContext {
    pub credential: Option<ProviderCredential>,
}
```

Add an opaque, host-owned extension set to it:

```rust
pub struct ProviderContext {
    pub credential: Option<ProviderCredential>,
    authority: Extensions,          // type-keyed, Arc'd, Clone, never serialized
}

impl ProviderContext {
    pub fn authority<T: Send + Sync + 'static>(&self) -> Option<&T>;
    pub fn with_authority<T: Send + Sync + 'static>(self, value: T) -> Self;
}
```

- The host attaches authority **once** (see *Where authority enters* below).
- The runtime constructs every `ProviderContext` from that one value, so an
  internal call inherits by construction rather than by remembering.
- Decorators read it with a typed accessor instead of a stringly-keyed map.
- It is not part of `GenerateRequest`, so it cannot reach the wire and there is
  nothing to strip.

Type-keyed (à la `http::Extensions`) rather than string-keyed: a host reading
its own type back cannot collide with another crate's, and the double-underscore
convention stops being necessary.

`ProviderContext` derives `Debug`, and authority is opaque host-owned data that
may hold a token — so the extension set must render opaquely (`"<authority>"`,
or a count), never its contents. §D10 is otherwise breached through a log line,
which is the same class of leak this RFC exists to close.

## Where authority enters

The carrier is `ProviderContext`; the *source* needs naming, because
`for_request` has three callers and they do not share a configuration surface:

| resolve site | configured by |
|---|---|
| `runtime.rs` (conversational loop) | `AgentConfig` — where AgenKitty's billing context lives today |
| `agent.rs` (typed `AiAgent` run) | `AiAgentBuilder` |
| `generate.rs` (`ctx.ai()` in flows) | the `Ai` builder — **no `AgentConfig` exists** |

So "attach it once where you configure the agent" is ambiguous, and the obvious
reading — put it on `AgentConfig` — silently starves the flow path, reintroducing
this RFC's own bug one layer over.

**Proposal: source authority the way credentials already are** — a host-supplied
resolver keyed on the principal, mirroring
`ProviderCredentials::resolve(provider, principal)`. That is the only surface all
three sites share, it is a shape already proven in this codebase (W6), and
`for_request` receives the per-principal resolved credential right next to where
the authority would sit.

One property holds whichever source is chosen, and is worth stating: because
this is a **signature** change rather than a map lookup, a resolve site that
fails to supply authority fails to *compile*. Silent-wrong becomes
compile-error-wrong — which is the property the whole RFC is arguing for.

`provider_options` and `compaction_provider_options` keep their current meaning
— provider-specific *wire* fields, per call site — which is what they are good
at. Nothing about this RFC asks a host to stop using them for that.

## Migration

1. Land `ProviderContext::authority` and thread it through `for_request`.
2. AgenKitty moves `BILLING_CONTEXT_OPTION` to a typed authority value and
   deletes `take_billing_context` and the strip step.
3. `compaction_provider_option` stays for real compaction-specific settings.

Steps 1 and 2 are independent releases; nothing has to change atomically.

## Acceptance

- An agent configured with authority and then forced through compaction reaches
  the provider decorator **with** that authority — asserted in `pocopine-agenkit`,
  since it is the crate that owns the propagation.
- Authority never appears in a serialized provider request. A round-trip test
  over `GenerateRequest` is enough to pin it.
- A host can assert what it attached without needing private-field access.

## Alternatives considered

**Make compaction inherit `provider_options` wholesale.** Rejected in b4a5f9b9
for a good reason: it would also inherit search and thinking escape-hatch fields
onto a summarizer that must not have them. The problem is not that compaction
inherits too little — it is that authority and wire fields are in one container.

**Give every internal call its own option map.** This is the status quo,
extended. It scales with the number of internal calls and fails silently each
time one is added.

**Make the option maps public so hosts can at least test compliance.** Treats
the symptom. The host would still have to know each lane exists.

**Carry it on `Principal` / `AuthUser.claims`.** Tempting, and worth dismissing
explicitly rather than by omission: the principal already reaches every provider
call through the `CURRENT_PRINCIPAL` task-local, so there is nothing to thread.
Rejected on two counts. `claims` is a `BTreeMap<String, _>`, so it reinstates
exactly the stringly-keyed collision problem the double-underscore convention
exists to work around. And authority is not identity — a tenant, a trace id, or
a budget handle are metering concerns that do not belong on an auth type, and
some of them have no principal at all (an anonymous or system-initiated run).

## Relationship to compaction usage recording

Distinct problem, opposite direction, already fixed separately — noted so the
two are not conflated. This RFC is the **outbound** path: authority reaching the
provider. Compaction also had a **return**-path defect — `summarize` called
`provider.generate` directly rather than through `run_model_step`, so its usage
was discarded and never recorded, leaving `AgentSession::usage()`/`cost()`
under-reporting by the whole of compaction. That landed on main as `8a335edf`.
Neither fix subsumes the other: authority can flow outward while spend stays
unrecorded, and vice versa.

## Open questions

- Should authority be `Clone` per call or `Arc`-shared for the agent's lifetime?
  Shared is cheaper and matches how a billing identity behaves; per-call is
  needed if a future internal call should carry a *derived* identity (for
  example, compaction charged under its own capability rather than the turn's).
- Should `ProviderContext` also carry the deadline and request id its doc
  comment mentions, so the same propagation work is done once?
- `Extensions` implementation: `http::Extensions` is the reference shape but
  pulling in `http` for one field is heavy; a hand-rolled
  `HashMap<TypeId, Arc<dyn Any + Send + Sync>>` is ~30 lines plus the manual
  `Debug` the §D10 note above requires.
