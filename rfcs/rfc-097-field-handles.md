# RFC 097 - Field handles: typed per-field projections of component state

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-06-12 |
| **Related** | [`rfc-095-reactive-core-de-alpine.md`](./rfc-095-reactive-core-de-alpine.md) (dirty sweep, W2c touch-hints post-mortem), [`rfc-096-signals-first-reactive-core.md`](./rfc-096-signals-first-reactive-core.md) (write mirror, typed text lane), [`rfc-081`] (generated typed ref accessors — the codegen precedent), issue #198 (signals are not the authoring surface) |

## 1. Summary

Two small, compiler-held mutability declarations that close the last
ergonomic/performance gap in the signals-first core:

1. **`FieldHandle<T>`** — a macro-generated, field-typed projection
   of one component field, obtained **from a `Handle<T>`** (never
   from `self`): `this::<Uploader>().progress()` →
   `FieldHandle<f64>`. `set` writes exactly one field through the
   write mirror — one version bump, one trigger, **no dirty sweep**
   — making high-frequency single-field updates from async tasks
   (upload progress, websocket ticks, animation state) cost
   proportional to what they touch.
2. **`&self` handlers skip the sweep.** A handler declared with an
   immutable receiver provably cannot mutate component state; the
   `#[handlers]` dispatch arm omits the `DirtySweep` bracket
   entirely. To keep that sound, `#[component]` rejects
   interior-mutability field types (`Cell`/`RefCell`/`Mutex` outer
   constructors) with a compile error.

Both are instances of one principle, learned from the W2c touch-hints
revert: **mutability knowledge must come from something the compiler
enforces** — a receiver token, a named field projection — never from
analysis that can drift (method-name lists, body AST guessing).

The authoring contract is untouched: inside a handler, `self.x = 1`
remains the only way to write state. Field handles exist solely for
contexts `&mut self` cannot reach.

## 2. Motivation

### 2.1 The async single-field stream

The dirty sweep makes `&mut self` ergonomics safe by hashing observed
fields around every handler/`Handle::update` call and triggering only
what changed. Its cost is per-call × observed-state-size — invisible
for a click handler, real for a stream:

```rust
// today: each tick pays a FULL sweep of every observed field
me.update(|s| s.progress = pct);   // hashes `rows` (10K structs) too,
                                   // 60×/sec, to discover it didn't change
```

The closure *names nothing*; the runtime must measure everything. Yet
the caller knows exactly which field changes. RFC-095 W2c tried to
recover that knowledge by analyzing handler bodies and died on an
unmaintainable method-name heuristic. The principled fix is to let
the caller **declare** the field at an API seam:

```rust
let progress = this::<Uploader>().progress();   // FieldHandle<f64>
spawn_scoped(async move {
    while let Some(pct) = stream.next().await {
        progress.set(pct);          // write mirror → bump → trigger("progress")
    }                               // no hashing, no sweep, nothing else re-runs
});
```

### 2.2 The read-only handler

Symmetrically: emit-only, clipboard, navigation, and dispatch-only
handlers mutate nothing, yet today pay a full sweep to prove it.
Receiver mutability already states the fact:

```rust
pub fn copy_link(&self) { … }      // cannot mutate → no sweep emitted
pub fn select(&mut self, id: usize) { … }   // swept, as today
```

### 2.3 Why not signals / why not more `update` variants

- Issue #198's stance survives the signals-first inversion: signal
  handles are not an authoring surface. `FieldHandle` is therefore
  **not reachable from `self`** — `self.count` (field) and a
  hypothetical `self.count()` (handle) coexisting in handler bodies
  is a confusion factory and a second way to write state.
- `Handle::update` keeps its job: **atomic multi-field changes**
  (one sweep, one consistent trigger set). `FieldHandle` is for
  single-field streams. Each tool has exactly one use; neither
  replaces the other.

## 3. Design

### 3.1 `FieldHandle<T>`

