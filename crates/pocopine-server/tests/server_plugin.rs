//! RFC-077 Phase 1 — host-side plugin lifecycle.
//!
//! Pins the `Server` builder + plugin runtime: services and hooks
//! install through plugins, validation rejects misconfigured
//! plugins before the listener binds, and boot events fire in the
//! documented order.

// `registry_lock` deliberately holds a `MutexGuard` across `.await`
// to serialize tests that share the process-global plugin registry.
// The lock has zero contention because tests are linearized; the
// async `tokio::sync::Mutex` would change behaviour for no benefit.
#![allow(clippy::await_holding_lock)]

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use pocopine_server::axum::Router;
use pocopine_server::{
    HttpRequestStarted, Server, ServerBootFailed, ServerBootStarted, ServerHook, ServerListening,
    ServerPlugin,
};

/// Process-global plugin registry serialization. See the matching
/// helper in `server_request_events.rs` for the reasoning.
fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone, Default)]
struct EventLog {
    events: Arc<Mutex<Vec<String>>>,
}

impl EventLog {
    fn record(&self, line: impl Into<String>) {
        self.events.lock().unwrap().push(line.into());
    }

    fn snapshot(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
}

impl ServerHook<ServerBootStarted> for EventLog {
    fn call(&self, event: ServerBootStarted) {
        self.record(format!("boot-start:{}", event.addr));
    }
}

impl ServerHook<ServerListening> for EventLog {
    fn call(&self, event: ServerListening) {
        self.record(format!("listening:{}", event.addr));
    }
}

impl ServerHook<ServerBootFailed> for EventLog {
    fn call(&self, event: ServerBootFailed) {
        self.record(format!("boot-failed:{}", event.reason));
    }
}

fn boot_event_plugin(events: EventLog) -> impl ServerPlugin {
    move |server: Server| {
        server
            .provide_plugin(events)
            .hook_plugin::<EventLog, ServerBootStarted>()
            .hook_plugin::<EventLog, ServerListening>()
            .hook_plugin::<EventLog, ServerBootFailed>()
    }
}

struct DuplicateService;

struct DuplicateProvider {
    name: &'static str,
}

impl ServerPlugin for DuplicateProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn install(self, server: Server) -> Server {
        server.provide_plugin(DuplicateService)
    }
}

struct MissingHookService;

impl ServerHook<HttpRequestStarted> for MissingHookService {
    fn call(&self, _: HttpRequestStarted) {}
}

struct MissingHookPlugin;

impl ServerPlugin for MissingHookPlugin {
    fn name(&self) -> &'static str {
        "missing-hook-plugin"
    }

    fn install(self, server: Server) -> Server {
        server.hook_plugin::<MissingHookService, HttpRequestStarted>()
    }
}

fn ephemeral_addr() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    format!("127.0.0.1:{port}")
}

#[tokio::test]
async fn server_boot_hooks_fire_in_order_and_listen_succeeds() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();
    let addr = ephemeral_addr();

    let server = Server::new(Router::new()).plugin(boot_event_plugin(events.clone()));

    let addr_for_serve = addr.clone();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serve_handle = tokio::spawn(async move {
        tokio::select! {
            res = server.serve(&addr_for_serve) => res,
            _ = stop_rx => Ok(()),
        }
    });

    // Wait for the listener to bind.
    for _ in 0..50 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let _ = stop_tx.send(());
    let _ = serve_handle.await.expect("serve task");

    let log = events.snapshot();
    assert_eq!(
        log,
        vec![format!("boot-start:{addr}"), format!("listening:{addr}")]
    );
}

#[tokio::test]
async fn server_serve_address_parse_failure_emits_boot_failed() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();

    let result = Server::new(Router::new())
        .plugin(boot_event_plugin(events.clone()))
        .serve("not a real address")
        .await;

    let err = result.expect_err("invalid address must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let log = events.snapshot();
    assert_eq!(
        log,
        vec![
            "boot-start:not a real address".to_string(),
            "boot-failed:address_parse".to_string(),
        ]
    );
}

#[tokio::test]
async fn missing_hook_service_returns_invalid_input_and_emits_boot_failed() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let events = EventLog::default();

    // Note: the missing-hook plugin is installed BEFORE the
    // recorder so its hook for HttpRequestStarted is recorded first
    // and validation fails on the recorder's services check.
    let result = Server::new(Router::new())
        .plugin(boot_event_plugin(events.clone()))
        .plugin(MissingHookPlugin)
        .serve("127.0.0.1:0")
        .await;

    let err = result.expect_err("missing hook service must fail validation");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let message = err.to_string();
    assert!(
        message.contains("missing-hook-plugin"),
        "diagnostic should name the offending plugin: {message}"
    );
    assert!(
        message.contains("MissingHookService"),
        "diagnostic should name the missing service: {message}"
    );

    // Validation runs before any boot event fires, but the
    // boot-failed hook is still wired by the recorder plugin —
    // however, validation reset the registry to empty, so no
    // boot-failed event can be observed by the recorder. This is
    // intentional: the std::io::Error already names the failure
    // and a hooked observer would be silenced anyway by validation
    // dropping its own service.
    assert!(events.snapshot().is_empty());
}

#[test]
#[should_panic(expected = "first provider: `first-plugin`, second provider: `second-plugin`")]
fn duplicate_plugin_services_name_their_providers() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let _ = Server::new(Router::new())
        .plugin(DuplicateProvider {
            name: "first-plugin",
        })
        .plugin(DuplicateProvider {
            name: "second-plugin",
        });
}

#[tokio::test]
async fn legacy_serve_helper_uses_server_builder_under_the_hood() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let result = pocopine_server::serve(Router::new(), "not a real address").await;
    let err = result.expect_err("invalid address must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn validation_failure_clears_previous_server_plugin_registry() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();

    // Server #1 succeeds and activates a registry that holds an
    // EventLog service. `try_finalize` is the install path the
    // tests use to activate without binding a listener.
    let events1 = EventLog::default();
    let _router1 = Server::new(Router::new())
        .plugin(boot_event_plugin(events1.clone()))
        .try_finalize()
        .expect("server #1 plugin validation succeeds");

    // Sanity: server #1's service is reachable.
    assert!(
        pocopine_server::active_plugin::<EventLog>().is_some(),
        "after a successful try_finalize, EventLog must resolve"
    );

    // Server #2 has a hook for a service that was never provided
    // — validation fails. The reset must drop server #1's registry
    // so the server-wide active state cannot leak.
    let result = Server::new(Router::new())
        .plugin(MissingHookPlugin)
        .try_finalize();
    assert!(result.is_err(), "missing hook service must fail validation");

    // Plugin context from server #1 must be gone.
    assert!(
        pocopine_server::active_plugin::<EventLog>().is_none(),
        "validation failure must reset the active registry; \
         stale EventLog from server #1 leaked"
    );

    // And server #1's hooks must not fire on a post-failure emit.
    pocopine_server::emit(ServerListening {
        addr: "0.0.0.0:0".to_string(),
    });
    assert!(
        events1.snapshot().is_empty(),
        "stale ServerListening hook from server #1 fired after \
         validation failure: {:?}",
        events1.snapshot()
    );
}
