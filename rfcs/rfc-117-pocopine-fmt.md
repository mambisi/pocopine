# RFC-117: `pocopine fmt` — template formatting + structural rules

**Status:** Implemented (v1 — see "What shipped")
**Crates:** `pocopine-cli` (subcommand + rules), `pocopine-template-parser` (scanner reuse)
**Relates to:** RFC-116 (inline `poco!` templates), RFC 092 (stylekit config precedent), RFC 050 (template parser)

## Summary

Add a **`pocopine fmt`** subcommand that formats `.poco` templates wherever
they live — files and inline `poco!` bodies alike — and applies a small,
clippy-style set of **structural rules** with developer-configured levels. The
flagship rule pair enforces RFC-116's inline-first canonical form:

- a component whose `.poco` file is under the threshold (default **150
  lines**) is rewritten to `template = poco! { ... }` and the file removed;
- an inline body at or over the threshold **warns**, and `--fix` extracts it
  to `<Struct>.poco`.

Like clippy, what runs and how hard it bites is up to the developer.

## Motivation

RFC-116 makes inline `poco!` the canonical form for small components and
`.poco` files the form for large templates. Without tooling that boundary is
convention-only: it drifts, and every code review re-litigates it. A formatter
that owns the boundary — and formats both template homes identically — makes
the canonical form self-maintaining, the same way rustfmt ended Rust style
debates.

## Design

### CLI

```
pocopine fmt [path]      # format + apply rules at configured levels
pocopine fmt --check     # CI mode: no writes, nonzero exit on diffs/warnings
pocopine fmt --fix       # additionally apply warn-level fixable rules
```

### Rule model — clippy-like, enumerable, no plugins

Configuration lives in the project's pocopine config:

```toml
[fmt]
inline_threshold = 150            # lines; 0 disables the rule pair
inline_small_templates = "fix"    # off | warn | fix   (default: fix)
extract_large_inline   = "warn"   # off | warn | fix   (default: warn)
```

Levels: `off` (rule skipped), `warn` (reported, exit-relevant under
`--check`), `fix` (rewritten by plain `pocopine fmt`; `--fix` promotes `warn`
rules to `fix` for that run). "Developer-defined rules" means choosing levels
and thresholds from this **built-in, enumerable table** — not a rule DSL or
plugin surface; new rules arrive as PRs to the table.

### Rules v1

1. **`inline_small_templates`** — a component with `template = "Foo.poco"`
   (explicit or convention-resolved) whose file is under `inline_threshold`
   lines is rewritten to `template = poco! { <body> }` and the `.poco` file
   deleted. Skipped with a warning when the template path doesn't resolve
   uniquely. The rewrite touches only the `#[component(...)]` attribute;
   rustfmt owns the surrounding Rust.
2. **`escape_inline_text`** — the transform that makes rule 1 total. Text
   content that would not lex as Rust tokens is entity-escaped on the way in
   (`'`→`&#39;`, `·`→`&middot;`, `←`→`&larr;`, `—`→`&mdash;`, emoji→`&#xNNNN;`,
   backtick→`&#96;`, `\`→`&#92;`, bare `//`→`&#47;&#47;`), preferring named
   entities where one exists and numeric otherwise. Escaping applies **only to
   text nodes and never inside attribute values** (already quoted, already
   safe). It is semantics-preserving — html5ever decodes the entities to the
   same characters — and reverses on extraction (rule 3), so a file →
   inline → file round trip returns the original characters. Measured need:
   84 of the repo's 359 templates (RFC-116).
   As a standalone lint on **hand-written** `poco!` bodies it reports the same
   hostile characters, which is the pre-lint `pocopine build` / `dev` run
   (RFC-116 "Making the failure legible") so authors get a pocopine-branded
   error instead of a raw rustc lexer error.
2. **`extract_large_inline`** — a `template = poco! { ... }` body at or over
   the threshold warns; `--fix` writes `<Struct>.poco` next to the `.rs`
   (deterministic convention name — the same one `#[component]` resolves) and
   drops the `template` argument entirely when the name matches convention.
3. **Template formatting** (always-on, safe): normalize element-structure
   indentation in `.poco` files and `poco!` bodies. Text nodes and
   whitespace-sensitive subtrees (`<pre>`, `<textarea>`) are preserved
   byte-for-byte. Formatting is idempotent (`fmt ∘ fmt = fmt`) and
   semantics-preserving — the parsed `TemplateAst` before and after must be
   equal modulo insignificant whitespace, enforced by test.

### Mechanics

- Inline bodies are discovered with RFC-116's `scan_inline_templates`;
  rewrites are span-precise text edits at token-verified ranges — never
  regex over Rust source.
- `style = "foo.css"` is untouched in both directions: CSS stays in `.css`.
- Rule 1 and rule 2 are exact inverses at the boundary, so a `fix`/`fix`
  configuration cannot oscillate: the threshold comparison is on the same
  line-count measurement both ways.

### Editor

`pocopine lsp` later exposes the formatter as a documentFormatting provider
for `.poco` buffers and `poco!` bodies (future work; the vscode-poco grammar
follow-up from RFC-116 is unaffected).

## Rollout

- Lands as its own branch/PR after RFC-116; the RFC-116 migration
  (`template_inline` removal) does not depend on it.
- Running `pocopine fmt` over `examples/` — flipping most example templates
  inline — is a deliberate, separate commit series once the rule is trusted,
  since examples are teaching material.

## What shipped

v1 implements the CLI, the config table, and both structural rules. Two parts
of the design above changed once they met the corpus.

**`escape_inline_text` is not implemented, and rule 1 is not total.** A
template that would not lex is **reported and left alone** rather than
rewritten. The measurement that decided it: 347 of the repo's 359 templates
sit under the threshold, and **35 of those already contain HTML entities**.
Quoting a text run holding `&amp;` re-escapes the `&`, so the page renders a
literal `&amp;amp;` — silent content corruption, in the one tool whose whole
value is that you can run it without reading the diff. Auto-escaping needs a
transform that decodes entities first and understands text nodes versus
comments versus unquoted attribute values; that is a design, not a detail, and
it waits. In exchange v1 never touches template content at all.

**Rule 3 (reindentation) is deferred** for the same reason: whitespace in HTML
is semantics, and a formatter that reflows `<pre>` or collapses significant
spacing between inline elements is worse than none. What v1 does do is indent
the body it moves — under the attribute on the way in, dedented on the way out
— which is what makes the round trip byte-stable.

Lexability is decided by the RFC-116 pre-lint's own character rules, so
"would the build reject this body?" has one answer in the codebase rather than
two that can drift.

## Alternatives rejected

- **User-extensible rule engine / DSL** — standing house rule: small
  enumerable surfaces, "PR welcome", no plugin protocols.
- **Aggressive HTML pretty-printing v1** (attribute reordering, line
  wrapping): deferred; v1 is conservative reindentation only, because
  whitespace in HTML is semantics and trust in the formatter comes first.
- **Bidirectional auto-fix by default** — extraction creates files; that's a
  developer decision, so it defaults to `warn` + `--fix`, clippy-style.
