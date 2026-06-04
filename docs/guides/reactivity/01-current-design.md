---
title: "Current reactivity model"
description: "Everything lives in per-wasm-module thread_locals (wasm is single-threaded, so this is just 'globals without the unsafe'):"
---

# Current reactivity model

## The five thread-locals

Everything lives in per-wasm-module `thread_local`s (wasm is single-threaded,
so this is just "globals without the unsafe"):

| TL | Type | Role |
|---|---|---|
| `NEXT_ID` | `Cell<u64>` | Monotonic id source for both scopes and effects. |
| `CURRENT_EFFECT` | `Cell<Option<EffectId>>` | Which effect is running right now. Read during `track`. |
| `EFFECTS` | `HashMap<EffectId, Rc<dyn Fn()>>` | The effect body. Rerun by id. |
| `DEPS` | `HashMap<(ScopeId, String), HashSet<EffectId>>` | Forward map: "when this field changes, rerun these effects." |
| `REVERSE` | `HashMap<EffectId, HashSet<(ScopeId, String)>>` | Back map: "this effect subscribes to these fields." Needed to clear stale deps on rerun. |
| `QUEUE` | `HashSet<EffectId>` | Pending reruns. Drained by `flush`. |
| `FLUSH_SCHEDULED` | `Cell<bool>` | One microtask at a time. |

## The lifecycle of a read

```text
directive body runs inside effect(f)
  └─ CURRENT_EFFECT = Some(id)
     └─ Reflect::get(&proxy, "count")
        └─ proxy.get trap fires (in JS, calling back to Rust)
           └─ track(scope_id, "count")
              ├─ DEPS[(scope_id, "count")].insert(current_effect)
              └─ REVERSE[current_effect].insert((scope_id, "count"))
           └─ return state.borrow().get("count")   // macro-generated
```

## The lifecycle of a write

Two paths, both end in `trigger`:

**Through the proxy** — e.g. a `pp-model` input event or `$dispatch`
writing to a field:

```text
Reflect::set(&proxy, "count", 3)
  └─ proxy.set trap
     ├─ state.borrow_mut().set("count", 3)
     └─ trigger(scope_id, "count")
        └─ QUEUE.extend(DEPS[(scope_id, "count")])
        └─ schedule_flush()
```

**Through a handler** — `#[handlers] fn increment(&mut self) { self.count += 1; }`
mutates Rust state directly, bypassing the proxy. We can't know which
fields were touched from inside a plain `&mut self` method, so
`Scope::invoke` calls `trigger_scope(id)` after the handler returns,
which fans out to every currently-tracked key of that scope. Coarser than
`trigger` but correct.

## Flushing

`schedule_flush` spawns a micro-task via
`JsFuture::from(Promise::resolve(&JsValue::NULL)).await`. When it resolves,
`flush` drains the queue and re-runs each effect.

Re-running an effect: `clear_deps_for(id)` first (via `REVERSE`), then set
`CURRENT_EFFECT`, run the body, restore. The clear-first step is what makes
conditional reads correct — if the effect ran `if a { b } else { c }`
and `a` flipped, the old dep on `b` is dropped before we rebuild the dep
set around `c`.

Effects that re-queue themselves during a flush land in the **next**
batch, not the current one, because `QUEUE.drain()` happens before any
effect body runs.

## Boundaries / known gaps

- **Reactivity is per-field, by name**. `"count"` is a string key, matched
  against the component's declared fields. No nested field tracking,
  no array element tracking, no index tracking in collections.
- **All reads go through `serde_wasm_bindgen::to_value`** on every proxy
  `get`. That's ergonomic (any `Serialize` field just works) but wasteful
  for hot reads.
- **Handler mutations trigger every key in scope**. Fine for a counter,
  bad for a component with one "hot" field and many cold ones.
- **No cleanup hooks** inside an effect — if an effect opens a resource
  (e.g. a `setInterval`), it can't register a tear-down to run on rerun
  or release.
- **No computed values**: nothing memoizes a derivation; two effects that
  both read `a.first + a.last` both pay the cost and both re-run when
  either field changes.
- **No global stores**. `$store` is documented-as-magic but not wired.
- **Scheduler is single-tier**. Alpine has pre/post/idle groups; we don't.
