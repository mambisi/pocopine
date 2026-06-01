# RFC 063 — Directive cleanup for Vue-3 alignment

| Field | Value |
|---|---|
| **Status** | Accepted (Tier 1 deletes landed: pp-cloak/pp-init/pp-data forbidden; §4.2 convergences, migrate-063 codemod, pp-outlet promotion & pine-icons rewrite pending) |
| **Author** | pocopine team |
| **Created** | 2026-04-28 |
| **Supersedes** | — (deletes/converges directives from RFC 001, RFC 007, RFC 011) |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 007](./rfc-007-pp-for-keys.md), [RFC 011](./rfc-011-scoped-slots.md), [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 061](./rfc-061-compiled-mount-only.md) |
| **Depends on** | RFC 061 implemented (compiled-mount-only — only the macro processes directives, so directive deletion is purely a macro/applier change) |

## 1. Summary

Three-bucket cleanup of the `pp-*` directive surface to align with
Vue 3 idiom and remove Alpine-era artifacts the post-RFC-061
architecture obsoletes:

- **Delete** (3 directives): `pp-init`, `pp-cloak`, `pp-data`.
  These are pre-`#[handlers]` Alpine patterns or
  runtime-walker-era workarounds; the new architecture has typed
  replacements.
- **Keep `pp-html`** (revised 2026-04-29). Every modern web
  framework ships an HTML-string injection primitive (Vue 3
  `v-html`, React `dangerouslySetInnerHTML`, Svelte `{@html}`,
  Solid `innerHTML`, Yew `Html::from_html_unchecked`). The use
  cases that need it (sanitized markdown output, server-rendered
  fragments, CMS embeds) are rare-but-load-bearing; removing
  the primitive puts pocopine at parity with no one. See §4.4
  for the load-bearing icon rewrite that retires the only
  current `pp-html` consumer (`pine-icons`'s SVG-string
  approach), leaving `pp-html` in the framework for the
  legitimate runtime cases only.
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
- **`pp-html` stays** (revised). Surveyed every other modern web
  framework — all ship an HTML-string injection primitive
  (Vue `v-html`, React `dangerouslySetInnerHTML`, Svelte
  `{@html}`, Solid `innerHTML`, Yew `Html::from_html_unchecked`).
  The use cases (sanitized markdown, server fragments, CMS
  embeds) are rare-but-real; removing puts pocopine at parity
  with no one. The only current consumer (PineIcon's SVG-string
  injection) is itself a legacy pattern — modern icon libraries
  ship one component per icon (Lucide / Heroicons / Tabler /
  Phosphor all converged on this). §4.4 specs the PineIcon
  rewrite that retires this consumer; `pp-html` then exists in
  the framework for the legitimate runtime cases only.

The other three (`pp-init`, `pp-cloak`, `pp-data`) are
~50-150 lines of macro + applier code each. Deletion is free
wasm-size win on top of RFC 061's bridge removal.

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

### 4.4 Retire `pine-icons`'s SVG-string pattern (the only `pp-html` consumer)

`pp-html` stays in the framework (§1, §2.1), but `pine-icons`
today uses it for SVG injection — a legacy pattern modern icon
libraries (Lucide, Heroicons, Tabler, Phosphor) abandoned in
favor of one-component-per-icon. This subsection specs the
rewrite that brings `pine-icons` to the modern pattern. After
it lands, `pp-html` has zero workspace consumers — exactly the
right shape for a primitive that exists only for legitimate
runtime-trusted-HTML edge cases.

#### 4.4.1 The current shape

```rust
// crates/pine-icons/src/PineIcon.poco
<root class="pine-icon" aria-hidden="true" pp-html="svg" :style="size_style"></root>
```

`PineIcon` takes an SVG string field (`svg`) from a runtime
icon registry and injects it into its root via `pp-html`. The
registry is a `HashMap<&str, &str>` populated at app boot;
icon name lookups happen at render time.

Problems:

- No tree-shaking: an app using 5 icons ships the entire
  registry (potentially 500+ icons).
- No type checking: `<pine-icon name="bel" />` (typo) fails
  silently at runtime.
- Per-icon parse cost: each render parses the SVG string into
  DOM via `innerHTML`.
- Locks `pp-html` as load-bearing instead of an edge-case
  primitive.

#### 4.4.2 The replacement

One typed `#[component]` per icon, generated at build time
from a Lucide-format manifest:

