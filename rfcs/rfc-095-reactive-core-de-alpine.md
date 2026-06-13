# RFC 095 - Reactive core de-Alpine: correctness and speed

| Field | Value |
|---|---|
| **Status** | Implemented (landed in 0.2.0 via `perf-reactive-dirty-tracking`; W3c expanded into RFC-096). W2 landed (−7.1% geomean), W1 landed (proxy-free root reads), W3a landed (one dependency graph — fields interned as signals; DEPS/REVERSE deleted), dead Alpine magics removed (\$el/\$refs/\$dispatch/\$id); W5 tried and reverted with measurements; W0 COMPLETE (semantics tests + 1000-op differential fuzz vs oracle + keyed fast-path symmetry gates); W3b landed (plan-gated lazy proxy minting); W3c expanded into [`rfc-096-signals-first-reactive-core.md`](./rfc-096-signals-first-reactive-core.md) (the full signals switch); W4 LANDED (descriptor variant — runLots −8.7%, gap to vanilla 1.41×→1.29×; see §7) |
| **Author** | pocopine team |
| **Created** | 2026-06-10 |
| **Related** | [`rfc-054`](./rfc-054-compiled-row-plans.md), [`rfc-058-compiled-views-walker-removal.md`](./rfc-058-compiled-views-walker-removal.md), [`rfc-094-conditional-chains-and-enum-matching.md`](./rfc-094-conditional-chains-and-enum-matching.md) |
| **Supersedes** | - |

## 1. Summary

Pocopine's remaining Alpine inheritance is three costs, all in the
reactive core: (1) serde serialization on proxy field reads,
(2) blanket cache-invalidate + scope-wide trigger after every
handler, (3) proxy-trap bridge crossings per expression read.
Alpine measures 3.53× vanilla on the official js-framework-
benchmark; the raw `wasm-bindgen` entry measures 1.08× — the
ceiling is near-vanilla, from Rust, if these go.

This RFC stages the removal as five workstreams, under one hard
constraint: **authoring ergonomics are untouchable.** Handlers
keep plain `self.count += 1`; templates keep `open = !open`. No
`Signal<T>` fields in user structs, no `set_x()` setters — the
proxy remains the authoring surface; it leaves the hot path only.

| WS | What | Status |
|---|---|---|
| W0 | Correctness harness: semantics tests, differential oracle, fuzz | partial — sweep semantics tests landed |
| W2 | Per-field dirty tracking (fingerprints) replaces blanket sweep | **landed** — geomean −7.1% |
| W5 | Boundary hygiene (interning) | **tried, reverted** — measured net loss (§6) |
| W1 | Typed field access: kill serde-on-read (FastExpr `ComponentField` root) | future |
| W3 | Signals become the engine; proxy demoted to lazy compat shim | future, after W1 |
| W4 | Batched DOM-mutation channel (descriptor variant) | **landed** — runLots −8.7%, add −6%, run −5.5% |

## 2. The cost model (audit, 2026-06-10)

```
TODAY:  Rust struct ──serde per read──► JS object ──Proxy traps──► expr eval ──per-op──► DOM
             ▲                                          │
             └── handler mutates, then trigger_scope ───┘
                 swept EVERY tracked key of the scope          (fixed by W2)

TARGET: Rust struct ──per-field subscriber lists──► compiled binding
        closures (typed, no Reflect) ──batched writes──► DOM
```

Ranked inherited costs (profiler + code audit):

1. **Serialization-on-read** — proxy `get` cache miss runs
   `serde_wasm_bindgen::to_value` (`scope.rs`); pre-W2, `#[model]`
   handlers also paid 2× serialize + `JSON.stringify` per model
   field per invocation (`model_runtime.rs`).
2. **Coarse triggering** — pre-W2 `Scope::invoke` ended with
   `invalidate_field_cache` (drop everything) + `trigger_scope`
   (fire every tracked key). Profiler: 40–70% of state-sync time.
3. **Proxy-trap crossings** — each `Reflect::get` in an
   expression is a wasm→JS→wasm closure call that also defeats
   V8's inline caches (published: ~5× a plain access, before our
   bridge cost).

