# RFC 096 - Signals-first reactive core: the bold switch

| Field | Value |
|---|---|
| **Status** | IMPLEMENTED on `perf-reactive-dirty-tracking`: S1 write mirror; S2 readers everywhere ($store/$route de-proxied, LoopScope/SlotScope re-chained by id, lazy listeners, eligibility = everything but structural/child/slot/opaque); S3 versioned projections (FIELD_CACHE/FRESH_FIELDS deleted) + typed pp-text lane + serde counter; S4 js_bridge + pp-as gating + mint counter (devtools was already direct). S5 profile-gated decision: NOT adopted — the dependency graph measures 0.0 ms on the heaviest action. Remaining tail: slot-fragment/structural-controller proxy threading (their plans keep the eager mint). |
| **Author** | pocopine team |
| **Created** | 2026-06-10 |
| **Related** | [`rfc-095-reactive-core-de-alpine.md`](./rfc-095-reactive-core-de-alpine.md) (subsumes its W3c into a full design), [`rfc-054`](./rfc-054-compiled-row-plans.md), [`rfc-058-compiled-views-walker-removal.md`](./rfc-058-compiled-views-walker-removal.md) |
| **Supersedes** | RFC-095 §7 "W3c" (expanded here) |

## 1. Summary

Make signals the **only** reactive engine. After this RFC, no
framework feature is implemented on the JS `Proxy`: reads resolve
through scoped readers (landed, RFC-095 W1), writes resolve
through a scoped **writer** (the set trap's body as a plain Rust
function), field values live **on their interned signals** (the
W3a `SignalId` gains a value slot), and serde crosses the
wasm↔JS boundary only where a genuine JS value is required. The
proxy survives as exactly one thing: an **explicit, opt-in
interop shim** (`js_bridge`) for foreign JS that insists on
touching state directly — minted on request, never on the
framework's behalf.

The bold part is what changes. The boring part — deliberately —
is what doesn't:

```rust
// user code, before and after this RFC — byte-identical
pub fn bump(&mut self) { self.count += 1; }
```
```html
<!-- templates, before and after — byte-identical -->
<button @click="open = !open">toggle</button>
<p pp-text="$store.theme.name"></p>
```

The `.poco` language is frozen; this RFC swaps its interpreter.

## 2. Motivation — what the data already proved

The `perf-reactive-dirty-tracking` branch ran the experiment that
justifies the switch, one workstream at a time, each measured:

| step | what moved off the proxy | result |
|---|---|---|
| W2 | change detection (fingerprints, per-field triggers) | −4 to −7% geomean back-to-back; select/remove −9% in the W2 pair; mutation ops now at vanilla parity (swap 164 vs 160, remove 203 vs 202) |
| W1 | read-path root resolution (scoped readers) | bench-neutral on the list workload by design; removed a wasm→JS→wasm trap bounce from every binding re-run |
| W3a | the dependency graph (fields interned as signals; string tables deleted) | one u64-keyed graph; `track` de-stringed |
| W3b | mount-time minting (plan-gated elision) | 2 trap `Closure`s + `Proxy` + 2 `Object`s saved per eligible instance |

External corroboration (sources in RFC-095): V8's own data shows
trapped access defeats inline caches (~5× a plain read even
after optimization); Vue rebuilt `@vue/reactivity` on
alien-signals (plain objects + linked lists) whose benchmark
entry runs at **1.04× vanilla**, while Alpine — the
proxy-does-everything ancestor — measures **3.53×**. Every
framework that reached near-vanilla moved the proxy off the hot
path; none kept it as the engine.

What remains proxy-implemented after the branch, and therefore
what this RFC must rebuild:

1. **Template assignments** — `open = !open` is the set trap.
2. **Cross-boundary writes** — parent `pp-bind` prop writes and
   `pp-model` mirror-in do `Reflect::set(child_proxy, …)`;
   native `pp-model`'s write side uses `write_path` through the
   trap.
3. **`$store` / `$route` reads** — resolve to proxy objects.
4. **`LoopScope` / `SlotScope` fall-through** — derived scopes
   read parent fields through the parent proxy inside
   `state.get`.
