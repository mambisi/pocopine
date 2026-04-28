# RFC 063 — Directive cleanup for Vue-3 alignment

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-28 |
| **Supersedes** | — (deletes/converges directives from RFC 001, RFC 007, RFC 011) |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 007](./rfc-007-pp-for-keys.md), [RFC 011](./rfc-011-scoped-slots.md), [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 061](./rfc-061-compiled-mount-only.md) |
| **Depends on** | RFC 061 implemented (compiled-mount-only — only the macro processes directives, so directive deletion is purely a macro/applier change) |

## 1. Summary

Three-bucket cleanup of the `pp-*` directive surface to align with
Vue 3 idiom and remove Alpine-era artifacts the post-RFC-061
architecture obsoletes:

- **Delete** (4 directives): `pp-init`, `pp-cloak`, `pp-data`,
  `pp-html`. These are pre-`#[handlers]` Alpine patterns or
  runtime-walker-era workarounds; the new architecture has typed
  replacements.
- **Converge** (3 directives): `pp-let` → folded into
  `pp-slot:name="binding"`; `pp-key` (on `<template>`) → `:key`
  on the row root; `pp-stagger` → `pp-for` modifier. Spelling
  changes that bring pocopine in line with Vue 3 conventions
  authors already know.
- **Promote** (1 directive): `pp-outlet` attribute → `<pp-outlet>`
  tag, matching the new `<pp-app>` root convention from RFC 061
  Q3 (if the council picks the tag form).

Closes task #78 (Alpine-inherited directive audit).

## 2. Motivation

### 2.1 The Alpine-era directives don't earn their keep anymore

Pocopine started life as an Alpine port (RFC 001). Several
directives were carried over from `x-init`, `x-cloak`, `x-data`,
`x-html` because Alpine had them. Post-RFC-058 (compiled views)
and RFC 061 (compiled-mount-only), the runtime constraints those
directives addressed are gone:

- **`pp-init`** existed because Alpine evaluated init expressions
  via the runtime walker. Pocopine now has `#[handlers] impl Foo
  { fn on_setup(&mut self) { ... } }` — typed access to `&mut
  self`, full Rust expressivity, lifecycle ordering guarantees.
  Two ways to do the same thing.
- **`pp-cloak`** existed because Alpine parsed HTML at runtime
  and there was a brief FOUC before bindings installed. Pocopine
  mounts synchronously off a precompiled plan; first paint is
  already bound.
- **`pp-data`** is an internal scope marker the macro
  auto-injects. Authors never write it; it just leaks into the
  documented surface as a public-looking attribute. Renaming
  the internal stamp to `data-pp-scope-id` removes one entry from
  the directive table.
- **`pp-html`** is `innerHTML` binding. Vue 3 keeps `v-html` but
  flags it as dangerous. Pocopine has no compelling use case
  the `#[handlers]` pattern doesn't cover (mount a child
  component, return a sanitized string from `pp-text`, etc.).

Each one is ~50-150 lines of macro + applier code. Deletion is
free wasm-size win on top of RFC 061's bridge removal.

### 2.2 The convergence three are spelled differently from Vue

Pocopine's syntax for scoped slots, keyed iteration, and stagger
animations is *almost* Vue 3 but diverges in small ways that
make muscle-memory ports painful:

| Pattern | Vue 3 | Pocopine today |
|---|---|---|
| Scoped slot | `<template v-slot:name="binding">` | `<template pp-slot="name" pp-let="binding">` |
| Keyed `for` | `<div v-for="t in tags" :key="t.id">` | `<template pp-for="t in tags" pp-key="t.id">` |
| Animation stagger | `useStagger(100)` (composables) | `pp-stagger="100"` |

