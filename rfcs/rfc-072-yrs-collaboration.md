# RFC 072 - Yrs collaboration over WebSocket and Redis

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Builds on** | [RFC 070](./rfc-070-event-spine-and-live-invalidation.md) |
| **Related** | [RFC 069](./rfc-069-observability.md), [RFC 071](./rfc-071-offline-sync-protocol.md) |

## 1. Summary

Pocopine should add `pocopine-collab`, a separate collaboration crate for
shared documents using `yrs`, Redis, and WebSocket. This is the CRDT
layer. It is not the general database sync protocol from RFC 071 and it
is not the live invalidation stream from RFC 070.

The design follows the useful shape of y/hub: stateless WebSocket
servers, Redis as the realtime distribution/cache layer, and persistent
document storage behind a trait. Pocopine must use that architecture as a
reference, not port y/hub code.

## 2. Problem Statement

Collaborative documents have different requirements from ordinary data
refresh:

- multiple users can edit the same logical value at the same time,
- offline edits must merge instead of overwriting one another,
- cursor/presence state is ephemeral,
- updates are binary CRDT deltas,
- read-only users must receive updates but must not publish them.

Those requirements do not fit SSE invalidation and should not be forced
into the database sync protocol.

## 3. Goals

- Use `yrs` as the Rust CRDT engine.
- Provide a WebSocket route for Yjs/Yrs-compatible document sync.
- Use Redis Streams for ordered update storage and Redis Pub/Sub for
  wake-up/distribution.
- Allow multiple WebSocket server instances with no sticky sessions.
- Persist snapshots and compacted updates through a storage trait.
- Support read-only and read-write access decisions per document.
- Keep awareness/presence ephemeral and TTL-backed.
- Emit tracing events through Pocopine observability.

## 4. Non-goals

- This RFC does not replace RFC 071 sync.
- This RFC does not define rich text editor UI components.
- This RFC does not expose raw Redis rooms to clients.
- This RFC does not require y-websocket wire compatibility in Phase A,
  though the protocol should stay close enough that a compatibility mode
  is possible.
- This RFC does not adopt y/hub's license or source code.

## 5. Public API Shape

```rust
let collab = pocopine::collab::CollabHub::builder()
    .backend(CollabBackend::redis_from_env()?)
    .store(MyCollabStore::new(...))
    .authorize(|ctx, doc| async move {
        if can_edit(ctx, doc).await {
            CollabAccess::ReadWrite
        } else if can_read(ctx, doc).await {
            CollabAccess::ReadOnly
        } else {
            CollabAccess::Deny
        }
    })
    .build();

let router = Router::new().merge(pocopine::collab::routes(collab));
```

Document identity:

```rust
pub struct CollabDoc {
    pub namespace: String,
    pub collection: String,
    pub id: String,
    pub branch: String,
}
```

Default endpoint:

```text
GET /__pocopine/collab/v1/:namespace/:collection/:doc_id?branch=main
Upgrade: websocket
```

The route uses normal Pocopine auth context. Signed query tokens are only
for environments where WebSocket headers or cookies are not available.

## 6. Wire Protocol

The transport is binary WebSocket frames.

Phase A supports Pocopine framing around Yjs/Yrs concepts:

- sync step 1: client state vector,
- sync step 2: missing update diff,
- update: incremental document update,
- awareness: ephemeral presence state,
- control: Pocopine error, access, and server metadata frames.

Read-only users may send state-vector requests and awareness frames if
awareness is enabled. They must not send document updates.

The server must validate frame size and reject malformed updates before
publishing to Redis.

## 7. Redis Layout

```text
pocopine:{app}:collab:{doc_hash}:updates
pocopine:{app}:collab:{doc_hash}:pubsub
pocopine:{app}:collab:{doc_hash}:awareness
pocopine:{app}:collab:persist
pocopine:{app}:collab:{doc_hash}:lock
```

The update stream stores accepted document updates and provides replay
for clients that connect after earlier updates. Pub/Sub is the fast
fan-out path for currently connected servers. Awareness is TTL-backed and
not persisted.

If a Redis Cluster backend is supported, the key layout must document the
hash-tag behavior. If Lua scripts operate on several keys for one
document, those keys must live in one slot.

## 8. Initial Sync

On WebSocket connection:

1. authenticate and authorize the document,
2. receive or request the client state vector,
3. load the latest persisted snapshot through `CollabStore`,
4. apply Redis stream updates newer than the snapshot cursor,
5. compute the missing diff with `yrs`,
6. send the diff,
7. subscribe the server instance to Redis Pub/Sub for new updates.

The WebSocket server should not keep all documents loaded forever. A
document may be loaded for initial sync and then released after the
required diff is computed.

## 9. Update Flow

When a client sends an update:

1. check write access for the connection,
2. validate update size and decodeability,
3. append the update to the Redis stream,
4. publish a wake-up on the document Pub/Sub channel,
5. fan out to local WebSocket clients,
6. debounce a persistence task.

Publishing and persistence scheduling should be atomic for a document
where the backend supports it.

## 10. Persistence

```rust
pub trait CollabStore {
    async fn load_snapshot(
        &self,
        doc: &CollabDoc,
    ) -> CollabResult<Option<CollabSnapshot>>;

    async fn save_snapshot(
        &self,
        doc: &CollabDoc,
        snapshot: CollabSnapshot,
    ) -> CollabResult<()>;
}
```

The worker reads document ids from the persistence queue, merges pending
updates using `yrs`, saves a compacted snapshot, and trims old Redis
updates only after durable storage succeeds.

Initial implementations may provide file, memory, or Postgres-backed
stores. S3-compatible blob storage can be added later.

## 11. Awareness

Awareness is presence, not durable content. It can carry cursor position,
selected range, display name, or avatar metadata. It must not carry
permissions or trusted identity.

Awareness state expires automatically. Disconnect should remove local
state when possible, but timeout is the correctness path.

## 12. Security Model

- Authorization is checked before initial sync.
- Write authorization is checked before every document update.
- Read-only users cannot publish CRDT updates.
- Update size caps are mandatory.
- Room names are server-derived from `CollabDoc`; clients do not choose
  raw Redis keys.
- Raw update bytes are not logged.
- Yrs client ids must be unique per active peer. If the server assigns
  ids, it must avoid reuse across active connections.

## 13. Observability

`pocopine-collab` must emit tracing events but must not install logging
subscribers:

- `pocopine.trace` for connection lifecycle, sync phases, Redis stream
  operations, and persistence worker steps.
- `pocopine.log` for auth denials, malformed frames, backend failures,
  and snapshot failures.
- `pocopine.metric` for connected clients, active documents, update
  bytes, sync latency, persistence lag, and rejected updates.

Document payloads and raw CRDT updates must be redacted.

## 14. Relationship To Live And Sync

`pocopine-collab` owns collaborative document state. `pocopine-sync` owns
ordinary database-shaped offline data. They may meet at persistence time:
when a collaborative document snapshot is saved, the collab worker may
publish a live invalidation such as `collection.changed` so regular
views refresh metadata or rendered previews.

The CRDT stream should never be treated as the database change stream.

## 15. Phases

### Phase A - In-memory WebSocket prototype

- Add `pocopine-collab` crate.
- Add WebSocket route with memory backend.
- Validate Yrs update decode/apply.
- Enforce read-only versus read-write access.

### Phase B - Redis realtime backend

- Add Redis Streams plus Pub/Sub backend.
- Support multiple WebSocket server instances.
- Add reconnect/replay tests gated by `REDIS_TEST_URL`.

### Phase C - Persistence worker

- Add `CollabStore` trait and worker queue.
- Persist compacted snapshots.
- Trim Redis streams only after durable save.

### Phase D - Awareness

- Add TTL-backed awareness state.
- Add server-side size and field caps.

### Phase E - Client provider helpers

- Add browser provider helpers for Pocopine apps.
- Optionally add y-websocket compatibility mode.

### Phase F - Live bridge

- Publish RFC 070 invalidations when documents persist.
- Keep raw CRDT updates out of live events.

## 16. Research References

- Yrs crate: https://docs.rs/yrs/latest/yrs/
- Yjs document updates: https://docs.yjs.dev/api/document-updates
- Yjs protocol notes: https://raw.githubusercontent.com/yjs/y-protocols/master/PROTOCOL.md
- y-protocols repository: https://github.com/yjs/y-protocols
- y/hub architecture reference: https://github.com/yjs/yhub
