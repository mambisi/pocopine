# RFC 101 - Per-row mount allocation hardening

| Field | Value |
|---|---|
| **Status** | Implemented (P1–P4), measured |
| **Author** | pocopine team |
| **Created** | 2026-06-13 |
| **Related** | [`rfc-054`] (the compiled-row mutation channel being measured), [`rfc-096-signals-first-reactive-core.md`](./rfc-096-signals-first-reactive-core.md) (typed text lane), [`rfc-097-field-handles.md`](./rfc-097-field-handles.md) (per-field-typed codegen — the rail the typed key reuses), [`rfc-098-core-hardening.md`](./rfc-098-core-hardening.md) (the prior reactive-core hardening pass) |

## 1. Summary

Mounting a `pp-for` list costs **~10 Rust heap allocations per row**
(runLots(10000) ≈ 106K allocations). This RFC reduces that — judged by
**allocation count** (deterministic, measured), with wall-clock required
only to stay **neutral** and correctness gated on the W0 battery +
keyed-symmetry. It is a follow-on to RFC-098 (which hardened the
reactive *core*); this hardens the per-row *mount path*.

**Outcome (P1–P4 shipped).** The four per-row buckets were removed in
sequence, each measured before/after on the same harness:
`runLots(10000)` total Rust allocations **118134 → 41133** (the
framework's channel-mount cost dropped from **~10/row to 1/row** — the
single remaining alloc is the genuine `LoopScope` reactive identity).
Wall-clock stayed neutral (release A/B: mean 426.3 ms vs baseline
427.6 ms, inside a ±50 ms noise band), wasm grew **+4.4 KB**, and every
phase held the full reactive + keyed-symmetry batteries green.

The headline of this RFC is the **measurement, not a hunch**. An
allocation-attribution harness (a counting `#[global_allocator]` read at
mount-phase boundaries) showed the per-row cost is **not** what we
assumed — it is four even ~2-alloc/row buckets, and the thing we
*expected* to dominate (serde marshalling) allocates ~0 Rust heap.

## 2. The measured attribution (evidence)

Per row, mutation channel on (production), `run(1000)` and
`runLots(10000)` identical:

| bucket | allocs/row | what it is | this RFC |
|---|---:|---|---|
| mount stamping (DOM / binding-apply / node-path / listener) | **~0** | JS-side; `set_inner_html`/`Reflect` create JS objects, not Rust heap | — (already lean) |
| **scope mint** (`Scope::new` + `LoopScope` `Rc` + `set_parent`) | **2.0** | per-row reactive identity | P4 (hard floor) |
| **`ChannelRow` build** (`binding_nodes: Box<[Element]>` + `listener_nodes: Vec`) | **2.0** | per-row DOM-handle vecs | P2 |
| **`RowInstance`** (`binding_cache: Vec` + `listener_routes: Box<[…]>`) | **2.0** | per-row instance vecs | **P1** |
| **keying** (`stringify_key` → `String` → `Rc<str>`) | **2.0** | per-row key, even for a numeric `pp-key` | P3 |
| other (harness `format!` ~1 + field read + misc) | ~3 | bench-side + plumbing | — |
| **total** | **~11** | | |

**Two findings that redirected the whole effort:**

1. **serde marshalling is not a Rust allocator.** `serde_wasm_bindgen::to_value(&Vec<Row>)` for 10000 rows = **1** Rust allocation (it builds JS-side objects, counted by V8). The "typed columnar marshalling" lane we scoped to cut it would not move the Rust alloc count — it is a JS-GC/wall-clock play only, deferred until a wall-clock A/B justifies it. (See §6 non-goals.)
2. **There is no single dominant allocator** — it is 4 × ~2/row across four distinct per-row structures. Reduction is therefore a set of independent, individually-measurable changes, not one refactor.

## 3. The optimizations

### P1 — Inline the `RowInstance` per-row vecs (SmallVec)

