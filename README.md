# pocopine

A **client-side reactive runtime** and **component framework** for the web,
written in Rust and compiled to WebAssembly.

In spirit pocopine is a Rust/WASM port of [Alpine.js][alpine] — the
directive model, the pragmatism, the "sprinkle-of-JS" ergonomics —
layered with a Vue-3-style reactive core (real `Proxy` traps, auto
dep-tracking), tag-based components, a type-safe server-function
bridge, and a built-in SPA router. Templates live in plain HTML files
(`.poco`), styles in plain CSS files, logic in plain Rust files. No
mixed-language SFCs, no JavaScript toolchain.

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

## Try an example

Build the tooling once:

```bash
cargo install wasm-pack
cargo build -p pocopine-cli --release
```

Then pick an example:

```bash
# static counter
cargo run -p pocopine-cli -- dev --path examples/counter

# Hacker News clone (Algolia API, comment tree, search with debounce)
cargo run -p pocopine-cli -- dev --path examples/hn

# SPA with client-side routing
cargo run -p pocopine-cli -- dev --path examples/spa
```

| Example | What it shows |
|---|---|
| [`counter`](./examples/counter) | Single component, basic directives |
| [`todo`](./examples/todo) | Multi-component, slots, stores |
| [`blog`](./examples/blog) | `App` + `#[server]` + axum server bin |
| [`spa`](./examples/spa) | Router + `<pp-outlet>` + `pp-route` |
| [`hn`](./examples/hn) | Full SPA — routing, server fns, transitions, pp-for |
| [`site`](./examples/site) | The marketing page, dogfooded |
| [`tailwind`](./examples/tailwind) | Tailwind v4 + `.poco` scanning (CDN-mode for demo) |

## Repository layout

```
crates/
├── pocopine-core/     reactive core, walker, directives, router
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

1. **Runtime** — directive walker, reactive engine, component scopes.
   Port of Alpine's model; no virtual DOM, mutations happen in place.
2. **Templates** — `.poco` files are pure HTML with `pp-*` directives.
   The `#[component]` macro wires them to Rust structs at compile
   time via `include_str!`.
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

## Styling with Tailwind / DaisyUI

Tailwind v4 is a first-class option — opt in via `Cargo.toml` and
`pocopine-cli` downloads the standalone Rust binary on first run,
then spawns it alongside `wasm-pack`:

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
