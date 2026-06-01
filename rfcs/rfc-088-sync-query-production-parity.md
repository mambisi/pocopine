# RFC 088 — `pocopine-sync-query` production parity

* **Status:** Implemented
* **Author:** sync framework working group
* **Tracking branches:** `wip/sync-query-persistence` (Section A), `wip/sync-query-selectors` (Section B), `wip/sync-query-per-params-topics` (Section C)
* **Supersedes:** the "Non-goals" line items in RFC 087 §"Non-goals" — this RFC closes those gaps.
* **Related:** [RFC 086 (pocopine-sync-query)](./rfc-086-sync-query.md), [RFC 087 (driver lifecycle)](./rfc-087-sync-query-driver.md), [RFC 071 (event spine)](./rfc-071-event-spine.md), [RFC 072 (offline sync protocol)](./rfc-072-offline-sync.md)

## Summary

PR #148 (RFC 087) made `pocopine-sync-query` actually sync — `client.observe(q)` now drives `/open` + `/pull` + live wakeup + offline replay end-to-end. What's left for production parity with `pocopine-sync-crud`:

* **A. Durable persistence across page reloads.** Subscription state is in-memory; a refresh wipes canonical rows + pending overlays. CRUD already has IndexedDB + SQLite implementations behind the `SyncLocalStore` trait — sync-query should reuse them.
* **B. `#[query]` selector composition.** The trait-gated DSL is conjunctive per resource. Joins, OR queries, and derived views (`projects_with_open_issues(workspace_id)`) require dropping into raw Rust without read tracking. A function-decorator pattern — Replicache/Zero-style — makes composition first-class without bloating the DSL.
* **C. Per-`(name, params_hash)` live topics.** Today every subscriber on a stream wakes up on every mutation; the client filters by params and decides whether to `/pull`. For high-fanout multi-tenant apps that's wasted bandwidth — the server should publish each invalidation to the specific params-hash topic so only the right subscribers wake.

This RFC consolidates the three under one document; each ships as its own PR (Section A first, B second, C third). All three are additive; old apps keep working unchanged.

## Motivation

Post-RFC-087, sync-query reaches "real framework" status — declared queries observe live data with reactive views. What stops it from being a production-grade drop-in for a Replicache/Zero/PowerSync-class app:

1. **First-paint UX.** Without persistence, every page load shows empty views for ~100ms while `/open` + `/pull` complete. CRUD users have IndexedDB-backed instant render today; sync-query users don't.
2. **Composition cliff.** A single resource is queryable through the trait DSL; joins and derived views are not. Every team that builds a Linear-clone hits this within the first day.
3. **Bandwidth amplification under multi-tenancy.** With N workspaces and M subscribers per workspace, a single workspace-scoped mutation wakes up every subscriber on the stream (M × N people), only M of whom care. At Linear/Slack scale that's measurably bad.

These three sit in the long tail because the routing engine + macro DSL + driver had to land first. Now they do.

## Goals

* **(A)** `SyncLocalStore`-backed persistence: hydrate canonical rows + pending overlays + cursor + schema_version on subscribe; persist after every `/pull` and every optimistic apply. Reuse existing IndexedDB / SQLite impls.
* **(B)** `#[query] fn name(args) -> T` produces a `name::observe(client, args) -> SelectorView<T>`. Reads from `Resource::query()` views inside the function body are tracked; output is cached keyed by `(fn_id, args_hash)`; upstream mutations re-run the function and fire `on_update` only when the output differs.
* **(C)** `SyncStreamSource::row_to_params` server-side hook (auto-derived from `#[query_param]` declarations) routes invalidations to per-`(name, params_hash)` topics. Client subscribes to both bare and per-params topics during the rollout window; server publishes to both.

## Non-goals

* **CRDT / merge model.** Server stays authoritative; conflict resolution via failed `apply_remote` + the existing rollback path.
* **Cross-resource transactions.** Mutators stay single-resource. Multi-resource workflows compose via separate `mutate()` calls.
* **Per-field invalidation projection beyond `row_to_params`.** Server publishes per-(name, params_hash), not per-(name, params_hash, field). Field-level deltas remain a future RFC if we have data showing the extra bandwidth saves matter.
* **`#[query]` async functions.** Read-tracking is thread-local and synchronous. Async selectors compose by calling the sync selector inside an async block.
* **P2P sync.** Future work.

