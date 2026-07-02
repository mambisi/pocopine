# RFC-111: Compile-time template-path validation

**Status:** IMPLEMENTED (this branch)
**Crates:** `pocopine-macros` (`template_paths` module, `#[component]`, `#[handlers]`)
**Relates to:** RFC-024 (expressions), RFC-049/084 (the slot-assertion idiom this reuses), RFC-057/058 (plan compilation), RFC-081 (typed refs — same typo→rustc-error strategy)

## Summary

Every expression root a `.poco` template can evaluate is now checked at
`cargo check` time: a root must name a struct field, an explicit-list
flatten leaf, a `#[computed]` field, or — for `pp-on` targets — a
`#[handlers]` method. A typo'd or renamed root fails compilation:

```
error[E0599]: no associated item named `__poc_bindable_countt` found for struct `Counter`
  --> src/counter.rs:8:1
   = rustc's did-you-mean usually suggests `__poc_bindable_count`
```

This closes the main safety gap of the plans-as-data architecture
relative to compiled-view frameworks (rename a field → compile error,
not a silent runtime `undefined`) at **zero bundle cost** — the whole
mechanism is `const _` items and hidden marker fns that exist only for
rustc's item resolution and get stripped from codegen.

## Design

Cross-macro validation via **compiler-resolved markers** (the RFC-049
`assert_child` idiom): the `#[component]` macro cannot see `#[computed]`
fields or handler methods (they live in the separate `#[handlers]` impl),
so instead of checking names directly it defers the join to rustc:

1. `#[component]` emits one hidden marker per struct field and explicit
   flatten leaf: `pub fn __poc_bindable_<name>() {}`.
2. `#[handlers]` emits `__poc_bindable_<name>` per `#[computed]` field
   and `__poc_handler_<name>` per dispatchable method (markers are
   cfg-unconditional so references resolve on host AND wasm even when
   the method is cfg-gated; a `BTreeSet` dedups cfg-split names).
3. `#[component]` harvests the template (a dedicated AST pass in
   `template_paths.rs` — independent of plan eligibility, so
   walker-fallback expressions are covered too) and emits one anonymous
   reference per distinct root: `const _: fn() = <T>::__poc_bindable_<root>;`.

Root harvesting recurses the `pocopine-expr` AST: `Path[0]` and
`Assign.path[0]` → bindable; `Call` names → handler; a bare single-ident
listener value is a handler (the RFC-024 backfill). Harvested attribute
set: `pp-text/html/show/if/else-if/match`, `pp-bind:*` / `:*`,
`pp-on:*` / `@*`, `pp-model[*]`, `pp-for` (items side), and `{{ interp }}`
text segments. Everything unrecognised is skipped — under-validation is
safe, over-validation breaks builds.

### Exemptions (deliberate)

- **`$`-rooted names** (`$store`, `$route`, `$event`, loop magics) —
  not locally checkable; the runtime warn path owns them.
- **Locally-bound names** — `pp-for` items and `pp-let` idents
  (including `pp-case pp-let` binds), whitelisted by a scope stack in
  the harvest walk. The items expression itself is harvested in the
  outer scope before the item binds.
- **Nested segments** — `user.name` checks `user` only; the macro
  cannot see into field types.
- **Bare `#[prop(flatten)]` components** — leaf names resolve at
  runtime through the `Props` trait, so any unknown root might be a
  leaf; bindable checks are skipped for those components (handler
  checks remain).
- **`unchecked_paths = "true"`** on `#[component]` — the escape hatch.

## What it caught on day one

Enabling the check across the workspace surfaced **six real dead
bindings and zero false positives**:

- `PineDateRangeField` / `PineTimeRangeField`: templates called
  `effective_start_max()` / `effective_end_min()` — methods that lived
  in a plain `impl` block, so `invoke_handler` had no arm and the
  range-clamping `min`/`max` props silently bound `undefined` since the
  primitives shipped. Fixed properly: converted to `#[computed]` (a
  handler call in a binding would also never re-run on `start`/`end`
  changes), templates now read them as fields.
- `PineRichTextRoot`: `:data-empty="state == null"` referenced a field
  that never existed → rebound to the real `doc` field.
- Website `ContextMenuDemo` / `ToolbarDemo`: `@click="action_bump"` /
  `action_reset` dispatched to handlers that were never written.
- Pine wasm-test fixtures: dead `dialog_open` / `open_it` bindings in
  `compiled_fixture!` fixtures (the macro gained an optional fields arm).

## Non-goals

- Type-checking expressions (`count + 1` where `count: String`) — that
  requires compiling expressions to Rust, rejected in the
  interpreter-vs-codegen analysis (it would trade the plans-as-data
  size/shipping model for marginal safety).
- Validating `$store.<name>.<field>` roots across components/crates.
- IDE-facing spans into `.poco` files (errors anchor on the
  `#[component]` attribute; the marker name carries the typo).

## Tests

- `pocopine-macros` unit tests: harvest classification, scope
  push/pop, magic/local skips, listener backfill, bindable-skip mode.
- `crates/pocopine/tests/template_paths_ui.rs` (trybuild): compile-pass
  (fields + computed + handlers + magics + loop/let locals), the
  escape hatch, and compile-fail snapshots for a typo'd field and a
  typo'd handler. Reject snapshots pin rustc E0599 framing —
  regenerate with `TRYBUILD=overwrite` if the nightly drifts.
- Whole-workspace build (host + the CI clippy-wasm gate with
  `--all-targets`) is the standing false-positive gate; pine's
  126-test wasm suite passes with the fixture fixes.
