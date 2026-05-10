# Sync Tutorial

`pocopine-sync` is the framework extension for cursor-based data sync.
The first implementation is intentionally small:

- the server owns named sync streams,
- the browser opens an authorized stream, then pulls a snapshot or
  incremental changes with a cursor,
- the browser can push server-confirmed optimistic mutations with stable
  mutation ids,
- `pocopine-live` is used only as a wake-up signal,
- committed stream data still moves through `POST /__pocopine/sync/v1/pull`.

This is not the full offline store yet. IndexedDB durability,
cross-reload mutation replay, SQLx-backed adapters, and CDC integrations
remain future work. The current API gives us the protocol boundary,
server-confirmed mutations, and a browser state model that those backends
can plug into later.

The runnable source of truth is [`examples/sync`](../examples/sync/).
When the sync API changes, update this document and the example in the
same PR.

## What You Build

A sync-enabled app needs five pieces:

1. A stable stream name for the data the browser may sync.
2. A host-side `SyncStreamSource` registered with `SyncServer`.
3. Sync routes mounted through `sync_server_plugin(...)`.
4. Optional live routes that allow the sync stream's wake-up topic.
5. A browser component with `CollectionState<T>` and `sync_plugin()`.

The example uses `MemorySyncStream<Post>` so the whole flow can run
without a database. Production adapters should implement
`SyncStreamSource` and keep the browser code unchanged.

## 1. Enable The Extension Crate

Sync is explicit. Do not enable a `pocopine` feature and do not import it
through framework core:

```toml
[dependencies]
pocopine = { path = "../../crates/pocopine" }
pocopine-sync = { workspace = true }
serde = { workspace = true }
wasm-bindgen = { workspace = true }

[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
pocopine-auth = { workspace = true }
pocopine-events = { workspace = true }
pocopine-live = { workspace = true }
pocopine-logging = { workspace = true }
pocopine-server = { workspace = true }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
tracing = { workspace = true }
```

`pocopine-sync` has target-specific modules internally. The browser gets
the client plugin and protocol/state types. The host gets the server
plugin and source traits.

## 2. Define A Stream

A stream is the server-approved flow of sync state the browser can open
and pull. It is not a database table name, arbitrary browser filter, or
SSE/WebSocket transport.

```rust
pub const POSTS_STREAM: &str = "posts_for_user";
pub const POSTS_COLLECTION: &str = "posts";
```

The initial memory source is useful for examples, tests, and explicit
single-process apps:

```rust
#[cfg(pocopine_host)]
pub fn posts_stream() -> pocopine_sync::MemorySyncStream<Post> {
    static POSTS_SYNC: std::sync::OnceLock<
        pocopine_sync::MemorySyncStream<Post>,
    > = std::sync::OnceLock::new();

    POSTS_SYNC
        .get_or_init(|| {
            let stream =
                pocopine_sync::MemorySyncStream::new(POSTS_STREAM, POSTS_COLLECTION)
                    .expect("stream names must be valid");
            stream
                .upsert("post_1", Post::seeded())
                .expect("seed post should sync");
            stream
        })
        .clone()
}
```

Browsers only ask for registered stream names. Stream guards decide
whether the request may use that stream; database-backed sources still
own tenant filters, delete privacy, cursor validation, and per-row
visibility.

`MemorySyncStream<T>` is not a production backend. It keeps an unbounded
in-memory change log and uses one process-local lock. Use it for tests,
examples, and explicit single-process demos.

## 3. Build The Sync Server

`SyncServer` owns the registered streams and optionally shares the live
event backend so it can publish wake-ups after mutations commit.

Every stream registration is explicit:

- `public_stream(...)` is for public demo or globally readable data.
- `guarded_stream(..., predicate)` accepts `pocopine-auth` predicates
  such as `require_auth()` or `require_role("admin")`.
- `guarded_stream_with(..., guard)` accepts an async guard that receives
  the same `RequestContext` used by server-function guards.

The example is intentionally public so it can run without an auth setup:

```rust
#[cfg(pocopine_host)]
pub fn sync_server() -> pocopine_sync::SyncServer {
    static SYNC_SERVER: std::sync::OnceLock<pocopine_sync::SyncServer> =
        std::sync::OnceLock::new();

    SYNC_SERVER
        .get_or_init(|| {
            pocopine_sync::SyncServer::builder()
                .public_stream(posts_stream())
                .events(std::sync::Arc::new(live_backend()))
                .build()
        })
        .clone()
}
```

A protected app should register guarded streams instead:

```rust
use pocopine_auth::{require_auth, require_role};

let sync = pocopine_sync::SyncServer::builder()
    .guarded_stream(user_posts_stream(), require_auth())
    .guarded_stream(admin_posts_stream(), require_role("admin"))
    .guarded_stream_with(tenant_posts_stream(), |ctx| async move {
        let user = ctx.require_user()?;
        ensure_tenant_access(user)?;
        Ok(())
    })
    .build();
```

Each `/open`, `/pull`, and `/push` request runs the stream guard before
the stream source is called. `SyncStreamSource` receives the
`RequestContext` after the guard passes, so sources can filter rows for
the authenticated user or tenant:

```rust
fn pull<'a>(
    &'a self,
    ctx: pocopine_auth::RequestContext,
    request: pocopine_sync::SyncPullRequest,
) -> pocopine_sync::SyncBoxFuture<'a, pocopine_sync::SyncPullResponse<serde_json::Value>>;
```

Publishing a sync invalidation does not send rows through SSE. It sends
a query-tag wake-up that tells the browser to call `pull`:

```rust
#[cfg(pocopine_host)]
async fn invalidate_posts() {
    if let Err(err) = sync_server().invalidate_stream(POSTS_STREAM).await {
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "failed to publish sync posts invalidation"
        );
    }
}
```

## 4. Mount Routes

Mount sync routes with the host server plugin. If live wake-up is
enabled, allow the topics reported by `sync.live_topics()`.

```rust
#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_live::{routes, LiveHub};
    use pocopine_server::{axum::Router, serve, static_files, Server};
    use pocopine_sync::sync_server_plugin;

    pocopine_logging::init_default().map_err(std::io::Error::other)?;

    let sync = sync_server();
    let sync_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topics(sync_topics.clone())
        .default_topics(sync_topics);

    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(env!("CARGO_MANIFEST_DIR")));

    let router = Server::new(router)
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .map_err(std::io::Error::other)?;

    serve(router, "127.0.0.1:3021").await
}
```

The default live topic policy is deny-all. `sync.live_topics()` returns
only the wake-up topics for registered streams.

`/open` is not a session token. It validates discovery and gives the
client metadata before the first pull, but `/pull` and `/push` still run
the stream guard independently. A client that skips `/open` does not
bypass access control.

## 5. Install The Browser Plugin

Install `sync_plugin()` in the app. Live wake-up is opt-in:

```rust
#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(pocopine_sync::sync_plugin().with_live_wakeup(true))
        .register::<SyncBoard>()
        .run();
}
```

`sync_plugin()` sends fetches without browser credentials by default. If
an app opts into `.with_credentials(true)`, the server must provide the
same CSRF protections it uses for any credentialed JSON POST endpoint.
When live wake-up is enabled, accepted pushes rely on the live
invalidation to trigger the follow-up pull. Without live wake-up, the
push path performs that pull directly.

Components store synced rows in `CollectionState<T>`:

```rust
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(style = "sync_board.css")]
pub struct SyncBoard {
    pub posts: pocopine_sync::CollectionState<Post>,
    pub status: String,
}
```

Open a collection from a lifecycle hook or handler:

```rust
#[handlers]
impl SyncBoard {
    pub fn on_mount(&mut self) {
        let result = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
            .stream(POSTS_STREAM)
            .and_then(|collection| collection.open());

        if let Err(err) = result {
            self.posts.set_error(err.to_string());
        }
    }
}
```

