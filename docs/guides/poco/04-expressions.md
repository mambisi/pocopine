---
title: ".poco expressions"
description: "Every pp-*='...' attribute value is a pine-expr expression. The surface is deliberately small — if you need more, compute it Rust-side and bind by name."
---

# `.poco` expressions — surface and convention

Every `pp-*="..."` attribute value is a **pine-expr** expression. The
surface is deliberately small. If you need more than this page lists,
the answer is to compute Rust-side and bind to a field by name —
not to write a bigger expression.

## What pine-expr supports

**Reads**

* Identifiers and dotted paths: `count`, `user.name`, `file.status`
* Literals: `true`, `false`, `null`, numbers, single- or double-quoted strings

**Operators**

* Comparison: `==`, `!=`, `<`, `<=`, `>`, `>=`
* Logical: `&&`, `||`, `!`
* Ternary: `cond ? a : b`
* `+` — string concatenation when either operand is a string;
  numeric add when both coerce to `f64` (this is the *only*
  arithmetic operator)

**Calls**

* Plain identifier calls only: `upper(name)`, `format_id(id)`
* The callee must be a plain identifier naming a handler method on the
  component (`#[handlers] impl …`); dotted method calls like
  `obj.method()` are rejected

**Assignment and sequences** (handler context only)

* `count = count + 1`
* `a; b; c`

That's the whole surface. There's a complete grammar reference in
`crates/pocopine-expr/src/lib.rs` — but the rule of thumb is: if it
looks like it needs more than reading or comparing, it doesn't belong
in the template.

## What pine-expr does NOT support

* Arithmetic beyond `+`: no `-`, `*`, `/`, `%`, `**`
* JS-style strict equality: no `===`/`!==` — use `==`/`!=`
* Method calls on objects: no `obj.method()`, no `.length`, no
  `.toFixed()`, no `.slice()`, no `.map()` / `.filter()`
* Globals: no `Math.*`, no `Date.*`, no `console.*`, no `JSON.*`
* Arrow functions / lambdas: no `x => x + 1`
* Spread, optional chaining, nullish coalescing: no `...`, `?.`, `??`
* Regular expressions

These aren't oversights. They are not coming. Read the next section.

## Where computation lives

The opinionated answer: **derived display state is a Rust field**,
named for what it represents, kept up to date by the component.

There are three canonical shapes for keeping a derived field up to
date. Pick the one that matches the dependency.

### 1. `#[computed]` — depends only on other fields

```rust
#[handlers]
impl PineUploadItem {
    #[computed]
    pub fn progress_label(progress: f64) -> String {
        format!("{}%", (progress * 100.0).round() as i32)
    }
}
```

```html
<span pp-text="progress_label"></span>
```

`#[computed]` methods are **static** (no `self`), take the fields they
depend on as parameters, and are exposed to templates as read-only
synthetic fields. The framework recomputes them when their inputs
change. Use this for any pure derivation: labels, formatted numbers,
truncated strings, percent strings, derived class names.

### 2. `#[watch(field)]` — needs `self`, recomputes on prop change

```rust
#[handlers]
impl PineUploadItem {
    #[watch(extension)]
    fn on_extension_change(&mut self, new: String, _prev: Option<String>) {
        self.thumb_label = new
            .chars()
            .take(3)
            .collect::<String>()
            .to_uppercase();
    }
}
```

`#[watch(field)]` methods receive the new value and the previous value
as typed arguments (`new: V, prev: Option<V>`). The first call after
mount passes `None` for `prev`.

Reach for `#[watch]` when the derivation isn't a pure function of a
single value — for example, when it touches other fields on `self`,
calls out to a store, or has side effects (logging, telemetry).

### 3. Plain handler — derives on user action

```rust
#[handlers]
impl Counter {
    pub fn increment(&mut self) {
        self.count = self.count + 1;
        self.count_label = self.count.to_string();
    }
}
```

If the only way a field changes is through user input, just write the
derived state into the same handler that changes the input. No
attribute needed.

## Why pine-expr stays small

The smaller pine-expr is, the more your logic lives in Rust where
**rust-analyzer, clippy, tests, fmt, and rename** can all reach it.
Every operator added to pine-expr is logic the toolchain can't see.

Concretely:

* `Math.round(progress * 100)` in a template is a string. A typo
  silently renders nothing. The same expression Rust-side is a
  compile error and a clippy hint.
* `files.filter(f => f.status === 'done').length` in a template
  would need an interpreter for arrow functions, method dispatch,
  and JS equality semantics. Rust-side it is a one-line `iter()`
  chain with full type inference.
* A `#[computed] done_count` field is named for what it represents.
  The template binds `pp-text="done_count"`. A reader who has never
  seen the file knows what it does.

Pocopine is opinionated. The convention is: **templates declare,
Rust computes.** When a JS muscle-memory expression doesn't parse,
the compiler will tell you, and the fix is to give the derivation
a name.

## Compile-time errors you may see

The pine-expr parser rejects common JS patterns at compile time with
a directive message. Each error names the unsupported construct and
points at the offending span in the `.poco` file:

| Pattern | Error message |
|---|---|
| `progress * 100`, `a / b`, `i % 2` | `` arithmetic operator `*` is not supported in pine-expr `` |
| `count - 1` | `arithmetic subtraction is not supported in pine-expr` |
| `status === 'done'` | `` `===` is not supported in pine-expr `` |
| `x !== 'y'` | `` `!==` is not supported in pine-expr `` |
| `files.filter(f)` | `method calls on objects are not supported in pine-expr` |
| `x => x + 1` | `arrow functions are not supported in pine-expr` |
| `a ?? b` | `` nullish coalescing `??` is not supported in pine-expr `` |
| `user?.name` | `` optional chaining `?.` is not supported in pine-expr `` |
| `...rest` | `` spread `...` is not supported in pine-expr `` |

The fix in every case is the same: compute the value as a
`#[computed]` field (or `#[watch]`, or in a handler) and bind to it
by name.

## Related

* `docs/guides/components/02-state.md` — state management conventions; the
  anti-pattern on manually-mirrored derived fields ties directly to
  this page.
* `docs/guides/components/03-composition.md` — **typed slot props** (RFC
  084) apply the same Rust-side-computation pattern to the
  parent↔child slot edge: declare the slot's exposed shape as a
  `Props` struct so the same compile-time validation applies to
  slot publications.
* `docs/guides/poco/01-format.md` — the broader `.poco` format.
* `crates/pocopine-expr/src/lib.rs` — grammar reference.
