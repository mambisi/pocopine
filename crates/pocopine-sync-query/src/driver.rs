//! Per-subscription background driver — owns `/open`, `/pull`, live
//! wakeup, and offline replay for one `QuerySubscription`.
//!
//! Spawned by [`QueryClient::observe`] the first time a query is
//! subscribed; survives across additional subscribes (refcount > 1)
//! and exits at its next `.await` point when the last
//! [`QueryHandle`](crate::QueryHandle) drops via [`DriverEpoch::bump`].
//!
//! ## Why a per-subscription driver, not per-stream
//!
//! Two subscriptions to the same stream with different params have
//! different `/pull` cursors and different canonical state
//! compartments — a per-stream driver would still need a per-
//! subscription state machine. See RFC 087 §"Alternatives B".
//!
//! ## Cancellation invariants
//!
//! 1. Every `.await` boundary in [`SubscriptionDriver::run`] is
//!    followed by an `if !self.epoch.is_current() { return; }`
//!    guard, OR an upgrade of the `Weak<QuerySubscription>` that
//!    bails on `None`. There is no path where a stale task can
//!    mutate state.
//! 2. `Drop` on the last `QueryHandle` runs
//!    `release_inner → maybe_bump_driver_epoch` which calls
//!    [`DriverEpoch::bump`]. The task observes the bump at its
//!    next yield via [`DriverEpoch::is_current`].
//! 3. The driver carries a `Weak<QuerySubscription<Row>>`, not
//!    `Rc`, so it doesn't keep the subscription alive past Drop.
//!    If the subscription is reclaimed while the driver is between
//!    awaits, the next `Weak::upgrade` returns `None` and the
//!    task exits.
//!
//! ## Native runtime requirement
//!
//! On native targets the driver is spawned via
//! `tokio::task::spawn_local` — it requires a `tokio::task::LocalSet`
//! to be active on the calling thread. Host tests must wrap their
//! body in `LocalSet::new().run_until(async { ... }).await`. Wasm
//! uses `wasm_bindgen_futures::spawn_local` which is local by
//! construction.

use std::cell::Cell;
use std::future::Future;
use std::rc::{Rc, Weak};
use std::time::Duration;

use pocopine_sync::{
    local_stream_key, LocalPendingMutation, LocalSnapshotBatch, LocalStreamSnapshot,
    SyncCollectionName, SyncCursor, SyncLocalStore, SyncOpenRequest, SyncOpenResponse,
    SyncOpenStream, SyncPullMode, SyncPullRequest, SyncPullResponse, SyncReason, SyncRow,
    SyncStreamName, SYNC_OPEN_PATH, SYNC_PULL_PATH,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::client::{QueryClient, QueryClientInner, QuerySubscription};
use crate::mutator::RowChange;
use crate::state::PendingOverlay;
use crate::wire::{build_open_request, build_pull_request};

/// Default sync endpoint prefix. Re-exported from `pocopine-sync`
/// at lib.rs.
pub const DEFAULT_SYNC_ENDPOINT: &str = pocopine_sync::SYNC_ENDPOINT_PREFIX;

/// Default driver polling cadence. Picked to match the
/// observed Linear/Slack patterns documented in RFC 087 §"Open
/// questions" — long enough that polling doesn't dominate the
/// request log, short enough that a missed live wakeup recovers
/// within human-noticeable latency.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// Tunable knobs for a [`QueryClient`]. See per-field docs for the
/// individual defaults.
#[derive(Clone)]
pub struct QueryClientConfig {
    /// Base endpoint for `/open`, `/pull`, `/push`. Defaults to
    /// [`DEFAULT_SYNC_ENDPOINT`]. Override for tests against a
    /// router mounted at a different path.
    pub endpoint: String,
    /// How often the driver tick polls `/pull` when no live wakeup
    /// fires. Defaults to [`DEFAULT_POLL_INTERVAL`]. `None`
    /// disables polling — the driver relies solely on live wakeup
    /// + manual refresh (currently a v2 feature; not exposed).
    pub poll_interval: Option<Duration>,
    /// Disable live wakeup subscription. Defaults to `false`. Set
    /// `true` for offline-only flows or tests that don't want SSE
    /// traffic.
    pub disable_live: bool,
    /// Send cookies / credentials with credentialed live-channel
    /// requests. Defaults to `true`.
    ///
    /// **Scope:** today this flag only controls the wasm
    /// `LiveClient.with_credentials(...)` call. The HTTP path
    /// (`/open` / `/pull` / `/push`) goes through
    /// `pocopine_core::fetch::call`, which doesn't expose a
    /// per-call credentials toggle — cross-origin credentialing
    /// for those calls is configured globally via fetch
    /// middleware (per RFC 078). A future RFC may unify the two,
    /// at which point this flag would govern both.
    pub with_credentials: bool,
    /// Durable local store. `None` means in-memory only (the
    /// historical default; no migration burden).
    ///
    /// When set, every spawned [`SubscriptionDriver`] hydrates from
    /// the store BEFORE the first `/open` (RFC 088 §A.3 Phase 0),
    /// persists snapshots after every successful `/pull`, and
    /// replays persisted pending mutations through the registered
    /// [`crate::mutator::AnyMutator`] entries.
    ///
    /// Stored as `Rc<dyn SyncLocalStore>` so the same store handle
    /// can be threaded into CRUD + sync-query clients in the same
    /// app — the two compartments don't collide because
    /// sync-query keys on
    /// [`pocopine_sync::local_stream_key`]`(stream, params)` while
    /// CRUD keys on the bare stream name.
    pub local_store: Option<Rc<dyn SyncLocalStore>>,
}

impl Default for QueryClientConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_SYNC_ENDPOINT.to_string(),
            poll_interval: Some(DEFAULT_POLL_INTERVAL),
            disable_live: false,
            with_credentials: true,
            local_store: None,
        }
    }
}

impl std::fmt::Debug for QueryClientConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryClientConfig")
            .field("endpoint", &self.endpoint)
            .field("poll_interval", &self.poll_interval)
            .field("disable_live", &self.disable_live)
            .field("with_credentials", &self.with_credentials)
            .field("local_store", &self.local_store.as_ref().map(|_| "<store>"))
            .finish()
    }
}

impl QueryClientConfig {
    /// Builder: attach a durable [`SyncLocalStore`]. See the field
    /// doc on [`Self::local_store`] for the persistence semantics.
    pub fn with_local_store(mut self, store: Rc<dyn SyncLocalStore>) -> Self {
        self.local_store = Some(store);
        self
    }
}

/// Snapshot-style generation token for the driver. Mirrors the
/// shape of `pocopine_sync::SyncEpoch` (private to that crate);
/// the driver carries one per spawn so a stale task that resumes
/// after `Drop` can detect the bump and bail.
///
/// Two values cooperate:
///
/// * `shared` — the live counter, held by every clone (the
///   subscription's master copy AND every spawned driver's
///   snapshot).
/// * `started` — the counter value captured at snapshot time.
///   `is_current` returns `true` while `shared.get() == started`.
#[derive(Clone, Debug)]
pub struct DriverEpoch {
    shared: Rc<Cell<u64>>,
    started: u64,
}

