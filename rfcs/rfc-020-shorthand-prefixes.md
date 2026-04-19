# RFC 020 — `:attr` and `@event` shorthand prefixes

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-008-event-handler-args.md`](./rfc-008-event-handler-args.md), [`rfc-010-attribute-fallthrough.md`](./rfc-010-attribute-fallthrough.md), Vue / Alpine shorthand conventions |

## 1. Summary

Let template authors write `:class="..."` and `@click="..."` in
place of `pp-bind:class="..."` and `pp-on:click="..."`. Identical
semantics, 40-60 % less horizontal noise per line across Pine
templates.

```html
<!-- today -->
<button pp-bind:class="btn_class"
        pp-bind:disabled="busy"
        pp-on:click.prevent="submit"
        pp-on:keydown.escape="cancel">
  Save
</button>

<!-- after RFC-020 -->
<button :class="btn_class"
        :disabled="busy"
        @click.prevent="submit"
        @keydown.escape="cancel">
  Save
</button>
```

Rewrites happen **at walker bind time** (before directive
dispatch). Nothing else changes.

## 2. Non-goals

- **Replacing the canonical `pp-*` names.** Both forms stay
  supported forever. The long form is sometimes clearer in
  docs and stays the authoritative spelling in RFC prose.
- **New shorthand for `pp-if` / `pp-for` / `pp-show` / etc.**
  Those aren't attribute-level directives with an arg — their
  names are self-contained. No shorthand needed.
- **Custom shorthand registration.** A fixed two — `:` and `@`
  — covers the long tail; author-defined shorthand complicates
  tooling for little payoff.

## 3. Surface

| shorthand              | expands to              |
|------------------------|-------------------------|
| `:<name>[.<mod>...]`   | `pp-bind:<name>[.<mod>...]` |
| `@<name>[.<mod>...]`   | `pp-on:<name>[.<mod>...]`   |

Everything after the prefix — including modifiers — is preserved
verbatim. `:class.camel="..."` → `pp-bind:class.camel="..."`;
`@click.outside.stop="close"` → `pp-on:click.outside.stop="close"`.

## 4. Semantics

The walker's attribute-collection pass (`walker::bind`) normalises
attribute names before dispatching:

```
if name.starts_with(":") && name.len() > 1 { name = "pp-bind" + name; }
else if name.starts_with("@") && name.len() > 1 { name = "pp-on:" + &name[1..]; }
```

After that rewrite, everything flows through the existing
directive registry, same as if the author wrote the long form.

### 4.1 Reserved-character interactions

- **`<br/>`-shaped self-closers** and whatever else HTML allows
  near a `/` don't interact with the shorthand.
- **`::`** — no TypeScript-path / namespace meaning. If anyone
  wants `pp-bind:foo:bar`, they can still write the long form;
  the shorthand just rewrites the first `:` and keeps the rest.
- **`@@`** — rewrites to `pp-on:@something`, which is invalid at
  parse time. Same error the author would get from
  `pp-on:@...` directly.

### 4.2 Fallthrough

Shorthand-prefixed attrs are *not* treated as fallthrough (RFC-010)
— they unambiguously name a directive and get consumed by it.
Fallthrough only picks up plain HTML attributes like `class`,
`style`, `aria-*`, etc.

### 4.3 Slots and scoped slots

`<slot :ctx="row">` already exists via RFC-011; this RFC doesn't
change it. Slots use `:` for per-slot bindings, and that parser
already lives in `materialize_slot`. The normalisation here applies
only to **non-slot** elements.

### 4.4 `pp-teleport` + `pp-if` + shorthand

Plays fine. The normalisation is purely string-level; any directive
that reads attribute names (e.g. `pp-teleport` checking for
`pp-if`) will see the normalised spelling.

## 5. Implementation

Single change to `crates/pocopine-core/src/walker.rs`:

```rust
for i in 0..attrs.length() {
    let Some(a) = attrs.item(i) else { continue };
    let name = a.name();
    let rewritten = normalise_attr_name(&name);
    if rewritten.starts_with("pp-") {
        pp_attrs.push((rewritten, a.value()));
    }
}

fn normalise_attr_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        if !rest.is_empty() {
            return format!("pp-bind:{rest}");
        }
    }
    if let Some(rest) = name.strip_prefix('@') {
        if !rest.is_empty() {
            return format!("pp-on:{rest}");
        }
    }
    name.to_string()
}
```

Same helper also handles the slot's `:prop="..."` binding site
(`materialize_slot`) so that feature stays unchanged — the
difference is at the component-level attribute walk.

## 6. Edge cases

- **Empty shorthand** (`:=` or `@=`). `len() > 1` guard stops
  both from expanding; the attribute is ignored by the walker.
  Authors see the attribute sitting unused on the element.
- **Shorthand inside `pp-for` body.** Walker normalises every
  element's attributes regardless of enclosing scope.
- **HTML parsers that mangle `@`**. None do in practice — `@`
  is a valid HTML attribute-name character since HTML5. (Older
  IE had bugs here; we don't support IE anyway.)
- **SVG.** `:` appears in SVG attribute names (`xlink:href`). We
  only rewrite when the attribute starts with `:`; a leading
  namespace like `xmlns:foo` stays untouched. `xlink:href`
  specifically would start with `x`, not `:`, so no
  interaction.
- **Data attributes.** `:data-x` → `pp-bind:data-x`. Works.

## 7. Migration

Existing templates keep working — no breaking change. Authors
can mix `pp-bind:` and `:` freely. A follow-up could auto-convert
the long form in examples via a codemod, but the canonical shape
in RFC prose and core tests stays long-form for clarity.

## 8. Example — PineDialog, before and after

**Before:**

```html
<div role="dialog"
     pp-bind:aria-labelledby="$id + '-title'"
     pp-on:keydown.escape="close"
     pp-on:click.outside="close">
  <h2 pp-bind:id="$id + '-title'"><slot name="title" /></h2>
  <slot />
</div>
```

**After:**

```html
<div role="dialog"
     :aria-labelledby="$id + '-title'"
     @keydown.escape="close"
     @click.outside="close">
  <h2 :id="$id + '-title'"><slot name="title" /></h2>
  <slot />
</div>
```
