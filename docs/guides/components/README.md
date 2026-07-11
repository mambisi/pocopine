---
title: "Components & state management"
description: "Opinionated structure for building pocopine components and managing their state."
---

# Components & state management

pocopine is an opinionated framework. For every common decision about
component structure and state placement there is one canonical answer,
documented in this section. Pick the pattern that fits; don't invent
alternatives.

## In this section

1. [`01-structure.md`](./01-structure.md) — file layout, component shape,
   naming conventions, handler conventions. The reference for what every
   component looks like.
2. [`02-state.md`](./02-state.md) — state management: local state,
   parent-to-child props, child-to-parent events, global stores, and async
   data. One canonical pattern per category.
3. [`03-composition.md`](./03-composition.md) — composing components:
   kebab-case custom-element tags, attribute props (static and `pp-bind:`),
   slots (default and scoped), and the `pp-for` iteration syntax.
4. [`04-lifecycle.md`](./04-lifecycle.md) — the four lifecycle hooks
   (`on_setup`, `on_mount`, `on_ready`, `on_unmount`), their order and
   receivers, and the borrow rules that govern deferred work.
5. [`05-extractors.md`](./05-extractors.md) — declaring what a method needs
   by type: lifecycle-context extractors for hooks, `FromHandlerArg` for
   event-handler arguments.
6. [`06-events.md`](./06-events.md) — emitting and listening: the `emit`
   one-liner, typed `#[derive(Emit)]` events, cancelable events, and the
   `on` / `on_emit` listeners.
7. [`07-dynamic-components.md`](./07-dynamic-components.md) — selecting a
   component reactively with typed `ComponentRef` values, forwarding props,
   preserving instances with `keep-alive`, and validating data-driven names.