5. **Field-value storage** — `FIELD_CACHE` holds serde-produced
   `JsValue`s per observed field; a changed field pays a full
   re-serialization on next read.
6. **Devtools browsing** and **undeclared JS interop**.

Items 1–5 are framework-internal and rebuildable on signals.
Item 6 splits: devtools reads Rust state directly; undeclared
interop becomes the explicit shim.

## 3. Goals

1. **No framework feature requires the proxy.** Every read,
   write, and subscription in the runtime goes through the
   signal engine. The proxy is never minted unless user code
   calls `js_bridge`.
2. **Serde exits the change path.** A field mutation updates its
   signal Rust-side; serialization happens only at
   *projection* — when a JS value is genuinely required (compound
   values crossing to the DOM, event details, client modules,
   devtools snapshots, `js_bridge`) — and is cached on the
   signal until the next change.
3. **Scalar fast lane.** For string/number/bool fields, the
   macro emits typed accessors so `pp-text="count"` goes
   `u32 → itoa-style format → set_text_content` — zero serde,
   zero `JsValue` intermediary, one bridge crossing.
4. **Authoring ergonomics are untouchable** (inherited from
   RFC-095 and re-asserted): plain `&mut self` field mutation,
   template assignment expressions, `$store`/`$route`/`$event`
   magics — all byte-identical for users.
5. **Every phase gated by the W0 harness**, extended to dual-run
   old-vs-new implementations before each cutover.

## 4. Non-goals

1. **`Signal<T>` fields in user structs.** The struct stays the
   source of truth and stays plain Rust — that is what keeps
   `self.count += 1` working. "Signals-first" means everything
   *downstream* of the struct is signals; it does not mean
   Leptos-style declared signals. (Rejected in §8.1.)
2. **Template syntax changes.** Frozen, including `$`-magics.
3. **Touching the client-module bridge.** It traffics in serde
   values and `ScopeId`s; it was never on the proxy and needs
   nothing from this RFC.
4. **The batched mutation channel (W4).** Separate RFC; this RFC
   makes W4 *easier* (typed Rust values at the DOM boundary are
   exactly what a byte-encoded op stream wants) but does not
   include it.
5. **Adopting alien-signals' propagation algorithm wholesale.**
   Its push-pull versioned graph is staged as an optional final
   phase (§6 S5), profile-gated — our HashMap subscriber sets
   have not yet appeared on a profile.

## 5. Design

### 5.1 Target architecture

```
                        ┌─ the ONE engine ─────────────────────────────┐
 user struct            │  field signal (per (scope, key), W3a id):    │
 (source of truth) ──►  │    subscribers: HashSet<EffectId>            │
 plain Rust fields      │    version:     u32                          │
        │               │    projection:  Option<JsValue>  (lazy)      │
        │               │    (scalar kinds: typed read, no projection) │
        ▼               └──────────────────────────────────────────────┘
 writes converge:                    reads converge:
   handler &mut self ─► dirty sweep    scoped reader (W1) ─► typed value
   `open = !open`    ─► scoped WRITER  or cached projection
   pp-bind/pp-model  ─► scoped WRITER
        │                            │
        ▼                            ▼
   signal.version += 1          DOM effect writes
   trigger(subscribers)         (string/number direct;
                                 compound via projection)

 proxy: NOT in this picture. `js_bridge(scope_id)` mints one on
 explicit request; its traps delegate to the reader/writer above.
```

### 5.2 Field signals gain values

W3a interned `(scope, key) → SignalId` for the *graph*. This RFC
gives the signal a body:

```rust
struct FieldSignal {
    subscribers: HashSet<EffectId>,   // exists today (SIGNAL_DEPS)
    version: u32,                     // bumped on every confirmed change
    projection: Option<JsValue>,      // serde output, lazily built,
                                      // invalidated by version bump
}
```

`FIELD_CACHE` is **deleted** — its job (cache the serde output of
an observed field) moves onto the signal, where invalidation is
a version bump instead of a map removal, and where the `patch_*`
APIs patch `projection` in place exactly as they patch the cache
today. `FRESH_FIELDS` dies with it (the sweep already consumes
it; with versioned projections the "survive the invalidate" dance
is unnecessary — a patched projection simply carries the new
version).

