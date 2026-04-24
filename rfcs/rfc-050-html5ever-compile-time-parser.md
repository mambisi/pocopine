# RFC 050 — Real HTML parser at compile time: `html5ever` in `pocopine-macros`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 045](./rfc-045-single-root-templates.md), [RFC 049](./rfc-049-typed-slot-contracts.md) |

## 1. Summary

Introduce [`html5ever`](https://crates.io/crates/html5ever) as
a **host-only compile-time dependency** of `pocopine-macros`.
The parser runs during `#[component]` expansion to produce a
real element AST for every `.poco` file, replacing the
current ad-hoc byte-level scanners with one shared
abstraction. No runtime cost, no wasm size regression —
proc-macro crates and their deps never link into the
consumer's output.

```toml
# crates/pocopine-macros/Cargo.toml
[dependencies]
# Bundles html5ever + ego-tree + selectors. ego-tree is the
# arena-based tree representation (NodeId indices, Vec-backed
# storage) — cleaner traversal than markup5ever_rcdom's
# Rc<RefCell<Node>>, and CSS-selector querying comes free for
# future structural checks.
scraper              = "0.21"
```

### Principles this RFC commits to

Four rules define the parser's role and govern every
future RFC that builds on it:

1. **Compile-time is authoritative.** All `.poco`
   *validation* and *static analysis* happens through the
   compile-time parser. The runtime byte walkers in
   `pocopine-core` (`compile_template`, `inject_pp_data`,
   `rewrite_root_placeholder`, `root_placeholder_has_attr`)
   are **legacy transformation machinery** — not a parallel
   source of truth. No new correctness-sensitive behaviour
   may be added to them.
2. **Framework-owned errors are fatal.** pocopine pre-parse
   and post-parse rules with anchored byte ranges (the
   self-close rule, future duplicate-attr rule, pp-ref
   uniqueness, etc.) fail `#[component]` compilation.
   html5ever's own spec-recovery notices are surfaced but
   not fatal — their wording isn't a stable contract we
   can match against. See §4.8 for the full split.
3. **Fail closed, not silent.** If a compile-time check
   can't confidently interpret a template construct, it
   errors — it does not skip the check and let potentially-
   wrong markup through.
4. **Byte-range fidelity is a hard requirement.** Opening-
   tag byte ranges must be exact for every authored tag.
   If `html5ever` can't deliver this robustly through a
   custom `TreeSink`, we swap to `swc_html_parser` —
   incorrect spans are unacceptable for RFC 049-class
   diagnostics.

These principles scope every subsequent section.

## 2. Motivation

### 2.1 Today's compile-time scanners

`pocopine-core/src/templates.rs` already holds four separate
byte-level scanners called at macro expansion:

| Scanner | Purpose | RFC |
|---|---|---|
| `check_single_root` | Exactly-one-root check | 045 |
| `inject_pp_data` | Splice `pp-data` into first tag | — |
| `rewrite_root_placeholder` | `<root>` → `<tag>` | 033 |
| `root_placeholder_has_attr` | Check a specific attr on `<root>` | 033 |

Each walks bytes independently, each duplicates quote
handling, each re-derives comment / doctype / PI skipping.
They share helpers (`find_byte`, `find_seq`, `find_tag_end`),
but each pass is still a hand-written state machine.

The pattern holds up for single-purpose one-off scans, but
it's not where we're going. RFC 049 needs tag enumeration
with positions. Future structural-validation RFCs — `pp-ref`
uniqueness, directive-name validation, `pp-slot` mapping to
the parent's slot table, possibly compile-time-evaluated
`pp-if` dead-code elimination — want the same thing: **a
real element tree with positions**. Trivial checks that
don't need structure may still live as lightweight byte
peeks; the claim is not "every static check needs a parser,"
it's that **every future structural template check we care
about does**.

### 2.2 Pay the migration cost now

Writing one more byte-scanner per RFC is tempting — it's
faster in the short run. But three more RFCs in, we have
six scanners re-deriving the same state machine, and the
cost of migrating to a real parser grows with each one.

The honest framing:

- **Now**: migrate two scanners (`check_single_root`,
  `rewrite_root_placeholder`), design the parser wrapper
  once, ship `html5ever` as a macro dep.
- **In 3 months**: migrate five scanners, have mutually
  inconsistent error paths, argue about which one has the
  "right" behaviour on malformed input.

Better to take the compile-time dependency now, establish
one canonical parse path, and let every future static check
build on it.

### 2.3 Why `html5ever` specifically

- **Correct by construction.** Servo's reference HTML5
  parser; handles every browser-compat quirk (void elements,
  attribute quoting variants, entity references, comment
  conditions) exactly the way `el.set_inner_html()` does at
  runtime. When the walker sees the DOM and the macro sees
  the AST, they agree.
- **Mature.** Predates most Rust HTML tooling; part of the
  rustc CI matrix via Servo.
- **Host-only.** Lives in `pocopine-macros` (a proc-macro
  crate). Compiles for the host, runs in rustc, never
  linked into the consumer's `.wasm`.
- **One parser, many consumers.** Every compile-time check
  we add from now on reads the same AST. No re-parsing per
  RFC.

## 3. Non-goals

- **No runtime parser.** `compile_template`,
  `inject_pp_data`, `rewrite_root_placeholder`, and
  `root_placeholder_has_attr` remain byte walkers in
  `pocopine-core` for now. They run inside the wasm at app
  boot; pulling html5ever into that path adds ~200KB to
  every bundle. These walkers are frozen — **legacy
  transformation-only**, per §1 principle 1 — and a future
  RFC folds them into the macro phase.
- **No DOM emulation.** We don't replicate what the browser
  does at runtime; we use html5ever for *inspection* only.
  Pocopine's runtime is still the browser's parser +
  `set_inner_html`.
- **No permissive recovery in validation.** html5ever is
  permissive by design — we wrap it in strict mode (§4.8).
  Any parser error fails compilation.
- **No new author-facing surface.** `.poco` files don't
  change. `#[component]` doesn't change. The parser is an
  implementation detail of the macro.
- **No replacement of `annotate-snippets`.** RFC 049 owns
  error rendering; this RFC owns parsing. The parser
  produces byte ranges; the renderer consumes them.

## 4. Design

### 4.1 Crate placement

| Crate | Role | New deps |
|---|---|---|
| `pocopine-macros` | proc-macro; host-only; runs inside rustc | `html5ever`, `markup5ever_rcdom` |
| `pocopine-core` | runtime library; compiles to wasm | **none added** |

The parser wrapper is a new module inside
`pocopine-macros` — `src/template_parser.rs` — and its
public API is used only by sibling modules of `#[component]`.
Nothing from `pocopine-core` imports it; nothing from
`pocopine-core` gains a new dependency.

### 4.2 What we use from `scraper`

[`scraper`](https://crates.io/crates/scraper) bundles the
three things we need:

- **`html5ever`** — the parser (Servo's reference impl).
- **`ego-tree`** — arena-based tree with `NodeId` indices.
  Simpler traversal than `markup5ever_rcdom`'s
  `Rc<RefCell<Node>>` and better cache behaviour for the
  read-heavy workloads every compile-time check has.
- **`selectors`** — CSS-selector matching. Lets structural
  checks use `Selector::parse("pine-context-menu-content >
  pine-random-thing")` instead of hand-written tree walks.

The minimal surface:

```rust
use scraper::{Html, Node, Selector};
use scraper::node::Element;

let html = Html::parse_fragment(raw);
for root in html.tree.root().children() {
    if let Node::Element(el) = root.value() {
        // el.name() → local name, el.attrs() → iterator of (name, value)
    }
}
```

`Html::parse_fragment` is the right entry — `.poco` files
are fragments, not full documents. Scraper configures
html5ever with the fragment context under the hood.

For position tracking (§4.3), scraper's own `Html` doesn't
track spans — same situation as raw html5ever — so we still
implement a custom `TreeSink` that wraps an `ego-tree::Tree`
and a `HashMap<NodeId, Range<usize>>`. Having the `ego-tree`
primitive available makes that wrapper shorter than it would
be against `RcDom`.

### 4.3 Position tracking — the byte-range contract

Per §1 principle 4, byte-range fidelity is a **hard
requirement**, not a best-effort target. The concrete
guarantee:

> **Opening-tag byte range** — for every element the author
> literally wrote in the `.poco` source, `element.opening_tag_range`
> spans from the `<` of the opening tag through the matching
> `>` (or `/>`). This is exact. It is not approximated from
> line/column. It is not omitted in any authored case.

Implementation constraints this drives:

- Tags synthesised by html5ever's tree construction (e.g.
  implicit `<html>`, `<head>`, `<body>` insertion on
  fragment contexts) are **not** authored tags; they carry
  no opening-tag range. Our AST walker skips them.
- Tags produced by the parser's foster-parenting /
  error-recovery heuristics — which would only exist on
  malformed input — never reach validation, because §4.8
  treats parse errors as fatal and bails before the walker
  ever runs.
- Multi-byte UTF-8 is handled by storing column offsets in
  bytes; `line_starts: Vec<usize>` is computed by scanning
  for raw `\n` bytes. `\r\n` line endings are normalised
  during line-start construction (the `\r` is treated as
  part of the preceding line).

`html5ever`'s tokenizer exposes `(Token, line_number)` in
its default configuration, but the tree-builder `RcDom`
doesn't preserve positions on nodes. We wrap `parse_fragment`
with a custom `TreeSink` that records positions at element-
creation time:

```rust
struct LocatingSink {
    dom: RcDom,                              // delegate
    positions: HashMap<NodeHandle, Range<usize>>,
    line_starts: Vec<usize>,                 // precomputed
    source: String,
}

impl TreeSink for LocatingSink {
    // Delegate most methods to `dom`. Override element-
    // creation and closing-tag hooks to record byte ranges.
    fn create_element(
        &mut self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> Self::Handle {
        let handle = self.dom.create_element(name, attrs, flags);
        let start = self.current_byte_position();
        self.positions.insert(handle.clone().into(), start..start);
        handle
    }
    // ... delegate everything else
}
```

The end offset for each element is filled in when the
matching close tag (or self-close) fires.

**Release-blocking test coverage.** Byte-range fidelity is
gated by a golden-file test matrix that must pass before
every release:

- single root, nested roots, self-closing tags, void
  elements, attribute quoting variants (single / double /
  unquoted), `>` inside quoted attribute values, comments,
  doctype, PI, leading and trailing whitespace, CRLF line
  endings, multi-byte UTF-8 in attribute values, multi-byte
  UTF-8 in text content, tags that span line boundaries.

Each case asserts the exact `Range<usize>` for every
authored element. A mismatch is a release blocker, not a
nice-to-fix.

**If html5ever can't deliver this.** The RFC's fallback is
explicit: **switch to `swc_html_parser`**, which tracks
byte-accurate spans natively. See §6.1 — this is a fallback
plan, not a "maybe." Incorrect spans break RFC 049's
diagnostics; we'd rather take the heavier dep tree than
ship misleading error arrows.

### 4.4 The parser wrapper — AST contract

Public API of `pocopine-macros::template_parser`:

```rust
pub struct TemplateAst {
    pub source: String,          // the raw .poco bytes, verbatim
    pub file_path: String,       // for annotate-snippets
    pub roots: Vec<Node>,        // all top-level nodes, in order
}

pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub byte_range: Range<usize>,
    pub opening_tag_range: Range<usize>,
}

pub enum Node {
    Element(Element),
    Text(String, Range<usize>),
    Comment(String, Range<usize>),
}

pub struct ParseError {
    pub message: String,
    pub byte_range: Range<usize>,
}

pub fn parse(source: &str, file_path: &str)
    -> (TemplateAst, Vec<ParseError>);
```

**Invariants the parser guarantees** (release-blocking):

- `Element.tag` is always the **lowercase HTML local
  name**. `<DIV>` and `<div>` both produce `"div"`.
  Downstream checks may compare with `==`; no casing
  concerns.
- `Element.attrs` preserves **source order**. When an RFC
  needs deterministic attribute walking (e.g. picking up
  the first `pp-if` / `pp-for` on a tag), it gets it.
- **Duplicate attributes** on the same element are
  reported as a parse error (§4.8) and fail compilation —
  not silently normalised, not last-write-wins. If a
  future RFC has a legitimate reason to accept duplicates,
  it can carve out an exception; v1 rejects them.
- `Element.byte_range` spans `<opening` through `</closing>`
  — inclusive of the whole element including children.
  `Element.opening_tag_range` spans `<` through `>` of
  just the opening tag.
- **Top-level nodes** means *all* top-level nodes —
  elements, text, comments, whatever the author wrote at
  the template root. `roots: Vec<Node>`, not
  `Vec<Element>`. "Top-level element roots" is a separate
  question answered by filtering (`ast.roots.iter()
  .filter_map(Node::as_element).count()`).
- **Comment and text nodes at any level** are preserved
  in the tree. Checks that want to ignore them filter;
  checks that want to reason about them have access.
- **Void elements** (`<br>`, `<img>`, etc.) appear as
  `Element { children: vec![], ... }` — no implicit empty
  close-tag in their `byte_range`. `opening_tag_range`
  covers just the `<tag ... >` / `<tag ... />`.
- **Implicit tags** produced by html5ever's fragment-
  parsing (synthetic `<html>`, `<head>`, `<body>`) are
  **not** surfaced in `roots`. The parser filters them; an
  author-written `<html>` is rejected earlier as malformed
  (§4.8) since a `.poco` is a fragment, not a document.

Every pocopine compile-time check consumes `TemplateAst`
and operates on the tree. No more hand-walked bytes per
RFC.

### 4.5 Migrating RFC 045 — macro-time diagnostics replace const-eval

This is more than a code move. It's a **policy shift** in
how pocopine reports template mistakes.

| | Before | After |
|---|---|---|
| Where the check runs | Const-eval inside downstream generated Rust | Inside the `#[component]` proc-macro |
| Error surface | `E0080 evaluation panicked` | `syn::Error` with pre-rendered `annotate-snippets` block |
| Error span | `#[component]` attribute | The exact offending `.poco` line + caret |
| Authored by | `const _: () = match check_single_root(include_str!(...)) { ... }` | `syn::Error::new(span, rendered_snippet).to_compile_error()` |

The policy: **all template-structure validation happens
in the proc-macro.** `const _: () = ...` is no longer the
preferred vehicle for template diagnostics once the parser
lands. It stays in pocopine's toolkit for legitimate
const-eval checks (numeric constraints, type-level
assertions) but not for parsing-driven errors.

The migration itself is small:

```rust
pub fn check_single_root(ast: &TemplateAst) -> RootCheck {
    match ast.roots.iter().filter(|n| matches!(n, Node::Element(_))).count() {
        0 => RootCheck::Missing,
        1 => RootCheck::Ok,
        _ => RootCheck::Multiple,
    }
}
```

(Filtering `Element` out of the top-level `Node` list per
the §4.4 contract.) The const-fn wrapper we ship today
(`crates/pocopine-core/src/templates.rs:check_single_root`)
is removed from core and re-implemented here. The macro
calls `parse` directly, matches on `RootCheck`, and on
failure emits a `syn::Error` with an `annotate-snippets`-
rendered snippet pointing at the second root's byte range.
The error arrives at the author exactly like RFC 049's —
same rendering path, same IDE-linkable format.

### 4.6 Migrating RFC 033 role-tag rewrite

`rewrite_root_placeholder` today does string replacement of
`<root>`. With an AST, the rewrite is: find the single root
element, rename it, splice attributes, re-serialise the
children back into a template string. html5ever provides
`serialize` for this; cost is a round-trip.

Decision point: **keep the byte-level rewrite in
`pocopine-core` at runtime** (what we do today), because
the rewrite output is the string we hand to
`register_template`, and that string is consumed at app
boot. Adding html5ever *serialisation* to the wasm path
would re-introduce the dependency we're trying to avoid.

Cleaner long-term path: do the rewrite at compile time in
the macro (using html5ever both to parse and serialise),
store the fully-rewritten template string as a macro-emitted
`&'static str`, and delete the runtime rewrite entirely.
That's a separate RFC — when we pull the trigger on that,
the runtime `templates.rs` shrinks a lot. **Out of scope
for RFC 050.**

### 4.7 Enabling RFC 049

RFC 049's consumer-side template scan (§5.2 there) needs to
walk the template tree looking for parent-tags in the
`uses` list and collect their direct children with byte
ranges. With `TemplateAst` that's a 20-line tree walk:

```rust
for root in &ast.roots {
    walk(root, &uses_table, &mut assertions);
}

fn walk(el: &Element, uses: &UsesTable, out: &mut Vec<Assertion>) {
    if let Some(parent_ty) = uses.lookup(&el.tag) {
        for child in el.element_children() {
            if let Some(child_ty) = uses.lookup(&child.tag) {
                out.push(Assertion {
                    parent_ty, child_ty,
                    byte_range: child.opening_tag_range.clone(),
                });
            }
        }
    }
    for child in el.element_children() {
        walk(child, uses, out);
    }
}
```

No byte walker, no edge cases around `pp-if` /
`<template>` wrappers (the AST represents them
uniformly), no state machine.

### 4.8 Parse-error policy — framework-owned errors are fatal

`html5ever` is permissive by design: it recovers from
malformed input the way a browser would (foster-parenting
stray content, implicit `</tag>` insertion, attribute
deduping, tag-name normalisation on bad characters). Some of
its recovery is visible via its parser-error stream; some
isn't.

The policy splits by **who owns the error**:

1. **Framework-owned errors** — produced by pocopine's own
   pre-parse / post-parse rules, always carry a non-zero
   `byte_range` anchored at the offending source position.
   Currently covers the `<tagname/>` self-close rule
   (§4.x). Future checks will add the framework-owned
   duplicate-attr diagnostic, pp-ref uniqueness, etc. These
   are **fatal** — `parse_strict` rejects, `#[component]`
   emits `syn::Error` with an `annotate-snippets`-rendered
   block, build fails.
2. **html5ever spec-recovery notices** — produced by
   html5ever's tokenizer / tree-builder. `byte_range ==
   0..0` (html5ever doesn't expose source positions for its
   errors on stable). Surface through `parse()`'s full
   error list for diagnostic rendering, but **do not** gate
   compilation in `parse_strict`.

The rule: **pocopine-macros does not turn html5ever into a
general HTML validity gate.** Matching on an html5ever
message-string to decide fatality is not a stable contract;
their wording changes across versions. Instead, any pocopine
correctness invariant we want to enforce strictly gets its
own framework-owned rule with an anchored byte range, which
automatically flows through the fatal path.

**Compile-time enforced today** (framework-owned rules):

- Forbidden `<tag/>` self-close on non-void / non-foreign
  elements (RFC 050 §4.x). Points at the offending tag.
- RFC 045 single-root rule (via `element_roots().count()` —
  the root count itself is framework logic, not an html5ever
  error).

**Surfaced but not fatal** (html5ever-sourced recovery):

- Unclosed tags (html5ever auto-closes at end-of-input).
- Duplicate attributes (html5ever keeps the first).
- Mis-nested close tags.
- `<html>` / `<head>` / `<body>` inside a fragment.
- "Non-space table text" foster-parenting.
- Other html5ever tokenizer/tree-builder recovery messages.

Each of these is *observable* through `parse()`'s
`Vec<ParseError>` — diagnostic renderers can show them to
authors as informational warnings in the future. They just
don't stop the build today. A follow-up RFC can promote any
specific case to framework-owned fatal status by writing a
pocopine pre-parse scanner for it (following the same shape
as the self-close rule).

**No silent recovery for framework-owned checks.** Any
future pocopine rule that can't confidently interpret a
template construct **must fail**, not skip. Best-effort
behaviour quietly lets bad templates through; that's the
failure mode RFC 050 exists to eliminate. The boundary is
about *what pocopine chooses to enforce*, not about what
html5ever can recover from.

## 5. Implementation

### 5.1 Phase 1 — introduce the parser

1. Add `html5ever` + `markup5ever_rcdom` to
   `crates/pocopine-macros/Cargo.toml`.
2. Land `crates/pocopine-macros/src/template_parser.rs`
   with `parse()` + the custom `TreeSink` for position
   tracking.
3. Golden-file tests under `crates/pocopine-macros/tests/`
   over a handful of representative `.poco` files (single
   root, nested, comments, doctype, self-closing, void
   elements) asserting the resulting `TemplateAst` matches
   a snapshot.

### 5.2 Phase 2 — migrate RFC 045

1. Remove the const-fn `check_single_root` from
   `crates/pocopine-core/src/templates.rs` (and its unit
   tests).
2. Remove `RootCheck` from `pocopine-core` exports (it
   moves to `pocopine-macros` as an internal type; the
   macro no longer emits a `const _: () = match ...` —
   it emits a `syn::Error` directly).
3. Re-implement `check_single_root` over `TemplateAst` in
   `pocopine-macros`.
4. Update `#[component]` expansion to parse the `.poco`
   once, run the single-root check, and emit an
   `annotate-snippets`-rendered error on failure.
5. Update `trybuild` tests: the existing RFC 045 negative-
   path tests now assert the rendered snippet format
   instead of the const-eval `E0080` format.

### 5.3 Phase 3 — wire RFC 049 on top

Once `TemplateAst` ships, RFC 049's consumer-side scan
plugs in directly per its §5.2. No new parser work needed
there.

### 5.4 Runtime surface — legacy transformation machinery

`crates/pocopine-core/src/templates.rs` continues to ship
the runtime byte walkers (`compile_template`,
`inject_pp_data`, `rewrite_root_placeholder`,
`root_placeholder_has_attr`). Per §1 principle 1 they are
**transformation-only**, not a parallel validation path:

- **Allowed:** the rewrites they perform today (splicing
  `pp-data`, rewriting `<root>` → `<tag>`). Unchanged
  behaviour.
- **Not allowed:** new correctness-sensitive checks,
  per-RFC byte-level scans, or anything that could
  diverge from the compile-time parser's view of the
  template. Reviewers enforce this during code review;
  the commit message should cite the RFC 050 principle
  when modifying the file.

These walkers are the only thing keeping html5ever out of
wasm. A future "compile-time template pre-pass" RFC folds
them into `#[component]` and deletes the runtime path;
until then they're frozen.

## 6. Alternatives considered

### 6.1 `swc_html_parser` — the explicit fallback

SWC's HTML parser tracks byte-accurate `Span`s natively —
no custom `TreeSink` needed. Drawbacks:

- Heavier dep tree (roughly 2× the transitive crate count).
- Less stable API across minor versions (SWC iterates
  fast).
- Tied to SWC's type-system (`ast::Element` uses SWC's
  own types, which we'd wrap anyway).

Per §1 principle 4, this is a **committed fallback**, not
a speculation. If the golden-file byte-range test matrix
(§4.3) fails on html5ever's custom `TreeSink`, we migrate
to `swc_html_parser` before release. The public AST
contract (§4.4) is parser-agnostic — downstream RFCs
shouldn't notice the swap. Neither parser affects wasm
output size; both are macro-only.

### 6.2 Bare `html5ever` + `markup5ever_rcdom` (no `scraper`)

Skip the scraper wrapper and pull in html5ever + the
reference `RcDom` directly. Saves scraper's CSS-selector
overhead (and its `selectors` / `cssparser` transitive deps).
Rejected for v1 because:

- `RcDom`'s `Rc<RefCell<Node>>` traversal is clunky compared
  to `ego-tree`'s arena access.
- CSS-selector support is load-bearing for RFC 049-style
  checks (finding specific parent/child shapes) and we'd
  end up re-implementing it by hand.
- The extra dep footprint is macro-only; wasm output is
  unaffected.

If scraper's API turns out to be too opinionated or the
CSS-selector surface is never used, swapping to raw
html5ever + ego-tree is mechanical.

### 6.3 `tl`

Small, simple, ~30KB. No HTML5 spec compliance; permissive
parser that'll accept malformed inputs we want to reject.
Positions are available (`Node::raw()` gives byte ranges),
but the correctness story is significantly weaker than
html5ever. Saves build time, loses spec alignment with the
browser at runtime — which is the whole point.

### 6.4 Keep the byte walkers, extend per-RFC

Status quo. Cheap short-term, expensive long-term (see
§2.2). Every RFC that wants position-aware template
inspection re-derives the same state machine and makes its
own mistakes on edge cases. RFC 049 alone would justify
~150 lines of new byte walker; by RFC 052 we're well past
the break-even point.

### 6.5 Hand-roll a real parser

Between "byte walker" and "html5ever" there's "write our
own AST-producing parser." ~800 lines to match html5ever's
correctness on the subset we care about. Saves the
dependency but costs a maintenance surface we don't need
— we're not gaining anything browsers won't match anyway.
Explicitly rejected.

### 6.6 Move runtime walkers into the macro *now*

Parse + rewrite + serialise at compile time, store the
finished template string as a `&'static str`. Tempting —
eliminates `compile_template` / `inject_pp_data` from
runtime entirely, shrinks wasm. Rejected for v1 because
it conflates the parser-adoption decision with a
runtime-rewrite-folding decision that deserves its own
RFC and its own benchmarks. Do the smaller thing first.

## 7. Rollout

1. Add `html5ever` + `markup5ever_rcdom` to
   `pocopine-macros`; land `template_parser` module with
   golden-file tests (§5.1).
2. Migrate RFC 045's single-root check to
   `TemplateAst`; remove the const-fn + its const-eval
   error path; update trybuild snapshots (§5.2).
3. Ship RFC 049's consumer-side scan on top of the new
   parser (§5.3).
4. Leave the runtime rewriters in `pocopine-core` alone
   (§5.4). A future "compile-time template pre-pass" RFC
   can revisit them.

No migration required for existing consumer code — `.poco`
files don't change. The only user-visible change is the
error format for multi-root templates, which gets *better*
(now rendered via `annotate-snippets` pointing at the
`.poco` line) not worse.

## 8. Open questions

Note: parse-error policy (§4.8), byte-range fidelity
requirements (§4.3), AST normalisation invariants (§4.4),
and the fail-closed principle (§1) are **decided** in the
RFC body — not open. The questions below are
implementation-level.

* **8.1 Compile-time cost.** Adding html5ever adds ~2-3
  seconds to first compile of any project using pocopine
  (transitive dep compile). Amortised across the crate
  graph, not per-component. Acceptable for the feature
  surface; call out in release notes.
* **8.2 Version pinning.** html5ever is on 0.27.x as of
  this RFC. Pin a minor version in `Cargo.toml` and run
  the golden-file tests on upgrades (same story as
  `annotate-snippets` per RFC 049 §8.6).
* **8.3 Error output format change for RFC 045.** Today's
  multi-root error surfaces as `E0080 evaluation panicked`
  at the `#[component]` attribute. After migration it
  surfaces as a pre-rendered `annotate-snippets` block
  pointing at the second root's line in the `.poco` —
  shape matches RFC 049. Document in the RFC 045 status
  entry when updating it for the migration.
* **8.4 When to migrate the runtime rewriters.** Not now;
  the trigger is when we want to ship RFC 033 role-based
  default attribute injection as a compile-time step, or
  when wasm size pressure justifies the macro-time fold.
  Track as a "future RFC" bookmark.
* **8.5 Enforcing "no new runtime walkers" via tooling.**
  The §5.4 rule is reviewer-enforced in v1. If drift
  becomes a real problem, options include (a) a custom
  `deny-list` clippy lint on additions to `templates.rs`,
  (b) moving the file's `mod` declaration into a more
  restrictive visibility, or (c) splitting it into
  `templates_rewriters.rs` with a crate-internal comment
  block forbidding new functions. Evaluate after ~2
  quarters of the new regime.
