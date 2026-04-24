# RFC 049 — Typed slot contracts: compile-time child constraints on parent components

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 011](./rfc-011-scoped-slots.md) §10, [RFC 032](./rfc-032-lifecycle-element-param.md), [RFC 045](./rfc-045-single-root-templates.md), [RFC 046](./rfc-046-children-extractor.md) |

## 1. Summary

Let a parent component declare, at the struct level, which
pocopine components are allowed as children of each of its
slots. The declaration emits a marker trait + blanket impls,
and the consumer's `#[component]` macro enforces the contract
by emitting a `const _` assertion per direct child tag in its
template. A foreign component as a child surfaces as a `rustc`
compile error with the offending tag's span.

```rust
// crates/pine/src/context_menu/mod.rs
#[component(template = "PineContextMenuContent.poco", role = "panel")]
#[slot(default, accepts = [
    PineContextMenuItem,
    PineContextMenuSeparator,
    PineContextMenuGroup,
    PineContextMenuLabel,
])]
pub struct PineContextMenuContent { /* ... */ }
```

```html
<!-- consumer — compiles -->
<pine-context-menu-content>
  <pine-context-menu-item>Open</pine-context-menu-item>
  <pine-context-menu-separator/>
</pine-context-menu-content>

<!-- consumer — rustc error -->
<pine-context-menu-content>
  <pine-random-thing/>
</pine-context-menu-content>
```

```text
error: custom attribute panicked
 --> src/my_menu.rs:5:1
  |
5 | #[component(template = "MyMenu.poco", ...)]
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

error: `PineRandomThing` is not an accepted child of
       `pine-context-menu-content`'s default slot
  --> src/MyMenu.poco:14:3
   |
14 |   <pine-random-thing/>
   |   ^^^^^^^^^^^^^^^^^^^^ not an accepted child
```

