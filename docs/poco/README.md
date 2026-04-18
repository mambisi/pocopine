# `.poco` — pocopine component templates

A `.poco` file holds **only** the HTML template for a component, with
`pp-*` directives. The Rust type lives in a `.rs` file beside it; the
styles live in a `.css` file beside it. Three files, one component.

Design constraint: **no mixed-language files.** Unlike Vue SFCs, we
don't embed Rust inside `<script>` or CSS inside `<style>`. Each
concern stays in its native file type so rustfmt, rust-analyzer,
clippy, and stylelint all work without plugins or wrappers. Future
editor tooling can do one focused thing: Rust-expression highlighting
inside `pp-*="..."` attribute values.

Docs in this folder:

1. [`01-format.md`](./01-format.md) — the `.poco` format itself, and
   the matching `.rs` + `.css` contract.
2. [`02-compilation.md`](./02-compilation.md) — how the `#[component]`
   macro's `template = "..."` + `style = "..."` arguments wire the
   three files together at compile time.
3. [`03-scoped-styles.md`](./03-scoped-styles.md) — the
   `data-pp-<hash>` + selector-rewrite strategy for CSS scoping.
