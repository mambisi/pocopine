//! Server-function error types.
//!
//! `#[server]` functions return [`Result<T>`] where the error type is
//! always [`ServerError`]. That keeps the wire protocol uniform and
//! lets the client cleanly distinguish "the server returned an error"
//! from "the network failed or the response didn't deserialize."

use std::fmt;

#[cfg(not(target_arch = "wasm32"))]
use http::{Extensions, HeaderMap, Method, Uri};
use serde::{Deserialize, Serialize};

/// All failures a client stub can return.
///
/// * [`ServerError::App`] is serialized by the server as part of a
///   `Result::Err` and decoded verbatim on the client.
/// * [`ServerError::Network`] is synthesized locally when the fetch
///   never reached the server, or the body didn't parse as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerError {
    /// An application-level error produced on the server side.
    App(String),
    /// Authentication is required but missing or invalid.
    Unauthorized(String),
    /// Authentication succeeded, but this caller cannot perform the action.
    Forbidden(String),
    /// The request payload was malformed or exceeded the framework body limit.
    BadRequest(String),
    /// Transport / deserialization failure on the client side.
    Network(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::App(msg) => write!(f, "server error: {msg}"),
            ServerError::Unauthorized(msg) => write!(f, "unauthorized: {msg}"),
            ServerError::Forbidden(msg) => write!(f, "forbidden: {msg}"),
            ServerError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            ServerError::Network(msg) => write!(f, "network error: {msg}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl ServerError {
    /// Build an authentication failure.
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        ServerError::Unauthorized(msg.into())
    }

    /// Build an authorization failure.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        ServerError::Forbidden(msg.into())
    }

    /// Build a malformed request failure.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        ServerError::BadRequest(msg.into())
    }
}

impl From<String> for ServerError {
    fn from(s: String) -> Self {
        ServerError::App(s)
    }
}

impl From<&str> for ServerError {
    fn from(s: &str) -> Self {
        ServerError::App(s.to_owned())
    }
}

/// Canonical `Result` alias for `#[server]` functions.
pub type Result<T> = core::result::Result<T, ServerError>;

/// A boxed result stream — the items a streaming `#[server]` function produces.
///
/// `Send` on the host (the body runs on a multi-threaded runtime); not `Send`
/// on wasm, where the client stream is driven by `!Send` browser futures.
#[cfg(not(target_arch = "wasm32"))]
pub type ServerStream<T> =
    core::pin::Pin<Box<dyn futures_core::Stream<Item = Result<T>> + Send + 'static>>;
/// A boxed result stream — the items a streaming `#[server]` function produces.
#[cfg(target_arch = "wasm32")]
pub type ServerStream<T> =
    core::pin::Pin<Box<dyn futures_core::Stream<Item = Result<T>> + 'static>>;

/// The streaming counterpart of [`Result`] (`ServerResult`) — RFC-107.
///
/// The outer `Result` is the HTTP handshake: it fails only when the call never
/// produced a stream (a transport failure, or a guard/auth/bad-request
/// rejection that returns a non-2xx status before the body runs). Everything
/// the handler body produces — a setup error *and* each streamed value — is an
/// in-band [`Result<T>`] item; a mid-stream `Err` is terminal. A streaming
/// `#[server]` fn is recognised by this return type alone, with no attribute
/// flag.
pub type StreamServerResult<T> = Result<ServerStream<T>>;

/// Server-side request metadata available to generated server-function routes.
///
/// This is intentionally framework-level, not auth-level: any middleware can
/// attach typed values to the request extensions, and auth is just one
/// consumer/producer of those values.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct RequestContext {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    extensions: Extensions,
}

#[cfg(not(target_arch = "wasm32"))]
impl RequestContext {
    /// Build a request context without extensions.
    pub fn new(method: Method, uri: Uri, headers: HeaderMap) -> Self {
        Self {
            method,
            uri,
            headers,
            extensions: Extensions::new(),
        }
    }

    /// Build a request context from HTTP request parts.
    pub fn from_parts(
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        extensions: Extensions,
    ) -> Self {
        Self {
            method,
            uri,
            headers,
            extensions,
        }
    }