```rust
pub struct FieldHandle<T> {
    scope_id: ScopeId,
    key: &'static str,          // macro-emitted field name
    _marker: PhantomData<fn() -> T>,
}
impl<T: Serialize + DeserializeOwned> FieldHandle<T> {
    pub fn get(&self) -> T;                 // tracked read (subscribes the
                                            // running effect, if any)
    pub fn set(&self, value: T);            // write mirror: serialize once,
                                            // version bump, trigger(key)
    pub fn update(&self, f: impl FnOnce(&mut T));  // read-modify-write of ONE field
    pub fn scope_id(&self) -> ScopeId;
}
impl<T> Clone for FieldHandle<T> { … }      // Copy-cheap; Send/Sync: no (wasm)
```

Semantics:

- `set`/`update` route through `write_field_tracked` — the same
  mirror templates and the proxy trap use — so flatten-container
  triggers, the typed text lane, and projection versioning all apply
  unchanged. Exactly one field triggers; **no `DirtySweep` runs**.
- `set` is unconditional (mirrors `self.x = v` semantics today). An
  equality guard is an open question (§7).
- Dead scope: mirror `Handle` semantics exactly — writes on a dead
  scope are silent no-ops, and `get` follows `Handle::with`'s
  existing dead-scope behavior. Field handles introduce **no new
  lifecycle policy**; whatever `Handle` does today is the contract.
- Stores: `store::<Prefs>().theme()` works identically —
  `Handle<T>` is the host either way.

### 3.2 Codegen

`#[component]` (and `#[store]`) emit one extension trait per type,
implemented for `Handle<T>`:

```rust
pub trait UploaderFields {
    fn progress(&self) -> FieldHandle<f64>;
    fn status(&self) -> FieldHandle<String>;
    …
}
impl UploaderFields for pocopine::Handle<Uploader> { … }
```

- One method per **declared serde-visible field** (skipping
  `#[serde(skip)]`). Props included — writing a prop locally is
  already possible via `update`; same rules apply.
- `#[computed]` fields get **read-only** handles (`get` only) in a
  later phase; v1 generates accessors for struct fields only.
- Bare-flatten leaves: **not** in v1 (the container field gets the
  handle; leaf-level handles are an open question, §7).
- **Reserved-name collisions:** a field named `update`, `with`,
  `scope_id`, or any inherent `Handle` method name would silently
  resolve to the inherent method at call sites. The macro emits a
  compile error naming the conflict and the rename expectation. The
  reserved list lives beside `Handle`'s impl with a test asserting
  it matches the inherent surface (so it cannot drift silently —
  the W2c lesson applied to ourselves).

### 3.3 `&self` handlers skip the sweep

In `#[handlers]`, the dispatch arm emission inspects
`method.sig.receiver()`:

- `&mut self` → today's swept invoke path, unchanged.
- `&self` → the arm calls the method **without** the
  `DirtySweep`/`apply` bracket and without any trigger. (The
  `model_runtime::with_scope_write` wrapper is also skipped — there
  is no write.)

