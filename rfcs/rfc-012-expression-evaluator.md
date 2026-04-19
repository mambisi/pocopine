# RFC 012 — Template expression evaluator

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — (extends [`path::resolve_truthy`](../crates/pocopine-core/src/path.rs)) |
| **Related** | [`rfc-004-pp-for.md`](./rfc-004-pp-for.md) §5.3 (pp-key paths) |

## 1. Summary

Replace the ad-hoc `resolve_truthy` (single leading `!` + dotted
path) with a proper mini-expression evaluator. Supports:

* dotted paths (already the existing grammar),
* literals — strings (`'foo'` / `"foo"`), numbers (`42`, `3.14`),
  booleans (`true` / `false`), null (`null`),
* unary `!`,
* comparisons: `==`, `!=`, `<`, `<=`, `>`, `>=`,
* logical `&&`, `||`,
* ternary `a ? b : c`,
* parentheses.

```html
<p pp-show="!loading && error"></p>
<template pp-if="count > 0 && !hidden">…</template>
<div pp-bind:class="open ? 'is-open' : 'is-closed'"></div>
<span pp-show="role == 'admin' || role == 'editor'">edit</span>
```

## 2. Motivation

Templates already lean on this — HN and several existing examples
had `pp-show="!loading && applied_query"` that silently never
evaluated correctly (the whole string was looked up as a single
path key). `resolve_truthy` patched the single-`!` case but every
other pattern (`count > 0`, ternary for class strings, equality
against a literal) still breaks.

shadcn-style components rely on exactly this kind of inline
boolean logic. Without it every template pays for a derived
field in Rust, and every Pine author hand-writes
`fn is_primary(&self) -> bool { self.variant == "primary" }`
for every visual state.

## 3. Non-goals

* **Arithmetic.** No `+`, `-`, `*`, `/`, `%`. Authors who need
  math derive it in Rust. Templates stay declarative.
* **Method calls.** `foo.bar()` is out. Scope access is a property
  lookup, period.
* **Assignment.** `a = b` doesn't exist; reactivity flows via
  dispatch / handler.
* **Bitwise, shifts, regex.**
* **String concatenation via `+`.** Use `cx!` / derived fields.
* **Function calls** (beyond the existing `$dispatch(...)` magic,
  which remains handled at the directive layer).
* **Optional chaining (`a?.b`), nullish (`a ?? b`).** Undefined
  paths already return `undefined`; `a || b` covers 95% of the
  use case for defaults.

## 4. Surface — grammar

```
expr       := ternary
ternary    := logic_or ( '?' expr ':' expr )?
logic_or   := logic_and ( '||' logic_and )*
logic_and  := equality  ( '&&' equality  )*
equality   := relation  ( ( '==' | '!=' ) relation )*
relation   := unary     ( ( '<=' | '<' | '>=' | '>' ) unary )*
unary      := '!' unary | primary
primary    := literal | path | '(' expr ')'
literal    := string | number | 'true' | 'false' | 'null'
string     := '"' ... '"' | "'" ... "'"
number     := /-?\d+(\.\d+)?/
path       := ident ( '.' ident )*
ident      := /[A-Za-z_$][A-Za-z0-9_$]*/
```

Whitespace is ignored between tokens. Identifiers starting with
`$` preserve their magic meaning (`$index`, `$store.*`, `$route.*`).

## 5. Semantics

### 5.1 Values

All intermediate values are `JsValue`. Comparisons and logic use
the JS coercion shape that matches what templates feel natural
for:

* `==` / `!=`: strict value compare. `"1" == 1` is `false`.
  Kept strict to avoid the classic JavaScript footguns.
* `<`, `<=`, `>`, `>=`: numeric compare via `as_f64`. Returns
  `false` when either side isn't numeric.
* `&&` / `||`: short-circuiting with JS truthiness (non-empty
  string, non-zero number, `true`, non-null/undefined/NaN).
