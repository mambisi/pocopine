#![cfg(not(target_arch = "wasm32"))]

use pocopine::ServerResult;
use pocopine_server::axum::Router;
use pocopine_server::Server;

#[pocopine::server(public, path = "/api/conflicting-server-function")]
async fn first_conflicting_route() -> ServerResult<()> {
    Ok(())
}

mod nested {
    use pocopine::ServerResult;

    #[pocopine::server(public, path = "/api/conflicting-server-function")]
    pub async fn second_conflicting_route() -> ServerResult<()> {
        Ok(())
    }
}

#[test]
fn duplicate_explicit_server_function_paths_fail_startup() {
    let err = Server::new(Router::new())
        .try_finalize()
        .expect_err("duplicate server-function paths should fail startup");
    let message = err.to_string();

    assert!(message.contains("server-function route conflict"));
    assert!(message.contains("/api/conflicting-server-function"));
    assert!(message.contains("first_conflicting_route"));
    assert!(message.contains("second_conflicting_route"));
}