The dirty sweep (W2) remains the bridge from opaque `&mut self`
mutation to the signal world, unchanged in shape: fingerprint
observed fields around the handler, and for changed fields bump
`version`, drop/patch `projection`, trigger subscribers.

### 5.3 Scalar fast lane — typed accessors

The macro knows every field's type. For the kinds that dominate
templates it emits typed readers alongside `field_fingerprint`:

```rust
// emitted per component, same arm pattern as field_fingerprint
fn field_as_text(&self, key: &str) -> Option<Cow<'_, str>>;   // String + Display kinds
fn field_as_f64(&self, key: &str) -> Option<f64>;             // numeric kinds
fn field_as_bool(&self, key: &str) -> Option<bool>;
```

`pp-text` / `pp-show` / scalar `pp-bind` effects consult these
first: `count: u32` renders via `format → set_text_content` with
no serde and no `JsValue`. Compound fields (`Vec`, structs,
enums) return `None` and take the projection path (§5.2). The
`StaticExpr`/`evaluate_with` machinery gains a typed evaluation
mode for the subset it already special-cases (`FastExpr` proved
this shape for rows — this is its generalization to component
fields, completing what W1 started).

### 5.4 The write mirror — scoped root writer

The read side has `read_field_tracked` (extracted from the get
trap, W1). The write side gets its twin, extracted from the set
trap:

```rust
/// The set trap's body as a plain function — the ONE write path.
pub fn write_field_tracked(scope_id: ScopeId, key: &str, value: JsValue) {
    // state.set (serde in — unchanged), flatten-container resolve,
    // signal version bump + projection invalidate (was: cache remove),
    // model-runtime origin bookkeeping, trigger(key) [+ container]
}
```

Consumers, each currently routing through the trap:

- **`Expr::Assign`** compiles to `write_field_tracked` for
  single-segment paths; dotted paths get a Rust-side
  `write_path_tracked` (read the penultimate projection, set the
  leaf, re-fingerprint the root field, bump + trigger — the
  RFC-024 §7 semantics preserved exactly).
  *Update (2026-07-02):* as shipped, the dotted path only set the
  leaf on the projection — no bump, no trigger, and (post-S3) no
  way back into Rust state, so deep writes were silently lost.
  `path::write_segments_with` now completes this bullet in the
  stronger form the projection model requires: after the leaf
  set it writes the whole root field back through the scoped
  writer. See the RFC-024 §7 update note.
- **`pp-bind` child-prop writes** and **`pp-model` mirror-in**
  call it with the child's `scope_id` (both already resolve the
  child scope; the `is_prop` gate and `WriteOrigin` plumbing move
  inside unchanged).
- **Native `pp-model` write side** (input events) calls it
  instead of `write_path` through the proxy.

The set trap itself — for as long as any proxy exists — delegates
to this function, so the two paths cannot diverge (the same
single-implementation rule the get trap follows since W1).

### 5.5 `$store` / `$route` on readers

Stores are `ComponentState` with their own scopes. `$store.x.y`
resolution moves from "build a JS object of store proxies, walk
it with `Reflect`" to: resolve the store's `ScopeId` by name,
read through its scoped reader, walk the remaining segments on
the projection. `$route` identically against the router scope.
The magics resolver keeps its API; only its implementation stops
minting proxies. (`$event` is a thread-local; untouched.)

### 5.6 Derived scopes re-chain

