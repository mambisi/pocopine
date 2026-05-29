<p align="center">
  <img src="./docs/assets/mascot.svg" alt="pocopine mascot" width="220">
</p>

<h1 align="center">pocopine</h1>

<p align="center">
  <em>A tiny, blazing-fast, reactive Rust/WASM UI framework with a
  full primitive library — ships server-rendered HTML, animates by
  default, benchmarks with the mature ones.</em>
</p>

---

pocopine is a directive-driven Rust/WASM UI framework: a Vue-3-style
reactive core (real `Proxy` traps, auto dep-tracking) wired into
compiled `.poco` template plans, with tag-based components, a
type-safe server-function bridge, and a built-in SPA router. Runtime
directive handling still exists for dynamic/adopted DOM boundaries,
but normal component templates mount through macro-generated install
code.
Templates live in plain HTML files (`.poco`), styles in plain CSS
files, logic in plain Rust files. No mixed-language SFCs, no virtual
DOM, and no JavaScript toolchain unless you opt into Pocopine-managed
typed `.client.ts` modules.

> Status: **pre-1.0 / experimental.** The API is still moving; every
> breaking change lands in an RFC under [`rfcs/`](./rfcs/).

```rust
// examples/counter/src/lib.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter { pub count: i32, pub label: String }

#[handlers]
impl Counter {
    pub fn increment(&mut self) { self.count += 1; }
    pub fn decrement(&mut self) { self.count -= 1; }
}

#[wasm_bindgen(start)]
pub fn main() { App::new().register::<Counter>().run(); }
```

```html
<!-- examples/counter/src/Counter.poco -->
<div>
  <p><strong pp-text="count"></strong> <span pp-text="label"></span></p>
  <button pp-on:click="decrement">-</button>
  <button pp-on:click="increment">+</button>
</div>
```

```html
<!-- examples/counter/index.html -->
<body>
  <counter label="clicks"></counter>
  <script type="module">
    import init from "/pkg/counter.js";
    init();
  </script>
</body>
```

That's the whole counter. No virtual DOM, no build step beyond
`wasm-pack`, no `Rc<RefCell<_>>` in the author's code.

## Highlights

* **Directives:** `pp-text`, `pp-html`, `pp-bind:<attr>`, `pp-on:<event>`,
  `pp-show`, `pp-model`, `pp-init`, `pp-for`, `pp-if`, `pp-cloak`,
  `pp-transition:*`, `pp-teleport`, `pp-ref`, `pp-route`.
* **Compiled templates.** Component templates, lifted `pp-if` /
  `pp-for` / `pp-teleport` bodies, and dynamic slot fragments install
  through generated closures instead of a generic fragment applier.
* **Tag-based components.** Declare a struct, drop `<my-thing>` in
  HTML, done. Props bind by attribute name (kebab → snake), slots via
  `<slot>`.
* **Lifecycle hooks.** Write `pub fn on_mount(&mut self)` /
  `pub fn on_unmount(&mut self)` and the macro auto-wires them; no
  `pp-init` attribute needed.
* **Devtools overlay.** Opt-in with `App::with_devtools()`. Lists live
  scopes, their fields, and registered refs; `Ctrl+Shift+D` toggles.
* **Reactive core.** `signal()` / `computed()` / `watch()` compose with
  components through a `js_sys::Proxy` — same semantics as Vue 3's
  reactivity.
* **Stores.** `#[store]` gives you an app-wide reactive cell addressed
  as `$store.*` from any template.
* **Server functions.** `#[server] async fn` compiles to a POST route
  on the server and a typed `fetch` stub on the client. One function,
  two build targets.
* **SPA router.** `App::route::<C>("/item/:id")` + `<pp-outlet>` +
  `pp-route` on links. Path params become component props through
  the same pipeline as HTML attributes.
* **Opinionated layout.** One canonical way per decision: `.poco`
  templates, `.rs` structs, `.css` stylesheets; each lives in its own
  file. No runtime config-shopping.
* **Tailwind-friendly transitions.** `pp-transition:enter`,
  `enter-start`, `enter-end` (and leave variants) — class strings go
  straight through, no custom CSS language.
* **Optional client modules.** Typed `.client.ts` files can import npm
  SDKs through the Pocopine CLI-managed esbuild path. No TSX/JSX,
  untyped managed modules, or framework islands.

## Performance

The `js-framework-benchmark` keyed-table action plan, run locally
under headless Firefox against pinned Rust/WASM and JS competitors.
Numbers are wall-clock geometric means (lower is better) across:
`run(1000)`, `update every 10th`, `select`, `swapRows`, `remove`,
`clear`, `runLots(10000)`, `add(1000)`. Vanilla is always measured as
the control because browser timing can drift between runs.

