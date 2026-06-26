---
title: "Sync: build an issue tracker"
description: "Build a workspace-scoped issue tracker end to end with the query data layer, a Source impl, and typed writes."
---

# Sync: build an issue tracker

End-to-end tutorial for `pocopine-sync-query`. By the end you have a
reactive, filtered, mutation-routed issue tracker scoped per workspace
with optimistic writes and live wake-ups — in roughly 40 lines of
component code.

This doc reads top-to-bottom and assumes nothing. Server contract
detail in [`sync-server.md`](../guides/data/sync-server.md); client runtime detail
in [`sync-client.md`](../guides/data/sync-client.md).

## What you build

```text
   server                                    wasm (browser)
   ──────                                    ──────────────
   Source<Issue>                             #[component] IssueList
      │ list_stream / create / update         │
      ▼                                      Issue::query()
   SourceResource ─── /pull / /push ────▶     .eq(field::workspace_id, …)
      │ live SSE per (stream, params_hash)    .bind(&qc, |s| &mut s.rows)
      ▼                                      │
   server plugin                              ▼
                                            ◀── pp-for="row in rows"

                          Issue::create(id, draft)
                              .optimistic(|p| …)
                              .push(&qc).await?
```

Three pieces share one row type:

| Layer       | Crate                       | Role                                              |
|-------------|-----------------------------|---------------------------------------------------|
| Server      | `pocopine-sync-query` host  | `Source` trait → `SourceResource` → server plugin |
| Wire        | `pocopine-sync`             | `/sync/v1/{open,pull,push}`, SSE wake-ups         |
| Client      | `pocopine-sync-query` wasm  | `QueryClient` plugin + `Query<Row>` DSL           |

## Step 1 — Shared row + draft

One file, used by both ends.

```rust
// crates/myapp-shared/src/issue.rs
use pocopine_sync_query::query_resource;
use serde::{Deserialize, Serialize};

#[query_resource(name = "issues", schema_version = 1, draft = IssueDraft)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub version: String,
    pub created_at: String,

    #[query_param(required)]
    pub workspace_id: String,

    #[query_param]
    pub status: String,

    #[query_param]
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueDraft {
    pub workspace_id: String,
    pub status: String,
    pub title: String,
}

// One-time conversion that the macro-emitted `Issue::create` /
// `Issue::update` use as the default optimistic overlay. Server-
// controlled fields (version, created_at) get sensible defaults;
// the rest comes from the draft. Override per-call with
// `.optimistic(custom)` or opt out with `.server_only()`.
impl From<(String, IssueDraft)> for Issue {
    fn from((id, draft): (String, IssueDraft)) -> Self {
        Self {
            id,
            version: String::new(),
            created_at: String::new(),
            workspace_id: draft.workspace_id,
            status: draft.status,
            title: draft.title,
        }
    }
}
```

`#[query_resource]` emits:

```rust
impl Issue {
    pub fn query() -> QueryBuilder<Self>    // DSL entry point
    // create / update / delete are emitted because `draft = IssueDraft` was declared:
    pub fn create(id: String, draft: IssueDraft) -> TypedMutation<Self, String, IssueDraft>
    pub fn update(id: String, draft: IssueDraft, expected_version: Option<RowVersion>)
        -> TypedMutation<Self, String, IssueDraft>
    pub fn delete(id: String, expected_version: Option<RowVersion>)
        -> TypedMutation<Self, String, IssueDraft>
}

pub mod issues {                             // module name = resource name
    pub const NAME: &str = "issues";
    pub const SCHEMA_VERSION: u32 = 1;
    pub const HAS_PER_PARAMS_LIVE_ROUTING: bool = true; // has a required field
    pub fn resource<S>(impl_: S) -> SourceResource<S, …>  // auto-wires id / version / partition_by
        where S: Source<Row = Issue, Id = String>;
    pub fn row_to_params(row: &Value) -> SyncResult<StreamParams>; // server §C projector
    pub fn row_to_params_typed(row: &Issue) -> StreamParams;       // typed sibling
    pub fn partition_for_topic(captured: &StreamParams) -> Option<u64>; // client subscribe hash
    pub fn matches(query: &Query<Issue>, row: &Issue) -> bool;     // predicate evaluator
    pub mod field { /* one typed marker per #[query_param]-annotated field */ }
}
```

