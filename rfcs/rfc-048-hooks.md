# RFC 048 — Scoped async tasks and extractor-driven `#[computed]`

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 026](./rfc-026-post-mount-watch-field.md), [RFC 027](./rfc-027-provide-inject.md), [RFC 032](./rfc-032-lifecycle-element-param.md), Vue 3 `computed()`, Leptos `spawn_local_scoped_with_cancellation` |

## 1. Summary

Add two framework-level capabilities:

1. native async task helpers:
   * `pocopine::spawn(...)`
   * `pocopine::spawn_scoped(...)`
   * `pocopine::spawn_latest(...)`
2. a component-native computed field model:
   * `#[computed] fn ...(...) -> T`

This RFC intentionally does **not** propose a broad `pocopine::hooks`
module in v1.

The direction after discussion is:

* keep component fields as pocopine's canonical state model,
* keep `#[watch(field)]` as the primary reactive side-effect surface,
* add async task helpers that feel lifecycle-native,
* add computed values as readonly synthetic fields generated from
  extractor-style function signatures.

## 2. Motivation

### 2.1 What feels awkward today

Pocopine already has strong primitives for component state and
reactivity:

* component fields,
* `#[watch(field)]`,
* `Handle<T>::update`,
* `provide` / `inject`,
* extractor-style lifecycle parameters such as `LifecycleContext`
  from [RFC 032](./rfc-032-lifecycle-element-param.md).

What is still awkward is:

* async work that should cancel with scope lifetime,
* "latest wins" UI work such as search/autocomplete,
* derived readonly values that are more than trivial methods but do
  not deserve hand-maintained mirrored fields.

Today authors end up doing some combination of:

* raw `wasm_bindgen_futures::spawn_local`,
* ad hoc task-handle storage,
* hand-written cancellation/replacement logic,
* extra component fields plus watcher glue only to mirror derived
  state.

That works, but it is noisy and easy to get subtly wrong.

### 2.2 Why not a full hook system first

The earlier draft of this RFC proposed `use_state`, `use_computed`,
`use_effect`, `use_watch`, and related APIs.

After review, that direction is too broad for pocopine's current
shape.

The main problem is `use_state`: it introduces a second competing
state model alongside component fields. That weakens clarity around:

* what belongs in the struct,
* what templates read,
* what devtools can inspect later,
* what `#[watch]`, `#[model]`, and field roles operate on.

Pocopine is already field-centric. The stronger v1 is to preserve
that model and add only the missing capabilities around async work
and derived readonly values.

### 2.3 Why `#[computed]` instead of hook-style `computed`

A hook-style `computed(|| ...)` works mechanically, but it is a worse
fit than a component-native derived field:

* templates already think in terms of fields,
* component state already lives on the struct,
* `#[watch(field)]` already speaks in field names,
* a synthetic readonly field is easier to inspect than an opaque
  hook cell.

So instead of introducing a second authoring style, this RFC makes
computed values look like readonly framework-managed fields.

### 2.4 Why extractor-driven `#[computed]`

The tempting first shape is:

```rust
#[computed(deps = [loading, results])]
fn empty_message(&self) -> String { ... }
```

That is a footgun. Authors can forget to update `deps = [...]` when
the body changes.

This RFC instead adopts the extractor pattern pocopine already uses in
other places: dependencies are declared in the function signature, not
spelled separately in an attribute.

That gives us:

* no hidden `self` reads,
* dependencies inferred from parameters,
* stronger macro validation,
* a path to later support framework-provided computed inputs using the
  same extractor model.

## 3. Goals

* Keep component fields as the canonical state model.
* Add native async spawning that aligns with scope lifetime.
* Add a readonly computed-field surface without mirrored-field boilerplate.
* Reuse pocopine's extractor-style mental model where possible.
* Stay understandable to Vue users without importing a full
  Composition API surface.

## 4. Non-goals

* **No `use_state` in v1.**
* **No broad hook module in v1.**
* **No async computed in v1.** Async derivation carries cancellation,
  stale-result, and loading/error policy and should stay separate.
* **No arbitrary `&self` access inside `#[computed]`.**
* **No separate crate in v1.** Runtime support belongs in
  `pocopine-core`, re-exported from `pocopine`.

## 5. Proposed API

### 5.1 Async task helpers

```rust
pub fn spawn(fut: impl Future<Output = ()> + 'static);

pub fn spawn_scoped(
    fut: impl Future<Output = ()> + 'static
) -> TaskHandle;

pub fn spawn_latest(
    task_name: impl Into<Cow<'static, str>>,
    fut: impl Future<Output = ()> + 'static,
) -> TaskHandle;
```

`task_name` is the scope-local task slot name. It is not the user
query or payload; it is the replacement/cancellation key.

### 5.2 `TaskHandle`

```rust
pub struct TaskHandle { /* opaque */ }

impl TaskHandle {
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
}
```

### 5.3 `#[computed]`

Computed values are declared as functions on the component `impl`:

```rust
impl SearchBox {
    #[computed]
    fn has_results(results: &[Item]) -> bool {
        !results.is_empty()
    }

    #[computed]
    fn empty_message(loading: bool, has_results: bool) -> String {
        if loading {
            "Searching...".into()
        } else if !has_results {
            "No results".into()
        } else {
            String::new()
        }
    }
}
```

