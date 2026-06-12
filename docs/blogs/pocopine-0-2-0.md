---
title: "Pocopine 0.2.0 — a signals-first core, enum matching, and a mutation channel"
description: "The reactive engine is now fine-grained signals under unchanged ergonomics, templates gained pp-else-if/pp-else and pp-match, structural templates became comment anchors, and keyed lists mount through one wasm→JS crossing. Here's how each piece works and what we measured."
date: 2026-06-11
---

# Pocopine 0.2.0 — a signals-first core

Pocopine 0.2.0 rewrites the reactive engine, adds the control-flow
directives templates have been missing, and lands the largest
performance work since the compiled row plans — while keeping the one
promise we treat as untouchable: **your code doesn't change.** A
handler is still `self.count += 1`. A template still writes
`open = !open`. No `Signal<T>` in your structs, no `set_x()` setters,
no `.value`.

This is the long version: how each piece works, why it's shaped the
way it is, and the numbers — including the experiments that failed
and were reverted.

## Where we started

Pocopine's reactive core descended from Alpine.js: every component
struct wrapped in a `js_sys::Proxy`, template reads tracked through
the proxy's get trap, writes through its set trap. It's a great
model — it's why `self.count += 1` works with no ceremony — but it
carried three structural costs:

1. **Serialization on read.** Field reads crossed wasm→JS and
   serialized via `serde_wasm_bindgen` on every cache miss.
2. **Coarse handler triggering.** A `&mut self` handler bypasses the
   proxy entirely — the runtime couldn't see *which* field changed,
   so after every handler it invalidated the whole field cache and
   re-ran every effect that had tracked *any* field in the scope.
   Profiling put this at 40–70% of state-sync time.
3. **Proxy-trap crossings.** Each `Reflect::get` in an expression is
   a wasm→JS→wasm closure round-trip that also defeats the JS
   engine's inline caches.

Alpine itself measures ~3.5× vanilla on the standard keyed-table
benchmark. Raw `wasm-bindgen` measures ~1.08×. The ceiling was
near-vanilla — if those three costs went away without touching the
authoring model.

## One signal graph

Every dependency edge in 0.2.0 — component field, store field,
standalone `signal()`, `#[computed]` result — lives in **one
id-keyed signal graph**. Component fields join it by *interning*:
the first `track(scope, "count")` allocates a numeric `SignalId` for
that `(scope, field)` pair; from then on, fields and signals are
indistinguishable to the dispatcher.

<video autoplay loop muted playsinline controls width="100%"><source src="/assets/blog/signal-graph.webm" type="video/webm"><source src="/assets/blog/signal-graph.mp4" type="video/mp4"></video>

```text
read  →  track(scope, key)            intern (scope,key) → SignalId
                                      SIGNAL_DEPS[sid] += current effect
write →  trigger(scope, key)          bump the field's version
                                      queue SIGNAL_DEPS[sid] subscribers
                                      → microtask flush → rerun effects
```

Subscriptions are rebuilt on every effect run (clear-before-run), so
conditional reads stay exact: an effect that read `detail` last run
and `summary` this run is subscribed to exactly `summary` now. The
ergonomics are Vue's `reactive()`; the engine underneath is
fine-grained signals, as in Solid or Leptos — there is no virtual
DOM and no render function anywhere in this story.

Reads and writes converge on two Rust-side mirror functions —
`read_field_tracked` and `write_field_tracked` — used identically by
compiled bindings, template assignment expressions, `pp-model`, and
(when one exists) the proxy's traps. One implementation, so the
proxy path and the proxy-free path cannot diverge.

## Fingerprinting: how `&mut self` stays ergonomic

The hard problem of this whole design: a handler mutates plain Rust
state, invisibly to any tracking. Most frameworks solve it by making
you route writes through setters or signal cells. We refused that
trade and instead **measure the state**.

### The bracket

`Scope::invoke` wraps every handler call (and `Handle::update`, and
store `update`):

<video autoplay loop muted playsinline controls width="100%"><source src="/assets/blog/dirty-sweep.webm" type="video/webm"><source src="/assets/blog/dirty-sweep.mp4" type="video/mp4"></video>

```text
DirtySweep::begin                     before[k] = fingerprint(field k)
  │                                   for every OBSERVED field
  ▼                                   (= every field anything has read)
state.invoke("increment")             ← your &mut self code, unobserved
  │
  ▼
DirtySweep::finish                    after[k] = fingerprint(field k)
                                      changed = { k : after[k] ≠ before[k] }
                                      bump versions + trigger ONLY those
```

