# Live Invalidation Tutorial

`pocopine-live` gives an app a liveness channel: the server publishes
database-agnostic invalidation events and the browser refetches the data
it already knows how to load. This is intentionally smaller than offline
sync or collaboration:

- live invalidation says "something changed; refetch this query",
- offline sync will own local persistence, conflict handling, and CDC,
- collaboration will own CRDT document updates.

The current transport is SSE. The current browser API is
`LiveRefresh::scoped()`. The runnable source of truth is
[`examples/live`](../examples/live/), and the CI regression test is
[`examples/live/tests/live_refresh.rs`](../examples/live/tests/live_refresh.rs).

When live APIs change, update this document, `examples/live/README.md`,
and the live example test in the same PR.

## What You Build

A live app needs four pieces:

1. Stable topic names for the data you expose.
2. A host-side event backend and `LiveHub`.
3. Server mutations that publish invalidations after they commit.
4. Browser components that subscribe and refetch.

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
        __create_post_route, __list_posts_route, __reset_posts_route, live_backend,
        POSTS_COLLECTION, POSTS_LIST_QUERY_TAG,
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
    let router = __list_posts_route(router);
    let router = __create_post_route(router);
    let router = __reset_posts_route(router);

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

## 6. Subscribe In The Component

Open the stream in `on_mount`. Use `LiveRefresh::scoped()` so the stream
closes when the component unmounts.

```rust
#[handlers]
impl LiveBoard {
    pub fn on_mount(&mut self) {
        self.status = "live stream opening".to_string();
        self.reload();

        #[cfg(pocopine_browser)]
        {
            let handle = this::<Self>();
            let refresh_handle = handle.clone();
            let gap_handle = handle.clone();
            let error_handle = handle;

            if let Err(err) = pocopine::live::LiveRefresh::scoped()
                .query_tag(POSTS_LIST_QUERY_TAG, move |_event| {
                    refresh_handle.update(|s| {
                        s.status = "live event received".to_string();
                        s.reload();
                    });
                })
                .on_gap(move |_event| {
                    gap_handle.update(|s| {
                        s.status = "live cursor gap; refetching".to_string();
                    });
                })
                .on_error(move |event| {
                    error_handle.update(|s| {
                        s.status = "live stream error".to_string();
                        s.error = format!("{:?}", event.live_event);
                    });
                })
                .open()
            {
                self.status = "live stream failed".to_string();
                self.error = err.to_string();
            }
        }
    }
}
```

The `reload` method should use the same server function you already use
for first load:

```rust
pub fn reload(&mut self) {
    self.loading = true;
    dispatch!(list_posts().await, |s, result| {
        s.loading = false;
        match result {
            Ok(posts) => {
                s.posts = posts;
                s.error.clear();
            }
            Err(err) => {
                s.status = "refresh failed".to_string();
                s.error = err.to_string();
            }
        }
    },);
}
```

If a component cares about every change to a collection, subscribe with
`.collection(POSTS_COLLECTION, ...)` instead of `.query_tag(...)`.

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

Use `MemoryEventBackend` only when all publishers and subscribers live in
one process. For multi-process servers, deploy a shared backend such as
Redis and build the hub from that backend instead. The browser stream
URL, component subscriptions, collection names, and query tags do not
change.

## Failure Model

- SSE delivery is at-least-once. Refresh callbacks must tolerate
  duplicate events.
- A `gap` means the backend could not replay enough retained history.
  Pocopine invokes registered refresh callbacks so the component refetches
  from scratch.
- An `error` goes to `on_error` callbacks and does not automatically
  refetch data.
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