`RowInstance` holds `binding_cache: vec![None; plan.bindings.len()]`
and `listener_routes: Box<[RowListenerRoute]>` — two heap blocks per
row, both small (jsbench rows: 3 bindings, ≤2 listeners). Store them in
`SmallVec<[_; N]>` so the common small case lives inline in the
instance (which already lives in the `ROW_INSTANCES` map), heap-spilling
only on large rows. **~2 allocs/row → ~0.** Contained to `RowInstance` +
its construction/access sites; the API (`binding_cache[j]`, iterate
`listener_routes`) is unchanged (SmallVec is Deref-compatible). Lowest
risk; ships first as the template for the harness-driven before/after.

### P2 — Inline the `ChannelRow` node vecs (SmallVec) ✅

Each `ChannelRow` carries `binding_nodes: Box<[Element]>` (resident in
`RowInstance`) + `listener_nodes: Vec<Element>` (transient, consumed
building `listener_routes`) — two heap blocks per row. **Shipped as the
SmallVec completion of P1** (`SmallVec<[Element; 4]>` / `<[Element; 2]>`,
inline for the common 3-binding/2-listener row). **~2 allocs/row → ~0**
(measured: ChannelRow bucket 20006 → 6 on runLots).

*Rejected the full columnar SoA* (a `(base, stride)` offset into shared
per-list buffers, what this section originally proposed): it reintroduces
an index-invalidation problem the keyed swap/remove reconcile would have
to maintain (tombstones / offset shifts on row removal), for no resident
win over inline storage. Inline `SmallVec` captures the same allocation
*and* resident win with no offset bookkeeping. Indexing is unchanged
(`SmallVec` Derefs to a slice).

### P3 — Typed key ✅

`stringify_key` allocated a `String` + `Rc<str>` **per row** even when
`pp-key="item.id"` is a `usize`. Shipped `enum RowKey { Int(i64), Str(Rc<str>) }`
(`Hash + Eq + Clone`): an integral, exact-range JS number takes the
zero-alloc `Int` path; everything else reuses `stringify_key` verbatim
for a byte-identical `Str` key. **~2 allocs/row → ~0 for numeric keys**
(measured: keying bucket 20001 → 1 on runLots; string-key lists keep
their one `Rc<str>` alloc, as expected). `i64` (not `u64`) so negative
ids round-trip; gated to the f64 exact-integer range (2⁵³) so large ids
fall back to `Str` rather than aliasing.

Derived `Eq`/`Hash` discriminate on the variant, so `Int(5)` and
`Str("5")` never collide — which also **fixes a latent aliasing bug** the
all-`String` form had. `RowKey` threads through the keyed-diff machinery
(`PrevItem`, the pool `HashMap<RowKey,_>`, the `seen` `HashSet<RowKey>`,
the two flip-snapshot maps, dedup — **53 sites**, all private to
`for_.rs`). The one genuinely-risky edit: `retract_from_prior` switches
`Rc::ptr_eq` → value `==` (an `Int` has no pointer identity) — strictly
more correct, on the cold leave path only. The 5 near-identical
construction sites collapsed into one `dedup_row_key` helper.

### P4 — Erase scope state without the double-Rc box ✅

`Scope::new` is 2 allocs/row of per-row reactive identity:
`Rc::new(RefCell::new(LoopScope{…}))` (the genuine identity) **and**
`Rc::new(state)` — which boxed the *already-`Rc`* state inside a **second
`Rc`** purely to satisfy the `Rc<dyn Any>` erasure. The second box is
removable with **zero plumbing**: erase `state` directly to `Rc<dyn Any>`
via an unsizing coercion (a fat-pointer rebind, no heap block) and
recover it in `typed::<T>()` with `Rc::downcast::<RefCell<T>>` instead of
`downcast_ref::<Rc<…>>`. Identical contract; every consumer goes through
the `typed()` method (no direct field access anywhere), so it is fully
encapsulated and benefits **all** scopes, not just rows. **~2 → ~1
alloc/row** (measured: scope-mint bucket 20009 → 10009).

