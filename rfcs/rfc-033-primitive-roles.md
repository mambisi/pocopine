# RFC 033 — Primitive roles: centralized default-element mapping

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 023](./rfc-023-pine-mvp.md), [RFC 019](./rfc-019-pp-as.md) |

## 1. Summary

Give `#[component]` a new `role = "..."` key that picks the
template root element based on the primitive's semantic role, and
stamps a `data-pine-role="<role>"` hook on the root for CSS
targeting. Mirrors [Reka UI's Primitive convention](https://github.com/unovue/reka-ui/tree/v2/packages/core/src/Primitive):
interactive primitives default to `<button>`, visual ones to
`<span>`, content panels to `<div>`, and so on — with `pp-as` as
the polymorphic escape hatch.

```rust
#[component(template = "PineAvatarRoot.poco", role = "visual")]
pub struct PineAvatarRoot { pub loaded: bool }
```

```html
<!-- PineAvatarRoot.poco -->
<root class="pine-avatar-root" :data-loaded="loaded">
  <slot></slot>
</root>
```

Renders as `<span class="pine-avatar-root" data-loaded="true"
data-pine-role="visual" pp-data="pine-avatar-root">…</span>`.

## 2. Why

Three pressure points converged on this design:

1. **Tag consistency.** Picking the right HTML element for each
   primitive (span/div/button/a/img) is role-based, not
   library-ambient. Reka UI and Radix both encode the decision in
   their `Primitive` component so every consumer is consistent.
   Without a central mapping we had to argue span vs div per
   component and got it wrong (sweep in 8e55a5b re-corrected
   Avatar* after a previous over-sweep).
2. **One place to change.** If we later want all interactive
   primitives to render as `<div role="button">` instead of
   `<button>`, changing one table beats touching 10+ `.poco`
   files.
3. **CSS hooks for surfaces.** With `data-pine-role="..."` on
   every primitive root, authors can write one selector that
   targets all clickable surfaces for effects like Material
   ripple, without depending on internal class names:
   `[data-pine-role="interactive"], [data-pine-role="surface"] { … }`.

## 3. Design

### 3.1 Role → tag table

| Role | Default tag | Baseline tweak | Intended for |
|---|---|---|---|
| `visual` | `<span>` | — | Visual/inline decorative roots (AvatarRoot, AvatarFallback, indicators) |
| `interactive` | `<button>` | `type="button"` auto-injected | SwitchRoot, CheckboxRoot, RadioGroupItem, TooltipTrigger |
| `link` | `<a>` | — | HoverCardTrigger |
| `media` | `<img>` | — (caller writes self-closing `<root/>`) | AvatarImage |
| `panel` | `<div>` | — | DialogContent, PopoverContent, TooltipContent, DropdownMenuContent |
| `scope` | `<div>` | — (caller may add `style="display:contents"`) | Pure scope holders with no rendered semantic |
| `surface` | `<div>` | — | Clickable cards, list items — CSS-hook alias of panel |

The role's *only* effect on the DOM is (a) picking the root tag,
(b) stamping `data-pine-role="<role>"` on the root, and (c) for
`interactive`, inserting `type="button"` when the template hasn't
supplied a `type` attribute. No surprising style or behavior gets
injected — authors own everything else.

### 3.2 Template convention

Role-annotated templates use `<root>...</root>` as the placeholder
root element. `root` is not a real HTML element, so every
occurrence in a `.poco` file is unambiguously the placeholder and
the rewrite is safe. Self-closing void roots (media) use `<root/>`.

Components without `role` don't use the placeholder — they keep
the classic path that just injects `pp-data` on whatever root
they write. Existing Pine components are unaffected until they
opt in.

### 3.3 Compile-time rewrite

The rewrite happens at template-registration time (runtime,
during `register_all()`), not in the proc-macro — templates are
already loaded through a runtime registry, so extending the
registrar is simpler than intercepting `include_str!`.

The `#[component]` macro:

1. Parses the `role = "..."` key; validates against the table
   (unknown role = compile error).
2. Emits a call to `pocopine::__private::compile_template(raw,
   name, Some((tag, role_name)))` in the generated `register()`.

At register time, `compile_template` walks the raw template:

1. Replaces every `<root>` / `<root ` / `<root/>` / `</root>` with
   the mapped tag.
2. Splices `data-pine-role="<role>"` and `pp-data="<name>"` into
   the first opening tag (skipping comments/doctypes).
3. For `button` tags without a `type="..."` attribute, additionally
   splices `type="button"`.

The parser is the same minimal quote-aware one used by
`inject_pp_data`, so attribute values containing `>` still work.

### 3.4 CSS hook examples

Authors get one selector surface for effects that span a role:

```css
/* Ripple on any clickable Pine surface */
[data-pine-role="interactive"],
[data-pine-role="surface"] {
  position: relative;
  overflow: hidden;
}

/* Focus ring for every interactive primitive */
[data-pine-role="interactive"]:focus-visible {
  outline: 2px solid var(--ring);
}

/* Panels get a default card shell */
[data-pine-role="panel"] {
  background: var(--panel-bg);
  border-radius: 8px;
}
```

## 4. Out of scope

- **Role-driven accessibility defaults.** Injecting `tabindex`,
  `aria-pressed`, etc. based on role is tempting but opens a
  rabbit hole — primitives vary enough that component-level
  wiring stays clearer.
- **Polymorphic `as` prop.** Reka's `asChild` / Pine's `pp-as`
  already cover runtime polymorphism. Roles pick the *default*;
  `pp-as` overrides at mount time.
- **Role hierarchy.** No inheritance between roles. Each role
  lives flat in the table; if two cases converge we'll collapse
  them by hand.

## 5. Migration

Opt-in. New primitives should pick a role. Existing primitives
will be migrated incrementally; this RFC ships with AvatarRoot
and AvatarFallback switched over as proof-of-concept. The role
attribute is additive — components without `role` keep behaving
exactly as before.