---

## Section A — Durable persistence

### Design

#### A.1 Storage interface

Reuse `pocopine_sync::SyncLocalStore`:

```rust
// crates/pocopine-sync/src/local_store.rs (existing)
pub trait SyncLocalStore {
    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot>;
    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()>;
    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()>;
    fn enqueue_pending_mutation(&self, stream: &SyncStreamName, pending: LocalPendingMutation)
        -> SyncLocalFuture<'_, ()>;
    fn pending_mutations(&self, stream: &SyncStreamName)
        -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>>;
    fn purge_pending_for_row(&self, stream: &SyncStreamName, key: &RowKey)
        -> SyncLocalFuture<'_, usize>;
    fn clear_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, ()>;
    fn clear_all_streams(&self) -> SyncLocalFuture<'_, ()>;
    // … identity + mutation_id methods elided
}
```

`SyncLocalFuture` is `Pin<Box<dyn Future>>` on wasm and `Pin<Box<dyn Future>>` (a ready future) on native. The sync-query driver already runs in an async context so the boxed future is consumed naturally.

#### A.2 Compartment keying

Each `(stream, params)` pair gets its own logical store compartment, keyed by:

```rust
// crates/pocopine-sync/src/protocol.rs (existing)
pub fn local_stream_key(stream: &SyncStreamName, params: &StreamParams) -> SyncStreamName;
// "issues" + {workspace_id: "W1"} → "issues__params_<fnv1a64hash>"
// "issues" + {} → "issues" (backwards compat with CRUD)
```

Sync-query uses this verbatim. Two subscriptions on the same stream with different params get distinct stores; the same params on the same stream get one shared store (refcount-shared subscription already enforces a single canonical state, so the store layer just mirrors that).

#### A.3 Driver lifecycle changes

`SubscriptionDriver::run` (from RFC 087) gains a Phase 0 before `/open`:

```rust
// crates/pocopine-sync-query/src/driver.rs
async fn run(self) {
    // Phase 0: hydrate from store (NEW)
    if let Some(store) = &self.local_store {
        let compartment = local_stream_key(stream, params);
        match store.hydrate_stream(&compartment).await {
            Ok(snapshot) => apply_hydrated_snapshot(&sub, snapshot),
            Err(_) => { /* fresh start; log + continue */ }
        }
    }

    // Phase 1: /open (unchanged)
    // … schema-drift check now also calls store.clear_stream() on mismatch
    // Phase 2-4: /pull + live wakeup + replay (unchanged)

    // After every successful /pull (NEW):
    if let Some(store) = &self.local_store {
        store.save_snapshot(snapshot_from_state(&sub)).await;
    }
}
```

