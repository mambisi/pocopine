# RFC 057 — Compile-time template plans

| Field | Value |
|---|---|
| **Status** | Draft — council amendments incorporated (pass 3), awaiting re-review |
| **Author** | pocopine team |
| **Created** | 2026-04-26 |
| **Supersedes** | — |
| **Related** | [RFC 050](./rfc-050-html5ever-compile-time-parser.md), [RFC 054](./rfc-054-compiled-pp-for-row-plans.md), [issue #10](https://github.com/mambisi/pocopine/issues/10) |

## 1. Summary

Move directive resolution for `.poco` templates from the runtime
walker to the `#[component]` macro. The macro already parses
every `.poco` into a `TemplateAst` (RFC 050); this RFC consumes
that AST to emit a `&'static StaticTemplatePlan` describing every
binding, listener, init, and ref the template carries. The
runtime mounts a planned template by stamping its (cleaned) HTML
once and applying the plan — no per-attribute scan, no runtime
template parser allocation for the eligible subtree.

This is **Phase 1** of the two-tier strategy in
[issue #10](https://github.com/mambisi/pocopine/issues/10): small
components compile-time-parsed and bundled in the wasm; large
components / pages get split out and server-rendered (Phase 2 —
separate RFC).

## 2. Motivation

### 2.1 What we ship today

The `#[component]` macro emits:

```rust
::pocopine::__private::register_template(
    name_str,
    ::pocopine::__private::compile_template(
        include_str!(template_path),  // raw .poco bytes ride along
        name_str,
        role_arg,
    ),
);
```

(`crates/pocopine-macros/src/lib.rs:~1260`)

So the wasm carries:

1. The raw `.poco` source as a `&'static str`.
2. The runtime `compile_template` rewriter
   (`crates/pocopine-core/src/templates.rs:42`) — light, just
   does `<root>` → `<tag>` and `pp-data` injection.
3. The runtime walker
   (`crates/pocopine-core/src/walker.rs`) — heavy, scans every
   `pp-*` / `:` / `@` attribute on every node to install
   bindings and listeners.

For the `counter` example that's 668 KB raw / 254 KB gzip wasm.
For the `jsbench/pocopine` harness it's 699 / 265. Leptos and
Yew sit at 189 / 69 and 246 / 92 respectively for the same
benchmark. The delta isn't the reactive runtime — it's that
pocopine ships templates as raw data and re-parses them at
mount time, while the other Rust/WASM frameworks compile
templates to Rust at macro time.

### 2.2 Why now

RFC 050 already lands the macro-time HTML parser. RFC 054
already proves the static-plan + `node_path` pattern works
end-to-end for `pp-for` row bodies. Generalising that pattern
to the whole template is the logical next step: the AST exists,
the runtime ABI shape exists, and the runtime walker already
has a "fall back to me when the static path can't handle it"
contract from RFC 054.

## 3. Goals

- Walk the existing `TemplateAst` once at macro time and emit a
  compact `&'static StaticTemplatePlan` covering the v1 envelope
  in §6.
- Bypass the runtime per-attribute scan for nodes covered by the
  plan, while preserving the exact ordering guarantees the
  current walker provides — refs registered before `pp-init`
  fires, child component scopes exist before any parent-side
  prop write touches them, post-order subtree binding.
- Leave the runtime walker intact for everything outside the v1
  envelope (§6) — slots, dynamic mounts, `pp-if` / `pp-for` /
  `pp-teleport` whole-element subtrees, `pp-model`, child-
  component prop targets, and unknown listener modifiers.
- Keep `.poco` hot-reload working through the existing
  `include_str!` dependency-pin.
- Reuse the framework's existing cleanup machinery
  (`with_current_el`, effect tracking, listener teardown via
  `walker::track_listener_on_with_opts`, `refs::register`,
  `on_scope_unmount`) — no parallel directive implementations.

## 4. Non-goals

- Removing the runtime walker entirely. It remains authoritative
  for slot content, `pp-if` enter/leave, child-component prop
  targets, and any template that ships through a dynamic-mount
  path (a string template, a router-injected tag, etc.).
- Tree-shaking individual directive runtime modules
  (`directives::text`, `directives::on`, …) based on whether
  any registered template uses them. Separate concern.
- Server-side rendering of large components / pages — that's
  Phase 2 of issue #10, in its own RFC.
- Changing user-facing template syntax. `.poco` files keep
  their current shape; this RFC is invisible to authors.
- Promoting `pp-model`, `pp-route`, child-component prop writes,
  or unknown listener modifiers into the plan in v1 — those are
  follow-ups gated on separate pieces of work (extracting their
  install helpers, ordering guarantees on child mount).

## 5. Proposal

### 5.1 Static plan shape

```rust
#[doc(hidden)]
pub struct StaticTemplatePlan {
    pub bindings: &'static [StaticBinding],
    pub listeners: &'static [StaticListener],
    pub inits:    &'static [StaticInit],
    pub refs:     &'static [StaticRef],
}

#[doc(hidden)]
pub struct StaticBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    pub expr_src: &'static str,
}

pub enum BindingKind {
    Text,                              // pp-text
    Html,                              // pp-html
    Bind { arg: &'static str },        // pp-bind:<arg> / :<arg>
    Show,                              // pp-show
    Class,                             // RFC 054 row plans only — see below
}

#[doc(hidden)]
pub struct StaticListener {
    pub node_path: &'static [u16],
    pub event: &'static str,
    pub modifiers: &'static [&'static str],   // see §6.1 for the supported set; debounce occupies two adjacent slots (`"debounce"` + ms string)
    pub expr_src: &'static str,
}

#[doc(hidden)]
pub struct StaticInit {
    pub node_path: &'static [u16],
    pub expr_src: &'static str,
}

#[doc(hidden)]
pub struct StaticRef {
    pub node_path: &'static [u16],
    pub name: &'static str,
}
```

`node_path` is a chain of child indices from the rendered
template root, matching the convention RFC 054 already uses.
Indices are over the cloned-DOM children produced by
`set_inner_html` — so block-directive subtrees that the macro
leaves alone (`<template pp-for>`, `<template pp-if>`,
`pp-teleport`, `<slot>`) are still in the index space; the plan
just doesn't have entries for them.

`expr_src` is the unparsed expression. The runtime applier
parses each one once via `expr::parse_cached` on first use and
caches the AST per-template (same shape as `for_plan.rs`'s
`CompiledBinding`).

`BindingKind::Class` is RFC-054 row-plan compatibility only —
keyed-list rows already emit it via `for_plan.rs` and the
runtime row-plan applier still recognises it. Template plans
emitted by this RFC always classify `:class` / `pp-bind:class`
as `Bind { arg: "class" }`, never as `Class`. The two paths
share install logic but stay textually distinguishable so a
single grep tells you which compiler emitted the entry.

### 5.2 Macro classification

A new `crates/pocopine-macros/src/template_plan.rs` walks the
existing `TemplateAst` (`template_parser::parse_strict` output)
in source order. For each element it first asks **is this a
known HTML5 native element?** — if not (custom-tag with a `-`,
unknown name), the element is a **whole-subtree boundary**:
every directive on it AND every directive on every descendant
falls through to the runtime walker, and the classifier stops
descending. This matches the way `pp-for` / `pp-if` /
`pp-teleport` / `<slot>` are handled (§6.2) and protects the
case where the non-HTML tag is a registered component whose
authored slot content contains native descendants — those
descendants must not be planned before `mount_component`
captures and materialises the slot. The macro intentionally
does **not** consult any "registered-component" table; the
runtime component registry is runtime state and would be
unsafe to project across the macro/runtime boundary.
Native-vs-non-native is a static lookup against the HTML5
element list and is the only eligibility gate v1 uses (see §6).

For native-tag elements the macro then classifies each
attribute:

| attribute (after RFC 020 normalisation) | classification | HTML |
|---|---|---|
| `pp-text="<expr>"` | `StaticBinding { kind: Text, … }` | stripped + `data-pp-text-managed` marker stamped |
| `pp-html="<expr>"` | `StaticBinding { kind: Html, … }` | stripped |
| `pp-bind:<arg>="<expr>"` (and `:<arg>`) | `StaticBinding { kind: Bind { arg }, … }` | stripped |
| `pp-show="<expr>"` | `StaticBinding { kind: Show, … }` | stripped |
| `pp-on:<event>[.<mod>]="<expr>"` (and `@event[.mod]`) | `StaticListener { event, modifiers, … }` *iff* every modifier is in the §6 supported set | stripped |
| `pp-init="<expr>"` | `StaticInit { … }` (deferred — see §5.7) | stripped |
| `pp-ref="<name>"` | `StaticRef { … }` (installs via existing `refs::register`) | stripped |
| `pp-data="<name>"` | preserved on the rewritten HTML (walker reads it for scope binding) | preserved |
| `pp-cloak` | preserved; the walker's existing strip handles it | preserved |
| `pp-for` / `pp-if` / `pp-teleport` / `<slot>` | **whole-element boundary** — every attribute on the element AND every descendant attribute stays attribute-preserving on the HTML; the runtime walker owns the subtree | preserved |
| `pp-model` | preserved; runtime path stays in v1 (deferred — see §7.1) | preserved |
| `pp-route` | preserved; runtime path stays in v1 (deferred — see §7.3) | preserved |
| any listener with a modifier outside the §6 supported set | the whole listener is preserved | preserved |

`pp-text` is a special case: stripping it would break
`interp::scan_children`, which today refuses to interpolate
`{...}` syntax inside elements that carry `pp-text`. The macro
stamps a private marker attribute (e.g. `data-pp-text-managed`)
in its place; the interpolation scanner is amended to honour
the marker (§5.6).

When the macro sees a block-directive attribute it stops
descending into that element entirely — no plan entries for
that subtree, and every directive on the boundary element AND
its descendants stays on the HTML. The runtime walker's
existing recursive `bind` pass picks it up when
`mount_component` calls into it for the leftover scaffold.

### 5.3 HTML re-serialisation

After classification, the macro re-serialises the AST through
`markup5ever_rcdom`'s serializer with the classified attributes
**stripped** from each opening tag. The serialised output is
what `register_template` receives. Block-directive markers
(`pp-for`, `pp-if`, `pp-teleport`, `pp-data`, `pp-cloak`,
`pp-model`, `pp-route`, `<slot>`), every directive on or under
a **non-HTML-native tag boundary**, and any listener with an
unsupported modifier survive the serialisation pass intact —
those are attribute-preserving fallbacks (§6).

The pre-existing `compile_template` rewrites (`<root>` → `<tag>`,
role attribute injection per RFC 033) move from the runtime
side into the macro pipeline — they only need string rewrites
on the AST's serialised output.

### 5.4 Marker attributes for ownership tracking

The plan and the runtime walker coordinate **per-attribute**,
not per-element. Stripped attributes are owned by the plan
(they're not on the HTML so the walker can't see them);
preserved attributes are owned by the walker as today. An
element can carry both — e.g. a planned `pp-text` plus a
preserved `pp-model` — and both run.

`data-pp-planned` is **not** a blanket "skip me" marker. The
walker still runs its per-attribute scan on every node; it
just can't see the stripped entries because they aren't on the
HTML. The marker is informational only — devtools, mount-time
diagnostics, and the fail-fast counter (§5.6) read it to
correlate planned entries against DOM nodes. Walker behaviour
on preserved attributes (`pp-model`, `pp-data`, `pp-cloak`,
block-directive markers, anything not in the v1 envelope) is
unchanged.

`data-pp-text-managed` is a **functional** marker: the macro
stamps it where `pp-text` was stripped, and
`interp::scan_children` is amended to honour it identically to
the current `pp-text` check, so braces inside the planned text
don't get hijacked by interpolation.

Both attribute names are in pocopine's reserved private
namespace; authors must not use them.

### 5.5 Runtime apply path

```rust
// crates/pocopine-core/src/templates_plan.rs (new)
pub fn register_template_plan(name: &str, plan: &'static StaticTemplatePlan);
pub fn template_plan_for(name: &str) -> Option<&'static StaticTemplatePlan>;
pub fn apply_static_plan(
    root: &Element,
    scope_id: ScopeId,
    proxy: &JsValue,
    plan: &'static StaticTemplatePlan,
);
```

`mount_component` (`walker.rs:~350`) gains:

```rust
if let Some(plan) = template_plan_for(tag) {
    apply_static_plan(&root, scope_id, &proxy, plan);
    // bind() still runs for the scaffold so block-directive
    // subtrees and walker-owned fallbacks pick up their
    // existing runtime treatment. bind() simply iterates the
    // attributes that are present on each node — stripped
    // entries aren't there to skip. The data-pp-planned marker
    // (§5.4) is purely diagnostic; bind() does not consult it.
} else {
    // existing path: bind(el) recursive walk
}
```

`apply_static_plan` resolves each `node_path` to a DOM node by
walking the freshly-cloned subtree, then calls into helper
functions extracted from each existing directive module
(`directives::text::install`, `directives::bind::install`,
`directives::on::install`, etc.). The helpers take
`(el, scope_id, proxy, ast, …)` directly rather than going
through `DirectiveCall`, **and they MUST go through the same
machinery the runtime path uses** — `with_current_el`, effect
tracking, listener teardown via `walker::track_listener_on_with_opts`,
`refs::register`, `on_scope_unmount` cleanup. The applier is a
thinner caller of the same code, not a parallel implementation.

### 5.6 Fail-fast on stripped-directive errors

Once the macro strips an attribute from the rewritten HTML the
runtime walker can no longer recover it. Plan-registration
failure, `expr::parse_cached` errors, or a `node_path` that
doesn't resolve to a live DOM node for a stripped entry are
**framework bugs**, not author errors, and are treated as such:

- In debug builds: `panic!` with a message naming the
  template, the directive kind, and the resolved node path.
- In release builds: `console::error_1` with the same message
  and abandon the install for that single entry. The
  surrounding mount continues so a single misclassification
  doesn't take the whole app down.
- In tests: a counter in the runtime increments on every
  fail-fast event so the suite can assert "no plan failures
  observed during this test" alongside the existing DOM
  assertions.

Silent fallback to the runtime walker is **only** valid for
attributes the macro preserved on the HTML. Once an attribute
is stripped, the plan owns it; if it can't deliver, the build
or test must surface the failure.

### 5.7 Deferred `pp-init`

The current walker defers every `pp-init` until descendants
have been walked, refs registered, and child scopes exist.
The plan applier must preserve that ordering exactly.

The walker's existing pending state lives behind a private
constant (`INIT_PENDING_KEY`) accessed via `set_private` —
both unexported. The implementation must extract a public
helper, e.g.:

```rust
// crates/pocopine-core/src/walker.rs
pub fn defer_init_on(el: &Element, expr_src: &str);
```

that wraps the existing pending-set machinery. Both
`directives::init::run` (the runtime path) and
`templates_plan::apply_static_plan` (the planned path) call
it. The plan applier never duplicates the private key or the
private `set_private` call — there is exactly one place that
knows how `pp-init` defers, and it isn't the plan code. The
walker's existing post-order drain fires the handler.

### 5.8 Hot-reload

The macro keeps the existing dependency-pin emission
(`crates/pocopine-macros/src/lib.rs:1262`):

```rust
const _: &str = include_str!(#template_path);
```

This is what tells cargo to rebuild the consumer when the
`.poco` changes. The literal isn't used at runtime anymore —
it's a build-graph anchor only. `pocopine dev`'s file watcher
already triggers `cargo build`; that path is unchanged.

## 6. Eligibility (v1 envelope)

The v1 plan covers **only directives on known HTML5 native
elements**. Eligibility is a static lookup against the HTML5
element list — pocopine does not consult any runtime
component-registry state at macro time, and does not try to
prove tag identity any other way. Any element whose local name
is not in the HTML5 list is treated as walker-owned, full stop.
That cleanly excludes child-component prop targets (the case
where the parent walker's mount order is load-bearing) without
the macro needing to know what's a component.

**Eligible (planned + stripped):**

- `pp-text`, `pp-html`, `pp-show` on an HTML5 native element.
- `pp-bind:<attr>` / `:<attr>` on an HTML5 native element.
- `pp-on:<event>` / `@event` on an HTML5 native element when
  every modifier is in the supported set below.
- `pp-ref` on an HTML5 native element.
- `pp-init` on an HTML5 native element (deferred enqueue per
  §5.7).

### 6.1 Listener modifier grammar (v1 supported set)

Modifiers carried in `StaticListener.modifiers` are the
`String` tokens `directives::on::run` already parses today.
The v1 install helper must produce identical behaviour to
the current `run` for every supported token — this is a
parity requirement, not "approximately same":

| token | semantics |
|---|---|
| `prevent` | `ev.prevent_default()` |
| `stop` | `ev.stop_propagation()` |
| `self` | only fire when `ev.target == el` |
| `once` | `AddEventListenerOptions::set_once(true)` |
| `window` | attach to `window` instead of `el` |
| `document` | attach to `document` instead of `el` |
| `outside` | attach to `document` with `set_capture(true)`; only fire when target is outside `el` (and not in `data-pp-outside-exempt` selector) |
| key modifier | one of the RFC-013 keys (`enter`, `escape`, `tab`, `space`, `arrow-up/down/left/right`, `ctrl`, `shift`, `alt`, `meta`, single-letter, named-key) |
| `debounce` | followed by an optional next token parseable as `u32` (the ms count); default 300 — stored as two adjacent strings in the modifier slice, not one opaque string |

`capture` is **not** in the v1 supported set. The current
runtime parses `.capture` but never applies it (only
`.outside` ever calls `set_capture(true)`). Any listener that
carries `.capture` is preserved on the HTML and stays
walker-owned. Promotion of `.capture` is gated on the install-
helper extraction either implementing it or explicitly
dropping it from `directives::on::run` first.

### 6.2 Not eligible in v1 (attribute-preserved, walker-owned)

- **Every directive on or under an element whose local name is
  not in the HTML5 native list.** Non-HTML tags are a
  whole-subtree boundary, just like the block directives below:
  the boundary element and every descendant stay walker-owned.
  This protects the case where the non-HTML tag is a registered
  component and its authored slot content contains native
  descendants — those native descendants must not be planned
  before `mount_component` captures the slot. Promoting any
  subtree under a non-HTML tag is gated on the same RFC-058
  child-mount-ordering work as the parent prop-write case.
- Every directive on or under an element that carries `pp-for`,
  `pp-if`, `pp-teleport`, or `<slot>` semantics. These are
  **whole-element block boundaries** — the v1 macro treats the
  boundary element and every descendant as walker territory.
  Mixing planned and walker ownership inside a boundary is too
  easy to mis-order; promoting the boundary subtree is a
  follow-up RFC.
- `pp-model` (§7.1) and `pp-route` (§7.3).
- Any listener with at least one modifier outside the §6.1
  supported set — the whole listener is preserved on the HTML.

Anything that doesn't match falls through to the runtime walker
**by being preserved on the rewritten HTML**. The plan never
silently drops a stripped directive — see §5.6.

Authors don't opt in. The plan is additive: as the install-helper
extraction work continues (`pp-model`, `pp-route`, `.capture`
support, non-HTML-tag directives), the v2/v3 envelope grows
without an author-facing change.

## 7. Deferred work / follow-ups

These items are explicitly **not in v1**. Each is gated on a
piece of work that has to land first; once it does, the
respective directive can be promoted into the plan envelope
without any author-facing change.

### 7.1 `pp-model` semantics

`pp-model` is two-way: it installs an event listener on the
input *and* a binding effect on the value. The natural plan
shape is two entries per `pp-model` directive — one binding,
one listener. But `pp-model:<arg>` has type-aware semantics
(checkboxes, selects, custom-element pp-model) that today live
in `directives::model`. v1 leaves `pp-model` on the rewritten
HTML and lets the runtime handle it as today; promotion is
gated on factoring the model directive into a cleanup-safe
install helper.

### 7.2 `pp-route` handling

`pp-route` on `<a>` tags installs a click listener that hands
off to `router::navigate`. v1 leaves it on the rewritten HTML.
Promotion is **blocked** by two things in `directives::route`:

1. `crates/pocopine-core/src/directives/route.rs:59` calls
   `closure.forget()` — the route listener leaks past unmount.
   Cannot be promoted into a plan that promises cleanup-safe
   install before this is migrated to
   `walker::track_listener_on_with_opts`.
2. The directive needs a public `route::install` helper
   factored out of `route::run` matching the shape of the
   other install helpers in §5.5.

When both land, `pp-route` becomes a `StaticListener` with a
small dedicated kind (no sentinel modifier hacks).

### 7.3 Non-HTML-tag directives

Custom-element / unknown tags are excluded from v1 by §6's
HTML5-native lookup. Promotion is gated on an explicit
ordering contract for child-component mounts: the plan must
guarantee a parent `pp-bind:<prop>` write happens *after*
the child's `mount_component` call for that exact node, not
during the parent's pre-walk plan pass. RFC-058 (TBD) will
own that contract.

### 7.4 `.capture` listener modifier

Today the runtime parses `.capture` and never applies it (only
`.outside` ever calls `set_capture(true)`). v1 excludes
`.capture` from the supported set (§6.1). Either the
install-helper extraction adds real `.capture` support
(the easy path — wire it through
`AddEventListenerOptions::set_capture(true)`) or
`directives::on::run` drops the modifier entirely. Either
way the plan can then include `.capture` in the supported
set with no further design work.

### 7.5 Codegen'd Rust per template

Listed in §8.2 as the alternative-not-taken. If profiling
later shows the per-binding indirection through
`apply_static_plan` is the hot spot, a codegen'd path can be
added on top of the plan shape — the static-plan approach
doesn't lock that out.

### 7.6 Bitflag modifier representation

`StaticListener.modifiers` is a `&'static [&'static str]` in
v1 (§5.1, §6.1). Each install pays one string-equality per
modifier per token in the supported set — negligible at
typical listener counts, and the grammar stays open to
arbitrary key-modifier names without a schema bump.

If profiling later shows the modifier walk is hot, a compact
bitflag for the fixed common subset (`prevent`, `stop`,
`self`, `once`, `window`, `document`, `outside`) is a clean
follow-up: a `u8` lives next to the slice, the install helper
checks the flag bits first and only walks the slice for
key-modifier and `debounce` tokens (which need the string
form anyway). The plan ABI gains a field; existing emitters
default it to zero. Until profiling justifies it, the
string-slice form is what ships.

## 8. Rationale

### 8.1 Why extend RFC 054 instead of starting over

RFC 054 already proved out:

- `node_path: &'static [u16]` indexing works against cloned
  DOM subtrees.
- `expr_src: &'static str` + `expr::parse_cached` works for
  per-binding ASTs.
- **Attribute-preserved fallback** is a durable contract — when
  the macro chooses not to plan a row, the directive stays on
  the HTML and the walker handles it as today.

This RFC reuses all three. **Stripped planned directives are
owned by the plan and fail fast on framework bugs (§5.6) — they
do not silently fall back to the walker, because the walker
can no longer see them.** Attribute-preserved fallback (the
RFC 054 contract) remains durable for everything in the §6
not-eligible list. The walker's behaviour on those preserved
attributes is unchanged.

### 8.2 Why a static plan and not codegen'd Rust per template

A codegen'd `fn mount_my_component(root: &Element, …)` per
template would be the most aggressive form — full Rust, no
runtime indirection, no `&'static [u16]` indexing. It's also a
bigger change: the macro would generate a substantial Rust
function per component, and each function would carry its own
copies of the binding install helpers. Compile time and binary
size both go up before measurement is even possible.

The static-plan shape is denser (one `&'static [u16]` per
directive vs a function per template), shares install helpers
across all templates, and matches what RFC 054 already ships.
If profiling later shows the per-binding indirection is the
hot spot, a codegen'd path can be added on top; the plan
shape doesn't lock that out.

## 9. Outcomes

To be filled after implementation. Targets:

- **Walker test parity** — `crates/pocopine/tests/walker.rs`
  stays at 50/50 green; `crates/pine/tests/pine.rs` stays at
  102/102 green.
- **Bundle size** — `wasm-pack build --release --target web`
  on `examples/counter` and `jsbench/pocopine`, reported
  against a stable pre-RFC-057 baseline on the same release
  profile. Outcomes must report each contributor separately,
  not just the final number, so future passes have a baseline:
    - **raw template bytes removed** (sum of stripped attribute
      bytes across every component the build registers),
    - **cleaned HTML bytes added** (the re-serialised
      `register_template` payload — should be smaller, but
      whitespace and quoting can shift),
    - **plan metadata bytes added** (sum of `&'static
      StaticTemplatePlan` constants — `node_path` slices,
      `expr_src` strings, modifier string slices),
    - **final raw + gzip wasm delta**.

  Targets:
    - ≥10% raw wasm drop on counter,
    - ≥20% raw wasm drop on the bench harness.
- **Mount performance** — `./jsbench/benchmark.sh pocopine
  --browser chromium` reported separately from bundle size.
  Shouldn't regress; modest win on `runLots(10000)` from
  skipping per-node attribute scans is expected.

Implementation is not complete until the test suite covers
every council-required scenario:

- A planned `pp-text` value that contains `{...}` braces does
  not trigger interpolation (verifies §5.4's
  `data-pp-text-managed` marker).
- A planned `pp-init` handler observes child refs and child
  scopes exactly as the runtime walker does today (verifies
  §5.7's deferred enqueue).
- `pp-bind:<prop>` on a non-HTML tag (registered or unknown
  custom-element) stays attribute-preserved and walker-owned,
  along with every directive on its descendants; the parent's
  bind fires after the child's mount (verifies the §6.2
  non-HTML-tag whole-subtree boundary).
- A stripped directive whose `node_path` doesn't resolve fails
  fast as a framework bug — debug build panics, release build
  emits `console.error` and the test suite's plan-failure
  counter increments (verifies §5.6, no silent degradation).
- Listener and effect cleanup runs on unmount for planned
  directives identically to the runtime path (verifies §5.5's
  reuse of existing cleanup machinery).
- A mixed template where planned native nodes coexist with
  walker-owned `pp-for` / `pp-if` / `pp-model` /
  custom-element subtrees mounts and unmounts cleanly
  (verifies the §6 whole-element block-boundary rule).

## 10. Migration notes

None for authors — this RFC is invisible from the user side.

For framework contributors:

- `BindingKind` (currently in `directives/for_plan.rs`)
  gains variants. RFC 054's row-plan emission stays
  backward-compatible (only the `Text` and `Class` variants
  it uses today are unchanged).
- `StaticListener` gains a `modifiers` field. RFC 054 emits an
  empty slice for it.
- New `templates_plan.rs` module owns the registry. Existing
  `templates.rs` is unchanged.
- Directive modules grow `pub fn install(…)` helpers extracted
  from their `run(call: &DirectiveCall)` bodies. The `run`
  entry-points become thin shims over `install`.

## 11. Council deliberation

> The amendments below were the gating set for advancement.
> §3 (Goals), §4 (Non-goals), §5.2-5.7 (Macro classification,
> markers, fail-fast, deferred init), §6 (Eligibility — v1
> envelope), and §9 (Outcomes — required test evidence) have
> been rewritten to incorporate them. This appendix preserves
> the original council text for the record.

### 11.1 Verdict

Advance the RFC, but do **not** mark it Accepted until the v1
envelope is tightened. The compile-time-plan direction is the
right successor to RFC 050 and RFC 054: it removes raw template
bytes from the runtime path, reuses the proven `node_path`
metadata shape, and attacks the same setup cost that shows up in
the benchmark work.

The draft is too broad as written. A global `apply_static_plan`
before the recursive walker can silently change ordering around
child-component mounts, `pp-init`, interpolation, and cleanup
registration. Those are correctness issues, not implementation
details.

### 11.2 Required amendments before acceptance

1. **V1 must exclude child-component prop targets.**
   `pp-bind:<prop>` on a registered/custom child component depends
   on the current walker order: the child tag is mounted first, then
   the parent's directive writes through to the child's proxy. A
   single pre-walk static-plan pass runs too early. For v1, any
   directive on a custom-element tag, or any tag the macro can prove
   is a component, stays on the runtime walker. A later RFC/phase can
   add an ordered per-node applier once the walker can install planned
   directives after `mount_component` for that exact node.

2. **Stripped directives cannot have silent runtime fallback.**
   Once the macro removes an attribute from the serialised HTML, the
   generic walker can no longer recover that directive if plan
   registration or `node_path` resolution fails. Parse failures and
   path mismatches for stripped entries are macro/runtime bugs and
   must be treated as fail-fast diagnostics in tests, not as "skip and
   keep going" fallbacks. Silent fallback is only valid for attributes
   that the macro preserved in the HTML.

3. **`pp-init` must preserve post-order timing.**
   The current walker defers `pp-init` until descendants have been
   walked, refs are registered, and child scopes exist. A static plan
   may record `pp-init`, but applying it must set the same deferred
   pending state the walker uses today; it must not invoke the handler
   during the initial plan pass.

4. **`pp-text` must keep an ownership marker for interpolation.**
   Today `interp::scan_children` skips elements that still carry
   `pp-text`, so runtime text values containing `{...}` are not
   mistaken for template interpolation. If RFC 057 strips `pp-text`,
   the applier must stamp an equivalent private marker and the
   interpolation scanner must honor it.

5. **Planned effects and listeners must use the existing cleanup
   machinery.**
   Install helpers cannot become parallel directive implementations.
   They must preserve `with_current_el`, effect tracking, listener
   teardown, ref registration, and scope-unmount behaviour. This is
   especially important because the codebase has already fixed leaks
   caused by forgotten event closures.

6. **Block-boundary ownership must be whole-element for v1.**
   If an element carries `pp-for`, `pp-if`, `pp-teleport`, or `<slot>`
   semantics, the v1 macro leaves every directive on that element and
   its subtree to the runtime walker. Mixing planned directives on the
   boundary element with runtime ownership inside it is too easy to
   mis-order.

### 11.3 Accepted v1 shape after amendments

The first accepted implementation should plan only native-element,
non-block, non-component directives whose ordering does not depend on
child component mount:

- `pp-text`, `pp-html`, `pp-show`,
- native `pp-bind:<attr>` / `:<attr>`,
- `pp-on:<event>` / `@event` where modifiers are supported by the
  extracted `on` helper,
- `pp-ref`,
- deferred `pp-init`.

`pp-model`, child-component prop writes, block directives, slots, and
unknown modifiers stay attribute-preserving and walker-owned. That
smaller envelope still removes the raw template source from the wasm
runtime path and gives a measurable mount-path win without splitting
semantics.

### 11.4 Evidence required for implementation completion

Implementation is not complete until tests cover:

- a planned `pp-text` value that contains braces and does not trigger
  interpolation,
- `pp-init` seeing child refs and child scopes exactly as before,
- `pp-bind` to child components staying on the runtime path,
- a stripped directive with a bad node path failing as a macro/runtime
  bug, not silently degrading,
- listener/effect cleanup on unmount for planned directives,
- mixed templates where planned native nodes and walker-owned
  `pp-for` / `pp-if` / `pp-model` nodes coexist.

The outcome numbers in §9 should report bundle size and mount
performance separately, against the same jsbench harness and a stable
pre-RFC-057 baseline.