Two error blocks: rustc's own one at `#[component]` plus a
second block we render ourselves via `annotate-snippets`
(rustc's own diagnostic-rendering crate) that points into the
`.poco` file with a proper caret. IDEs parse the
`--> src/MyMenu.poco:14:3` line as a clickable file link.
§4.6 covers the rendering; no nightly features required.

Typed iteration — yielding `Handle<T>` from direct children
— stays an RFC 046 concern; RFC 049 only introduces the
compile-time assertion. §4.4 covers the division of
responsibility.

## 2. Motivation

### 2.1 The pattern

Compound primitives in Reka UI, Radix, Headless UI, and Ark
all declare "my content accepts these specific item types."
Usage mistakes — wrong tag, typoed tag, foreign component
dropped in — are rejected up front. Authors in those frameworks
get TypeScript errors the moment they ship a bad child. A
Menu that accepts `<MenuItem>` simply won't compile with a
`<CustomCard>` inside.

pocopine's walker is permissive: any tag is a valid child at
runtime. Unexpected tags render, inherit no ARIA from the
parent, miss scope-level keyboard wiring, and quietly degrade
the primitive's behaviour. Today the only guard is convention
and documentation — both of which authors bypass the first
time they try to reuse markup cleverly.

### 2.2 Concrete failure modes we've seen

- **ARIA misattribution.** `<pine-context-menu-content>` sets
  `aria-setsize` on each direct child in `on_mount`. If a
  consumer wraps items in a `<div class="list">`, the content
  walker writes `aria-setsize="1"` on the `<div>`, not the
  items inside. Screen reader reads "one of one" three times.
- **Keyboard gaps.** `pp-roving` installs on items. A foreign
  tag between items breaks roving's tab chain; the author
  doesn't find out until they tab through the UI on review.
- **Focus trap leaks.** Dialog expects its `<pine-dialog-close>`
  child to own dismiss events. A custom wrapper swallows them;
  the dialog can't be closed.
- **Silent style drift.** CSS selectors scoped to
  `pine-context-menu-content > pine-context-menu-item` skip
  wrapped items — consumer sees unstyled content and blames
  the primitive.

All four are first-time-wrong, easy-to-miss bugs that
compile-time typing eliminates.

### 2.3 Why now

RFC 046 proposes `Children::of::<T>()` as a *runtime* tag
filter. It works regardless of what consumers pass, but it
can't answer "is my slot content correct at all?" — only "what
in the slot matches?" That question *is* compile-time once we
have a contract; RFC 049 is the missing half and builds on the
direction RFC 046 sets out.

RFC 011 §10 explicitly deferred compile-time slot typing.
Shipping RFC 045's `const _: () = ...` pattern showed that
template-shape mistakes can surface as `rustc` compile errors
without needing a bespoke diagnostic channel; we reuse that
same mechanism here for a different category of error.

## 3. Non-goals

* **Not enforcing plain HTML elements.** Raw `<div>`,
  `<span>`, `<p>` inside a typed slot pass silently. We're not
  here to micromanage semantic HTML; pocopine trusts authors
  to write valid markup. See §4.6 for the "warn" variant —
  future work, not v1.
* **Not enforcing deep descendants.** Contract applies to
  *direct children of the slot the author writes the
  `<pine-component>` tag in*. Items nested inside
  `<pine-context-menu-group>` are the group's problem; the
  group declares its own `#[slot]` contract.
* **Not retrofitting every component.** `#[slot(accepts=…)]`
  is opt-in. A primitive that doesn't declare constraints
  accepts anything — same as today. Authors opt in when the
  ergonomics matter.
* **Not replacing `<slot>` defaults or scoped slots.** RFC
  011's `<slot name="…">` / `pp-let` / default content all
  stay. Typed contracts layer *on top* of them: a scoped slot
  can also constrain its children.
* **Not cross-crate type invention.** The trait and its impls
  live in the parent's crate. Consumers in downstream crates
  get the trait via the normal `use` path; no linker tricks.
* **Not runtime-checked in release builds.** Contract is
  static — once `cargo build --release` passes, there's no
  per-mount tag verification. The walker stays lean.

## 4. Design

### 4.1 Parent declaration — `#[slot]` attribute

A new proc-macro attribute `#[slot]`, paired with
`#[component]` on the parent struct. Each `#[slot]` instance
declares one slot's contract:

```rust
#[component(template = "PineContextMenuContent.poco", role = "panel")]
#[slot(default, accepts = [
    PineContextMenuItem,
    PineContextMenuSeparator,
    PineContextMenuGroup,
    PineContextMenuLabel,
])]
pub struct PineContextMenuContent { /* ... */ }
```

Forms accepted:

| Form | Meaning |
|---|---|
| `#[slot(default, accepts = [A, B])]` | Constrain the default slot loosely: accepted component children are checked when directly present, but HTML wrappers still pass. |
| `#[slot(name = "footer", accepts = [A])]` | Constrain a named slot loosely. |
| `#[slot(default, only = [A, B])]` | Constrain the slot strictly: every direct child element must be one of the listed accepted component tags. |
| `#[slot(default)]` with no `accepts` | Declare the slot exists; accept anything (opt-in to typed yield in §4.4 without locking the child set). |
| Repeat `#[slot]` per named slot | One attribute per slot keeps each contract readable. |

**Alternative form** (rejected, §6.2) — folding into
`#[component(slots = { … })]`. Works, reads worse for
multi-slot primitives.

### 4.2 What `#[component]` emits for each `#[slot]`

`#[component]` owns the expansion. `#[slot]` is an **inert
helper attribute** — `#[component]` parses it off the struct
(same pattern `#[prop]` already uses per RFC 031) and emits
one concrete marker trait plus one blanket `impl` per entry
in `accepts`. There is no abstract "slot trait family";
every emitted trait is a standalone concrete item.

```rust
// Emitted for #[slot(default, accepts = [Item, Separator, ...])]
// on PineContextMenuContent. Lives as a module-level sibling
// item in the same module as the struct — not nested inside it.

/// Marker trait — implementors are the allowed default-slot
/// children of [`PineContextMenuContent`].
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an accepted child of \
               `pine-context-menu-content`'s default slot",
    note = "allowed children are declared on \
            `PineContextMenuContent` via `#[slot(default, accepts=[...])]`",
)]
pub trait PineContextMenuContentDefaultChild {}