Each diverges for a real reason at the time it was added, but
the architecture has matured past those reasons. Aligning the
spelling is a free DX win for any developer coming from Vue 3
(which is the audience this framework increasingly targets per
RFC 058 + 061's positioning).

### 2.3 `pp-outlet` is the only attribute-shaped framework hook left

After `<pp-app>` lands (if the council picks the tag form on
RFC 061 Q3), `pp-outlet` is the lone framework anchor still
spelled as an attribute. Promoting to `<pp-outlet>` makes the
framework's "magic" elements visually distinct from author-defined
custom tags + author-defined attributes. Routine consistency, no
behavioural change.

## 3. Non-goals

- **Not a rewrite of the directive engine.** All surviving
  directives keep their current implementation; this RFC just
  removes / renames.
- **Not a deprecation of `<Transition>`-style preset directives.**
  `pp-transition`, `pp-transition:enter`, etc. (RFC 005) are
  scope-locked.
- **Not a redesign of `pp-as` or `pp-anchor`.** Those are
  pocopine-specific value-adds with no Vue equivalent.
- **Not a change to event modifiers.** `@click.prevent`,
  `@keydown.enter.exact`, etc. (RFC 013) stay exactly as
  spelled.

## 4. Design

### 4.1 Deletions

#### 4.1.1 `pp-init`

**Replacement**: `#[handlers] impl Foo { fn on_setup(&mut self) { ... } }`.

The macro emits a `compile_error!` with a hint pointing to the
`on_setup` lifecycle hook. No silent removal — old code fails
loudly at compile time:

```text
error: `pp-init` was removed in v2 (see RFC 063).
       Use the `on_setup` lifecycle hook instead:

       #[handlers]
       impl MyComponent {
           fn on_setup(&mut self) {
               // your init code here
           }
       }
  --> src/pages/home.rs:14:5
   |
14 |     <div pp-init="self.count = 5">...</div>
   |          ^^^^^^^
```

Removes ~80 lines from `pocopine-core/src/directives/init.rs` +
the macro lifting code.

#### 4.1.2 `pp-cloak`

**Replacement**: none needed. Mount is synchronous; first paint
is bound. The `[pp-cloak] { display: none !important }` style
injection (currently done by `start_compiled` to hide content
before mount completes) is also deleted.

Compile error if used, with a "no longer needed — see RFC 063"
hint.

#### 4.1.3 `pp-data`

**Replacement**: the macro stamps an internal `data-pp-scope-id`
attribute on every component root automatically. Authors who
were typing `pp-data` (rare; it was always implicit) get a
compile error pointing them at the auto-stamping behaviour.

The runtime's `SCOPE_ID_KEY` private property pattern (set via
JS `Reflect::set` rather than as a DOM attribute) stays as the
performance-path identifier; `data-pp-scope-id` is debug-only
(emitted in dev builds for devtools, stripped in release).

#### 4.1.4 `pp-html`

**Replacement**: see §4.1.4 migration table — wrap the dynamic
HTML in a child component, or use `pp-text` with sanitized output.

Compile error with the recommended replacements listed inline.

### 4.2 Convergences

#### 4.2.1 `pp-let` → `pp-slot:name="binding"`

Today's syntax (two attributes):

```html
<template pp-slot="row" pp-let="row_data">
  {{row_data.title}}
</template>
```

