#![cfg(not(target_arch = "wasm32"))]

//! HTTP integration tests for `RailwayClient`. Each test spins up a
//! `wiremock` server, points the blocking client at its URL as the
//! GraphQL endpoint, and asserts on the exact mutation/query name +
//! variable payload we send.
//!
//! Railway's API is one POST per GraphQL operation, so operations are
//! distinguished by `body_string_contains` on the operation name plus
//! `body_partial_json` on the `variables` object. These tests pin the
//! request shape; `tests/spec_drift.rs` checks the live schema still
//! accepts it.
//!
//! Implementation note: `wiremock` is async; our client is blocking.
//! We start the mock server in a `#[tokio::test]` and call the blocking
//! client through `spawn_blocking` so the runtime stays responsive.

use std::collections::BTreeMap;

use pocopine_deploy_railway::client::{RailwayClient, ServiceInstanceConfig};
use serde_json::json;
use wiremock::matchers::{bearer_token, body_partial_json, body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_TOKEN: &str = "railway-test-token";

/// Run a blocking closure off the mock server's runtime so reqwest's
/// blocking calls don't dead-lock the tokio reactor.
async fn call_client<F, T>(endpoint: String, f: F) -> T
where
    F: FnOnce(RailwayClient) -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let client = RailwayClient::with_endpoint(&endpoint, TEST_TOKEN);
        f(client)
    })
    .await
    .expect("blocking task should not panic")
}

/// A `projects` query response carrying one project with one
/// environment and one service.
fn projects_response(project_name: &str) -> serde_json::Value {
    json!({
        "data": { "projects": { "edges": [
            { "node": {
                "id": "proj_42",
                "name": project_name,
                "environments": { "edges": [
                    { "node": { "id": "env_prod", "name": "production" } },
                ] },
                "services": { "edges": [] },
            } },
        ] } }
    })
}

// ─── Projects ───────────────────────────────────────────────────────────

#[tokio::test]
async fn find_project_returns_the_matching_project() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("projects {"))
        .respond_with(ResponseTemplate::new(200).set_body_json(projects_response("myapp")))
        .expect(1)
        .mount(&server)
        .await;

    let project = call_client(server.uri(), |c| c.find_project("myapp"))
        .await
        .unwrap()
        .expect("project found");
    assert_eq!(project.id, "proj_42");
    assert_eq!(
        project.default_environment().map(|e| e.id.as_str()),
        Some("env_prod"),
    );
}

#[tokio::test]
async fn ensure_project_short_circuits_when_project_exists() {
    let server = MockServer::start().await;
    // Only the `projects` query is mounted — a `projectCreate` request
    // would 404 from wiremock and fail the test.
    Mock::given(method("POST"))
        .and(body_string_contains("projects {"))
        .respond_with(ResponseTemplate::new(200).set_body_json(projects_response("myapp")))
        .expect(1)
        .mount(&server)
        .await;

    let project = call_client(server.uri(), |c| c.ensure_project("myapp", None))
        .await
        .unwrap();
    assert_eq!(project.id, "proj_42");
}