`apply_hydrated_snapshot` populates `QueryState::canonical_rows`, `state.pending` (from snapshot's pending_mutations + replay closure re-derivation, see A.5), `state.cursor`, and `state.application_schema_version`. Bumps version once; observers re-render.

#### A.4 Schema-drift on hydrate

```rust
// Inside the driver's apply_open path:
let server_version = open_response.schema_version;
match state.application_schema_version {
    Some(local_version) if local_version != server_version => {
        // Drift: wipe persisted state + in-memory state.
        if let Some(store) = &self.local_store {
            store.clear_stream(&compartment).await.ok();
        }
        state.reset();
        state.application_schema_version = Some(server_version);
        // Continue with a fresh /pull.
    }
    _ => state.application_schema_version = Some(server_version),
}
```

Same logic CRUD uses in `start_open_then_pull`; reused unchanged.

#### A.5 Pending mutation replay on hydrate

The trickiest piece: `PendingOverlay` carries an `optimistic_row` + a `mutation: ClientMutation<Value>` (the wire payload). It does NOT carry the `apply_remote` closure — that's a `Box<dyn Fn>` and isn't serializable. So on hydrate we have wire payloads but no closures.

Resolution: introduce a `MutatorRegistry` on `QueryClient`:

```rust
// crates/pocopine-sync-query/src/client.rs
pub struct QueryClientInner {
    // … existing fields
    mutators: RefCell<HashMap<TypeId, Box<dyn AnyMutator>>>,
}

impl QueryClient {
    pub fn register_mutator<M: Mutator + 'static>(&self) {
        self.inner.mutators.borrow_mut()
            .insert(TypeId::of::<M>(), Box::new(MutatorEntry::<M>::new()));
    }
}
```

Mutators self-register at construction time (or via an `App::plugin` hook). On hydrate, the driver iterates persisted pending mutations, looks up the matching Mutator by `STREAM` + payload shape (mutator name carried on `ClientMutation`), and re-derives the replay invocation. If a mutator's code has been removed since the user last opened the app, that pending mutation is dropped + logged.

#### A.6 Configuration

```rust
// crates/pocopine-sync-query/src/driver.rs (extending QueryClientConfig)
pub struct QueryClientConfig {
    // … existing fields
    pub local_store: Option<Rc<dyn SyncLocalStore>>,
}

impl QueryClientConfig {
    pub fn with_local_store(mut self, store: Rc<dyn SyncLocalStore>) -> Self {
        self.local_store = Some(store);
        self
    }
}

// User code:
let store = IndexedDbLocalStore::open("my-app").await?;
let client = QueryClient::with_config(
    QueryClientConfig::default()
        .with_local_store(Rc::new(store))
);
```

Default is `None` → in-memory only (matches current behavior; no migration burden).

#### A.7 Backwards compat

Existing apps that don't configure a store keep working — `local_store: None` means the driver skips Phase 0 + the save calls. The first opt-in to persistence is a one-line change.

CRUD apps that share a `SyncLocalStore` instance with a sync-query client get isolated compartments because sync-query keys on `local_stream_key(stream, params)` and CRUD keys on `stream`. The two don't collide.

#### A.8 Verification (PR-A)

* `cargo test -p pocopine-sync-query --test persistence` — hydrate-then-observe roundtrip via a `MemorySyncLocalStore` test impl.
* Schema-drift: store has v1 snapshot; `/open` returns v2; verify `clear_stream` + canonical rebuilt.
* Pending replay: persist a pending mutation; simulate restart; verify the mutator re-fires on first online tick.
* Multi-compartment: two subscriptions on the same stream with different params persist into distinct compartments.

---

## Section B — `#[query]` selector composition

### Design

#### B.1 User-facing API

```rust
#[query]
fn projects_with_open_issues(workspace_id: String) -> Vec<(Project, Vec<Issue>)> {
    Project::query()
        .eq(field::workspace_id, &workspace_id).rows()
        .into_iter()
        .map(|p| {
            let issues = Issue::query()
                .eq(field::project_id, &p.id)
                .any_of(field::status, [Status::Open]).rows();
            (p, issues)
        })
        .collect()
}

// Call site — mirrors `Issue::query().observe(&client)`:
let view = projects_with_open_issues::observe(&client, "W1".to_string());

view.value();        // Vec<(Project, Vec<Issue>)>
view.on_update(|| render());  // fires when output differs from cached
```

Args must be `Hash + Eq + Clone + 'static`. Output must be `Clone + PartialEq + 'static`. Selectors are sync (`fn`, not `async fn`).

#### B.2 Read tracking

A thread-local stack of "currently-running selectors":

```rust
// crates/pocopine-sync-query/src/selector.rs
thread_local! {
    static CURRENT_SELECTOR: RefCell<Vec<SelectorId>> = RefCell::new(Vec::new());
}

pub(crate) fn record_read(subscription: Rc<dyn AnyQuerySubscription>) {
    CURRENT_SELECTOR.with(|stack| {
        if let Some(selector_id) = stack.borrow().last().copied() {
            // Push `subscription` into the selector's tracked-set.
            with_selector(selector_id, |entry| entry.track(subscription));
        }
    });
}
```

`QueryView::rows()` calls `record_read(self.subscription.clone() as Rc<dyn AnyQuerySubscription>)` before returning. Reads outside a selector context are no-ops.

The thread-local is a STACK (not a Cell) so nested selectors work: selector A calls selector B; B's reads register with B, not A. This is a deliberate departure from `pocopine_core::reactive::CURRENT_EFFECT` (which is a Cell); the selectors module owns its own thread-local because the semantics differ.

#### B.3 Cache & invalidation

```rust
struct SelectorEntry<T> {
    fn_id: SelectorId,                  // FNV-1a(module_path + fn_name)
    args_hash: ArgsHash,
    last_output: RefCell<Option<T>>,    // PartialEq diffing
    tracked: RefCell<Vec<Rc<dyn AnyQuerySubscription>>>,
    refcount: Cell<usize>,
    listeners: RefCell<Vec<(u64, Rc<dyn Fn()>)>>,
    listener_token_seq: Cell<u64>,
}

// Per-client registry, parallel to the existing subscription registry:
struct QueryClientInner {
    // … existing fields
    selectors: RefCell<HashMap<(SelectorId, ArgsHash), Rc<dyn AnySelector>>>,
}
```

`observe(client, args)` flow:

1. Hash args → `args_hash`.
2. If `selectors[(fn_id, args_hash)]` exists, bump refcount + return a `SelectorView<T>` wrapping it.
3. Else: push `fn_id` onto the read-tracking stack, run the user function, pop. Capture the tracked subscriptions + register `on_update` callbacks on each.
4. Store the entry, return the view.

`on_update` callback (registered on each tracked subscription) fires when ANY upstream changes. The callback:

1. Re-pushes `fn_id` onto the stack, re-runs the function (capturing a NEW tracked set), pops.
2. Compares the new tracked set against the cached one. Subscriptions no longer tracked have their `QueryHandle` dropped; new ones are subscribed.
3. Diffs `new_output == cached_output`. If different, updates cache + fires the selector's own listeners.

#### B.4 Refcount lifecycle

`SelectorView<T>` holds an `Rc<SelectorEntry<T>>`. `Drop` decrements the selector entry's refcount; on zero, the entry is removed from the registry, dropping its tracked `QueryHandle`s, which in turn release the upstream subscriptions per the existing refcount path.

#### B.5 Output diffing opt-out

```rust
#[query(no_diff)]
fn raw_view(...) -> NonComparableOutput { … }
```

Without `no_diff`, the macro emits a compile error if `T` doesn't impl `PartialEq`. With `no_diff`, every re-run fires `on_update` (caller's responsibility to debounce).

