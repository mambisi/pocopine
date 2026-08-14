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

- The host attaches authority **once**, where it configures the agent.
- The runtime constructs every `ProviderContext` from that one value, so an
  internal call inherits by construction rather than by remembering.
- Decorators read it with a typed accessor instead of a stringly-keyed map.
- It is not part of `GenerateRequest`, so it cannot reach the wire and there is
  nothing to strip.

Type-keyed (à la `http::Extensions`) rather than string-keyed: a host reading
its own type back cannot collide with another crate's, and the double-underscore
convention stops being necessary.

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

## Open questions

- Should authority be `Clone` per call or `Arc`-shared for the agent's lifetime?
  Shared is cheaper and matches how a billing identity behaves; per-call is
  needed if a future internal call should carry a *derived* identity (for
  example, compaction charged under its own capability rather than the turn's).
- Should `ProviderContext` also carry the deadline and request id its doc
  comment mentions, so the same propagation work is done once?