#[tokio::test]
async fn ensure_project_creates_when_absent() {
    let server = MockServer::start().await;
    // First `projects` query → no match.
    Mock::given(method("POST"))
        .and(body_string_contains("projects {"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "projects": { "edges": [] } }
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    // `projectCreate` mutation → new project, with the workspace threaded.
    Mock::given(method("POST"))
        .and(body_string_contains("projectCreate"))
        .and(body_partial_json(json!({
            "variables": { "input": { "name": "myapp", "workspaceId": "ws_1" } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "projectCreate": { "id": "proj_new" } }
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Second `projects` query (ensure_project re-queries after create) →
    // the freshly-created project.
    Mock::given(method("POST"))
        .and(body_string_contains("projects {"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "projects": { "edges": [
                { "node": {
                    "id": "proj_new",
                    "name": "myapp",
                    "environments": { "edges": [
                        { "node": { "id": "env_prod", "name": "production" } },
                    ] },
                    "services": { "edges": [] },
                } },
            ] } }
        })))
        .mount(&server)
        .await;

    let project = call_client(server.uri(), |c| c.ensure_project("myapp", Some("ws_1")))
        .await
        .unwrap();
    assert_eq!(project.id, "proj_new");
}

// ─── Services ───────────────────────────────────────────────────────────

#[tokio::test]
async fn create_service_posts_source_image() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("serviceCreate"))
        .and(body_partial_json(json!({
            "variables": { "input": {
                "projectId": "proj_42",
                "environmentId": "env_prod",
                "name": "myapp-web",
                "source": { "image": "ghcr.io/owner/myapp:sha" },
            } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "serviceCreate": { "id": "svc_1", "name": "myapp-web" } }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let vars = std::collections::BTreeMap::<String, String>::new();
    let svc = call_client(server.uri(), move |c| {
        c.create_service(
            "proj_42",
            "env_prod",
            "myapp-web",
            "ghcr.io/owner/myapp:sha",
            None,
            &vars,
        )
    })
    .await
    .unwrap();
    assert_eq!(svc.id, "svc_1");
    assert_eq!(svc.name, "myapp-web");
}

#[tokio::test]
async fn update_service_instance_posts_image_replicas_and_healthcheck() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("serviceInstanceUpdate"))
        .and(body_partial_json(json!({
            "variables": {
                "serviceId": "svc_1",
                "environmentId": "env_prod",
                "input": {
                    "source": { "image": "ghcr.io/owner/myapp:sha" },
                    "numReplicas": 2,
                    "healthcheckPath": "/healthz",
                    "region": "us-west1",
                }
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "serviceInstanceUpdate": true }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = ServiceInstanceConfig {
        image: "ghcr.io/owner/myapp:sha".into(),
        num_replicas: 2,
        healthcheck_path: Some("/healthz".into()),
        region: Some("us-west1".into()),
        registry_credentials: None,
    };
    call_client(server.uri(), move |c| {
        c.update_service_instance("svc_1", "env_prod", &cfg)
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn update_service_instance_sends_registry_credentials_when_present() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("serviceInstanceUpdate"))
        .and(body_partial_json(json!({
            "variables": { "input": {
                "registryCredentials": { "username": "ci-bot", "password": "tok-xyz" }
            } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "serviceInstanceUpdate": true }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = ServiceInstanceConfig {
        image: "ghcr.io/owner/myapp:sha".into(),
        num_replicas: 1,
        healthcheck_path: None,
        region: None,
        registry_credentials: Some(pocopine_deploy::RegistryCredentials {
            username: "ci-bot".into(),
            token: "tok-xyz".into(),
        }),
    };
    call_client(server.uri(), move |c| {
        c.update_service_instance("svc_1", "env_prod", &cfg)
    })
    .await
    .unwrap();
}

// ─── Variables ──────────────────────────────────────────────────────────

#[tokio::test]
async fn upsert_variables_posts_variable_collection() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("variableCollectionUpsert"))
        .and(body_partial_json(json!({
            "variables": { "input": {
                "projectId": "proj_42",
                "environmentId": "env_prod",
                "serviceId": "svc_1",
                "variables": { "POCOPINE_PROCESS": "web", "LOG_LEVEL": "info" },
                "replace": false,
            } }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "variableCollectionUpsert": true }
        })))
        .expect(1)
        .mount(&server)
        .await;

    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("POCOPINE_PROCESS".into(), "web".into());
    vars.insert("LOG_LEVEL".into(), "info".into());
    call_client(server.uri(), move |c| {
        c.upsert_variables("proj_42", "env_prod", "svc_1", &vars)
    })
    .await
    .unwrap();
}

// ─── Deployments ────────────────────────────────────────────────────────

#[tokio::test]
async fn redeploy_deployment_posts_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("deploymentRedeploy"))
        .and(body_partial_json(json!({
            "variables": { "id": "deploy_1" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "deploymentRedeploy": { "id": "deploy_2" } }
        })))
        .expect(1)
        .mount(&server)
        .await;

    call_client(server.uri(), |c| c.redeploy_deployment("deploy_1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn deploy_latest_source_posts_service_instance_deploy() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(bearer_token(TEST_TOKEN))
        .and(body_string_contains("serviceInstanceDeploy"))
        .and(body_partial_json(json!({
            "variables": { "serviceId": "svc_1", "environmentId": "env_prod" }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "serviceInstanceDeploy": true }
        })))
        .expect(1)
        .mount(&server)
        .await;

    call_client(server.uri(), |c| {
        c.deploy_latest_source("svc_1", "env_prod")
    })
    .await
    .unwrap();
}

// ─── Domains ────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_service_domain_returns_the_generated_domain() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("serviceDomainCreate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "serviceDomainCreate": { "domain": "myapp-web-production.up.railway.app" } }
        })))
        .mount(&server)
        .await;

    let domain = call_client(server.uri(), |c| {
        c.create_service_domain("svc_1", "env_prod")
    })
    .await;
    assert_eq!(
        domain.as_deref(),
        Some("myapp-web-production.up.railway.app")
    );
}

#[tokio::test]
async fn create_service_domain_returns_none_on_error() {
    // Domain creation is best-effort — an error (e.g. a domain already
    // exists) must yield None, never fail the deploy.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("serviceDomainCreate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": "service already has a domain" }]
        })))
        .mount(&server)
        .await;

    let domain = call_client(server.uri(), |c| {
        c.create_service_domain("svc_1", "env_prod")
    })
    .await;
    assert!(domain.is_none());
}

// ─── Error handling ─────────────────────────────────────────────────────

#[tokio::test]
async fn graphql_errors_surface_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("projects {"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": "Not Authorized" }]
        })))
        .mount(&server)
        .await;

    let err = call_client(server.uri(), |c| c.find_project("myapp"))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("Not Authorized"), "got: {err}");
}

#[tokio::test]
async fn http_failure_surfaces_as_error() {
    // A non-2xx response must surface as an `Err`, never a panic or a
    // silently-empty success.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
        .mount(&server)
        .await;

    let result = call_client(server.uri(), |c| c.find_project("myapp")).await;
    assert!(result.is_err(), "a 500 response must surface as an error");
}
