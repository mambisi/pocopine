---
title: "Inline templates: real HTML in Rust, and the lexer wall we hit"
description: "poco! puts a template next to the code that owns it — bare HTML, no DSL, no rsx. Getting there meant measuring which of our 359 templates Rust's tokenizer would reject, finding an escape hatch that costs nothing, and deciding twice not to be clever."
date: 2026-08-15
---

# Inline templates: real HTML in Rust

Pocopine templates have always lived in their own files. `Counter.rs`
sits next to `Counter.poco`, and the `#[component]` macro wires them
together. That is still the right shape for a template of any size —
but it is a lot of ceremony for a component whose whole template is a
button.

So templates can now be written inline:

```rust
#[component(template = poco! {
    <div class="counter">
        <button pp-on:click="increment">+</button>
        <span pp-text="count"></span>
    </div>
})]
pub struct Counter {
    pub count: i32,
}
```

That is ordinary HTML with the usual `pp-*` directives. Not a
braces DSL, not Rust expressions interleaved with markup, not a
bespoke syntax. The same thing you would put in the file, in the
place where the file would otherwise have to exist.

`poco!` also works on its own, returning a `PocoTemplate`:

```rust
const ROWS: PocoTemplate = poco! { <li>a</li> <li>b</li> };
```

This post is about how it works, and about the two places where the
obvious implementation was wrong.

## One parser, not two

The tempting way to build this is to parse the tokens. Rust hands the
macro a `TokenStream`; you walk it, recognise `<`, `div`, `class`,
`=`, and build an HTML tree. Every `html!`-style macro in the Rust
ecosystem does some version of that.

We do not, and the reason is that pocopine already has an HTML parser.
`.poco` files go through html5ever, and every compile-time check —
the single-root rule, slot contracts, `pp-for` row plans, template
path validation — is written against the AST it produces. A second
parser means two implementations of "what is a pocopine template", and
the day they disagree is the day an inline template compiles and its
file twin does not.

So `poco!` does not interpret its body at all. It joins the spans of
the first and last token, asks rustc for the original source text
covering that range, and hands the string to the same html5ever parser
that reads `.poco` files:

```
poco! { <div class="card">{{ label }}</div> }
   │
   │  Span::join(first, last).source_text()
   ▼
"<div class=\"card\">{{ label }}</div>"   ← byte-identical to what you typed
   │
   │  pocopine-template-parser (html5ever)
   ▼
  the same AST a .poco file produces
```

Whitespace, indentation, `{{ }}` interpolation — all of it survives,
because the macro never rewrote anything. And `#[component]` desugars
the inline form into the existing pipeline *before* validation runs,
so the whole ladder applies unchanged, with errors pointing at the
offending line inside your `.rs` file.

Before building this we checked whether some crate already did the
verbatim-recovery trick — a sweep of the full 1,281-crate
proc-macro-helper category on crates.io, plus GitHub code search for
`Span::source_text`. Nothing does. The closest is rstml's
`rawtext-stable-hack`, which exists to work around a limitation that
does not apply to us, since pocopine already pins a nightly toolchain
for other span work. The recovery itself is about thirty lines.

## The wall

Here is the part that does not appear in any `html!` macro's README.

A macro body has to lex as Rust tokens. Not *parse* — the macro never
parses it — but **lex**, because rustc tokenizes the whole file before
any proc macro runs. And ordinary English prose does not always
survive that step:

```rust
poco! { <p>don't stop</p> }
```

```
error: prefix `don` is unknown
```

That is not our error. It happens upstream of macro expansion, so no
diagnostic we write can improve it or even see it. Rust 2021 reserved
prefixed identifiers, and `don't` reads as the identifier `don`
followed by the lifetime `'t`.

The question is how much this actually costs, and the honest way to
answer it is to measure rather than guess. We ran every real template
in the repository through the Rust lexer:

> **275 of 359 templates (76%) lex unchanged.**

The 84 that fail are almost entirely typographic characters in UI
chrome — `←` `‹` `·` `─` `…` `⌘` account for 70 of them — plus
apostrophes, backticks and emoji. The dividing line for non-ASCII is
letters versus symbols: `café` and `日本語` are valid Rust identifier
characters and pass; `©` and `→` are not tokens at all.

Everything that makes a pocopine template a pocopine template is
fine. `{{ }}`, `@click`, `:title`, `pp-if`,
`pp-on:click.debounce.300`, self-closing tags, comments, entities,
URLs in attribute values — all lex cleanly. It is only prose.

## The escape hatch that costs nothing

A Rust string literal is a single opaque token. The lexer never looks
inside one. So the fix is to let a template quote the text that needs
it:

```poco
<p>"Don't stop — © 2026 · ⌘K 🎉"</p>
```

At expansion the quotes are removed, the Rust escapes decoded, and the
result HTML-escaped into the template as **static text** — no runtime
interpolation, no entity juggling. `"5 < 10 & rising"` renders
correctly without the author thinking about `&lt;` at all.

The disambiguation is positional and cheap: a string literal preceded
by `=` is an attribute value and is left exactly as written, so
`class="card"` is untouched. Literals inside `{{ }}` belong to the
expression parser and are never touched either — which means
`{{ "Don't stop" }}` also works, at the cost of a reactive node for a
constant.

Quoting is per-run, not per-element, so `<p>Hello "don't" world</p>`
mixes freely. And prose that merely *looks* like Rust is fine
unquoted: `'tis` and `the 'static lifetime` lex as lifetime tokens,
so they need no quotes at all.

## Making the failure legible

An escape hatch does not help if the error that sends you to it is
`error: unknown start of token: \u{b7}`. That message names no
template, no component, and no fix.