impl DriverEpoch {
    /// Build a fresh epoch with both `shared` and `started` at 0.
    pub fn new() -> Self {
        Self {
            shared: Rc::new(Cell::new(0)),
            started: 0,
        }
    }

    /// Snapshot the current generation for a new spawn. The
    /// returned value shares the underlying counter with the
    /// caller; both observe later [`bump`](Self::bump) calls.
    pub fn snapshot(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            started: self.shared.get(),
        }
    }

    /// `true` until the next `bump` invalidates this snapshot.
    /// Re-checked after every `.await` boundary inside the driver.
    pub fn is_current(&self) -> bool {
        self.shared.get() == self.started
    }

    /// Advance the shared generation. Run from
    /// `release_inner` on the last handle drop so any spawned
    /// driver bails at its next yield.
    pub fn bump(&self) {
        self.shared.set(self.shared.get().wrapping_add(1));
    }
}

impl Default for DriverEpoch {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle to a spawned driver task, parked on the subscription so
/// `Drop` semantics flow through the existing refcount path.
///
/// The handle is currently a marker only — its presence on a
/// subscription tells `QueryClient::observe` "a driver is already
/// running for this subscription, don't spawn a second one". The
/// actual cancellation goes through [`DriverEpoch`], not through
/// a `JoinHandle::abort`, because aborting a task mid-`/pull`
/// would leave the subscription's `state.syncing = true` and
/// confuse the next observer.
#[derive(Debug)]
pub struct DriverHandle {
    /// Marker only; the field stops Rust from complaining the type
    /// has no inhabitable shape on host (where we don't store a
    /// concrete future handle).
    _placeholder: (),
}

impl DriverHandle {
    #[allow(dead_code)]
    pub(crate) fn placeholder() -> Self {
        Self { _placeholder: () }
    }
}

/// Per-subscription driver. One spawn per `(TypeId, QueryKey)`.
pub(crate) struct SubscriptionDriver<Row: 'static> {
    /// Weak ref to the subscription. Upgraded on every tick;
    /// failure → task exits.
    subscription: Weak<QuerySubscription<Row>>,
    /// Weak ref to the client so the driver can call the
    /// canonical-pull ingest path without keeping the client
    /// alive past its drop.
    client: Weak<QueryClientInner>,
    /// Snapshot of the subscription's driver epoch — captured
    /// once at spawn time; checked after every `.await`.
    epoch: DriverEpoch,
    /// Sync endpoint prefix for this driver's HTTP calls.
    endpoint: String,
    /// Poll cadence. `None` disables the heartbeat — the driver
    /// only pulls in response to live wakeup events.
    poll_interval: Option<Duration>,
    /// Whether to attempt a live-wakeup subscription. Per RFC 087
    /// §6 this is wasm-only today; the host path always treats
    /// `disable_live = false` as "no live wakeup available; rely
    /// on polling".
    disable_live: bool,
    /// Mirrors `QueryClientConfig::with_credentials` — passed to the
    /// wasm `LiveClient.with_credentials(...)` so cookie- or
    /// session-backed live channels actually carry credentials.
    ///
    /// **Note on HTTP `/open` / `/pull` / `/push`:** the
    /// `pocopine_core::fetch::call` path doesn't expose a
    /// per-call credentials toggle; cross-origin credentialing
    /// for those calls is configured globally via fetch
    /// middleware. This flag therefore only affects the live
    /// channel today. The doc on
    /// [`QueryClientConfig::with_credentials`] records the same
    /// limitation.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    with_credentials: bool,
    /// Durable local store hooked into Phase 0 hydrate + post-pull
    /// persist + schema-drift wipe. `None` keeps the driver
    /// in-memory-only — every store call site short-circuits and
    /// the driver behaves identically to the pre-RFC-088 shape.
    local_store: Option<Rc<dyn SyncLocalStore>>,
}

