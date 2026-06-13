# RFC 101 - Per-row mount allocation hardening

| Field | Value |
|---|---|
| **Status** | Draft |
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

### P2 — SoA the `ChannelRow` build

Each `ChannelRow` carries `binding_nodes: Box<[Element]>` +
`listener_nodes: Vec<Element>` — two heap blocks per row. But the
channel's `out` array is **already columnar** (`out.get(row*stride+j)`).
So a row can hold a `(base, stride)` offset into shared per-list buffers
instead of N per-row boxes, collapsing 2 allocs/row to a handful per
*list*. Resident-memory win too (these live for the row's lifetime).
Medium risk (touches the channel mount + `RowInstance` access).

### P3 — Typed key

`stringify_key` allocates a `String` + `Rc<str>` **per row** even when
`pp-key="item.id"` is a `usize`. A typed key —
`enum RowKey { Int(u64), Str(Rc<str>) }`, `Hash + Eq + Clone` — skips
both for numeric keys. **~2 allocs/row → ~0 for numeric keys.** This is
where the *typed-field-access* capability (RFC-096 `field_as_text`,
RFC-097's per-field codegen) actually pays — not in marshalling. Higher
effort: `RowKey` threads through the keyed-diff machinery (`PrevItem`,
the pool, the `seen` set, dedup — ~80 sites in `for_.rs`); gated on the
keyed-symmetry battery + the differential fuzz.

### P4 — Per-row scope-mint reduction (open / hard)

`Scope::new` + `LoopScope` `Rc` + `set_parent` is 2 allocs/row of
*per-row reactive identity*. Reducing it means pooling/reusing scopes or
a lighter `LoopScope` representation — the deepest change, and **not
larger than the others**, so it is last. Likely its own follow-up RFC;
listed here for completeness of the attribution.

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

## 6. Phasing

| phase | change | allocs/row | risk |
|---|---|---|---|
| P1 | `RowInstance` SmallVec | ~2 → 0 | low |
| P2 | `ChannelRow` SoA (offsets into the columnar `out`) | ~2 → ~0 | medium |
| P3 | typed `RowKey` | ~2 → ~0 (numeric) | medium (keyed-diff surface) |
| P4 | per-row scope-mint reduction | ~2 → ? | high / open |

Each phase lands independently, gated on the harness before/after + the
full battery + an A/B. P1 first (lowest risk, validates the loop).

## 7. Relation to RFC-097

RFC-097 (field handles) is a **different axis** — per-field stream
updates + `&self`-skips-the-sweep — and does not touch the per-row mount
path. The connection is the **codegen rail**: RFC-097 specifies
emitting per-field *typed* accessors on a generated trait (the RFC-081
precedent); P3's typed key and any future typed row-field access reuse
that machinery. The two RFCs are complementary, not overlapping; neither
blocks the other.
