# RFC 071 - Event spine and live invalidation streams

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Related** | [RFC 002](./rfc-002-app-stores-servers.md), [RFC 066](./rfc-066-server-function-auth.md), [RFC 069](./rfc-069-observability.md), [RFC 072](./rfc-072-offline-sync-protocol.md), [RFC 073](./rfc-073-yrs-collaboration.md) |

## 1. Summary

Pocopine needs a database-agnostic way to publish application events and
stream safe, authorized invalidation messages to clients. This RFC adds
two layers:

- `pocopine-events`: the generic event spine. It owns event envelopes,
  topics, cursors, audience metadata, replay contracts, and backend
  traits.
- `pocopine-live`: the browser/client invalidation layer. It consumes
  `pocopine-events` and exposes a guarded server-to-client stream for
  events such as "collection changed" and "query tag invalidated".

The first browser transport is Server-Sent Events (SSE). WebSocket can be
added later for bidirectional protocols, but live invalidation is
server-to-client and should stay simple until sync requires more.

## 2. Problem Statement

Applications need to refresh client data when server-side resources
change. Today each app must invent its own polling, WebSocket, or
database-specific realtime integration. That leads to unsafe defaults:

- clients subscribe to raw database topics,
- deleted rows leak through old payloads,
- authorization is checked when the stream opens but not per resource,
- cursors cannot resume after disconnects,
- no clear separation exists between "refresh this query" and "replicate
  this row".

Pocopine should provide the safe framework path first. The default should
not expose database replication internals to browsers.

## 3. Goals

- Provide a reusable event envelope and backend abstraction.
- Stream collection/query invalidations to browsers with auth filtering.
- Support reconnect and replay using opaque cursors.
- Make in-memory delivery available for single-process deployments and
  tests.
- Allow Redis Streams plus Pub/Sub as the production multi-process
  backend.
- Keep native browser notifications out of the core data invalidation
  protocol.
- Emit tracing events through the framework observability stack instead
  of installing loggers in runtime crates.

## 4. Non-goals

- This RFC does not replicate database rows to offline clients.
- This RFC does not define conflict resolution.
- This RFC does not define CRDT collaboration.
- This RFC does not let browsers subscribe to raw Redis, Postgres, or CDC
  topics.
- This RFC does not implement operating-system browser notifications.
  Those can be built as a UX adapter on top of application events.

## 5. Crate Boundaries

### 5.1 `pocopine-events`

`pocopine-events` is transport neutral:

```rust
pub struct EventEnvelope {
    pub protocol: &'static str,
    pub id: EventId,
    pub topic: Topic,
    pub kind: EventKind,
    pub audience: Audience,
    pub cursor: EventCursor,
    pub payload: serde_json::Value,
    pub created_at_ms: u64,
    pub schema_version: u32,
}

pub trait EventBackend {
    async fn publish(&self, event: EventEnvelope) -> EventResult<EventCursor>;
    async fn subscribe(&self, request: SubscribeRequest) -> EventResult<EventStream>;
    async fn replay(&self, request: ReplayRequest) -> EventResult<Vec<EventEnvelope>>;
}
```

The envelope is intentionally generic. It does not know what a database,
browser, sync shape, or CRDT document is.

### 5.2 `pocopine-live`

`pocopine-live` is the data invalidation layer:

```rust
pocopine::live! {
    collection posts {
        scope: |ctx, post| post.tenant_id == ctx.tenant_id();
        tags: ["posts:list", "posts:detail"];
    }
}
```

The macro registers public collection names and guards. The browser sees
`posts`, not `public.posts`, a Redis key, or a logical replication slot.

## 6. Live Protocol

The first endpoint is:

```text
GET /__pocopine/live/v1/stream
Accept: text/event-stream
Last-Event-ID: <optional cursor>
```

The client may request collections and query tags using query parameters
or a signed request body variant in a later phase:

```text
/__pocopine/live/v1/stream?collection=posts&tag=posts:list
```

