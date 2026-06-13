# RFC 024 — Expression-based directive values

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-008-event-handler-args.md`](./rfc-008-event-handler-args.md), [`rfc-012-expression-evaluator.md`](./rfc-012-expression-evaluator.md), [`rfc-020-shorthand-prefixes.md`](./rfc-020-shorthand-prefixes.md) |

## 1. Summary

Let `pp-on` / `@` directive values be **expressions**, not just
handler names. Function calls (`select(item.value)`) and
assignments (`open = !open`) both work; the current single-
identifier shape (`@click="on_click"`) stays valid.

```html
<!-- before -->
<li role="menuitem" data-value="copy" @click="on_menu_click">...</li>
<!-- where on_menu_click reads ev.target data-value and delegates. -->

<!-- after RFC-024 -->
<li role="menuitem" @click="select('copy')">Copy</li>
<li role="menuitem" @click="open = !open">Toggle nested</li>
<li role="menuitem" @click="copy($event); close()">Copy & close</li>
```

This collapses the vast majority of handler boilerplate Pine
components accumulate (every `toggle_*` / `close_*` / action
delegator method written purely because the directive value was a
string naming a handler).

## 2. Non-goals

- **Turing-complete template expressions.** Still no
  `for` / `if` / loops in values — those stay as directives.
- **Method chaining beyond one call.** `a().b()` isn't planned.
  A single call, plus `path = rhs`, plus sequences. If a handler
  wants to orchestrate more, write a method.
- **Macro / helper calls from inside expressions.**
  `@click="cx!('foo')"` doesn't parse. Component handlers only.
- **Mutation of global / external state.** Assignments target the
  evaluating scope's proxy. No `window.foo = 1`.
- **Optional chaining / nullish coalescing** (`a?.b`, `a ?? b`).
  Nice later but not required for the shape Pine needs.

## 3. Surface

A directive value is parsed as a **statement sequence** — one or
more statements separated by `;`. Each statement is one of:

- **Call**: `ident(arg1, arg2, ...)` — invokes a handler on the
  scope. Args are expressions evaluated left-to-right.
- **Assignment**: `path = expr` — writes `expr`'s value through
  the scope proxy's `set` trap at `path`. `path` is one or more
  dotted identifiers (`foo`, `foo.bar`, `user.profile.name`).
- **Expression**: any ordinary expression (evaluated and
  discarded — rare in `@event`, common in `pp-text` / `pp-bind`).

`$event` is bound inside `pp-on` values to the DOM event that
fired. Other magics (`$el`, `$refs`, `$store`, `$route`, `$id`)
resolve as usual.

### 3.1 Backward compatibility

- `@click="on_click"` — plain identifier. Treated as
  `@click="on_click($event)"` — single argument, the event. This
  preserves every existing handler that declared `(&mut self, ev:
  web_sys::Event)`.
- `@click="submit"` — same; a no-arg handler just ignores the
  extra `$event` arg via the `#[handlers]` dispatch shape.
- Old `@click.prevent="foo"` modifiers keep working unchanged —
  modifiers are a `pp-on` concern, independent of value parsing.

### 3.2 Non-`pp-on` directives

`pp-text` / `pp-bind` / `pp-show` / `pp-if` already use the expr
evaluator today and gain `+` / paths / ternary from RFC-012.
RFC-024 adds call + assign, which are **not** useful in those
directives (pp-text reads, doesn't assign). The grammar extends
uniformly; the evaluator rejects calls and assigns from inside a
`pp-text`-style read-only context by raising a parse error with a
clear message.

## 4. Grammar additions

```
stmt_seq     := stmt ( ';' stmt )*
stmt         := assign | call | expr
assign       := path '=' expr
call         := ident '(' arg_list? ')'
arg_list     := expr ( ',' expr )*
path         := ident ( '.' ident )*         # existing
expr         := ternary                      # existing, unchanged
```

`(`, `)`, `,`, `=` become lexer tokens. `=` (assignment) is
distinct from `==` (existing equality).

## 5. Semantics

### 5.1 Assignment

`path = expr`:

1. Evaluate `expr` → `JsValue`.
2. Walk `path` to the penultimate segment. `foo.bar.baz = v`
   resolves `foo.bar` on the scope proxy (subscribing both
   reads), then sets `baz` on that object via `Reflect::set`.
3. For single-segment paths (`foo = v`), `Reflect::set(proxy,
   "foo", v)`, which hits the scope's `set` trap and triggers
   reactivity like any other write.

### 5.2 Call

`ident(args...)`:

1. Evaluate each arg left-to-right.
2. `invoke_handler(current_scope_id, ident, args)` — same path
   `pp-on` already uses for the no-arg case. `FromHandlerArg`
   per RFC-008 converts each slot to the handler's declared
   type.

### 5.3 Statement sequence

`a; b; c`: evaluate each in order. Result of a sequence is the
last statement's value (relevant only for expressions that
produce a value; `pp-on` discards the result).