We cannot intercept it — again, it fires before our macro exists. But
the pocopine CLI owns the build, so it can look first:

```
$ pocopine build

Error: inline templates contain text the Rust lexer cannot read.

  src/lib.rs:41:28
      symbol `—` — write `&mdash;` or quote the run

  src/lib.rs:41:30
      symbol `©` — write `&copy;` or quote the run
```

`build`, `run` and `dev` scan `.rs` sources before invoking cargo, and
the dev watcher re-checks on every rebuild — since typing prose into a
template is exactly how this breaks mid-session.

That scan is textual rather than token-based, which looks backwards
next to everything above until you notice the constraint: the files
that need this diagnostic are precisely the files that do not
tokenize. A token walk goes blind exactly when it is needed.

## Tooling had to follow

An inline template that Stylekit cannot see generates no CSS. An
inline template the language server cannot see gets no diagnostics.
Both tools walked the filesystem for `.poco` files, so on the day
`poco!` shipped, both would have quietly stopped working for anything
written inline — the worst kind of gap, because the template still
*looks* like a template.

They now share one discovery pass that matches the `poco` `!`
`⟨group⟩` triple over **tokens**. That choice matters: a `poco!`
inside a comment, or the string `"poco! { … }"`, is not an invocation
and is skipped for free. A regex would report both.

The interesting part is what happens next. Rather than extracting the
bodies, we **mask** the file — blank every byte outside a template
body, keeping newlines:

```
src/lib.rs                    masked view
─────────────────────────     ─────────────────────────
#[component(template =
    poco! {                   ␣␣␣␣␣␣␣␣
        <div class="p-4">             <div class="p-4">
    }                         ␣
)]                            ␣␣
struct Card;                  ␣␣␣␣␣␣␣␣␣␣␣␣
```

Line numbers and columns are unchanged, so Stylekit and the LSP run
their existing passes and report positions that land on the real `.rs`
file with no offset arithmetic. Neither tool learned that inline
templates exist. And because only body text survives, a Rust string
literal can never be mistaken for a class list.

One subtlety cost us a bug. The first version padded by *bytes*, which
keeps byte offsets stable — but LSP columns are counted in UTF-16 code
units, and `é` is two bytes and one column. Any template later on a
line containing non-ASCII Rust was reported a column to the right.
Padding is now per character, sized in UTF-16 units.

## Where a template lives is now a rule

With two equally valid homes, "inline or file?" becomes something to
argue about in review. So `pocopine fmt` owns the boundary:

```
pocopine fmt           # apply the rules
pocopine fmt --check   # CI: report only, non-zero exit
pocopine fmt --fix     # also apply rules configured as `warn`
```

Levels are clippy-shaped — `off` / `warn` / `fix` per rule in
`[package.metadata.pocopine.fmt]`. Under 150 lines a template is
pulled inline and its file removed; at or over it, an inline body is
reported so you can extract it. Indentation is handled in both
directions, so moving a template out and back reproduces the original
file byte for byte.

## Twice we decided not to be clever

Two features were designed, specified in the RFC, and then dropped
after measuring. Both are worth stating plainly, because the reasoning
is more useful than the features would have been.

**Automatic escaping.** The plan was for `fmt` to entity-escape or
quote hostile text on its way inline, making the rule total — every
small template inlines, no exceptions. Then we counted: 347 of 359
templates sit under the threshold, and **35 of them already contain
HTML entities**. Quoting a run that holds `&amp;` re-escapes the `&`,
and the page renders a literal `&amp;amp;`. That is silent content
corruption in a tool whose entire value proposition is that you run it
without reading the diff. A correct transform has to decode entities
first and distinguish text nodes from comments from unquoted attribute
values — that is a design, not a detail. So `fmt` reports templates it
cannot inline and changes nothing.

**Markup reformatting.** The same instinct says a formatter should
reindent the HTML too. But whitespace in HTML is semantics: a space
between two `<span>`s renders, and `<pre>` means what it says.
Reflowing those is worse than not formatting at all. Prettier's HTML
printer exists mostly to solve that one problem, and it is where
Prettier's HTML support has its longest-standing complaints. v1 moves
templates and indents what it moves; it does not reflow markup.

## What it cost, and what it did not

`template_inline = "..."` — the old string-literal escape hatch — is
gone. 206 bodies across 51 files moved to `poco!`. Six needed more
than a mechanical rewrite, all in one test file: five typographic
symbols now sit in quoted runs, and one single-quoted HTML attribute
(legal markup that Rust reads as a character literal) became double
quotes with entities.

One case looked like it would force the string form back. A test
helper generated 71 components through a `macro_rules!` that took the
template as a string literal — and you cannot write bare HTML at a
call site that passes it through a macro variable. So we checked
whether span recovery survives `macro_rules!` expansion. It does: the
forwarded tokens keep their call-site spans, and the body reads back
byte-exact. The helper now forwards token trees, and no string form
was needed.

The macro returns `PocoTemplate` rather than `&'static str` — a
const, zero-cost newtype. Nothing public consumes templates by value
today, so this is deliberate future-proofing: the type is a
compile-time proof that html5ever accepted the source, and it is where
precomputed metadata can ride later without breaking callers.
Changing a return type after it ships is a breaking change; adding a
field to a newtype is not.

## Reading further

- [RFC-116](https://github.com/mambisi/pocopine/blob/main/rfcs/rfc-116-inline-poco-macro.md)
  — the macro, the measured constraint table, quoted text
- [RFC-117](https://github.com/mambisi/pocopine/blob/main/rfcs/rfc-117-pocopine-fmt.md)
  — `pocopine fmt`, and the two deferred rules
- The compilation guide covers the day-to-day surface: both `template`
  forms, quoting, and the `fmt` config table.