The server intersects the requested subscriptions with the current
`RequestContext`. Unauthorized subscriptions are ignored or rejected with
a typed error before the stream opens.

Example SSE frame:

```text
event: collection.changed
id: live:v1:redis:1700000000000-0
data: {"protocol":"pocopine.live.v1","collection":"posts","op":"upsert","keys":["post_123"],"query_tags":["posts:list","posts:detail:post_123"],"cursor":"live:v1:redis:1700000000000-0","schema_version":1}
```

Reserved event names:

- `ready`: stream accepted and replay position established.
- `collection.changed`: one or more records may have changed.
- `collection.deleted`: one or more records may have been deleted.
- `query.invalidated`: a query tag should be refetched.
- `gap`: the cursor is too old or unavailable; the client must refetch.
- `error`: the stream failed in a typed way.

Keepalive comments may be sent without changing the cursor.

## 7. Security Model

Event publication and event subscription are separate checks.

Publishers may be server functions, jobs, CDC adapters, or application
code. They publish into framework topics. They do not choose browser
audiences directly unless the topic type explicitly allows it.

Subscribers are authenticated through the existing server auth context.
Every live collection must declare an access policy. Missing policies
should produce compile-time warnings once the macro layer exists.

Deletion events must not include full deleted records by default. They
may include public primary keys or query tags only. If an app opts into
old payloads, that payload must pass the same guard as a normal read.

## 8. Backends

### 8.1 In-memory backend

The in-memory backend is valid for:

- tests,
- local development,
- single-process production deployments that explicitly choose it.

It keeps a bounded ring buffer for replay. If a cursor falls out of the
ring, the backend returns `gap`.

### 8.2 Redis backend

The Redis backend uses:

```text
pocopine:{app}:events:{topic_hash}:stream
pocopine:{app}:events:{topic_hash}:pubsub
```

Streams provide replay and cursors. Pub/Sub is only a wake-up path so
listeners do not need to poll streams constantly.

The backend must document whether keys are intentionally hash-tagged for
single-slot Redis Cluster behavior. If Lua scripts span multiple keys,
the key layout must keep those keys in one slot.

## 9. Observability

Runtime crates must emit events and spans but must not install a
subscriber. Important targets:

- `pocopine.trace` for stream lifecycle and backend operations.
- `pocopine.log` for connection failures, replay gaps, and auth denials.
- `pocopine.metric` for counters such as connected clients and replay
  latency.

Payloads should be redacted by default. Record bodies are not logged.

## 10. Relationship To Sync And Collab

`pocopine-live` is not the offline sync protocol. It is the live
invalidation channel that can tell a client that a sync shape is stale.

`pocopine-collab` is not built on live events for document updates. CRDT
updates need a binary WebSocket protocol and different ordering rules.
After a collaborative document is persisted, it may publish a live
invalidation so non-collaborative views can refresh.

## 11. Phases

### Phase A - Event spine

- Add `pocopine-events` types and in-memory backend.
- Add event cursor, topic, and replay contracts.
- Add unit tests for ring-buffer replay and gap behavior.

### Phase B - Live SSE

- Add `pocopine-live` route builder and SSE stream.
- Support collection/query invalidation payloads.
- Enforce collection registration and access policies.

### Phase C - Redis backend

- Add Redis Streams plus Pub/Sub backend.
- Add reconnect and replay tests gated by `REDIS_TEST_URL`.

### Phase D - Framework integration

- Add server-function helpers such as `ctx.live().invalidate(...)`.
- Add generated warnings for public collections without policies.
- Add client helpers that refetch registered query tags.

## 12. Research References

- Supabase Realtime protocol: https://supabase.com/docs/guides/realtime/protocol
- Supabase Postgres changes: https://supabase.com/docs/guides/realtime/postgres-changes
- CouchDB replication protocol: https://docs.couchdb.org/en/stable/replication/protocol.html
- ElectricSQL shapes: https://electric-sql.com/product/sync
