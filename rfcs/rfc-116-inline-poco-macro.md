# RFC-116: Inline `poco!` templates

**Status:** Implemented
**Crates:** `pocopine-macros` (macro + `#[component]` wiring), `pocopine-core` (`PocoTemplate`), `pocopine` (re-export), `pocopine-template-parser` (inline scanner), `pocopine-cli` (stylekit + LSP discovery)
**Relates to:** RFC 045 / RFC 050 §4.5 (compile-time template validation), RFC-058 (`template_inline` contract), RFC-111 (template-path validation), RFC 092 (stylekit)

## Summary

Add a function-like **`poco!`** macro whose body is a verbatim `.poco` template
— real HTML plus the usual poco sugar (`pp-*` directives, `{{ }}`
interpolation, `:attr` bindings, `@event` handlers) — validated at macro
expansion time and expanded to a **`PocoTemplate`**: a zero-cost newtype over
the verbatim `&'static str` source, so template-typed APIs can demand proof
of validation:

```rust
const CARD: PocoTemplate = poco! {
    <div class="card">
        <span pp-text="title"></span>
        <button @click="dismiss">{{ label }}</button>
    </div>
};
```

And let `#[component]` accept the same form as its template source-of-truth:

```rust
#[component(template = poco! {
    <div class="counter">
        <button pp-on:click="increment">+</button>
        <span pp-text="count"></span>
    </div>
})]
pub struct Counter {
    #[prop]
    pub count: i32,
}
```

This is the maud idea — templates inline in Rust, checked by the compiler —
**without** maud's braces DSL, without rsx-style mixed-Rust bodies (leptos /
tachys), and without a custom syntax (dioxus). The body is the same HTML that
would live in a `.poco` file, and the framework's own tooling (stylekit
extraction, `pocopine lsp`) discovers inline bodies the same way it discovers
file templates.

## Motivation

Today the only inline form is `template_inline = "..."` — a string literal
documented as a test-fixture affordance. It works, but the template reads as a
Rust string: escaped quotes, no visual separation, and for anything beyond a
line or two the escaping tax dominates. Small leaf components, library-internal
fixtures, and documentation examples deserve templates that read as HTML while
staying next to the Rust that owns them.

With `poco!` in place there is no reason to keep two inline forms:
**`template_inline` is removed** by this RFC (superseding RFC-061's "not
deleting `template_inline`" stance, which predates an HTML-native
alternative). One canonical pattern per decision: **inline-first** — `poco!`
is the default authoring form for small components, `.poco` files serve large
templates, and the boundary (default 150 lines) is owned by tooling, not
convention: `pocopine fmt` (RFC-117) auto-inlines beneath it and warns above
it. The docs guides (`docs/guides/poco/01-format.md`,
`docs/guides/components/01-structure.md`) reframe accordingly.

This RFC, together with RFC-117, inverts the prior framing: inline `poco!`
becomes the canonical form for small components, and `.poco` files serve the
templates large enough to deserve their own file.

## Design

### Source recovery

The macro body must lex as Rust tokens (a hard constraint of any function-like
macro — maud shares it). `poco!` does **not** interpret those tokens. It
recovers the raw source text between the first and last body token via
`Span::join` + `Span::source_text` and emits that string verbatim. The
workspace is already pinned to nightly for `proc_macro_span`
(`rust-toolchain.toml`), which is what makes whole-body recovery reliable:
whitespace, indentation, quoted attributes, and `{{ }}` interpolation all
survive byte-for-byte.

### Validation

- **Standalone `poco! { ... }`** runs `pocopine_template_parser::parse_strict`
  at expansion time. Malformed HTML fails the build with the same
  `annotate-snippets` rendering file templates get. The single-root rule is
  **not** enforced standalone — the returned template may be a fragment;
  root-shape rules belong to the consumer that gives the string component
  semantics.
