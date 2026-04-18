# pocopine design docs

Working notes, plans, and code sketches. Source of truth for *why* the code
looks the way it does — the code itself tells you *what*.

- [`components/`](./components/) — opinionated structure for building
  components and managing state. Read first.
- [`reactivity/`](./reactivity/) — the reactive core: effects, dep tracking,
  the JS `Proxy` bridge, and everything we want to bolt on next.
- [`pcx/`](./pcx/) — the `.pcx` template format (HTML + directives),
  paired with sibling `.rs` + `.css` files. No mixed-language SFCs.

Formal design decisions live one level up in [`../rfcs/`](../rfcs/).
For the server-function end-to-end example, see
[`examples/blog/`](../examples/blog/) — it wires `App`, a `#[server]`
function, and an axum server binary into one working app.