`LoopScope.parent: JsValue` (the parent proxy, for row exprs
reading parent fields) becomes `parent: ScopeId` + the parent's
reader; `SlotScope`'s compound-ctx composition likewise reads
parent fields through readers instead of `Reflect::get` on the
parent proxy. These are the last *internal* proxy consumers; with
them gone, `pp-for` and slot machinery stop forcing
`needs_proxy = true` (W3b's eligibility widens to most of pine).

### 5.7 Listeners go lazy, eligibility goes wide

Listener installs already receive `scope_id`. They stop capturing
the proxy and evaluate dispatch-time expressions against
reader/writer — `Call` via `invoke_handler(scope_id, …)`
(already proxy-free), `Assign` via the write mirror, reads via
the reader. With listeners, structural controllers (§5.6), and
models (§5.4) off the proxy, **`needs_proxy` is `false` for
every plan** — W3b's gate becomes vestigial and mount never
minted anything to skip.

### 5.8 The proxy endgame — `js_bridge`

```rust
/// Explicit interop shim: mint (once, cached) a JS Proxy whose
/// traps delegate to read_field_tracked / write_field_tracked.
/// For foreign JS that must touch component state directly.
/// Everything pocopine ships works without it.
pub fn js_bridge(scope_id: ScopeId) -> JsValue;
```

`scope_of_element` / `enclosing_scope` stop lazy-minting (their
callers are migrated in S2/S3); devtools reads `state.get` /
`field_as_*` directly. `into_proxy` becomes the private backer
of `js_bridge` alone. The mount path contains zero proxy code.

### 5.9 What each existing piece becomes

| piece | today | after |
|---|---|---|
| `read_field_tracked` | track + cache + serde get | track + version-checked projection / typed read |
| set trap | the write path | delegate to `write_field_tracked`; only inside `js_bridge` |
| `FIELD_CACHE` / `FRESH_FIELDS` | side tables | deleted — folded into signal `projection` + `version` |
| `DirtySweep` | fingerprint diff → cache invalidate + trigger | fingerprint diff → version bump + trigger (same shape) |
| `patch_*_inline` | patch cache + fresh mark | patch projection in place (same API, same purpose) |
| `scoped_static_evaluator` | reader fallback to proxy | reader + typed lane; proxy arg deleted from signatures |
| `needs_proxy` (W3b) | plan gate | vestigial → removed once all plans qualify |
| magics `$store`/`$route` | proxy objects | reader-backed resolution |
| devtools | proxy reads | direct state reads |

## 6. Phasing — each gated, each measured

Every phase lands behind the W0 harness **extended first**: the
differential fuzz gains a dual-run mode (old implementation and
new implementation of the phase's surface execute side by side;
DOM and observed values diffed per flush) *before* the cutover
commit, then the old path is deleted in the same phase.

- **S1 — the write mirror (LANDED).** `write_field_tracked` +
  `write_path_with`; Assign compilation; pp-bind/pp-model writes
  (by child scope id — no proxy forced onto elided children);
  set trap delegates. *Gate met:* fuzz extended with proxy-less
  assignment + prop-write ops, 1000 oracle-checked mutations.
  *Acceptance met:* all 331 tests green; jsbench 289.7, inside
  the branch noise band.
- **S2 — readers everywhere.** `$store`/`$route` resolution;
  `LoopScope`/`SlotScope` re-chaining; listeners lazy. *Gate:*
  pine suite (the compound corpus) + store/router test suites.
  *Acceptance:* `needs_proxy = false` for ≥90% of pine plans
  (measured by a build-time count); mount-N-primitives fixture
  (new) shows the per-instance saving at scale.
- **S3 — values onto signals.** `FieldSignal` storage; delete
  `FIELD_CACHE`/`FRESH_FIELDS`; typed accessors + scalar lane.
  *Gate:* dual-run differential (projection path vs old cache
  path) across the fuzz corpus; representation-drift tests
  (serde attrs, `None` canonicalization, float formatting —
  typed lane output must equal `js_to_string(serde(value))` for
  every covered kind). *Acceptance:* `update every 10th` and
  `select` close further on vanilla; zero serde calls on the
  scalar-field change path (assert via a debug counter).
- **S4 — proxy endgame.** `js_bridge`; devtools direct reads;
  lazy-mint sites removed; mount proxy-free; trap machinery
  reachable only via `js_bridge`. *Acceptance:* wasm size delta
  (trap plumbing leaves the default path); a grep gate — no
  `into_proxy` caller outside `js_bridge`.
- **S5 (DECIDED: not adopted) — alien-signals algorithm.**
  The gate was run on the final S4 state: the mount profiler's
  `state_sync` buckets (effect closure + invalidation + trigger
  dispatch — i.e. the entire dependency-graph cost) measure
  **0.0 ms** on `runLots(10000)` (action total 559 ms; the real
  costs are `reconcile_reorder` 178 ms and per-row mount DOM
  work — bridge territory, i.e. W4). Adopting the push-pull
  linked-list graph would optimize a line item that does not
  register on our profile. Decision recorded per the rule that
  killed W5: algorithms are adopted on OUR numbers, not admired
  benchmarks. Revisit only if a future profile shows the graph.

W4 (mutation channel) slots after S3 at the earliest — it wants
S3's typed values at the DOM boundary.

## 7. Compatibility

**User code: zero changes.** The complete audit (RFC-095 +
session survey):

| surface | impact |
|---|---|
| `&mut self` handlers, `Handle::update`, `patch_*` | none — same APIs, same semantics |
| `open = !open`, dotted writes, `$store`/`$route`/`$event` | none — same syntax, new interpreter |
| props, `pp-model` (both legs), emits, slots, refs, lifecycle | none — internal rewiring only |
| client-module bridge (`.client.ts`) | none — values + `ScopeId`s, never saw the proxy |
| pine-richtext-style imperative JS interop | none — handle-based by design |
| undeclared JS mutating state via a captured proxy | **the one break** — was never documented; remedy is `js_bridge` |
| devtools | reimplemented reads; same panels |

## 8. Alternatives considered

### 8.1 `Signal<T>` fields in user structs (the Leptos shape)

Rejected. It buys declared-dependency precision we already get
from fingerprints, at the cost of the project's single firmest
ergonomic commitment (`self.count += 1`, no setters) and a
migration across every app. The audit's "Option A, 6+ months,
breaks APIs" — the bold switch is bold about the *engine*, not
about user-facing types.

### 8.2 Delete the proxy outright, no shim

Rejected. The shim is ~nothing to keep (it reuses
reader/writer), and "foreign JS pokes state" is a real, if rare,
escape hatch — better explicit and cheap than impossible.

### 8.3 Keep the proxy for writes, signals for reads only

Rejected by symmetry and by data: the write path through the trap
pays the same closure bounce the read path did, and a single
write implementation (`write_field_tracked`) shared by trap and
compiler is the same cannot-diverge rule that made W1 safe.

### 8.4 Adopt alien-signals' core now

Deferred to S5. Adopting an algorithm because a benchmark admires
it elsewhere is how W5 (interning) happened; our graph hasn't
shown up on a profile. The harness makes a later swap cheap.

## 9. Risks

1. **Representation drift in the typed lane** (S3) — a typed
   accessor disagreeing with serde output in a corner (float
   formatting, `Option` canonicalization, field serde attrs).
   *Mitigation:* the equality property test in §6 S3 over every
   covered kind; compound kinds never take the typed lane.
2. **Write-mirror semantic gaps** (S1) — `WriteOrigin`
   bookkeeping, flatten containers, `#[watch]` double-fire rules
   live in the trap today. *Mitigation:* extraction (move the
   body, don't rewrite it), trap delegates to the same fn, fuzz
   ops cover assignment + prop-write + flatten-leaf writes.
3. **Derived-scope re-chaining** (S2) touches slot/loop scope
   composition, historically the subtlest code in the runtime.
   *Mitigation:* pine 118 + scoped-slot suites are the gate;
   dual-run mode for `SlotScope.get`.
4. **Devtools regressions** — quiet field signals already
   diverge from user signals; S3/S4 must keep the scope panel's
   data source explicit. *Mitigation:* devtools snapshot tests.
5. **Scope creep toward S5.** The phases are separable on
   purpose; each lands green or reverts with numbers — the
   branch has done this twice and the discipline holds.

## 10. Verification & measurement

- W0 fuzz extended per phase (new ops: assignments, prop writes,
  flatten-leaf writes, store writes); dual-run differential
  before every cutover; the 331-test corpus green per phase.
- Acceptance benchmarks per phase as in §6, measured
  back-to-back on the jsbench harness; the mount-N-primitives
  fixture added in S2 becomes part of the standing suite.
- Standing debug counters: serde-calls-on-change-path (S3 target
  0 for scalars), proxies-minted (S4 target 0 without
  `js_bridge`).
- Size: twiggy before/after S4 (trap machinery off the default
  path should finally pay back the +1KB the branch carries).