- **`template = poco! { ... }`** desugars inside `ComponentArgs` into the
  existing inline-template pipeline (the internal slot `template_inline` used
  to feed), so the full pass ladder applies unchanged: parse + single-root
  (RFC 045), slot contracts (RFC 049), unknown-tag / forbidden-directive
  checks (RFC 060/063), row plans (RFC 054), template-path assertions
  (RFC-111), and dynamic-component assertions (RFC-112). Diagnostics anchor on
  the span of the HTML body, so errors squiggle the template itself.

`POCOPINE_TEMPLATES_LENIENT=1` downgrades expansion-time template errors to
warnings, exactly as for file templates (RFC 045 §9).

### The `PocoTemplate` type

```rust
// pocopine-core — wasm-safe, zero-cost
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PocoTemplate(&'static str);

impl PocoTemplate {
    #[doc(hidden)]
    pub const fn __new(source: &'static str) -> Self { Self(source) }
    pub const fn as_str(self) -> &'static str { self.0 }
}
// + Deref<Target = str> and Display
```

Standalone `poco!` expands to `PocoTemplate::__new("…")`, making the type a
compile-time proof-of-validation token: an API that takes `PocoTemplate`
cannot be handed an arbitrary unvalidated string (`__new` is `doc(hidden)`
expansion plumbing, the house convention for macro-only surface). It is also
the evolution point — precomputed metadata (root tag, role, plan references)
can ride the newtype later without breaking callers, which a bare
`&'static str` return could never absorb. `as_str()` / `Deref` keep interop
free, and every constructor is `const` so `const CARD: PocoTemplate = poco! { … }`
works.

No public API consumes templates by value today (the component pipeline is
macro-internal), so the first consumers are the forward-looking surfaces —
dynamic registration, SSG, `pp-surface`. The type lands now because changing
`poco!`'s return type after it ships would be a breaking change.

The `template = poco! { … }` attribute form is desugared before expansion and
never materializes a value; the type contract belongs to the standalone
macro. `PocoTemplate` is re-exported at `pocopine::PocoTemplate` and in the
prelude.

### Editor / speculative expansion

rust-analyzer runs speculative expansions where span provenance is unavailable
(`source_text` returns `None`). Following the Tier-3 precedent from template
path resolution: the expansion stays buildable — a lossy token re-print is
emitted, validation is skipped, and a `#[deprecated]`-const warning explains.
A real cargo build always has provenance, so the lossy path never ships.

### Tooling discovery

Inline templates are first-class for the framework's own tooling, not just for
rustc. The shared piece is a **scanner** in `pocopine-template-parser` behind a
new `inline-scan` cargo feature:

```rust
pub struct InlineTemplate {
    /// Byte range of the body between the `poco!` delimiters,
    /// into the scanned Rust source.
    pub body_range: std::ops::Range<usize>,
}

/// Lex `rust_source` with proc-macro2's fallback lexer
/// (`span-locations`) and return every `poco ! ⟨group⟩` body,
/// in source order. Unlexable input returns an empty list —
/// rustc owns reporting broken Rust.
pub fn scan_inline_templates(rust_source: &str) -> Vec<InlineTemplate>
```

It matches the token triple `poco` `!` `⟨delimited group⟩` anywhere in the
token tree — expression position and `#[component(template = poco! { … })]`
attribute position fall out of the same walk, and path-qualified
`pocopine::poco!` matches because only the trailing triple is inspected.
Because the scan operates on lexed tokens (never regex), commented-out code
and string literals containing `poco!` are excluded for free. Body ranges are
delimiter-to-delimiter, so template byte `i` maps to file byte
`body_range.start + i` — diagnostics point into the real `.rs` file. The
feature gate keeps proc-macro2/`span-locations` out of the proc-macro build
graph (`pocopine-macros` depends on the parser without it; resolver v3 does
not unify tool features into that graph).

**Stylekit (RFC 092).** The CLI's collector walks `src/**/*.rs` alongside
`src/**/*.poco`; each recovered body becomes one source unit (named after its
`.rs` file, carrying the body offset) feeding `compile_project` unchanged.
Utility classes written in `poco!` bodies generate CSS exactly like classes in
file templates, in one-shot builds and dev-watch recompiles alike (`.rs` edits
already trigger the watch tick).

