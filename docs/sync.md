# Sync Tutorial

`pocopine-sync` is the framework extension for cursor-based data sync.
The first implementation is intentionally small:

- the server owns named sync shapes,
- the browser pulls a snapshot, then incremental changes with a cursor,
- `pocopine-live` is used only as a wake-up signal,
- the data payload still moves through `POST /__pocopine/sync/v1/pull`.

This is not the full offline store yet. IndexedDB persistence,
optimistic mutation replay, SQLx-backed adapters, and CDC integrations
remain future work. The current API gives us the protocol boundary and a
browser state shape that those backends can plug into later.

The runnable source of truth is [`examples/sync`](../examples/sync/).
When the sync API changes, update this document and the example in the
same PR.

## What You Build

A sync-enabled app needs five pieces:

1. A stable shape name for the data the browser may sync.
2. A host-side `SyncShapeSource` registered with `SyncServer`.
3. Sync routes mounted through `sync_server_plugin(...)`.
4. Optional live routes that allow the sync shape's wake-up topic.
5. A browser component with `CollectionState<T>` and `sync_plugin()`.

The example uses `MemorySyncShape<Post>` so the whole flow can run
without a database. Production adapters should implement
`SyncShapeSource` and keep the browser code unchanged.

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

## 2. Define A Shape

A shape is the server-approved subset of data the browser can sync. It
is not a database table name or arbitrary browser filter.

```rust
pub const POSTS_SHAPE: &str = "posts_for_user";
pub const POSTS_COLLECTION: &str = "posts";
```

The initial memory source is useful for examples, tests, and explicit
single-process apps:

```rust
#[cfg(pocopine_host)]
pub fn posts_shape() -> pocopine_sync::MemorySyncShape<Post> {
    static POSTS_SYNC: std::sync::OnceLock<
        pocopine_sync::MemorySyncShape<Post>,
    > = std::sync::OnceLock::new();

    POSTS_SYNC
        .get_or_init(|| {
            let shape =
                pocopine_sync::MemorySyncShape::new(POSTS_SHAPE, POSTS_COLLECTION)
                    .expect("shape names must be valid");
            shape
                .upsert("post_1", Post::seeded())
                .expect("seed post should sync");
            shape
        })
        .clone()
}
```

Database-backed apps should hide authorization, tenant filters, delete
privacy, and cursor validation inside their source implementation.
Browsers only ask for registered shape names.

## 3. Build The Sync Server

`SyncServer` owns the registered shapes and optionally shares the live
event backend so it can publish wake-ups after mutations commit:

```rust
#[cfg(pocopine_host)]
pub fn sync_server() -> pocopine_sync::SyncServer {
    static SYNC_SERVER: std::sync::OnceLock<pocopine_sync::SyncServer> =
        std::sync::OnceLock::new();

    SYNC_SERVER
        .get_or_init(|| {
            pocopine_sync::SyncServer::builder()
                .shape(posts_shape())
                .events(std::sync::Arc::new(live_backend()))
                .build()
        })
        .clone()
}
```

Publishing a sync invalidation does not send rows through SSE. It sends
a query-tag wake-up that tells the browser to call `pull`:

```rust
#[cfg(pocopine_host)]
async fn invalidate_posts() {
    if let Err(err) = sync_server().invalidate_shape(POSTS_SHAPE).await {
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
only the wake-up topics for registered shapes.

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
            .shape(POSTS_SHAPE)
            .and_then(|collection| collection.open());

        if let Err(err) = result {
            self.posts.set_error(err.to_string());
        }
    }
}
```

`open()` pulls once. When live wake-up is enabled, it also subscribes to
the shape's wake-up topic and pulls again whenever the server invalidates
that shape. `pull()` can be called manually for refresh buttons.

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

## 6. Mutate Then Invalidate

Server functions can mutate the source and publish a wake-up after the
write succeeds:

```rust
#[pocopine::server(public)]
pub async fn create_post(title: String, body: String) -> ServerResult<Post> {
    let post = insert_post(title, body)?;

    posts_shape()
        .upsert(post.id.clone(), post.clone())
        .map_err(|err| ServerError::App(err.to_string()))?;

    invalidate_posts().await;
    Ok(post)
}
```

Do not publish before the mutation commits. If wake-up publishing fails
after a successful mutation, log it and return the mutation result; the
next manual pull or page load should still see the committed data.

## 7. Run The Example

```bash
cargo run -p pocopine-cli -- dev --path examples/sync
```

Open `http://127.0.0.1:3021` in two tabs. Create or reset posts in one
tab. The other tab should receive a live wake-up and then pull the new
sync changes.

For a fast compile check:

```bash
cargo check -p sync-example
cargo test -p pocopine-sync
```

## Protocol Boundary

- `open` validates shape names and reports registered shape metadata.
- `pull` returns either a full snapshot or incremental changes.
- `push` exists in the protocol, but the memory shape rejects it until
  optimistic mutations and conflict policy are implemented.
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
- Authorization belongs to the shape source and route policy. Do not
  treat a cursor or browser-provided shape name as proof of access.

## Future Backends

Keep these as separate adapter crates:

- `pocopine-sync-sqlx` for compile-time checked SQL shapes,
- `pocopine-sync-indexeddb` for browser persistence,
- `pocopine-sync-redis` for shared cursor/change storage.

Those crates should implement the same protocol contracts rather than
adding optional backend settings to framework core.
