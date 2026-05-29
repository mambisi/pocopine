# Sync Tutorial: Build a Multi-Workspace Issue Tracker

This walks through building a complete sync app end-to-end:

- **Server**: SQLite-backed `CrudSource` serving a multi-tenant `issues` stream
- **Browser**: IndexedDB local cache + filtered subscriptions via the typed query DSL
- **Live wakeup**: RFC 088 §C precise routing — a push into workspace `W1` only wakes `W1` subscribers, `W2` stays silent

The end product is the canonical pattern from
[`sync-crud-query-composition.md`](./sync-crud-query-composition.md):
CRUD owns the write path, Query owns the read path, `.params_of` is
the bridge.

If you only need filtered reads OR typed writes (not both), skip to
the relevant section in the composition doc.

## Prerequisites

- Rust toolchain with `wasm32-unknown-unknown` target
  (`rustup target add wasm32-unknown-unknown`)
- A workspace using `pocopine` (or this repository's workspace if
  you're contributing)
- Familiarity with `pocopine::prelude::*` and the component model
  (see [`docs/sync.md`](./sync.md) for the basics)

## What you'll build

```
┌─────────────────────────────┐         ┌─────────────────────────────┐
│         Browser             │         │           Server            │
│                             │         │                             │
│  ┌──────────────────────┐   │         │   ┌──────────────────────┐  │
│  │ <IssueList>          │   │         │   │ CrudResource         │  │
│  │  filter: workspace_W1│   │  /pull  │   │  + .params_of(typed) │  │
│  │  view: rows()        │◄──┼─────────┼──►│                      │  │
│  │  Issue::query()      │   │         │   │ IssuesSource         │  │
│  │    .eq(workspace,W1) │   │  /push  │   │  (SQLx + SQLite)     │  │
│  │    .observe(client)  │◄──┼─────────┼──►│                      │  │
│  └──────────────────────┘   │         │   └──────────────────────┘  │
│                             │         │                             │
│  IndexedDbLocalStore        │  SSE    │   LiveHub + MemoryEvent     │
│  (offline cache)            │◄────────┼── (sync:stream:issues:<W1>) │
└─────────────────────────────┘         └─────────────────────────────┘
```

We'll get there in eight steps:

1. Workspace dependencies
2. Define the row type with both macros
3. Implement `CrudSource` with SQLite storage
4. Build the sync server with the `.params_of` bridge
5. Wire the browser with `IndexedDbLocalStore` and the query client plugin
6. Write a component that subscribes to a filtered view + pushes mutations
7. Run it and observe the live wakeup
8. Add a second workspace and watch §C precision in action

## 1. Workspace dependencies

Two crates: a host binary, a browser crate.

```toml
# Cargo.toml (workspace member: issues-server)
[dependencies]
pocopine = { workspace = true }
pocopine-sync = { workspace = true }
pocopine-sync-crud = { workspace = true }
pocopine-sync-query = { workspace = true }
serde = { workspace = true }

[target.'cfg(pocopine_host)'.dependencies]
pocopine-events = { workspace = true }
pocopine-live = { workspace = true }
pocopine-server = { workspace = true }
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
async-trait = "0.1"
serde_json = { workspace = true }
```

```toml
# Cargo.toml (workspace member: issues-browser)
[dependencies]
pocopine = { workspace = true }
pocopine-sync = { workspace = true }
pocopine-sync-query = { workspace = true }
serde = { workspace = true }

[target.'cfg(pocopine_browser)'.dependencies]
pocopine-sync-indexdb = { workspace = true }
wasm-bindgen = { workspace = true }
```

The `cfg(pocopine_host)` / `cfg(pocopine_browser)` gates keep
server-only deps (SQLx, tokio, axum) out of the wasm bundle and
browser-only deps (wasm-bindgen, indexdb) off the server side.

## 2. Define the row type

One struct, two macros. `#[query_resource]` declares the read shape;
`#[derive(Serialize, Deserialize)]` covers the wire encoding for
CRUD writes.

```rust
// issues/src/lib.rs (shared between host + browser)
use pocopine_sync_query::query_resource;
use serde::{Deserialize, Serialize};

pub const STREAM: &str = "issues";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Open,
    InProgress,
    Closed,
}

// `#[query_resource]` MUST come before `#[derive(...)]` so it strips
// the `#[query_param]` annotations before serde sees them.
#[query_resource(name = "issues", schema_version = 1)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,

    /// Tenant gate. `required` makes the predicate reject any query
    /// that doesn't filter on `workspace_id` — cross-tenant safety
    /// at compile time. This is also the §C partition key.
    #[query_param(required)]
    pub workspace_id: String,

    #[query_param]
    pub status: Status,

    #[query_param]
    pub title: String,

    pub body: String,
    pub version: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueDraft {
    pub workspace_id: String,
    pub status: Status,
    pub title: String,
    pub body: String,
}
```

What the macro generates (you'll consume it in steps 4 and 6):

- `Issue::query()` — typed query builder
- `issues::field::{workspace_id, status, title}` — field markers
- `issues::matches(query, row)` — predicate evaluator
- `issues::row_to_params(&Value)` and `issues::row_to_params_typed(&Issue)` — §C projectors
- `issues::partition_for_topic(captured_params)` — client-side hash

The `Draft` type is a separate struct: it represents "what a client
can send when creating or editing an issue." Server-controlled fields
like `id` and `version` aren't on the Draft.

## 3. Implement `CrudSource` with SQLite

This is your code. Pocopine doesn't generate SQL; you write idiomatic
SQLx.

```rust
// issues-server/src/source.rs
use async_trait::async_trait;
use pocopine_auth::RequestContext;
use pocopine_sync::{RowVersion, SyncError, SyncResult};
use pocopine_sync_crud::{CrudRemoveResult, CrudSource, CrudWriteResult};
use sqlx::SqlitePool;

use issues::{Issue, IssueDraft, Status};

#[derive(Clone)]
pub struct IssuesSource {
    pub pool: SqlitePool,
}

impl IssuesSource {
    pub async fn new(database_url: &str) -> sqlx::Result<Self> {
        let pool = SqlitePool::connect(database_url).await?;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS issues (
                id            TEXT    PRIMARY KEY,
                workspace_id  TEXT    NOT NULL,
                status        TEXT    NOT NULL,
                title         TEXT    NOT NULL,
                body          TEXT    NOT NULL,
                version       INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS issues_by_workspace
              ON issues(workspace_id);
            "#,
        )
        .execute(&pool)
        .await?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl CrudSource for IssuesSource {
    type Id = String;
    type Row = Issue;
    type Draft = IssueDraft;

    async fn list(&self, _ctx: RequestContext, limit: usize) -> SyncResult<Vec<Issue>> {
        let rows = sqlx::query_as::<_, Issue>(
            "SELECT id, workspace_id, status, title, body, version
               FROM issues
               ORDER BY id
               LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| SyncError::backend(e.to_string()))?;
        Ok(rows)
    }

    async fn get(&self, _ctx: RequestContext, id: String) -> SyncResult<Option<Issue>> {
        let row = sqlx::query_as::<_, Issue>(
            "SELECT id, workspace_id, status, title, body, version
               FROM issues
              WHERE id = ?",
        )
        .bind(&id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| SyncError::backend(e.to_string()))?;
        Ok(row)
    }

    async fn create(
        &self,
        _ctx: RequestContext,
        id: String,
        draft: IssueDraft,
    ) -> SyncResult<Issue> {
        let issue = Issue {
            id: id.clone(),
            workspace_id: draft.workspace_id,
            status: draft.status,
            title: draft.title,
            body: draft.body,
            version: 1,
        };
        sqlx::query(
            "INSERT INTO issues (id, workspace_id, status, title, body, version)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&issue.id)
        .bind(&issue.workspace_id)
        .bind(serde_json::to_string(&issue.status).unwrap())
        .bind(&issue.title)
        .bind(&issue.body)
        .bind(issue.version as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| SyncError::backend(e.to_string()))?;
        Ok(issue)
    }

    async fn save(
        &self,
        _ctx: RequestContext,
        id: String,
        draft: IssueDraft,
        base_version: Option<RowVersion>,
    ) -> SyncResult<CrudWriteResult<Issue>> {
        // Optimistic-concurrency guard: if the client sent
        // `base_version`, fail the save if the stored version
        // doesn't match.
        let current = self.get(_ctx.clone(), id.clone()).await?
            .ok_or_else(|| SyncError::backend("missing issue"))?;
        if let Some(expected) = &base_version {
            if expected.as_str() != current.version.to_string() {
                return Ok(CrudWriteResult::stale(Some(current)));
            }
        }
        let next = Issue {
            id: current.id,
            workspace_id: draft.workspace_id,
            status: draft.status,
            title: draft.title,
            body: draft.body,
            version: current.version.saturating_add(1),
        };
        sqlx::query(
            "UPDATE issues
                SET workspace_id = ?, status = ?, title = ?, body = ?, version = ?
              WHERE id = ?",
        )
        .bind(&next.workspace_id)
        .bind(serde_json::to_string(&next.status).unwrap())
        .bind(&next.title)
        .bind(&next.body)
        .bind(next.version as i64)
        .bind(&next.id)
        .execute(&self.pool)
        .await
        .map_err(|e| SyncError::backend(e.to_string()))?;
        Ok(CrudWriteResult::applied(next))
    }

    async fn remove(
        &self,
        _ctx: RequestContext,
        id: String,
        _base_version: Option<RowVersion>,
    ) -> SyncResult<CrudRemoveResult<Issue>> {
        sqlx::query("DELETE FROM issues WHERE id = ?")
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(|e| SyncError::backend(e.to_string()))?;
        Ok(CrudRemoveResult::applied())
    }
}
```

Most of the work is your existing SQL knowledge. The Pocopine contract
adds: `RequestContext` (for tenant headers / auth), `CrudWriteResult`
(typed conflict outcomes), and `RowVersion` (the concurrency token).

For production-grade idempotency you'd pair this with
`.transactional(SqlxCrudTransactionRunner::new(pool), SqlxCrudMutationLog::new(scope_fn))`
so the row write and the accepted-mutation log insert share one DB
transaction. The basic `MemoryCrudMutationLog` is fine for tutorial
purposes.

## 4. Build the sync server with the `.params_of` bridge

```rust
// issues-server/src/main.rs
use std::sync::Arc;

use pocopine_events::{MemoryEventBackend, SharedEventBackend};
use pocopine_live::{routes, LiveHub};
use pocopine_server::{axum::Router, static_files, Server};
use pocopine_sync::{sync_server_plugin, SyncServer};
use pocopine_sync_crud::{resource, MemoryCrudMutationLog};

use issues::{Issue, STREAM};
use issues_server::source::IssuesSource;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    pocopine_logging::init_default().map_err(std::io::Error::other)?;

    // 1. Storage.
    let source = IssuesSource::new("sqlite://issues.db?mode=rwc")
        .await
        .map_err(std::io::Error::other)?;

    // 2. Sync server. The .params_of line is the bridge — it plugs
    //    the macro-generated typed projector into the CRUD adapter
    //    so RFC 088 §C per-(stream, params_hash) live wakeups route
    //    precisely.
    let backend: SharedEventBackend = Arc::new(MemoryEventBackend::new());
    let sync = SyncServer::builder()
        .events(backend.clone())
        .public_stream(
            resource(STREAM, source)
                .expect("stream name")
                .id(|r: &Issue| r.id.clone())
                .version(|r: &Issue| r.version)
                .mutation_log(MemoryCrudMutationLog::<Issue>::new())
                .params_of(issues::row_to_params_typed),
        )
        .build();

    // 3. Live hub. allow_topic_prefixes (NOT allow_topics) is
    //    critical — it authorizes both the bare `query:sync:stream:issues`
    //    topic AND every per-(stream, params_hash) extension.
    let topic_prefixes = sync.live_topic_prefixes();
    let default_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(backend)
        .allow_topic_prefixes(topic_prefixes)
        .default_topics(default_topics);

    // 4. Mount routes.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir));

    Server::new(router)
        .plugin(sync_server_plugin(sync))
        .serve("127.0.0.1:3021")
        .await
}
```

Three things to notice:

- `.params_of(issues::row_to_params_typed)` is the only Query-aware
  line. Drop it and you're back to bare-topic broadcasts — every
  subscriber wakes for every mutation.
- `MemoryEventBackend` is fine for single-process. For multi-node
  production use `RedisEventBackend::new(url, app)?` and put a Redis
  instance behind both processes. See
  [`docs/live.md`](./live.md) for the wire contract.
- `allow_topic_prefixes` (not `allow_topics`) is required for §C. The
  exact-match variant would silently reject every per-params
  subscription because the hashes are computed at runtime.

## 5. Wire the browser

The browser does three things: install the sync plugin with IndexedDB
storage, install the query client plugin, and register your components.

```rust
// issues-browser/src/main.rs
use pocopine::prelude::*;
use pocopine_sync::sync_plugin;
use pocopine_sync_indexdb::IndexedDbLocalStore;
use pocopine_sync_query::query_client_plugin;
use wasm_bindgen::prelude::*;

use issues_browser::components::IssueList;

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(
            sync_plugin()
                // Live wakeup over SSE; without this the client
                // polls on a timer.
                .with_live_wakeup(true)
                // Durable browser cache so reloads don't restart
                // from an empty snapshot.
                .local_store(IndexedDbLocalStore::new()),
        )
        // Query subscriptions + reactive views. Independent of CRUD;
        // installs the registry every component will pull from.
        .plugin(query_client_plugin())
        .register::<IssueList>()
        .run();
}
```

That's all the framework glue. Components access both plugins through
the standard `self.plugin::<T>()` accessor.

## 6. A component that subscribes + pushes

This is where CRUD and Query both show up at once. The component
carries **two** pieces of sync state side by side:

- `QueryView<Issue>` — the filtered, reactive **read** handle. This
  is what the template renders.
- `CollectionState<Issue>` — the **write-side bookkeeping** the
  `SyncClient` needs: local mutation queue, pending overlay, cursor
  for confirmation pulls. Doesn't drive the UI directly.

```
                         ┌─────────────────────────────┐
                         │       IssueList             │
                         │                             │
   reads ◄── view.rows() │  view:    QueryView<Issue> ─┼──► /sync/v1/pull
                         │                             │      filtered by
                         │                             │      workspace_id
                         │                             │
   writes ── push ─────► │  writes:  CollectionState   ─┼──► /sync/v1/push
                         │           <Issue>           │
                         └─────────────────────────────┘
                                  one canonical store on the wire;
                                  two client-side handles onto it
```

Why two fields? `QueryView` and `CollectionState` are independent
client-side caches today — `QueryView` subscribes through the query
driver (one pull per `(stream, params)`), `CollectionState` carries
the SyncClient's mutation queue. A future `#[resource]`-macro release
will collapse the write side into the same Query subscription; until
then the two coexist. The wire side is unified — both pull from one
canonical row store on the server.

```rust
// issues-browser/src/components/issue_list.rs
use pocopine::prelude::*;
use pocopine_sync::{ClientMutationDraft, CollectionState, SyncClient, SyncRow};
use pocopine_sync_query::{QueryClient, QueryView};
use serde::{Deserialize, Serialize};
use std::rc::Rc;

use pocopine_sync_crud::CrudMutationPayload;

use issues::{field, Issue, IssueDraft, Status, STREAM};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "issue_list.css")]
pub struct IssueList {
    workspace: String,
    draft_title: String,
    rows: Vec<Issue>,
    status: String,

    /// Write-side bookkeeping for the SyncClient: local mutation
    /// queue, pending overlay, cursor. Not rendered directly — the
    /// UI reads come from `view`.
    writes: CollectionState<Issue>,

    /// Filtered reactive read handle. Holds the §C subscription
    /// alive; drops on component teardown.
    #[serde(skip)]
    view: Option<Rc<QueryView<Issue>>>,
    next_local_id: u64,
}

#[handlers]
impl IssueList {
    pub fn on_mount(&mut self) {
        // Hardcode the workspace for the tutorial; in a real app
        // you'd read it from the URL / auth context / app state.
        self.workspace = "W1".to_string();
        self.open_writes();
        self.subscribe_reads();
    }

    /// Wire up the write-side: SyncClient opens the stream against
    /// our `writes` field, which gives us the local mutation queue
    /// and the cursor used to confirm pushes.
    fn open_writes(&mut self) {
        let result = self
            .plugin::<SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.writes)
            .stream(STREAM)
            .and_then(|collection| collection.open());
        if let Err(err) = result {
            self.status = format!("sync open failed: {err}");
        }
    }

    /// Wire up the read-side: a typed Query subscribed to this
    /// workspace, status ∈ {Open, InProgress}. The §C `partition_hash`
    /// (from `#[query_resource]`'s `partition_for_topic`) subscribes
    /// the SSE stream to `query:sync:stream:issues:<W1-hash>`.
    fn subscribe_reads(&mut self) {
        let client = self.plugin::<Rc<QueryClient>>().clone();

        // The typed DSL. `field::workspace_id` is a zero-sized
        // marker the macro emits; `.eq()` is compile-time checked
        // against the field's declared type (String here). Drop
        // the workspace_id filter and the macro-generated
        // `matches()` predicate rejects every row — required-field
        // gating prevents accidental cross-tenant leaks.
        let query = Issue::query()
            .eq(field::workspace_id, self.workspace.clone())
            .any_of(field::status, [Status::Open, Status::InProgress])
            .unwrap()
            .build();

        // .observe() returns a `QueryView<Row>` — the reactive
        // handle. Drop it and the subscription unregisters; we
        // park it on `self.view` to keep it alive.
        let view = Rc::new(client.observe(query));
        self.rows = view.rows();

        // Re-render whenever canonical or pending overlays change.
        // `view.rows()` returns the merged result (server-confirmed
        // + optimistic) every time it's called.
        let handle = pocopine::this::<Self>();
        let view_clone = view.clone();
        view.on_update(move || {
            handle.update(|this: &mut Self| {
                this.rows = view_clone.rows();
            });
        });

        self.view = Some(view);
        self.status = format!("subscribed to {STREAM} (workspace={})", self.workspace);
    }

    pub fn on_create_clicked(&mut self) {
        let title = std::mem::take(&mut self.draft_title);
        if title.trim().is_empty() {
            self.status = "title required".to_string();
            return;
        }

        self.next_local_id = self.next_local_id.saturating_add(1);
        let id = format!("issue_local_{}", self.next_local_id);
        let draft = IssueDraft {
            workspace_id: self.workspace.clone(),
            status: Status::Open,
            title: title.clone(),
            body: String::new(),
        };

        // Two pieces of mutation state to construct:
        //
        //   1. The wire envelope — `CrudMutationPayload::create(id,
        //      draft)`. The server's `CrudResource` decodes this
        //      shape and dispatches to `IssuesSource::create`. Note
        //      the payload is generic over `<Id, Draft>`, NOT `Row`
        //      — the row id and the editable fields, not the full
        //      stored row.
        //
        //   2. The optimistic row — a placeholder `Issue` that
        //      paints immediately while the server confirms. The
        //      version starts at 0; the server's `create` sets it
        //      to 1 and the canonical row replaces this one when
        //      the response arrives.
        let result = (|| -> pocopine_sync::SyncResult<()> {
            let mutation = CrudMutationPayload::create(id.clone(), draft)
                .into_sync_draft()?
                .key(id.clone())?;
            let optimistic = SyncRow::new(
                id.clone(),
                Issue {
                    id: id.clone(),
                    workspace_id: self.workspace.clone(),
                    status: Status::Open,
                    title,
                    body: String::new(),
                    version: 0,
                },
            )?;

            // Hand off to SyncClient.
            //
            //   1. The mutation lands in `writes`'s local queue
            //      (durable across reloads via IndexedDbLocalStore).
            //   2. The optimistic Issue lands in the pending overlay.
            //      The Query routing engine evaluates the macro-
            //      generated `matches()` predicate against the row
            //      — `workspace_id == "W1"` AND status ∈ {Open,
            //      InProgress} both hold, so `view.rows()` returns
            //      the new issue and the listener fires.
            //   3. POST /sync/v1/push runs in the background. The
            //      response either confirms (row moves from pending
            //      → canonical) or conflicts (handled by your
            //      `CrudSource::save`'s base_version logic).
            //   4. The server publishes to
            //      `query:sync:stream:issues:<W1-hash>`. Other tabs
            //      subscribed to this workspace wake and pull;
            //      tabs in W2 stay silent.
            //
            // `push_with_generated_id` has two generics: M (the
            // wire payload, here `CrudMutationPayload<String,
            // IssueDraft>`) and T (the optimistic row type, here
            // `Issue` matching `CollectionState<Issue>`).
            self.plugin::<SyncClient>()
                .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.writes)
                .stream(STREAM)
                .and_then(|c| c.push_with_generated_id(mutation, Some(optimistic)))
        })();

        if let Err(err) = result {
            self.status = format!("push failed: {err}");
        }
    }
}
```

The two fields don't double-store rows. `CollectionState<Issue>`
holds the local mutation queue + pending overlay for writes;
`QueryView<Issue>` holds the filtered subscription state for reads.
Both pull canonical rows from the same server-side store. The
"two handles, one truth" shape is the honest version of CRUD+Query
composition today — once the `#[resource]` macro lands, the write
field collapses into the same `QueryView` and you'll only see one.