The framework treats each computed function as a readonly synthetic
field:

* function name becomes the computed field name,
* return type becomes the field type,
* parameters declare dependencies,
* the generated field is readable from templates and runtime
  introspection,
* the generated field is not directly settable.

## 6. `#[computed]` model

### 6.1 Signature rules

In v1:

* `#[computed]` applies to inherent component methods only.
* The method must not take `self`, `&self`, or `&mut self`.
* The method return type is required.
* Parameter names must resolve to either:
  * a component field, or
  * another computed field in the same component.
* Parameter extraction is readonly.

### 6.2 Supported parameter shapes

Initial support should stay narrow and explicit:

* owned field types where cloning is acceptable,
* shared references such as `&T`,
* slice-style borrows such as `&[T]` when the underlying field type
  makes that straightforward,
* computed-to-computed dependencies by name.

If a requested parameter shape cannot be extracted safely, the macro
should reject it with a compile error.

### 6.3 Generated behavior

For each `#[computed] fn empty_message(...) -> String`, the macro
generates roughly:

* runtime-managed synthetic storage keyed by the public field name,
* initialization during component setup,
* recomputation wiring through the existing lazy `Computed<T>` runtime,
* readonly exposure under the public name `empty_message`,
* runtime metadata so template/expression resolution can treat it like
  a field.

### 6.4 Computed depending on computed

Computed values may depend on other computed values:

```rust
impl SearchBox {
    #[computed]
    fn has_results(results: &[Item]) -> bool {
        !results.is_empty()
    }

    #[computed]
    fn empty_message(loading: bool, has_results: bool) -> String {
        if loading {
            "Searching...".into()
        } else if !has_results {
            "No results".into()
        } else {
            String::new()
        }
    }
}
```

This requires the macro/runtime to build a dependency graph:

* component fields are source nodes,
* computed values are derived nodes,
* recomputation follows topological order,
* cycles are rejected with a clear compile-time error.

### 6.5 Why no `self`

`self` would allow hidden reads that are not visible in the function
signature.

The point of extractor-driven `#[computed]` is to force dependency
declaration into the signature so authors cannot silently drift out of
sync.

## 7. Async task semantics

### 7.1 `spawn`

Detached fire-and-forget work.

### 7.2 `spawn_scoped`

Scope-bound async work.

Properties:

* tied to the current scope,
* auto-cancelled when the scope is removed,
* not cancelled by rerender or ordinary field updates.

### 7.3 `spawn_latest`

Scope-bound latest-wins task slot.

Properties:

* scoped like `spawn_scoped`,
* keyed by `task_name`,
* starting a new task with the same `task_name` cancels the previous
  active task in that scope first.

### 7.4 Cancellation model

Cancellation is cooperative and best effort:

* older tasks are no longer the active task for that slot,
* futures that observe cancellation should stop promptly,
* already-triggered external side effects are not automatically rolled
  back.

So the guarantee is "older task does not remain the active task for
this slot", not "all previous effects are undone".

## 8. Examples

### 8.1 With `spawn_latest`

```rust
#[derive(Serialize, Deserialize)]
pub struct SearchBox {
    pub query: String,
    pub results: Vec<Item>,
    pub loading: bool,
}

#[handlers]
impl SearchBox {
    #[watch(query)]
    fn on_query_change(&mut self, query: String, _prev: Option<String>) {
        self.loading = true;

        let handle = this::<Self>();
        spawn_latest("search", async move {
            let results = fetch_results(query).await;
            handle.update(|s| {
                s.results = results;
                s.loading = false;
            });
        });
    }
}
```

### 8.2 With `#[computed]`

```rust
#[derive(Serialize, Deserialize)]
pub struct SearchBox {
    pub query: String,
    pub results: Vec<Item>,
    pub loading: bool,
}

impl SearchBox {
    #[computed]
    fn has_results(results: &[Item]) -> bool {
        !results.is_empty()
    }

    #[computed]
    fn empty_message(loading: bool, has_results: bool) -> String {
        if loading {
            "Searching...".into()
        } else if !has_results {
            "No results".into()
        } else {
            String::new()
        }
    }
}
```

## 9. Runtime model

### 9.1 Layering

Implementation belongs in:

* `pocopine-core` for runtime storage and scheduler integration,
* `pocopine` for public exports.

### 9.2 Async task storage

Per-scope runtime state stores:

* active scoped tasks,
* named latest-wins task slots,
* cancellation handles.

### 9.3 Computed storage

Per-component runtime state stores:

* computed field entries keyed by public name,
* dependency ordering resolved by the macro,
* readonly exposure in runtime field resolution.

## 10. Rollout

1. Land scoped task runtime support in `pocopine-core`.
2. Re-export `spawn`, `spawn_scoped`, `spawn_latest` from `pocopine`.
3. Add macro/runtime support for `#[computed]`.
4. Expose computed values through template/runtime field reads.
5. Add compile errors for invalid parameter extraction and computed
   cycles.

## 11. Verification

Implemented and verified with:

* `cargo check -p pocopine-core`
* `cargo check -p pocopine`
* `cargo check -p pine`
* `cargo test -p pocopine-core --no-run`
* `cargo test -p pocopine --no-run`
* `wasm-pack test --firefox --headless crates/pocopine`