#### B.6 Macro emission

```rust
// User input:
#[query]
fn projects_with_open_issues(workspace_id: String) -> Vec<(Project, Vec<Issue>)> { … }

// Macro emits:
pub mod projects_with_open_issues {
    use super::*;

    #[derive(Hash, Eq, PartialEq, Clone)]
    pub struct Args {
        pub workspace_id: String,
    }

    pub fn observe(
        client: &::pocopine_sync_query::QueryClient,
        workspace_id: String,
    ) -> ::pocopine_sync_query::SelectorView<Vec<(Project, Vec<Issue>)>> {
        let args = Args { workspace_id };
        client.observe_selector(
            ::pocopine_sync_query::selector::selector_id_for(module_path!(), "projects_with_open_issues"),
            args.clone(),
            move || super::projects_with_open_issues(args.workspace_id.clone()),
        )
    }
}

// User function `fn projects_with_open_issues(...)` is emitted verbatim.
```

#### B.7 Verification (PR-B)

* `cargo test -p pocopine-sync-query-macros --test selector`:
    - Cache hit on repeat observe with same args (verify a single underlying subscription runs once).
    - Upstream mutation triggers re-run; output differs → `on_update` fires.
    - Equal-output re-run is suppressed (diff layer).
    - Drop the SelectorView; upstream `client.active_subscription_count()` decrements.
    - Compile-fail trybuild for non-`PartialEq` output without `no_diff`.
* Manual: build a small example using nested selectors (A reads B), verify B's reads register with B and not A.

---

## Section C — Per-`(name, params_hash)` live topics

### Design

#### C.1 Topic format

Existing: `query:sync:stream:{stream}` (carried by `sync_stream_tag(stream)`).