impl PineContextMenuContentDefaultChild for PineContextMenuItem {}
impl PineContextMenuContentDefaultChild for PineContextMenuSeparator {}
impl PineContextMenuContentDefaultChild for PineContextMenuGroup {}
impl PineContextMenuContentDefaultChild for PineContextMenuLabel {}
```

The trait name is mechanically derived:
`<StructIdent><SlotName>Child`, where `<SlotName>` is the
slot's name in PascalCase (`Default` for the unnamed slot).
Documented in §7 as part of the public API so consumers
importing the trait aren't guessing at spelling.

The trait lives as a module-level sibling of the struct —
reachable at `<parent-crate>::<module-path>::PineContextMenuContentDefaultChild`
(the same module that exports `PineContextMenuContent`). This
RFC does not introduce nested-in-struct namespacing; Rust
doesn't have it.

The `#[diagnostic::on_unimplemented]` attribute (stable since
Rust 1.78) replaces the default "trait not implemented" message
with a pocopine-flavoured one. §4.6 covers diagnostic shape.

### 4.3 Consumer-side enforcement

When a consumer's `#[component]` macro processes its own
`.poco` template, it scans for component tags that are backed
by typed slots and emits one assertion per direct child of
each typed-slot usage.

```rust
// Consumer's MyMenu.poco:
//   <pine-context-menu-content>
//     <pine-context-menu-item .../>
//     <div>oops</div>
//   </pine-context-menu-content>

// Emitted alongside MyMenu::register():
const _: fn() = || {
    fn assert_child<T: PineContextMenuContentDefaultChild>() {}
    assert_child::<PineContextMenuItem>();
    assert_child::<::std::... /* type for <div> */>(); // fails
};
```

The resolution rules for "tag → Rust type" are deliberately
conservative (see §4.7 for why):

1. **Known pocopine-component tags** — resolved via an
   explicit `uses` list on the consumer (see §4.7). The list
   pairs each type with the tag string the consumer's
   template uses, so the macro never has to "read" anything
   from a type path.
2. **Plain HTML tags (`<div>`, `<span>`, text nodes)** —
   skipped in `accepts` mode; the contract doesn't constrain
   them unless the parent used `only = [...]` on that slot.
3. **Unknown custom-element tags not in `uses`** — skipped
   silently. v1 deliberately doesn't emit a warning here;
   warning-level proc-macro diagnostics aren't a stable
   surface pocopine uses elsewhere, and "unknown tag"
   legitimately covers both typos and not-yet-registered
   external components. Authors who want the check add the
   component to their `uses` list.
4. **`only` slots** — any direct child element that is not one
   of the listed accepted component tags is rejected. That
   includes plain HTML wrappers like `<div>` and `<span>`.
   `only = [...]` exists for compounds whose semantics depend
   on direct-child structure (`aria-*` distribution, roving
   focus, `parent > child` selectors, dismiss wiring, etc.).

`uses` is a **local registry for this consumer only**. It does
not inherit from parents, from the app root, or from any
workspace-global table. A component moved to another subtree
should not change validity because some distant ancestor
happened to import a different tag set; the contract must stay
readable from the component's own source.

Assertions run per-consumer, per-usage. If `MyMenu` uses
`<pine-context-menu-content>` three times in its template,
three independent assertion blocks are emitted, each with the
direct children of that instance. Conditional branches
(`pp-if` / `pp-for` / `<template>`) union all possible children
into the same assertion block — still a static set (§8.3).

### 4.4 Typed iteration stays in RFC 046

RFC 049 does **not** introduce a new iteration method on
`Children`. It only adds the concrete marker trait + consumer-
side assertion. Typed iteration — yielding `Handle<T>` from
filtered direct children — is an RFC 046 concern and lives
there as a sibling to `Children::of::<T>()` (e.g. a future
`Children::handles_of::<T: Component>()`); adding that method
does not depend on a parent's slot contract.

Authors who want to iterate typed children today use the RFC
046 shape:

```rust
#[handlers]
impl PineContextMenuContent {
    pub fn on_mount(&mut self, children: Children) {
        // Runtime filter (RFC 046) — yields Element.
        let total = children.count_of::<PineContextMenuItem>();
        for (i, item) in children.of::<PineContextMenuItem>().enumerate() {
            let _ = item.set_attribute("aria-posinset", &(i + 1).to_string());
            let _ = item.set_attribute("aria-setsize",  &total.to_string());
        }
    }
}
```

