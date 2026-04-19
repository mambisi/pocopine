# RFC 010 — Attribute fallthrough + `cx!` macro

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md) §5 (props + slots) |

## 1. Summary

Two companion features that unblock ergonomic component libraries
(notably the upcoming `pine` crate):

1. **Attribute fallthrough.** Any HTML attribute written on a
   component tag that isn't a declared prop and isn't a `pp-*`
   directive forwards to the component's template root. `class` and
   `style` merge; other attrs (`id`, `aria-*`, `data-*`, `role`,
   `title`, …) overwrite whatever the template had.

2. **`cx!` macro.** Author-side utility for building class
   strings conditionally from Rust. Complementary to fallthrough —
   one is framework-level inheritance, the other is
   author-controlled composition.

```html
<!-- user -->
<pine-button class="mr-2" id="cta" aria-describedby="hint">Save</pine-button>
```

```html
<!-- PineButton.poco (component root) -->
<button type="button" class="pine-btn" pp-bind:class="variant_classes">
  <slot></slot>
</button>
```

The rendered DOM (after RFC-010):

```html
<button type="button"
        class="pine-btn pine-btn-primary mr-2"
        id="cta"
        aria-describedby="hint">Save</button>
```

## 2. Motivation

Component libraries live or die on "can the user tweak this one
thing without forking it." Today, if a Pine user needs to add
`mr-2` to a button, the button either hardcodes a prop for it
(absurd — infinite surface) or the user accepts that their
`class="mr-2"` silently overwrote the base class (also absurd).
Every Pine component would otherwise need a manual
`extra_class: String` boilerplate field + `pp-bind:class="..."`
merge gymnastics.

`class` and `style` are the two attrs that matter most — they
compose. Everything else (`id`, ARIA, `data-*`) replaces.

`cx!` is the other half: even after fallthrough handles user
extras, the component author still needs to build its own
internal classes per variant + state. A readable macro beats
hand-rolled `format!` chains.

## 3. Non-goals

* **Event listener fallthrough.** `pp-on:*` on a component tag does
  NOT forward to the template root; it attaches to the tag itself.
  Users wanting to listen to events from inside a component use
  `pp-on:<custom-event>` against events the component emits.
* **Nested fallthrough.** A component can't re-forward fallthrough
  attrs to a grand-child. Flat: user → component root.
* **Replacement merge strategies.** `class` appends, `style`
  appends with `;` separator, other attrs overwrite. No
  "replace mode" or "prepend mode" knobs.
* **Class-name deduplication.** `class="pine-btn pine-btn"` stays
  as-is if the user supplies a duplicate. Not our job.
* **`#[component(inherit_attrs = false)]`** opt-out in v0. Can add
  if a real need shows up; most components want the default.

## 4. Surface

### 4.1 Fallthrough

On any element whose tag is a registered component (via
`#[component]`), every static HTML attribute that:

* does **not** start with `pp-` or `__pp_`, and
* does **not** match a declared field of the component's state
  (after `kebab-case → snake_case` conversion),

is forwarded to the template root. Matching attrs continue to flow
into the prop path (existing behaviour).

Merge rules per attribute:

| Attribute | Action on the root |
|---|---|
| `class` | append, space-separated |
| `style` | append, `;`-separated |
| anything else | `root.setAttribute(name, value)` — overwrites |

### 4.2 `cx!`

A declarative-macro helper exported from `pocopine::prelude`:

```rust
let class: String = cx!(
    "pine-btn",
    self.variant == "primary"     => "pine-btn-primary",
    self.variant == "destructive" => "pine-btn-destructive",
    self.size == "sm"             => "pine-btn-sm",
    self.size == "lg"             => "pine-btn-lg",
    self.disabled                 => "is-disabled",
    &self.icon_class,
);
```

Each comma-separated arg is one of:

* **String literal** `"foo"` — always emit.
* **Condition → string** `cond => "foo"` — emit when `cond` is
  truthy. `cond` is any boolean expression.
* **`&str` / `String` expression** `expr` — emit when non-empty.
  Lets authors splice in a field or computed value.

Empty strings are dropped. Non-empty emissions join with a single
space. One final `String` allocation.

## 5. Semantics