`#[query_resource]` must come before `#[derive(...)]` so it strips
the per-field `#[query_param]` annotations before downstream derives
see the struct.

`#[query_param(required)]` on `workspace_id` makes it a **tenant gate**:
predicates without it are rejected, and every subscription is
partitioned per-value for precise live wake-ups (W1 mutations don't
wake W2 subscribers). Bare `#[query_param]` is filterable but optional.

## Step 2 — Server: `Source` + mount

Three things in one file: a typed request context, a `Source` impl,
and the mount.

### Typed request context

The `Source` trait's `type Context` lets you extract everything you
need from a request once, then operate on a strongly-typed handle
inside every storage method. Skip `RequestContext` plumbing entirely.

```rust
// crates/myapp-server/src/issues.rs
use pocopine_auth::RequestAuthExt;

#[derive(Clone)]
pub struct WorkspaceCtx {
    pub workspace_id: String,
    pub user_id: String,
}

impl WorkspaceCtx {
    fn extract(ctx: &RequestContext, db: &Db) -> SyncResult<Self> {
        let user_id = ctx.require_user()
            .map_err(|_| SyncError::unauthorized("unauthenticated"))?
            .id.clone();
        let workspace_id = ctx.header("x-workspace-id")
            .ok_or_else(|| SyncError::client("missing x-workspace-id"))?
            .to_string();
        // Real impl would call db.assert_member(&user_id, &workspace_id) here.
        Ok(Self { workspace_id, user_id })
    }
}
```

### The `Source` impl

```rust
use pocopine_sync_query::source::{
    DeleteResult, Source, SourceFuture, SourceStream, WriteResult,
};

pub struct IssueStore { pub db: Db }

impl Source for IssueStore {
    type Id = String;
    type Row = Issue;
    type Draft = myapp_shared::issue::IssueDraft;
    type Context = WorkspaceCtx;

    fn extract_context<'a>(
        &'a self,
        ctx: RequestContext,
    ) -> SourceFuture<'a, SyncResult<Self::Context>> {
        let db = self.db.clone();
        Box::pin(async move { WorkspaceCtx::extract(&ctx, &db) })
    }

    fn list_stream<'a>(
        &'a self,
        ctx: WorkspaceCtx,
        query: &'a Query<Issue>,
    ) -> SourceStream<'a, Issue> {
        // `query.params()` exposes typed filters; push them into SQL.
        // `query.limit()` is already clamped by the adapter.
        Box::pin(self.db.fetch_issues(ctx, query.params(), query.limit()))
    }

    fn get<'a>(&'a self, ctx: WorkspaceCtx, id: String)
        -> SourceFuture<'a, SyncResult<Option<Issue>>>
    {
        let db = self.db.clone();
        Box::pin(async move { db.get_issue(&ctx, &id).await })
    }

    fn create<'a>(&'a self, ctx: WorkspaceCtx, id: String, draft: IssueDraft)
        -> SourceFuture<'a, SyncResult<Issue>>
    {
        let db = self.db.clone();
        Box::pin(async move { db.insert_issue(&ctx, id, draft).await })
    }

    fn update<'a>(
        &'a self, ctx: WorkspaceCtx,
        id: String, draft: IssueDraft,
        expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Issue>>> {
        let db = self.db.clone();
        Box::pin(async move { db.update_issue(&ctx, id, draft, expected_version).await })
    }

    fn delete<'a>(
        &'a self, ctx: WorkspaceCtx,
        id: String, expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Issue>>> {
        let db = self.db.clone();
        Box::pin(async move { db.delete_issue(&ctx, id, expected_version).await })
    }
}
```

