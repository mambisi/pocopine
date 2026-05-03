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
| 032 | [Extractor-style `LifecycleContext` params for `on_mount` / `on_ready`](./rfc-032-lifecycle-element-param.md) | Implemented |
| 033 | [Primitive roles — centralized default-element mapping](./rfc-033-primitive-roles.md) | Implemented |
| 034 | [`pp-roving.virtual` (activedescendant mode)](./rfc-034-pp-roving-activedescendant.md) | Implemented |
| 042 | [`class` / `style` parity — arrays, custom properties, `\|important`](./rfc-042-class-style-parity.md) | Draft |
| 043 | [`pocopine::text` layout engine](./rfc-043-text-layout.md) | Implemented |
| 044 | [`#[model]` field role for two-way component contracts](./rfc-044-model-fields.md) | Draft |
| 045 | [Single-root `.poco` templates enforced at compile time](./rfc-045-single-root-templates.md) | Implemented |
| 048 | [Scoped async tasks and extractor-driven `#[computed]`](./rfc-048-hooks.md) | Implemented |
| 046 | [`Children` extractor on `LifecycleContext`](./rfc-046-children-extractor.md) | Draft |
| 047 | [`$slots` magic + slot-presence probes](./rfc-047-slots-magic.md) | Draft |
| 049 | [Typed slot contracts — compile-time child constraints](./rfc-049-typed-slot-contracts.md) | Draft |
| 050 | [Real HTML parser at compile time — `html5ever` in `pocopine-macros`](./rfc-050-html5ever-compile-time-parser.md) | Draft |
| 051 | [Component registry safety — aliases, prefixes, boot verification](./rfc-051-component-registry-safety.md) | Deferred to 056 |
| 052 | [Typed structural parent extractors](./rfc-052-parent-extractors.md) | Deferred to 056 |
| 053 | [Typed component interaction surface](./rfc-053-typed-component-interaction.md) | Deferred to 056 |
| 054 | [Compiled `pp-for` row plans](./rfc-054-compiled-pp-for-row-plans.md) | Draft |
| 055 | [Typed context ergonomics on top of keyed `provide` / `inject`](./rfc-055-typed-context.md) | Deferred to 056 |
| 056 | [Component interaction safety batch](./rfc-056-component-interaction-safety-batch.md) | Implemented (all phases + follow-on infrastructure) |
| 057 | [Compile-time template plans](./rfc-057-compile-time-template-plans.md) | Superseded by 058 |
| 058 | [Compiled views and walker removal](./rfc-058-compiled-views-walker-removal.md) | Phases 1–6.5 implemented; 7–8 deferred |
| 059 | [Server-side rendering and hydration](./rfc-059-server-side-rendering-and-hydration.md) | Draft (revised post-RFC-058 Phase 6.5) |
| 060 | [`uses` as the authoritative component registry](./rfc-060-component-uses-registry.md) | Draft |
| 061 | [Compiled-mount-only architecture](./rfc-061-compiled-mount-only.md) | Draft |
| 062 | [Per-component mount specialization](./rfc-062-per-component-mount-specialization.md) | Draft |
| 063 | [Directive cleanup for Vue-3 alignment](./rfc-063-directive-cleanup-vue-alignment.md) | Draft |
| 064 | [Performance roadmap to community-credible benchmarks](./rfc-064-performance-roadmap.md) | Draft |
| 065 | [Route-cluster bundling](./rfc-065-route-cluster-bundling.md) | Draft |
| 066 | [Server-function auth and access policy](./rfc-066-server-function-auth.md) | Draft |
| 067 | [Background jobs with Redis and memory backends](./rfc-067-redis-background-jobs.md) | Draft |
| 068 | [SVG namespace template support](./rfc-068-svg-namespace-template-support.md) | Draft |
| 069 | [Unified observability, logging, and analytics](./rfc-069-observability.md) | Draft |
| 070 | [JWT-based authentication verification](./rfc-070-jwt-auth-verification.md) | Draft |
| 070 | [Event spine and live invalidation streams](./rfc-070-event-spine-and-live-invalidation.md) | Draft |
| 071 | [Offline sync protocol](./rfc-071-offline-sync-protocol.md) | Draft |
| 072 | [Yrs collaboration over WebSocket and Redis](./rfc-072-yrs-collaboration.md) | Draft |
