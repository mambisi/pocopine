# Vue 3 as a reference

Upstream reference repo: <https://github.com/pinglu85/vue3-deep-dive>.
The `7-intro-to-reactivity/`, `8-building-reactivity-from-scratch/deps.html`,
and `9-building-the-reactive-api/reactive.html` chapters walk through the
exact primitives we care about.

## Where Vue 3 and pocopine agree

| Concept | Vue 3 | pocopine (today) |
|---|---|---|
| "currently running effect" | global `activeEffect` | `CURRENT_EFFECT: Cell<Option<EffectId>>` |
| proxy trap on read | `get(target, key)` → `dep.depend()` | `get` closure → `track(scope_id, key)` |
| proxy trap on write | `set(target, key, val)` → `dep.notify()` | `set` closure → `trigger(scope_id, key)` |
| "the subscribers of a (target, key)" | `WeakMap<obj, Map<key, Set<Effect>>>` | `HashMap<(ScopeId, String), HashSet<EffectId>>` |
| effect rerun | `subscribers.forEach(e => e())` | drain `QUEUE` in microtask |

The shape is the same. We just key by `(ScopeId, key)` instead of
`(raw_object, key)` because (a) Rust can't hash a `JsValue` by identity
and (b) our scope already has a stable id.

## Where Vue 3 does more than we do

These are the specific Vue 3 features worth mining for our roadmap.
Links are to directory paths inside the reference repo.

### 1. `ref()` — a reactive single value

Vue wraps a scalar: `const r = ref(0); r.value++`. The `.value` access
flows through a minimal proxy that calls `track` / `trigger` against a
synthetic `(ref_id, 'value')` key. This is literally the "signal" we
sketched in `03-signals.md`; adopting Vue's naming (`ref` + `.value`) or
Solid's (`signal` + `count()`) is a taste question — the mechanics are
identical. **Recommendation**: call ours `signal` to avoid overlap with
Rust's `&ref` semantics and the `ref!` keyword proposals.

### 2. `computed()` — lazy memoized derivation

Vue's computed doesn't run its body eagerly. It returns an object whose
`.value` getter:
1. if a `dirty` flag is set, runs the effect and stores the result;
2. tracks the *caller* as a subscriber of the computed itself;
3. returns the cached value.

The effect's `scheduler` just sets `dirty = true` and re-notifies the
computed's own subscribers — it does *not* recompute until someone reads.
**Take-away for us**: `computed` needs an `effect` with a custom
scheduler, which means our `effect()` API needs to grow an options
argument. Sketch:

```rust
pub fn effect_with<F: Fn() + 'static>(
    f: F,
    opts: EffectOptions,
) -> EffectId;

pub struct EffectOptions {
    pub lazy: bool,
    pub scheduler: Option<Rc<dyn Fn(EffectId)>>,
}
```

### 3. `effect(fn, { scheduler })`

Schedulers are how Vue plugs in its job queue (pre / sync / post flush
timing). For us, the scheduler is a hook that replaces the default
"push to `QUEUE` + microtask flush" with "call this closure with the
effect id, you decide what to do." Biggest payoff: computed (above),
watchEffect (one-shot), and batched test-mode flush.

### 4. Deep / shallow / readonly

`reactive(obj)` wraps recursively — reading `user.address.street` returns
a proxy of `address`, which tracks `.street`. `shallowReactive` stops at
the first level. `readonly` forbids `set`.

Ours is flat-only today because a component is a single struct with
primitive/owned fields. The first time someone writes a
`Vec<TodoItem>` field and wants per-item reactivity, we'll hit this.
**Deferred**: proper nesting gates `pp-for`, so it's on the critical path
for that milestone, not this one.

### 5. Collection handlers (Map / Set)

Vue supplies separate proxy handlers for `Map`, `Set`, `WeakMap`,
`WeakSet` because their mutating methods bypass the normal property
traps. We don't face this directly — our "collections" would be
`Vec<T>` / `HashMap<K,V>` on the Rust side, and we'd build tracking
around macro-generated accessors, not JS proxy traps. Worth remembering
the *shape* though: reads track, writes trigger, and for collections you
also track a synthetic "iteration" key so anyone iterating is
re-notified on any insert/delete.

### 6. `effectScope()`

Vue groups effects so a parent can stop them all at once
(`scope.stop()`). Our equivalent would be "release every effect an
element owns" — which the walker already does on unmount via
`__pp_effects`. The generalization: let users group arbitrary effects
into a scope for manual teardown.

### 7. `trigger`'s `TriggerOpTypes`

Vue's trigger carries an op kind (`SET` / `ADD` / `DELETE` / `CLEAR`).
This is what makes iteration-aware tracking work — a plain SET doesn't
re-notify an iteration effect, but ADD or DELETE does. We can get away
without this for flat fields; it becomes necessary the moment we do
reactive collections.

## Direct adoption list, ranked

1. **Signal / ref** (trivial) — covered in `03-signals.md`.
2. **Computed with scheduler** (small) — needs `effect_with(opts)`.
3. **`effectScope`** (small) — we have the data structure already
   (`__pp_effects`); promote it to a public API.
4. **Flush timing tiers** (medium) — `queueJob` / `queuePostFlushCb`
   idea; good once we have transitions or async components.
5. **Deep reactive** (medium–large) — gates `pp-for`, park until then.
6. **Collection handlers** (large) — same gate as deep reactive.
7. **Op-typed triggers** (medium) — only after 5.

Reading path in the reference repo for us:
`9-building-the-reactive-api/reactive.html` first (it's the condensed
build), then back-fill with `8-building-reactivity-from-scratch/deps.html`
if anything's opaque.