### 5.1 Evaluation order

Fallthrough processing happens after `mount_component` clones the
template into the tag's innerHTML and resolves the template root,
but before the walker recurses into the root's subtree. This means:

* The template's own `class` / `style` are already set when the
  forward hits.
* `pp-bind:class="..."` on the template root still wins for
  dynamic updates — fallthrough class is a one-shot write into the
  initial attribute; `pp-bind` rewrites on change and thus clobbers
  fallthrough additions. **This is a known sharp edge** — Pine
  components that want fallthrough to survive reactive updates
  should include the user's extras in their `cx!`:

  ```rust
  cx!(
      "pine-btn",
      self.variant == "primary" => "pine-btn-primary",
      &self.user_extras,   // component has read them from fallthrough
  )
  ```

  Or simpler: don't use `pp-bind:class` on the same element
  fallthrough targets; compute variants as a `pp-bind:data-variant`
  and let CSS select.

  v1 improvement (separate RFC): make `pp-bind:class` aware of
  fallthrough, preserving user extras automatically.

### 5.2 Matching declared fields

The walker already loops attrs and calls
`state.set(kebab_to_snake(name), val)`. A prop is "declared" iff
the generated `ComponentState::keys()` includes the
`kebab_to_snake`-ed attribute name. Fallthrough uses the same
check — if the field exists, it's a prop; otherwise it's a
fallthrough.

### 5.3 Slot content interaction

Fallthrough targets the template root, **not** the captured slot
content. If the component's template is `<div><slot></slot></div>`
then fallthrough goes onto the `<div>`, not onto the user's
children.

### 5.4 Non-string props

Declared props can be JSON (`items='[...]'`), numbers, bools, etc.
Those still flow through the existing prop path unchanged —
fallthrough only applies to attrs that weren't matched to a field.

## 6. Examples

### 6.1 Basic variant + extras

```rust
#[component(style = "pine-button.css")]
pub struct PineButton {
    pub variant: String,  // "primary" | "destructive" | "ghost"
    pub size: String,     // "sm" | "md" | "lg"
    pub disabled: bool,
}

#[handlers]
impl PineButton {}

impl PineButton {
    fn variant_classes(&self) -> String {
        cx!(
            self.variant == "primary"     => "pine-btn-primary",
            self.variant == "destructive" => "pine-btn-destructive",
            self.variant == "ghost"       => "pine-btn-ghost",
            self.size == "sm" => "pine-btn-sm",
            self.size == "lg" => "pine-btn-lg",
            self.disabled     => "is-disabled",
        )
    }
}
```

```html
<!-- PineButton.poco -->
<button
  type="button"
  class="pine-btn"
  pp-bind:class="variant_classes"
  pp-bind:disabled="disabled"
>
  <slot></slot>
</button>
```

Usage:

```html
<pine-button variant="primary" class="mr-2" aria-label="save">
  Save
</pine-button>
```

Output: `<button type="button" class="pine-btn pine-btn-primary mr-2" aria-label="save">Save</button>`.

### 6.2 ARIA / data-* pass-through

```html
<pine-dialog data-testid="confirm-dialog" aria-labelledby="title">
  …
</pine-dialog>
```

Both the `data-testid` and `aria-labelledby` end up on the dialog
template's root — no code change in `PineDialog` needed.

## 7. Implementation

### 7.1 Fallthrough

`walker::mount_component` currently:

```rust
apply_static_props(el, &scope);  // flows every non-pp-* attr into state.set
```

Split into two loops:

1. **Declared pass.** For each attr, if `kebab_to_snake(name)` is
   in `state.keys()`, call `state.set(field, value)`.
2. **Fallthrough pass.** For each remaining attr, apply to the
   template root:
   * `class` → `root.setAttribute("class", merge(existing, val))`,
   * `style` → same with `;` separator,
   * anything else → `root.setAttribute(name, val)`.

Skip `pp-*` and `__pp_*` throughout (both passes).

The template root is the one `mount_component` pins the scope on
via `first_element_child(el)`; the fallthrough step runs right
after that pin.

### 7.2 `cx!`

A `macro_rules!` in `pocopine-macros` (or inline in `pocopine`).
Sketch:

```rust
#[macro_export]
macro_rules! cx {
    ( $( $arg:tt )* ) => {{
        let mut __out = String::new();
        $crate::cx_push!(__out; $($arg)*);
        __out
    }};
}

#[macro_export]
#[doc(hidden)]
macro_rules! cx_push {
    ($out:ident;) => {};
    ($out:ident; $cond:expr => $cls:expr, $($rest:tt)*) => {
        if $cond {
            if !$out.is_empty() { $out.push(' '); }
            $out.push_str($cls);
        }
        $crate::cx_push!($out; $($rest)*);
    };
    ($out:ident; $cond:expr => $cls:expr) => {
        if $cond {
            if !$out.is_empty() { $out.push(' '); }
            $out.push_str($cls);
        }
    };
    ($out:ident; $lit:literal, $($rest:tt)*) => {
        if !$out.is_empty() { $out.push(' '); }
        $out.push_str($lit);
        $crate::cx_push!($out; $($rest)*);
    };
    ($out:ident; $lit:literal) => {
        if !$out.is_empty() { $out.push(' '); }
        $out.push_str($lit);
    };
    ($out:ident; $expr:expr, $($rest:tt)*) => {
        let __s: &str = &$expr;
        if !__s.is_empty() {
            if !$out.is_empty() { $out.push(' '); }
            $out.push_str(__s);
        }
        $crate::cx_push!($out; $($rest)*);
    };
    ($out:ident; $expr:expr) => {
        let __s: &str = &$expr;
        if !__s.is_empty() {
            if !$out.is_empty() { $out.push(' '); }
            $out.push_str(__s);
        }
    };
}
```

Sketch only — the actual macro may end up as a proc-macro if
declarative-macro ambiguity (cond-expr vs. `&str` expression) bites.
`proc-macro2` / `syn` give cleaner disambiguation; a small
proc-macro is fine.

## 8. Edge cases

* **`class` explicitly set to empty.** `<pine-button class="">`
  writes an empty string into the fallthrough; nothing appends.
  Base class stays.
* **`style` without trailing semicolon in the template.** Merge
  strips trailing whitespace + adds `; ` before appending the
  user's string.
* **User attribute collides with a pp-bind target.** `pp-bind:class`
  re-runs on reactive changes and overwrites the initial fallthrough
  class. See §5.1 — this is documented, not fixed in v0.
* **Same attr appears multiple times** (impossible in HTML; the
  parser keeps the last).
* **Attribute with uppercase** — HTML parses attribute names
  case-insensitively and lowercases them. No special handling.
* **Boolean attrs** (`disabled`, `hidden`, `readonly`). The
  `coerce_attr_value` function already turns `"true"` into
  `JsValue::TRUE`; fallthrough uses raw string value so
  `disabled=""` or `disabled` both land on the root as-is.

## 9. Alternatives considered

* **Explicit `$attrs` object on the template root** (Vue's
  `v-bind="$attrs"` when `inheritAttrs: false`). More control,
  more ceremony, and 95% of components want the default inheritance
  anyway.
* **A dedicated `pp-extra-class` attribute.** Solves only the
  `class` case; every other pass-through attr would need its own
  directive. Doesn't scale.
* **Merge rules per-attribute via a table the author writes** —
  would let authors choose `replace` vs `append` per attr.
  Not worth the config surface for v0 — `class`/`style` append,
  everything else replaces, matches every other framework.
* **Use `data-*` for extras and forbid top-level fallthrough.**
  Pushes the ugly onto every user of every Pine component. No.

## 10. Out of scope (future)

* **Fallthrough-aware `pp-bind:class`** — preserve user extras
  across reactive rebuilds without the author having to thread
  them through.
* **Per-component opt-out**
  (`#[component(inherit_attrs = false)]`).
* **Multi-root component fallthrough** — when the template has
  more than one root, decide which receives the extras (or require
  an explicit `<slot>` / marker to pick).
* **`cx!` Tailwind-merge mode** — dedupe conflicting Tailwind
  utilities (`p-2` + `p-4` → last wins). Useful for Pine users
  stacking overrides; possibly shipped as a separate macro
  (`cx_twm!`) or via a Tailwind-merge crate integration.