*Rejected scope pooling* (this section's original "reuse scopes" idea):
reusing a `ScopeId` across reconciles is a use-after-logical-free —
`FIELD_SIGNALS` / `SIGNAL_DEPS` / `PROJECTIONS` / `PARENTS` /
`ROW_INSTANCES` are all keyed by a bare, non-generational `u64`, and an
in-flight leave callback / queued effect / delegated listener can still
hold an `Rc` clone of the recycled `LoopScope`. The remaining 1 alloc/row
(the `LoopScope` `Rc` itself) is therefore left in place by design.

## 4. Non-goals (binding)

1. **Typed columnar marshalling for Rust allocs.** Refuted by
   measurement (serde → JS is ~0 Rust alloc). Off the table *as an
   allocation lever*; revisit only as a wall-clock/JS-GC change with a
   timing A/B that justifies rewriting the channel interpreter.
2. **Mount stamping.** Already ~0 Rust alloc/row (RFC-054). Untouched.
3. **Wall-clock objectives.** Like RFC-098, the gate is allocation-count
   reduction + wall-clock *neutrality*; a per-alloc cut that regresses
   dispatch latency is rejected.
4. **Authoring surface.** Purely internal; `pp-for`/`pp-key` semantics
   unchanged.

## 5. Verification

- **Allocation-attribution harness** (the deterministic gate): a
  counting `#[global_allocator]` in the jsbench harness feeds
  `alloc_profile`, read at mount-phase boundaries; the driver reports
  per-bucket allocs/row for `run`/`runLots`/`select`/`update`/`add`
  before and after each phase. P1 must drop the `RowInstance` bucket
  from ~2.0 to ~0/row with the others unchanged.
- **Correctness:** the W0 reactive battery (`wasm-pack test --node
  crates/pocopine-core`) + the keyed-symmetry/`template_plan` firefox
  battery, green at every phase. P2/P3 especially must pass keyed
  reconcile (swap/remove/reorder) + the differential fuzz.
- **Neutrality:** a chromium A/B (`select`/`update`/`runLots`/`clear`)
  within ±2% geomean, reversed double-run (the RFC-098 protocol).

## 6. Phasing (measured)

Each phase landed independently, gated on the harness before/after + the
full battery. `runLots(10000)` total Rust allocations, measured on the
attribution branch after each cherry-pick:

| phase | change | bucket allocs/row | runLots total | risk | status |
|---|---|---|---:|---|---|
| — | baseline (`main`) | — | 118134 | — | — |
| P1 | `RowInstance` SmallVec (`binding_cache` + `listener_routes`) | 2.0 → 0 | 96134 | low | ✅ |
| P2 | `ChannelRow` SmallVec (`binding_nodes` + `listener_nodes`) | 2.0 → 0 | 74134 | low | ✅ |
| P3 | typed `RowKey` (`Int` no-alloc for numeric keys) | 2.0 → 0 | 52134 | medium | ✅ |
| P4 | scope state erased without the double-`Rc` box | 2.0 → 1.0 | 41133 | medium | ✅ |

The framework's per-row channel-mount allocation went **~10/row → 1/row**
(the surviving alloc is the `LoopScope` `Rc`, kept by design — see P4).
Full-stack release wall-clock A/B: mean **426.3 ms** vs baseline
**427.6 ms** on `runLots(10000) ×25` — neutral within the metric's
±50 ms noise floor (the ~100K-alloc reduction is sub-millisecond and
below resolution; the A/B's role is to rule out a regression). wasm
**+4.4 KB**. Reactive battery 30/30 + 9/9 and the firefox
keyed-symmetry battery (`template_plan` 43/43, plus `refs_typed`,
`component_refs`, `typed_slot_props` for P4's `typed()` change) green at
every phase.

## 7. Relation to RFC-097

RFC-097 (field handles) is a **different axis** — per-field stream
updates + `&self`-skips-the-sweep — and does not touch the per-row mount
path. The connection is the **codegen rail**: RFC-097 specifies
emitting per-field *typed* accessors on a generated trait (the RFC-081
precedent); P3's typed key and any future typed row-field access reuse
that machinery. The two RFCs are complementary, not overlapping; neither
blocks the other.