The component file (`issue_list.poco`) is conventional Pocopine:

```html
<!-- issues-browser/src/components/issue_list.poco -->
<div>
  <header>
    <h2>Issues — {{workspace}}</h2>
    <p>{{status}}</p>
  </header>

  <form @submit.prevent="on_create_clicked">
    <input pp-model="draft_title" placeholder="New issue title" />
    <button type="submit">Create</button>
  </form>

  <ul>
    <li pp-for="issue in rows" :key="issue.id">
      <span class="status">{{issue.status}}</span>
      <span class="title">{{issue.title}}</span>
    </li>
  </ul>
</div>
```

## 7. Run it

In two terminals:

```bash
# Terminal 1: server
cargo run -p issues-server

# Terminal 2: browser dev server (Pocopine CLI)
pocopine dev --path issues-browser
```

Open `http://localhost:3000` (browser) — it talks to
`http://127.0.0.1:3021` (server). The first paint shows whatever's
already in `issues.db` (initially nothing). Type a title and click
"Create."

Expected behavior:

1. The new issue appears immediately (optimistic overlay).
2. The server accepts it; the row swaps from pending → canonical.
3. The server publishes to topic
   `query:sync:stream:issues:<W1-hash>` over SSE.
4. The browser's live wakeup fires `/pull` with a cursor; the canonical
   row gets confirmed (no UI change, since the optimistic value matched).

