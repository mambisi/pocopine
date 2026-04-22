# RFC 042 — `class` / `style` parity: arrays, custom properties, `|important`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-22 |
| **Supersedes** | — |
| **Related** | [`rfc-020-shorthand-prefixes.md`](./rfc-020-shorthand-prefixes.md), [Svelte `class` docs](https://svelte.dev/docs/svelte/class), [Svelte `style:` docs](https://svelte.dev/docs/svelte/style) |

## 1. Summary

Pocopine already supports reactive `:class="expr"` (string + object
form) and `:style="expr"` (string + object form), plus RFC-010 class
fallthrough from author to component root. Five gaps remain against
Svelte 5 parity:

1. **`:class` array form** — `:class="['foo', cond && 'bar', {baz: flag}]"`
   silently drops today because `serialise_class` only branches on
   `String` and `Object`.
2. **`:style` custom properties with non-string values** —
   `:style="{ '--count': 3 }"` renders `--count:;` today because
   `serialise_style` reads every value as `as_string().unwrap_or_default()`.
3. **`|important` on `:style` values** — no way to force
   `!important` on a bound property without hand-serialising the
   string. Svelte spells it `style:color|important="red"`.
4. **`class:<name>="expr"` directive** — ergonomic one-off
   conditional class, parity with Svelte / Vue / Alpine. Compiles
   to the same serialiser pipeline as `:class`, so it composes
   with fallthrough and with other `:class` / `class:` on the
   same element.
5. **`style:<name>="expr"` directive** — symmetric short form for
   single-property style binds, including CSS custom properties:
   `style:--columns="count"`, `style:color|important="red"`.

Plus one Rust-side addition: `sx!` macro, mirroring the existing
`cx!` (RFC-010) for building style strings in handlers / computed
getters without manual `format!`ing.

This RFC closes those gaps additively. Nothing breaks.

## 2. Non-goals

- **A `ClassValue` type export.** Pocopine is Rust/WASM; there's
  no author-facing TS type surface to extend. Rust authors already
  cover the same shapes with field types: a `Vec<String>` field
  serialises to a JS array, `HashMap<String, bool>` to an object,
  `HashMap<String, String>` to a key-value style object — all
  consumed by the serialisers this RFC extends.
- **Array form for attributes other than `class`/`style`.** Arrays
  in plain HTML attributes have no conventional meaning; restricting
  the shape keeps error messages actionable.
- **Tailwind-merge-aware dedup in `cx!` / `sx!` / `:class`.** Same
  reason as RFC-010 §7 — a separate concern, opt-in later if
  demand appears.

## 3. Surface

### 3.1 `:class` array form

Clsx-style flattening. Each element contributes zero or more class
tokens:

| element type | contribution |
|---|---|
| non-empty string | the string, split on whitespace |
| truthy-valued object | every key whose value is truthy |
| nested array | recursively flattened |
| `null` / `undefined` / `false` / `""` / `0` / `NaN` | nothing |
| number (truthy) | stringified |
| `true` / other truthy primitive | nothing (matches clsx) |

```html
<div :class="['card', size === 'lg' && 'card-lg', { selected, disabled: !ready }]"></div>

<!-- state: size='lg', selected=true, ready=false                            -->
<!-- rendered: class="card card-lg selected disabled"                        -->
```

Composition via component fallthrough stays unchanged (RFC-010) —
the outer author class is merged onto the root via the same
`merge_space` helper, regardless of whether either side started as
an array.

### 3.2 `:style` custom-property value coercion

`:style` object values today land through `val.as_string().unwrap_or_default()`,
so a bound number or bool writes an empty value. Extend the
coercion to mirror `serialise_plain`'s existing behavior:

| value type | rendered |
|---|---|
| string `"red"` | `red` |
| number `14` | `14` (authors still author the unit in the key: `padding: '14px'` or write a derived string) |
| bool `true` / `false` | skip the property entirely |
| `null` / `undefined` | skip the property entirely |

The existing object-key iteration, key-name preservation, and `;`
separator stay the same. Crucially this makes CSS custom properties
carry numeric values:

```html
<div :style="{ '--columns': cols, '--gap-rem': 1.5 }"></div>
<!-- cols=3 ⇒ style="--columns:3;--gap-rem:1.5;"                             -->
```

### 3.3 `class:<name>="expr"` directive

Sugar for a single conditional class token. `expr` is evaluated
reactively; the token is present when truthy, absent when falsy.

```html
<!-- one conditional ─ directive reads well -->
<button :class="base" class:active="is_active">

<!-- many conditionals ─ object form still scales better -->
<button :class="['base', { active: is_active, loading: is_busy, disabled: !ready }]">
```

Both forms can sit on the same element; they accumulate into the
same serialiser output. The shorthand form (`class:active` with no
value) isn't adopted — Svelte's shorthand leans on same-named
locals, which doesn't carry over to pocopine's scope-proxy read
model. Authors write the expression explicitly.

### 3.4 `style:<name>="expr"` directive

Sugar for a single-property style bind. Supports CSS custom
properties directly (no escaping required) and the `|important`
modifier on the directive name.

```html
<!-- plain -->
<div style:color="theme_color">

<!-- custom property -->
<div style:--columns="col_count">

<!-- !important, attached to the directive name -->
<div style:color|important="theme_color">

<!-- stacked -->
<div style:color="c" style:background="bg" style:--radius="r">
```

Multiple `style:` directives on the same element merge into one
write, preserving author order. `style:` and `:style` coexist on
the same element — each contributes to the final `style` attribute;
the merge order is `:style` first, then `style:` directives in
source order, so `style:` wins on conflicts (matches Svelte).

### 3.5 `|important` modifier on `:style` values

For parity with Svelte we accept a trailing `|important` inside the
**value string** of either form:

```html
<!-- object form -->
<div :style="{ color: 'red|important', 'font-size': base_size }"></div>

<!-- string form -->
<div :style="'color: red|important; opacity: 0.5'"></div>
```

### 3.6 All three forms on one element

Static HTML, reactive `:class` / `:style`, and per-property
`class:` / `style:` directives are **designed to coexist** on the
same element. Each contributes independently to the final
attribute; the walker merges them in a deterministic order on
every reactive tick.

```html
<button class="btn btn-lg"                       <!-- static -->
        :class="{ loading: is_busy, err: has_err }"   <!-- reactive set -->
        class:active="is_active"                      <!-- single conditional -->
        class:disabled="!is_ready"
        style="font-weight: 600"                      <!-- static -->
        :style="{ 'background-color': theme_bg }"     <!-- reactive props -->
        style:color="theme_fg"                        <!-- single property -->
        style:--radius|important="computed_radius">
  Click
</button>
```

**Merge order — class**:

```
final_class = static_class ∪ serialise_class(:class) ∪ truthy class:<name>
```

Set union — token order is static → `:class` → directives in
source order. Duplicates stay (see §5 on dedup policy).

**Merge order — style**:

```
final_style = static_style ; serialise_style(:style) ; style:<name> in source order
```

Later declarations win per CSS cascade, so `style:` directives
override conflicting properties from `:style`, which override the
static `style`. `|important` on any contributor raises its
cascade weight per normal CSS rules.

**Fallthrough (RFC-010)** sits *around* this whole pipeline — the
outer author tag's `class` / `style` is merged onto the inner
template root's computed attribute, not re-parsed through the
serialisers.

This is the "use the right form for the job" pattern: static for
what never changes, directives for the 1-2 clear conditionals,
object/array for bulk or composed values. They don't fight each
other.

`|important` is parsed at serialise time, stripped from the written
value, and `!important` is appended to the property instead:

```
color:red !important;font-size:14px;opacity:0.5;
```

We deliberately keep `|important` on the value side (not a modifier
on the directive name like `:style.important`) because it composes
with object entries — authors can mark individual properties
without mode-switching the whole binding.

## 4. Semantics

### 4.1 `serialise_class` — array handling

```rust
fn serialise_class(v: &JsValue) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    push_class_tokens(v, &mut out);
    Some(out.join(" "))
}

fn push_class_tokens(v: &JsValue, out: &mut Vec<String>) {
    if v.is_null() || v.is_undefined() { return; }
    if let Some(s) = v.as_string() {
        for tok in s.split_ascii_whitespace() { out.push(tok.to_string()); }
        return;
    }
    if let Some(n) = v.as_f64() {
        if n != 0.0 && !n.is_nan() { out.push(n.to_string()); }
        return;
    }
    if Array::is_array(v) {
        let arr: Array = v.clone().unchecked_into();
        for i in 0..arr.length() { push_class_tokens(&arr.get(i), out); }
        return;
    }
    if v.is_object() {
        let obj: Object = v.clone().unchecked_into();
        for k in Object::keys(&obj).iter() {
            let truthy = Reflect::get(&obj, &k)
                .map(|val| !val.is_falsy())
                .unwrap_or(false);
            if truthy {
                if let Some(name) = k.as_string() { out.push(name); }
            }
        }
    }
}
```

`Array::is_array` check must precede `is_object` because arrays are
objects in JS. Everything else is falsy-skipped.

### 4.2 `serialise_style` — value coercion + `|important`

```rust
fn serialise_style(v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() { return Some(rewrite_important(&s)); }
    if !v.is_object() || Array::is_array(v) { return None; }
    let obj: Object = v.clone().unchecked_into();
    let mut out = String::new();
    for k in Object::keys(&obj).iter() {
        let Some(name) = k.as_string() else { continue };
        let Ok(val) = Reflect::get(&obj, &k) else { continue };
        let Some(val_s) = coerce_style_value(&val) else { continue };
        let (val_s, important) = split_important(&val_s);
        out.push_str(&name);
        out.push(':');
        out.push_str(&val_s);
        if important { out.push_str(" !important"); }
        out.push(';');
    }
    Some(out)
}

fn coerce_style_value(v: &JsValue) -> Option<String> {
    if v.is_null() || v.is_undefined() { return None; }
    if let Some(b) = v.as_bool() { return b.then(|| String::from("true")); }
    if let Some(n) = v.as_f64() { return Some(n.to_string()); }
    v.as_string()
}

fn split_important(s: &str) -> (String, bool) {
    if let Some(stripped) = s.trim_end().strip_suffix("|important") {
        return (stripped.trim_end().to_string(), true);
    }
    (s.to_string(), false)
}

fn rewrite_important(s: &str) -> String {
    // Inline string form — split on `;`, apply split_important per
    // declaration, re-join. Preserves declaration ordering and
    // existing whitespace except for the stripped marker.
    s.split(';')
        .filter_map(|decl| {
            let decl = decl.trim();
            if decl.is_empty() { return None; }
            let (lhs, imp) = split_important(decl);
            Some(if imp { format!("{lhs} !important") } else { lhs })
        })
        .collect::<Vec<_>>()
        .join(";")
}
```

Bool `true` renders as the string `"true"` only for `data-*`/`aria-*`
consistency in the plain path; for `style` values we treat `true`
as a "keep the key" marker on the rare author who writes
`{ 'font-style': italic_flag }` — skip if `false`, render literal
`"true"` otherwise. (Authors who want conditional properties use
`{ color: active ? 'red' : null }` — `null` skips cleanly.)

### 4.3 `class:<name>` + `style:<name>` directive registration

Two new directives in `crates/pocopine-core/src/directives/`:

- `class_prop.rs` (directive registered as `class:<name>`)
- `style_prop.rs` (directive registered as `style:<name>`)

Each installs a reactive `effect` that reads `expr` and mutates a
per-element **contribution list** stored on the element via the
same `__pp_*` private-key scheme used elsewhere. On every re-render
the serialiser recomputes the full attribute from:

```
final_class = serialise_class(:class_expr) + author_class_tokens
              + every truthy class:<name>
final_style = serialise_style(:style_expr) ; every style:<name> ;
              fallthrough_style
```

Appearance order is deterministic: the base attribute first, then
directive contributions in source order. This matches Svelte's
rule (directives win on conflicts for `style:`; class tokens are
set-like, so order doesn't matter functionally but we still fix
it for snapshot-test stability).

Shorthand parsing on the directive name:

- `class:foo` → contribution key `foo`, no modifiers.
- `style:foo` → contribution key `foo`, no modifiers.
- `style:--foo` → contribution key `--foo` (custom property).
- `style:foo|important` → contribution key `foo`, important flag set.
- `style:--foo|important` → custom property + important flag.

The parser splits on the first `|`; everything after is the
modifier set (only `important` is defined today, reserved space
for future additions). The `:` / `@` shorthand rewrite from RFC-020
does **not** touch `class:` / `style:` — those are fully-qualified
directive names, not shorthand prefixes. The walker's normalisation
only rewrites a leading single `:` or `@` into `pp-bind:` /
`pp-on:`; `class:` / `style:` are directive names in their own
right.

### 4.4 `sx!` Rust macro

Mirror of the existing `cx!` macro (RFC-010 §4.2), exported from
`pocopine::prelude`:

```rust
let style: String = sx!(
    "color: var(--fg)",
    self.disabled          => "opacity: 0.5",
    self.highlighted       => "background: var(--bg-hover)",
    format!("padding: {}px", self.pad_px),
);
```

Each arg is one of:

- String literal `"color: red"` — emit as-is.
- Condition → literal `cond => "opacity: 0.5"` — emit when truthy.
- `String` / `&str` expression — emit when non-empty.

Non-empty emissions join with `; ` (space preserved for
readability; browsers ignore the extra whitespace). A `!important`
marker in a literal or expression passes through verbatim — `sx!`
doesn't parse `|important`; use it when authoring the literal:

```rust
sx!("color: red !important", ...)
```

(The `|important` marker is a template-side shorthand; the Rust
macro writes raw CSS and doesn't need the translation layer.)

Implementation mirrors `__cx_push!` but with `; ` as the joiner
and no empty-guard on the joiner (style doesn't have the same
"collapse adjacent separators" expectation as class). One `String`
allocation; zero dependencies. ~40 lines including doc and test.

### 4.5 Fallthrough interaction

RFC-010's `apply_fallthrough_attrs` + `merge_space` continues to
receive the already-serialised string from the outer tag. The new
shapes (array, number-valued style, `class:<name>`, `style:<name>`)
are all resolved on the component's own root before fallthrough
runs, so the merge helper keeps its current pure-string contract.

### 4.6 SSR parity

All serialiser changes are string-local. Server-side rendering
(RFC-041 deploy targets) reuses the same `serialise_class` /
`serialise_style` entry points — no per-runtime branching needed.
The `class:<name>` / `style:<name>` directives resolve their
initial value server-side and contribute to the SSR'd attribute
string the same way `:class` does.

## 5. Edge cases

- **Duplicate class tokens.** `:class="['foo', {foo: true}]"` yields
  `"foo foo"`. Browsers dedupe visually; we don't pre-dedupe to
  avoid an O(n²) pass on hot re-renders. Authors who care can
  hoist into a computed expression.
- **`:class="false"` / `:class="null"`**. Already handled by the
  current code path (non-string, non-object → `None` → attribute
  removed). Stays the same.
- **Class tokens containing `|important`**. Class names aren't
  parsed for the modifier — only style values are. A class called
  `my-class|important` would be legal per this RFC but isn't a
  valid CSS class selector anyway.
- **`|important` mid-value**. `color: red blue|important` —
  `split_important` strips only a trailing `|important`, so this
  stays literal. Authors who want importance on each declaration
  mark each one.
- **Nested arrays with shared references**. No cycle detection;
  a proxy-referencing cycle inside `:class` would stack overflow.
  Hardening later if anyone hits it; not worth a tracked-set on
  every re-render.
- **Custom properties with colon in value**. `:style="{ '--url': 'url(\"x:y\")' }"`
  — `serialise_style` uses a single `:` between key and value and
  doesn't re-escape values. That matches CSS's tolerance (the
  browser parses the declaration, not us).

## 6. Implementation

Touches three files plus one new test module:

1. `crates/pocopine-core/src/directives/bind.rs` —
   - `serialise_class`: replace two-branch body with recursive
     `push_class_tokens`.
   - `serialise_style`: add array guard, `coerce_style_value`,
     `split_important`, `rewrite_important` helpers.
   - No changes to `run_bind`, `is_state_attr`, or the
     memoisation cell.

2. `crates/pocopine-core/src/directives/class_prop.rs` (new) +
   `style_prop.rs` (new) — register `class:<name>` / `style:<name>`
   directives; maintain per-element contribution lists; recompute
   the attribute on every effect tick.

3. `crates/pocopine-core/src/directives/mod.rs` —
   register the two new directives in the dispatch table.

4. `crates/pocopine/src/lib.rs` — add `sx!` + `__sx_push!`
   mirroring `cx!` (already lives here at lines 125–168).

Tests (`crates/pocopine-core/tests/bind_class_style.rs`, new):

- `class_array_mixed`: `['a', cond && 'b', {c: on, d: off}, null]` →
  truthy tokens joined.
- `class_nested_array`: `[['a', 'b'], ['c']]` → `"a b c"`.
- `style_custom_prop_number`: `{ '--count': 3 }` → `"--count:3;"`.
- `style_important_object`: `{ color: 'red|important' }` →
  `"color:red !important;"`.
- `style_important_string`: `"color: red|important; opacity: 0.5"`
  → `"color: red !important;opacity: 0.5"`.
- `class_fallthrough_with_array`: outer tag passes
  `class="outer"`, inner template binds `:class="['inner', flag && 'dyn']"`
  — root ends up `class="inner dyn outer"` (order per RFC-010).
- `class_prop_directive`: `<button class="btn" class:active="flag">`
  with `flag=true` renders `class="btn active"`, toggles on state
  change.
- `style_prop_directive`: `<div style:color="c" style:--gap="g">`
  + `style:color|important="c"` renders
  `color:red !important;--gap:1rem`.
- `class_prop_and_bind_combined`: `:class="['a']" class:b="flag"`
  renders `"a b"` when `flag=true`.
- `sx_macro_basic`: `sx!("color: red", cond => "opacity: 0.5", &expr)`
  joins with `; `, skips empty, matches expected snapshot.

## 7. Migration

Strictly additive. All existing tests stay green; the only shape
that changes output is a bound style value that was already broken
(number / bool → empty). No deprecations, no version bump required.

A follow-up codemod could convert
`:class="is_active ? 'active' : ''"` to
`:class="{ active: is_active }"` where beneficial, but that's a
style-guide job, not a mechanical one.

## 8. Example — PineAccordion item, before and after

**Before** (manual ternary, no `!important`, no custom-property
short form):

```html
<li class="pine-accordion-item"
    :class="open ? 'pine-accordion-item-open' : ''"
    :style="'max-height: ' + (open ? 'none' : '0') + '; transition: max-height 200ms'">
  <slot />
</li>
```

**After — object/array form:**

```html
<li :class="['pine-accordion-item', { 'pine-accordion-item-open': open }]"
    :style="{ 'max-height': open ? 'none' : '0', 'transition-duration': '200ms|important' }">
  <slot />
</li>
```

**After — directive form** (same element, equivalent output):

```html
<li class="pine-accordion-item"
    class:pine-accordion-item-open="open"
    style:max-height="open ? 'none' : '0'"
    style:transition-duration|important="'200ms'">
  <slot />
</li>
```

## 9. Example — Rust side with `cx!` + `sx!`

Computed getter on a component, consumed via `:class` / `:style`:

```rust
use pocopine::prelude::*;

impl PineButton {
    fn class_str(&self) -> String {
        cx!(
            "pine-btn",
            self.variant == "primary"     => "pine-btn-primary",
            self.variant == "destructive" => "pine-btn-destructive",
            self.size == "sm"             => "pine-btn-sm",
            self.disabled                 => "is-disabled",
            &self.user_extras,
        )
    }

    fn style_str(&self) -> String {
        sx!(
            "color: var(--fg)",
            self.disabled       => "opacity: 0.5",
            self.highlighted    => "background: var(--bg-hover)",
            format!("padding: {}px", self.pad_px),
        )
    }
}
```

Template:

```html
<button :class="class_str()" :style="style_str()">
  <slot />
</button>
```

Same effect as the inline object/array form, but centralised —
useful when the class set has enough branches that a template-level
expression gets illegible.
