#![cfg(not(target_arch = "wasm32"))]

//! HTTP integration tests for `MachinesClient` (RFC 080 Phase 4
//! follow-up). Each test spins up a `wiremock` server, points the
//! blocking client at its base URL, and asserts on the exact HTTP
//! verb / path / body shape we send. Field names come from Fly's
//! OpenAPI spec; the pinned snapshot lives at
//! `tests/fixtures/fly-openapi-v1.json` and is what
//! `openapi_spec_contains_endpoints_we_use` checks against.
//!
//! Goal: catch the next renaming/schema move (cf. RFC 080 §11 q7)
//! before it surfaces as a 4xx in production.
//!
//! Implementation note: `wiremock` is async; our client is blocking.
//! We start the mock server in a `#[tokio::test]` and call the
//! blocking client through `spawn_blocking` so the runtime stays
//! responsive while reqwest blocks on socket I/O.

use std::collections::BTreeMap;

use pocopine_deploy_fly::machines::{
    CreateAppRequest, CreateMachineRequest, CreateVolumeRequest, MachineConfig, MachineInit,
    MachinePort, MachineService, MachineServiceCheck, MachinesClient,
};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_TOKEN: &str = "fly-test-token";

/// Run a blocking closure off the mock server's runtime so reqwest's
/// blocking calls don't dead-lock the tokio reactor.
async fn call_client<F, T>(server_uri: String, f: F) -> T
where
    F: FnOnce(MachinesClient) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let client = MachinesClient::with_base_url(&server_uri, TEST_TOKEN);
        f(client)
    })
    .await
    .expect("blocking task should not panic")
}

// ─── Apps ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn get_app_returns_some_on_200() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp"))
        .and(bearer_token(TEST_TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "app_123",
            "name": "myapp",
            "status": "deployed"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let app = call_client(server.uri(), |c| c.get_app("myapp"))
        .await
        .unwrap();
    let app = app.expect("Some(App)");
    assert_eq!(app.name, "myapp");
    assert_eq!(app.id.as_deref(), Some("app_123"));
}

#[tokio::test]
async fn get_app_returns_none_on_404() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/missing"))
        .and(bearer_token(TEST_TOKEN))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let app = call_client(server.uri(), |c| c.get_app("missing"))
        .await
        .unwrap();
    assert!(app.is_none());
}

#[tokio::test]
async fn create_app_posts_to_apps_with_request_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apps"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_partial_json(json!({
            "app_name": "myapp",
            "org_slug": "personal"
        })))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let result = call_client(server.uri(), |c| {
        c.create_app(&CreateAppRequest {
            name: "myapp".into(),
            org_slug: "personal".into(),
            network: None,
            enable_subdomains: None,
        })
    })
    .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn ensure_app_creates_then_returns_the_app_when_missing() {
    let server = MockServer::start().await;

    // First GET → 404.
    Mock::given(method("GET"))
        .and(path("/apps/myapp"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // POST /apps → 201.
    Mock::given(method("POST"))
        .and(path("/apps"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;
    // Subsequent GET /apps/myapp → 200.
    Mock::given(method("GET"))
        .and(path("/apps/myapp"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "name": "myapp", "id": "app_new" })),
        )
        .mount(&server)
        .await;

    let app = call_client(server.uri(), |c| c.ensure_app("myapp", "personal"))
        .await
        .unwrap();
    assert_eq!(app.name, "myapp");
}

#[tokio::test]
async fn ensure_app_short_circuits_when_app_already_exists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "name": "myapp", "id": "a" })),
        )
        .mount(&server)
        .await;
    // No POST /apps mount: a request hitting it would 404 from wiremock
    // and the test would fail.

    let app = call_client(server.uri(), |c| c.ensure_app("myapp", "personal"))
        .await
        .unwrap();
    assert_eq!(app.name, "myapp");
}

// ─── Machines ───────────────────────────────────────────────────────────

