# RFC 045 — Single-root `.poco` templates enforced at compile time

| Field | Value |
|---|---|
| **Status** | Implemented (amendment in flight — see §9) |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 033](./rfc-033-primitive-roles.md), [RFC 050](./rfc-050-html5ever-compile-time-parser.md) §4.5 |

## 1. Summary

A `.poco` template must contain exactly one element root. Zero roots
or two-plus sibling roots are rejected at **Rust compile time** by
the `#[component]` macro — not deferred to app startup or first
render. The check is a `const fn` validator over the raw template
string, invoked from the `#[component]` expansion via a `const _: ()
= ...` item, so the failure surfaces as a normal `rustc` diagnostic
pointing at the offending component.

```html
<!-- Accepted -->
<root class="x">
  <slot/>
</root>
```

```html
<!-- Rejected at compile time -->
<div>first</div>
<div>second</div>
```

```text
error[E0080]: evaluation panicked: pocopine: template for component
`my-component` has more than one root element (pocopine templates
require exactly one root)
  --> src/my_component.rs:3:1
   |
 3 | #[component]
   | ^^^^^^^^^^^^ evaluation of `MyComponent::register::_` failed here
```

> The message carries the bug ("has more than one root element");
> the trailing `evaluation of … failed here` line is a structural
> artefact of const-eval panics and isn't configurable without
> attribute-macro-specific tooling. See §4.4.

## 2. Motivation

### 2.1 The bug class we're foreclosing

Picture a mid-sized app. An author writes what looks like a normal
`.poco`:

```html
<!-- PineMenuItem.poco — looks fine -->
<div class="pine-menu-item" @click="on_select">
  <slot/>
</div>
<span class="pine-menu-item-kbd" pp-text="shortcut"></span>
```

They were probably thinking "emit two sibling boxes, flex the
parent." The component mounts, the first `<div>` renders with its
click handler, and the `<span>` *also appears in the DOM* because
the browser parses it into the fragment. Visually it works.

Then keyboard shortcuts stop updating. The author changes
`shortcut` on the component and the `<span>` text never changes.
`pp-text` "doesn't work." They spend an afternoon in devtools,
sprinkle `console.log`, and eventually find that the `<span>` has
no `pp-data` attribute, no scope binding, no effect watching
`shortcut`. It's in the DOM but it's not **reactive** DOM. From
the runtime's perspective, that `<span>` may as well be a static
string.

Every pocopine directive on the stranded second root silently does
nothing: `pp-text`, `pp-show`, `@click`, `:class`, `pp-for`, slot
wiring, `pp-model`, `pp-ref`. The author sees output, assumes the
template is sound, and debugs the wrong layer.

### 2.2 Why this happens (walker mechanics)

Mounting a component is a three-step dance:

1. `compile_template` splices `pp-data="<name>"` into the **first
   opening tag** of the raw template string — full stop. Anything
   after that first element's opening tag is pass-through text
   with no marker.
2. The walker does `first_element_child()` on the parsed template
   fragment, then clones that one element into the DOM.
3. Every directive binding, every slot materialization, every
   lifecycle hook, every scope registration is keyed off that one
   rooted subtree.

Sibling roots fall off the edge of step 2. They never receive a
scope id, so:

* `:foo="x"` on the second root isn't reactive.
* `@click` isn't wired.
* `pp-for` doesn't iterate.
* `pp-ref` doesn't register.
* `on_mount` / `on_ready` do run (they fire per-component, not
  per-element) but they operate on `self` whose element graph
  doesn't include the orphan — so `refs::get` returns `None`.
* Unmount doesn't clean up siblings because they were never bound
  to the scope.

Nothing in this pipeline raises an error. It just… renders half
of what the author wrote.

### 2.3 The cost today

A sampling of the pain this already imposes:

* **Silent divergence.** The rendered DOM and the `.poco` source
  disagree. Reviewers can't catch it by reading the template —
  they'd have to run it.
* **Debugging time is high and skill-dependent.** Spotting
  "second root is an orphan" requires knowing walker internals.
  A new author has no reason to suspect the template layer.
* **The failure mode gets worse over time.** As components grow
  (add a `pp-model`, add `pp-for`, wire a slot prop), reactivity
  that used to work "by coincidence" on the orphan quietly stops.
  The bug shifts load onto whatever feature the author shipped
  last, which confuses root-cause analysis.
* **Tooling can't help.** `cargo check` passes. `cargo test`
  passes. The dev server happily reloads. CI is green.
