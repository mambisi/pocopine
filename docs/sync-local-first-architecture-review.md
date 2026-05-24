# Local-First Sync Architecture Review

This document records the architecture check behind the next Pocopine
local-first sync phases. It is written for framework authors reviewing
whether the current direction is sound and what the next implementation
slices should prove.

Pocopine should remain a database-agnostic local-first sync engine, not a
sync database product. The framework owns protocol shape, local
durability, optimistic state, rebase, conflict metadata, live wake-up
integration, and resource ergonomics. Apps and adapter crates own
database queries, schemas, authorization filters, domain validation, and
backend-specific change tracking.

## Reference Architectures

These systems validate the current Pocopine direction, but none should be
copied wholesale.

### Replicache

Replicache is the closest match for the low-level Pocopine model. Its
client applies mutations optimistically, persists pending work, pulls
canonical server state, and rebases local mutations over the latest
server state. Its server "poke" is a wake-up signal that tells clients to
pull; it is not the row transport itself.

Pocopine should keep:

- canonical server state separate from rendered optimistic state,
- deterministic replay of pending mutations,
- mutation ids as idempotency keys,
- live wake-up as "pull this stream", not "here are rows".

Pocopine should avoid:

- coupling the app model to one hosted sync service,
- hiding backend authorization or database writes inside the sync layer.

Reference: <https://doc.replicache.dev/concepts/how-it-works>

### Zero

Zero uses local mutators, server-side mutators, optimistic state, and
server reconciliation. Its useful lesson is that custom business logic,
authorization, and partial sync have to be first-class design concerns.

Pocopine should keep:

- generated resource ergonomics on top of explicit sync contracts,
- server-side transaction boundaries for writes,
- app-owned business logic instead of generated SQL.

Pocopine should avoid:

- making the sync crate own the app's data model,
- treating a queued local write as server success.

References: <https://zero.rocicorp.dev/docs/sync>,
<https://zero.rocicorp.dev/docs/mutators>

### PowerSync

PowerSync validates the durable local queue side of the architecture. The
client writes locally, records local changes into an upload queue, keeps
working offline, and later routes uploads through the app backend.

Pocopine should keep:

- durable enqueue before reporting a local-first write as queued,
- replay after reconnect,
- app backend ownership of validation and side effects,
- explicit handling for validation failures and conflicts.

Pocopine should avoid:

- making browser SQLite the public application database API,
- turning the framework into a managed sync database.

References: <https://docs.powersync.com/architecture/client-architecture>,
<https://docs.powersync.com/architecture/consistency>

### Electric

Electric's current Postgres sync product is primarily a read-sync and
shape system. The older ElectricSQL generation also emphasized routing
writes through the application's backend/API. The lesson Pocopine should
carry forward is the shape/read boundary, not any one Electric write
model.

Pocopine should keep:

- opaque cursors,
- backend-specific adapters outside the core,
- room for partial resource subscriptions later.

Pocopine should avoid:

- making Postgres or shape streams a core assumption,
- requiring database-specific changefeeds before the resource runtime is
  ergonomic.

References: <https://electric-sql.com/product/sync>,
<https://electric.ax/products/postgres-sync>

### CouchDB

CouchDB's replication model shows the cost of multi-master document
sync: conflicts are normal data and applications must resolve them.

Pocopine should keep:

- conflicts visible and explicit,
- rejected writes separate from stale-version conflicts,
- deterministic rollback/rebase behavior.

Pocopine should avoid:

- silent last-write-wins for normal CRUD rows,
- automatic merging without a resource-specific policy.

Reference: <https://docs.couchdb.org/en/stable/replication/conflicts.html>

### Firestore

Firestore validates the value of transparent local cache and latency
compensation, but it resolves multiple writes to the same document by
last write wins.

Pocopine should keep:

- local reads before network freshness,
- UI updates from cached data,
- simple installation for common apps.

Pocopine should avoid:

- last-write-wins as the default conflict policy,
- hiding offline/server status from author-facing APIs.

Reference: <https://firebase.google.com/docs/firestore/manage-data/enable-offline>

### Yjs And Automerge

Yjs and Automerge validate CRDT-backed collaborative documents. Their
updates are designed to merge concurrent edits automatically.

Pocopine should keep:

- CRDT fields as a separate collaboration layer,
- explicit boundaries between row sync and document collaboration.

Pocopine should avoid:

- treating ordinary business rows as CRDT documents by default,
- using automatic merge semantics for payments, inventory, or tenant
  scoped CRUD without an app-approved policy.

References: <https://docs.yjs.dev/api/document-updates>,
<https://automerge.org/>

## Architecture Decision

The current Pocopine architecture is sound if we keep these invariants:

1. Canonical rows remain separate from rendered optimistic rows.
2. Rendered rows are canonical rows plus pending local overlay.
3. Pending mutations replay deterministically in queue order.
4. Mutation ids are durable idempotency keys.
5. A local-first write reports success only after it is durably queued.
6. Local-first CRUD creates use app-visible client-generated ids as final
   resource ids. The server must echo that id in the canonical row. Apps
   that require server-generated ids should use an online-only/custom
   create flow instead of the offline-capable generated create path.
7. Rejected writes remove the pending overlay and rebase from canonical
   rows. Conflicted writes keep user-visible data available and marked
   conflicted.
8. Server writes remain app-owned and authorization-aware.
9. Conflicts stay visible; ordinary CRUD does not silently merge.
10. Live wake-up invalidates streams and triggers pulls; it does not move
   row data.
11. SQLite/IndexedDB/OPFS stores are sync caches, not the app's database
   abstraction.
12. CRDT collaboration remains separate from default CRUD row sync.

The main gap after the current foundation is not protocol shape. It is
the resource runtime that turns these invariants into the API authors use
every day.

## Reviewable Implementation Phases

Each phase below should be one logical commit. Each commit should receive
a Claude Code CLI review before the next phase begins. Findings should be
fixed in the same phase commit until the review has no blockers. After
all phases pass, run a final full-branch Claude review before opening the
PR.

### Phase 1: Architecture And Phase Doc

Goal: make the design and review path explicit.

Deliverables:

- this architecture review document,
- explicit reference-system comparison,
- phase list tied to commit boundaries,
- criteria for what must stay out of the core.

Acceptance:

- the doc explains why the next slice is a resource runtime, not a
  database adapter,
- the doc states the durable enqueue boundary,
- the doc separates CRUD conflict handling from CRDT collaboration.

### Phase 2: Non-Macro CRUD Client Runtime

Goal: add the runtime contract that future generated resource modules can
target without proc-macro complexity.

Deliverables:

- a typed `CrudClientResource` or equivalent handle in
  `pocopine-sync-crud`,
- `create`, `save`, and `remove` methods,
- `_with_options` or fluent options methods that use the existing
  `CreateOptions`, `SaveOptions`, and `RemoveOptions`,
- a lower-level async sync hook that reserves the mutation id, enqueues
  the pending mutation, applies optimistic state, and returns the
  reserved id only after the durable enqueue succeeds,
- `QueueOffline` mapped to durable generated-id queueing,
- `RequireOnline` mapped to the online-only sync push helper,
- `save` and `remove` default `base_version` sourced from
  `LocalResourceView::base_version`.

Acceptance:

- normal CRUD callers do not build `ClientMutationDraft` directly,
- save/remove use canonical base versions when available,
- create/save can attach optimistic rows,
- remove can hide the row through a delete overlay,
- rejected create/save/remove outcomes remove the overlay and rebase from
  canonical rows,
- tests cover queue-offline and require-online mapping,
- `cargo test -p pocopine-sync-crud` passes,
- `cargo test -p pocopine-sync-crud --target wasm32-unknown-unknown --no-run`
  passes.

Phase 2 should choose the stronger API shape: generated and non-macro
resource methods return `Queued<Id>` only after the store has durably
reserved and enqueued the mutation id. That requires an async runtime
entry point instead of the existing fire-and-forget helper. Existing
low-level `ClientMutationDraft` callers remain supported as protocol
escape hatches; this phase removes direct draft construction from normal
CRUD examples and docs, not from the public sync API.

### Phase 3: Example And Author-Facing Docs

Goal: make the API reviewable in a small app without hiding important
state.

Deliverables:

- add a focused author-facing CRUD runtime walkthrough in
  `docs/sync-crud.md`,
- add tests in `pocopine-sync-crud` that exercise the same public runtime
  path shown in the walkthrough,
- document the author-facing create/save/remove path,
- document when to use `QueueOffline` versus `RequireOnline`,
- document how `LocalResourceView` exposes pending/conflict/base-version
  state.

Acceptance:

- the documented code path no longer hand-builds CRUD sync envelopes for
  ordinary create/save/remove,
- docs show the common path first and protocol escape hatches second,
- no doc implies Pocopine owns SQL or application schema.

### Phase 4: Final Verification And PR

Goal: prove the branch is reviewable and CI-shaped before PR creation.

Deliverables:

- `cargo fmt`,
- targeted `pocopine-sync-crud` tests,
- wasm no-run check for target-gated code where relevant,
- final full-branch Claude Code CLI review,
- PR with phase summary and verification commands.

Acceptance:

- Claude final review has no blockers,
- local verification passes or any skipped check is explicitly justified,
- PR description lists each phase commit and the review status.

## Deferred Work

These are intentionally outside this PR:

- SQLx helper adapters,
- backend-specific changefeeds,
- conflict resolution UI helpers such as `retry_local` or
  `merge_with`,
- auth cache partitioning helpers,
- proc-macro code generation,
- CRDT field integration.

Those layers become safer once the non-macro resource runtime has a
tested contract.