The compile-time guarantee RFC 049 adds is that this template
*can't contain the wrong child component in the first place*
— the consumer-side assertion rejects the template before any
iteration runs. Narrowing the iteration API to a "only-
compiles-when-Parent-declared-T" bound would require either
const-string-generic machinery (unstable, not a realistic v1
design) or per-parent generated iteration methods (high
generated-surface area for uncertain ergonomic win). We stay
out of that design space in v1 and let runtime-filter
iteration serve both typed and untyped cases.

### 4.5 Named slots

Each named slot gets its own marker trait:

```rust
#[component(...)]
#[slot(default, accepts = [Item, Separator])]
#[slot(name = "header", accepts = [Title, Subtitle])]
pub struct Foo { ... }

// Emits:
pub trait FooDefaultChild {}
pub trait FooHeaderChild {}

impl FooDefaultChild for Item {}
impl FooDefaultChild for Separator {}
impl FooHeaderChild for Title {}
impl FooHeaderChild for Subtitle {}
```

The consumer-side scan keys off `pp-slot="…"` on the
`<template>` wrapper:

```html
<pine-foo>
  <template pp-slot="header">
    <pine-title/>        <!-- asserted against FooHeaderChild -->
  </template>
  <pine-item/>           <!-- asserted against FooDefaultChild -->
</pine-foo>
```

### 4.6 Diagnostic ergonomics — pre-rendered snippets via `annotate-snippets`

Stable proc-macros can't construct spans that point inside
external files (`proc_macro_span` is unstable). Rustc's arrow
on a `compile_error!` attached to our assertion therefore
lands on the `#[component]` attribute in the `.rs` file, not
at line 14 of the `.poco`. That's a dead-end for the error
shape authors want.

The way out is to stop trying to drive rustc's arrow and
**render the snippet ourselves**, in-process, using
[`annotate-snippets`](https://crates.io/crates/annotate-snippets)
— the crate rustc itself uses for its own error rendering.
Our error ships as pre-formatted multi-line text embedded in
the `syn::Error` message; rustc prints it verbatim, and IDEs
parse the `--> path:line:col` pattern as a clickable file
link regardless of who produced it.

**Rendering path.** At assertion-emit time, for each
offending child tag the consumer-side scan has tracked:

```rust
use annotate_snippets::{Level, Renderer, Snippet};

let poco_text: &str = /* template source, from include_str! */;
let tag_byte_range: std::ops::Range<usize> = /* walker-tracked */;
let file_path: String = /* e.g. "src/MyMenu.poco", relative */;
let parent_tag: &str  = "pine-context-menu-content";
let slot_name: &str   = "default";
let child_ty_name: &str = "PineRandomThing";

let title = format!(
    "`{child_ty_name}` is not an accepted child of \
     `{parent_tag}`'s {slot_name} slot"
);

let message = Level::Error
    .title(&title)
    .snippet(
        Snippet::source(poco_text)
            .origin(&file_path)
            .fold(true)
            .annotation(
                Level::Error
                    .span(tag_byte_range)
                    .label("not an accepted child"),
            ),
    );

let rendered = format!("{}", Renderer::styled().render(message));
```

The rendered string is the payload the macro returns via
`syn::Error::new(span, rendered).to_compile_error()`. Rustc
prints it as-is. Authors see:

```text
error: custom attribute panicked           <-- rustc's boilerplate
 --> src/my_menu.rs:5:1
  |
5 | #[component(template = "MyMenu.poco", ...)]
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

error: `PineRandomThing` is not an accepted child of   <-- our snippet
       `pine-context-menu-content`'s default slot
  --> src/MyMenu.poco:14:3
   |
14 |   <pine-random-thing/>
   |   ^^^^^^^^^^^^^^^^^^^^ not an accepted child
```

Two error blocks, both rendered by the same engine
(`annotate-snippets` is exactly what rustc uses) so the shape
matches byte-for-byte. Editors that linkify rustc output
(rust-analyzer, IntelliJ Rust, every terminal with a file:line
parser) open `MyMenu.poco` at line 14 on click.

**What this costs.** One dependency —
`annotate-snippets = "0.11"` — added to `pocopine-macros`.
About 100 lines of glue: walker tracks byte ranges per tag,
macro passes them through to the renderer, macro emits the
pre-formatted string. No nightly features, no unstable APIs,
no span manipulation.

**Why not `#[diagnostic::on_unimplemented]`.** It's useful,
but it can only customise *rustc's* default trait-mismatch
message — which still lands on `#[component]`, not on the
`.poco` tag. We can layer the two: keep
`on_unimplemented` as a fallback safety net for assertions
whose template walker *didn't* record a byte range (belt and
braces), while the primary diagnostic path is the
`annotate-snippets` render.