impl<Row> SubscriptionDriver<Row>
where
    Row: Clone + Serialize + DeserializeOwned + 'static,
{
    /// Build a driver. The caller threads this into [`spawn_driver`].
    pub(crate) fn new(
        subscription: Weak<QuerySubscription<Row>>,
        client: Weak<QueryClientInner>,
        epoch: DriverEpoch,
        config: &QueryClientConfig,
    ) -> Self {
        Self {
            subscription,
            client,
            epoch: epoch.snapshot(),
            endpoint: config.endpoint.clone(),
            poll_interval: config.poll_interval,
            disable_live: config.disable_live,
            with_credentials: config.with_credentials,
            local_store: config.local_store.clone(),
        }
    }

    /// Drive the subscription forever (or until cancelled).
    pub(crate) async fn run(self) {
        // ── Phase 0: hydrate from durable store (RFC 088 §A.3) ──
        //
        // Runs INSIDE the driver task (not as a separate spawn) so
        // it shares the same cancellation semantics as the
        // /open/pull phases. Populates canonical_rows + pending
        // overlays + cursor + application_schema_version on the
        // subscription's state so observers see cached rows on
        // first read — before /open even fires. The schema-drift
        // gate in apply_open will wipe the in-memory state if
        // /open advertises a fresh version; until that lands, the
        // hydrated rows are shown.
        self.hydrate_phase().await;
        if !self.epoch.is_current() {
            return;
        }

        // ── Phase 1: /open ──────────────────────────────────────
        //
        // Sets `loading = true` synchronously BEFORE the await so a
        // UI binding observes the loading state immediately on
        // subscribe. If `/open` fails we still mark the error and
        // continue to the polling loop (the loop retries on the
        // next interval).
        self.mark_loading();
        let open_response = match self.send_open().await {
            Ok(resp) => resp,
            Err(err) => {
                if !self.epoch.is_current() {
                    return;
                }
                self.mark_error(err);
                // Fall through to the loop so a subsequent retry can
                // recover when the network comes back.
                self.run_loop().await;
                return;
            }
        };
        if !self.epoch.is_current() {
            return;
        }
        if let Err(()) = self.apply_open(open_response).await {
            // Schema-drift path bumped state.version; nothing more
            // to do here, the next /pull rebuilds the rows. The
            // durable wipe (clear_stream) was awaited inside
            // apply_open so it's already on disk.
        }

        // ── Phase 2: initial /pull ──────────────────────────────
        let pull_response = match self.send_pull(None).await {
            Ok(resp) => resp,
            Err(err) => {
                if !self.epoch.is_current() {
                    return;
                }
                self.mark_error(err);
                self.run_loop().await;
                return;
            }
        };
        if !self.epoch.is_current() {
            return;
        }
        self.apply_pull(pull_response);
        if !self.epoch.is_current() {
            return;
        }
        // Persist the freshly-pulled canonical snapshot. Runs after
        // apply_pull so the persisted view matches what's on the
        // subscription's state.
        self.persist_snapshot().await;
        if !self.epoch.is_current() {
            return;
        }
        self.clear_loading();

        // ── Phase 3: main loop ──────────────────────────────────
        self.run_loop().await;
    }

    /// Run the post-initial loop: heartbeat polling + live wakeup +
    /// offline replay. The function returns when the epoch goes stale
    /// or the subscription's Weak fails to upgrade.
    async fn run_loop(&self) {
        // Open the live wakeup channel if enabled. The receiver
        // participates in `wait_for_tick`'s select! alongside the
        // polling timer. Host targets get a permanently-empty
        // receiver because the LiveClient is wasm-only.
        let mut live = self.open_live_wakeup();
        loop {
            // 1. Wait for the next tick: either the poll timer or
            //    a matching live event (whichever first).
            let outcome = self.wait_for_tick(&mut live).await;
            match outcome {
                TickOutcome::Poll | TickOutcome::Live => {}
                TickOutcome::Stale => return,
            }
            if !self.epoch.is_current() {
                return;
            }
            if self.subscription.upgrade().is_none() {
                return;
            }
            // 2. Issue a pull.
            self.tick_pull().await;
            if !self.epoch.is_current() {
                return;
            }
            // 3. Walk pending replay queue (offline-replay).
            self.replay_pending().await;
            if !self.epoch.is_current() {
                return;
            }
        }
    }

    /// Open the live-wakeup receiver. On wasm this calls into
    /// `pocopine-live::LiveClient` to subscribe to the per-stream
    /// **query-tag** topic (`sync_stream_tag(stream)`) — the same
    /// topic `SyncServer::invalidate_stream` publishes to and the
    /// CRUD client's `LiveRefresh::scoped().query_tag(...)`
    /// listener consumes. Earlier drafts subscribed to the
    /// collection-name topic; that mismatched the server's publish
    /// path and silently dropped every live invalidation, leaving
    /// query views to refresh only on the poll fallback. On host
    /// (or with `disable_live = true`) returns a stub that never
    /// fires.
    fn open_live_wakeup(&self) -> LiveWakeup {
        if self.disable_live {
            tracing::debug!(
                target: "pocopine.log",
                "sync-query driver: live wakeup disabled by config"
            );
            return LiveWakeup::disabled();
        }
        #[cfg(target_arch = "wasm32")]
        {
            let Some(sub) = self.subscription.upgrade() else {
                return LiveWakeup::disabled();
            };
            let stream_name = sub.query().stream().as_str().to_string();
            let captured_params = sub.query().params().clone();
            LiveWakeup::open_on_stream(stream_name, captured_params, self.with_credentials)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            LiveWakeup::disabled()
        }
    }

    /// Wait for the next driver tick. Returns the originating
    /// signal so the loop can decide whether to pull. The select
    /// on (sleep, live) is biased toward live wakeup so a coherent
    /// burst of events doesn't get starved by the poll timer.
    async fn wait_for_tick(&self, live: &mut LiveWakeup) -> TickOutcome {
        use futures::future::{self, Either};
        match self.poll_interval {
            Some(interval) => {
                let timer = sleep(interval);
                let live_fut = live.next_matching();
                futures::pin_mut!(timer);
                futures::pin_mut!(live_fut);
                match future::select(timer, live_fut).await {
                    Either::Left((_, _)) => {
                        if !self.epoch.is_current() {
                            return TickOutcome::Stale;
                        }
                        TickOutcome::Poll
                    }
                    Either::Right((event, _)) => {
                        if !self.epoch.is_current() {
                            return TickOutcome::Stale;
                        }
                        match event {
                            LiveTickOutcome::Match => TickOutcome::Live,
                            LiveTickOutcome::Closed => {
                                // Live channel closed (subscription
                                // dropped or server hung up); fall
                                // back to polling — the next loop
                                // iteration just sleeps again.
                                TickOutcome::Poll
                            }
                        }
                    }
                }
            }
            None => {
                // Polling disabled. Block on live; if that's also
                // unavailable, exit cleanly.
                if live.is_disabled() {
                    tracing::debug!(
                        target: "pocopine.log",
                        "sync-query driver: polling disabled + no live wakeup; exiting"
                    );
                    return TickOutcome::Stale;
                }
                match live.next_matching().await {
                    LiveTickOutcome::Match => TickOutcome::Live,
                    LiveTickOutcome::Closed => TickOutcome::Stale,
                }
            }
        }
    }

    /// Issue a /pull and route results into canonical state.
    /// Errors surface into `state.error`; the next tick retries.
    async fn tick_pull(&self) {
        let cursor = self.with_state_borrow(|s| s.cursor.clone()).flatten();
        let response = match self.send_pull(cursor).await {
            Ok(resp) => resp,
            Err(err) => {
                if !self.epoch.is_current() {
                    return;
                }
                self.mark_error(err);
                return;
            }
        };
        if !self.epoch.is_current() {
            return;
        }
        self.apply_pull(response);
        if !self.epoch.is_current() {
            return;
        }
        // Persist after every successful /pull (RFC 088 §A.3) so a
        // reload after the tick observes the latest canonical set.
        self.persist_snapshot().await;
    }

    /// Walk the pending-replay queue on the client and re-fire
    /// `apply_remote` for each entry. Successful replays clear
    /// the queue entry; persistent failures leave it for the next
    /// tick.
    ///
    /// The replay set is keyed by `mutation_id`. Each entry holds
    /// a boxed `Future`-builder so the driver doesn't need the
    /// concrete `Mutator` type. The original payload is
    /// preserved verbatim — the wire dedup contract (RFC 072) is
    /// what keeps a replay-after-accept idempotent.
    async fn replay_pending(&self) {
        let Some(client_inner) = self.client.upgrade() else {
            return;
        };
        let entries = QueryClient::take_replay_entries(&client_inner);
        for entry in entries {
            if !self.epoch.is_current() {
                return;
            }
            // The replay future builds against a strong
            // QueryClient handle — we materialize one for the
            // duration of the call, then release it. This avoids
            // pinning the client alive in a long-lived closure.
            let client = QueryClient::from_inner(client_inner.clone());
            let fut = (entry.replay)(&client);
            let outcome = fut.await;
            if !self.epoch.is_current() {
                return;
            }
            match outcome {
                ReplayOutcome::Accepted => {
                    // Routing engine already cleared the pending
                    // overlay via route_canonical_changes; nothing
                    // more to do.
                }
                ReplayOutcome::StillOffline => {
                    // Re-queue. The map entry was popped on take;
                    // push it back so the next successful tick
                    // retries.
                    QueryClient::reinsert_replay_entry(&client_inner, entry);
                }
                ReplayOutcome::Rejected => {
                    // Server explicitly rejected (4xx, app error).
                    // Roll back the pending overlay ACROSS EVERY
                    // subscription on the stream — the original
                    // optimistic apply fanned out to every view
                    // whose predicate matched at apply_local time,
                    // and the replay queue is also shared across
                    // every driver on the stream (any driver may
                    // dequeue this entry). Cleaning only the
                    // driver-local subscription leaks stale
                    // pending rows into every other view that
                    // received the same optimistic upsert. The
                    // wire contract says retries of a rejected
                    // mutation must NOT change the outcome — so
                    // dropping the queue entry is correct.
                    QueryClient::dequeue_pending_for_stream::<Row>(
                        &client_inner,
                        &entry.stream,
                        &entry.mutation_id,
                    );
                }
            }
        }
    }

    /// Issue the `/open` HTTP call. Returns the deserialized
    /// response or a typed error wrapped into a `String` for the
    /// state.error sink.
    async fn send_open(&self) -> Result<SyncOpenResponse, DriverError> {
        let query = self.with_subscription(|sub| {
            let q = sub.query();
            build_open_request_for(q)
        })?;
        let url = build_url(&self.endpoint, SYNC_OPEN_PATH);
        let res: Result<SyncOpenResponse, _> =
            pocopine_core::fetch::call::<SyncOpenRequest, SyncOpenResponse>(&url, &query).await;
        res.map_err(DriverError::from_server_error)
    }

    /// Issue the `/pull` HTTP call.
    async fn send_pull(
        &self,
        cursor: Option<SyncCursor>,
    ) -> Result<SyncPullResponse<Value>, DriverError> {
        let request = self.with_subscription(|sub| build_pull_request(sub.query(), cursor))?;
        let url = build_url(&self.endpoint, SYNC_PULL_PATH);
        let res: Result<SyncPullResponse<Value>, _> =
            pocopine_core::fetch::call::<SyncPullRequest, SyncPullResponse<Value>>(&url, &request)
                .await;
        res.map_err(DriverError::from_server_error)
    }

    /// Apply an `/open` response: validate the stream, detect
    /// schema drift, install cursor.
    ///
    /// Returns `Err(())` when schema-drift triggered a state
    /// reset — caller should NOT treat that as a hard error
    /// (the next `/pull` rebuilds the canonical set).
    ///
    /// Schema-drift wipe atomicity: when drift is detected we
    /// awaited the durable wipe (`clear_stream`) FIRST, then the
    /// in-memory `state.reset()` runs synchronously while the
    /// `RefMut` is held. Observers see the canonical row set
    /// transition in one borrow — no observer can land between
    /// `clear_stream` and `reset` and read empty-but-stale-version
    /// state.
    async fn apply_open(&self, response: SyncOpenResponse) -> Result<(), ()> {
        let Some(sub) = self.subscription.upgrade() else {
            return Ok(());
        };
        let stream_name = sub.query().stream().clone();
        let Some(opened) = response
            .streams
            .iter()
            .find(|s| s.stream == stream_name)
            .cloned()
        else {
            // Server didn't accept this stream. Surface as an
            // error; the next tick retries.
            self.mark_error(DriverError::Client(format!(
                "/open response missing stream {}",
                stream_name.as_str()
            )));
            return Ok(());
        };
        let SyncOpenStream {
            cursor,
            schema_version,
            ..
        } = opened;
        // Schema-drift detection MUST run while not holding the
        // state borrow — the durable wipe is async and we don't
        // want a `RefMut` held across an .await. Read the cached
        // version, drop the borrow, run the wipe, then re-borrow
        // to apply the in-memory transition atomically.
        let cached = self
            .with_state_borrow(|s| s.application_schema_version)
            .flatten();
        let drift = matches!(cached, Some(c) if c != schema_version);
        if drift {
            tracing::info!(
                target: "pocopine.log",
                stream = stream_name.as_str(),
                from = ?cached,
                to = schema_version,
                "sync-query: schema drift detected; resetting state + wiping store",
            );
            if let Some(store) = &self.local_store {
                let compartment = self.compartment_key();
                if let Err(err) = store.clear_stream(&compartment).await {
                    tracing::warn!(
                        target: "pocopine.log",
                        stream = stream_name.as_str(),
                        error = %err,
                        "sync-query: schema-drift wipe failed; in-memory reset still proceeds",
                    );
                }
                if !self.epoch.is_current() {
                    return Ok(());
                }
            }
        }
        let mut state = sub.state().borrow_mut();
        if drift {
            // Reset BEFORE writing the new schema_version so an
            // observer that reads inside the RefMut would see
            // `(canonical_empty, version=new)` rather than
            // `(canonical_stale, version=new)`.
            state.reset();
        }
        state.application_schema_version = Some(schema_version);
        state.cursor = cursor;
        state.error.clear();
        state.last_reason = SyncReason::Initial;
        drop(state);
        sub.notify_listeners_external();
        if drift {
            Err(())
        } else {
            Ok(())
        }
    }

    /// Apply a `/pull` response.
    ///
    /// `Snapshot` mode replaces THIS subscription's canonical set:
    /// only the originating subscription's rows are wiped before
    /// the routing engine re-upserts the snapshot's rows. Other
    /// subscriptions on the same stream keep their independent
    /// canonical state — they may be ahead or behind this
    /// subscription's cursor because each driver issues its own
    /// `/pull`.
    ///
    /// `Incremental` mode just routes the deltas through
    /// `route_canonical_pull`; nothing is wiped first.
    fn apply_pull(&self, response: SyncPullResponse<Value>) {
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        let Some(client_inner) = self.client.upgrade() else {
            return;
        };
        let stream = response.stream.clone();
        let new_cursor = response.cursor.clone();

        // Build a Vec<RowChange<Row>> from the wire response.
        // Decode failures are warned + skipped per RFC 086.
        let changes = decode_pull_changes::<Row>(&response);

        if matches!(response.mode, SyncPullMode::Snapshot) {
            // Wipe only THIS subscription's canonical set; the
            // routing engine's predicate evaluation below
            // re-inserts the matching rows. Other subscriptions
            // on the same stream are untouched — their cursors
            // and snapshots are independent.
            let keys: Vec<pocopine_sync::RowKey> = {
                let state = sub.state().borrow();
                state.canonical_rows().map(|r| r.key.clone()).collect()
            };
            if !keys.is_empty() {
                let mut state = sub.state().borrow_mut();
                for key in keys {
                    state.remove_canonical(&key);
                }
            }
        }

        QueryClient::route_canonical_pull::<Row>(&client_inner, &stream, &changes);

        // Update cursor and reason on the originating
        // subscription (not all subscriptions on the stream — a
        // different subscription's cursor may be ahead or behind
        // this one, since each subscription's /pull is
        // independent).
        let mut state = sub.state().borrow_mut();
        state.cursor = new_cursor;
        state.syncing = false;
        state.loading = false;
        state.last_reason = SyncReason::Manual;
        state.error.clear();
        drop(state);
        sub.notify_listeners_external();
    }

    /// Stable cache compartment for this driver's subscription.
    /// Returns `local_stream_key(stream, params)` per RFC 088 §A.2.
    /// Two subscriptions on the same stream with different params
    /// get distinct compartments; the same params share one.
    fn compartment_key(&self) -> SyncStreamName {
        match self.subscription.upgrade() {
            Some(sub) => local_stream_key(sub.query().stream(), sub.query().params()),
            // Subscription already reclaimed — fall back to a bare
            // placeholder so the caller's await doesn't panic; the
            // epoch check will catch the cancellation immediately
            // after.
            None => SyncStreamName::new("__sync_query_orphaned").expect("static valid stream"),
        }
    }

    /// Phase 0 (RFC 088 §A.3): hydrate canonical rows, pending
    /// overlays, cursor, and application_schema_version from the
    /// durable store BEFORE `/open` fires. No-op when the client
    /// has no `local_store` configured.
    ///
    /// Failures are warned and skipped — the subsequent `/open`
    /// + `/pull` rebuild from scratch, the user just doesn't get
    ///   the instant-paint UX.
    async fn hydrate_phase(&self) {
        let Some(store) = self.local_store.clone() else {
            return;
        };
        let compartment = self.compartment_key();
        let snapshot = match store.hydrate_stream(&compartment).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = compartment.as_str(),
                    error = %err,
                    "sync-query: hydrate failed; continuing with empty state",
                );
                return;
            }
        };
        if !self.epoch.is_current() {
            return;
        }
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        self.apply_hydrated_snapshot(&sub, snapshot);
    }

    /// Pour a `LocalStreamSnapshot` into the subscription's
    /// `QueryState`. Decodes the wire-shape `Vec<SyncRow<Value>>`
    /// into typed `SyncRow<Row>` (rows that fail to decode are
    /// dropped + warned, matching the `/pull` decode policy). The
    /// pending overlays are reconstructed with `evicted_key` /
    /// `deleted_row` left at `None` because the persisted layer
    /// doesn't carry the routing engine's optimistic-departure
    /// metadata — the next /pull or replay reconciles either way.
    fn apply_hydrated_snapshot(
        &self,
        sub: &Rc<QuerySubscription<Row>>,
        snapshot: LocalStreamSnapshot,
    ) {
        let stream_name = sub.query().stream().clone();
        let mut state = sub.state().borrow_mut();
        // Adopt the cached schema_version BEFORE writing rows so
        // an observer that races a read in between sees the
        // version + rows together (we never expose a row count
        // with a stale schema_version).
        state.application_schema_version = snapshot.application_schema_version;
        state.cursor = snapshot.cursor.clone();
        // Hydrated canonical rows. Per-row decode failures are
        // logged and skipped; the next /pull will resurface them
        // if they're still server-confirmed.
        for wire_row in snapshot.rows.iter() {
            match serde_json::from_value::<Row>(wire_row.value.clone()) {
                Ok(value) => {
                    state.upsert_canonical(SyncRow {
                        key: wire_row.key.clone(),
                        version: wire_row.version.clone(),
                        value,
                        pending: false,
                        conflict: false,
                    });
                }
                Err(err) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        stream = stream_name.as_str(),
                        key = wire_row.key.as_str(),
                        error = %err,
                        "sync-query: dropping hydrated row that failed to decode"
                    );
                }
            }
        }
        // Hydrated pending overlays. The persisted shape carries
        // the wire `ClientMutation<Value>` + an optional
        // `optimistic_row: SyncRow<Value>`; we decode the optimistic
        // row payload back to `Row` when present, and leave the
        // departure metadata as None (the replay tick will route
        // the canonical changes through `route_canonical_changes`
        // which clears the overlay either way).
        for pending in snapshot.pending_mutations.iter() {
            let optimistic_row: Option<SyncRow<Row>> =
                pending.optimistic_row.as_ref().and_then(|wire_row| {
                    match serde_json::from_value::<Row>(wire_row.value.clone()) {
                        Ok(value) => Some(SyncRow {
                            key: wire_row.key.clone(),
                            version: wire_row.version.clone(),
                            value,
                            pending: true,
                            conflict: false,
                        }),
                        Err(err) => {
                            tracing::warn!(
                                target: "pocopine.log",
                                stream = stream_name.as_str(),
                                key = wire_row.key.as_str(),
                                error = %err,
                                "sync-query: dropping hydrated pending row that failed to decode"
                            );
                            None
                        }
                    }
                });
            state.push_pending(PendingOverlay {
                mutation_id: pending.mutation.id.clone(),
                mutation: pending.mutation.clone(),
                optimistic_row,
                deleted_row: None,
                evicted_key: None,
                conflict: false,
            });
        }
        drop(state);
        sub.notify_listeners_external();
        // Hand persisted pending mutations to the client-level
        // replay path so the first tick after /open runs them
        // through the registered MutatorRegistry → AnyMutator.
        // We pass the wire mutation list (with persistence
        // envelope intact); the registry unwraps before invoking
        // apply_remote.
        let Some(client_inner) = self.client.upgrade() else {
            return;
        };
        let stream_for_replay = stream_name;
        QueryClient::enqueue_hydrated_pending(
            &client_inner,
            stream_for_replay,
            snapshot
                .pending_mutations
                .into_iter()
                .map(|p| p.mutation)
                .collect(),
        );
    }

    /// Persist the current canonical state into the local store.
    /// Called after every successful `/pull` (RFC 088 §A.3). No-op
    /// when the client has no `local_store` configured.
    async fn persist_snapshot(&self) {
        let Some(store) = self.local_store.clone() else {
            return;
        };
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        // Snapshot canonical rows + pending under a single
        // immutable borrow so the persisted view is a coherent
        // slice. We re-serialize through `serde_json::Value`
        // because the local store contract is `SyncRow<Value>`.
        let snapshot_inputs = {
            let state = sub.state().borrow();
            let stream = sub.query().stream().clone();
            let cursor = state.cursor.clone();
            let app_version = state.application_schema_version;
            let canonical: Vec<SyncRow<Value>> = state
                .canonical_rows()
                .filter_map(|row| {
                    let value = serde_json::to_value(&row.value).ok()?;
                    Some(SyncRow {
                        key: row.key.clone(),
                        version: row.version.clone(),
                        value,
                        pending: false,
                        conflict: false,
                    })
                })
                .collect();
            let pending: Vec<LocalPendingMutation> = state
                .pending()
                .iter()
                .map(|overlay| {
                    let optimistic_row: Option<SyncRow<Value>> =
                        overlay.optimistic_row.as_ref().and_then(|r| {
                            let value = serde_json::to_value(&r.value).ok()?;
                            Some(SyncRow {
                                key: r.key.clone(),
                                version: r.version.clone(),
                                value,
                                pending: true,
                                conflict: false,
                            })
                        });
                    LocalPendingMutation::new(overlay.mutation.clone())
                        .with_optimistic_row(optimistic_row)
                })
                .collect();
            (stream, cursor, app_version, canonical, pending)
        };
        let (stream, cursor, app_version, canonical, pending) = snapshot_inputs;
        let compartment = local_stream_key(&stream, sub.query().params());
        // sync-query streams don't have a separate "collection"
        // surface; reuse the stream name as the collection token
        // so the local store's compartment is stable. The
        // collection field is informational on the local store
        // (used by CRUD callers); sync-query never reads it back.
        let collection = match SyncCollectionName::new(compartment.as_str()) {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = compartment.as_str(),
                    error = %err,
                    "sync-query: persist_snapshot could not derive a collection name; skipping",
                );
                return;
            }
        };
        let batch = LocalSnapshotBatch::new(compartment.clone(), collection, canonical, cursor)
            .with_application_schema_version(app_version);
        if let Err(err) = store.save_snapshot(batch).await {
            tracing::warn!(
                target: "pocopine.log",
                stream = compartment.as_str(),
                error = %err,
                "sync-query: persist_snapshot failed; in-memory state still authoritative",
            );
        }
        if !self.epoch.is_current() {
            return;
        }
        // The save_snapshot call replaces canonical_rows but
        // leaves any pre-existing persisted pending mutations
        // intact in MemoryLocalStore + SQLite + IndexedDB. So we
        // also enqueue the current pending overlay's wire
        // payload, which is idempotent — the store's
        // enqueue_pending_mutation overwrites by mutation id (per
        // its contract).
        for pending_mut in pending {
            if let Err(err) = store
                .enqueue_pending_mutation(&compartment, pending_mut)
                .await
            {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = compartment.as_str(),
                    error = %err,
                    "sync-query: enqueue_pending_mutation during persist failed; will retry on next pull"
                );
            }
        }
    }

    /// Set the subscription's loading flag without writing to
    /// canonical state. Synchronous; runs before the first await.
    fn mark_loading(&self) {
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        let mut state = sub.state().borrow_mut();
        state.loading = true;
        state.last_reason = SyncReason::Initial;
        drop(state);
        sub.notify_listeners_external();
    }

    /// Clear loading after a successful initial pull.
    fn clear_loading(&self) {
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        let mut state = sub.state().borrow_mut();
        state.loading = false;
        state.syncing = false;
        drop(state);
        sub.notify_listeners_external();
    }

    /// Park an error message on the subscription's state.
    /// Surfaces in the reactive view so UIs can show "syncing
    /// failed" / "offline" banners. Does NOT clear the canonical
    /// rows — the cache stays visible during a network blip.
    fn mark_error(&self, err: DriverError) {
        let Some(sub) = self.subscription.upgrade() else {
            return;
        };
        let mut state = sub.state().borrow_mut();
        state.error = err.to_string();
        state.syncing = false;
        state.loading = false;
        state.last_reason = SyncReason::Error;
        drop(state);
        sub.notify_listeners_external();
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "sync-query driver tick failed; will retry on next interval"
        );
    }

    /// Borrow the subscription's state and run `f` against an
    /// immutable view. Returns `None` if the subscription has
    /// been reclaimed.
    fn with_state_borrow<R, F: FnOnce(&crate::state::QueryState<Row>) -> R>(
        &self,
        f: F,
    ) -> Option<R> {
        let sub = self.subscription.upgrade()?;
        let state = sub.state().borrow();
        Some(f(&state))
    }

    /// Run `f` against the subscription. Returns
    /// `Err(DriverError::Cancelled)` if the subscription has
    /// already been reclaimed — caller treats it as a clean
    /// stale-task exit.
    fn with_subscription<R, F: FnOnce(&QuerySubscription<Row>) -> R>(
        &self,
        f: F,
    ) -> Result<R, DriverError> {
        match self.subscription.upgrade() {
            Some(sub) => Ok(f(&sub)),
            None => Err(DriverError::Cancelled),
        }
    }
}