#[tokio::test]
async fn create_machine_posts_to_apps_machines_with_init_object() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apps/myapp/machines"))
        .and(bearer_token(TEST_TOKEN))
        // Regression: init must serialise as an object with cmd, not a
        // bare string array. body_partial_json asserts the request body
        // is a superset of this fragment.
        .and(body_partial_json(json!({
            "name":   "web-1",
            "region": "fra",
            "config": {
                "image": "registry.fly.io/myapp:sha",
                "init":  { "cmd": ["web"] }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m_abc",
            "name": "web-1",
            "state": "starting",
            "instance_id": "i_1"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let req = CreateMachineRequest {
        name: "web-1".into(),
        region: "fra".into(),
        config: MachineConfig {
            image: "registry.fly.io/myapp:sha".into(),
            init: Some(MachineInit {
                cmd: vec!["web".into()],
                ..Default::default()
            }),
            services: vec![MachineService {
                internal_port: 8080,
                protocol: "tcp".into(),
                ports: vec![MachinePort {
                    port: 443,
                    handlers: vec!["tls".into(), "http".into()],
                }],
                autostart: Some(true),
                autostop: Some("suspend".into()),
                checks: vec![MachineServiceCheck {
                    kind: "http".into(),
                    interval: "10s".into(),
                    timeout: "5s".into(),
                    grace_period: Some("5s".into()),
                    method: Some("GET".into()),
                    path: Some("/healthz".into()),
                }],
            }],
            ..Default::default()
        },
        skip_launch: None,
        skip_service_registration: None,
        min_secrets_version: None,
    };

    let m = call_client(server.uri(), move |c| c.create_machine("myapp", &req))
        .await
        .unwrap();
    assert_eq!(m.id, "m_abc");
    assert_eq!(m.instance_id.as_deref(), Some("i_1"));
}

#[tokio::test]
async fn update_machine_posts_to_machine_id_path() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apps/myapp/machines/m_abc"))
        .and(bearer_token(TEST_TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "m_abc",
            "name": "web-1",
            "state": "starting",
            "instance_id": "i_2"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let req = CreateMachineRequest {
        name: "web-1".into(),
        region: "fra".into(),
        config: MachineConfig {
            image: "registry.fly.io/myapp:sha2".into(),
            ..Default::default()
        },
        skip_launch: None,
        skip_service_registration: None,
        min_secrets_version: None,
    };
    let m = call_client(server.uri(), move |c| {
        c.update_machine("myapp", "m_abc", &req)
    })
    .await
    .unwrap();
    assert_eq!(m.instance_id.as_deref(), Some("i_2"));
}

#[tokio::test]
async fn destroy_machine_sends_delete_with_force_param() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/apps/myapp/machines/m_abc"))
        .and(query_param("force", "true"))
        .and(bearer_token(TEST_TOKEN))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    call_client(server.uri(), |c| c.destroy_machine("myapp", "m_abc", true))
        .await
        .unwrap();
}

#[tokio::test]
async fn destroy_machine_treats_404_as_success() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/apps/myapp/machines/m_gone"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    call_client(server.uri(), |c| c.destroy_machine("myapp", "m_gone", true))
        .await
        .unwrap();
}

#[tokio::test]
async fn wait_machine_threads_instance_id_state_and_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/machines/m_abc/wait"))
        .and(query_param("instance_id", "i_1"))
        .and(query_param("state", "started"))
        .and(query_param("timeout", "60"))
        .and(bearer_token(TEST_TOKEN))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    call_client(server.uri(), |c| {
        c.wait_machine("myapp", "m_abc", "i_1", "started", 60)
    })
    .await
    .unwrap();
}

// ─── Secrets ────────────────────────────────────────────────────────────

#[tokio::test]
async fn set_secrets_posts_values_map_and_parses_version() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apps/myapp/secrets"))
        .and(bearer_token(TEST_TOKEN))
        // Body must wrap secrets in `{ "values": { ... } }`.
        .and(body_partial_json(json!({
            "values": { "DATABASE_URL": "postgres://..." }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 7,
            "secrets": [{ "name": "DATABASE_URL" }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut values: BTreeMap<String, String> = BTreeMap::new();
    values.insert("DATABASE_URL".into(), "postgres://...".into());
    let resp = call_client(server.uri(), move |c| c.set_secrets("myapp", &values))
        .await
        .unwrap();
    assert_eq!(resp.version, 7);
}

// ─── Volumes ────────────────────────────────────────────────────────────

#[tokio::test]
async fn ensure_volume_creates_when_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/volumes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/apps/myapp/volumes"))
        .and(body_partial_json(json!({
            "name": "worker_data",
            "region": "fra",
            "size_gb": 5
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "vol_new",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 5
        })))
        .expect(1)
        .mount(&server)
        .await;

    let v = call_client(server.uri(), |c| {
        c.ensure_volume("myapp", "worker_data", "fra", 5)
    })
    .await
    .unwrap();
    assert_eq!(v.id, "vol_new");
    assert_eq!(v.size_gb, Some(5));
}

#[tokio::test]
async fn ensure_volume_reuses_existing_when_size_matches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/volumes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "vol_existing",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 5
        }])))
        .mount(&server)
        .await;
    // No POST mount — reuse must not create a new volume.

    let v = call_client(server.uri(), |c| {
        c.ensure_volume("myapp", "worker_data", "fra", 5)
    })
    .await
    .unwrap();
    assert_eq!(v.id, "vol_existing");
}

