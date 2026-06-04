---
title: "Live Invalidation Tutorial"
description: "Wire SSE live invalidation with collection and query refresh callbacks."
---

# Live Invalidation Tutorial

`pocopine-live` gives an app a liveness channel: the server publishes
database-agnostic invalidation events and the browser refetches the data
it already knows how to load. This is intentionally smaller than offline
sync or collaboration:

- live invalidation says "something changed; refetch this query",
- offline sync will own local persistence, conflict handling, and CDC,
- collaboration will own CRDT document updates.

The current transport is SSE. The recommended browser API is
`LiveQuery::scoped(...)`, which stores loading/error/data state in a
component-owned `QueryState<T>`. The runnable source of truth is
[`examples/live`](../../examples/live/), and the CI regression test is
[`examples/live/tests/live_refresh.rs`](../../examples/live/tests/live_refresh.rs).

When live APIs change, update this document, `examples/live/README.md`,
and the live example test in the same PR.

## What You Build

A live app needs four pieces:

1. Stable topic names for the data you expose.
2. A host-side event backend and `LiveHub`.
3. Server mutations that publish invalidations after they commit.
4. Browser components that use `LiveQuery` to subscribe and refetch.

The example below tracks a posts list. A post mutation publishes both a
collection invalidation and a query-tag invalidation:

- `collection:posts` is useful for any view that cares about the whole
  collection.
- `query:posts:list` is useful for one logical query, such as the current
  list screen.

Publishing only the collection event does not wake a browser subscriber
that opened `query:posts:list`.

## 1. Enable The Crates

For a single-process development app, enable the umbrella `live` feature
and use the memory backend:

```toml
[dependencies]
pocopine = { path = "../../crates/pocopine", features = ["live"] }
serde = { workspace = true }
wasm-bindgen = { workspace = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
pocopine-events = { workspace = true }
pocopine-logging = { workspace = true }
pocopine-server = { path = "../../crates/pocopine-server" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
tracing = { workspace = true }
```

For Redis-backed multi-process deployments, enable `live-redis` instead
of `live`. The browser code stays the same; only the backend
construction changes.

## About Target Cfgs

The snippets use the same cfg aliases as `examples/live`:

- `#[cfg(pocopine_host)]` for non-wasm server code,
- `#[cfg(pocopine_browser)]` for wasm browser code.

Add this `build.rs` to an app if you want the shorter aliases:

```rust
fn main() {
    println!("cargo:rustc-check-cfg=cfg(pocopine_browser)");
    println!("cargo:rustc-check-cfg=cfg(pocopine_host)");

    match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("wasm32") => println!("cargo:rustc-cfg=pocopine_browser"),
        _ => println!("cargo:rustc-cfg=pocopine_host"),
    }
}
```

Without that build script, use `#[cfg(not(target_arch = "wasm32"))]`
and `#[cfg(target_arch = "wasm32")]` directly.

## 2. Define Stable Topics

Put collection and query tag names next to the server functions that load
or mutate that data. Do not duplicate these strings in the component and
server mount code.

```rust
pub const POSTS_COLLECTION: &str = "posts";
pub const POSTS_LIST_QUERY_TAG: &str = "posts:list";
```

Use collection names for broad invalidations and query tags for logical
queries. Query tags should be stable enough that a refactor does not
silently break refresh behavior.

## 3. Create A Backend

The memory backend is process-local. It is correct for tests, demos, and
single-process development, but it does not coordinate multiple server
processes.

```rust
#[cfg(pocopine_host)]
use {
    pocopine_events::{EventBackend, MemoryEventBackend},
    std::sync::OnceLock,
};

#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();

#[cfg(pocopine_host)]
pub fn live_backend() -> MemoryEventBackend {
    LIVE_BACKEND.get_or_init(MemoryEventBackend::new).clone()
}
```

The backend must be shared by the live SSE route and by any server code
that publishes invalidations. If those call sites use different backend
instances, subscribers will not receive the events.

## 4. Mount The Live Hub

Mount `pocopine::live::routes(...)` in the server binary and explicitly
allow the topics the browser may request. The default policy is deny-all.