/// Helper to build an open request that doesn't borrow the
/// builder return for longer than the closure — keeps
/// `with_subscription` polymorphic.
fn build_open_request_for<Row>(query: &crate::query::Query<Row>) -> SyncOpenRequest {
    build_open_request(query)
}

/// Decode a `/pull` response's rows / changes into typed
/// `RowChange<Row>` values. Rows that fail to decode are
/// `tracing::warn`'d and skipped per the RFC 086 wire-shape
/// mismatch rule.
fn decode_pull_changes<Row>(response: &SyncPullResponse<Value>) -> Vec<RowChange<Row>>
where
    Row: DeserializeOwned + 'static,
{
    let mut out: Vec<RowChange<Row>> = Vec::new();
    match response.mode {
        SyncPullMode::Snapshot => {
            for row in &response.rows {
                match serde_json::from_value::<Row>(row.value.clone()) {
                    Ok(decoded) => out.push(RowChange::Upsert(decoded)),
                    Err(err) => {
                        tracing::warn!(
                            target: "pocopine.log",
                            stream = response.stream.as_str(),
                            key = row.key.as_str(),
                            error = %err,
                            "sync-query: dropping /pull snapshot row that failed to decode"
                        );
                    }
                }
            }
        }
        SyncPullMode::Incremental => {
            for change in &response.changes {
                match (&change.op, &change.row, &change.key) {
                    (pocopine_sync::SyncOp::Upsert, Some(row), _) => {
                        match serde_json::from_value::<Row>(row.value.clone()) {
                            Ok(decoded) => out.push(RowChange::Upsert(decoded)),
                            Err(err) => {
                                tracing::warn!(
                                    target: "pocopine.log",
                                    stream = response.stream.as_str(),
                                    key = row.key.as_str(),
                                    error = %err,
                                    "sync-query: dropping /pull incremental upsert that failed to decode"
                                );
                            }
                        }
                    }
                    (pocopine_sync::SyncOp::Delete, _, Some(key)) => {
                        out.push(RowChange::Delete(key.clone()));
                    }
                    _ => {
                        // Malformed change — skip per the wire-
                        // shape mismatch rule.
                        tracing::warn!(
                            target: "pocopine.log",
                            stream = response.stream.as_str(),
                            "sync-query: skipping malformed /pull change (op/row/key shape mismatch)"
                        );
                    }
                }
            }
        }
    }
    out
}

