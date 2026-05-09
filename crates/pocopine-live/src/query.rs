use serde::{Deserialize, Serialize};

pub const QUERY_REASON_INITIAL: &str = "initial";
pub const QUERY_REASON_MANUAL: &str = "manual";
pub const QUERY_REASON_LIVE: &str = "live";
pub const QUERY_REASON_GAP: &str = "gap";
pub const QUERY_REASON_STREAM_ERROR: &str = "stream_error";

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
    pub last_reason: String,
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
            last_reason: String::new(),
            request_generation: 0,
        }
    }

    pub fn begin_initial(&mut self) -> QueryRequest {
        self.begin(QUERY_REASON_INITIAL, false)
    }

    pub fn begin_refresh(&mut self, reason: impl Into<String>) -> QueryRequest {
        self.begin(reason, false)
    }

    pub fn begin_live_refresh(&mut self, reason: impl Into<String>) -> QueryRequest {
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

    pub fn mark_stale(&mut self, reason: impl Into<String>) {
        self.stale = true;
        self.last_reason = reason.into();
    }

    pub fn record_stream_error(&mut self, error: impl ToString) {
        self.loading = false;
        self.refreshing = false;
        self.last_reason = QUERY_REASON_STREAM_ERROR.to_string();
        self.error = error.to_string();
    }

    pub fn is_current(&self, request: QueryRequest) -> bool {
        self.request_generation == request.generation
    }

    fn begin(&mut self, reason: impl Into<String>, stale_during_refresh: bool) -> QueryRequest {
        self.request_generation = self.request_generation.saturating_add(1);
        self.refresh_count = self.refresh_count.saturating_add(1);
        self.last_reason = reason.into();
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
        assert_eq!(state.last_reason, QUERY_REASON_INITIAL);
    }

    #[test]
    fn query_state_ignores_stale_success() {
        let mut state = QueryState::new(vec![1]);
        let stale = state.begin_initial();
        let current = state.begin_refresh(QUERY_REASON_MANUAL);

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

        let request = state.begin_refresh(QUERY_REASON_MANUAL);
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

        let live = state.begin_live_refresh(QUERY_REASON_LIVE);

        assert_eq!(state.live_event_count, 1);
        assert_eq!(state.last_reason, QUERY_REASON_LIVE);
        assert!(state.stale);
        assert!(state.refreshing);
        assert!(state.finish_success(live, vec![2]));
        assert!(!state.stale);
    }
}