    /// Attach a typed request extension.
    pub fn with_extension<T>(mut self, value: T) -> Self
    where
        T: Clone + Send + Sync + 'static,
    {
        self.extensions.insert(value);
        self
    }

    /// HTTP method used for the server-function call.
    pub fn method(&self) -> &Method {
        &self.method
    }

    /// Request URI used for the server-function call.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// All request headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// All request extensions visible to server middleware and generated
    /// routes.
    pub fn extensions(&self) -> &Extensions {
        &self.extensions
    }

    /// Typed request extension by value type.
    pub fn extension<T>(&self) -> Option<&T>
    where
        T: Send + Sync + 'static,
    {
        self.extensions.get::<T>()
    }

    /// Header value as UTF-8 text.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    /// Bearer token from the `Authorization` header, if present.
    pub fn bearer_token(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        let (scheme, token) = value.split_once(' ')?;
        if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
            Some(token.trim())
        } else {
            None
        }
    }

    /// Cookie value by name from the `Cookie` header.
    ///
    /// This deliberately small parser is intended for simple session cookies.
    /// It does not implement full RFC 6265 quoted-value parsing.
    pub fn cookie(&self, name: &str) -> Option<&str> {
        let cookies = self.header("cookie")?;
        for part in cookies.split(';') {
            if let Some((key, value)) = part.trim().split_once('=')
                && key.trim() == name
            {
                return Some(value.trim());
            }
        }
        None
    }
}

/// Extract a server-only value from [`RequestContext`].
///
/// The `#[server]` macro recognizes extractor-shaped parameters and omits
/// them from the client payload, then calls this trait on the host before the
/// user body runs.
#[cfg(not(target_arch = "wasm32"))]
pub trait FromRequestContext: Sized {
    /// Resolve this value from request metadata/extensions.
    fn from_request_context(ctx: &RequestContext) -> Result<Self>;
}

#[cfg(not(target_arch = "wasm32"))]
impl FromRequestContext for RequestContext {
    fn from_request_context(ctx: &RequestContext) -> Result<Self> {
        Ok(ctx.clone())
    }
}

/// Axum-style typed request extension extractor for `#[server]` functions.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Extension<T>(pub T);

#[cfg(not(target_arch = "wasm32"))]
impl<T> Extension<T> {
    /// Consume the extractor and return the inner extension value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> core::ops::Deref for Extension<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> FromRequestContext for Extension<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn from_request_context(ctx: &RequestContext) -> Result<Self> {
        ctx.extension::<T>().cloned().map(Self).ok_or_else(|| {
            ServerError::bad_request(format!(
                "missing request extension `{}`",
                core::any::type_name::<T>()
            ))
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<T> FromRequestContext for Option<Extension<T>>
where
    T: Clone + Send + Sync + 'static,
{
    fn from_request_context(ctx: &RequestContext) -> Result<Self> {
        Ok(ctx.extension::<T>().cloned().map(Extension))
    }
}

const SERVER_FUNCTION_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const SERVER_FUNCTION_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = SERVER_FUNCTION_HASH_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(SERVER_FUNCTION_HASH_PRIME);
    }
    hash
}

/// Returns the compiler-generated default route path for a server function.
///
/// The suffix is based on the Rust module path and function name so two
/// modules can expose the same function name without colliding.
#[doc(hidden)]
pub fn server_function_default_path(module_path: &str, function: &str) -> String {
    let mut key = String::with_capacity(module_path.len() + function.len() + 2);
    key.push_str(module_path);
    key.push_str("::");
    key.push_str(function);
    let hash = fnv1a64(key.as_bytes());

    format!("/_pocopine/{function}_{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::server_function_default_path;

    #[test]
    fn default_paths_are_scoped_by_module_path() {
        let first = server_function_default_path("app::filters", "get_filter");
        let second = server_function_default_path("app::admin", "get_filter");

        assert_ne!(first, second);
        assert!(first.starts_with("/_pocopine/get_filter_"));
        assert!(second.starts_with("/_pocopine/get_filter_"));
    }

    #[test]
    fn default_paths_are_stable() {
        assert_eq!(
            server_function_default_path("app::filters", "get_filter"),
            "/_pocopine/get_filter_b0e2ae5de9927498"
        );
    }
}
