---
title: "Introduction"
description: "What pocopine is — a full-stack Rust application framework — and how its pieces fit together."
---

# Introduction

**pocopine is a full-stack application framework written in Rust.** You
build the whole application in one language: the front end, the server
logic it calls, and the data, auth, and deploy layers around them.

- **Front end** — a directive-driven Rust/WASM UI layer with a
  Vue-3-style reactive core (real `Proxy` traps, automatic dependency
  tracking) wired into compiled `.poco` template plans, tag-based
  components, and a built-in SPA router.
- **Back end** — a type-safe server-function bridge. Write an
  `async fn`, mark it `#[server]`, and call it from the client as a
  typed stub. No hand-written endpoints or fetch glue.
- **The rest of the app** — opt-in crates for a query-centric data
  layer, auth, object storage, live updates, background jobs,
  observability, and deploy. You add only what you use.

## How code is organised

Templates live in plain HTML files (`.poco`), styles in plain CSS files,
and logic in plain Rust files. There are no mixed-language single-file
components, no virtual DOM, and no JavaScript toolchain unless you opt
into Pocopine-managed typed `.client.ts` modules.

pocopine is **opinionated**: one canonical way per decision, so
application code stays small and consistent across projects.

## Where to go next

- **[Installation](./installation.md)** — install the CLI and check your toolchain.
- **[Quickstart](./quickstart.md)** — scaffold an app, write a component, run it.
- **Guides** — concept-by-concept coverage of each part of the stack.
- **Tutorials** — build complete features end to end.