The `Source::Context` extractor runs once per request. Auth failures
in `extract_context` short-circuit before any storage method runs.

### Mount on the `Server`

```rust
use myapp_shared::issue::issues;
use pocopine_server::Server;
use pocopine_sync::{sync_server_plugin, SyncServer};

let sync = SyncServer::builder()
    .public_stream(
        issues::resource(IssueStore { db })
            .mutation_log(MemoryMutationLog::<Issue>::with_scope_fn(|ctx| {
                ctx.header("x-workspace-id")
                    .map(|s| s.to_string())
                    .ok_or_else(|| SyncError::unauthorized("missing x-workspace-id"))
            })),
    )
    .build();

let router = axum::Router::new(); // add your other routes here
let server = Server::new(router)
    .plugin(sync_server_plugin(sync));
```

`issues::resource(impl_)` is the macro-emitted convenience that
pre-wires `.id` (from the row's `id` field), `.version_field` (from
the row's `version` field), and `.partition_by` (from
`row_to_params_typed`). Override any default by chaining the matching
builder method on the result (e.g. `.id(custom_projector)` to use a
non-`id`-named field).

`.mutation_log(...)` is technically optional — omit it and the
builder defaults to an in-memory log + emits a one-shot
`tracing::warn!`. Don't omit it in production.

## Step 3 — Client: install + render

The plugin gives every `#[component]` access to one shared
`QueryClient`:

```rust
// crates/myapp-wasm/src/lib.rs
use pocopine::prelude::*;
use pocopine_sync_query::query_client_plugin;

#[wasm_bindgen(start)]
pub fn start() {
    App::new()
        .register::<IssueList>()
        .register::<IssueComposer>()
        .plugin(query_client_plugin())
        .run();
}
```

A list component subscribes in one chain:

```rust
// crates/myapp-wasm/src/IssueList.rs
use std::rc::Rc;
use pocopine::prelude::*;
use pocopine_sync_query::{Order, QueryClient};
use myapp_shared::issue::{Issue, issues};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "issue_list.css")]
pub struct IssueList {
    #[prop] pub workspace_id: String,
    pub rows: Vec<Issue>,
}

#[handlers]
impl IssueList {
    pub fn on_mount(&mut self) {
        let qc = self.plugin::<Rc<QueryClient>>();
        Issue::query()
            .eq(issues::field::workspace_id, &self.workspace_id)
            .eq(issues::field::status, "open")
            .order_by_raw("created_at", Order::Desc)
            .limit(50)
            .bind::<Self, _>(&qc, |s: &mut Self| &mut s.rows);
    }
}
```

```html
<!-- IssueList.poco -->
<section class="issues">
  <header>
    <h1>Issues — {{ workspace_id }}</h1>
    <span class="count" pp-text="rows.length"></span>
  </header>

  <ol class="issues__list" pp-show="rows.length">
    <template pp-for="row in rows" pp-key="row.id">
      <li class="issue">
        <h2 pp-text="row.title"></h2>
        <span class="status" pp-text="row.status"></span>
      </li>
    </template>
  </ol>
</section>
```

`.bind(&qc, projector)` does the whole bridge: observe the query,
collect rows into a `Signal`, run an effect-scoped sync that writes
into the component field, tear everything down on unmount. Field
markers (`issues::field::workspace_id`) are typed: passing the wrong
shape is a build error.

## Step 4 — Client: typed write

Same component pattern; one chained call for the whole optimistic
+ wire push.

