---
title: "Quickstart"
description: "Scaffold an app, write your first component, and run it."
---

# Quickstart

This walks you from an empty directory to a running component. It
assumes the [CLI is installed](./installation.md).

## 1. Scaffold an app

A pocopine app is a Rust library crate compiled to WebAssembly. Create
one and add the `pocopine` runtime (and optionally `pine` for UI
primitives):

```bash
cargo new --lib hello-pine
cd hello-pine
cargo add pocopine serde --features serde/derive
```

Then set the crate type in `Cargo.toml` so wasm-pack can build a browser-
loadable module:

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

## 2. Write a component

A component is a Rust struct plus a `.poco` template file. The struct
holds state; `#[handlers]` methods mutate it.

```rust
// src/lib.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub n: u32,
}

#[handlers]
impl Counter {
    pub fn bump(&mut self) {
        self.n += 1;
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<Counter>().run();
}
```

```poco
<!-- src/Counter.poco -->
<button @click="bump">
  clicked <strong pp-text="n"></strong> times
</button>
```

By default `#[component]` looks for a template named after the struct
(`Counter.poco`) in the same directory as the `.rs` file.

Create an `index.html` with a `pp-app` root — `App::run()` scans for
that attribute and mounts all registered components it finds inside it:

```html
<!-- index.html -->
<!doctype html>
<html>
<body>
  <div pp-app>
    <counter></counter>
  </div>
  <script type="module">
    import init from "/pkg/hello_pine.js";
    init();
  </script>
</body>
</html>
```

## 3. Run it

`pocopine dev` builds the wasm bundle, serves it on a local port, and
rebuilds on save.

```bash
pocopine dev
# → listening on http://127.0.0.1:5243
```

Ship a release build with `pocopine build --release`, then deploy with
`pocopine deploy`.

## Next steps

- **[Components](../guides/components/README.md)** — structure and state, in depth.
- **[Templates](../guides/poco/README.md)** — the full `.poco` directive set.
- **[Tutorials](../tutorials/issue-tracker-sync.md)** — build a real feature end to end.