Open a **second tab** at the same URL. You'll see the issue appear in
both tabs after the live wakeup propagates.

## 8. Watch §C precision

Now the payoff. Change the workspace on one tab and observe that the
two tabs no longer interfere.

In Tab A, the component subscribes to `W1` (default). In Tab B, edit
`on_mount` to use `"W2"` instead, reload.

Tab B sees zero issues — correct, because no `W2` issues exist yet.
Now create an issue from Tab A (in `W1`). What happens:

- Tab A sees the new issue.
- Tab B does **not** wake. Its SSE subscription is to
  `query:sync:stream:issues:<W2-hash>`, and the server only publishes
  to `<W1-hash>` for this push.

This is the fanout-reduction §C delivers. Without `.params_of` (and
without the topic-prefix allowlist), Tab B would wake for every
mutation on the stream regardless of workspace, then `/pull` to
discover nothing changed for its filter. With §C, the server routes
the wakeup precisely.

You can verify this with the integration test pattern in
[`crates/pocopine-sync-crud/tests/params_partitioning.rs`](../crates/pocopine-sync-crud/tests/params_partitioning.rs)
— a non-UI assertion that the W2 SSE body stays silent for 200ms
after a W1 push.

## Reactive selectors (bonus)

`#[query]` is Query's memoization layer for derived computations.
Imagine you want "open issue count by workspace" as a piece of
reactive state.

