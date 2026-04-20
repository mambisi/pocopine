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
| 014 | [Focus & timing utilities](./rfc-014-focus-utilities.md) | Implemented |
| 015 | [`pp-anchor` (popover positioning)](./rfc-015-pp-anchor.md) | Implemented |
| 016 | [`pp-resize` and `pp-intersect`](./rfc-016-pp-resize-pp-intersect.md) | Implemented |
| 017 | [`pp-on:click.outside`](./rfc-017-click-outside.md) | Implemented |
| 018 | [`$id` magic (unique IDs)](./rfc-018-id-magic.md) | Implemented |
| 019 | [`pp-as` polymorphic rendering](./rfc-019-pp-as.md) | Implemented |
| 020 | [`:attr` / `@event` shorthand](./rfc-020-shorthand-prefixes.md) | Implemented |
| 021 | [`scroll_lock` utility](./rfc-021-scroll-lock.md) | Implemented |
| 022 | [`pp-roving` tabindex / arrow navigation](./rfc-022-pp-roving.md) | Implemented |
| 023 | [Pine MVP — 8 unstyled UI primitives](./rfc-023-pine-mvp.md) | Implemented |
| 024 | [Expression-based directive values](./rfc-024-expression-values.md) | Implemented |
| 025 | [Inline `{expr}` text interpolation](./rfc-025-text-interpolation.md) | Implemented |
| 026 | [`post_mount` lifecycle + `#[watch(field)]` sugar](./rfc-026-post-mount-watch-field.md) | Implemented |
| 027 | [Parent-scope context (`provide` / `inject`)](./rfc-027-provide-inject.md) | Implemented |
| 028 | [`emit` / `emit_from` helpers](./rfc-028-emit.md) | Implemented |
| 029 | [Rename `post_mount` → `on_ready`](./rfc-029-on-ready-rename.md) | Implemented |
| 030 | [Typed `InjectKey` (Symbol-style provide/inject)](./rfc-030-inject-key-symbols.md) | Implemented |
| 031 | [`#[prop]` / `#[state]` field roles](./rfc-031-prop-vs-state.md) | Implemented (breaking) |
| 032 | [Extractor-style params for `on_mount` / `on_ready`](./rfc-032-lifecycle-element-param.md) | Draft |
