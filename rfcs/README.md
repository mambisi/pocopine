# pocopine RFCs

Formal design documents for non-trivial features. Each RFC captures a
committed decision — what we're building, why, and what's explicitly
out of scope — so downstream code and docs can reference a stable target.

Conventions:

* One RFC per file: `rfc-NNN-short-name.md`.
* Status lifecycle: **Draft → Accepted → Implemented → Superseded**.
* Changes to an Accepted RFC require a new RFC that supersedes it.
* Docs under `docs/` are the *explanatory* surface; RFCs are the
  *authoritative* surface.

## Index

| # | Title | Status |
|---|---|---|
| 001 | [Components](./rfc-001-components.md) | Accepted |
| 002 | [Application framework, stores, server functions](./rfc-002-app-stores-servers.md) | Accepted |
| 003 | [Client-side SPA router](./rfc-003-router.md) | Accepted |
| 004 | [`pp-for` (list iteration)](./rfc-004-pp-for.md) | Accepted |
| 005 | [`pp-transition` (enter / leave animations)](./rfc-005-pp-transition.md) | Implemented |
| 006 | [`pp-teleport` (dialogs, popovers, portals)](./rfc-006-pp-teleport.md) | Implemented |
| 007 | [`pp-for` keyed iteration](./rfc-007-pp-for-keys.md) | Implemented |
| 008 | [event handler arguments](./rfc-008-event-handler-args.md) | Implemented |
| 009 | [`pp-model` on components](./rfc-009-pp-model-components.md) | Implemented |
| 010 | [Attribute fallthrough + `cx!`](./rfc-010-attribute-fallthrough.md) | Implemented |
| 011 | [Scoped slots](./rfc-011-scoped-slots.md) | Implemented |
| 012 | [Template expression evaluator](./rfc-012-expression-evaluator.md) | Implemented |
| 013 | [Key modifiers on `pp-on`](./rfc-013-pp-on-key-modifiers.md) | Implemented |