**LSP.** `pocopine lsp` branches on the document: `.poco` buffers keep the
current path; `.rs` buffers are scanned, each body parsed, and diagnostics
published offset-shifted against the `.rs` URI. Hover and completions inside a
body reuse the existing `.poco` machinery through the same position mapping.
For editors that don't route `.rs` buffers to the server, the existing
`**/*.rs` file watcher re-scans changed files from disk and publishes
diagnostics on save.

### What `#[component]` accepts after this RFC

| form                                | meaning                                 |
| ----------------------------------- | --------------------------------------- |
| `template = "Foo.poco"`             | file template (canonical)               |
| `template = poco! { <div>…</div> }` | inline template, full validation ladder |

`template` stays the one source-of-truth key; its two forms are
self-disambiguating and duplicates are rejected across both. The
`template_inline` argument is **removed**: passing it is a hard error with a
migration hint pointing at `template = poco! { ... }`.

## Authoring constraints

The body must lex as Rust tokens. This is the one real cost of bare-HTML
bodies, it is **upstream of this macro** (rustc's lexer runs before any proc
macro), and it is therefore not something better diagnostics inside `poco!`
can soften. The table below is measured, not assumed — every row was compiled
against the pinned toolchain.

**Structure and directives are unaffected.** All of these lex:

`{{ interpolation }}` · `@click="x"` · `:title="x"` · `pp-if="x"` ·
`pp-on:click.debounce.300="x"` · `pp-model.number="x"` · `<br/>` ·
`<!-- comments -->` · `<!DOCTYPE html>` · `&amp;` entities · any URL inside a
quoted attribute value · CJK and accented text (`café`, `日本語`) ·
`a -> b` · `5 / 2` · `1.5.3` · `0x1F` · `5px` · `$9.99` · `100%` · `#tag`

### Quoted text — the escape hatch

A Rust string literal is a **single token**, so the lexer never inspects its
contents. Wrapping a hostile run of text in quotes therefore makes every
constraint below disappear:

```html
<p>"Don't stop — © 2026 · ⌘K 🎉"</p>
<p>"5 < 10 & rising"</p>
```

A string literal in **text position** is unquoted at expansion time, its Rust
escapes decoded, and the result HTML-escaped into the template as **static
text** — no runtime interpolation, no entity knowledge required from the
author. Quoting is per-run, not per-node: `<p>Hello "don't" world</p>` mixes
freely.

Disambiguation is positional and token-based: a string literal preceded by `=`
is an attribute value and is left verbatim (`class="card"` is untouched);
anywhere else at the top level it is text. Literals **inside `{{ }}` are never
touched** — those belong to the expression parser.

Which yields the second form, free of charge: `{{ "Don't stop" }}` already
works, because `pocopine-expr` parses string literals. It costs a reactive
interpolation node for a constant, so prefer bare `"..."` for static copy and
use the `{{ }}` form when already inside an expression.

With quoting available, the table below is a fallback for authors who prefer
entities, and `pocopine fmt` prefers quoting when it inlines.

**Otherwise, text content must avoid these**, each with its escape:

| construct in text                | escape as                        |
| -------------------------------- | -------------------------------- |
| `'` — `don't`, `students'`        | `&#39;`                          |
| non-ASCII **symbols**: `—…©€·←‹─⌘` | `&mdash;` `&hellip;` `&copy;` … |
| backtick `` ` ``                  | `&#96;`                          |
| emoji                            | `&#x1F389;`                      |
| `\`                              | `&#92;`                          |
| `//` in text (starts a comment)  | `&#47;&#47;`                     |
| a lone `{` or `"`                | `&#123;` / `&quot;`              |
| ident immediately before a quote | separate them                    |

The dividing line for non-ASCII is **letters vs symbols**: any script's
letters are valid Rust identifier characters and pass (`café`, `日本語`);
punctuation and pictographic symbols do not (`·`, `←`, `©`, emoji).

