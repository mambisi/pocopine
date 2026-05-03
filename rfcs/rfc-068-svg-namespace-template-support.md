# RFC 068 - SVG namespace template support

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Related** | [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 063](./rfc-063-directive-cleanup-vue-alignment.md) |
| **Supersedes** | - |

## 1. Summary

Pocopine templates should treat SVG as first-class structured DOM, not as
HTML strings. This RFC adds namespace-aware support for compiled SVG
subtrees, starting with the shape needed by Pine Charts:

```html
<svg viewBox="0 0 100 100">
  <g>
    <template pp-for="line in grid" pp-key="line.key">
      <line :x1="line.x1" :y1="line.y1" :x2="line.x2" :y2="line.y2"></line>
    </template>
  </g>
</svg>
```

The runtime must mount the repeated `<line>` rows as real SVG elements in the
SVG namespace. It must not use `pp-html` or `innerHTML` string injection as
the public strategy for framework-owned SVG.

## 2. Motivation

RFC 063 keeps `pp-html` for rare author-owned HTML injection, but explicitly
retires the Pine icon pattern where framework-owned SVG is stored as a string
and injected through `pp-html`. Charts have the same constraint as icons:
their SVG is framework-owned structure that needs typed DOM ownership,
directive bindings, and predictable namespace semantics.

The current compiled runtime handles `<svg>` itself correctly when it is
parsed as part of a normal HTML template, but `<template pp-for>` inside SVG
is different. Browsers do not expose that anchor as an
`HTMLTemplateElement`; it is an SVG/foreign element with live children. A
runtime that blindly casts every `pp-for` controller to `HTMLTemplateElement`
silently fails to install the list, leaving the static prototype children in
the DOM.

## 3. Design

### 3.1 SVG elements are native plan targets

The macro classifier treats common SVG element names as native DOM elements
for template-plan purposes. Directives such as `:x1`, `:d`, `pp-text`,
`pp-show`, `@click`, and `pp-ref` on SVG nodes are lifted the same way they
are lifted on native HTML nodes.

SVG elements are not custom component hosts. A `<g>` or `<line>` must not be
classified as a child component mount just because it is outside the HTML5
element list.

### 3.2 SVG template anchors are controller anchors

An SVG `<template pp-for>` is a Pocopine controller anchor, not an HTML
template. During install the runtime:

1. resolves the anchor as an `Element`;
2. preserves a cloned prototype of its first element child for fallback
   cloning;
3. clears the live anchor children so prototypes do not render or appear as
   rows;
4. marks the anchor hidden; and
5. inserts mounted rows before the anchor, matching HTML `<template pp-for>`
   ordering.

When the macro emitted a body function, the runtime prefers that body
function. The body function is responsible for installing compiled bindings
and listeners against the row scope.

### 3.3 Fragment body parsing is namespace-aware

Macro-emitted `pp-if`, `pp-for`, and `pp-teleport` body functions parse their
cleaned body HTML through a namespace-aware helper:

- HTML roots keep the existing `<template>.innerHTML` path.
- SVG roots are parsed inside an off-DOM `<svg>` wrapper created with
  `document.createElementNS("http://www.w3.org/2000/svg", "svg")`.

This ensures a lifted row body like `<line :x1="line.x1">` becomes an
`SVGLineElement`, not an HTML unknown element.

### 3.4 Scope

Phase 1 covers compiled SVG bindings and `pp-for` controller anchors inside
SVG. That is enough for SVG chart axes, grid lines, ticks, and repeated marks.

`pp-if` and `pp-teleport` inside SVG may reuse the same namespace-aware body
parser, but this RFC does not require dedicated SVG controller-anchor
semantics for them until a real component needs those shapes.

## 4. Non-goals

- No `pp-html`-based SVG rendering strategy for framework-owned SVG.
- No virtual-SVG renderer or canvas fallback.
- No SVG diffing engine beyond existing Pocopine directive effects.
- No special chart-only runtime path.
- No support for arbitrary XML namespaces beyond SVG in this phase.

## 5. Acceptance Criteria

- A component with `<template pp-for>` inside `<svg>` mounts repeated SVG
  rows in the SVG namespace.
- SVG prototype children under the controller anchor are cleared and do not
  count as rendered rows.
- Dynamic SVG attributes installed through `:attr` / `pp-bind:attr` update
  through the existing binding directive.
- Pine Charts can render grid lines and tick labels through template-owned
  SVG rather than `pp-html`.
- Browser tests cover the runtime behavior with `wasm-pack test`.

## 6. Open Questions

- Should `pp-if` inside SVG use the same controller-anchor abstraction now,
  or wait until the first real SVG component needs conditional SVG nodes?
- Should the SVG native-element list be exhaustive or intentionally limited
  to elements Pocopine components use?
- Should RFC 063's icon migration generate one component per SVG asset, or
  should it also allow generated static SVG fragments once this namespace
  machinery is stable?
