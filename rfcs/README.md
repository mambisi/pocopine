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
| 049 | [Typed slot contracts — compile-time child constraints](./rfc-049-typed-slot-contracts.md) | Implemented |
| 050 | [Real HTML parser at compile time — `html5ever` in `pocopine-macros`](./rfc-050-html5ever-compile-time-parser.md) | Implemented |
| 051 | [Component registry safety — aliases, prefixes, boot verification](./rfc-051-component-registry-safety.md) | Deferred to 056 |
| 052 | [Typed structural parent extractors](./rfc-052-parent-extractors.md) | Deferred to 056 |
| 053 | [Typed component interaction surface](./rfc-053-typed-component-interaction.md) | Deferred to 056 |
| 054 | [Compiled `pp-for` row plans](./rfc-054-compiled-pp-for-row-plans.md) | Implemented |
| 055 | [Typed context ergonomics on top of keyed `provide` / `inject`](./rfc-055-typed-context.md) | Deferred to 056 |
| 056 | [Component interaction safety batch](./rfc-056-component-interaction-safety-batch.md) | Implemented (all phases + follow-on infrastructure) |
| 057 | [Compile-time template plans](./rfc-057-compile-time-template-plans.md) | Superseded by 058 |
| 058 | [Compiled views and walker removal](./rfc-058-compiled-views-walker-removal.md) | Accepted |
| 059 | [Server-side rendering and hydration](./rfc-059-server-side-rendering-and-hydration.md) | Draft (revised post-RFC-058 Phase 6.5) |
| 060 | [`uses` as the authoritative component registry](./rfc-060-component-uses-registry.md) | Accepted |
| 061 | [Compiled-mount-only architecture](./rfc-061-compiled-mount-only.md) | Implemented |
| 062 | [Per-component mount specialization](./rfc-062-per-component-mount-specialization.md) | Accepted |
| 063 | [Directive cleanup for Vue-3 alignment](./rfc-063-directive-cleanup-vue-alignment.md) | Accepted |
| 064 | [Performance roadmap to community-credible benchmarks](./rfc-064-performance-roadmap.md) | Draft |
| 065 | [Route-cluster bundling](./rfc-065-route-cluster-bundling.md) | Draft |
| 066 | [Server-function auth and access policy](./rfc-066-server-function-auth.md) | Implemented |
| 067 | [Background jobs with Redis and memory backends](./rfc-067-redis-background-jobs.md) | Implemented |
| 068 | [SVG namespace template support](./rfc-068-svg-namespace-template-support.md) | Implemented |
| 069 | [Unified observability, logging, and analytics](./rfc-069-observability.md) | Implemented |
| 070 | [JWT-based authentication verification](./rfc-070-jwt-auth-verification.md) | Implemented |
| 071 | [Event spine and live invalidation streams](./rfc-071-event-spine-and-live-invalidation.md) | Implemented |
| 072 | [Offline sync protocol](./rfc-072-offline-sync-protocol.md) | Accepted |
| 073 | [Yrs collaboration over WebSocket and Redis](./rfc-073-yrs-collaboration.md) | Draft |
| 074 | [`pocopine-auth-credentials` and the `Provider` trait](./rfc-074-auth-credentials-and-provider-trait.md) | Accepted |
| 076 | [App plugin lifecycle](./rfc-076-app-plugin-lifecycle.md) | Implemented |
| 077 | [Server plugin lifecycle](./rfc-077-server-plugin-lifecycle.md) | Implemented (Phase 4 typed hooks rejected) |
| 078 | [Client route guards, loaders, and fetch middleware](./rfc-078-client-route-guards-and-loaders.md) | Implemented |
| 079 | [`pine-richtext` TablesExtension](./rfc-079-pine-richtext-tables-extension.md) | Draft |
| 080 | [Heroku-style deploy contract (process graph + services)](./rfc-080-deploy-contract.md) | Accepted |
| 081 | [Component handle refs](./rfc-081-component-handle-refs.md) | Implemented |
| 082 | [Storage-agnostic file/object storage](./rfc-082-pocopine-storage.md) | Accepted |
| 084 | [Typed slot props](./rfc-084-typed-slot-props.md) | Accepted |
| 086 | [`pocopine-sync-query`](./rfc-086-sync-query.md) | Implemented |
| 087 | [`pocopine-sync-query` driver lifecycle](./rfc-087-sync-query-driver.md) | Implemented |
| 088 | [`pocopine-sync-query` production parity](./rfc-088-sync-query-production-parity.md) | Implemented |
| 089 | [SPA router parity and nested outlets](./rfc-089-spa-router-parity.md) | Accepted |
| 090 | [Merge `pocopine-sync-crud` into `pocopine-sync-query`](./rfc-090-merge-crud-into-query.md) | Implemented |
| 092 | [Pine Stylekit utility compiler](./rfc-092-pocopine-stylekit.md) | Accepted |
| 093 | [Pocopine Agenkit plan](./rfc-093-pocopine-agenkit.md) | Draft |
| 112 | [Dynamic component rendering (`<pp-component :is>`)](./rfc-112-dynamic-component.md) | Implemented |
| 113 | [Typed node views and external blocks for `pine-richtext`](./rfc-113-pine-richtext-typed-node-views.md) | Draft |
