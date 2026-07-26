use pocopine_codec::base64_encode;
use pocopine_crypto::{Algorithm, Hasher, SecretString};
use pocopine_deploy_cloudflare_pages::client::PagesClient;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn envelope(result: serde_json::Value) -> serde_json::Value {
    json!({
        "success": true,
        "errors": [],
        "messages": [],
        "result": result,
    })
}

fn pages_hash(bytes: &[u8], extension: &str) -> String {
    let encoded = base64_encode(bytes);
    let mut hasher = Hasher::new(Algorithm::Blake3);
    hasher.update(encoded.as_bytes());
    hasher.update(extension.as_bytes());
    hasher.finalize_hex()[..32].to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creates_project_and_runs_direct_upload_protocol() {
    let server = MockServer::start().await;
    let project_path = "/accounts/account-1/pages/projects/site";

    Mock::given(method("GET"))
        .and(path(project_path))
        .and(header("authorization", "Bearer api-secret"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/accounts/account-1/pages/projects"))
        .and(header("authorization", "Bearer api-secret"))
        .and(body_json(json!({
            "name": "site",
            "production_branch": "main",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
            "name": "site",
            "subdomain": "site.pages.dev",
            "production_branch": "main",
        }))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{project_path}/upload-token")))
        .and(header("authorization", "Bearer api-secret"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(envelope(json!({ "jwt": "upload-jwt" }))),
        )
        .expect(1)
        .mount(&server)
        .await;

    let content = b"<h1>pocopine</h1>";
    let hash = pages_hash(content, "html");
    Mock::given(method("POST"))
        .and(path("/pages/assets/check-missing"))
        .and(header("authorization", "Bearer upload-jwt"))
        .and(body_json(json!({ "hashes": [hash.clone()] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([hash.clone()]))))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/pages/assets/upload"))
        .and(header("authorization", "Bearer upload-jwt"))
        .and(body_json(json!([{
            "key": hash.clone(),
            "value": base64_encode(content),
            "metadata": { "contentType": "text/html" },
            "base64": true,
        }])))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(Value::Null)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/pages/assets/upsert-hashes"))
        .and(header("authorization", "Bearer upload-jwt"))
        .and(body_json(json!({ "hashes": [hash.clone()] })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "success": true })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{project_path}/deployments")))
        .and(header("authorization", "Bearer api-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!({
            "id": "deployment-1",
            "url": "https://deployment-1.site.pages.dev",
            "environment": "preview",
            "aliases": null,
            "latest_stage": {
                "name": "deploy",
                "status": "success",
                "started_on": "2026-07-26T00:00:00Z",
                "ended_on": "2026-07-26T00:00:01Z"
            }
        }))))
        .expect(1)
        .mount(&server)
        .await;

    let dist = tempfile::tempdir().unwrap();
    std::fs::write(dist.path().join("index.html"), content).unwrap();
    let dist_path = dist.path().to_path_buf();
    let base_url = server.uri();
    let (project, deployment) = tokio::task::spawn_blocking(move || {
        let client = PagesClient::with_base_url(SecretString::new("api-secret"), base_url).unwrap();
        let project = client.ensure_project("account-1", "site", "main").unwrap();
        let deployment = client
            .deploy_directory("account-1", "site", "preview", "abc1234", &dist_path)
            .unwrap();
        (project, deployment)
    })
    .await
    .unwrap();

    assert_eq!(project.name, "site");
    assert_eq!(deployment.id, "deployment-1");
    assert_eq!(deployment.url, "https://deployment-1.site.pages.dev");

    let requests = server.received_requests().await.unwrap();
    let deployment_request = requests
        .iter()
        .find(|request| request.url.path() == format!("{project_path}/deployments"))
        .unwrap();
    let multipart = String::from_utf8_lossy(&deployment_request.body);
    assert!(multipart.contains("name=\"manifest\""));
    assert!(multipart.contains(&format!("\"/index.html\":\"{hash}\"")));
    assert!(multipart.contains("name=\"branch\""));
    assert!(multipart.contains("preview"));
    assert!(multipart.contains("name=\"commit_hash\""));
    assert!(multipart.contains("abc1234"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn latest_deployment_filters_the_requested_environment() {
    let server = MockServer::start().await;
    let project_path = "/accounts/account-1/pages/projects/site";
    let project = json!({
        "name": "site",
        "subdomain": "site.pages.dev",
        "production_branch": "main",
    });
    Mock::given(method("GET"))
        .and(path(project_path))
        .and(header("authorization", "Bearer api-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(project)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{project_path}/deployments")))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "1"))
        .and(query_param("env", "production"))
        .and(header("authorization", "Bearer api-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([{
            "id": "production-1",
            "url": "https://production-1.site.pages.dev",
            "environment": "production",
            "latest_stage": { "name": "deploy", "status": "success" }
        }]))))
        .expect(1)
        .mount(&server)
        .await;

    let base_url = server.uri();
    let deployment = tokio::task::spawn_blocking(move || {
        PagesClient::with_base_url(SecretString::new("api-secret"), base_url)
            .unwrap()
            .latest_deployment("account-1", "site", Some("production"))
            .unwrap()
    })
    .await
    .unwrap()
    .unwrap();

    assert_eq!(deployment.id, "production-1");
    assert_eq!(deployment.environment.as_deref(), Some("production"));
}
