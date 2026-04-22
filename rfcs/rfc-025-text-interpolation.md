# RFC 025 — Inline `{expr}` text interpolation

> **Superseded by [RFC-040](./rfc-040-text-interpolation-double-brace.md).**
> Current syntax is `{{expr}}`. The single-brace design below is
> kept for historical context.

| Field | Value |
|---|---|
| **Status** | Superseded by RFC-040 |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-012-expression-evaluator.md`](./rfc-012-expression-evaluator.md), [`rfc-024-expression-values.md`](./rfc-024-expression-values.md) |

## 1. Summary

Let authors write reactive text inline, mid-template:

```html
<p>Hello, {name}! You have {count} messages.</p>
<button>Delete ({count})</button>
```

…instead of the current `pp-text`-per-value shape:

```html
<p>Hello, <span pp-text="name"></span>!
   You have <span pp-text="count"></span> messages.</p>
<button>Delete (<span pp-text="count"></span>)</button>
```

Every template in `pine-demo` and the `examples/` apps already
contains a half-dozen `<span pp-text="…">` wrappers that only
exist because there was no inline shape. This RFC closes that
gap.

## 2. Non-goals

- **Attribute interpolation.** `class="foo-{variant}"` stays the
  `:class` / `pp-bind:class` / `cx!` territory — binding an
  attribute is a single-expression operation, already one
  directive call, and has different value-coercion rules
  (strings vs. booleans vs. numbers).
- **HTML interpolation.** No `{{ rawHtml }}` / `{@html …}`.
  Escapes are a footgun; authors who genuinely need raw HTML
  reach for `pp-html` (already exists) or a slot.
- **Expression-inside-expression.** `{outer({inner})}` is not
  supported; `{`/`}` are reserved delimiters inside text
  content. Authors who need braces in their text escape them:
  `\{` → literal `{`. (See §4.)
- **Multi-line expressions.** An `{expr}` block must start and
  end on the same text node; a newline inside the delimiters is
  a syntax error, same as inside `pp-text`.

## 3. Surface

### 3.1 Grammar

Every text node in the template is scanned for `{…}` pairs. A
text node's content becomes a sequence of **segments**:

- **Static** — everything outside `{…}`.
- **Dynamic** — the contents between a `{` and its matching
  `}`, parsed as a [RFC-012][rfc-012] expression.

Escapes:

- `\{` → literal `{` in the static text.
- `\}` → literal `}` in the static text.
- `\\` → literal `\`.

Any other `\X` is passed through unchanged (for forward compat).

An unmatched `{` with no closing `}` before end-of-node is a
syntax error logged via `console.error` with the original text;
the whole text node falls back to its raw content (no partial
binding). Same for a stray `}` with no opener.

### 3.2 Evaluation

Each dynamic segment is evaluated in the **enclosing scope's
proxy** — the same proxy `pp-text` on the segment's parent
would see. Every segment becomes its own text node in the DOM
with its own effect. That way:

- Static text between segments is preserved byte-for-byte
  (including whitespace, entities, newlines).
- A change to `name` re-runs only the `{name}` segment's
  effect — the `{count}` segment's text node is untouched.
- Sibling elements and their directives are unaffected.

### 3.3 Where interpolation runs

The scanner runs on **every text node the walker visits** after
its parent element's directives have bound, regardless of
whether the parent has any `pp-*` attributes. That covers:

- Plain HTML (server-rendered or literal) with interpolation.
- Children of `pp-if`, `pp-for`, `pp-teleport` fragments — the
  text nodes inside each cloned copy get their own scan.
- Slot content — scanned against the *calling* scope, same
  proxy slot directives resolve against.

Interpolation does **not** apply to:

- Attribute values. (Use `:attr`.)
- `<script>`, `<style>`, `<template>` content. The walker
  already skips those.
- Text inside a node that has `pp-text` set on its parent
  element — that directive owns the parent's textContent, so
  the scanner skips children it's about to overwrite anyway.

## 4. Examples

```html
<h1>{title}</h1>
<p>Total: {items.length} (showing {visible})</p>
<button type="button">{open ? 'Close' : 'Open'}</button>
<span>\{literal braces\}</span>
```

renders, after binding:

```html
<h1>My Feed</h1>
<p>Total: 12 (showing 5)</p>
<button type="button">Close</button>
<span>{literal braces}</span>
```

Updating `visible` re-runs only the one segment's effect; the
surrounding `"Total: "`, `" (showing "`, and `")"` text nodes
are plain DOM text and never re-evaluated.

## 5. Implementation notes

### 5.1 Split on walk

Immediately after `bind(el)` returns for an element, iterate its
**direct** child nodes (not descendants — `walk` recurses for
those). For each child that's a `Text` node:

1. Scan the node's `data` for `{…}` pairs.
2. If none, skip.
3. Otherwise, for each segment, insert a new `Text` node before
   the original. Give dynamic segments an empty initial string
   (the effect will set real content on first run).
4. Remove the original text node.
5. For each dynamic segment, install an effect that sets the
   new text node's `data` from the parsed expression, pinned to
   the **enclosing element** via `track_effect_on` so release
   follows the existing unmount path.

The scanner lives in a new module
`pocopine-core/src/directives/interp.rs` and is invoked from
`walker::bind` after the attribute pass finishes. Reusing the
directive directory keeps text binding and attribute binding
next to each other — both are shapes of "reactive DOM
write-through."

### 5.2 Error handling

A parse error on a segment behaves like `pp-text` errors: the
segment's text node is set to the original raw `{…}` substring
and a single `console.error` line points at the problem. One
bad segment never kills a whole text node's other segments.

### 5.3 Cost

Every text node is touched once on mount to check for `{`. A
byte-scan over UTF-8 is cheap; on a 500-element page with
~1500 text nodes the walker already pays O(text total length)
once for its own purposes. Measured cost: negligible; see the
`bench` regression harness.

### 5.4 `pp-text` is not deprecated

`pp-text` stays as the canonical "this element's entire
content is this one expression" shape — it sets `textContent`
rather than splitting, avoids the scan cost, and reads more
clearly when the expression is complex. Authors pick:

- `{foo}` when interpolating into surrounding text.
- `pp-text="foo"` when the element's whole body is one
  expression.

The demos + docs will standardise the `{expr}` form for inline
cases going forward.

[rfc-012]: ./rfc-012-expression-evaluator.md
