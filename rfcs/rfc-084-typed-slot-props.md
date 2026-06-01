# RFC 084 - Typed slot props

| Field | Value |
|---|---|
| **Status** | Accepted (Phases 1–2 compound-side `props = T` validation landed; Phase 3 caller-side `pp-let` checking + compile-fail tests pending) |
| **Author** | pocopine team |
| **Created** | 2026-05-26 |
| **Related** | [`rfc-044-props.md`](./rfc-044-props.md), [`rfc-049-slot-contracts.md`](./rfc-049-slot-contracts.md), [`rfc-050-template-ast.md`](./rfc-050-template-ast.md), [`rfc-081-component-handle-refs.md`](./rfc-081-component-handle-refs.md) |
| **Supersedes** | - |

## 1. Summary

Scoped-slot publications are stringly-typed today. The compound's
template publishes string-keyed JS values via
`<slot :name="name" :status="status">`, the caller reads them via
`<template pp-slot="default" pp-let="row">` + `row.name` /
`row.status`, and nothing in between checks that keys match,
value types are right, or that publication covers every field
the caller reads. Typos surface as silent `undefined`s.

This RFC extends `#[slot(...)]` with **one** optional argument —
`props = T` — that types the publication. `T` is declared with
the existing `#[derive(Props)]` (the same wire surface the
parent→child prop pipeline uses), and the macro verifies that:

1. The compound's `<slot :foo="…">` publications cover every
   `#[prop]` field on `T`.
2. The caller's `pp-let` binding is typed as `T`; `row.X` reads
   resolve against `T`'s prop set at macro time.

There is **no separate `iterates` argument and no wrapper type**.
The macro dispatches **static vs iterated by template context**:
when the `<slot>` element sits inside a `pp-for`, the iteration
binding is auto-published as the slot binding and the Rust
typechecker verifies the iteration item matches `T`. When the
slot sits outside any `pp-for`, the author writes explicit
`<slot :foo="…" :bar="…">` publications. One rule, one arg, two
template contexts.

```rust
#[derive(Default, Props, Serialize, Deserialize)]
pub struct UploadHeaderProps {
    #[prop] pub queue_size: usize,
    #[prop] pub all_done: bool,
}

#[component(template = "UploadRoot.poco", role = "scope")]
#[slot(name = "header", props = UploadHeaderProps)]   // static slot
#[slot(name = "row",    props = UploadFile)]          // iterated slot — binding type
#[slot(name = "footer", props = UploadFooterProps)]
pub struct UploadRoot { /* ... */ }
```

## 2. Motivation

PR #127 shipped the `03-composition.md` "Why doesn't my pp-text
work inside a compound's slot?" section that nailed down the
existing scoped-slot pattern: `<slot :LHS="RHS">` + `pp-let="X"`,
where LHS is the caller-visible key and RHS is an expression in
the compound's scope. The pattern works end-to-end, but the same
PR's review surfaced that the binding edges are entirely
string-keyed:

- A typo on the compound side (`<slot :nmae="name">`) parses
  cleanly and publishes an `nmae` key the caller's `row.name`
  silently misses.
- A typo on the caller side (`row.nmae`) reads `undefined` with
  no diagnostic.
- A field-type mismatch (compound publishes `status: String` but
  caller reads it via `pp-show="row.status"`) is evaluated as JS
  truthiness without warning.
- Renaming a published key on the compound silently breaks every
  caller reading the old key.

The framework treats every other binding edge as typed at the
macro boundary — `#[component]` props go through
`#[derive(Props)]` + `PropValue`, handler arguments go through
`FromHandlerArg`, event names go through compile-time directive
validation. Slot publications are the one major reactive edge
still stringly-typed. This RFC closes that gap by reusing the
existing Props surface rather than inventing a parallel
`Ctx`/`SlotProps`/`SlotContext` vocabulary or wrapper type.

## 3. Goals

1. **Compile-time validation of slot publication shape.** The
   compound's `<slot :foo=…>` publications are checked against
   the declared `Props` type at macro expansion. Missing key →
   compile error. Extra key not on the type → compile error
   (with a hint to add it to the props struct or remove the
   stray publication).