Key audit facts: the proxy is needed only for *auto-tracking*,
not access (the magics — `$el`/`$refs`/`$dispatch`/`$event` —
don't need it); `Signal`/`Computed` already exist Rust-side;
`FastExpr` already proves typed proxy-free evaluation for
`LoopScope`; the dependency table `DEPS[scope][key]` was already
per-field — only the write side ignored that precision.

## 3. W2 — per-field dirty tracking (landed)

After a handler or `Handle::update` closure returns, the runtime
must decide what changed without being told. The proxy `set` trap
knows (template assignments stay precise); `&mut self` mutation
doesn't. W2 answers with **fingerprints**:

```
begin (before handler):  for every OBSERVED key — tracked keys
                         (DEPS) ∪ cached keys (FIELD_CACHE) —
                         fingerprint the field: serde stream →
                         Fnv64, Rust-side, zero bridge crossings
finish (after handler):  re-fingerprint; changed = differing ∪
                         unknown(None)
                         → invalidate cache for changed only
                           (FRESH_FIELDS marks consumed; patched
                            slots survive)
                         → trigger changed keys only
fallbacks:               unknown fingerprint (computed, flatten
                         leaf, non-field key) ⇒ treated changed;
                         re-entrant borrow ⇒ blanket path.
                         Both reproduce pre-W2 behavior exactly.
```

Pieces:

- `pocopine_crypto::Fnv64` — streaming FNV-1a/64 implementing
  `core::hash::Hasher`; non-cryptographic, documented as such.
- `pocopine_core::fingerprint` — a serde `Serializer` feeding any
  hasher, with type/structure tag bytes so `1u8` / `"1"` / `[1]` /
  `Some(1)` don't collide, length-prefixed strings, delimited
  nesting. Property: *what the fingerprint sees is what the proxy
  sees* (both walk the field's `Serialize`).
- `ComponentState::field_fingerprint` (default `None`);
  `#[component]` and `#[store]` emit one arm per declared field —
  same `Serialize` bound as `get()`, so it compiles wherever
  `get` does.
- `DirtySweep` (`scope.rs`) wired into `Scope::invoke` and
  `Handle::update` (profiler buckets preserved).
- `model_runtime` snapshots switch from JSON.stringify to
  `field_fingerprint` (hashing the same direct-field serde stream
  `get_model_value` serializes — change semantics exact); event
  details serialize lazily, changed keys only.

Safety analysis: a *false changed* (HashMap iteration-order
wobble, unknown fingerprint) over-triggers — the pre-W2 status
quo. A *missed change* requires a 64-bit collision between the
before/after streams of one field (~2⁻⁶⁴ per write) or a field
whose `Serialize` hides UI-relevant data — which the proxy then
couldn't see either.

## 4. W2 measured results

Same-session jsbench, headless Firefox, mean ms, this machine:

| action | main | W2 | delta |
|---|---|---|---|
| run(1000) | 307.2 | 299.7 | −2.5% |
| update every 10th | 196.1 | 180.2 | −8.1% |
| select | 194.0 | 175.9 | **−9.3%** |
| swapRows | 178.4 | 163.9 | **−8.1%** |
| remove | 218.8 | 207.7 | −5.1% |
| clear | 313.4 | 274.2 | −12.5%¹ |
| runLots(10000) | 1261.6 | 1249.4 | −1.0% |
| add(1000) | 406.0 | 368.2 | −9.3% |
| **geomean** | **304.2** | **282.7** | **−7.1%** |

¹ clear is bimodal (2× spread) — treat directionally.

The wins land exactly where the theory says: actions whose
handlers touch one field no longer pay a full reconcile of the
untouched list (`select`), and every action stops paying blanket
cache loss. `runLots` is flat — fingerprinting a 10k-row Vec
costs ~the noise floor (serde-to-hasher, no allocation).

## 5. W0 — correctness machinery

Landed with W2 (`pocopine-core/tests/reactive.rs`): changed-only
triggering, no-op handlers trigger nothing, unknown fingerprints
fall back to full triggering, `Handle::update` parity, cache
survival for unchanged fields. The 153-test browser corpus
(template_plan 35 + pine 118) passes unchanged on the new path.

All landed:

- **Differential fuzz** (`differential_fuzz_fine_grained_matches_oracle`):
  5 deterministic seeds × 200 random mutations through every
  write path (handler invoke, `Handle::update`, proxy set-trap),
  oracle-checked after every flush, including the precision probe
  (a mutate-and-restore handler must re-run zero effects).
- **Symmetry gates** (`keyed_fast_paths_match_list_oracle`):
  mutation shapes selecting each pp-for fast path (append,
  prepend, single-remove, two-swap) plus general-path shapes
  (reorder+insert+remove, relabel-in-place, clear, cold rebuild),
  rendered row order asserted against the list after every step.

This satisfies the gate W3c and W4 require.

## 6. W5 — interning: tried, measured, reverted

`wasm-bindgen`'s `enable-interning` + `intern()` on all
compiler-known plan vocabulary (binding attr names, event names,
child tags, row-plan vocabulary) was landed and benchmarked:

| action | main | W2+intern | W2 only |
|---|---|---|---|
| runLots(10000) | 1261.6 | **1351.6** | 1249.4 |
| geomean | 304.2 | 293.7 | 282.7 |

The feature adds a cache check to **every** Rust→JS string
crossing; 10k dynamic row labels pay it per create while the
interned static names are short and infrequent. Net loss —
reverted (`b97860d3`), with a do-not-re-enable note in
`pocopine-core/Cargo.toml`. Lesson recorded: boundary-hygiene
advice from JS-framework contexts must re-prove itself under
*this* workload's string mix. Revisit only alongside W4, where
batching changes the string-crossing profile entirely.

## 7. Future workstreams

- **W1 — typed field access.** Generalize `FastExpr`'s
  `FastPathRoot` with a `ComponentField` root; `#[component]`
  emits typed accessors so compiled bindings read fields straight
  from the `RefCell` — no serde, no Reflect, no proxy. The
  proxy's `get` trap delegates to the same accessors. Kills cost
  #1 and most of #3 with zero user-facing change. Acceptance:
  `update every 10th` / `select` deltas vs vanilla shrink.
- **W3 — signals as the engine.** Phased:
  - **3a (landed)** — one dependency graph: `(scope, key)` interns
    a `SignalId` on first track; fields subscribe/trigger/tear
    down through `SIGNAL_DEPS` exactly like `Signal<T>`/
    `Computed`. String hashed once per track/trigger; the
    string-keyed `DEPS`/`REVERSE` tables are gone.
  - **3b (landed)** — plan-gated lazy proxy minting: the macro
    emits `needs_proxy` per plan; bindings/interps/refs-only,
    `$`-free plans skip `into_proxy` at mount (2 trap Closures +
    1 Proxy + 2 Objects saved per instance) and lazy-mint via
    `scope_of_element` on first dynamic need. v1 eligibility is
    conservative — extending it to listener-carrying plans
    (dispatch-time lazy resolve) is the 3b follow-on.
  - **3c** — signal-backed field VALUES: the field cache becomes
    per-field signal storage with a Rust-side write path,
    removing serde from the change path entirely. Needs W0's
    differential harness in place first.
- **W4 — batched mutation channel.** LANDED (`mutation_channel.rs`)
  as a *descriptor* variant rather than the byte-buffer sketch:
  because the items array already lives JS-side and the per-plan
  op sequence is static, registering the plan once as a JS
  descriptor and making ONE rich call per batch carries
  everything an op stream would — with no encoder/decoder pair
  to drift (the original buffer condition is thereby satisfied
  vacuously). The interpreter clones the prototype, stamps scope
  ids, evaluates item-rooted Text/Class bindings reading
  `items[i]` natively, and returns all node handles in one flat
  array; bookkeeping (RowInstance, list watcher, key dedup,
  scope minting) stays Rust-side. Scope: cold mounts + append
  fast paths on keyed, row-plan, proxy-elided sites. Runtime
  toggle keeps both modes in one binary; the W0 differential
  gate (`channel_and_direct_keyed_mounts_match` + the keyed fuzz
  oracle channel-on) asserts byte-identical DOM. Measured:
  runLots(10000) −8.5/−8.9% across reversed back-to-back pairs,
  add −4/−8%, run −5/−6.5%, others flat; runLots gap to vanilla
  1.41× → 1.29×. Acceptance ("close on vanilla") partially met —
  the remaining ~170 ms is items-projection serde, fingerprint
  sweep of the 10K-row write, per-row enter transitions, and
  handle extraction; each is a separate follow-on lever. Bundle:
  +9.6 KB wasm + 3.5 KB JS snippet.

## 8. Non-goals

1. **Changing authoring ergonomics.** `self.x = 1` in handlers
   and `a.x = 1` in templates are the contract. Any workstream
   that would force setters or signal types into user structs is
   out of scope by definition.
2. **Removing the proxy entirely.** It stays as the dynamic
   surface (devtools, magics composition, assignment
   expressions) — it just stops being load-bearing for hot-path
   reads and writes.
3. **A second reactive engine.** Signals/Computed and the proxy
   share one effect engine today and must continue to — Vue
   Vapor's parity-by-shared-engine is the model.