New: `query:sync:stream:{stream}:{params_hash}` where `params_hash` is the FNV-1a 64-bit hash of canonical sorted-by-key `StreamParams` JSON — same hash function used by `local_stream_key`.

```rust
// crates/pocopine-sync/src/protocol.rs (new)
pub fn sync_stream_params_topic(stream: &str, params_hash: u64) -> String {
    format!("sync:stream:{stream}:{params_hash:016x}")
}
```

#### C.2 Server-side hook

```rust
// crates/pocopine-sync/src/server.rs (extends SyncStreamSource)
pub trait SyncStreamSource: Send + Sync {
    // … existing methods

    /// Project a row payload into the StreamParams that identify which
    /// subscribers care about it. Default: empty (backwards compat).
    /// Macro auto-emits an impl from #[query_param] / params(...).
    fn row_to_params(&self, _row: &serde_json::Value) -> SyncResult<StreamParams> {
        Ok(StreamParams::new())
    }
}
```

The macros (`#[query_resource]` and `#[resource]`) auto-emit `row_to_params` from the params declarations:

```rust
// Macro-emitted (sketch):
fn row_to_params(row: &Value) -> SyncResult<StreamParams> {
    let mut params = StreamParams::new();
    // For each #[query_param(required)] field of type T:
    let workspace_id: String = serde_json::from_value(row["workspace_id"].clone())?;
    params.insert("workspace_id".into(), json!(workspace_id));
    // For each #[query_param] field with inner type T (Eq + InSet capable):
    let status: Status = serde_json::from_value(row["status"].clone())?;
    params.insert("status".into(), json!(status));
    // Contains-only fields (title) are SKIPPED — substring filters
    // don't partition the topic space.
    Ok(params)
}
```

#### C.3 Publish path

```rust
// crates/pocopine-sync/src/server.rs
impl SyncServer {
    pub async fn invalidate_stream_with_row(
        &self,
        stream: &str,
        row: &serde_json::Value,
    ) -> SyncResult<()> {
        let registered = self.stream(stream)?;
        let bare_tag = sync_stream_tag(stream);

        // Publish to bare topic (backwards compat — clients that haven't
        // opted in continue to receive every event).
        let bare_topic = pocopine_live::query_tag_topic(&bare_tag)?;
        events.publish(pocopine_live::query_invalidated(bare_topic, [bare_tag.clone()])).await?;

        // Compute per-row params + publish to per-params topic.
        let params = registered.source.row_to_params(row)?;
        if !params.is_empty() {
            let hash = stream_params_hash(&params);
            let tag = sync_stream_params_topic(stream, hash);
            let topic = pocopine_live::query_tag_topic(&tag)?;
            events.publish(pocopine_live::query_invalidated(topic, [tag])).await?;
        }
        Ok(())
    }
}
```

The CRUD `/push` handler at `crates/pocopine-sync/src/server.rs:576-584` calls `invalidate_stream_with_row` per accepted row instead of the existing `invalidate_stream(stream_name)`. Old non-CRUD sources that don't override `row_to_params` get the bare-topic publish only.

#### C.4 Client subscribe

`SubscriptionDriver::open_live_wakeup` (RFC 087 §6) subscribes to both topics:

```rust
let params_hash = stream_params_hash(query.params());
let live = pocopine_live::LiveClient::new()
    .query_tag(sync_stream_tag(stream))                          // bare topic
    .query_tag_with_params(sync_stream_tag(stream), params_hash) // per-params topic
    .with_credentials(with_credentials)
    .on_event(/* … */)
    .open();
```

The driver dedupes events by `mutation_id` (if the server publishes the same invalidation to both topics, the driver sees one wakeup, not two). Client-side params filtering (RFC 087 §6) stays in place as the final gate — for the rollout window, bare-topic events still arrive and need filtering.

#### C.5 `LiveClient` API

```rust
// crates/pocopine-live/src/lib.rs (new)
impl LiveClient {
    pub fn query_tag_with_params(mut self, tag: impl Into<String>, params_hash: u64) -> Self {
        self.topics.push(format!("query:{}:{:016x}", tag.into(), params_hash));
        self
    }
}
```

#### C.6 Backwards compat rollout