* **Our own invariant is fragile.** Every walker feature added
  downstream — roles (RFC 033), teleport (RFC 006), transitions
  (RFC 005), scope-based cleanup — *assumes* one root. The
  invariant is load-bearing but unenforced; future refactors can
  break it without any test turning red.

### 2.4 Why compile time, specifically

Three properties of the check change depending on when it runs.

|  | Walker (runtime, today) | `compile_template` (runtime, boot) | Proc-macro (Rust compile time) |
|---|---|---|---|
| Blocks shipping | No — silent | No — requires running the app | **Yes** — `cargo check` fails |
| Error span | WASM console, far from source | WASM console | `rustc` arrow at the `#[component]` |
| Cost | Zero (because it's absent) | Per component per app boot | Once per component per `cargo build` |
| Works in CI that doesn't run the app | No | No | **Yes** |

Compile time is the only row where the error is impossible to
ship. The other rows require someone to run the code and happen
to exercise the broken component; in a large app with lazy-mounted
routes, that can be weeks later.

### 2.5 Who this helps

* **New authors.** Get a readable `rustc` message the first time
  they try a two-root template, instead of a mystery non-reactive
  `<span>` days into building a feature.
* **Reviewers.** A template that compiles is guaranteed to be
  single-root, so reviews can skip that class of concern.
* **Downstream walker features.** Roles, teleport, transitions,
  scope cleanup all lean on the one-root invariant; making it
  enforced lets those features stop hedging against the "what if
  there are stray siblings" case.
* **Future us.** Fragment support (if we ever want it) becomes a
  deliberate, RFC'd relaxation of a known rule — not an accident
  of parser permissiveness.

## 3. Non-goals

* **No full HTML parser.** The validator is a byte-level
  pre-parser tuned for the `.poco` dialect: tag opens/closes,
  attribute-value quoting, HTML void elements, comments, doctype,
  and XML processing instructions. It does not validate arbitrary
  HTML correctness — malformed attributes, unclosed nested tags,
  missing quotes are all still the author's problem, surfacing at
  walker time as they do today.
* **No fragment support.** Multi-root templates are explicitly out
  of scope; the one-root invariant is load-bearing for the walker
  (`pp-data` placement, role rewriting, scope ownership) and for
  the author's mental model. A future fragment RFC would need to
  relax the walker first.
* **No lint for "empty" templates.** A template that is whitespace
  or a single comment is still rejected (no root element), but we
  don't distinguish "empty" from "multiple roots" — both fail with
  the same error message pointing at the component.
* **No `.poco` path in the error.** `const`-eval panic messages
  are static strings; the component name is baked in via `concat!`
  but the on-disk template path is not. The struct ident and the
  component name are sufficient to locate the file.
* **No semantic linting beyond root count.** "Root is a void
  element," "author hand-wrote `pp-data`," "`<slot>` is missing,"
  and similar richer checks are out of scope. They might be worth
  future RFCs, but each one tightens the `const fn` budget (see
  §4.5); we're not spending that budget on v1.

## 4. Design

### 4.1 Validator

A `pub const fn check_single_root(raw: &str) -> RootCheck` lives
in `crates/pocopine-core/src/templates.rs`, where `RootCheck` is
a small `#[repr(u8)]` enum:

```rust
pub enum RootCheck {
    Ok,       // exactly one element root
    Missing,  // zero element roots (empty / comment-only / stray text)
    Multiple, // first root parses fine, but sibling content follows
}
```

Distinguishing the two failure cases is a deliberate choice (see
Codex feedback) — "no root element" and "more than one root
element" are genuinely different bugs with different fixes, and
the cost of branching is one extra `match` arm in the macro.

The validator walks three phases:

1. **Leading noise.** Whitespace, `<!-- ... -->`, `<!DOCTYPE ...>`,
   and `<?xml ... ?>` are skipped before the root is sought. If
   we reach end-of-input or stray non-element text, return
   `Missing`.
2. **Root span.** Track a depth counter across opening / closing
   / self-closing tags until depth returns to zero. That marks
   the end of the one legal root.
3. **Trailing noise.** Whitespace and `<!-- ... -->` are allowed
   after the root closes; any element, text, doctype, or PI
   returns `Multiple`.

Rules the validator applies while walking:

* **Self-closing**: `<foo/>` counts as one complete element.
* **Void elements**: `area`, `base`, `br`, `col`, `embed`, `hr`,
  `img`, `input`, `link`, `meta`, `source`, `track`, `wbr` are
  treated as implicitly self-closing when written without `/>`,
  so `<img>` can be the sole root or appear inside a root without
  throwing depth tracking off.
* **Nesting**: opening/closing tag pairs adjust the depth counter.
  The root is considered complete when depth returns to zero.
* **Attribute quoting**: `>` inside `"…"` or `'…'` is not treated
  as a tag terminator (same rule `inject_pp_data` already uses).
* **Unquoted attribute values**: HTML permits `<div class=foo>`.
  The validator treats an unquoted value as ending at the next
  ASCII whitespace or `>`, whichever comes first, so these parse
  correctly. We do not attempt to validate quoting style —
  pocopine style guides can prefer quoted values, but the
  compile-time check won't fail on legal HTML.

The `<root>` placeholder used with `role = "..."` (RFC 033) is
treated as an opening/closing tag like any other, so role-based
primitives validate identically before the rewrite step.

### 4.2 Wiring

In `crates/pocopine-macros/src/lib.rs`, next to the existing
`register_template_stmt`, the macro emits a `const _` item that
invokes the validator on the `include_str!`-loaded template and
branches on the result:

```rust
const _: () = match ::pocopine::__private::check_single_root(
    include_str!("MyComp.poco"),
) {
    ::pocopine::__private::RootCheck::Ok => (),
    ::pocopine::__private::RootCheck::Missing => ::core::panic!(concat!(
        "pocopine: template for component `",
        "my-comp",
        "` has no root element ",
        "(pocopine templates require exactly one root)"
    )),
    ::pocopine::__private::RootCheck::Multiple => ::core::panic!(concat!(
        "pocopine: template for component `",
        "my-comp",
        "` has more than one root element ",
        "(pocopine templates require exactly one root)"
    )),
};
```

The panic fires during `const` evaluation — before codegen — so
`rustc` surfaces it as a `const` eval error with the span of the
`#[component]` attribute / template literal. `concat!` folds the
literal pieces at macro-expansion time so each arm's message is
`'static`, which is what const-context `panic!` requires.