2. **Compile-time validation of caller access** to the extent
   reasonable. `pp-let="row"` + `row.foo` in the caller's
   `<template pp-slot>` resolves `foo` against the declared
   props type; an unknown key is a compile error pointing at
   the props struct.
3. **Reuse the existing Props plumbing.** Same
   `#[derive(Props)]`, same `PropValue` leaf-type constraints,
   same `#[prop(flatten)]` for nested structs. No new traits,
   no new derives, no wrapper type.
4. **Single decl per slot, single rule for the macro.** No
   parallel `iterates =`, no `SlotContext<T>` /
   `PropsIterator<T>` wrappers. The decl says one thing: the
   binding type. The template's `pp-for` context — which the
   macro already walks — decides static vs iterated.
5. **Backwards compatible.** Existing untyped `#[slot(default)]`
   and `#[slot(name = "...")]` keep working unchanged. Adding
   `props = T` is opt-in.

## 4. Non-goals

1. **Replacing the `<slot :LHS="RHS">` syntax.** The publication
   syntax stays the same. The RFC only adds a *type* to validate
   it against.
2. **Inline type annotations in templates.** Per the `.poco`
   format rule, types live in Rust. No `<slot :name@type<String>>`
   or similar.
3. **A wrapper type to mark iteration.** Considered (`Each<T>`,
   `ItemOf<T>`, `PropsIterator<T>`) and rejected; see §8.2.
4. **A `iterates = field` parallel macro arg.** Considered and
   rejected; see §8.1.
5. **Built-in iteration metadata** (`$index`, `$last`,
   `$first`) as fields on the props type. Authors define their
   own Props struct flattening the item plus the metadata and
   publish explicitly — see §5.4.
