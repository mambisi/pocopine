use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

#[cfg(target_arch = "wasm32")]
use {
    pocopine_core::{current_scope_id, Handle, Scope},
    std::{future::Future, rc::Rc},
};

use crate::LiveClientError;

#[cfg(any(test, target_arch = "wasm32"))]
use crate::{live_refresh_targets, LiveEvent};

/// Reason attached to the most recent query state transition.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryReason {
    #[default]
    Idle,
    Initial,
    Manual,
    Live,
    Gap,
    StreamError,
}

/// Opaque generation token for one query request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueryRequest {
    generation: u64,
}

impl QueryRequest {
    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// Serializable component field for server-function backed data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[serde(bound(
    serialize = "T: Serialize",
    deserialize = "T: Default + Deserialize<'de>"
))]
pub struct QueryState<T> {
    pub data: T,
    pub loading: bool,
    pub refreshing: bool,
    pub stale: bool,
    pub error: String,
    pub version: u64,
    pub refresh_count: u64,
    pub live_event_count: u64,
    pub last_reason: QueryReason,
    #[serde(skip)]
    request_generation: u64,
}

impl<T: Default> Default for QueryState<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> QueryState<T> {
    pub fn new(data: T) -> Self {
        Self {
            data,
            loading: false,
            refreshing: false,
            stale: false,
            error: String::new(),
            version: 0,
            refresh_count: 0,
            live_event_count: 0,
            last_reason: QueryReason::Idle,
            request_generation: 0,
        }
    }

    pub fn begin_initial(&mut self) -> QueryRequest {
        self.begin(QueryReason::Initial, false)
    }

    pub fn begin_refresh(&mut self, reason: QueryReason) -> QueryRequest {
        self.begin(reason, false)
    }

    pub fn begin_live_refresh(&mut self, reason: QueryReason) -> QueryRequest {
        self.live_event_count = self.live_event_count.saturating_add(1);
        self.begin(reason, true)
    }

    pub fn finish_success(&mut self, request: QueryRequest, data: T) -> bool {
        if !self.is_current(request) {
            return false;
        }

        self.data = data;
        self.loading = false;
        self.refreshing = false;
        self.stale = false;
        self.error.clear();
        self.version = self.version.saturating_add(1);
        true
    }

    pub fn finish_error(&mut self, request: QueryRequest, error: impl ToString) -> bool {
        if !self.is_current(request) {
            return false;
        }

        self.loading = false;
        self.refreshing = false;
        self.stale = self.version > 0 || self.stale;
        self.error = error.to_string();
        true
    }

    pub fn set_error(&mut self, error: impl Into<String>) {
        self.loading = false;
        self.refreshing = false;
        self.error = error.into();
    }

    pub fn clear_error(&mut self) {
        self.error.clear();
    }

    pub fn mark_stale(&mut self, reason: QueryReason) {
        self.stale = true;
        self.last_reason = reason;
    }

    pub fn record_stream_error(&mut self, error: impl ToString) {
        self.loading = false;
        self.refreshing = false;
        self.last_reason = QueryReason::StreamError;
        self.error = error.to_string();
    }

    pub fn is_current(&self, request: QueryRequest) -> bool {
        self.request_generation == request.generation
    }

    fn begin(&mut self, reason: QueryReason, stale_during_refresh: bool) -> QueryRequest {
        self.request_generation = self.request_generation.saturating_add(1);
        self.refresh_count = self.refresh_count.saturating_add(1);
        self.last_reason = reason;
        self.error.clear();
        self.stale = stale_during_refresh;

        if self.version == 0 {
            self.loading = true;
            self.refreshing = false;
        } else {
            self.loading = false;
            self.refreshing = true;
        }

        QueryRequest {
            generation: self.request_generation,
        }
    }
}

/// Scope-bound browser query runner for server-function backed data.
pub type QuerySelector<C, T> = for<'a> fn(&'a mut C) -> &'a mut QueryState<T>;