**Plain HTML children.** A `<div>` inside a typed slot has no
associated pocopine-component type. In the default mode it
**silently passes**: the scan sees a plain HTML tag, finds no
matching entry in the consumer's `uses` list, and emits no
assertion — so no snippet renders. Authors who want ad-hoc
HTML wrappers (`<div class="stack">…</div>`) keep working.

For compounds that must preserve direct-child structure, the
parent uses `only = [...]` instead of `accepts = [...]`:

```rust
#[slot(default, only = [PineContextMenuItem, PineContextMenuSeparator])]
```

In an `only` slot, any direct child element that is **not**
one of the listed accepted component tags is rejected,
including raw HTML wrappers:

```html
<pine-context-menu-content>
  <div class="list">
    <pine-context-menu-item/>
  </div>
</pine-context-menu-content>
```

The `<div>` is the direct child, so the consumer gets a
compile-time error pointing at that wrapper. Nested accepted
children do not "redeem" an invalid wrapper. This keeps
structural compounds honest without forcing every slot in the
framework to forbid layout wrappers.

**Alternative renderers.** `codespan-reporting`, `ariadne`,
and `miette` all produce similar output. Picked
`annotate-snippets` because it's rustc's own crate — the
output is identical by construction, and the dependency is a
transitive one most Rust users already have via rustc's
build-time deps. See §6.7.

### 4.7 Tag → Rust type resolution

A proc-macro cannot inspect foreign type metadata at expansion
time — it can only read the token stream of its own invocation.
So the consumer has to hand the macro both the type path *and*
the tag string explicitly. The `uses` list is syntax-level pair
data, not a lookup into some other crate's associated consts:

```rust
#[component(uses = [
    (PineContextMenuContent,   "pine-context-menu-content"),
    (PineContextMenuItem,      "pine-context-menu-item"),
    (PineContextMenuSeparator, "pine-context-menu-separator"),
])]
pub struct MyMenu { /* ... */ }
```

The macro:

1. Parses each `(TypePath, "tag-string")` pair from the
   `uses` token stream. Both sides are literal — the type
   path as `syn::Path`, the tag as `LitStr`.
2. Builds a local `tag → TypePath` table from the pairs.
3. Rejects duplicate tag mappings inside the same `uses`
   list. Two entries resolving to the same tag string are a
   hard compile error on the consumer, because picking one
   silently would make slot checks nondeterministic.
4. For each `<tag>` in the template, looks up the type path
   in the table. Unknown → skipped (raw HTML or external).
5. Emits assertions only for tags present in the table.

**Shorthand for the common case.** `#[component]`'s default
kebab-casing rule (`PineContextMenuItem` → `"pine-context-menu-item"`)
is public and deterministic; the consumer macro can apply the
same rule to bare entries:

```rust
#[component(uses = [
    PineContextMenuContent,   // kebab-cased to "pine-context-menu-content"
    PineContextMenuItem,
    (OddlyNamed, "my-custom-tag"), // override when the component used
                                   // `#[component(name = "my-custom-tag")]`
])]
```

Bare entries get the default-kebab mapping; tuple entries
carry an explicit override. No cross-crate type inspection at
any point — the tag is either deterministic from the ident or
explicit from the tuple, both readable from the consumer's
own source.

`uses` is therefore best understood as a **localized registry**
owned by the consumer component:

- local in scope — visible only to this template's compile-time
  checks,
- explicit in source — no linker or runtime discovery,
- conflict-checked — duplicate tag strings are rejected at
  macro expansion,
- intentionally non-inherited — descendants declare their own
  `uses` when they need slot-contract validation.

**Alternatives considered (§6).** A `linkme` distributed
slice lets components register themselves at link time, but
that data isn't visible during proc-macro expansion. A
convention-only glob-import mapping breaks when authors
rename types. Explicit pairing is the only cross-crate-safe
shape proc-macros can realistically implement.

## 5. Implementation

### 5.1 `#[component]` owns expansion; `#[slot]` is inert

