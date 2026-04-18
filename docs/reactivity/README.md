# Reactivity

Docs in this folder, in reading order:

1. [`01-current-design.md`](./01-current-design.md) — the model as it
   exists today (effects, deps, proxy traps, microtask flush).
2. [`02-roadmap.md`](./02-roadmap.md) — the short list of things we want
   next (watchers, computed, stores, nested reactivity, schedulers), with
   tradeoffs and a suggested order.
3. [`03-signals.md`](./03-signals.md) — deep-dive on what a "signal"
   layer would look like under our current effect engine.
4. [`04-vue3-reference.md`](./04-vue3-reference.md) — how Vue 3's
   `effect` / `reactive` / `ref` / `computed` map onto our primitives,
   and what to adopt from it.

The code lives in `crates/pocopine-core/src/`:
- `reactive.rs` — `effect` / `track` / `trigger` / `flush`
- `scope.rs` — `ComponentState` trait + `into_proxy()`
- `handler.rs` — `HandlerDispatch` trait (macro-side)
