#![cfg(not(target_arch = "wasm32"))]

use pocopine::ServerResult;
use pocopine_server::Server;
use pocopine_server::axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use pocopine_server::tower::ServiceExt;

#[pocopine::server(public)]
async fn auto_echo(value: String) -> ServerResult<String> {
    Ok(format!("root:{value}"))
}

mod nested {
    use pocopine::ServerResult;

    #[pocopine::server(public)]
    pub async fn auto_echo(value: String) -> ServerResult<String> {
        Ok(format!("nested:{value}"))
    }
}

#[pocopine::server(public, path = "/api/custom-echo")]
async fn custom_echo(value: String) -> ServerResult<String> {
    Ok(format!("custom:{value}"))
}

#[test]
fn default_route_paths_are_module_scoped() {
    assert_ne!(__auto_echo_path(), nested::__auto_echo_path());
    assert_ne!(
        __auto_echo_function_path(),
        nested::__auto_echo_function_path()
    );
    assert!(matches!(
        __auto_echo_function_path().strip_suffix("::auto_echo"),
        Some(module) if module.ends_with("server_function_auto_routes")
    ));
    assert!(matches!(
        nested::__auto_echo_function_path().strip_suffix("::auto_echo"),
        Some(module) if module.ends_with("server_function_auto_routes::nested")
    ));
    assert!(matches!(
        __auto_echo_path().strip_prefix("/_pocopine/auto_echo_"),
        Some(suffix) if suffix.len() == 16
    ));
    assert!(matches!(
        nested::__auto_echo_path().strip_prefix("/_pocopine/auto_echo_"),
        Some(suffix) if suffix.len() == 16
    ));
}

#[test]
fn explicit_route_path_is_stable() {
    assert_eq!(__custom_echo_path(), "/api/custom-echo");
}

#[tokio::test]
async fn server_new_installs_linked_server_functions() {
    let router = Server::new(Router::new())
        .try_finalize()
        .expect("server functions should finalize");

    assert_eq!(post(&router, __auto_echo_path(), "one").await, "root:one");
    assert_eq!(
        post(&router, nested::__auto_echo_path(), "two").await,
        "nested:two"
    );
    assert_eq!(
        post(&router, __custom_echo_path(), "three").await,
        "custom:three"
    );
}

async fn post(router: &Router, path: &str, value: &str) -> String {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!([value]).to_string()))
                .unwrap(),
        )
        .await
        .expect("server-function route should respond");
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let result: ServerResult<String> = serde_json::from_slice(&bytes).unwrap();
    result.unwrap()
}