| framework  | geomean (ms) | vs vanilla |
|------------|-------------:|-----------:|
| vanilla JS |       185.41 |       1.00× |
| Vue 3      |       202.17 |       1.09× |
| **pocopine** |   **215.92** |   **1.16×** |
| Yew        |       225.07 |       1.21× |
| Leptos     |       281.45 |       1.52× |

pocopine now sits between Vue and Yew in the Firefox harness after
RFC 064's compiled fragment installs, static string surfaces,
compiled expression envelope, and keyed `pp-for` fast paths. No
virtual-DOM diff runs in the hot path; generated template code and
fine-grained `Proxy` reactivity mutate real DOM nodes in place.

Reproduce locally:

```bash
python3 jsbench/measure.py --browser firefox jsbench/vanilla
./jsbench/benchmark.sh pocopine --browser firefox --no-build
./jsbench/benchmark.sh --all --browser firefox
```

The harness pins each competitor to a fixed version and runs the
plan with 2 warm-up + N measured passes per action. Source under
[`jsbench/`](./jsbench/).

## Get started in 60 seconds

### 1. Install the CLI

The `pocopine` CLI handles building, serving, and hot-reload —
one install covers all three.

```bash
cargo install pocopine-cli
```

From a source checkout, use the repo helper:

```bash
./install.sh
pocopine doctor --path .
```

If a project needs wrappers or pinned tool paths, add a local
`.pocopine.toml`. Pocopine reads this file instead of guessing from
global tools, without shelling out through npm scripts:

```toml
[tools]
cargo = { command = "cargo", args = ["+stable"] }
rustc = { command = "rustc", args = ["+stable"] }
wasm-pack = "/opt/tools/wasm-pack"
package-manager = "pnpm"
node = "node"
tailwindcss = "tailwindcss"
```

### 2. Scaffold an app

A pocopine app is a regular Rust library crate. Add `pocopine`
(runtime) and `pine` (optional UI primitives).

```bash
cargo new --lib hello-pine
cd hello-pine
cargo add pocopine pine
```

### 3. Write your first component

A component is a Rust struct plus a sibling `.poco` template.

```rust
// src/lib.rs
use pocopine::prelude::*;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Counter.poco")]
pub struct Counter { pub n: u32 }

#[handlers]
impl Counter {
    pub fn bump(&mut self) { self.n += 1; }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<Counter>().run();
}
```

```html
<!-- src/Counter.poco -->
<button @click="bump">
  clicked <strong pp-text="n"></strong> times
</button>
```

### 4. Run it

`pocopine dev` builds the wasm bundle, serves it on a local
port, and rebuilds on save.

```bash
pocopine dev
# → listening on http://127.0.0.1:5243
```

Ship with `pocopine build --release`.

## Examples

Drop into any one with `pocopine dev --path examples/<name>`:

| Example | What it shows |
|---|---|
| [`counter`](./examples/counter) | Single component, basic directives |
| [`todo`](./examples/todo) | Multi-component, slots, stores |
| [`blog`](./examples/blog) | `App` + `#[server]` + axum server bin |
| [`live`](./examples/live) | SSE live invalidation + collection/query refresh |
| [`spa`](./examples/spa) | Router + `<pp-outlet>` + `pp-route` |
| [`hn`](./examples/hn) | Full SPA — routing, server fns, transitions, pp-for |
| [`website`](./examples/website) | Pine UI — every primitive, side-by-side |
| [`site`](./examples/site) | The marketing page, dogfooded |
| [`tailwind`](./examples/tailwind) | Tailwind v4 + `.poco` scanning (CDN-mode for demo) |

## Repository layout

```
crates/
├── pocopine-core/     reactive core, compiled plans, directives, router
├── pocopine-macros/   #[component], #[handlers], #[store], #[server]
├── pocopine-server/   host-side: axum + tower-http glue for #[server]
├── pocopine-cli/      `pocopine build | run | dev`
└── pocopine/          thin façade + prelude (what apps depend on)
docs/                  design notes (how + why)
rfcs/                  accepted design decisions (authoritative)
examples/              runnable apps demonstrating each feature
```

## Architecture

Three layers you can reach for independently:

1. **Runtime** — reactive engine, component scopes, directives, and
   the adopted-DOM bridge for dynamic HTML. No virtual DOM; mutations
   happen in place against the real DOM.