6. **Generic slots** (a typed slot whose props type is generic
   over the compound's own generics). Ship monomorphic slots
   first; revisit in a follow-up RFC.
7. **Caller-side destructuring sugar** (e.g.
   `pp-let="{ name }"`). `pp-let="row"` + `row.name` remains
   the v1 form.
8. **Cross-instance reach via slot props.** Use RFC 081's
   `pp-ref` + `$ref.name.X` for that — slot props are scoped to
   the slot content, by design.
9. **Runtime type checking.** The publication is a JS object on
   the wire; runtime cost stays zero. Validation is macro-time.

## 5. Design

### 5.1 The single arg

`#[slot(name = "...", ...)]` accepts one new optional argument:

| Argument | Meaning |
|---|---|
| `props = Type` | The binding type the caller's `pp-let="X"` exposes. `Type` must derive `Props`. |

The macro dispatches **static vs iterated by the template-AST
position of the `<slot>` element**:

- The `<slot>` is **not** an descendant of any `pp-for` → **static
  mode**. The compound's template MUST publish explicitly via
  `<slot :LHS="RHS">` attributes covering every `#[prop]` field
  on `Type`.
- The `<slot>` **is** a descendant of a `pp-for="X in expr"` →
  **iterated mode**. The macro auto-emits a publication where
  the slot binding is `X` itself. No explicit `:LHS="RHS"` on
  `<slot>` is required (the macro errors if any are present —
  the two modes don't mix; see §5.4). The Rust typechecker
  verifies that `X`'s type matches `Type` via an emitted type
  assertion (see §5.6).

That's the entire rule. Everything else falls out from "single
arg + template-AST dispatch."

### 5.2 Static-mode example

```rust
// UploadRoot.rs
#[derive(Default, Props, Serialize, Deserialize)]
pub struct UploadHeaderProps {
    #[prop] pub queue_size: usize,
    #[prop] pub all_done: bool,
}

#[component(template = "UploadRoot.poco", role = "scope")]
#[slot(name = "header", props = UploadHeaderProps)]
pub struct UploadRoot {
    pub queue_size: usize,
    pub all_done: bool,
    /* ... */
}
```

```html
<!-- UploadRoot.poco -->
<div>
  <slot name="header" :queue_size="queue_size" :all_done="all_done"></slot>
  <!-- ... -->
</div>
```

```html
<!-- caller -->
<upload-root>
  <template pp-slot="header" pp-let="hdr">
    <h2>{{ hdr.queue_size }} files</h2>
    <span pp-show="hdr.all_done">All uploaded</span>
  </template>
</upload-root>
```

The macro validates:

- Every `#[prop]` field on `UploadHeaderProps` is published by a
  matching `:foo=…` on the `<slot name="header">` element.
  Missing one → compile error: *"slot 'header' publication
  doesn't cover prop `all_done` declared on
  `UploadHeaderProps`."*
- Each published `:foo="expr"` resolves `foo` to a declared
  `#[prop]` field on `UploadHeaderProps`. Extra key → compile
  error: *"slot 'header' publishes `notes` which isn't a prop
  on `UploadHeaderProps`; add `#[prop] pub notes: …` or remove
  the publication."*
- Each `hdr.X` read inside the caller's
  `<template pp-slot="header">` resolves `X` against
  `UploadHeaderProps`'s prop set; unknown keys → compile error.

### 5.3 Iterated-mode example

```rust
// UploadRoot.rs
#[component(template = "UploadRoot.poco", role = "scope")]
#[slot(name = "row", props = UploadFile)]
pub struct UploadRoot {
    pub files: Vec<UploadFile>,
    /* ... */
}
```

```html
<!-- UploadRoot.poco — slot inside pp-for; publication is auto-emitted -->
<ul>
  <li pp-for="file in files">
    <slot name="row"></slot>
  </li>
</ul>
```

```html
<!-- caller -->
<upload-root>
  <template pp-slot="row" pp-let="file">
    <span pp-text="file.name"></span>
    <span pp-show="file.status == 'uploading'">
      <span pp-text="file.progress_label"></span>
    </span>
  </template>
</upload-root>
```

The macro:

- Walks the template AST and locates the `<slot name="row">`
  element.
- Sees it's a descendant of `<li pp-for="file in files">`.
- Dispatches to iterated mode: auto-emits the publication of
  `file` as the slot binding.
- Emits a Rust type assertion that verifies
  `typeof(file) == UploadFile` (the props type). If the
  iteration variable's type doesn't match, Rust errors point
  at the emitted assertion, the `#[slot]` attr, and the
  `pp-for` site.
- Errors at macro time if any `<slot :foo="…">` attrs are
  present on a `<slot>` in iterated mode — the two modes are
  mutually exclusive at the per-`<slot>`-element level (see
  §5.4).
- On the caller side, `pp-let="file"` binds an object whose
  shape is `UploadFile`'s `#[prop]` field set. `file.bogus` →
  compile error.

`UploadFile` must `#[derive(Props)]` — i.e., be defined with
`#[derive(Default, Props, Serialize, Deserialize)]` and `#[prop]`
on the leaves the caller can read. If it doesn't, the macro
errors: *"`UploadFile` must `#[derive(Props)]` to serve as a
slot publication type; see RFC 044."*

### 5.4 Iteration with metadata — back to static mode

Authors who need iteration metadata (`$index`, `$last`,
`$first`, derived labels) declare a Props struct that flattens
the iteration item plus the extra fields and publish explicitly
— that's static mode, and it's the SAME rule as §5.2:

```rust
#[derive(Default, Props, Serialize, Deserialize)]
pub struct UploadRow {
    #[prop(flatten)] pub file: UploadFile,
    #[prop] pub index: usize,
    #[prop] pub is_last: bool,
}

#[slot(name = "row", props = UploadRow)]
```

```html
<ul>
  <li pp-for="file in files">
    <slot name="row" :file="file" :index="$index" :is_last="$last"></slot>
  </li>
</ul>
```

The macro sees the `<slot>` is inside a `pp-for` AND has explicit
publications, which would be a mode mix; instead the rule is:
**presence of any `:LHS=` on the `<slot>` element forces static
mode**, regardless of whether the element sits inside a
`pp-for`. The author is opting into "I'm publishing explicitly,
the iteration variable is just one of the inputs."

This gives a single role-split: **iterated mode is sugar for
"just publish the iteration item, no metadata"** (zero `:LHS=`
on `<slot>`); **static mode is everything else** (one or more
`:LHS=` on `<slot>`). Same `props = T` arg in both cases.

### 5.5 Validation rules

The macro emits checks at component expansion time. Concrete
rule set:

**On the compound side** (the component declaring `#[slot]`):

1. `props = T` requires `T: pocopine::__private::Props` —
   verified via an emitted `const _: () = { fn _assert<T:
   Props>(){} _assert::<#T>(); }`-style boundary so the
   diagnostic points at the `#[slot]` attribute, not at a
   downstream macro.
2. AST scan locates every `<slot name="X">` element in the
   compound's template. (Default slot — `<slot>` with no name
   — is treated as `name = "default"`.)
3. **Mode resolution per element**: if the element has any
   `:LHS=…` attributes → static mode. Else if the element is a
   descendant of `pp-for="VAR in EXPR"` → iterated mode. Else
   → static mode with an empty publication set (errors if T
   has any `#[prop]` fields the publication doesn't cover).
4. **Static mode validation** (set-equality on publications):
   - Missing publication for a `#[prop]` field on T → error
     per missing field.
   - Extra publication for a key not on T → error per stray
     key.
5. **Iterated mode validation**:
   - The `<slot>` element MUST have zero `:LHS=…` attrs (else
     mode is static — see rule 3).
   - The enclosing `pp-for`'s iteration variable's type MUST
     equal T. This is verified by emitting a Rust assertion
     like `let _: T = #iter_var.clone();` (or a `let _: &T =
     &#iter_var;`) into a hidden function the macro generates,
     so the Rust typechecker reports the mismatch with the
     emitted assertion's span on one side and the `#[slot]`
     attr on the other.
6. **Multiple `<slot name="X">` elements with the same X**:
   each is mode-resolved independently and validated; the
   per-slot props type stays the same. The fallback-slot
   pattern (a second `<slot name="X">` deeper in the tree as a
   fallback) keeps working.

**On the caller side** (the component instantiating the
compound and writing `<template pp-slot="…" pp-let="…">`):

1. The caller's `pp-let` binding (`row`, `hdr`, `file`, etc.)
   is registered with the props type from the resolved
   `#[slot(name = "X", props = T)]` of the child component
   (looked up via `uses = […]` per RFC 060, or via type
   inference when the child component is the immediate
   target).
2. Every `binding.X` read inside the `<template pp-slot>`
   resolves `X` against the bound type's `#[prop]` field set;
   unknown keys → error.
3. Leaf types are checked transitively for compatibility with
   the directive consuming them (e.g., `pp-show="row.flag"`
   requires `flag` to be `bool`-coercible; this is already the
   pp-show contract and isn't new).
4. Caller-side typing is best-effort in v1: it requires the
   caller's component to declare `uses = [UploadRoot]` (the
   existing typed-tag opt-in). Without `uses`, the caller is
   untyped (same as today) and runtime resolution applies.

### 5.6 Type-checking the iteration item via Rust

The `pp-for` iteration variable's type isn't always statically
inferrable from the template alone — `pp-for="X in foo.bar"`
might involve method calls (no, see RFC 012 — but field paths
exist). Rather than build a parallel type-inference pipeline in
the macro, the macro emits a Rust assertion and lets rustc do
the work:

For a `<slot name="row">` sitting inside `pp-for="file in
files"` declared `#[slot(name = "row", props = UploadFile)]`,
the macro emits — into the component's generated mount body or
a hidden `const _: fn()` boundary — code morally equivalent to:

```rust
const _: fn(&Self) = |this: &Self| {
    // Assert the iteration variable's resolved type matches
    // the declared props.
    let _: &UploadFile = match this.files.iter().next() {
        Some(file) => file,
        None => unreachable!(),
    };
};
```

Rust's typechecker emits the mismatch error if `files`'s
element type isn't `UploadFile`. The diagnostic includes both
the emitted assertion's span and the `#[slot]` attr's span via
`syn::Error::new_spanned`.

For `pp-for` over an expression more complex than a bare field
path — the macro emits the same assertion but parameterized on
the parsed pine-expr's evaluated type. The simplest v1
implementation handles bare field paths only; complex
expressions fall back to "static mode required" with a clear
error message ("`pp-for` over a complex expression isn't
supported for iterated slot inference; use static mode with
explicit publications").

### 5.7 Wire encoding

Slot publications today are JS objects (built lazily inside
`SlotScope::get` per `crates/pocopine-core/src/slot_scope.rs`).
Nothing in this RFC changes the wire shape — typing is
macro-only. The runtime continues to:

- Build the object from `(prop, path)` pairs in
  `SlotScope::bindings`.
- Resolve each `path` against the compound's proxy
  (`bind_source`).
- Fall through to the caller's proxy for non-binding keys.

For iterated mode, the macro emits exactly one `(prop, path)`
pair internally — `(VAR, VAR)` — so the runtime path is
identical to static mode with a single publication. The author
just doesn't write that publication; the macro does.

The `Props` trait already exposes `prop_leaves()` and the
serialization layer the macro uses to emit `:foo=` ↔
`PropValue` glue elsewhere; the slot-publication path reuses
that machinery.

### 5.8 Backwards compatibility

Every existing `#[slot]` site keeps working. The new arg is
optional; absent it, the macro's behavior is identical to v0
(no publication-shape validation, no caller-side prop checking).

Migration is one struct definition + one attribute argument per
slot:

```rust
// Before
#[slot(name = "row")]

// After (typed, opt-in)
#[slot(name = "row", props = UploadFile)]
```

Apps that don't migrate continue to compile and run. The
opinionated docs (`02-state.md`, `03-composition.md`) recommend
the typed form once it lands.

## 6. Implementation phasing

### Phase 1 — `props = T` + static-mode validation

- Extend `slot::SlotArg` parser in
  `crates/pocopine-macros/src/slot.rs` with a `Props(Path)`
  variant.
- Extend `SlotDecl` to carry the optional props type.
- Macro emits the `T: Props` boundary so the diagnostic
  anchors at the `#[slot]` attribute on non-`Props` types.
- AST scan of the component template finds the matching
  `<slot name="X">` element(s); collect `:foo=…` publications.
- Set-equality check against `T`'s prop field set; emit
  per-key compile errors for missing/extra.
- One pine primitive (suggested: a Tabs trigger or similar
  short-publication-list compound) migrated as the inaugural
  consumer.

### Phase 2 — Iterated mode via template-AST dispatch

- Extend the per-`<slot>`-element mode-resolution in the macro
  (per §5.5 rule 3) to detect "no `:LHS=` attrs AND inside a
  `pp-for`."
- AST scan walks ancestors of `<slot name="X">` looking for
  the nearest `pp-for` ancestor; resolve the iteration
  variable name from `pp-for="VAR in EXPR"`.
- Macro auto-emits the `(VAR, VAR)` publication pair the
  runtime needs.
- Macro emits the Rust type assertion (per §5.6) so rustc
  validates the iteration item's type matches `props = T`.
- `PineUploadRoot` (or the next iterated primitive that
  lands) migrated as the second consumer.

### Phase 3 — Caller-side type checking

- Hook into the existing RFC 060 `uses = [...]` consumer-side
  scan in `crates/pocopine-macros/src/lib.rs` (search for
  `slot_assertions`).
- For each `<template pp-slot="X">` in a typed-consumer
  template, look up the child's `#[slot(name = "X", props =
  T)]` and bind `pp-let="ID"` to `ID: T`.
- Validate `ID.field` reads against `T`'s prop set.
- Emit `compile_error!` for unknown keys, pointing at the
  caller's `.poco` line *and* at the declared props type.

### Phase 4 — Docs + migration sweep

- Update `docs/components/03-composition.md` "Slots" section
  with the typed form alongside the existing untyped form;
  recommend the typed form as the default going forward.
- Sweep pine primitives (`pine/src/tabs/`, `pine/src/combobox/`,
  upload primitive from PR #127 follow-up, etc.) migrating to
  `props = T`.
- Cross-link from `docs/poco/04-expressions.md` to the
  typed-slot pattern (it's the formal version of the
  "compute Rust-side, expose by name" reflex).

## 7. Open questions

1. **Template-AST scan for the `pp-for` ancestor.** RFC 050's
   element walker already supports walking ancestors via the
   AST. Pin the exact rule for which `pp-for` "owns" a `<slot>`
   when multiple `pp-for`s are nested — the nearest enclosing
   one, or the outermost? Nearest is the cleaner default;
   confirm before Phase 2.
2. **Default slot in iterated position.** `<slot>` (no name)
   inside a `pp-for` is the most common shape for list
   compounds. Treat it identically to a named slot at the
   AST level (mode resolution applies the same way).
3. **`pp-for` over complex expressions.** §5.6 punts on
   anything other than a bare field path. What's the right
   error message and what does the user do? Two options:
   (a) fail with "use static mode," (b) allow the macro to
   try harder via a stricter Rust type assertion. Lean (a)
   for v1.
4. **Generic compounds.** A `DataTable<R>` with
   `#[slot(name = "row", props = R)]` where `R` is a generic
   on the struct — does this work? The `T: Props` boundary
   becomes `R: Props` (a `where` clause on the struct). v1
   scope per non-goal 4.6.
5. **Errors landing at the right span.** RFC 050's diagnostic
   renderer should highlight the offending `<slot>` element
   or `#[slot]` attribute — not the surrounding `#[component]`.
   Reuse the byte-span machinery already in place for
   forbidden-directive diagnostics.

## 8. Alternatives considered

### 8.1 Separate `iterates = field` macro argument

Earlier draft of this RFC. Rejected because it introduced a
**parallel mechanism** for what is fundamentally the same
question: "does this slot publish the iteration variable?" The
template's `pp-for` context already answers that; making the
author repeat the answer as a macro arg is redundant and adds
a way for the macro arg and template to disagree.

Concretely the rejected shape was:

```rust
#[slot(name = "row", iterates = files)]
// or as two-arg combination:
#[slot(name = "row", props = UploadRow, iterates = files)]
```

Versus the accepted shape:

```rust
#[slot(name = "row", props = UploadFile)]
// template's <li pp-for="file in files"><slot name="row"></slot></li> tells the macro the rest
```

### 8.2 Wrapper type to mark iteration

Considered `Each<T>`, `ItemOf<T>`, `PropsIterator<T>`,
`SlotContext<T>` as type-level markers of "this slot iterates
over T." Rejected because the wrapper encodes at the **type
level** what is actually a **template-level** concern: where
the `<slot>` element sits relative to a `pp-for`. The macro
already has to scan the template AST for that; a marker type
duplicates information and forces the author to keep two
things in sync (the marker AND the template position).

The naming friction we hit during design was the tell — every
candidate wrapper name (`PropsIterator`, `Each`, `ItemOf`,
`For`) felt off because the wrapper was trying to name "the
iteration source" while the slot's per-iteration binding is
"the item." The right answer was to drop the wrapper and have
the decl describe the binding directly.

### 8.3 Separate `Ctx` / `SlotContext` vocabulary

Initial brainstorm during PR #127's review: declare a
`UploadItemCtx` struct via `#[derive(Serialize)]` and
reference it from `#[slot(name = "row", ctx =
UploadItemCtx)]`. Rejected because the framework already has
Props as the typed-wire concept; a parallel "Ctx" vocabulary
splits the mental model (*"is this a prop or a ctx?"*) for no
implementation gain. Slot publication and parent→child prop
passing are the same shape going opposite directions; one
vocabulary covers both.

### 8.4 Implicit `$host.X` from `#[expose]`

Considered: make slot content automatically see the compound's
`#[expose]`'d fields via a `$host.X` magic. Rejected because
(a) it conflates slot scoping with RFC 081's
component-handle refs surface (which *does* the cross-instance
reach the magic implies), and (b) the per-slot explicit
publication is more type-discoverable — readers see exactly
what each slot publishes by looking at the compound's `<slot>`
element, without having to cross-reference an `#[expose]`
list.

### 8.5 Slot name as binding identifier

Considered: drop `pp-let="row"` and infer the binding name
from the slot name (`pp-slot="row"` binds `row` automatically).
Rejected because it couples slot identity with caller-side
variable identity (a rename ripples into every caller) and
forecloses the "two slots publishing similar shapes" case.
`pp-let` stays explicit.

### 8.6 Inline type annotations on `<slot>`

Proposed: `<slot :name@type<String>>`. Rejected per the
`.poco`-format rule (RFC 008 §"no mixed files"): types live in
Rust, not in template attribute values. The macro can
synthesize the same validation by reading the props type
declared in Rust.

### 8.7 Drop slot props entirely; require RFC 081 refs

Considered: skip the typed-slot mechanism and tell authors to
expose state via `pp-ref="instance_name"` + `$ref.X` (RFC 081).
Rejected because RFC 081 forces the caller to name every
instance even when the slot's content is the natural
consumer — friction on the common path. Typed slot props keep
the implicit (no naming required) slot-content path working
while opting into type safety.

## 9. Risks

1. **Set-equality strictness on `:LHS=` publications.** A
   typed static slot with 12 `#[prop]` fields means the
   compound author writes 12 `:foo="expr"` attributes on
   `<slot>`. Mitigation: `#[prop(flatten)]` on the props type
   already lets a single `:user="user"` flatten into 8 leaves;
   the leaf-coverage check follows the flatten unwinding.
   Watch for whether real components push toward looser
   "subset is fine" semantics during Phase 1.
2. **Mode is implicit.** A reader scanning the `#[slot]` attr
   doesn't see "iterated vs static" — they have to look at the
   template to know which. Mitigation: the doc page in
   Phase 4 leads with two side-by-side examples; the
   distinction is one keystroke (presence of `pp-for` ancestor)
   so readers learn it fast. The simplicity of "one arg, two
   contexts" justifies the implicitness.
3. **Mode-mix detection error message clarity.** The rule
   "presence of any `:LHS=` on the `<slot>` element forces
   static mode" needs a clean diagnostic when authors
   inadvertently mix (e.g., they expect iterated auto-publish
   AND add a `:label=...` for derived data). The error should
   say "this slot has explicit publications, so it's in static
   mode; iterated mode means zero `:LHS=` on the `<slot>`
   element."
4. **Caller-side typing requires `uses = [...]`**. Apps that
   don't declare `uses` get no caller-side validation. Same
   constraint applies to other RFC 060 features; not new, but
   worth highlighting in the migration doc.
5. **Macro-emitted boundary assertions slow `cargo check`.**
   Each `#[slot(props = T)]` emits a `T: Props` boundary plus
   (in iterated mode) an iteration-type assertion. Watch for
   incremental-rebuild times on apps with many typed slots;
   if it becomes a problem, fold the assertions into a single
   per-component emit rather than per-slot.
6. **Renaming a prop on the props struct breaks the
   compound's `<slot :LHS="…">` publications.** This is the
   intended behavior (the rename should surface), but it
   means slot props are part of the compound's public API
   surface — name them as carefully as Rust function
   parameters.

## 10. Verification

Phase 1 (`props = T` + static mode):

- Existing `#[slot(default)]` + `#[slot(default, only=[…])]`
  call sites in `pine/` compile unchanged (no opt-in).
- A test fixture under `crates/pocopine/tests/typed_slots.rs`
  declares `#[slot(name = "header", props = HeaderProps)]` and
  asserts the macro emits the expected validation tokens.
- Compile-fail UI tests (matching the
  `pocopine-sync-crud-macros/tests/ui/` trybuild pattern) for:
  - Missing `:foo=` publication.
  - Extra `:foo=` publication.
  - Props type without `#[derive(Props)]`.

Phase 2 (iterated mode via template-AST dispatch):

- Fixture with `<slot>` inside `pp-for`, no `:LHS=` attrs, and
  `#[slot(name = "row", props = UploadFile)]` where
  `files: Vec<UploadFile>` — should compile and auto-publish.
- Compile-fail tests for:
  - `pp-for` iteration item type ≠ props type (e.g.,
    `files: Vec<OtherFile>`).
  - `<slot>` with `:LHS=` attrs sitting inside a `pp-for`
    (mode-mix detection — must error saying "static mode is
    in force because of the explicit publications, so all
    `#[prop]` fields on T must be published").
  - Props element type without `#[derive(Props)]`.
  - `pp-for` over an unsupported expression shape (per §5.6).

Phase 3 (caller-side type checking):

- Caller fixture under `crates/pocopine/tests/` declaring
  `uses = [UploadRoot]` + `<template pp-slot="row" pp-let="file">`;
  compile-fail tests for `file.unknown_field` reads.
- Positive test for `file.X` reads against the declared
  props type compiling cleanly.

Phase 4 (docs + migration):

- `03-composition.md` and `04-expressions.md` link to the
  typed-slot pattern.
- At least two pine primitives migrated and used as
  canonical examples.