/// Join two path components with at most one '/' between.
fn build_url(endpoint: &str, path: &str) -> String {
    let trimmed_prefix = endpoint.trim_end_matches('/');
    // `path` already starts with the canonical "/__pocopine/..."
    // when callers pass `SYNC_OPEN_PATH` (the absolute path
    // constants). If the configured endpoint overrides the prefix,
    // re-attach the suffix.
    if path.starts_with(trimmed_prefix) || trimmed_prefix.is_empty() {
        path.to_string()
    } else if let Some(suffix) = path.strip_prefix(pocopine_sync::SYNC_ENDPOINT_PREFIX) {
        format!("{trimmed_prefix}{suffix}")
    } else {
        path.to_string()
    }
}

/// Typed driver error. Folds host-fetch failures and
/// subscription-cancelled signals into one enum so the loop can
/// pattern-match instead of carrying free-form strings.
#[derive(Debug)]
pub enum DriverError {
    /// Network or HTTP failure surfaced by `pocopine::fetch`.
    Network(String),
    /// Server returned a typed 4xx/5xx — non-network, but the
    /// driver should still retry on the next interval (the
    /// server might be temporarily unhealthy).
    Server(String),
    /// Local client error (build URL failed, etc.).
    Client(String),
    /// Subscription has been reclaimed; caller should exit cleanly.
    Cancelled,
}