`#[slot]` is **not a separate proc-macro**. It's an inert
helper attribute that `#[component]` recognises on its own
struct — the same shape `#[prop]` already takes (RFC 031 §5
strips `#[prop]` from the emitted struct and consumes its
metadata inside `#[component]`). `#[slot]` follows that model.

Reasons to keep ownership in one macro:

- Attribute-macro ordering on the same item is a fragile
  contract. Saying "these compose by ordering" is not a
  specification; it's a hope.
- `#[component]` and the slot declaration need to share
  state (the struct ident feeds into the trait name;
  the slot metadata may influence future `#[component]`
  emissions). One owner avoids cross-macro coordination.
- Authors don't have to remember which macro to import
  first or write in which order.

**Expansion.** For each `#[slot(...)]` attribute on the
struct, `#[component]` emits one concrete trait + blanket
impls per `accepts` entry as a **module-level sibling** of
the struct (not nested inside it — Rust has no such
namespacing):

```rust
// Given:
#[component(template = "PineContextMenuContent.poco", role = "panel")]
#[slot(default, accepts = [PineContextMenuItem, PineContextMenuSeparator])]
pub struct PineContextMenuContent { /* ... */ }

// #[component] expands to (conceptually):
pub struct PineContextMenuContent { /* ... */ }

#[diagnostic::on_unimplemented( /* ... */ )]
pub trait PineContextMenuContentDefaultChild {}
impl PineContextMenuContentDefaultChild for PineContextMenuItem {}
impl PineContextMenuContentDefaultChild for PineContextMenuSeparator {}

// …plus the existing ComponentState impl and register() fn.
```

The trait is reachable at the same module path as the struct
— if `PineContextMenuContent` is exported from
`pine::context_menu`, the trait is exported as
`pine::context_menu::PineContextMenuContentDefaultChild`.
Primitives that want a cleaner prelude re-export the trait
manually through their crate's own exports.

### 5.2 Consumer-side template scan and rendering

