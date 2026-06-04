# Performance Strategy

This document captures the current performance view for pocopine,
especially for large keyed lists and `js-framework-benchmark`-style
workloads.

## Current problem

Pocopine is paying too much generic runtime cost per repeated row.

For large keyed `pp-for` lists, we still do too much of this work at
runtime:

- template walking,
- directive dispatch,
- expression parsing,
- effect setup,
- row-scope invalidation,
- generic subtree removal/reinsert logic.

That is acceptable for ordinary UI. It is not acceptable for
`10,000`-row workloads.

## What Vue and Svelte do better

### Vue

Vue’s compiler feeds the renderer exact update hints:

- **patch flags** tell the runtime what can change,
- **stable fragments** let it skip child-order work,
- **tree flattening** means it only traverses dynamic descendants.

Official reference:
<https://vuejs.org/guide/extras/rendering-mechanism>

Important consequence:

Vue does not rediscover the meaning of a simple row on every update.
The runtime already knows which parts are dynamic.

### Svelte

Svelte shifts much more work to compile time:

- components compile to direct DOM operations,
- there is no generic runtime-first template walker for each row,
- browser work is limited to the exact updates the compiler emitted.

Official references:
<https://svelte.dev/>
<https://v4.svelte.dev/>

Important consequence:

Svelte wins not just because it avoids a virtual DOM, but because it
avoids generic runtime interpretation in hot repeated paths.

### Solid as an additional reference

For update precision, Solid is a useful model:

- fine-grained subscriptions,
- targeted updates,
- minimal reruns.

Official reference:
<https://docs.solidjs.com/advanced-concepts/fine-grained-reactivity>

We are not trying to become Solid, but the lesson is important:
unchanged rows should not pay row-wide invalidation cost.

## What this means for pocopine

The key lesson is not “wasm is faster”.

The benchmark measures rendering duration including DOM work. Rust/WASM
helps only when we use it to:

- precompute more,
- allocate less,
- skip more,
- patch fewer nodes.

So the correct Rust-first strategy is:

1. **compile row plans**
2. **keep row caches dense**
3. **patch DOM directly**
4. **avoid generic runtime interpretation in hot loops**

## Strategy

### 1. Make `10,000` rows the main target

The important workloads are:

- `runLots(10000)`
- `update every 10th`
- `add(1000)` on a large table
- `clear` from `10,000`

`run(1000)` still matters, but it is not the main bar.

### 2. Add compiled `pp-for` row plans

For simple keyed row templates, the runtime should not treat every row
as a fresh mini-template that must be walked and rebound from scratch.

Instead:

- analyze the row once,
- compile a compact binding/listener plan,
- clone and patch directly from that plan.

See RFC 054.

### 3. Keep the generic path as fallback

The fast path should only apply to simple eligible rows.

Complex templates still use the current generic walker/runtime path.

### 4. Reduce row-wide invalidation

For reused keyed rows:

- unchanged rows should not retrigger row-local updates,
- changed rows should patch only the bindings that actually changed.

This is the most important update-path principle.

### 5. Batch mount and removal work

Large bulk operations should use:

- `DocumentFragment` insertion,
- cheap no-transition removal paths when safe,
- fewer per-row browser round trips.

## Immediate implementation priorities

### Priority 1: `runLots(10000)`

Goal:

- cut per-row mount/setup cost,
- avoid generic walker work for simple rows,
- batch insertion aggressively.

### Priority 2: `update every 10th`

Goal:

- stop broad row invalidation,
- patch only changed row bindings,
- avoid rerunning unchanged rows.

### Priority 3: `clear`

Goal:

- cheap bulk removal path when no transitions are present.

### Priority 4: `add(1000)`

Goal:

- reuse the same row-plan and bulk-insert optimizations from
  `runLots(10000)`.

## Current partial wins

The codebase already includes some low-level improvements:

- memoized `pp-text`,
- expression parse caching,
- batched suffix insertion in keyed `pp-for`,
- no-transition fast remove path,
- skipping some unchanged row retriggers.

These are worth keeping, but they are not enough on their own.

## Success criteria

The strategy is working when:

- `runLots(10000)` drops materially, not just `run(1000)`,
- `update every 10th` reflects changed-row work instead of whole-list work,
- large-list costs are dominated by actual DOM work rather than framework
  interpretation overhead.

## References

- Vue rendering mechanism:
  <https://vuejs.org/guide/extras/rendering-mechanism>
- Svelte homepage / compiler positioning:
  <https://svelte.dev/>
  <https://v4.svelte.dev/>
- Solid fine-grained reactivity:
  <https://docs.solidjs.com/advanced-concepts/fine-grained-reactivity>
- js-framework-benchmark:
  <https://github.com/krausest/js-framework-benchmark>