`open()` first calls `/__pocopine/sync/v1/open` to validate the stream,
then calls `/__pocopine/sync/v1/pull`. A fresh client still pulls
without a cursor so it receives a snapshot; the server's current cursor
from `open` is metadata, not permission to skip initial data. When live
wake-up is enabled, `open()` also subscribes to the stream's wake-up topic
and pulls again whenever the server invalidates that stream. `pull()` can
be called manually for refresh buttons.

Templates read `CollectionState<T>` directly:

```html
<p pp-show="posts.error">
  <span pp-text="posts.error"></span>
</p>
<p pp-show="posts.loading">Loading snapshot.</p>
<p pp-show="posts.syncing">Pulling changes.</p>

<ol pp-show="posts.rows.length">
  <template pp-for="row in posts.rows" pp-key="row.key">
    <li>
      <h2 pp-text="row.value.title"></h2>
      <p pp-text="row.value.body"></p>
    </li>
  </template>
</ol>
```

## 6. Push Optimistic Mutations

`SyncCollection::push` applies an optimistic row locally, sends a stable
mutation id to `/__pocopine/sync/v1/push`, then applies the server's
accepted, rejected, or conflict response. Accepted pushes also wake live
listeners through the sync server's event backend, so other tabs pull the
committed stream changes.

```rust
pub fn create(&mut self) {
    let post = Post {
        id: "post_local_1".to_string(),
        title: self.title.clone(),
        body: self.body.clone(),
    };
    let result = build_create_mutation(post)
        .and_then(|(mutation, optimistic)| {
            self.plugin::<pocopine_sync::SyncClient>()
                .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
                .stream(POSTS_STREAM)
                .and_then(|collection| collection.push(mutation, Some(optimistic)))
        });

    if let Err(err) = result {
        self.posts.set_error(err.to_string());
    }
}
```

Stream sources own validation and write policy. In the reference memory
source, invalid payloads are returned in `rejected`, stale
`base_version` values are returned in `conflicts`, and accepted upserts
append an incremental stream change. Production sources should keep the
same rule: do not emit a live wake-up until the mutation has committed.

The example uses short client-local ids to keep the code readable.
Production apps should use server-assigned ids or client-generated UUIDs
to avoid row-key collisions across tabs and devices.

## 7. Run The Example

```bash
cargo run -p pocopine-cli -- dev --path examples/sync
```

Open `http://127.0.0.1:3021` in two tabs. Create or reset posts in one
tab. The other tab should receive a live wake-up and then pull the new
sync changes.

For fast host checks:

```bash
cargo check -p sync-example
cargo test -p pocopine-sync
cargo test -p sync-example
```

For the browser smoke test that mounts a real component in headless
Firefox and verifies `open -> pull -> render`:

```bash
wasm-pack test --firefox --headless crates/pocopine-sync --test client_browser
```

## Protocol Boundary

- `open` validates stream names and reports registered stream metadata.
  The browser client calls it before the first `pull`.
- `pull` returns either a full snapshot or incremental changes.
- `push` sends client-generated mutation ids plus app payloads to the
  stream source. Responses split mutations into `accepted`, `rejected`,
  and `conflicts`; accepted mutations may include canonical rows and also
  wake live listeners so clients can pull the committed stream changes.
- Cursors are opaque. Components should store and resend them, not parse
  them.
- A live wake-up is not a data packet. It is only a prompt to pull.

## Failure Model

- Pull responses are server-authoritative.
- Duplicate live wake-ups are safe; the client pulls with its current
  cursor and applies idempotent upsert/delete/reset changes.
- A stale async response cannot overwrite a newer pull in
  `CollectionState<T>`.
- Memory backends are single-process. Multi-process sync needs shared
  event and data-source backends.
- Authorization belongs to stream guards plus the stream source's
  row-level policy. Do not treat a cursor or browser-provided stream name
  as proof of access.

## Future Backends

Keep these as separate adapter crates:

- `pocopine-sync-sqlx` for compile-time checked SQL streams,
- `pocopine-sync-indexeddb` for browser persistence,
- `pocopine-sync-redis` for shared cursor/change storage.

Those crates should implement the same protocol contracts rather than
adding optional backend settings to framework core.
