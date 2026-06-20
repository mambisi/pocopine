#![cfg(not(target_arch = "wasm32"))]
//! RFC-107 — end-to-end host test for a streaming `#[server]` function.
//!
//! Drives the macro-generated route through axum and decodes the SSE body with
//! the same `sse` codec the wasm `fetch::call_stream` client uses.

use futures::StreamExt;
use pocopine::sse::{Decoded, SseDecoder, decode_payload};
use pocopine::{ServerError, ServerResult, StreamServerResult};
use pocopine_server::Server;
use pocopine_server::axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use pocopine_server::tower::ServiceExt;

#[pocopine::server(public)]
async fn count(n: u32) -> StreamServerResult<u32> {
    Ok(futures::stream::iter((0..n).map(Ok::<u32, ServerError>)).boxed())
}

#[pocopine::server(public)]
async fn count_then_fail(n: u32) -> StreamServerResult<u32> {
    let items = (0..n)
        .map(Ok::<u32, ServerError>)
        .chain(std::iter::once(Err(ServerError::App("boom".into()))));
    Ok(futures::stream::iter(items).boxed())
}

async fn collect(router: &Router, path: &str, n: u32) -> (Vec<ServerResult<u32>>, bool) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::json!([n]).to_string()))
                .unwrap(),
        )
        .await
        .expect("streaming route should respond");
    assert_eq!(response.status(), 200);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let mut decoder = SseDecoder::new();
    let mut payloads = decoder.push(&bytes);
    if let Some(p) = decoder.flush() {
        payloads.push(p);
    }

    let mut items = Vec::new();
    let mut done = false;
    for payload in payloads {
        match decode_payload::<u32>(&payload) {
            Decoded::Item(item) => items.push(item),
            Decoded::Done => done = true,
        }
    }
    (items, done)
}

#[tokio::test]
async fn streams_items_then_done() {
    let router = Server::new(Router::new())
        .try_finalize()
        .expect("server functions should finalize");
    let (items, done) = collect(&router, __count_path(), 3).await;
    assert!(done, "stream should terminate with [DONE]");
    let values: Vec<u32> = items.into_iter().map(|i| i.unwrap()).collect();
    assert_eq!(values, vec![0, 1, 2]);
}

#[tokio::test]
async fn mid_stream_error_is_in_band_and_terminal() {
    let router = Server::new(Router::new())
        .try_finalize()
        .expect("server functions should finalize");
    let (items, done) = collect(&router, __count_then_fail_path(), 2).await;
    assert!(done);
    assert_eq!(items.len(), 3);
    assert!(matches!(items[0], Ok(0)));
    assert!(matches!(items[1], Ok(1)));
    assert!(matches!(&items[2], Err(ServerError::App(m)) if m == "boom"));
}