/// Scope-bound browser query runner for server-function backed data.
pub struct LiveQuery<C = (), T = (), F = ()> {
    selector: QuerySelector<C, T>,
    fetch: F,
    endpoint: Option<String>,
    collections: Vec<String>,
    query_tags: Vec<String>,
    last_event_id: Option<String>,
    with_credentials: bool,
    refetch_on_open: bool,
    _marker: PhantomData<fn(C) -> T>,
}

impl LiveQuery<(), (), ()> {
    pub fn scoped<C, T, F>(selector: QuerySelector<C, T>, fetch: F) -> LiveQuery<C, T, F> {
        LiveQuery {
            selector,
            fetch,
            endpoint: None,
            collections: Vec::new(),
            query_tags: Vec::new(),
            last_event_id: None,
            with_credentials: false,
            refetch_on_open: true,
            _marker: PhantomData,
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn refresh_scoped<C, T, F, Fut, E>(
        selector: QuerySelector<C, T>,
        fetch: F,
    ) -> Result<(), LiveClientError>
    where
        C: 'static,
        T: 'static,
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = Result<T, E>> + 'static,
        E: ToString + 'static,
    {
        let handle = current_component_handle::<C>()?;
        start_query(handle, selector, Rc::new(fetch), QueryReason::Manual, false);
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn refresh_scoped<C, T, F, Fut, E>(
        selector: QuerySelector<C, T>,
        fetch: F,
    ) -> Result<(), LiveClientError>
    where
        C: 'static,
        T: 'static,
        F: Fn() -> Fut + 'static,
        Fut: std::future::Future<Output = Result<T, E>> + 'static,
        E: ToString + 'static,
    {
        let _ = (selector, fetch);
        Ok(())
    }
}

impl<C, T, F> LiveQuery<C, T, F> {
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }

    pub fn collection(mut self, collection: impl Into<String>) -> Self {
        self.collections.push(collection.into());
        self
    }

    pub fn query_tag(mut self, tag: impl Into<String>) -> Self {
        self.query_tags.push(tag.into());
        self
    }

    pub fn last_event_id(mut self, cursor: impl Into<String>) -> Self {
        self.last_event_id = Some(cursor.into());
        self
    }

    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    pub fn refetch_on_open(mut self, enabled: bool) -> Self {
        self.refetch_on_open = enabled;
        self
    }

    #[cfg(target_arch = "wasm32")]
    pub fn open<Fut, E>(self) -> Result<(), LiveClientError>
    where
        C: 'static,
        T: 'static,
        F: Fn() -> Fut + 'static,
        Fut: Future<Output = Result<T, E>> + 'static,
        E: ToString + 'static,
    {
        let handle = current_component_handle::<C>()?;
        let selector = self.selector;
        let fetch = Rc::new(self.fetch);

        if self.refetch_on_open {
            start_query(
                handle.clone(),
                selector,
                fetch.clone(),
                QueryReason::Initial,
                false,
            );
        }

        if self.collections.is_empty() && self.query_tags.is_empty() {
            return Ok(());
        }

        let mut client = crate::LiveClient::new();
        if let Some(endpoint) = self.endpoint {
            client = client.endpoint(endpoint);
        }
        if let Some(cursor) = self.last_event_id {
            client = client.last_event_id(cursor);
        }
        client = client.with_credentials(self.with_credentials);
        for collection in &self.collections {
            client = client.collection(collection.clone());
        }
        for query_tag in &self.query_tags {
            client = client.query_tag(query_tag.clone());
        }

        let collections = Rc::new(self.collections);
        let query_tags = Rc::new(self.query_tags);
        client.connect_scoped(move |event| {
            match live_query_action(&event, &collections, &query_tags) {
                LiveQueryAction::Refresh(reason) => {
                    start_query(handle.clone(), selector, fetch.clone(), reason, true);
                }
                LiveQueryAction::Error(error) => {
                    handle.update(|state| {
                        selector(state).record_stream_error(error);
                    });
                }
                LiveQueryAction::Ignore => {}
            }
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn open<Fut, E>(self) -> Result<(), LiveClientError>
    where
        C: 'static,
        T: 'static,
        F: Fn() -> Fut + 'static,
        Fut: std::future::Future<Output = Result<T, E>> + 'static,
        E: ToString + 'static,
    {
        let LiveQuery {
            selector,
            fetch,
            endpoint,
            collections,
            query_tags,
            last_event_id,
            with_credentials,
            refetch_on_open,
            _marker,
        } = self;
        let _ = (
            selector,
            fetch,
            endpoint,
            collections,
            query_tags,
            last_event_id,
            with_credentials,
            refetch_on_open,
        );
        Ok(())
    }
}

#[cfg(any(test, target_arch = "wasm32"))]
#[derive(Clone, Debug, Eq, PartialEq)]
enum LiveQueryAction {
    Refresh(QueryReason),
    Error(String),
    Ignore,
}

#[cfg(any(test, target_arch = "wasm32"))]
fn live_query_action(
    event: &LiveEvent,
    collections: &[String],
    query_tags: &[String],
) -> LiveQueryAction {
    let targets = live_refresh_targets(event);

    if let Some(reason) = targets.error {
        return LiveQueryAction::Error(reason);
    }

    if targets.gap.is_some() {
        return LiveQueryAction::Refresh(QueryReason::Gap);
    }

    let matches_collection = targets
        .collections
        .iter()
        .any(|target| collections.iter().any(|value| value == target));
    let matches_query_tag = targets
        .query_tags
        .iter()
        .any(|target| query_tags.iter().any(|value| value == target));

    if matches_collection || matches_query_tag {
        LiveQueryAction::Refresh(QueryReason::Live)
    } else {
        LiveQueryAction::Ignore
    }
}

#[cfg(target_arch = "wasm32")]
fn current_component_handle<C: 'static>() -> Result<Handle<C>, LiveClientError> {
    let scope_id = current_scope_id().ok_or_else(|| {
        LiveClientError::Browser(
            "LiveQuery::scoped used outside a component handler or lifecycle hook".to_string(),
        )
    })?;
    let scope = Scope::find(scope_id)
        .ok_or_else(|| LiveClientError::Browser("current component scope is gone".to_string()))?;
    let typed = scope.typed::<C>().ok_or_else(|| {
        LiveClientError::Browser(format!(
            "LiveQuery::scoped expected current scope to be {}",
            std::any::type_name::<C>()
        ))
    })?;
    Ok(Handle::new(typed, scope_id))
}

#[cfg(target_arch = "wasm32")]
fn start_query<C, T, F, Fut, E>(
    handle: Handle<C>,
    selector: QuerySelector<C, T>,
    fetch: Rc<F>,
    reason: QueryReason,
    live_event: bool,
) where
    C: 'static,
    T: 'static,
    F: Fn() -> Fut + 'static,
    Fut: Future<Output = Result<T, E>> + 'static,
    E: ToString + 'static,
{
    let request = handle.update(|state| {
        let query = selector(state);
        if live_event {
            query.begin_live_refresh(reason)
        } else {
            query.begin_refresh(reason)
        }
    });
    let scope_id = handle.scope_id();
    pocopine_core::spawn_for_scope(scope_id, async move {
        let result = (fetch)().await;
        if Scope::find(scope_id).is_none() {
            return;
        }

        handle.update(|state| {
            let query = selector(state);
            match result {
                Ok(data) => {
                    query.finish_success(request, data);
                }
                Err(error) => {
                    query.finish_error(request, error);
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_state_defaults_to_idle_empty_data() {
        let state = QueryState::<Vec<u32>>::default();

        assert_eq!(state.data, Vec::<u32>::new());
        assert!(!state.loading);
        assert!(!state.refreshing);
        assert!(!state.stale);
        assert!(state.error.is_empty());
        assert_eq!(state.version, 0);
        assert_eq!(state.refresh_count, 0);
        assert_eq!(state.live_event_count, 0);
    }

    #[test]
    fn query_reason_serializes_as_snake_case_string() {
        let value = serde_json::to_value(QueryReason::StreamError).unwrap();

        assert_eq!(value, serde_json::json!("stream_error"));
    }

    #[test]
    fn query_state_success_updates_data_and_clears_flags() {
        let mut state = QueryState::new(vec![1]);
        let request = state.begin_initial();

        assert!(state.loading);
        assert!(state.finish_success(request, vec![2, 3]));

        assert_eq!(state.data, vec![2, 3]);
        assert!(!state.loading);
        assert!(!state.refreshing);
        assert!(!state.stale);
        assert!(state.error.is_empty());
        assert_eq!(state.version, 1);
        assert_eq!(state.refresh_count, 1);
        assert_eq!(state.last_reason, QueryReason::Initial);
    }

    #[test]
    fn query_state_ignores_stale_success() {
        let mut state = QueryState::new(vec![1]);
        let stale = state.begin_initial();
        let current = state.begin_refresh(QueryReason::Manual);

        assert!(!state.finish_success(stale, vec![9]));
        assert_eq!(state.data, vec![1]);
        assert!(state.finish_success(current, vec![2]));
        assert_eq!(state.data, vec![2]);
    }

    #[test]
    fn query_state_error_preserves_existing_data() {
        let mut state = QueryState::new(vec![1]);
        let request = state.begin_initial();
        assert!(state.finish_success(request, vec![2]));

        let request = state.begin_refresh(QueryReason::Manual);
        assert!(state.finish_error(request, "network down"));

        assert_eq!(state.data, vec![2]);
        assert_eq!(state.error, "network down");
        assert!(state.stale);
        assert!(!state.loading);
        assert!(!state.refreshing);
    }

    #[test]
    fn live_refresh_counts_event_and_marks_stale_while_fetching() {
        let mut state = QueryState::new(vec![1]);
        let request = state.begin_initial();
        assert!(state.finish_success(request, vec![1]));

        let live = state.begin_live_refresh(QueryReason::Live);

        assert_eq!(state.live_event_count, 1);
        assert_eq!(state.last_reason, QueryReason::Live);
        assert!(state.stale);
        assert!(state.refreshing);
        assert!(state.finish_success(live, vec![2]));
        assert!(!state.stale);
    }

    #[test]
    fn live_query_action_matches_collection_or_query_tag() {
        let event = LiveEvent::CollectionChanged {
            collection: "posts".to_string(),
            op: crate::LiveOp::Upsert,
            keys: vec![],
            query_tags: vec!["posts:list".to_string()],
            schema_version: 1,
            cursor: None,
            created_at_ms: None,
        };

        assert_eq!(
            live_query_action(&event, &[], &["posts:list".to_string()]),
            LiveQueryAction::Refresh(QueryReason::Live)
        );
        assert_eq!(
            live_query_action(&event, &["posts".to_string()], &[]),
            LiveQueryAction::Refresh(QueryReason::Live)
        );
        assert_eq!(live_query_action(&event, &[], &[]), LiveQueryAction::Ignore);
    }

    #[test]
    fn live_query_action_refreshes_on_gap_and_records_error() {
        assert_eq!(
            live_query_action(
                &LiveEvent::Gap {
                    reason: "retention_gap".to_string()
                },
                &[],
                &[]
            ),
            LiveQueryAction::Refresh(QueryReason::Gap)
        );
        assert_eq!(
            live_query_action(
                &LiveEvent::Error {
                    reason: "forbidden_topic".to_string()
                },
                &[],
                &[]
            ),
            LiveQueryAction::Error("forbidden_topic".to_string())
        );
    }
}
