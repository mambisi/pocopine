# RFC-040 — Text interpolation uses `{{expr}}`

Status: **Accepted** — supersedes the single-brace form in RFC-025.

## Problem

RFC-025 shipped inline text interpolation with single braces:

```html
<p>Hi {name}, you have {count} messages.</p>
```

Single `{...}` collides with everything authors paste into a
template:

- Rust code samples: `pub struct Foo { x: u32 }`
- JSON / TypeScript snippets in `<code>` blocks
- CSS-in-HTML samples: `@media (…) { … }`
- Shell/script samples: `{ cd; cargo build; }`

Shipped mitigation was a set of hard-coded bypasses (`<pre>`,
`<code>`, `<script>`, `<style>` skip interpolation) plus a
`pp-raw` opt-out attribute. That worked for docs templates but
was ad-hoc — it punts on nested cases (`<div><code>{ … }</code></div>`
where the outer div has real interpolation the same scanner
visits), and authors still couldn't use `{…}` in ordinary text
that happened to look like an expression.

## Design

Use double braces:

```html
<p>Hi {{name}}, you have {{count}} messages.</p>
```

Properties:

- **Unambiguous.** `{` in ordinary text is always literal. Code
  samples paste in untouched.
- **Familiar.** Same escape convention as Mustache, Vue
  (`{{mustache}}`), Handlebars, Django / Jinja, even Rust's
  `format!`. Authors coming from any of those recognise it.
- **Drops the hack.** No more tag allow-lists, no `pp-raw`
  escape hatch, no `\{` backslash escape machinery — just two
  braces delimit an expression, one brace anywhere else is
  literal text.

### Escaping

Literal `{{` in text (rare — mostly templating-code tutorials)
escapes with a backslash: `\{{literal}}`. The scanner consumes
the backslash and writes `{{literal}}` verbatim.

### Migration

Only two production templates used single-brace interpolation
at the time of this RFC — both in `examples/website`. Fixed
in the same commit; `grep -r "{[a-zA-Z_].*}" --include='*.poco'`
across the workspace gave a clean sweep for false positives in
embedded code blocks after the switch.

## Implementation

- `crates/pocopine-core/src/directives/interp.rs`:
  - Scanner looks for `{{` / `}}` instead of `{` / `}`.
  - Escape sequence: `\{{` → literal `{{`. `\}}` → literal
    `}}`. `\\` → literal `\`.
  - Dropped the `<pre>`/`<code>`/`<script>`/`<style>` tag
    allow-list and the `pp-raw` attribute opt-out — they're
    unnecessary now that single braces in text are always
    literal.

- `rfcs/rfc-025-text-interpolation.md`: marked superseded.

## Non-goals

- **Expression sublanguage changes.** Inside `{{…}}` the
  expression grammar is identical to RFC-024 (the shared
  expression evaluator used by `pp-bind`, `pp-on`, `pp-text`).
  Nothing about what you can write between the braces changes.
- **Migrating other frameworks' syntaxes.** Vue uses
  `{{mustache}}`; this RFC doesn't try to match their filter
  pipe (`{{ foo | uppercase }}`) or any other Vue-specific
  grammar.

## Verification

1. `cargo test -p pocopine-core` — the interp scanner's unit
   tests pass with double braces, single braces treated as
   literal.
2. `wasm-pack test --firefox --headless crates/pine` —
   regression: existing Pine primitives don't use interpolation
   in their templates, so they should be unaffected.
3. Manual in-browser: open the website; `{{actions_fired}}` on
   the dropdown-menu demo increments as expected; code samples
   in the tutorial render literally.
