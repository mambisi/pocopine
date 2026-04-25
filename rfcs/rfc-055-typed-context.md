# RFC-055 — Typed Context Ergonomics on Top of Keyed `provide` / `inject`

Status: Draft

Author: Codex

Created: 2026-04-24

## 1. Summary

Keep keyed context as the authoritative runtime model:

```rust
pocopine::inject_key!(ROOT: Handle<PineDialogRoot>);
provide(&ROOT, this::<PineDialogRoot>());
let root = inject(&ROOT);
```

But add a more framework-native ergonomic layer on top:

- keyed extractor form for handlers / lifecycle:
  `Inject<ROOT, Handle<PineDialogRoot>>`
- method-style helpers on keys:
  `ROOT.provide(value)` and `ROOT.inject()`
- future sugar for declaring context channels:
  `#[context]`

This RFC does not replace `InjectKey<T>`. It standardizes a better authoring surface on top of it.

## 2. Motivation

RFC 027 and RFC 030 established the right safety model:

- context channels must have explicit identity
- type-only injection is ambiguous
- string keys are weak and refactor-hostile

That model is still correct. The problem is ergonomics.

Today, the API feels more like a low-level runtime utility than a framework-native authoring tool:

```rust
pocopine::inject_key!(ROOT: Handle<PineDialogRoot>);
provide(&ROOT, this::<PineDialogRoot>());
let root = inject(&ROOT);
```

This is serviceable, but the surface can be improved in three ways:

1. The key declaration macro now does more than its name suggests.
   It defines the runtime key and, for extractor use, also needs a type-level identity.

2. The callsites are noisier than they need to be.
   `provide(&ROOT, ...)` and `inject(&ROOT)` are explicit, but the repeated `&ROOT` reads like utility API, not framework API.

3. Handler and lifecycle extractors want a keyed injection story that is both explicit and pleasant:

```rust
pub fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) { ... }
```

That is a strong shape, but the rest of the API should line up with it.

## 3. Goals

- Preserve keyed context as the only authoritative runtime model.
- Make keyed context feel native in handlers, lifecycle hooks, and component internals.
- Allow a future `#[context]` surface without weakening the safety guarantees of keyed injection.

## 4. Non-goals

- Replacing keyed context with type-only injection.
- Making bare `Inject<T>` the default public API.
- Supporting arbitrary auto-discovery of context providers.
- Changing the parent-scope walk semantics from RFC 027.

## 5. Proposal

### 5.1 Keep `InjectKey<T>` as the core primitive

The runtime remains keyed:

```rust
InjectKey<T>
provide(&KEY, value)
inject(&KEY)
```

No new non-keyed runtime path is introduced.

### 5.2 Add key-method sugar

Every declared key should support:

```rust
ROOT.provide(value);
ROOT.inject();
```

Equivalent to:

```rust
provide(&ROOT, value);
inject(&ROOT);
```

This keeps the existing primitive functions but gives framework code a cleaner dominant style.

### 5.3 Standardize keyed extractor syntax

Handler and lifecycle extraction should use:

```rust
Inject<ROOT, Handle<PineDialogRoot>>
Option<Inject<ROOT, Handle<PineDialogRoot>>>
```

This is the user-facing extractor shape. It is explicit, reviewable, and collision-safe.

### 5.4 Clarify key declaration naming

The current `inject_key!` name is serviceable but increasingly misleading, because the declared item is now used for:

- runtime provide/inject calls
- keyed extractor identity
- future sugar surfaces

This RFC allows either of these outcomes:

1. Keep `inject_key!` for compatibility and treat it as the stable low-level name.
2. Add a clearer alias, such as:
   - `context_key!`
   - `provide_key!`

If an alias is added, `inject_key!` remains supported.

### 5.5 Add future `#[context]` sugar

This RFC reserves a higher-level declaration form:

```rust
#[context]
pub struct DialogContext(pub Handle<PineDialogRoot>);
```

or:

```rust
#[context]
pub struct DialogContext {
    pub root: Handle<PineDialogRoot>,
    pub open: bool,
}
```

The important rule is:

- `#[context]` is sugar over generated keyed context
- it does not introduce type-only injection as the underlying model

The generated expansion may include:

- a hidden `InjectKey<DialogContext>`
- helper methods like `DialogContext::provide(...)`
- extractor compatibility via `Inject<DialogContextKey, DialogContext>`

The exact `#[context]` expansion is future work, but this RFC establishes the rule that it must compile down to keyed context.

## 6. Design Rationale

### 6.1 Why not `Inject<T>` by type?

Because it is ambiguous.

Two distinct ancestors can legitimately provide the same value type:

```rust
Handle<PineDialogRoot>
Handle<PineDialogRoot>
```

or:

```rust
String
bool
Handle<AppState>
```

Type-only injection makes these collisions silent and difficult to review. Keyed injection keeps the dependency explicit.

### 6.2 Why keep both functions and key methods?

Because they serve different roles:

- `provide` / `inject` remain the simple core runtime primitives
- `ROOT.provide(...)` / `ROOT.inject()` become the ergonomic dominant style

This mirrors other pocopine APIs where a low-level primitive remains available but a more fluent surface becomes normal in component code.

### 6.3 Why allow `#[context]` later?

Because authors should not need to manually hand-roll every context type and helper forever. But the ergonomic layer should not erase the safety model.

`#[context]` is worth doing only if it preserves:

- explicit channel identity
- stable generated keys
- clear extractor signatures

## 7. Examples

### 7.1 Current low-level form

```rust
pocopine::inject_key!(ROOT: Handle<PineDialogRoot>);

#[handlers]
impl PineDialogRoot {
    pub fn on_mount(&mut self) {
        provide(&ROOT, this::<Self>());
    }
}
```

### 7.2 Preferred keyed-method form

```rust
pocopine::inject_key!(ROOT: Handle<PineDialogRoot>);

#[handlers]
impl PineDialogRoot {
    pub fn on_mount(&mut self) {
        ROOT.provide(this::<Self>());
    }
}
```

### 7.3 Keyed extractor in a child

```rust
#[handlers]
impl PineDialogCloseButton {
    pub fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) {
        root.update(|dialog| dialog.close());
    }
}
```

### 7.4 Optional keyed extractor

```rust
#[handlers]
impl MaybeDialogChild {
    pub fn on_mount(&mut self, root: Option<Inject<ROOT, Handle<PineDialogRoot>>>) {
        self.inside_dialog = root.is_some();
    }
}
```

## 8. Migration

No breaking change is required for the base runtime model.

Existing code:

```rust
provide(&ROOT, value);
inject(&ROOT);
```

continues to work.

New code may gradually adopt:

```rust
ROOT.provide(value);
ROOT.inject();
Inject<ROOT, T>
```

## 9. Open Questions

1. Should `inject_key!` remain the primary name, or should `context_key!` be added as the preferred alias?
2. Should key-method sugar be implemented on the key type itself, on the `LazyLock`, or via macro-generated wrapper glue?
3. What exact `#[context]` expansion is best:
   - tuple newtype
   - field struct
   - hidden key + generated helpers
4. Should keyed extraction use `Inject<ROOT, T>` exactly, or a slightly longer name like `InjectKeyed<ROOT, T>` for symmetry with the runtime?

## 10. Recommendation

Implement in this order:

1. keyed extractor form `Inject<KEY, T>`
2. method-style sugar `KEY.provide(...)` / `KEY.inject()`
3. optional naming alias for `inject_key!`
4. only then explore `#[context]` as generated sugar over the same keyed model