```rust
// crates/myapp-wasm/src/IssueComposer.rs
use std::rc::Rc;
use pocopine::prelude::*;
use pocopine_sync_query::QueryClient;
use myapp_shared::issue::{Issue, IssueDraft};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct IssueComposer {
    #[prop] pub workspace_id: String,
    pub title: String,
    pub saving: bool,
    pub error: String,
}

#[handlers]
impl IssueComposer {
    pub async fn create(&mut self) {
        if self.title.trim().is_empty() { return; }
        self.saving = true;
        self.error.clear();

        let qc = self.plugin::<Rc<QueryClient>>();
        let row_id = format!("iss_{}", uuid::Uuid::now_v7());
        let draft = IssueDraft {
            workspace_id: self.workspace_id.clone(),
            status: "open".into(),
            title: std::mem::take(&mut self.title),
        };

        match Issue::create(row_id, draft).push(&qc).await {
            Ok(_mutation_id) => self.saving = false,
            Err(err) => { self.saving = false; self.error = err.to_string(); }
        }
    }
}
```

`Issue::create(id, draft).push(&qc)` reads the `From<(String,
IssueDraft)> for Issue` impl declared back in Step 1 to build the
optimistic Row automatically. No per-call closure. Override with
`.optimistic(custom)` before `.push(&qc)` when a write needs a
non-default overlay (server-side computed fields you predict
explicitly); skip the overlay entirely with `.server_only()` for
"only render after server confirms" writes.

```html
<!-- IssueComposer.poco -->
<form class="composer" pp-on:submit.prevent="create">
  <label><span>Title</span><input pp-model="title" autocomplete="off" /></label>
  <p pp-show="error" class="error" pp-text="error"></p>
  <button type="submit" pp-show="!saving">Create</button>
  <button type="button" disabled pp-show="saving">Working…</button>
</form>
```

What `.push(&qc)` does:

```text
1. Builds the optimistic Issue via Self::from((id, draft))
   (the impl from Step 1).
2. Routes it through every active QueryView whose predicate matches
   (workspace W1 + status="open"); each view's bound component
   re-renders the new row.
3. POSTs /sync/v1/push with the typed envelope. Stream name +
   push URL come from the macro + QueryClient endpoint; the
   MutationId is a fresh UUIDv7.
4. Server: extract_context → reserve_mutation → Source::create.
5. Accepted → overlay stays until next /pull replaces it with
   canonical. Rejected → overlay rolls back, on_update fires,
   caller gets Err(SyncError).
6. Live SSE fans the canonical row out to other clients on
   (issues, W1-hash).
```

For retries that need to collapse to the same logical write (durable
counter, deterministic test id), use `.push_with_id(&qc, id)` instead
and own the id yourself.

## Step 5 — Live wake-ups

Server publishes to topics shaped by the `row_to_params_typed`
projector that `issues::resource(...)` auto-wired into
`.partition_by(...)` in Step 2. With `workspace_id` as the required
param, every accepted mutation on W1 wakes only W1 subscribers — W2
clients stay silent.

Client side: nothing to do. `query_client_plugin()` opens one SSE
stream per `(stream, params_hash)` topic and re-pulls the matching
subscription on each event.

Disable for offline-only flows or tests:

```rust
use pocopine_sync_query::QueryClientConfig;
use std::time::Duration;

app.plugin(
    query_client_plugin().config(QueryClientConfig {
        disable_live: true,
        poll_interval: Some(Duration::from_secs(5)),
        ..Default::default()
    })
);
```

## Where to next

- **Server contract:** [`sync-server.md`](../guides/data/sync-server.md) — full
  `Source` trait, `SourceResource` builder, `MutationLog` invariants,
  `partition_by`, schema migration ordering.
- **Client API:** [`sync-client.md`](../guides/data/sync-client.md) — `Query<Row>`
  DSL reference, `QueryView` raw surface, manual signal bridge,
  pending overlay vs canonical.
- **Selectors:**
  [`sync-query-selector-mechanism.md`](../internal/sync-query-selector-mechanism.md) —
  derived queries with the `#[query]` macro.