Soundness requires that `&self` truly cannot mutate observed state.
The one Rust loophole is interior mutability, and `Cell<T>`/
`RefCell<T>` are `Serialize`. Therefore `#[component]` **rejects**
fields whose outer type constructor is `Cell`, `RefCell`, `Mutex`,
`RwLock`, or `UnsafeCell`, with a diagnostic pointing at the
value-semantics contract (the documented "component fields are plain
data" rule, now enforced). A type alias can still smuggle one past
syntactic detection; doing so is documented as forfeiting
reactivity guarantees — the same standing as `#[serde(skip)]`
fields being invisible to the sweep. This rejection ships with the
`&self` change (it is what makes it sound), but is desirable
independently.

### 3.4 Non-goals

- **No `get_untracked` / `peek`.** `Handle::with` is the canonical
  untracked read; field handles do not duplicate it.
- **No handler-side surface.** Nothing is generated on `self`;
  handler bodies are unchanged.
- **No per-index collection ops.** `rows().set(vec)` replaces the
  value; surgical list mutation remains the `patch_*_inline` family.
- **No `Signal<T>` unification.** `FieldHandle` is not a `Signal`;
  it does not enter the prelude's signal API or change issue #198's
  resolution.

## 4. Reactivity semantics (precise)

| op | tracks? | triggers | sweep | projection |
|---|---|---|---|---|
| `fh.get()` in effect | yes — interns + subscribes | — | — | tracked read path (typed text lane N/A — returns `T`) |
| `fh.get()` outside effect | no-op track | — | — | read mirror |
| `fh.set(v)` | — | exactly `key` (+ flatten container if leaf) | **none** | version bump; rebuilt on next read |
| `fh.update(f)` | — | exactly `key` | **none** | as `set` |
| `&self` handler | — | **nothing** | **none** | untouched |

Interaction with the mutation channel, list watchers, and `pp-for`
reconciliation: none beyond a normal single-field trigger — `set` on
a list field is indistinguishable from a swept handler that changed
only that field.

## 5. Implementation sketch

- `pocopine-core/src/handle.rs`: `FieldHandle<T>` (~80 lines) over
  `write_field_tracked` / `read_scope_key`.
- `pocopine-macros`: emit the extension trait in `#[component]` /
  `#[store]` (mirrors the RFC-081 ref-accessor emission); reserved-
  name check; interior-mutability field rejection; `&self` receiver
  branch in the `#[handlers]` arm emission.
- Reserved-name parity test in core; trybuild cases for the two new
  compile errors.

## 6. Verification

- `fh.set` from a spawned task: only the named field's effect
  re-runs (counter-pinned: zero fingerprint calls — add a
  `fingerprint_count` test counter alongside the existing
  `serde_projection_count`).
- A `&self` handler invocation: zero fingerprints, zero triggers,
  DOM untouched.
- Differential: a component driven to the same end state via
  (a) swept `update` closures and (b) `FieldHandle::set` calls
  renders byte-identical DOM (the W0 oracle pattern).
- trybuild: field named `update` → compile error; `Cell<u32>` field
  → compile error.
- Dead-scope `set`/`get` no-op semantics match `Handle`'s existing
  tests.

## 7. Open questions (resolved at implementation)

1. **Equality guard on `set`** — **unconditional in v1** (follows
   `self.x =` semantics), as proposed. The write mirror's projection
   versioning already collapses redundant DOM work downstream; revisit
   only if a stream source re-sending identical values shows up in a
   profile.
2. **Flatten-leaf handles** — **not in v1.** The container field gets a
   handle; leaf-level handles wait for demand.
3. **Computed read-handles** — **not in v1** (phase 2). v1 generates
   accessors for struct fields only.
4. **`update` naming** — **kept `update`.** `FieldHandle::update(|v| …)`
   takes `&mut T` (one field) where `Handle::update(|s| …)` takes
   `&mut Self` (whole struct); the receiver type at the call site makes
   the scope unambiguous, and a second verb (`modify`/`mutate`) would be
   one more thing to remember for no clarity gain.

## 8a. Implementation notes (as shipped)

- `FieldHandle<T>` lives in `pocopine-core/src/handle.rs`; `set`/`update`
  route through `scope::write_field` (dead-scope-safe, one trigger, no
  sweep), `get` through `read_scope_key` + serde (`unwrap_or_default`,
  so a dead scope reads as `T::default()` — `get`/`update` therefore
  require `T: Default`, `set` does not).
- `#[component]`/`#[store]` share three macro helpers
  (`field_handles_tokens`, `interior_mut_rejection`,
  `reserved_name_rejection`); both rejections early-return a single
  `compile_error!` (trybuild-pinned).
- The `&self`-skip is **runtime-gated, not arm-emitted**: the macro
  partitions handlers by receiver into `HandlerDispatch::
  is_readonly_handler`, `#[component]`/`#[store]` delegate it onto
  `ComponentState`, and `Scope::invoke` consults it to skip the
  `DirtySweep` + `with_scope_write` bracket. (`DirtySweep` was always
  runtime-side, never in the emitted arm.)
- A `fingerprint_count` debug/devtools counter (beside
  `serde_projection_count`) backs the acceptance tests: `FieldHandle::
  set`/`update` and `&self`-handler dispatch are asserted to issue zero
  fingerprints, while a swept `Handle::update`/`&mut self` handler
  issues > 0.

## 8. Compatibility

Purely additive at the API level. The interior-mutability rejection
is technically breaking for any component already holding a
`Cell`/`RefCell` field — a pre-1.0 changelog note plus the diagnostic
text (pointing at the `thread_local`-keyed-by-`ScopeId` pattern for
non-data handles) covers it; a workspace survey found zero such
fields in pocopine, pine, and the examples.