* `!`: logical not, returning bool.
* `?:`: evaluates left → right, only the taken branch is tracked
  as a dependency (so an un-reachable branch doesn't over-subscribe).

### 5.2 Dependency tracking

Every path read goes through the existing proxy `get` trap, which
calls `track(scope_id, key)`. Literals and operators don't track.
Short-circuited branches in `&&` / `||` / `?:` don't evaluate,
so they don't track — effects re-subscribe to their actual deps
each run. Same "re-run clears prior deps" semantics we already
have in `reactive::run_effect`.

### 5.3 Error handling

* Invalid syntax: `console::error` once at parse time (directive
  setup), return a constant-false expression. The rest of the
  template keeps working.
* Non-numeric compared with `<`: `false`.
* Unknown path: `undefined` → falsy everywhere that expects a bool.
* Divide-by-zero / stack overflow are impossible because
  arithmetic / recursion aren't in the grammar.

### 5.4 Where it applies

Replaces the path-only RHS of every existing directive:

| Directive | Value expression |
|---|---|
| `pp-show` | truthy eval |
| `pp-if` | truthy eval |
| `pp-text` | stringified JsValue (no change to the value, just how it's computed) |
| `pp-html` | same |
| `pp-bind:<attr>` | evaluated, then passed to the existing attribute / prop plumbing |
| `pp-for` RHS path | **stays path-only** — keyed diffing on arbitrary expressions is out; use a computed field if you need a derived array |
| `pp-key` | evaluated (extends today's item-relative path resolution) |
| `pp-on:... ="handler"` | **stays** — the handler dispatch surface is a literal method name, not an expression |

### 5.5 Caching

Each directive setup parses its expression once and caches the
AST behind its effect closure. Subsequent evaluations walk the
AST, not a fresh parse.

## 6. Examples

### 6.1 Boolean combinators

```html
<button pp-show="open && !loading"   pp-on:click="submit">Save</button>
<p      pp-show="error || warning"   pp-text="error || warning"></p>
<div    pp-if="count >= 5"           class="pagination">…</div>
```

### 6.2 Ternary class + attribute

```html
<button
  pp-bind:class="active ? 'is-active' : 'is-idle'"
  pp-bind:aria-pressed="active"
></button>
```

### 6.3 Literal comparison

```html
<span pp-show="role == 'admin'">admin tools</span>
<span pp-show="status != 'done'">still working</span>
```

### 6.4 Working with store + route

```html
<template pp-if="$route.path == '/' || $store.user.authenticated">
  …
</template>
```

## 7. Implementation

New module `crates/pocopine-core/src/expr.rs`:

```rust
pub enum Expr {
    Literal(JsValue),
    Path(Vec<String>),
    Not(Box<Expr>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
}
pub enum BinOp { And, Or, Eq, Ne, Lt, Le, Gt, Ge }

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, String>;
    pub fn evaluate(&self, scope: &JsValue) -> JsValue;
}
```

Recursive-descent parser — a single `Parser { tokens: Vec<Tok>, pos: usize }`
struct, one method per grammar production. ~300 lines.

`evaluate` walks the AST, calls `path::resolve_path` for `Path`,
short-circuits in `And`/`Or`/`Ternary`, defers string equality to
`JsValue::as_string`, numeric compare to `JsValue::as_f64`.

### 7.1 Directive integration

Helpers:

```rust
pub fn resolve(scope: &JsValue, src: &str) -> JsValue {
    cached_expr(src).evaluate(scope)
}

pub fn resolve_truthy(scope: &JsValue, src: &str) -> bool {
    !resolve(scope, src).is_falsy()
}
```

Cached per call site inside the directive's effect closure:

```rust
let expr = Expr::parse(&call.value).unwrap_or_else(|e| {
    console::error_1(&format!("pp-show: {e}").into());
    Expr::Literal(JsValue::FALSE)
});
let id = effect(move || {
    let truthy = !expr.evaluate(&proxy).is_falsy();
    // … apply to element …
});
```

`resolve_truthy` (top-level utility used today by `pp-show` /
`pp-if`) stays but now delegates to `Expr`. Existing callers
(the ones not using `Expr` directly) still work — the surface
becomes a strict superset of today's behaviour.

### 7.2 Path compatibility

The grammar's `path` production matches today's dotted-path shape
exactly. Every directive whose value is currently a plain path
(`pp-text="foo.bar"`) keeps working as-is — the parser just sees
it as a one-node `Expr::Path` and `evaluate` calls `resolve_path`
the same way.

## 8. Edge cases

* **Empty expression** (`pp-show=""`). Parser error → constant
  `false`.
* **Path that starts with `$`** — `$index`, `$store`, `$route`.
  Identifier grammar already includes `$`; proxy get handles the
  lookup.
* **String containing `"` or `'`.** Support both delimiters so
  either can be escaped-free in HTML (`pp-show="role == 'admin'"`).
  No backslash escapes in v0; if you need a quote inside a string
  literal, use the other delimiter.
* **Very long expressions.** No length limit imposed; parser is
  O(n) in source length.
* **Right-associative ternary** — `a ? b : c ? d : e` parses as
  `a ? b : (c ? d : e)`. Matches JS/Vue/React.
* **Operator precedence collisions** — standard C-family order:
  `!` > `<` / `>` / `<=` / `>=` > `==` / `!=` > `&&` > `||` > `?:`.

## 9. Alternatives considered

* **Embed a real JS evaluator** (something like `boa` or a tiny
  JS parser). Too big, too many corner cases, and Pocopine
  templates don't want JS semantics — they want a tiny
  predictable subset.
* **Compile expressions at build time via proc-macro.** Moves the
  parser cost to compile, at the price of needing expressions as
  string literals in macros, which breaks the "templates are
  HTML files" separation.
* **Pre-declared expression fields on the component**
  (`#[derive(Expressions)]` macro). Verbose; defeats the point of
  inline conditionals.
* **Leave it at `resolve_truthy` and have authors derive fields
  in Rust.** Viable today; ugly for `pp-bind:class` ternaries and
  every Pine `variant == "x"` check. Papercuts add up.

## 10. Out of scope (future)

* Arithmetic (`+`, `-`, `*`, `/`, `%`).
* String concatenation (`+` on strings).
* Array / object literals (`[1, 2, 3]`, `{ a: 1 }`).
* Nullish coalescing (`??`), optional chaining (`?.`).
* Calling methods on scope values (`items.length` already
  resolves via property access — that's fine; but
  `items.filter(...)` is not).
* Expression in `pp-for` RHS, `pp-on:*` handler name. Both stay
  path-grammar.