impl DriverError {
    /// Returns `true` for errors the driver treats as transient
    /// — pending overlays stay in the replay queue for these.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Network(_) | Self::Server(_))
    }

    /// Convert a `pocopine_core::server::ServerError` (the wire
    /// error from `fetch::call`) into a driver-side category.
    /// All `Network` variants stay `Network` so the offline
    /// replay path triggers. Other variants are `Server`.
    pub fn from_server_error(err: pocopine_core::server::ServerError) -> Self {
        use pocopine_core::server::ServerError;
        match err {
            ServerError::Network(msg) => Self::Network(msg),
            ServerError::App(msg)
            | ServerError::Unauthorized(msg)
            | ServerError::Forbidden(msg)
            | ServerError::BadRequest(msg) => Self::Server(msg),
        }
    }
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(msg) => write!(f, "network error: {msg}"),
            Self::Server(msg) => write!(f, "server error: {msg}"),
            Self::Client(msg) => write!(f, "client error: {msg}"),
            Self::Cancelled => f.write_str("driver cancelled"),
        }
    }
}

impl std::error::Error for DriverError {}

/// Outcome of one replay attempt. The driver feeds this back
/// from the replay future into [`SubscriptionDriver::replay_pending`].
#[derive(Debug)]
pub enum ReplayOutcome {
    /// Server accepted; canonical reconcile already ran; the
    /// pending overlay was cleared by `route_canonical_changes`.
    Accepted,
    /// Still offline (network error). Re-queue.
    StillOffline,
    /// Server rejected (non-network error). Drop the pending
    /// overlay; the user's UI should re-surface the failure via
    /// `state.error`.
    Rejected,
}

