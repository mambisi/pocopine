---
title: ".poco"
description: "The .poco template format — HTML plus pp-* directives, paired with sibling .rs and .css files."
---

# `.poco` — pocopine component templates

A `.poco` file holds **only** the HTML template for a component, with
`pp-*` directives. The Rust type lives in a `.rs` file beside it; the
styles live in a `.css` file beside it. Three files, one component.

**No mixed-language files.** Unlike Vue SFCs, pocopine doesn't embed
Rust inside `<script>` or CSS inside `<style>`. Each concern stays in
its native file type so rustfmt, rust-analyzer, clippy, and stylelint
all work without plugins or wrappers. An editor plugin needs to do
exactly one focused thing: switch to a Rust-expression grammar inside
`pp-*="..."` attribute values.

In this section:

1. [`01-format.md`](./01-format.md) — the `.poco` format itself, and
   the matching `.rs` + `.css` contract.
2. [`02-compilation.md`](./02-compilation.md) — how the `#[component]`
   macro's `template = "..."` + `style = "..."` arguments wire the
   three files together at compile time.
3. [`03-scoped-styles.md`](./03-scoped-styles.md) — the
   `data-pp-<hash>` + selector-rewrite strategy for CSS scoping.
4. [`04-expressions.md`](./04-expressions.md) — the pine-expr
   surface inside `pp-*="..."` attributes, what doesn't belong
   there, and the `#[computed]` / `#[watch]` patterns for
   derived state.
5. [`05-control-flow.md`](./05-control-flow.md) — `pp-show`,
   `pp-if`/`pp-else-if`/`pp-else` chains, `pp-match` enum
   dispatch, `pp-for`, and the comment anchors behind them.