New syntax (one attribute, mirrors Vue 3's `v-slot:name="binding"`):

```html
<template pp-slot:row="row_data">
  {{row_data.title}}
</template>
```

The macro accepts both syntaxes during the migration window
(Phase 2 below); the new form is canonical. After migration,
`pp-let` is removed.

#### 4.2.2 `pp-key` (template) → `:key` (row root)

Today:

```html
<template pp-for="t in tags" pp-key="t.id">
  <li>{{t.label}}</li>
</template>
```

New (matches Vue 3's `<div v-for :key>` shape):

```html
<template pp-for="t in tags">
  <li :key="t.id">{{t.label}}</li>
</template>
```

The runtime semantics are identical — the `:key` value is
extracted from the row root at row instantiation time,
identical to today's `pp-key` evaluation. The macro accepts
both spellings during Phase 2; `pp-key` removed after.

#### 4.2.3 `pp-stagger` → `pp-for.stagger`

Today:

```html
<template pp-for="t in tags" pp-stagger="100">...</template>
```

New (modifier on the controlling directive, matching the
`@click.prevent` modifier pattern):

```html
<template pp-for.stagger="100" for="t in tags">...</template>
```

Or, if the macro can't cleanly parse the modifier on `pp-for`'s
left-hand side, the simpler form keeps the directive name and
moves the modifier to a separate attribute:

```html
<template pp-for="t in tags" pp-for-stagger="100">...</template>
```

**Open question (§7 Q1)**: which spelling.

### 4.3 Promotion

#### 4.3.1 `pp-outlet` → `<pp-outlet>`

Today:

```html
<main pp-outlet></main>
```

New (matches `<pp-app>`):

```html
<main><pp-outlet></pp-outlet></main>
```

Or, if `<pp-app>` lands as `[pp-app]` attribute (RFC 061 Q3),
`pp-outlet` stays as an attribute for consistency. **Decision
depends on RFC 061 Q3.**

## 5. Migration

### 5.1 Phase 1 — deprecation warnings (one minor release)

Macro emits `#[deprecated]`-style warnings for every directive
in §4.1 (deletions) and `pp-let` / `pp-key` / `pp-stagger`
(convergences). Code keeps working; users see warnings during
their next build with the message + replacement.

### 5.2 Phase 2 — codemod tool

Ship `cargo pocopine migrate-063` that:

- rewrites `pp-init="..."` to a generated `on_setup` stub +
  flags the user to fill in the conversion (the expression-to-
  Rust translation is generally not automatic);
- removes `pp-cloak` attributes wholesale;
- removes `pp-data` from author-written templates (it was
  always implicit anyway);
- rewrites `pp-let` → `pp-slot:name="binding"` (mechanical);
- rewrites `pp-key` template attribute → `:key` on row root
  (mechanical);
- rewrites `pp-stagger` to whichever spelling §7 Q1 picks;
- flags `pp-html` usages with a per-site judgement call (no
  automatic conversion possible — usually a child component
  refactor).

The codemod runs idempotently and is safe to re-run after
manual edits.

### 5.3 Phase 3 — hard removal

One major version after Phase 1. The deprecated directives
become hard compile errors. Migration window: ~6 months.

## 6. Testing requirements

The RFC is not implemented until tests cover:

- every existing directive test in `crates/pocopine/tests/`
  passes against the renamed/converged syntax;
- compile-error fixtures (`tests/ui/`) for each deleted
  directive prove the error message matches the documented
  replacement;
- the codemod tool is runnable + idempotent on the workspace
  (`cargo pocopine migrate-063` followed by `cargo build`
  produces no diff on a second run);
- Pine compounds + the website example continue to render
  correctly post-codemod.

## 7. Open questions

1. **`pp-stagger` spelling** — `pp-for.stagger="100"` (modifier
   on the directive name) vs `pp-for-stagger="100"` (separate
   attribute). The first matches `@click.prevent` precedent;
   the second is easier for the macro to parse.
2. **`pp-outlet` promotion gating** — should it definitely
   promote to `<pp-outlet>` regardless of RFC 061 Q3, or
   stay an attribute if RFC 061 Q3 picks `[pp-app]` attribute?
3. **`pp-html` last-mile audit** — is there a use case we're
   missing? Examples scan: zero `pp-html` usages in the
   workspace today. Library users may have some. Worth a
   cargo-greppable pre-flight survey before deletion.
4. **`pp-data`'s debug-build `data-pp-scope-id`** — keep it
   in dev only, or always-emit for devtools simplicity?
   Always-emit is ~1 byte per element of bloat; dev-only is
   one more conditional in the codegen path.
