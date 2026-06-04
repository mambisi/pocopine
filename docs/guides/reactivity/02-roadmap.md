---
title: "Reactivity roadmap"
description: "Candidates for the next milestone, ordered by 'most leverage per line of code.' Each row names the feature, a one-line why, the main tradeoff, and rough…"
---

# Reactivity roadmap

Candidates for the next milestone, ordered by "most leverage per line of
code." Each row names the feature, a one-line why, the main tradeoff, and
rough scope. Pick 2–3 per milestone.

| # | Feature | Why | Main tradeoff | Scope |
|---|---|---|---|---|
| 1 | **Signals** (typed reactive cells) | First-class reactive primitives that don't pay the `JsValue` round-trip on every read. Foundation for 2–4. | Two models side by side (proxy fields AND signals) until we unify. | Medium |
| 2 | **Computed** (memoized derivation) | `let full = computed(|| first() + last())` pays once per change, not per reader. | Needs a "dirty flag + lazy recompute" pattern, not just `effect`. | Small–medium |
| 3 | **Fine-grained handler triggers** | `#[handlers]` macro tracks `&mut self.field` access and triggers only the keys touched, replacing `trigger_scope`. | Macro has to pattern-match assignments; a few corner cases (method calls, `Deref` targets). | Medium |
| 4 | **`$watch`** | Imperative watcher: `$watch("count", cb)` runs `cb` on change without a DOM directive. | Needs a way to tear down (own-cleanup, or return a handle). | Small |
| 5 | **Stores** (`$store`) | Cross-component state; cleans up most "prop drilling" patterns. | API shape question: global `Proxy`? Module-scoped registration? | Medium |
| 6 | **Effect cleanup hooks** (`on_cleanup`) | Timers, subscriptions, event listeners can tear down correctly. | Have to remember to call registered closures on rerun AND on `release`. | Small |
| 7 | **Scheduler tiers** (pre / flush / post) | Parity with Alpine's `nextTick`/`cleanupScheduler`; enables ordering without hacks. | Changes the microtask flow — easy to regress on determinism. | Medium |
| 8 | **Nested reactivity** | Write to `user.name.first` and have dependents of the full path rerun. | Deep-proxy wrap adds overhead; needs a clear "wrap vs passthrough" rule. | Large |
| 9 | **Array / Vec tracking** | `pp-for` needs index-stable keying; without it every push retraverses. | Keyed vs positional diff choice; lots of edge cases. | Large (gates `pp-for`) |
| 10 | **Batching API** (`batch(\|\| …)`) | Dedupe triggers inside a larger imperative block. | Tiny surface; easy to add after #3. | Trivial |

## Suggested first slice: 1 + 2 + 4 + 6 + 10

A self-contained "signals tier" that sits *alongside* the current proxy
model without replacing it. Components can keep using `#[component]` fields;
power users reach for `signal()` / `computed()` / `watch()` when they need
performance or imperative control. Cleanup hooks and batching come cheap
once signals exist.

Deferred to the *next* slice: #3 (fine-grained triggers, needs macro
surgery), #5 (stores, design question), #7 (scheduler tiers — worth doing
after we have real workloads to measure against), #8–9 (big, gated by
`pp-for` anyway).

## Open questions to settle before starting

- **Do signals call directly, or through a getter?** `count()` vs
  `count.get()`. JS-land Solid/Preact-signals picked calls; Leptos picked
  `.get()/.set()`. Calls read cleaner but hide that you're invoking a fn.
- **Can a signal hold a non-`Clone` value?** If yes, the getter has to
  return `Ref<T>`, which complicates the call site. Default to `Clone` and
  add a `with` escape hatch.
- **Do signals integrate with the proxy?** Option A: they're a separate
  track/trigger pool with the same flush. Option B: each signal gets a
  synthetic `(ScopeId::SIGNAL, id)` key in the existing `DEPS`. B is
  smaller code and automatically unifies flushing; prefer B unless we hit
  a reason not to.
- **Who owns computed's cached value?** `Rc<Cell<Option<T>>>` if `T: Copy`,
  `Rc<RefCell<Option<T>>>` otherwise. Pay the `RefCell` cost universally
  to keep the API uniform.