* Phase 1 (this RFC): server publishes to both; clients on old version still listen only on bare topic. New clients listen on both.
* Phase 2 (future): clients audit shows >95% on per-params capable version. Server flips to per-params-only via a `SyncServer::publish_bare_topic: false` config knob.
* Phase 3 (future): bare-topic publish removed entirely.

Phases 2 + 3 are deliberately NOT in this RFC — keep both forever is a fine production posture; the perf win comes from the bandwidth reduction in `/open`-time per-params subscriptions, not from removing the bare-topic publish.

#### C.7 Verification (PR-C)

* `cargo test -p pocopine-sync-query --test per_params_topics` — two subscriptions with different params; mutate row matching only one; assert only matching driver receives wakeup.
* `cargo test -p pocopine-sync-crud-macros` — trybuild test for `row_to_params` codegen on a struct with `required` + plain + `contains` annotated fields; verify Contains-only fields are omitted.
* Server integration test: spin up `pocopine-server` with `CrudResource`, two clients with different params; mutate row matching only one; assert only that client gets a live wakeup.

---

## Alternatives considered

* **A. Reuse `pocopine_sync::SyncCollection<C, T>` for persistence** — same rejection as in RFC 087 §"Alternatives A". CRUD coupling re-creates the tension we split.
* **B. Use `Box<dyn Any>` for selector cache values** — avoids the `PartialEq` requirement. Rejected: opaque cache values defeat the diff-suppress optimization that makes selectors usable in reactive UI loops. Document `no_diff` as the escape hatch for non-comparable outputs.
* **C. Per-`(name, params_hash, field)` topics** — even tighter routing. Server would publish per-(field) granularity ("the `status` of row X changed"). Rejected: complexity not justified without traffic data. Field-level invalidation can be a future RFC.
* **D. Pre-derive `Args: Hash + Eq` automatically via the macro vs require user trait impls** — the macro generates the Args struct, so deriving `Hash + Eq` on it works as long as each arg type already impls those. If one arg doesn't impl `Hash + Eq`, the user gets a compile error at the macro expansion site. Acceptable.

## Implementation status

* **PR-A** — Section A; `wip/sync-query-persistence`. ~600 LOC + 400 tests. Lands first.
* **PR-B** — Section B; `wip/sync-query-selectors`. ~800 LOC + 500 tests. References this RFC; no RFC edit needed.
* **PR-C** — Section C; `wip/sync-query-per-params-topics`. ~700 LOC + 400 tests. Cross-cuts client + server.

Each PR goes through Codex review at the checkpoint, matching the RFC 086 / 087 cadence.

## Migration / rollout

* **Section A** — Additive. Existing apps with `local_store: None` see no behavior change. Opt in with one line.
* **Section B** — Additive. New attribute macro; no existing code touched. Existing `#[query_resource]` users keep working.
* **Section C** — Additive. Server publishes to both topics; old clients work; new clients listen to both. No coordinated client/server deploy needed.

## Open questions

* **A:** when the user's app code drops + recreates a `QueryClient` (e.g., after sign-out), should `clear_all_streams()` fire automatically? Recommendation: yes — sign-out semantics from CRUD apply unchanged. Documented in the cookbook.
* **B:** does `#[query]` support generic functions (`fn projects_with_status<S: Into<Status>>(s: S)`)? Recommendation: not in v1; require monomorphized signatures. Generic selectors are doable but the args-hash story gets thorny — defer.
* **C:** what's the right `params_hash` collision recovery? Two distinct param maps that hash to the same 64-bit FNV-1a would route to the same topic; the client-side params filter (RFC 087 §6) catches the false positive. Documented; not worth blake3 yet (would change the wire format).

## Related RFCs

* [RFC 086](./rfc-086-sync-query.md) — `pocopine-sync-query` crate (routing engine).
* [RFC 087](./rfc-087-sync-query-driver.md) — driver lifecycle (the foundation this RFC builds on).
* [RFC 071](./rfc-071-event-spine.md) — live invalidation channel (Section C extends).
* [RFC 072](./rfc-072-offline-sync.md) — offline sync protocol (Section A reuses the `SyncLocalStore` trait that RFC 072 defines).