A handler that touches one field out of twenty re-runs one field's
effects. "Observed" matters: a field nothing renders has no interned
signal, so it's never hashed at all.

### The hasher is a serde Serializer

`fingerprint(value)` is **not** "serialize to JSON, hash the
string" — that would allocate per field per handler call. It's a
custom serde `Serializer` whose output type is a hash state: serde
walks the value's own `Serialize` impl, and every `serialize_*`
method feeds bytes straight into a streaming **FNV-64** hasher. No
intermediate buffer, no allocation for common shapes, and — the
property the design exists for — **zero wasm↔JS crossings**. The
value never leaves Rust.

```text
Row { id: 57, label: "row 57" }

[STRUCT_START]
  [FIELD]"id"    [UINT][57 LE]
  [FIELD]"label" [STR][len=6 LE][b"row 57"]
[STRUCT_END]                              ──► one u64
```

Each piece of structure in that stream exists to kill a specific
collision class:

| defense | collision it prevents |
|---|---|
| type tags (one byte per value) | `1u64` vs `1i64` vs `"1"` vs `Some(1)` |
| length prefixes on strings/bytes | `("ab","c")` vs `("a","bc")` |
| start/end sentinels on compounds | `[[1],[2]]` vs `[[1,2]]` |
| struct **field names** hashed | `#[serde(skip_serializing_if)]` siblings emitting identical event streams |
| enum variant index hashed | `Status::Idle` vs `Status::Loading` |
| floats hashed by bit pattern | `0.0` vs `-0.0` (the sign is observable from JS) |

And one parity invariant: the hasher answers
`is_human_readable() = true`, matching `serde_wasm_bindgen` — so a
type like `chrono::DateTime` that serializes differently per mode
feeds the fingerprint the *same representation the template sees*.
A `#[serde(skip)]` field is invisible to both the UI and the hash;
the UI can never depend on data the hash misses.

### Every failure direction is "trigger more"

| case | behavior |
|---|---|
| `Serialize` impl errors | treated as **changed** — spurious re-run, never a missed one |
| `HashMap` rehash, same content | iteration order may differ → false "changed" — extra work, not wrongness |
| re-entrant invoke (state mid-borrow) | fall back to the old blanket invalidate + scope-wide trigger |
| true 64-bit collision | the only way to miss an update — ~2⁻⁶⁴ per write |

### The O(1) length probe

Hashing is linear in state size, and the worst case is a handler
that *replaces a huge collection*: `self.rows = vec![…10_000]` would
hash ten thousand structs to learn what `.len()` already says. The
probe exploits a serde detail — `Vec`'s `Serialize` impl calls
`serialize_seq(Some(len))` *before* iterating any element. A
serializer that returns `Err` from exactly that call, smuggling the
length out through the error type, reads any collection's length in
O(1), through the same serde machinery, with no per-type code and no
guessing about which types are collections.

The sweep captures `(length, hash)` per field. At finish, a moved
length — or a `Some`/`None` flip, e.g. `Option<Vec<T>>` going
`Some → None` — **proves** change with no content hash. Equal
lengths prove nothing and fall through to the hash compare, so a
same-length edit is still caught. Correctness is untouched; the
create/append/clear-shaped handlers skip their dominant hash
entirely.

### Playing nicely with surgical patches

The `patch_list_at_inline` family mutates a field's cached JS
projection in lockstep with the Rust mutation, so a keyed `pp-for`
reconcile sees the same Array identity with one changed cell. Those
patches *mark* the field; the sweep consumes the mark and re-stamps
the projection to the new version instead of invalidating it. A
patched list never pays a full re-serialize just because the sweep
also noticed the field changed.

### Versioned projections and the typed text lane

The old field cache was invalidated by events; the new one by
arithmetic. A field's JS projection is stored with the version it
was built at; a write bumps the version; a stale stamp means rebuild
on next read. Nothing ever walks a cache to clear it.

Scalar `pp-text` skips JS values entirely: a second special-purpose
serde serializer converts a scalar field straight to the string the
DOM needs — `2.0f64` → `"2"`, `None` → `""` — refusing compound
shapes. A counter increment performs **zero** serde-to-JS
conversions, which a `serde_projection_count` test counter pins in
CI.