```rust
use pocopine_sync_query::query;

#[query]
pub fn open_count(workspace: String) -> usize {
    let view = Issue::query()
        .eq(field::workspace_id, workspace)
        .eq(field::status, Status::Open)
        .observe(&Self::query_client());   // implicit; see the macro docs
    view.rows().len()
}
```

The selector re-runs only when the underlying view's `rows()` change.
A header component reading `open_count("W1".into())` gets a cached
number plus an `on_update` callback for free.

See
[`sync-query-selector-mechanism.md`](./sync-query-selector-mechanism.md)
for the full mechanics.

## What you didn't have to do

- Write a `SyncStreamSource` impl by hand — CRUD's adapter generates
  it from your `CrudSource`.
- Wire SSE topics manually — `LiveHub::allow_topic_prefixes(sync.live_topic_prefixes())`
  is one line.
- Compute `params_hash` on either side — the macro emits both halves
  (`row_to_params_typed` on server, `partition_for_topic` on client)
  and the wire contract pins they agree byte-for-byte.
- Manage the local cache — `IndexedDbLocalStore` handles hydration
  and persistence; the sync client uses it transparently.

## Where to go next

- **Production idempotency**: swap `MemoryCrudMutationLog` for
  `SqlxCrudMutationLog::new(scope_fn)` with a tenant-scoped log key
  so the same `MutationId` from two tenants doesn't collide. See
  [`docs/sync-crud.md`](./sync-crud.md) §6 ("Mutation Lifecycle").
- **Atomic writes**: chain `.transactional(runner, log)` to put the
  row write and log insert in one DB transaction. See
  [`docs/sync-sqlx.md`](./sync-sqlx.md).
- **Multi-node deployment**: switch the live backend from
  `MemoryEventBackend` to `RedisEventBackend::new(url, app)?`. See
  [`docs/live.md`](./live.md).
- **Schema evolution**: bump `schema_version = 2` and register
  `.migrate_with(|from, to, value| { … })` to transform stale-schema
  client payloads on the wire. See
  [`docs/sync-schema-versioning.md`](./sync-schema-versioning.md).
- **Selectors deep-dive**:
  [`docs/sync-query-selector-mechanism.md`](./sync-query-selector-mechanism.md).