```rust
// generated by `pine-icons` build script, one per icon:
#[component(template_inline = r#"
<root class="pine-icon" aria-hidden="true" :style="size_style">
  <svg viewBox="0 0 24 24" fill="none" stroke="currentColor"
       stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
    <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/>
    <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>
  </svg>
</root>
"#)]
pub struct PineIconBell { /* size, color, etc. */ }
```

Authors write `<pine-icon-bell/>` — typed, tree-shakeable,
no `pp-html`. Same DX as `lucide-react`'s `<Bell />`.

For dynamic icon names (string from CMS), a thin
`<pine-icon-dynamic name="bell" />` registry component does
runtime dispatch via `Component::MOUNT_FN` lookup. Still no
`pp-html`.

#### 4.4.3 Implementation surface

- New `pine-icons/build.rs` — reads
  `pine-icons/icons/manifest.json` (Lucide format), emits
  one `#[component]` per icon into a generated module.
- Delete `pine-icons/src/PineIcon.poco`.
- Migrate the runtime registry into a typed `PHF_ICONS:
  &'static phf::Map<&'static str, fn() -> Component>` for
  the dynamic-name path.
- Delete the `pp-html` use site.

#### 4.4.4 Acceptance

- `cargo check` succeeds across the workspace with the new
  per-icon components.
- Counter / website examples render the same icons as before.
- Tree-shaking demo: an app using 3 icons produces a smaller
  binary than today's full-registry-shipping baseline.
- Workspace `grep 'pp-html'` returns zero hits in `pine-*`
  source.

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
- rewrites `pp-stagger` to whichever spelling §7 Q1 picks.

`pp-html` is **not** rewritten by the codemod — it stays in
the framework. Authors who were using it (rare; only
`pine-icons`'s legacy SVG injection) get redirected to §4.4's
typed-component icon pattern via separate tooling, not the
syntax codemod.

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
3. **`pp-html` rename to `pp-html-unsafe`** — purely aesthetic;
   would mirror React's `dangerouslySetInnerHTML` pattern. Same
   runtime, more accurate name. Not in scope for this RFC; flag
   for a future syntax-cleanup if the council values the
   visibility-at-call-site pattern.
4. **`pp-data`'s debug-build `data-pp-scope-id`** — keep it
   in dev only, or always-emit for devtools simplicity?
   Always-emit is ~1 byte per element of bloat; dev-only is
   one more conditional in the codegen path.

## 8. Implementation status

Tier 1 deletes shipped on `wip/rfc-062` (eligible for the next
PR cut against `main`):

| Directive | Commit | Status |
|---|---|---|
| `pp-cloak` | 9d4b733 | ✅ Runtime style deleted; macro errors on author use |
| `pp-init` | e117589 | ✅ Directive module + plan IR + 3 fixtures + macro emit deleted; macro errors on author use |
| `pp-data` | 7e08eee | ✅ Author-facing surface forbidden; macro auto-stamp continues; internal rename to `data-pp-scope-id` is a follow-up cleanup PR |

Mechanism: a new `forbidden_directives` macro module walks
each template AST at expansion time and emits `compile_error!`
for entries in a `FORBIDDEN: &[(&str, &str)]` table. Adding a
new directive is the entire migration step — the walker
handles diagnostic emission. Module-level docs cite RFC 063
as the spec source.

`pp-html` is **explicitly excluded** from the FORBIDDEN
table (documented in the module-level doc-comment) — every
modern web framework ships an HTML-string injection primitive
(Vue `v-html`, React `dangerouslySetInnerHTML`, Svelte
`{@html}`, Solid `innerHTML`, Yew `Html::from_html_unchecked`).
See §1 + §4.4.

### Deferred to follow-up PRs

| Item | Reason |
|---|---|
| `pp-data` → `data-pp-scope-id` internal rename | Pure internal cleanup; touches `inject_pp_data` + runtime read site + `phf_lookup.rs` test assertion + macro-internal comments. Not a user-facing change so doesn't need to ship with the Tier 1 deletes |
| `pp-let` → `pp-slot:name="binding"` (§4.2.1) | Macro parser change + Pine fixture migration (`PlanScopedSlotHost.html` + Pine compounds). Needs codemod first |
| `pp-key` → `:key` on row root (§4.2.2) | Same — macro parser change + sweep of every Pine `pp-for pp-key` site |
| `pp-stagger` spelling (§4.2.3) | Blocks on §7 Q1 council decision |
| `pp-outlet` → `<pp-outlet>` (§4.3.1) | Blocks on RFC 061 Q3 council decision |
| `pine-icons` rewrite (§4.4) | Needs Lucide-format manifest + build.rs scaffolding; independent Pine improvement |