### 4.3 Where the validator is invoked

Once, per component, during Rust compilation of the owning crate.
The validator is not called at runtime: `compile_template` stays
exactly as today (no runtime short-circuit, no extra per-mount
cost). A debug-assertion fallback is deliberately not added — the
compile-time error is the one canonical signal.

### 4.4 Diagnostic ergonomics

On current Rust, the failure surfaces as:

```text
error[E0080]: evaluation panicked: pocopine: template for component
`<name>` has <reason> (pocopine templates require exactly one root)
  --> path/to/file.rs:N:1
   |
 N | #[component]
   | ^^^^^^^^^^^^ evaluation of `<Component>::register::_` failed here
```

Our panic message appears on the first line — `E0080: evaluation
panicked:` is the const-eval preamble but it sits *before* the
actual message, which makes the diagnostic readable on a first
glance. The second line (`evaluation of … failed here`) is
boilerplate from the const-eval machinery and can't be removed
without attribute-macro-specific tooling.

Takeaway for docs and reviewers: the pocopine error is on the
top line alongside `E0080`; the underlined `#[component]` span
tells you *which* component is broken.

If this ever turns into an onboarding papercut, a follow-up RFC
can move the check to an attribute-macro-level `syn::Error`
(which would give us our own error header and a precise span),
at the cost of reading the template file ourselves at macro time
(see §6 for why we're not doing that now).

### 4.5 Future-proofing and the `const fn` ceiling

Using `const _: () = { ... }` ties the validator to what `const
fn` supports *today*. That's fine for v1 — the algorithm is
genuinely small (byte walk, depth counter, quote tracker) and
every relevant primitive is already const-stable.

What this commits us to, explicitly:

* Any future extension ("root must not be a void element," "the
  author must not hand-write `pp-data`," "the root must be a
  known HTML element or a custom tag," etc.) has to stay inside
  whatever `const fn` permits at the time. `const fn`
  capabilities grow every release, so this ceiling rises, but
  it's still a ceiling.
* If a future check legitimately can't fit — e.g., anything
  needing real HTML parsing, attribute-value expression parsing,
  or string-allocating error messages — we take that as the
  signal to graduate to the attribute-macro path (§4.4). At
  that point we pay the `syn::Error` complexity once and inherit
  full formatting flexibility.

For v1 we accept the ceiling knowingly: one rule, small
algorithm, minimal attack surface on the compile path.

## 5. Migration

None. Every `.poco` file currently shipped in `crates/pine`,
`crates/pine-icons`, and the `examples/` tree has a single root
(checked during design). New components continue to be written
the same way.

Authors who accidentally introduce a second root element get a
Rust compile error on the next `cargo check` instead of a silent
render that drops half the template.

## 6. Alternatives considered

* **Runtime panic in `compile_template`.** Would catch the bug at
  app startup but not during `cargo check`; CI that only builds
  would let the mistake ship to an environment where it shows up
  as a WASM panic. Worse signal-to-source distance.
* **Proc-macro-time file read.** Read the template in the macro
  with `std::fs::read_to_string` using `CARGO_MANIFEST_DIR` plus
  a heuristic. Stable `proc_macro::Span::source_file()` doesn't
  exist; the heuristic would diverge from `include_str!`'s own
  path resolution and break in edge cases (nested modules, paths
  containing `..`). The `const _` approach piggybacks on
  `include_str!` and inherits its exact resolution.
* **Walker-time silent drop (status quo).** Already proven to
  generate hard-to-debug bugs (see §2). Explicitly rejected.

## 7. Implementation notes

1. `crates/pocopine-core/src/templates.rs` — add `pub enum
   RootCheck { Ok, Missing, Multiple }` and `pub const fn
   check_single_root(raw: &str) -> RootCheck` with byte-level
   parsing helpers (`const fn`-compatible versions of `find_byte`
   / `find_seq` / `find_tag_end`). Unit-test matrix covers:
   single root, leading comments / doctype / PI, self-closing,
   void elements, nested, trailing whitespace and comments,
   multiple sibling roots, stray trailing element, stray trailing
   text, empty string, comment-only input, unquoted attribute
   values, and `>` inside quoted attribute values. Failing cases
   assert the *specific* `Missing` vs `Multiple` variant, not
   just "not Ok."
2. `crates/pocopine/src/lib.rs` — re-export `check_single_root`
   and `RootCheck` from `__private`.
3. `crates/pocopine-macros/src/lib.rs` — emit the `const _` item
   next to `register_template_stmt`, sharing the same span as
   the existing `include_str!(#template_path)` so diagnostics
   point at the template literal. Three-arm `match` on
   `RootCheck` so the user sees the right failure message.
4. No documentation page yet — this RFC is the spec; once
   Implemented, a line goes into `docs/guides/poco/` describing the
   one-root rule and the two failure messages.

## 8. In-flight migration — RFC 050 §4.5

The v1 implementation (shipped and marked Implemented) uses a
`const fn check_single_root` called from a `const _: () =
match ...` block, so failure surfaces as an `E0080` const-eval
panic attached to the `#[component]` attribute.

RFC 050 §4.5 migrates the check to run inside the proc-macro
on the `html5ever`-produced `TemplateAst`:
`ast.element_roots().count() == 1`. The rule is the same; the
diagnostic improves to a pre-rendered `annotate-snippets`
block pointing at the offending `.poco` line. That migration
is the reference consumer for RFC 050's parser and lands on
the same branch.

Updated wiring in `#[component]`:

```rust
match ::pocopine_macros::template_parser::parse_strict(
    include_str!(#template_path),
    #template_path_for_diagnostics,
) {
    Ok(ast) => match ast.element_roots().count() {
        1 => { /* OK */ }
        0 => return emit_rendered_error("has no root element", …),
        _ => return emit_rendered_error("has more than one root element", …),
    },
    Err(parser_errors) => return emit_rendered_errors(parser_errors),
}
```

The `element_roots()` helper — not `roots.len()` — is the
canonical count, because RFC 050 surfaces text, comments, and
synthetic nodes at `TemplateAst.roots` for diagnostic reasons.

## 9. Developer escape hatch — `POCOPINE_TEMPLATES_LENIENT`

Strict-by-default (§1 + §4.8 in RFC 050) is the right policy
for CI and release builds. During local iteration it's a
papercut: a half-written template broken for 30 seconds stops
the whole workspace from compiling, making it hard to see
the error *in its rendered context* alongside the rest of the
running app.

The escape hatch is an environment variable read by
`#[component]` at macro expansion:

```text
POCOPINE_TEMPLATES_LENIENT=1 cargo check
POCOPINE_TEMPLATES_LENIENT=1 cargo run
```

When set to a truthy value (`1`, `true`, `yes` — case-
insensitive), **every template-strictness rule governed by
the `#[component]` macro downgrades to a compile-time
warning**:

- single-root violation (this RFC),
- forbidden self-close syntax (RFC 050 §4.8),
- any `parse_strict`-level parse error (RFC 050 §4.8),
- future static checks (RFC 049's slot contracts, etc.)
  that build on the same pipeline.

The mechanism is one `env::var_os("POCOPINE_TEMPLATES_LENIENT")`
call in the diagnostic-emission path: if lenient, the
`annotate-snippets`-rendered error is still composed but is
attached to a `syn::Attribute` as a deprecation-style warning
instead of a `syn::Error`. The template walker still produces
the AST, and `#[component]` still emits `register_template(…)`
using whatever the walker produced — so the app compiles and
the author sees the rendered error block alongside a live
render that reflects the parser's best-effort interpretation.

### 9.1 Constraints

- **Not a per-component attribute.** No `#[component(lenient =
  true)]`. A per-call flag invites drift — once set, nobody
  remembers to remove it — and inverts the default. The env
  var is ambient: toggle on for a session, forget it, the
  next clean build reverts.
- **Not a `Cargo.toml` feature.** Features are compile-time;
  toggling would rebuild every dependency and land in
  `Cargo.lock`. Env-var is a local-session knob that requires
  no workspace changes.
- **Not observable at runtime.** The flag only affects
  whether `#[component]` emits errors or warnings. The
  generated code is byte-identical in both modes; no
  `cfg(pocopine_lenient)` branches in runtime code.
- **CI must not set it.** Document the guard-rail in
  `docs/guides/poco/` and encourage teams to add a CI check that
  `POCOPINE_TEMPLATES_LENIENT` is unset in the build
  environment. Making it easy to opt out is worth the risk;
  the build-green-in-CI invariant is unchanged.

### 9.2 What it does not relax

- **`parse()` vs `parse_strict()` inside the parser module.**
  The module contract is unchanged — `parse_strict` still
  returns `Err` on any error. The env var is read by the
  `#[component]` macro layer, above the parser, to decide
  how to *surface* errors.
- **Correctness of the template.** A lenient build of a
  multi-root `.poco` still silently drops the second root at
  walker time (the runtime behaviour RFC 045 was created to
  eliminate). The warning is the only visible signal; the
  render is as wrong as it was before RFC 045 shipped. That's
  the trade — we're reconnecting the historical runtime
  behaviour for dev-only iteration.

### 9.3 Diagnostic shape in lenient mode

```text
warning: pocopine: template for component `my-menu` has more
         than one root element — only the first root will
         render at runtime (second and subsequent roots are
         dropped by the walker). Set `POCOPINE_TEMPLATES_LENIENT`
         to `0` or unset to turn this into a hard error.
  --> src/MyMenu.poco:14:1
   |
14 | <div>second root</div>
   | ^^^^^^^^^^^^^^^^^^^^^^ additional root — drops at runtime
```

The warning body explicitly names the consequence ("dropped
by the walker") so authors aren't surprised by the
difference between dev and CI.

### 9.4 Implementation notes

1. `crates/pocopine-macros/src/lib.rs` — a helper that reads
   `env::var("POCOPINE_TEMPLATES_LENIENT")` once per
   `#[component]` invocation and branches the diagnostic
   path. Re-read per macro call (not per workspace-compile),
   so toggling takes effect on the next `cargo check`.
2. Warnings are emitted as a `const _: () = { ... }` item
   containing a nested `#[deprecated(note = "…")]`-decorated
   `const` whose identifier is immediately "used" via
   `let _ = <ident>;`. rustc's deprecated-use lint fires on
   the use, surfacing the full multi-line `note` body as a
   warning with our `annotate-snippets`-rendered snippet
   intact. This is the cleanest way to emit a custom multi-
   line warning from a proc-macro on stable Rust.
3. `trybuild` tests gate both modes: strict mode rejects,
   lenient mode accepts-with-warning. The warning body is
   snapshotted via `trybuild`'s stderr comparison.

### 9.5 Known limitation — `deny(deprecated)` / `deny(warnings)`

The lenient-mode mechanism rides rustc's deprecated-use lint.
A consumer crate (or its workspace) that sets
`#![deny(deprecated)]` or `#![deny(warnings)]` in `lib.rs` /
`Cargo.toml` `[lints]` will turn the warning back into a
hard error — even with `POCOPINE_TEMPLATES_LENIENT=1` set.

This is acceptable because:

- Strict lint policies are a deliberate team choice. A team
  that's strict enough to `deny(warnings)` is also the
  audience least likely to need the lenient mode in the
  first place.
- The env var is for local iteration; teams that have
  warning-denying lints in CI still get the fail-fast they
  want.
- Adding `#[allow(deprecated)]` at the consumer level would
  silence ALL deprecated-use warnings, not just ours —
  worse cure than the disease.

Document this limitation under `docs/guides/poco/` alongside the
env-var's usage, with the workaround: authors on
`deny(deprecated)` projects either temporarily unset the
deny, or fix the template.
