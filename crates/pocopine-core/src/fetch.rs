//! Client-side fetch helper for `#[server]` functions.
//!
//! `pocopine::fetch::call` is a thin wrapper over `window.fetch`: it
//! JSON-serializes the argument tuple, POSTs it to `url`, and parses the
//! JSON response as a `Result<R>` where the error type is always
//! [`ServerError`]. Network and deserialization failures surface as
//! [`ServerError::Network`]; server-returned errors come through as
//! [`ServerError::App`] inside the `Err` variant the server serialized.

use serde::{de::DeserializeOwned, Serialize};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

use crate::server::{Result as ServerResult, ServerError};

/// Post `args` as JSON to `url` and deserialize the JSON response into
/// `Result<R>`. The server is expected to respond with a JSON encoding
/// of `serde_json::to_string(&result)` where `result: Result<R>`.
pub async fn call<A, R>(url: &str, args: &A) -> ServerResult<R>
where
    A: Serialize,
    R: DeserializeOwned,
{
    let body = serde_json::to_string(args)
        .map_err(|e| ServerError::Network(format!("serialize args: {e}")))?;

    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(&body));

    // Content-Type header — axum's `Json` extractor expects it.
    let headers =
        web_sys::Headers::new().map_err(|e| ServerError::Network(format!("headers: {e:?}")))?;
    let _ = headers.set("content-type", "application/json");
    init.set_headers(&headers);

    let request = Request::new_with_str_and_init(url, &init)
        .map_err(|e| ServerError::Network(format!("build request: {e:?}")))?;

    let win =
        web_sys::window().ok_or_else(|| ServerError::Network("no window available".to_string()))?;
    let resp_js = JsFuture::from(win.fetch_with_request(&request))
        .await
        .map_err(|e| ServerError::Network(format!("fetch failed: {e:?}")))?;

    let resp: Response = resp_js
        .dyn_into()
        .map_err(|_| ServerError::Network("fetch returned non-Response".into()))?;
    if !resp.ok() {
        return Err(ServerError::Network(format!(
            "HTTP {} {}",
            resp.status(),
            resp.status_text()
        )));
    }

    let text_js = JsFuture::from(
        resp.text()
            .map_err(|e| ServerError::Network(format!("read body: {e:?}")))?,
    )
    .await
    .map_err(|e| ServerError::Network(format!("read body: {e:?}")))?;

    let text = text_js
        .as_string()
        .ok_or_else(|| ServerError::Network("body was not a string".into()))?;

    let outer: ServerResult<R> = serde_json::from_str(&text)
        .map_err(|e| ServerError::Network(format!("parse response: {e}")))?;
    outer
}