Extend the `.poco` pre-walk that `#[component]` already runs
(RFC 045's single-root validator path) to additionally:

1. Parse the `uses = [...]` token stream into a
   `(tag: String, type_path: syn::Path)` list (bare idents
   apply the default kebab rule; tuples carry explicit
   overrides — §4.7).
2. Walk the template bytes, tracking each element tag's
   `Range<usize>` (start byte → end byte) in the `.poco`
   source. This is a small extension on top of the existing
   validator walk, which already tracks enough offsets for
   root-counting.
3. Walk the template AST looking for opening tags whose name
   matches one of the `uses` entries. Non-matching tags
   (plain HTML, unknown customs) are skipped silently.
4. For each matching parent tag, collect its direct children
   — including the union of `pp-if` / `pp-for` /
   `<template>` branches — and determine each child's own
   `(tag, type_path, byte_range)` from the same `uses` list
   and the position tracker.
5. For each (parent, child) pair, emit one `const _: fn() =
   || { ... }` assertion block. The trait the assertion
   targets is mechanically derived by the consumer macro from
   the parent type path: `<ParentType>DefaultChild` for
   unnamed-slot children, `<ParentType><SlotName>Child` for
   `pp-slot="…"` children.
6. **Pre-render a diagnostic snippet** via `annotate-snippets`
   for each (parent, child) pair and stash it as a
   doc-comment or `const _: &str` next to the assertion, so
   that when rustc reports the unimplemented-trait error the
   rendered snippet is visible to the author (and to any IDE
   parsing the output). The snippet origin is the `.poco`
   path relative to the consumer crate's `CARGO_MANIFEST_DIR`;
   the span is the child tag's byte range.

Note the consumer macro does **not** need to know whether the
parent actually declared a typed slot — it just emits the
assertion `fn assert_child<T: <ParentType>DefaultChild>()`.
If the parent never declared one, the trait doesn't exist,
and rustc emits "cannot find trait `…DefaultChild`" pointing
at the same template span. That's acceptable — authors using
`uses = [ParentType]` on a non-typed parent opt into the
check and get an error if the parent didn't opt in too.

The walk is ~150 lines of byte-level parsing on top of what
RFC 045 already builds; all token emission happens inside the
same `#[component]` expansion. Renderer glue adds ~40 lines
against the `annotate-snippets` API.

### 5.3 `annotate-snippets` integration

Add the dependency to `crates/pocopine-macros/Cargo.toml`:

```toml
[dependencies]
annotate-snippets = "0.11"   # rustc's own diagnostic renderer
```

One helper module wraps the renderer:

```rust
// crates/pocopine-macros/src/diagnostics.rs
use annotate_snippets::{Level, Renderer, Snippet};

pub(crate) fn render_template_error(
    poco_src:  &str,
    file_path: &str,
    byte_range: std::ops::Range<usize>,
    title:     &str,
    label:     &str,
) -> String {
    let message = Level::Error.title(title).snippet(
        Snippet::source(poco_src)
            .origin(file_path)
            .fold(true)
            .annotation(Level::Error.span(byte_range).label(label)),
    );
    format!("{}", Renderer::styled().render(message))
}
```

The returned string is what the macro hands to `syn::Error`
or `compile_error!`.

### 5.4 Tests

- **Positive path.** `#[slot(default, accepts = [A, B])]` on a
  parent; consumer template with `<A/>` and `<B/>` children;
  assert consumer compiles.
- **Negative path (trybuild).** Same parent; consumer with
  `<C/>`; assert `trybuild` captures the rendered snippet
  text including the `--> path/to/file.poco:LINE:COL` line
  and the tag caret.
- **Named-slot path.** `#[slot(name="footer", …)]`; consumer
  uses `pp-slot="footer"`; assert assertions target the
  named trait.
- **Skip cases.** Raw HTML child (`<div>`); unknown custom
  tag not in `uses`; `pp-if` / `pp-for` branches — all
  compile cleanly.
- **Ownership.** A struct with `#[slot]` but no `#[component]`
  produces a clear "`#[slot]` requires `#[component]`" error
  (the `#[slot]` attribute name is unknown to rustc on its
  own, so this is the default-path behaviour we verify).
- **Renderer snapshot.** Golden-file test against the rendered
  snippet output — catches accidental changes to the
  `annotate-snippets` API or our formatting helper.

## 6. Alternatives considered

### 6.1 Tag registry via `linkme` distributed slice

Every `#[component]` emits a `#[linkme::distributed_slice]`
entry with its tag and type path. At the consumer's
`#[component]` expansion we'd… fail, because `linkme` is
link-time: the slice exists in the final binary, but proc-
macros can't read it during the consumer's compile. Useful
for **runtime** assertions (`children.debug_expect_all`), not
compile-time.

### 6.2 Fold into `#[component(slots = { … })]`

```rust
#[component(
    template = "PineFoo.poco",
    slots = {
        default: [Item, Separator],
        header:  [Title],
    },
)]
pub struct PineFoo { ... }
```

Works. Downsides: `#[component]` becomes the god-macro, slot
constraints crowd the other keys (`template`, `role`, `name`,
`transition`, …), and each named slot's accepts list reads
worse than a dedicated `#[slot]` line. Separate attribute
wins on readability.

### 6.3 Convention-based tag→type

Kebab-case tag + ambient glob import:
`<pine-context-menu-item>` → `PineContextMenuItem`, assume
the type is in scope via `use pocopine_pine::*`. Brittle: glob
imports collide, authors who `use` a different aliased name
can't opt in, and the macro can't verify the import resolved.
The `uses = [...]` list makes the mapping explicit and
local.

### 6.4 Enum-wrapped children

Parent takes children as `Vec<PineContextMenuSlot>` where
`PineContextMenuSlot` is an enum of allowed types. Works in
React-style render props; doesn't match pocopine's HTML-first
composition where children are *tags*, not values. Would
require a whole new templating mode. Rejected.

### 6.5 Runtime-only assertion

`children.debug_expect_all::<T>()` (the option-2 from the
earlier sketch). Shipped in RFC 046 as a *complementary*
utility, not a replacement — dev-mode catches typos in
components that don't opt into RFC 049, and RFC 049 itself
eliminates the need in typed compounds. Not mutually
exclusive.

### 6.6 TypeScript-style "slots as typed props"

Author passes children as typed Rust values through an
explicit builder API instead of template tags. Killed by
pocopine's commitment to HTML-first composition (RFC 001).

### 6.7 Other diagnostic renderers

[`annotate-snippets`] is picked in §4.6, but three siblings
produce similar output:

- **`codespan-reporting`** — popular (gluon / rustc-lint-era
  roots), richer multi-file support, larger dep graph. Good
  choice if we ever need to annotate across multiple
  `.poco` files simultaneously.
- **`miette`** — the most polished visually (colours,
  unicode, labels); used by SWC and Turbopack. Overkill for
  single-snippet rendering and adds `thiserror`-style
  derives we don't need.
- **`ariadne`** — ecosystem-native in Chumsky parsers;
  colourful; smaller than `miette`. Different visual style
  from rustc.

Picked `annotate-snippets` because **it's rustc's own
renderer**. Its output matches built-in errors byte-for-byte,
so the two blocks authors see (rustc's own boilerplate + our
snippet) look consistent. Every other crate produces
subtly-different arrow / line-number / colour styling that
reads as "this came from somewhere else."

[`annotate-snippets`]: https://crates.io/crates/annotate-snippets

## 7. Rollout

1. Add `annotate-snippets = "0.11"` to
   `crates/pocopine-macros/Cargo.toml` and the helper module
   sketched in §5.3.
2. Extend `#[component]` in `pocopine-macros` to parse
   `#[slot]` helper attributes off its own struct and emit
   the concrete trait + blanket impls as module-level
   siblings. No consumer-side changes yet — the traits exist
   but nothing asserts against them.
3. Add `uses = [...]` parsing to `#[component]`. Still no
   assertions — the entry is a no-op placeholder.
4. Land the template byte-range tracker + assertion emitter +
   pre-rendered-snippet attachment inside the same
   `#[component]` expansion. `trybuild` tests and renderer
   golden-file snapshots gate correctness.
5. Migrate one Pine primitive as reference —
   `PineContextMenuContent`.
6. Author-facing docs in `docs/components/typed-slots.md`,
   including a screenshot of the rendered two-block error so
   reviewers can evaluate the UX before adopting.
7. Roll into the rest of Pine compound-by-compound as
   primitives mature — some may prefer to stay permissive.

No migration required for existing code. `#[slot]` is opt-in;
primitives without it keep the permissive runtime behaviour.

## 8. Open questions

* **8.1 Wildcard accepts.** Should `accepts = [*]` declare
  "this slot is typed but accepts any `Component`"? Useful
  for primitives that want to opt into the rejection of plain
  HTML while keeping the component surface open. Leaning yes;
  syntax TBD.
* **8.2 Parent-tag discovery.** The consumer scan has to know
  which tags in its template are parents with typed slots.
  Option: require every typed parent to appear in the
  consumer's `uses` list, so the lookup is always local.
  That's the proposed contract for v1 — simple, explicit,
  no cross-crate trait probing.
* **8.3 `pp-if` / `pp-for` children.** A conditional branch
  introduces a static set of *possible* children — unioning
  them in the assertion is correct and easy. Dynamic children
  ("a variable holds an unknown tag") don't exist in pocopine
  templates, so this stays tractable.
* **8.4 Raw HTML policy.** Silent-pass for v1 (§4.6).
  Revisit if primitives request an opt-in warn.
* **8.5 Re-exports.** Each generated trait is public API on
  the parent's crate. Cosmetic: should `#[component]` also
  push the trait into a `traits::` submodule convention to
  keep the module root tidy? Up to parent authors.
* **8.6 `annotate-snippets` API stability.** The crate's v0.x
  line has had minor breaking changes (0.9 → 0.10 → 0.11
  renamed `Label` → `Level`, tweaked the builder shape). Pin
  an exact minor in `Cargo.toml` and run the renderer-snapshot
  test in §5.4 on upgrades. Low-risk; any churn is mechanical.
  Migrating to `miette` or `codespan-reporting` later is a
  single-function swap (see §6.7).