### 5.4 `$event`

Inside `pp-on`, `$event` resolves to the DOM event that fired.
Backed by a thread-local `CURRENT_EVENT: RefCell<Option<JsValue>>`
— pp-on sets it before evaluating the expr and clears it after.
Other contexts (`pp-text`, etc.) leave it `None`; reading
`$event` outside a `pp-on` fire returns `JsValue::UNDEFINED`.

### 5.5 Errors

Parse errors report via `console::error` with the same `Span` +
hint shape RFC-012 already uses. Evaluation errors (unknown
handler, path not found) silently no-op — same contract as a
misspelled handler name today.

## 6. Implementation

Three files:

### `crates/pocopine-core/src/expr.rs`

- **Tokens**: add `LParen`, `RParen`, `Comma`, `Eq` (single `=`;
  the existing `EqEq` parse already handles `==`).
- **Expr variants**: `Call(String, Vec<Spanned<Expr>>)`,
  `Assign(Vec<String>, Box<Spanned<Expr>>)`, `Seq(Box<Spanned<Expr>>,
  Box<Spanned<Expr>>)`.
- **parse_stmt_seq**: top-level for directive values. Parses
  statements separated by `;`. Left-associative folds into `Seq`.
- **parse_assign**: tries `path '='`; on match, build `Assign`;
  otherwise fall through to `parse_ternary` (existing).
- **parse_primary**: when it sees `ident` followed by `(`, parse
  a `Call`.
- **evaluate**: handle `Call`, `Assign`, `Seq` in addition to the
  existing variants.

### `crates/pocopine-core/src/magics.rs`

- Thread-local `CURRENT_EVENT: RefCell<Option<JsValue>>`.
- `resolve` adds `"$event"` → clone the thread-local value.
- `with_current_event(ev, f)` helper — sets, runs `f`,
  restores. Mirrors `with_current_el`.

### `crates/pocopine-core/src/directives/on.rs`

- At bind time: `expr::parse(&call.value)` → AST. On parse error,
  `console::error` + return (no listener installed).
- In the closure: call `magics::with_current_event(&ev, || {
      expr::evaluate(&ast, &proxy)
  })` under the existing `with_current_el` wrap.
- **Backward-compat hook**: if the parsed AST is a plain
  `Expr::Path([name])` (single identifier), wrap it as
  `Expr::Call(name, vec![Expr::Path("$event")])` at parse-post
  time so `@click="on_click"` continues to receive the event as
  its first arg.

## 7. Edge cases

- **Call result discarded.** `@click="compute()"` calls the
  handler, ignores the return value. Handler's `-> JsValue` isn't
  observable from the directive. Same today.
- **Assign path extends beyond single-segment.** `@click="user.name = 'x'"`
  writes into the user object on the scope. Works because
  `Reflect::set` on the intermediate object (read through the
  proxy) mutates it in place; the outer scope-level set-trap fires
  for the containing key on the NEXT read-trigger path. For v0,
  document that assigning into nested paths **does not**
  automatically trigger reactivity on the outer key — authors
  assigning deep state should either mutate through a handler or
  use a flat scope field. Follow-up RFC can deepen the write-path
  tracking.
- **Side-effects in arg evaluation order.** Left-to-right, same
  as JS. Authors writing `foo($event, self_mutator())` get
  predictable ordering.
- **Assignment vs `==`.** The lexer must prefer `==` over `=` —
  check `peek(1) == b'='` before emitting `Eq`. Tested.
- **`$event` inside nested call.** `@click="handle($event.target.value)"`
  — `$event.target.value` resolves through the path evaluator
  with `$event` as the root.

## 8. Example diffs (Pine demo)

Before:

```html
<pine-button id="pop-trig" @click="toggle_popover">…</pine-button>
```
```rust
pub fn toggle_popover(&mut self) {
    self.popover_open = !self.popover_open;
}
```

After:

```html
<pine-button id="pop-trig" @click="popover_open = !popover_open">…</pine-button>
```
(no Rust method needed)

Before (Tabs):

```html
<div role="tablist" @click="on_click">
  <button :data-value="tab.value">{tab.label}</button>
</div>
```
```rust
pub fn on_click(&mut self, ev: Event) {
    let t = ev.target()…closest("[data-value]")…;
    let Some(v) = t.get_attribute("data-value") else { return };
    self.select(v);
}
```

After:

```html
<template pp-for="tab in tabs">
  <button @click="select(tab.value)">{tab.label}</button>
</template>
```
(no delegation, no `data-value` smuggling)