Measured against the repo's 359 real `.poco` templates, **275 (76%) lex
unchanged**; the 84 that don't are almost entirely typographic symbols in UI
chrome (`←` `‹` `·` `─` `…` `⌘` account for 70). Those are mechanically
escapable, and `pocopine fmt` (RFC-117) performs the escaping as part of
inlining, so the migration is not hand work.

### Making the failure legible

A violation surfaces as a rustc **lexer** error — `error: prefix 'don' is
unknown`, `unknown start of token: \u{b7}` — which never mentions pocopine and
which no macro can intercept. Since the canonical build path is the pocopine
CLI, that is where the gap is closed: `pocopine build` / `run` / `dev`
pre-lint `.rs` sources before invoking cargo, locating `poco!` bodies by raw
text scan (brace matching on text, so it works on input that does not lex) and
reporting hostile characters with file, line, column, the character, and its
entity — plus the quoting hatch and a `pocopine fmt --fix` hint. The dev
watcher re-checks each rebuild tick. Bare-cargo builds still get rustc's raw
message; that is the documented trade.

Two facts the implementation had to respect:

- The lint scans **characters, not tokens**. `don't` does tokenize — as `don`
  plus the lifetime `'t` — and is only rejected afterwards as a reserved
  prefix, so "it tokenized" does not imply "it compiles".
- Quoted runs are skipped, which is what keeps attribute values (already
  quoted) out of the report.

## Known limitations

- **rustfmt** leaves the body untouched (by design — the source is the
  artifact).
- **LSP goto-definition and self-identifier checks** don't reach inline bodies
  in v1: both key off the `Counter.poco ↔ Counter` filename convention, which
  inline bodies don't have. Wiring body → owning `#[component]` struct is
  future work; compile-time validation (RFC-111) already covers the
  correctness half.

## Migration

`template_inline` has ~55 in-repo usage files, essentially all test fixtures
(`crates/pocopine/tests/**` including every trybuild `ui/*.rs`,
`crates/pine/tests`, `crates/pine-charts/tests`), plus
`pine-richtext`'s node-view manager, `examples/observability-frontend`, and
two published docs pages. Migration is mechanical
(`template_inline = "<div>…</div>"` → `template = poco! { <div>…</div> }`)
with two carve-outs:

- A fixture body that can't lex as Rust tokens (stray apostrophe, unbalanced
  quote in text — expected to be rare) either entity-escapes or moves to a
  `.poco` fixture file.
- Trybuild `.stderr` snapshots regenerate where spans/messages shift from the
  string literal to the HTML body.

RFC-058's contract test #4 (directives inside inline templates bind) retargets
to the `poco!` form. Phasing inside one branch: (1) macro + desugar land with
the internal slot intact, (2) mechanical migration commits per crate area,
(3) the `template_inline` key is removed and docs updated.

## Rollout

After this lands, the **vscode-poco** extension (sibling repo) gets a
follow-up: a TextMate injection grammar into `source.rust` scoping
`poco! { ... }` bodies as poco markup, so inline templates highlight exactly
like `.poco` files. The extension's language client picks up the new `.rs`
diagnostics from `pocopine lsp` without changes; highlighting is the only
extension-side work.

`pocopine fmt` (RFC-117) follows as its own branch and mechanizes the
inline-first boundary; this RFC's migration does not depend on it.

## Alternatives rejected

- **rsx / mixed-Rust bodies** (leptos `view!`, tachys): rejected outright —
  pocopine templates are HTML, and the runtime parser is the single template
  ingestion path (see the template + bundle-size strategy).
- **String-literal macro** (`poco!(r#"..."#)`): that's what `template_inline`
  was; no HTML-native authoring win, so no string form survives.
- **A new attribute key** (`template_poco = ...`): overloading `template`
  keeps one key as the template source-of-truth and makes the two forms
  self-disambiguating at the call site.

## Non-goals

- No `template!` alias — one name.
- `PocoTemplate` stays `&'static str`-backed, with no public unchecked
  constructor — one arrives only when a public API actually consumes
  runtime-built templates.
- No change to the runtime pipeline: `poco!` output feeds the same
  `compile_template` / `register_template` path as every other template.
