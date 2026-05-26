# Pocopine sync — gap analysis against the local-first field (2026-05-26)

**Scope:** Identify what pocopine-sync needs to ship to deliver "solid local-first sync with no compromises", with a secondary constraint that the wire/storage contract remain shaped so an eventual P2P layer drops in cleanly. The roadmap (`docs/sync-local-first-roadmap.md`) only lists three remaining items (SQLx examples, browser CI, changefeed adapters); this report argues the roadmap is materially undercounting.

**Method:** Cross-walked pocopine's current crate surface against the published designs of Replicache, ElectricSQL, PowerSync, Linear, InstantDB, Jazz, Automerge, Loro, CR-SQLite, Yjs, TanStack DB, and the original Ink & Switch local-first essay. Each axis below cites the specific contract those systems expose, then names what's missing in pocopine.

---

## Axis 1: Schema migrations (versioned payloads, queued-mutation drift)

- **Status in pocopine:** Missing.
- **Field evidence:**
  - Linear's reverse-engineering notes ([wzhudev/reverse-linear-sync-engine](https://github.com/wzhudev/reverse-linear-sync-engine)) explicitly call this out: "Migrations trigger via schema hash comparisons stored in metadata. When model definitions change, the hash updates, prompting `IndexedDB.open()` with an incremented version number."
  - CR-SQLite has a first-class API ([vlcn.io migrations](https://vlcn.io/docs/cr-sqlite/migrations)): "tables that have been upgraded to crrs need some extra handling … start that modification with a call to `crsql_begin_alter`, and complete with `crsql_commit_alter`. Bookkeeping metadata needs to be migrated as well."
  - Automerge and Loro both publish "save format" / on-disk schema versions because they have to migrate every doc ever written.
- **Gap:** `ClientMutation<M>` carries a serde `Value` payload with no schema version. A user updates `#[resource]` to add a non-default field; queued mutations on a stale device will deserialize-fail on the server, with no graceful path. Cached rows in OPFS/IndexedDB face the same issue on the read side.
- **Minimum surface:**
  - `schema_version: u32` on `SyncCollectionName` registration; server includes it in `SyncOpenResponse`.
  - Client compares versions on open; on mismatch, server either runs a registered `migrate_payload(old_version, value) -> value` for queued mutations, or returns `SyncError::SchemaMigration { from, to }` so the client can drop the queue with a documented surface.
  - `SyncLocalStore::current_schema_version(stream)` for hydration-time validation; row cache is purged on bump.
- **Priority:** **P1.** First production user who ships a `cargo run` ALTER will lose data. There is no workaround at the framework level — the type sig itself doesn't carry version.

## Axis 2: Partial sync / shape subscriptions

- **Status in pocopine:** Partial (server-defined opaque streams; no client-driven shape).
- **Field evidence:**
  - ElectricSQL ([electric.ax/docs/guides/shapes](https://electric.ax/docs/guides/shapes)): "Shapes consist of … Table, Where Clause (optional): filters rows using PostgreSQL expressions, Columns (optional): specifies which fields to include in sync." Shapes are explicitly the unit of subscription.
  - PowerSync's [Sync Rules](https://docs.powersync.com/usage/sync-rules) parameterise buckets by JWT claims, client params, or DB values: `SELECT * FROM lists WHERE owner_id = bucket.user_id`. Buckets exist per user.
  - Triplit (now defunct, domain parked at [sedo.com](https://sedo.com/search/details/?domain=triplit.dev)) used query subscriptions; InstantDB ([instantdb.com/docs/instaql](https://instantdb.com/docs/instaql)) does the same with `where`, `$gt`, `$in` etc.
- **Gap:** `SyncStreamName` is opaque and a stream's filter is hard-coded server-side via `Source::filter`. To express "issues assigned to me" or "messages in channel X" you must mint a `SyncStreamName` per tenant/filter combination, blowing up the stream registry and making fanout-by-broadcast inefficient.
- **Minimum surface:**
  - `SyncStreamName` becomes `{ name, params: Map<String, Value> }`. The server-registered `Source` gets `(ctx, params)` so it can compose its filter.
  - Define a small whitelist of comparators on registered fields (eq/in/range/contains) — no arbitrary SQL surface (opinionated-by-default).
  - Live invalidation cross-checks `params` so server-pushed changes only wake subscribed clients.
- **Priority:** **P1.** Without this, every multi-tenant app uses per-user stream registries — a scaling cliff Linear documents explicitly.

## Axis 3: Streaming pulls / chunked snapshots

- **Status in pocopine:** Missing.
- **Field evidence:**
  - ElectricSQL's HTTP shape stream is chunked by design — `requestSnapshot()` paginates and supports `changes_only` to skip snapshots entirely ([electric.ax shapes](https://electric.ax/docs/guides/shapes)).
  - PowerSync's bucket protocol ([powersync-protocol](https://docs.powersync.com/architecture/powersync-protocol)) streams checkpoints; clients can stop and resume mid-snapshot.
- **Gap:** `SyncPullResponse` carries `rows: Vec<SyncRow<T>>` in one allocation. A 100k-row initial sync blows wasm heap, holds a single response open against backpressure, and forces server-side full materialisation.
- **Minimum surface:**
  - Add `SyncPullChunkRequest { stream, cursor, limit }` returning `{ rows, next_cursor, more }`. Snapshot mode becomes a loop of chunks.
  - `CollectionState::begin_pull_chunked` token that survives across requests; rows accumulate into `canonical_rows` incrementally, conflict/pending overlay is applied lazily.
  - SSE streaming as an optional transport (server framework already has it for live).
- **Priority:** **P2.** Survivable until first user with >50k rows in a stream; then it's a hard wall.

## Axis 4: Compaction / log garbage collection

- **Status in pocopine:** Missing.
- **Field evidence:**
  - Linear ([wzhudev/reverse-linear-sync-engine](https://github.com/wzhudev/reverse-linear-sync-engine)): "Persisted transactions are cached in IndexedDB's `_transaction` table and removed after server acknowledgment and corresponding sync action arrival."
  - Yjs ([INTERNALS.md](https://github.com/yjs/yjs/blob/main/INTERNALS.md)): "When garbage collection is enabled … content is replaced with a `GC` object that only stores the length of removed content."
  - Loro ([loro.dev](https://loro.dev/)) ships history compression as a headline feature; 360k ops → 361KB stored.
  - Replicache ([server-pull](https://doc.replicache.dev/reference/server-pull)) requires servers track `lastMutationID` per client so the protocol can discard accepted mutations on next pull.
- **Gap:** `__pocopine_mutations` keeps every accepted/rejected mutation row forever. There is no documented purge horizon, no `compact()` API, no `mark_purge_safe_below(mutation_id)`. After a year of use the table is the database.
- **Minimum surface:**
  - `MemoryCrudMutationLog::purge_below(client_id, mutation_id)` + same on `SqlxCrudMutationLog`.
  - Server returns `safe_purge_below: Option<MutationId>` on `SyncPushResponse`; client purges queue and store.
  - Server-side adapter (per stream) for "all clients that opened in last N days have acked through mid M" — admin-tunable horizon for the case where a device is offline forever.
- **Priority:** **P1.** Storage growth is unbounded and silent; this hits every deployment but at different speeds.

## Axis 5: Causal consistency across devices

- **Status in pocopine:** Partial.
- **Field evidence:**
  - Triplit relied on HLCs ([Hybrid Logical Clocks, Sergei Turukin](https://sergeiturukin.com/2017/06/26/hybrid-logical-clocks.html)) for cross-device ordering.
  - Yjs uses (clientId, clock) tuples — a Lamport-style vector ([yjs INTERNALS](https://github.com/yjs/yjs/blob/main/INTERNALS.md)): "A state vector defines the known state of each client (a set of tuples of client and clock)."
  - Linear takes the opposite tack and serialises everything through a server-assigned `syncId` ([wzhudev](https://github.com/wzhudev/reverse-linear-sync-engine)): "all transactions sent by clients follow a total order … this total order is represented by the sync id, which is an incremental integer."
- **Gap:** Within one device, `MutationId = device_id:counter` preserves causality. Across devices, the server is the only ordering authority — but the server has no causal metadata in `ClientMutation`, only arrival order. Concurrent edits on two devices A and B that depend on the same prior state can interleave non-causally if device A's mutations arrive late.
- **Minimum surface:**
  - Add `base_lamport: u64` to `ClientMutation` (already half-present as `base_version` per row — generalise to whole-stream Lamport).
  - Server rebases by `base_lamport`, not arrival; emits new Lamport on accept and stamps it into the row version.
  - On reload, client replays its queue in `base_lamport` order against fresh canonical state — the existing rebase already does this for `base_version`; just lift to stream scope.
- **Priority:** **P1 for the P2P-shaped-contract goal, P2 for the immediate solid-sync goal.** The mutation-ID format is already P2P-ready (counter-based per device, per [user feedback](feedback_no_app_secret_storage.md)); without a stream Lamport, P2P merge has no anchor.

## Axis 6: Multi-stream / multi-row transactional consistency

- **Status in pocopine:** Missing.
- **Field evidence:**
  - Replicache ([how it works](https://doc.replicache.dev/concepts/how-it-works)): "When you invoke a mutator, Replicache applies changes locally and creates a pending mutation record … mutators contain the resolution logic." A single `transfer({from, to, amount})` mutator can hit two rows atomically.
  - Linear ([fujimon blog](https://www.fujimon.com/blog/linear-sync-engine)): the transaction queue is the unit; "transactions created within the same event loop will share the same batchIndex."
  - InstantDB ([instaql](https://instantdb.com/docs/instaql)) commits multi-entity transactions atomically.
- **Gap:** `pocopine-sync-crud` is row-scoped: `save`, `delete`, `create` each take one row. A "transfer", "move issue to project + reorder", or "create issue and assign in one click" can't be expressed atomically — they split into two mutations, two server round-trips, two retry domains.
- **Minimum surface:**
  - `ClientMutation<M>` already wraps a typed payload — extend `M` to be an enum of typed mutators (Replicache's named-mutators pattern), and let a mutator return `Vec<(SyncCollectionName, RowKey, SyncOp)>`.
  - `CollectionState::apply_optimistic_transaction(mutations: Vec<…>)` — single atomic optimistic apply across collections.
  - Server `Source::apply_transaction(ctx, mutations) -> Result<…>` so the database transaction wraps all rows.
- **Priority:** **P1.** Without this, you cannot model the issue-tracker / banking / chat-with-channel-membership use case truthfully; you ship sync with a "row at a time only" footnote.

## Axis 7: Network resilience (backoff, queue depth, offline limits)

- **Status in pocopine:** Missing (explicit policy; some retry behaviour incidental).
- **Field evidence:**
  - AWS Architecture Blog [Exponential Backoff and Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/): "Adding jitter avoids alignment of requests into spikes; backoff diminishes load escalation."
  - Replicache ([server-push](https://doc.replicache.dev/reference/server-push)) specifies HTTP error semantics: 401 → re-auth, other → exponential backoff.
- **Gap:** `SyncClient` does not publish a backoff policy. There's no documented queue-depth cap, no "offline > 30 days → reset" rule, no per-error retry classification (5xx vs 4xx vs timeout). An app under spotty network will hammer the server or stall silently.
- **Minimum surface:**
  - `SyncClient::with_retry_policy(RetryPolicy { initial, max, multiplier, jitter, max_attempts })`. Default: 250ms → 30s, full jitter, 7 attempts.
  - `max_queue_depth: usize` per stream; `SyncError::QueueFull` surfaces to app.
  - `max_offline_age: Duration` — exceeded → next pull triggers `Gap` and full resnapshot.
- **Priority:** **P2.** Not load-bearing for correctness but load-bearing for production credibility.

## Axis 8: Tombstones / soft delete vs hard delete

- **Status in pocopine:** Partial (`SyncOp::Delete` exists; persistence/TTL semantics unspecified).
- **Field evidence:**
  - CR-SQLite ([HN discussion](https://news.ycombinator.com/item?id=33606311), [GitHub README](https://github.com/vlcn-io/cr-sqlite)): "Causal length (cl) indicates whether the row is still present or was deleted. CL-Set is similar to OR-Set but supports 'un-delete' via a counter."
  - Yjs garbage-collects tombstones aggressively (see Axis 4).
- **Gap:** If client C opens with a cursor older than the most recent delete, but the server has already compacted that delete, C will keep the row indefinitely. There's no tombstone TTL contract, no "if cursor older than compaction horizon, force resnapshot" rule (this is Axis 16 again, from the other direction).
- **Minimum surface:**
  - Server stores `deleted_at` for each row and serves `SyncOp::Delete` until `deleted_at + tombstone_ttl`; afterwards it's compacted out and a stale-cursor client gets `SyncError::Gap`.
  - `tombstone_ttl` (per-stream) tunable; default 30 days.
- **Priority:** **P2.** Mostly handled if Axis 16 (gap + resnapshot) is solid, but worth specifying explicitly so app authors can reason about delete propagation guarantees.

## Axis 9: Observability / introspection

- **Status in pocopine:** Missing.
- **Field evidence:**
  - Replicache exposes `inspectorDelegate` and a debug UI showing pending mutations, last cookie, conflict state ([replicache docs](https://doc.replicache.dev/)).
  - TanStack DB ([mutations docs](https://tanstack.com/db/latest/docs/guides/mutations)) makes the transaction lifecycle states (`pending` → `persisting` → `completed` / `failed`) first-class queryable surface.
- **Gap:** Nothing in pocopine answers "why isn't this row showing up?" beyond raw tracing logs. There's no `SyncDebugInfo { stream, cursor, pending: Vec<…>, canonical: usize, conflicts: Vec<…>, last_error: Option<…> }` you can subscribe to from app code.
- **Minimum surface:**
  - `SyncClient::debug_snapshot(stream) -> SyncDebugInfo`. Per-collection counters (pending, accepted, rejected, conflicts) wired into `tracing::info!(target: "pocopine.metric", …)` per the [logging-library rule](feedback_use_logging_library.md).
  - A first-party `pocopine_sync_devtools` crate (opt-in) that exposes a Pine view of the debug snapshot — eats own dogfood.
- **Priority:** **P2.** Not load-bearing but it's the difference between "production-grade" and "I have to print-debug serde."

## Axis 10: Storage quota / eviction

- **Status in pocopine:** Missing.
- **Field evidence:**
  - [MDN Storage quotas](https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria): Firefox best-effort = 10% of disk or 10 GiB; Chrome = 60% of disk; LRU eviction; `navigator.storage.persist()` opts into protected storage; throws `QuotaExceededError` when full.
  - [RxDB on IndexedDB limits](https://rxdb.info/articles/indexeddb-max-storage-limit.html): apps must monitor `navigator.storage.estimate()`.
- **Gap:** `SyncLocalStore` does not surface `estimate()`, doesn't call `navigator.storage.persist()` on init, doesn't have an eviction strategy if writes start throwing `QuotaExceededError`. A user with a full disk silently loses optimistic state.
- **Minimum surface:**
  - On `SyncClient::start()` in wasm: call `navigator.storage.persist()`, log result. Document the prompt UX.
  - `SyncLocalStore::usage() -> Option<StorageUsage { used, quota }>` (wasm only — implementation calls `estimate()`).
  - On `QuotaExceededError` during a snapshot save, fall through to in-memory only with `SyncError::StorageFull`; app gets a chance to drop optional streams.
- **Priority:** **P2.** Edge case for most, fatal for power users storing media offline.

## Axis 11: Optimistic UI lifecycle

- **Status in pocopine:** Partial (`pending`, `conflict` flags on `SyncRow`).
- **Field evidence:**
  - TanStack DB ([deepwiki optimistic updates](https://deepwiki.com/TanStack/db/5.2-optimistic-updates)): "lifecycle consists of five phases: optimistic application … handler invocation … backend persistence … sync confirmation … optimistic cleanup."
  - Replicache distinguishes "speculative (local) and canonical (server) results."
- **Gap:** Pocopine has `pending` and `conflict` but no `optimistic_deleted` (hidden but not yet confirmed), no `prior_value` for diff-rendering edits, no per-mutation lifecycle state observable from view code (`pending` is per-row, not per-mutation).
- **Minimum surface:**
  - `SyncRow<T> { value, prior_value: Option<T>, pending, optimistic_deleted, conflict }`.
  - `SyncClient::mutation_state(id) -> Option<MutationState { Pending, Persisting, Completed, Failed }>` (Tanstack-style).
- **Priority:** **P2.** Apps can ship with current flags but rich-diff UI ("show what's about to change") requires this. Bundled with Axis 9.

## Axis 12: Bulk operations

- **Status in pocopine:** Missing.
- **Field evidence:** Replicache push ([server-push](https://doc.replicache.dev/reference/server-push)) is inherently batched: "mutations can be applied together." PowerSync ([sync rules](https://docs.powersync.com/usage/sync-rules)) batches by bucket. Linear batches per `batchIndex`.
- **Gap:** `CrudSource` exposes `create`, `save`, `delete` row-by-row. Importing 1000 rows = 1000 mutations, 1000 server invocations.
- **Minimum surface:**
  - `create_many<T>(Vec<T>)`, `save_many<T>(Vec<T>)`, `delete_many(Vec<RowKey>)` generated by the `#[resource]` macro.
  - Single `ClientMutation` carries `payload: Vec<…>` (or many `ClientMutation`s in a single push request — easier change, no breaking).
- **Priority:** **P2.** Closely tied to Axis 6 (transactions). If transactions land, batches are a degenerate single-collection case.

## Axis 13: Server-driven invalidation / reconnect

- **Status in pocopine:** Partial (server-side stream registry + live wake-up; SSE reconnect policy unspecified).
- **Field evidence:**
  - PowerSync ([protocol](https://docs.powersync.com/architecture/powersync-protocol)) uses checkpoint sequence IDs so a reconnecting client can resume mid-stream.
  - Replicache long-polls / pokes the pull endpoint.
- **Gap:** SSE drop handling — reconnect with exponential backoff (Axis 7), resume cursor (Axis 16). Cross-tab live coordination is half-done (sign_out cascade exists; routine updates don't broadcast).
- **Minimum surface:** Roll into Axes 7 + 15 + 16; no standalone surface needed.
- **Priority:** **P2** as a composite.

## Axis 14: Authentication token refresh

- **Status in pocopine:** Missing.
- **Field evidence:** Replicache spec ([server-push](https://doc.replicache.dev/reference/server-push)): "401 → triggers re-authentication." Most engines surface a `getAuth()` callback that re-fetches the token.
- **Gap:** When a JWT expires mid-session, `SyncError::Unauthorized` propagates but there's no hook for "refresh and retry once." App has to tear down the SyncClient and rebuild.
- **Minimum surface:**
  - `SyncClient::with_auth_refresher(impl Fn() -> Future<Result<String>>)`. On `Unauthorized`, refresher is invoked once; on success, request is retried; on failure, error bubbles up.
- **Priority:** **P1.** This is a half-day's work and breaks every long-session app today.

## Axis 15: Cross-tab coordination

- **Status in pocopine:** Partial (`BroadcastChannel` cascade exists for sign_out).
- **Field evidence:** Linear shares a single IndexedDB across tabs; Jazz coordinates via leader-election. The MDN [Storage quotas page](https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria) notes IDB/OPFS access from multiple tabs.
- **Gap:** Tab A's push doesn't notify Tab B locally — Tab B only sees the change after Tab A's mutation round-trips and Tab B re-pulls. With OPFS shared SQLite, both tabs are writing to the same store but each has its own `CollectionState` in memory.
- **Minimum surface:**
  - Leader-election (`Web Locks API`) for who owns the SSE connection.
  - On any mutation commit to local store, emit `pocopine.sync.local-write` on `BroadcastChannel`; non-leader tabs invalidate their `CollectionState` and re-hydrate from store.
- **Priority:** **P2.** Already covered by sign_out cascade for correctness-critical events; this is UX polish.

## Axis 16: Sync session resume (cursor gap handling)

- **Status in pocopine:** Partial (`SyncError::Gap` exists; resnapshot path documented?).
- **Field evidence:** Replicache ([server-pull](https://doc.replicache.dev/reference/server-pull)): "When a cookie is problematic or unusable, return all data. This is done by first sending a `clear` op followed by multiple `put` ops." PowerSync checkpoints expire — clients re-bootstrap.
- **Gap:** `SyncError::Gap` exists but I cannot find evidence in the codebase that `SyncClient` automatically transitions to `begin_initial` and resnapshots on `Gap`. If it requires app-level handling, that's an undocumented sharp edge.
- **Minimum surface:**
  - On `Gap`: `CollectionState` self-issues a `begin_initial`, replays queued mutations on top of fresh snapshot. App sees this as a single transient `Syncing` state, not a hard failure.
  - `SyncError::Gap` carries `last_known_cursor` and `server_horizon_cursor` for diagnostics.
- **Priority:** **P1.** Pairs with Axis 4 (compaction): once compaction exists, gaps will happen routinely and the resume path must be invisible.

## Axis 17: Mutation idempotency at the application layer

- **Status in pocopine:** Partial (mutation_ids reserved; side-effect contract unspecified).
- **Field evidence:** Replicache ([server-push](https://doc.replicache.dev/reference/server-push)): "Even for invalid mutations, the server must still mark the mutation as processed by updating `lastMutationID`, otherwise clients become permanently blocked retrying."
- **Gap:** Pocopine's `CrudMutationLog::reserve_mutation_id` gives at-most-once semantics for the *row write*. But a `send_email(invoice_id)` side effect attached to that mutation must check "have I already done this for this mutation_id?" — there's no example, no helper. App authors will get this wrong.
- **Minimum surface:**
  - Document the contract: side-effecting handlers must look up `mutation_id` in their own log before firing, and must commit "side-effect done" in the same DB transaction as the row write.
  - Helper: `CrudMutationLog::has_processed(mutation_id) -> bool` (already present logically; expose it).
  - Cookbook page with the canonical "send email exactly once" pattern.
- **Priority:** **P2.** Mostly a docs gap; the primitives exist.

## Axis 18: Per-row vs per-document state (parent/children)

- **Status in pocopine:** Missing.
- **Field evidence:** Linear's data model is issue + comments + reactions; the sync engine treats them as separate streams but UI composes them. Automerge represents a document as a single tree; mutations on children are part of the same op log ([automerge concepts](https://automerge.org/docs)).
- **Gap:** `CollectionState<T>` is per-row of one collection. A "document = issue + comments" view requires the app to multiplex two `LocalResourceView`s and reason about both pending queues. There's no `joined_view` primitive.
- **Minimum surface:**
  - `LocalResourceView::join_by_key(other, fk_extractor)` — reactive joined view, conflict/pending flags propagate.
  - Or: design pattern doc + helper macro for the common "parent + child list" case. (Opinionated default beats general feature.)
- **Priority:** **P3.** Workable today via composition; ship when transactions (Axis 6) and shapes (Axis 2) make the joined surface obvious.

## Axis 19: Schema-typed payloads end-to-end

- **Status in pocopine:** Partial (typed at API; serde Value internally).
- **Field evidence:** InstantDB schemas, Triplit schemas, Jazz CoValue schemas are all typed end-to-end and validate at parse time. Yjs is the outlier (untyped by design).
- **Gap:** `ClientMutation<M>` is generic but the wire/store representation goes through `serde_json::Value`. A field rename on the server with no client redeploy will serialize-succeed and apply garbage.
- **Minimum surface:**
  - Pairs with Axis 1 (schema version). The version IS the type-drift detector if it's bumped on field renames.
  - Optional: registered schema-hash check (server compares `M::schema_hash()` returned by macro) — reject mismatches with a typed error before parse, not after.
- **Priority:** **P2** (subsumed by Axis 1 if done right).

## Axis 20: P2P-readiness checklist

- **Status in pocopine:** Aware, partially shaped.
- **Field evidence:**
  - The CRDT taxonomy paper ([Delta State Replicated Data Types, Almeida et al.](https://arxiv.org/pdf/1603.01529)): "Delta-state CRDTs aim to achieve the best of both worlds … communicate only the changed state."
  - Yjs and Loro both publish their over-the-wire format AS the P2P format.
  - The user's own [project_direction](project_direction.md) notes: "MutationId format: `<device_id>:<counter>` — counter is what all P2P-ready engines use."
- **Gap (relative to "easy to build P2P on top"):**
  - **(a) No stream Lamport** (see Axis 5). P2P merge needs a total-ordering anchor independent of the server.
  - **(b) Mutator payload is opaque JSON, not deterministic CRDT ops.** A peer can't apply another peer's mutation without re-running the server function. Replicache's "mutator name + args" model is closer to P2P-friendly because the mutator code is replicable.
  - **(c) No `state_vector()` / `diff_since(vec)` API** on `CollectionState`. Yjs's `encodeStateAsUpdate(otherStateVector)` is exactly the anti-entropy primitive P2P needs.
  - **(d) No conflict resolution that's deterministic across peers.** Currently the server arbitrates conflicts; P2P has to do it without a server. Either ship per-field LWW (timestamps), or per-row LWW + a per-row "merge function" hook.
- **Minimum surface (for contract-shape only, not P2P itself):**
  - `CollectionState::state_vector() -> StateVector` and `diff_since(StateVector) -> Vec<SyncOp>` — even if only used by client/server today, the shape forces the abstraction.
  - Bump `MutationId` carrier to include `lamport` (the Axis-5 work).
  - Document a "named mutator" pattern (Axis 6 transactions) so the app's mutation set is enumerable and re-applicable.
  - Per-field LWW timestamps as an opt-in `#[resource]` attribute (`#[lww(ts = updated_at)]`) — server already does LWW implicitly; lift to client.
- **Priority:** **P2 for contract-shape work** (it shapes Axes 1, 5, 6); **P3 for actual P2P engine.** The user explicitly framed P2P as goal-shaping, not goal-replacing.

---

## Synthesis: top 5 gaps to ship next

Rank-ordered for "solid local-first sync with no compromises." Effort estimates are calendar-time on the current single-author cadence (small days = <1d, medium = 2–4d, large = 1–2w).

1. **Schema versioning + migration adapter (Axis 1).** Effort: medium. This is the single feature whose absence makes a v1.0 commitment dishonest: any non-trivial app will hit it within a quarter, and there is no graceful failure mode today. Add `schema_version` to `SyncCollectionName` registration + `SyncOpenResponse`, surface `SyncError::SchemaMigration { from, to }`, document the "drop queue" and "register migrator" paths.

2. **Mutation-log compaction + Gap-resume coupling (Axes 4 + 16).** Effort: medium. Ship `purge_below` on `CrudMutationLog`, tombstone TTL on the source, and auto-resnapshot inside `CollectionState` on `SyncError::Gap`. These three are one feature because compaction is what makes Gaps happen, and Gap-resume is what makes compaction safe. Without this loop closed, storage grows forever and any deployment of compaction breaks long-offline clients.

3. **Shape subscriptions (Axis 2).** Effort: medium–large. Extend `SyncStreamName` with bounded typed `params`, plumb through `Source::filter(ctx, params)`, and gate live invalidation on params. This unlocks the multi-tenant case without the stream-registry explosion that every other engine has had to redesign around (Linear is the cautionary tale). Keep the comparator vocabulary tight — eq, in, range, contains — to stay opinionated.

4. **Multi-row transactional mutators (Axis 6).** Effort: large. Lift `ClientMutation<M>` from "one row + one op" to "named mutator + typed args + Vec<row-op>" Replicache-style. `CollectionState::apply_optimistic_transaction`, server `Source::apply_transaction` in one DB tx. This is the gateway to honest models for issue trackers, finance apps, and anything with referential integrity. It also drags Axis 12 (bulk ops) and Axis 17 (side-effect idempotency examples) along for free, and is the foundation Axis 20's "named mutators" P2P-shape relies on.

5. **Auth refresh hook + retry policy (Axes 14 + 7).** Effort: small–medium. `SyncClient::with_auth_refresher` and `with_retry_policy(RetryPolicy { initial, max, multiplier, jitter, max_attempts })`. Both are single-day pieces individually; bundled, they take pocopine from "works in a happy demo" to "survives a flaky cafe wifi + JWT-rotating production server." Defaults: 250ms→30s full-jitter, 7 attempts, one auto-refresh on 401.

**What's deliberately not in the top 5:** observability (P2, ship after the above are stable so the dashboard reflects real state), cross-tab live propagation (P2, current sign_out cascade is enough for correctness), chunked snapshots (P2, no user has hit the wall yet), shape of P2P contract (P2/P3, the work in #1, #3, #4 already pulls the contract in the right direction — see Axis 20).

**Net effect when these five ship:** pocopine moves from "works for a single-tenant todo app with row-scoped writes and forever-growing logs" to a sync engine that survives schema evolution, multi-tenancy, long-offline clients, atomic multi-row writes, and production network conditions — i.e. the lower bar of what users mean when they ask for "no compromises." The remaining items (10–13, 15, 17–19) are polish layers on a sound contract, not contract changes; they can ship one at a time without breaking apps.

---

## Sources

- [Replicache - How It Works](https://doc.replicache.dev/concepts/how-it-works)
- [Replicache - Server Pull](https://doc.replicache.dev/reference/server-pull)
- [Replicache - Server Push](https://doc.replicache.dev/reference/server-push)
- [ElectricSQL - Shapes](https://electric.ax/docs/guides/shapes)
- [PowerSync - Protocol](https://docs.powersync.com/architecture/powersync-protocol)
- [PowerSync - Sync Rules](https://docs.powersync.com/usage/sync-rules)
- [Linear - Scaling the Sync Engine (announcement)](https://linear.app/blog/scaling-the-linear-sync-engine)
- [wzhudev - Reverse Linear Sync Engine](https://github.com/wzhudev/reverse-linear-sync-engine)
- [fujimon - Linear Sync Engine](https://www.fujimon.com/blog/linear-sync-engine)
- [Local-First Software - Ink & Switch](https://www.inkandswitch.com/essay/local-first/)
- [InstantDB - InstaQL](https://instantdb.com/docs/instaql)
- [Jazz - Database](https://www.jazz.tools/docs)
- [Automerge - Concepts](https://automerge.org/docs)
- [Loro - Home](https://loro.dev/)
- [Loro - CRDT algorithms (DeepWiki)](https://deepwiki.com/loro-dev/loro/6.1-crdt-algorithms)
- [Yjs - INTERNALS.md](https://github.com/yjs/yjs/blob/main/INTERNALS.md)
- [CR-SQLite - Migrations](https://vlcn.io/docs/cr-sqlite/migrations)
- [CR-SQLite - GitHub README](https://github.com/vlcn-io/cr-sqlite)
- [TanStack DB - Mutations](https://tanstack.com/db/latest/docs/guides/mutations)
- [TanStack DB - Optimistic Updates (DeepWiki)](https://deepwiki.com/TanStack/db/5.2-optimistic-updates)
- [MDN - Storage Quotas and Eviction](https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria)
- [RxDB - IndexedDB Max Storage Limit](https://rxdb.info/articles/indexeddb-max-storage-limit.html)
- [AWS Architecture Blog - Exponential Backoff and Jitter](https://aws.amazon.com/blogs/architecture/exponential-backoff-and-jitter/)
- [Hybrid Logical Clocks - Sergei Turukin](https://sergeiturukin.com/2017/06/26/hybrid-logical-clocks.html)
- [Delta State Replicated Data Types - Almeida et al., arXiv:1603.01529](https://arxiv.org/pdf/1603.01529)
- [CRDT Survey, Part 3 - Matthew Weidner](https://mattweidner.com/2023/09/26/crdt-survey-3.html)
- [LocalFirst.fm #15 - Tuomas Artman](https://www.localfirst.fm/15)