#[tokio::test]
async fn ensure_volume_extends_when_existing_is_smaller() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/volumes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "vol_small",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 1
        }])))
        .mount(&server)
        .await;
    // PUT /extend must be hit with the new size.
    Mock::given(method("PUT"))
        .and(path("/apps/myapp/volumes/vol_small/extend"))
        .and(body_partial_json(json!({ "size_gb": 10 })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "vol_small",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 10
        })))
        .expect(1)
        .mount(&server)
        .await;

    let v = call_client(server.uri(), |c| {
        c.ensure_volume("myapp", "worker_data", "fra", 10)
    })
    .await
    .unwrap();
    assert_eq!(v.size_gb, Some(10));
}

#[tokio::test]
async fn ensure_volume_refuses_to_shrink() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/volumes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "vol_big",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 50
        }])))
        .mount(&server)
        .await;
    // No PUT /extend, no POST /volumes mount — neither should be hit.

    let err = call_client(server.uri(), |c| {
        c.ensure_volume("myapp", "worker_data", "fra", 5)
    })
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("can't shrink"));
}

#[tokio::test]
async fn create_volume_response_extend_wraps_volume_key() {
    // The /extend endpoint sometimes wraps the response under `volume`;
    // the client must handle both flat and wrapped forms.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/apps/myapp/volumes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "id": "vol_x",
            "name": "worker_data",
            "region": "fra",
            "size_gb": 1
        }])))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/apps/myapp/volumes/vol_x/extend"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "volume": {
                "id": "vol_x",
                "name": "worker_data",
                "region": "fra",
                "size_gb": 3
            }
        })))
        .mount(&server)
        .await;

    let v = call_client(server.uri(), |c| {
        c.ensure_volume("myapp", "worker_data", "fra", 3)
    })
    .await
    .unwrap();
    assert_eq!(v.size_gb, Some(3));
}

#[tokio::test]
async fn create_volume_request_serializes_name_region_size() {
    // Direct create call: confirms the bare CreateVolumeRequest body
    // shape (no extra fields slipping in via Default).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/apps/myapp/volumes"))
        .and(body_partial_json(json!({
            "name": "data",
            "region": "iad",
            "size_gb": 2
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "vol_direct",
            "name": "data",
            "region": "iad",
            "size_gb": 2
        })))
        .expect(1)
        .mount(&server)
        .await;

    let v = call_client(server.uri(), |c| {
        c.create_volume(
            "myapp",
            &CreateVolumeRequest {
                name: "data".into(),
                region: "iad".into(),
                size_gb: 2,
            },
        )
    })
    .await
    .unwrap();
    assert_eq!(v.id, "vol_direct");
}

// Spec-drift detection moved to `tests/spec_drift.rs` — it downloads
// Fly's live OpenAPI and checksum-compares it, rather than vendoring a
// snapshot fixture in the repo.