2. **Templates** — `.poco` files are pure HTML with `pp-*` directives.
   The `#[component]` macro wires them to Rust structs, emits static
   template metadata, and specializes eligible binding/listener
   installs at compile time.
3. **Server functions** — `#[server] async fn` on the backend; client
   gets a typed stub that POSTs to `/_pocopine/<fn_name>` and
   deserializes the response. Works with any serde-compatible type.

Design docs live under [`docs/`](./docs); the authoritative
decisions are in [`rfcs/`](./rfcs):

| # | Title |
|---|---|
| 001 | [Components](./rfcs/rfc-001-components.md) |
| 002 | [Application framework, stores, server functions](./rfcs/rfc-002-app-stores-servers.md) |
| 003 | [Client-side SPA router](./rfcs/rfc-003-router.md) |
| 004 | [`pp-for` list iteration](./rfcs/rfc-004-pp-for.md) |
| 005 | [`pp-transition` enter/leave animations](./rfcs/rfc-005-pp-transition.md) |
| 006 | [`pp-teleport` dialogs / popovers / portals](./rfcs/rfc-006-pp-teleport.md) |
| 007 | [`pp-for` keyed iteration](./rfcs/rfc-007-pp-for-keys.md) |

## Styling

**Pine Stylekit is the recommended way to style Pocopine apps** — a
native utility-CSS compiler with Tailwind-shaped classes, compiled
in-process at build time (no external watcher, no Node). It runs **by
default**: write utility classes in your `.poco` templates, declare any
colours in an `@theme` block, link `/pkg/stylekit.css`, and
`pocopine build`/`dev` does the rest. It parses `.poco` with the real
compiler (not text scanning), fails loud on typos with source spans, and
ships Tailwind's default palette + a Preflight. See
[`docs/pine-stylekit.md`](./docs/pine-stylekit.md).

```html
<!-- index.html -->
<link rel="stylesheet" href="/pkg/stylekit.css" />
```

```css
/* app.css — only needed if you use custom colours */
@theme {
  --color-surface: #ffffff;
  --color-accent: oklch(0.54 0.13 252);
}
```

### Tailwind / DaisyUI (fallback)

Prefer Tailwind? It stays a first-class option — add a
`[package.metadata.pocopine.tailwind]` block (with no `[stylekit]`
block) and Stylekit defers to it. `pocopine-cli` downloads the
standalone Rust binary on first run, then spawns it alongside
`wasm-pack`:

```toml
[package.metadata.pocopine.tailwind]
input = "app.css"            # entry CSS
output = "pkg/tailwind.css"  # compiled bundle
# version = "v4.0.0"         # optional — pin the upstream release
# binary = "./tailwindcss"   # optional — use your own binary instead
```

Your `app.css` is a normal Tailwind entry. `.poco` files aren't a
recognised extension, but Tailwind's parser scans raw text, so a
`@source` line is all it takes:

```css
@import "tailwindcss";
@source "./src/**/*.poco";
```

`cargo run -p pocopine-cli -- dev --path examples/tailwind` does the
whole dance: binary in `target/pocopine/bin/tailwindcss`, watch mode,
compiled CSS at `/pkg/tailwind.css`. No Node, no `npm install`.

DaisyUI is a plugin:

```css
@import "tailwindcss";
@plugin "daisyui";
@source "./src/**/*.poco";
```

If your `pp-bind:class="..."` expressions live in Rust strings, expand
the glob: `@source "./src/**/*.{poco,rs}";`.

## Development

```bash
# cross-target checks
cargo check --workspace --target wasm32-unknown-unknown
cargo clippy --workspace --all-targets -- -D warnings

# core unit tests
cargo test -p pocopine-core --lib
```

PRs welcome — non-trivial features should open an RFC first (or be
paired with one in the same PR). See [`rfcs/README.md`](./rfcs/README.md)
for the convention.

## Inspiration

* [**Alpine.js**][alpine] — the directive model and author ergonomics.
* [**Vue 3**](https://github.com/vuejs/core) — the `Proxy`-based
  reactive core.
* [**Headless UI**](https://headlessui.com) — the `<Transition>` API
  that `pp-transition:*` mirrors.
* [**Solid**](https://solidjs.com) / [**Leptos**](https://leptos.dev) —
  fine-grained reactivity references.

## License

Dual-licensed under either of

* **Apache License, Version 2.0** ([`LICENSE-APACHE`](./LICENSE-APACHE))
* **MIT License** ([`LICENSE-MIT`](./LICENSE-MIT))

at your option.

[alpine]: https://alpinejs.dev
