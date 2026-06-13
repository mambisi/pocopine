# RFC 018 — `$id` magic (unique ID generator)

| Field | Value |
|---|---|
| **Status** | Superseded by RFC-095 — the `$id` magic was removed in the signals rewrite (zero usage; `ctx.scope_id` covers it). Only the `+` string-concat operator from this RFC survives. |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-012-expression-evaluator.md`](./rfc-012-expression-evaluator.md) |

## 1. Summary

Per-component-instance unique IDs for a11y wiring. Used on every
dialog, combobox, accordion, tablist, switch, and form field Pine
will ship.

```html
<!-- PineField.poco -->
<div>
  <label pp-bind:for="$id + '-input'"><slot name="label" /></label>
  <input pp-bind:id="$id + '-input'"
         pp-bind:aria-describedby="$id + '-help'" />
  <p pp-bind:id="$id + '-help'"><slot name="help" /></p>
</div>
```

Rendering two `<pine-field>`s on the same page yields distinct
`for` / `id` pairs — `pp-1-input` vs `pp-2-input` — so
`label[for]` correctly associates with the right input.

## 2. Non-goals

- **Deterministic / SSR-stable IDs.** Not planned. SSR is out of
  scope until the server-rendering RFC lands; at that point the
  counter seed becomes request-scoped. For now IDs are just
  monotonic, mint-order.
- **Function-call syntax** (`$id("input")`). The expression
  evaluator stays function-call-free; users compose via `+`.
- **Stable identifiers across reloads.** Every page load restarts
  the counter; IDs are process-lifetime only.

## 3. Surface

### 3.1 Template magic

`$id` — a stable `String` unique per component **instance**. Always
begins with `pp-` to avoid colliding with author-chosen IDs.

```html
<button pp-bind:id="$id">OK</button>               <!-- id="pp-1" -->
<label pp-bind:for="$id + '-email'">Email</label>  <!-- for="pp-2-email" -->
<input pp-bind:id="$id + '-email'" />              <!-- id="pp-2-email" -->
```

The same `$id` value is returned on every access within the same
component instance — bindings that re-evaluate don't mint fresh IDs.
Reading `$id` never triggers reactivity; the value is frozen at
mount time.

### 3.2 Handler access

Handlers can read their own `$id` through a new helper:

```rust
use pocopine::id;

#[handlers]
impl PineDialog {
    pub fn on_open(&mut self) {
        let title_id = format!("{}-title", id::current());
        // ...
    }
}
```

`id::current()` returns `Option<String>` — `None` outside a handler
invocation.

## 4. Required evaluator change

Sub-ID composition (`$id + '-title'`) requires **`+`** in the
expression evaluator. The evaluator, today, has no arithmetic. This
RFC ships `+` alongside `$id` because without it the magic has no
ergonomic surface.

`+` semantics:

- If either operand is a string, both are coerced to string and
  concatenated.
- Otherwise, if both coerce to `f64`, numeric addition.
- Otherwise, returns an empty string.

Precedence: between relational (`<` / `<=` / …) and unary (`!`). So
`a + b == 'foo-bar'` parses as `(a + b) == 'foo-bar'`. No support
for `-` yet; add later if needed.

## 5. Semantics

### 5.1 Minting

A single thread-local counter — `NEXT_ID: Cell<u64>` — increments
on first `$id` read for a given scope. The minted value is cached
in the scope's private slot so subsequent reads return the same
string.

```
pp-1, pp-2, pp-3, ...
```

Single prefix, decimal counter, no padding. Short enough to keep
templates readable.

### 5.2 Caching per scope

The scope owns the cache; eviction (`Scope::remove`) drops the
cached ID along with everything else.

### 5.3 Reactivity

`$id` does not subscribe to any reactive source. Bindings that read
it will not re-run when unrelated state changes — but the `+`
operator's *other* operand (e.g. a reactive `self.title`) does, and
the combined expression recomputes in the usual way.

## 6. Implementation

- New module `crates/pocopine-core/src/id.rs` — ~50 lines. Exports
  `generate(scope: ScopeId) -> String` (minting + caching) and
  `current() -> Option<String>` (handler-facing shortcut).
- `magics::resolve` gets a `"$id"` branch that calls
  `id::generate(scope_id)`.
- `expr.rs` gains `BinOp::Plus`, `Tok::Plus`, lexer recognition of
  `+`, a `parse_additive` layer between `parse_relation` and
  `parse_unary`, and `BinOp::Plus` evaluation (string-aware).

Cache lives in a thread-local `HashMap<ScopeId, String>`; cleared
from `Scope::remove` alongside `refs::clear_scope`.

## 7. Edge cases

- **Reading `$id` in a scope that never gets evicted.** The cache
  entry lives forever — but the scope itself also does, so no leak
  relative to the scope's lifetime.
- **Multiple refs to `$id` in the same template.** All return the
  same string. Counter only increments on first access.
- **`$id` inside a `pp-for` body.** Each iteration is a separate
  `LoopScope`; each gets its own `$id`. Good — a keyed list of
  form fields produces one id per field.
- **Numeric `+` vs string `+`.** Mixed types stringify both sides,
  following JavaScript. Users wanting pure numeric behaviour write
  `Number(x) + Number(y)` — but we don't ship `Number()`; in
  practice, numeric `+` is rare in templates. If this becomes a
  real pain point, a separate numeric `+` RFC can add a strict
  mode.
- **String `-`.** Not added here; if someone needs it they can
  compose `'${prefix}-${suffix}'`-style via concat.

## 8. Examples

### Dialog labelled / described

```html
<div role="dialog"
     pp-bindpp-bind:aria-labelledby="$id + '-title'"
     pp-bind:aria-describedby="$id + '-desc'">
  <h2 pp-bind:id="$id + '-title'"><slot name="title" /></h2>
  <p  pp-bind:id="$id + '-desc'"><slot name="description" /></p>
  <slot />
</div>
```

### Tablist / panels

```html
<div role="tablist">
  <button pp-for="(tab, i) in tabs"
          role="tab"
          pp-bind:id="$id + '-tab-' + i"
          pp-bind:aria-controls="$id + '-panel-' + i"
          pp-bind:aria-selected="i == selected">
    {tab.label}
  </button>
</div>
<div pp-for="(tab, i) in tabs"
     role="tabpanel"
     pp-bind:id="$id + '-panel-' + i"
     pp-bindpp-bind:aria-labelledby="$id + '-tab-' + i"
     pp-show="i == selected">
  {tab.body}
</div>
```