### The proxy becomes an interop shim

With the mirrors carrying all reads and writes, the compiled
template plan records `needs_proxy`; a component whose every install
is provably servable Rust-side (bindings, interpolation, listeners,
native models, chains and matches with proxy-free bodies,
item-rooted `pp-for`) **mounts without minting a proxy at all** —
two `Object`s, two trap closures, and a `Proxy::new` saved per
instance, with a `proxies_minted_count` counter pinned to zero in
the acceptance tests. When JS genuinely needs an object there is one
explicit, memoized door:

```rust
let bridge = pocopine::js_bridge(scope_id); // traps call the same mirrors
```

The engine work alone measured **−4 to −7% geomean**, guarded by a
differential harness: five seeds × two hundred randomized list
operations, the DOM checked against a plain-vector oracle after
every step, plus symmetry gates forcing the keyed fast paths and the
generic path to produce identical trees.

## Templates: chains, enum matching, and honest CSS

### `pp-else-if` / `pp-else`

```html
<template pp-if="count > 5">      <p>big</p></template>
<template pp-else-if="count > 0"> <p>small</p></template>
<template pp-else>                <p>zero</p></template>
```

Chains are contiguous `<template>` siblings compiled into **one
controller with one effect**. The semantics are Vue's, deliberately:
first truthy branch wins; conditions past the active branch are
neither evaluated nor tracked; exactly one clone exists, always at
the chain's position; same branch ⇒ no DOM work; branch change ⇒
remount with transitions. A re-flip back to a branch whose clone is
still leave-animating *cancels the leave and resumes the same clone*
— a double-toggled dialog doesn't flicker or lose form state.
Orphan members, members after `pp-else`, and expressions on
`pp-else` are compile errors with real messages.

### `pp-match` — enums drive UI

```rust
#[derive(Default, Serialize, Deserialize)]
enum Status {
    #[default]
    Idle,
    Loading,
    Ready(String),
    Err { code: i32 },
}
```

```html
<template pp-match="status">
  <template pp-case="Idle | Loading">
    <p class="pending">pending…</p>
  </template>
  <template pp-case="Ready" pp-let="msg">
    <p class="ready">{{msg}}</p>
  </template>
  <template pp-case="_">
    <p class="error">something broke</p>
  </template>
</template>
```

Arms are **literal variant names** (`Ready`, `Idle | Loading`, `_`)
— expression-shaped arms, duplicate variants, and arms after `_` are
compile errors. Matching follows serde's externally-tagged encoding:
unit variants by name, payload variants by tag with `pp-let` binding
the payload (`pp-let="e"`, then `e.code` for a struct variant). A
plain `String` field matches its value as the tag, so `pp-match`
doubles as a string switch. **Same variant, new payload ⇒ no
remount** — the payload updates in place through a per-mount payload
scope; variant change remounts like a chain branch.

### Comment anchors — a CSS bug class, closed

Structural `<template>`s used to stay in the live DOM. A template is
invisible, but it is an **element**, and CSS structural selectors
count elements:

<video autoplay loop muted playsinline controls width="100%"><source src="/assets/blog/comment-anchor.webm" type="video/webm"><source src="/assets/blog/comment-anchor.mp4" type="video/mp4"></video>

```text
before 0.2.0:                       0.2.0:
<ul>                                <ul>
  <li>…</li>                          <li>…</li>
  <li>…</li>                          <li>…</li>
  <template pp-for>  ← phantom        <!--pp:for-->  ← invisible to CSS
</ul>                               </ul>

li:last-child  → matched NOTHING    li:last-child → the last row, finally
```

At install, every structural controller swaps its template for a
labeled comment — `<!--pp:cond-->`, `<!--pp:match-->`,
`<!--pp:for-->` — and inserts clones in front of it. `:nth-child`,
`:last-child`, and Stylekit's `space-*`/`divide-*` utilities finally
see only live content, and the label tells a devtools inspection
which controller owns each position. Authoring is unchanged; the one
shipping subtlety was that anything resolving data through DOM
ancestry must do so *before* the swap — the keyed row-plan registry
taught us that one.

## The mutation channel

### The motivating profile

After the signals work we profiled `runLots(10000)` — create ten
thousand keyed rows — and found the dependency graph costing
**0.0 ms**. The cost had moved entirely to the wasm↔JS boundary:

```text
mount profile, per 10K rows (before the channel):
  clone_template_body      43 ms   ← cloneNode × 10,000
  initial_binding_apply    34 ms   ← setText/setAttribute × ~30,000
  listener_installation    27 ms   ← node-path walks × 20,000
  paths + insertion        28 ms   ← appendChild × 10,000 + walks
                          ─────
        ~10 crossings × 10,000 rows ≈ 100,000 crossings per click
```

Each crossing costs ~50–100 ns of pure overhead, and varying-shape
calls from wasm also defeat the JS engine's inline caches. No amount
of Rust-side cleverness removes a cost that *is* the boundary — the
fix had to change how many times we cross it.

### One crossing per batch

0.2.0 mounts a keyed batch with a single call into a ~150-line JS
interpreter that ships beside the wasm:

<video autoplay loop muted playsinline controls width="100%"><source src="/assets/blog/mutation-channel.webm" type="video/webm"><source src="/assets/blog/mutation-channel.mp4" type="video/mp4"></video>

```text
wasm (Rust)                            JS interpreter (ONE call)
───────────                            ─────────────────────────
plan descriptor — registered ONCE      pp_chan_mount_rows(plan, proto,
per row plan: node paths, binding        anchor, items, range, scopeIds,
kinds (text/class), item-rooted          parentVals):
expressions, listener paths              for each row i:
                                           root = proto.cloneNode(true)
per flush:                                 root[scopeKey] = scopeIds[i]
  key dedup, LoopScope minting             for each binding:
  (pure Rust — scope ids cross               node = walk(root, path)
  as one numeric slice)                      node.text/class =
  parent-rooted values resolved                eval(expr, items[i],
  once (row-invariant)                              parentVals)
                                           collect handles; append
                                         insertBefore(fragment, anchor)
                                         return [root, …nodes] flat
```

The insight that shaped the design: **the items array already lives
on the JS side** (it's the field's serde projection), and a row
plan's operation sequence is static at compile time. So instead of
streaming opcodes through shared memory — the classic byte-buffer
"sledgehammer" design — the plan registers a *descriptor* once and
each flush is one rich call. Same single-crossing economics, and
**no encoder/decoder pair that can drift**: the descriptor is data,
not a wire format.

What deliberately stays in Rust: key resolution and duplicate
detection (the reconcile pool is Rust), scope minting, all
`RowInstance` bookkeeping for later updates, and the list watcher
that owns parent-field reactivity. The interpreter is a *mount
executor*, not a second framework.

### Parent-dependent bindings

A row binding like `:class="selected_id == row.id ? 'danger' : ''"`
mixes a per-row side (`row.id`) with a row-invariant side
(`selected_id`). The flush resolves the invariant side **once** —
untracked, deliberately: the flush runs inside the list's reconcile
effect, and a tracked read there would subscribe the entire
reconcile to `selected_id`, when the list watcher owns that
subscription — and passes it as a value the interpreter indexes. The
old path repainted parent-dependent bindings in a 10,000-row walk
*after* mount; that walk is gone.

### Fallback, not faith

The channel refuses rather than guesses:

- Node paths are validated against the prototype **before any DOM
  mutation**; an unresolvable path returns the whole batch to the
  direct web-sys path, which warns and degrades per-entry — the
  long-standing contract.
- `$store`/`$route`-rooted bindings are channel-ineligible: they
  resolve through the magic-scope layer the flush-time evaluator
  doesn't carry, so those plans take the direct path.
- A runtime toggle keeps both lanes in one binary
  (`window.__POCOPINE_MUTATION_CHANNEL = false`), which is also how
  the A/B numbers below were measured.

### Parity is tested, not assumed

Any divergence between the interpreter and the Rust originals shows
up as text or classes that *flap* between mount and first update —
so the helpers mirror to an exacting level: `Object.keys` rather
than `for…in` (which walks inherited prototype properties),
`Reflect.get`-on-primitive semantics (no auto-boxed
`"abc".length`), guarded `JSON.stringify` (a circular value renders
`""` like the Rust side instead of throwing through the wasm
import), and one shared number-to-string used by *every* text path —
channel, direct, and the typed text lane — so `1e21` can't render
two different ways. A differential test mounts the same list through
both lanes and asserts **byte-identical** `innerHTML`; a max-effort
multi-agent review of the branch caught five parity divergences
before release, and each fix is pinned by that test.

### What it bought

Same-binary A/B pairs, run in both orderings to defeat thermal
drift:

| step | runLots(10000) | add(1000) |
|---|---:|---:|
| the channel itself | **−8.7%** | −4 to −8% |
| + enter-skip & length probe | **−5%** | −7% |
| + parent-value painting | **−5.1%** | −4.9% |

(The enter-skip: row plans share one prototype, and enter
transitions key entirely on subtree attributes — so one
`has_transition_in_subtree(proto)` check replaces 10,000 per-row
walks.) Cumulatively, runLots moved from **1.41× to 1.17× vanilla**.
The remaining gap is browser layout inside the action window and the
benchmark app's own row-generation string work — costs vanilla pays
too, just from a shorter call stack.

## The numbers

Full suite, one session, headless Firefox, mean ms over the
js-framework-benchmark-style keyed table (ratio vs vanilla in
parentheses; lower is better):

| action | vanilla | **pocopine 0.2.0** | Vue | Yew | Leptos |
|---|---:|---:|---:|---:|---:|
| run(1000) | 165 | **193 (1.17×)** | 194 (1.17×) | 217 (1.31×) | 222 (1.35×) |
| update every 10th | 144 | **158 (1.09×)** | 150 (1.04×) | 162 (1.12×) | 142 (0.98×) |
| select | 110 | **125 (1.14×)** | 115 (1.04×) | 129 (1.17×) | 994 (9.00×) |
| swapRows | 146 | **150 (1.02×)** | 145 (0.99×) | 163 (1.11×) | 155 (1.06×) |
| remove | 162 | **170 (1.05×)** | 169 (1.04×) | 188 (1.16×) | 167 (1.03×) |
| clear | 165 | **195 (1.18×)** | 178 (1.08×) | 219 (1.32×) | 213 (1.29×) |
| runLots(10000) | 586 | **688 (1.17×)** | 721 (1.23×) | 852 (1.45×) | 899 (1.53×) |
| add(1000) | 226 | **230 (1.02×)** | 259 (1.14×) | 273 (1.21×) | 268 (1.18×) |
| **geomean** | **185** | **204 (1.10×)** | 201 (1.09×) | 227 (1.23×) | 284 (1.53×) |

Pocopine lands at **1.10× vanilla** — a statistical tie with Vue on
geomean, ahead of both Rust frameworks on every action, and ahead of
Vue where the channel works: **runLots 688 vs 721** and **add within
2% of hand-written vanilla JS**.

### The experiments that didn't make it

Three optimizations were built, measured, and **reverted**, each
with its numbers recorded in the repo so nobody re-runs a dead end:

- **String interning in the hot path**: regressed runLots +7% — the
  two-level field-signal map was already cheap.
- **A JSON lane for big projections** (`serde_json::to_string` +
  one `JSON.parse` instead of per-property object building):
  measured neutral — bracket profiling showed the projection was
  never the cost — and it pulled serde_json into the wasm bundle
  (+40 KB).
- **Compile-time handler touch hints** (prove a handler's write-set
  from its AST, skip the sweep): measured neutral — FNV over
  benchmark-sized state is sub-millisecond — and its
  write-detection leaned on a hand-maintained list of std
  mutating-method *names*, which is unmaintainable by definition.
  Removed entirely.

The pattern behind all three: profile first, let the same-session
A/B pair decide, and treat "no win" as a verdict, not a challenge.

## Breaking changes

`pocopine-core` moves to 0.2.0:

- The Alpine-era magics `$el` / `$refs` / `$dispatch` / `$id` are
  removed (a survey found zero uses; `pp-ref` + typed ref accessors
  and `emit()` are the supported paths).
- Pre-signals internals are gone: the old cache-invalidation entry
  points, the dual dependency tables, proxy-as-engine plumbing. App
  code that stuck to the documented surface — struct fields,
  handlers, `#[computed]` / `#[watch]` / `#[store]`, the `pp-*`
  directives — needs no changes.
- DOM-shape snapshots churn where structural templates became
  comments, and CSS that *compensated* for phantom template siblings
  can delete the workaround.

## What's next

The bundle is the next frontier. Templates currently ship as source
strings plus a runtime parser; the plan is two-tier — small
components fully compile-time-parsed, page-sized templates split out
and **server-rendered**. The comment anchors this release introduced
are, not coincidentally, exactly the stable positions a hydrator
needs to claim server-rendered rows and branches. That's the 0.3
conversation.