```rust
#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use my_app::{
        live_backend, POSTS_COLLECTION, POSTS_LIST_QUERY_TAG,
    };
    use pocopine::live::{collection_topic, query_tag_topic, routes, LiveHub};
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, serve, static_files};

    init_default().map_err(std::io::Error::other)?;

    let posts_topic = collection_topic(POSTS_COLLECTION)
        .map_err(std::io::Error::other)?;
    let posts_list_topic = query_tag_topic(POSTS_LIST_QUERY_TAG)
        .map_err(std::io::Error::other)?;

    let live_hub = LiveHub::new(live_backend())
        .allow_topics([posts_topic.clone(), posts_list_topic.clone()])
        .default_topics([posts_topic, posts_list_topic]);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir));

    serve(router, "127.0.0.1:3000").await
}
```

`allow_topics` is the authorization boundary for the current live stream
surface. Do not use `allow_all_topics()` unless every possible requested
topic is already public for that application.

## 5. Publish After Mutations

Publish only after the mutation has committed. If the mutation fails, do
not publish. If publishing fails after a successful mutation, log it and
return the mutation result; the next page load or manual refresh should
still see committed data.

```rust
#[cfg(pocopine_host)]
async fn publish_posts_invalidation(op: pocopine::live::LiveOp, key: impl Into<String>) {
    let collection_draft = pocopine::live::LiveInvalidation::new(POSTS_COLLECTION, op)
        .keys([key.into()])
        .query_tags([POSTS_LIST_QUERY_TAG])
        .into_draft();

    publish_live_draft(collection_draft).await;

    let query_draft = pocopine::live::query_tag_topic(POSTS_LIST_QUERY_TAG)
        .and_then(|topic| {
            pocopine::live::query_invalidated(topic, [POSTS_LIST_QUERY_TAG])
        });

    publish_live_draft(query_draft).await;
}

#[cfg(pocopine_host)]
async fn publish_live_draft(
    draft: pocopine_events::EventResult<pocopine_events::EventDraft>,
) {
    let Ok(draft) = draft else {
        return;
    };

    if let Err(err) = live_backend().publish(draft).await {
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "failed to publish live posts invalidation"
        );
    }
}
```

Then call the publisher from the server function:

```rust
#[pocopine::server(public)]
pub async fn create_post(draft: PostDraft) -> ServerResult<Post> {
    let post = insert_post(draft)?;
    publish_posts_invalidation(
        pocopine::live::LiveOp::Upsert,
        post.id.clone(),
    )
    .await;
    Ok(post)
}
```

The server function body does not need `#[cfg(pocopine_host)]`; the
`#[pocopine::server]` macro already compiles the original body only for
the host target and generates a browser fetch stub for wasm.

## 6. Query In The Component

Store server-function data in a `QueryState<T>` field. The field is
serializable, so templates can read `data`, `loading`, `refreshing`,
`stale`, `error`, and counters directly.

```rust
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(style = "live_board.css")]
pub struct LiveBoard {
    pub posts: pocopine::live::QueryState<Vec<Post>>,
    pub title: String,
    pub body: String,
    pub saving: bool,
    pub status: String,
}

#[handlers]
impl LiveBoard {
    pub fn on_mount(&mut self) {
        self.status = "live query opening".to_string();
        if let Err(err) = pocopine::live::LiveQuery::scoped(
            |s: &mut Self| &mut s.posts,
            || async { list_posts().await },
        )
        .query_tag(POSTS_LIST_QUERY_TAG)
        .open()
        {
            self.status = "live query failed".to_string();
            self.posts.set_error(err.to_string());
        }
    }
}
```

`LiveQuery::scoped(...)` does three things:

- it fetches once on mount,
- it subscribes to the query tag and closes the SSE stream on unmount,
- it refetches when the server publishes a matching live invalidation or
  reports a replay gap.

Manual refresh uses the same field selector and server function:

```rust
pub fn reload(&mut self) {
    self.status = "refreshing".to_string();
    if let Err(err) = pocopine::live::LiveQuery::refresh_scoped(
        |s: &mut Self| &mut s.posts,
        || async { list_posts().await },
    ) {
        self.status = "refresh failed".to_string();
        self.posts.set_error(err.to_string());
    }
}
```

