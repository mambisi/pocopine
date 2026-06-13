#![cfg(not(target_arch = "wasm32"))]
//! `pocopine-native` — backend-neutral core of the pocopine native
//! target (RFC-104).
//!
//! A pocopine app compiles to wasm and runs in a webview; its
//! `#[server]` functions run host-side behind an axum
//! [`Server`](pocopine_server::Server) router. This crate provides the
//! pieces a *native* shell needs to host that app in-process, with **no**
//! dependency on any particular windowing toolkit:
//!
//! * [`bridge`] — turn an incoming `http::Request` into an axum request,
//!   drive the app router via `oneshot`, and return the `http::Response`.
//!   No socket, no IPC. Unit-tested on any host.
//! * [`NativeApp`] — a small builder describing the window and how to
//!   assemble the in-process server.
//! * [`dev_dir`] / [`DEV_DIR_ENV`] — the dev-mode static-asset root the
//!   CLI exports.
//!
//! The Tauri backend that consumes this — registering a custom URI
//! scheme and opening a window — lives in the separate
//! `pocopine-native-tauri` crate. A different backend would implement
//! the same shape against this contract.

mod assets;
pub mod bridge;

pub use assets::{dev_dir, DEV_DIR_ENV};

use pocopine_server::Server;

/// Default native window size, used when [`NativeApp::inner_size`] is
/// not called.
const DEFAULT_SIZE: (f64, f64) = (1024.0, 768.0);

/// Describes the native window and how to assemble the app's
/// server-function router.
///
/// Build one with [`NativeApp::new`], then hand it to a backend's launch
/// entry point (e.g. `pocopine_native_tauri::run!`).
pub struct NativeApp {
    title: String,
    inner_size: (f64, f64),
    configure: Box<dyn FnOnce(Server) -> Server + Send>,
}

impl NativeApp {
    /// A native app with the default title (`"pocopine"`) and window
    /// size, and an identity server-configuration hook.
    pub fn new() -> Self {
        Self {
            title: "pocopine".to_string(),
            inner_size: DEFAULT_SIZE,
            configure: Box::new(|server| server),
        }
    }

    /// Set the native window title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the initial inner window size in logical pixels.
    pub fn inner_size(mut self, width: f64, height: f64) -> Self {
        self.inner_size = (width, height);
        self
    }

    /// Hook to configure the in-process [`Server`] before it is
    /// finalized — install auth, plugins, or extra routes:
    ///
    /// ```ignore
    /// NativeApp::new().configure(|server| server.with_auth(LocalKeyring))
    /// ```
    ///
    /// `#[server]` routes are installed automatically by
    /// [`Server::new`]; this is only for app-level additions. Runs once,
    /// when the window's backend router is built.
    pub fn configure<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(Server) -> Server + Send + 'static,
    {
        self.configure = Box::new(configure);
        self
    }

    /// Decompose into owned parts for a backend crate to consume.
    ///
    /// Backend implementations (e.g. `pocopine-native-tauri`) call this
    /// to read the window settings and the server-configuration hook;
    /// application code uses [`NativeApp::new`] and the builder methods.
    #[doc(hidden)]
    pub fn into_parts(self) -> NativeAppParts {
        NativeAppParts {
            title: self.title,
            inner_size: self.inner_size,
            configure: self.configure,
        }
    }
}

impl Default for NativeApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Owned decomposition of a [`NativeApp`], produced by
/// [`NativeApp::into_parts`] for backend crates.
#[doc(hidden)]
pub struct NativeAppParts {
    pub title: String,
    pub inner_size: (f64, f64),
    pub configure: Box<dyn FnOnce(Server) -> Server + Send>,
}