/// Boxed replay-future builder. Type-aliased so clippy doesn't
/// flag the closure-shape as "very complex type" — the surface
/// IS load-bearing here (heterogeneous across Mutator impls).
pub(crate) type ReplayFuture =
    Box<dyn for<'a> Fn(&'a QueryClient) -> futures::future::LocalBoxFuture<'a, ReplayOutcome>>;

/// One queued mutation replay. The replay closure is built
/// inside `QueryClient::mutate` and captures the original
/// payload + Mutator type via a generic argument.
pub(crate) struct ReplayEntry {
    pub(crate) stream: SyncStreamName,
    pub(crate) mutation_id: pocopine_sync::MutationId,
    /// `Box<dyn Fn(...) -> Pin<Box<dyn Future>>>` instead of an
    /// `async fn` so the entry can be heterogeneous across
    /// `Mutator` types in the same client-side replay queue.
    pub(crate) replay: ReplayFuture,
}

impl std::fmt::Debug for ReplayEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayEntry")
            .field("stream", &self.stream.as_str())
            .field("mutation_id", &self.mutation_id)
            .finish_non_exhaustive()
    }
}

/// Why the driver woke up. Returned from `wait_for_tick` so the
/// loop can record telemetry (poll vs live) without re-checking
/// the timer.
#[derive(Debug)]
enum TickOutcome {
    /// Heartbeat poll timer fired.
    Poll,
    /// A matching live wakeup event arrived.
    Live,
    /// Epoch is stale OR neither source can produce another event;
    /// caller exits.
    Stale,
}

/// Outcome of one live-wakeup pump.
#[derive(Debug)]
enum LiveTickOutcome {
    /// An incoming event matched the captured params (or carried
    /// no field projection, which we conservatively treat as a
    /// match per RFC 087 §6).
    Match,
    /// The underlying receiver was closed.
    Closed,
}

/// Per-subscription live wakeup. Owns the `LiveSubscription` (so
/// the underlying EventSource closes on drop) and the receiver
/// end of the bridge mpsc.
///
/// On host targets this is a stub — the LiveClient is wasm-only;
/// `next_matching()` returns `LiveTickOutcome::Closed` once.
struct LiveWakeup {
    /// `Some` when live wakeup is wired up; `None` for the
    /// host-stub / disabled-by-config path.
    receiver: Option<futures::channel::mpsc::UnboundedReceiver<LiveWakeupEvent>>,
    /// Captured params used for client-side filtering on every
    /// arriving event. Stored here (not just on the subscription)
    /// so the filter can be exercised in unit tests without an
    /// active LiveClient.
    captured_params: pocopine_sync::StreamParams,
    /// Owned EventSource handle. On host this is always `None`;
    /// on wasm dropping it closes the SSE stream so the live
    /// subscription's lifetime is tied to the driver's.
    #[cfg(target_arch = "wasm32")]
    _subscription: Option<pocopine_live::LiveSubscription>,
}

/// Payload pushed through the live-wakeup mpsc. Carries the
/// affected_fields projection (when the server includes one) so
/// the client-side filter can decide whether to trigger /pull.
#[derive(Debug)]
struct LiveWakeupEvent {
    affected_fields: std::collections::BTreeMap<String, Value>,
}

impl LiveWakeup {
    /// Build a no-op wakeup — used for host targets and the
    /// `disable_live = true` config knob.
    fn disabled() -> Self {
        Self {
            receiver: None,
            captured_params: pocopine_sync::StreamParams::new(),
            #[cfg(target_arch = "wasm32")]
            _subscription: None,
        }
    }

    /// Returns `true` when the wakeup channel will never deliver
    /// — host stub or `disable_live`.
    fn is_disabled(&self) -> bool {
        self.receiver.is_none()
    }

    /// Pump one matching live event. Drops every non-matching
    /// event until either a match arrives or the receiver closes.
    /// The "no projection" case forwards to /pull per the
    /// RFC 087 §6 conservative fallback.
    async fn next_matching(&mut self) -> LiveTickOutcome {
        use futures::stream::StreamExt;
        let Some(rx) = self.receiver.as_mut() else {
            // Never resolves — but the caller checks
            // `is_disabled()` first and short-circuits.
            futures::future::pending::<()>().await;
            return LiveTickOutcome::Closed;
        };
        loop {
            match rx.next().await {
                Some(event) => {
                    if live_event_matches_params(&self.captured_params, &event.affected_fields) {
                        return LiveTickOutcome::Match;
                    }
                    // Mismatched — drop and wait for the next.
                }
                None => return LiveTickOutcome::Closed,
            }
        }
    }

    /// Open a live subscription against the per-stream query-tag
    /// topic — same topic `pocopine_sync::SyncServer::invalidate_stream`
    /// publishes to. wasm-only; host falls back to `disabled()` in
    /// the caller.
    ///
    /// On open failure we drop the receiver and return
    /// `Self { receiver: None, ... }`. That makes
    /// [`Self::is_disabled`] true and [`Self::next_matching`]
    /// block forever, so the driver's `select!` resolves cleanly
    /// on the poll timer instead of busy-looping on a closed
    /// receiver (which would treat every immediate-`Closed`
    /// resolution as a poll trigger and hammer `/pull` without
    /// honoring the configured poll interval).
    #[cfg(target_arch = "wasm32")]
    fn open_on_stream(
        stream_name: String,
        captured_params: pocopine_sync::StreamParams,
        with_credentials: bool,
    ) -> Self {
        let (tx, rx) = futures::channel::mpsc::unbounded::<LiveWakeupEvent>();
        let live_tag = pocopine_sync::sync_stream_tag(stream_name.as_str());
        let connect_result = pocopine_live::LiveClient::new()
            .query_tag(live_tag.clone())
            .with_credentials(with_credentials)
            .on_event({
                let tx = tx.clone();
                move |event| {
                    let affected_fields = extract_affected_fields(&event);
                    let _ = tx.unbounded_send(LiveWakeupEvent { affected_fields });
                }
            })
            .open();
        match connect_result {
            Ok(subscription) => Self {
                receiver: Some(rx),
                captured_params,
                _subscription: Some(subscription),
            },
            Err(err) => {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = stream_name.as_str(),
                    live_tag = live_tag.as_str(),
                    error = ?err,
                    "sync-query driver: live wakeup open failed; relying on polling"
                );
                // CRITICAL: drop the receiver. Keeping `Some(rx)`
                // with closed senders makes `next_matching`
                // resolve `Closed` immediately; the driver's
                // select treats that as a poll trigger, then
                // loops back to `next_matching` which resolves
                // `Closed` again — a tight loop that hammers
                // `/pull` ignoring the configured poll interval.
                Self {
                    receiver: None,
                    captured_params,
                    _subscription: None,
                }
            }
        }
    }
}