If a component cares about every change to a collection, subscribe with
`.collection(POSTS_COLLECTION)` instead of `.query_tag(...)`.

Templates read the query field:

```html
<p pp-show="posts.error" class="error">
  <span pp-text="posts.error"></span>
</p>
<p pp-show="posts.loading">Loading posts.</p>
<p pp-show="posts.refreshing">Refreshing posts.</p>

<ol pp-show="posts.data.length">
  <template pp-for="post in posts.data" pp-key="post.id">
    <li>
      <h2 pp-text="post.title"></h2>
      <p pp-text="post.body"></p>
    </li>
  </template>
</ol>
```

`QueryState<T>` preserves existing data on refresh errors. A stale async
response cannot overwrite a newer request, so rapid manual refreshes and
live invalidations are safe.

## Advanced: Manual Streams

Use `LiveRefresh::scoped()` directly only when a component needs custom
event handling instead of a query refetch:

```rust
let handle = this::<LiveBoard>();
pocopine::live::LiveRefresh::scoped()
    .query_tag(POSTS_LIST_QUERY_TAG, move |_event| {
        handle.update(|s| {
            s.status = "custom live event".to_string();
        });
    })
    .open()?;
```

Most app data should use `LiveQuery`; manual streams are the escape hatch.

## 7. Run The Example

```bash
cargo run -p pocopine-cli -- dev --path examples/live
```

Open `http://127.0.0.1:3020` in two tabs. Create or reset posts in one
tab; the other tab should refresh through the SSE stream.

For the CI-level regression:

```bash
cargo test -p live-example --test live_refresh
```

That test opens the real SSE route for `query:posts:list`, calls
`create_post`, and asserts that `query.invalidated` is delivered.

## 8. Production Backends

Pocopine ships two event backends; the one you pick is the single most
important deployment decision for live invalidation.

| Backend | Crate path | Use when |
|---|---|---|
| `MemoryEventBackend` | `pocopine_events::MemoryEventBackend` | Tests, dev, single-process deployments only |
| `RedisEventBackend` | `pocopine_events::RedisEventBackend` (`redis` feature) | Multi-process / horizontally-scaled servers |

### Why `MemoryEventBackend` is not enough for production

`MemoryEventBackend` is a per-process `tokio::sync::broadcast`. Events
published on one process **never reach a subscriber on another process** —
they live and die inside one node's memory.

```text
   Single node (MemoryEventBackend works):

     ┌─────────── Node 1 ──────────────┐
     │                                  │
     │  /push  ──► MemoryEventBackend   │
     │              │                    │
     │              ▼                    │
     │  ◄── SSE to all clients on Node 1 │
     └──────────────────────────────────┘


   Multi-node with MemoryEventBackend (BROKEN):

     Client A ──SSE──► Node 1                     Node 2 ◄──/push── Client B
                       │                            │
                       │  Memory backend            │  Memory backend
                       │  (Node 1, in-proc)         │  (Node 2, in-proc)
                       │                            │
                       ▼                            ▼
                Sees only events Node 1     Sees only events Node 2
                publishes locally           publishes locally

                ❌ Client A never receives Client B's mutation.
                   The publish stayed in Node 2's memory.


   Multi-node with shared broker (works):

     Client A ──SSE──► Node 1                     Node 2 ◄──/push── Client B
                       │                            │
                       ▼                            ▼
                  RedisEventBackend  ◄────►  RedisEventBackend
                       │                            ▲
                       │      ┌── Redis ──┐         │
                       └──────┤  PUB/SUB  ├─────────┘
                              │  + Streams│
                              └───────────┘

                ✓ Node 1 receives Client B's invalidation via Redis,
                  forwards to Client A's SSE.
```

If you run more than one server process — `kubectl` replica count > 1,
Render auto-scaling, blue/green deploy, anything — you need a shared
backend. The symptom of a missing shared backend is "live updates work
locally but mysteriously drop in production"; the fix is one line.

### Switching to `RedisEventBackend`

