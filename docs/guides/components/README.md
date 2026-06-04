---
title: "Components & state management"
description: "Opinionated structure for building pocopine components and managing their state."
---

# Components & state management

pocopine is an opinionated framework. When there's a choice to make
about how to structure a component or where to put state, we pick one
answer and document it here. Users aren't meant to invent this stuff.

> **The rule:** if this folder says "do X," do X. If it says "don't do Y,"
> don't do Y. If something isn't covered, ask for it to be added here
> before inventing a pattern. That's how we stay consistent.

Docs in this folder:

1. [`01-structure.md`](./01-structure.md) — where files live, what a
   component looks like, naming conventions, handler conventions.
   The "every component I write looks like this" doc.
2. [`02-state.md`](./02-state.md) — state management: local state,
   parent ↔ child communication, global stores, async data. One
   canonical pattern per category.
3. [`03-composition.md`](./03-composition.md) — composing components:
   `<pp-*>` custom-element tags, attribute props (static and
   `pp-bind:`), slots, and the iteration syntax planned for when
   array reactivity lands.