/// Best-effort extraction of `affected_fields` from a live event.
/// The wire shape (RFC 071) carries this on the `payload`
/// envelope; the typed `LiveEvent` doesn't expose it directly, so
/// we rebuild from the event's `keys` (treated as a degenerate
/// projection) until the live protocol grows a richer field
/// projection in a follow-up RFC.
#[cfg(target_arch = "wasm32")]
fn extract_affected_fields(
    event: &pocopine_live::LiveEvent,
) -> std::collections::BTreeMap<String, Value> {
    use pocopine_live::LiveEvent;
    // Today's wire shape: server publishes the affected `keys`
    // for the collection. Until RFC 071 grows a field-level
    // projection we conservatively treat the projection as
    // empty — every collection event triggers a pull, which the
    // client-side filter then narrows on the SUBSEQUENT match
    // attempt. (The filter logic returns `true` when the
    // projection is missing-key, per RFC 087 §6.)
    let _ = event;
    match event {
        LiveEvent::CollectionChanged { .. }
        | LiveEvent::CollectionDeleted { .. }
        | LiveEvent::QueryInvalidated { .. }
        | LiveEvent::Ready { .. } => std::collections::BTreeMap::new(),
        LiveEvent::Gap { .. } | LiveEvent::Error { .. } | LiveEvent::Custom { .. } => {
            std::collections::BTreeMap::new()
        }
    }
}

// ─── spawn shim ─────────────────────────────────────────────────────
//
// Per RFC 087 §2, the driver is spawned via a `#[cfg]`-split
// helper instead of a public `Runtime` trait. Both forms park the
// driver on the current task's executor:
//
// * wasm — `wasm_bindgen_futures::spawn_local` (single-threaded by
//   construction).
// * native — `tokio::task::spawn_local` (requires the caller to
//   have an active `LocalSet`).
//
// The driver type itself holds `Rc<...>`/`Weak<...>` interior, so
// the future is `!Send`. This is why the native path uses
// `spawn_local`, not `tokio::spawn`.

/// Spawn the driver on the runtime's local task set.
#[cfg(target_arch = "wasm32")]
pub(crate) fn spawn_driver<F>(fut: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(fut);
}

/// Spawn the driver on the host's `tokio::task::LocalSet`. Panics
/// if no LocalSet is active — host tests must wrap their body in
/// `LocalSet::new().run_until(async { ... }).await`. This is
/// documented on [`QueryClient::with_config`](crate::QueryClient::with_config)
/// because the panic location otherwise points deep into tokio.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn spawn_driver<F>(fut: F)
where
    F: Future<Output = ()> + 'static,
{
    // `spawn_local` returns a JoinHandle that we deliberately
    // discard — cancellation flows through `DriverEpoch::bump`,
    // not through aborting the future. The `drop` makes the
    // discard explicit to the clippy `let_underscore_future`
    // lint (which would otherwise flag `let _ = ...` as
    // potentially-forgetting-an-await).
    drop(tokio::task::spawn_local(fut));
}

/// Asynchronously sleep for `duration` on both wasm and native.
/// On wasm we use `gloo-timers` equivalent via a small JS bridge;
/// on native we use `tokio::time::sleep` directly.
#[cfg(target_arch = "wasm32")]
async fn sleep(duration: Duration) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    let ms = duration.as_millis() as i32;
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(win) = web_sys::window() {
            let resolve_for_timeout = resolve.clone();
            let closure = Closure::once_into_js(move || {
                let _ = resolve_for_timeout.call0(&wasm_bindgen::JsValue::NULL);
            });
            let _ = win
                .set_timeout_with_callback_and_timeout_and_arguments_0(closure.unchecked_ref(), ms);
        } else {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

/// Filter a live-wakeup event against the captured params.
///
/// Public for reuse from the wasm wiring; host code paths call it
/// in tests to verify filtering logic without going through a real
/// SSE source.
///
/// `affected_fields` carries the server's per-row field projection
/// for the touched row; the filter is `true` (forward to /pull)
/// when every captured param either matches or is absent from the
/// server's projection. A missing field is treated as "server didn't
/// report this field; conservatively allow" per RFC 087 §6.
pub fn live_event_matches_params(
    captured_params: &pocopine_sync::StreamParams,
    affected_fields: &std::collections::BTreeMap<String, Value>,
) -> bool {
    for (key, want) in captured_params {
        match affected_fields.get(key.as_str()) {
            None => continue,
            Some(value) if value == want => continue,
            Some(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_snapshot_observes_bump() {
        let epoch = DriverEpoch::new();
        let snap = epoch.snapshot();
        assert!(snap.is_current());
        epoch.bump();
        assert!(!snap.is_current(), "post-bump, the snapshot is stale");
    }

    #[test]
    fn epoch_multiple_snapshots_share_counter() {
        let epoch = DriverEpoch::new();
        let snap_a = epoch.snapshot();
        let snap_b = epoch.snapshot();
        epoch.bump();
        assert!(!snap_a.is_current());
        assert!(!snap_b.is_current());
    }

    #[test]
    fn config_default_uses_canonical_endpoint() {
        let cfg = QueryClientConfig::default();
        assert_eq!(cfg.endpoint, DEFAULT_SYNC_ENDPOINT);
        assert_eq!(cfg.poll_interval, Some(DEFAULT_POLL_INTERVAL));
        assert!(!cfg.disable_live);
        assert!(cfg.with_credentials);
    }

    #[test]
    fn live_filter_drops_mismatched_params() {
        let mut want = pocopine_sync::StreamParams::new();
        want.insert("workspace_id".into(), serde_json::json!("W1"));

        let mut got = std::collections::BTreeMap::new();
        got.insert("workspace_id".to_string(), serde_json::json!("W2"));
        assert!(!live_event_matches_params(&want, &got));

        let mut got = std::collections::BTreeMap::new();
        got.insert("workspace_id".to_string(), serde_json::json!("W1"));
        assert!(live_event_matches_params(&want, &got));
    }

    #[test]
    fn live_filter_allows_absent_fields() {
        let mut want = pocopine_sync::StreamParams::new();
        want.insert("workspace_id".into(), serde_json::json!("W1"));

        // Server didn't include workspace_id in affected_fields —
        // allow per RFC 087 §6.
        let got = std::collections::BTreeMap::new();
        assert!(live_event_matches_params(&want, &got));
    }

    #[test]
    fn driver_error_is_transient_for_network_and_server() {
        assert!(DriverError::Network("offline".into()).is_transient());
        assert!(DriverError::Server("503".into()).is_transient());
        assert!(!DriverError::Client("bad url".into()).is_transient());
        assert!(!DriverError::Cancelled.is_transient());
    }

    #[test]
    fn build_url_passthrough_when_path_matches_prefix() {
        let url = build_url(DEFAULT_SYNC_ENDPOINT, SYNC_OPEN_PATH);
        assert_eq!(url, SYNC_OPEN_PATH);
    }

    #[test]
    fn build_url_rewrites_when_endpoint_overrides_prefix() {
        let url = build_url("/custom/sync", SYNC_OPEN_PATH);
        assert!(url.starts_with("/custom/sync/"));
        assert!(url.ends_with("/open"));
    }
}