```rust,ignore
use pocopine_events::{build_event_backend, EventBackendConfig, RedisEventConfig};
use pocopine_live::LiveHub;

let backend = build_event_backend(EventBackendConfig::Redis(
    RedisEventConfig::new("redis://prod-redis:6379", "myapp")?,
))?;

let hub = LiveHub::new(backend)
    .allow_topic_prefixes(sync.live_topic_prefixes());  // RFC 088 §C; or
    // .allow_topics(sync.live_topics())                // pre-§C bare-only.
```

No `LiveHub` / `SyncServer` / `LiveClient` / browser code changes. The
backend is plug-and-play.

### Choosing a broker at scale

For most apps, `RedisEventBackend` is the right answer; it's batteries-
included and handles ~1M topics on a single node comfortably. The table
below covers the alternatives if your scale or platform constraints
make Redis a poor fit.

| Broker | Scale | When to pick |
|---|---|---|
| **Redis Pub/Sub + Streams** (built-in) | ~1M topics single-node, multi-million across Redis Cluster | Default. Already a dep. Streams give replay; fall back to /pull works. |
| **NATS JetStream** (custom backend) | 10M+ subjects across cluster | Subject-based routing maps naturally to RFC 088 §C's `(stream, params_hash)` topic shape. Best for fanout-heavy workloads. |
| **Kafka** (custom backend) | High-throughput sequential workloads | Use if you already run Kafka. Topic cardinality stresses Kafka — prefer one topic + header filtering over one-topic-per-(stream, hash). |
| **Postgres LISTEN/NOTIFY** (custom backend) | <10k concurrent subscribers per node | Smallest-scale option; no extra infra if you already run Postgres. |

`pocopine-events` ships Memory + Redis; NATS / Kafka / Postgres
backends are user-implementable via the `LiveEventBackend` trait.

### Topic cardinality with RFC 088 §C

RFC 088 §C (`sync-query` selector-level live routing) publishes to
per-`(stream, params_hash)` topics. With 1M users observing N streams ×
M workspaces (or other partition keys), the active topic count is
typically `N × M` — for 10 streams × 5k workspaces, that's 50k topics.
Redis single-node handles this without breaking a sweat.

If your topic count crosses ~10M (e.g. millions of distinct partition
hashes), shard with Redis Cluster (topic name is the shard key) or
move to NATS / Kafka. The `LiveHub::allow_topic_prefixes` policy is
broker-agnostic; only your config + backend change.

### What §C changes (and doesn't)

- **Subscribers per topic** drops dramatically (the whole point — from
  O(all clients) to O(matching clients)).
- **Topic count** rises (more topics, each with fewer subscribers).
- **Events per mutation** stays approximately the same (one bare publish
  + one per-params publish per row, vs the pre-§C single bare publish).
- **Broker CPU** is roughly a wash; broker memory rises slightly with
  topic metadata.

You only see the bandwidth win once the matching-subscriber count is
much smaller than the all-subscriber count — i.e. for filtered
multi-tenant workloads. For single-tenant or low-traffic apps, the
plain bare topic is enough; `MemoryEventBackend` + `allow_topics` is
fine.

## Failure Model

- SSE delivery is at-least-once. Refresh callbacks must tolerate
  duplicate events. `LiveQuery` handles this by refetching idempotently.
- A `gap` means the backend could not replay enough retained history.
  `LiveQuery` refetches from scratch.
- A stream `error` is recorded on the query state and does not erase the
  last successful data.
- Topic authorization is server-side. `LiveHub` denies all topics until
  the app installs a topic policy or explicit allowlist.
- Audience fields are descriptive metadata today. Do not rely on them for
  authorization.
- Live invalidation events do not carry database rows. They carry enough
  metadata for the browser to decide what to refetch.

## Naming Rules

- Use collection names for broad model surfaces: `posts`, `comments`.
- Use query tags for logical browser queries: `posts:list`,
  `posts:detail:{id}`.
- Keep tag constants near the server functions that publish them.
- Reuse the same constants in the server topic allowlist and browser
  subscriptions.

The safest rule is simple: the code that publishes an invalidation and
the code that subscribes to it should share a named constant, never a
copied string literal.
