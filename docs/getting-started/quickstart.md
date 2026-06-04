---
title: "Quickstart"
description: "Scaffold an app, write your first component, and run it."
---

# Quickstart

This walks you from an empty directory to a running component. It
assumes the [CLI is installed](./installation.md).

## 1. Scaffold an app

A pocopine app is a regular Rust library crate. Add `pocopine`
(the runtime) and, optionally, `pine` (UI primitives).

```bash
cargo new --lib hello-pine
cd hello-pine
cargo add pocopine pine
```

## 2. Write a component

A component is a Rust struct plus a sibling `.poco` template. The
struct holds state; `#[handlers]` methods mutate it.

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

Drop `<counter></counter>` into your `index.html` and the runtime
mounts it.

## 3. Run it

`pocopine dev` builds the wasm bundle, serves it on a local port, and
rebuilds on save.

```bash
pocopine dev
# → listening on http://127.0.0.1:5243
```

Ship a release build with `pocopine build --release`, then
`pocopine deploy`.

## Next steps

- **[Components](../guides/components/README.md)** — structure and state, in depth.
- **[Templates](../guides/poco/README.md)** — the full `.poco` directive set.
- **[Tutorials](../tutorials/issue-tracker-sync.md)** — build a real feature end to end.
