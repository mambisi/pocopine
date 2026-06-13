#![cfg(not(target_arch = "wasm32"))]
//! `pocopine-native-tauri` — the [Tauri](https://tauri.app) backend for
//! the pocopine native target (RFC-104).
//!
//! It re-exports the backend-neutral API from [`pocopine_native`] and
//! adds the Tauri webview wiring: a custom URI scheme whose handler
//! drives the app's axum router in-process (via
//! [`pocopine_native::bridge`]), plus the window. That wiring links the
//! platform webview backend, so it is gated behind the `tauri` feature;
//! the app's `src-tauri` crate enables it.
//!
//! # Usage
//!
//! The app's `src-tauri/src/main.rs`:
//!
//! ```ignore
//! // Links the app rlib so its `#[server]` inventory entries are present.
//! use my_app as _;
//!
//! fn main() {
//!     pocopine_native_tauri::run!(
//!         pocopine_native_tauri::NativeApp::new()
//!             .title("My App")
//!             .inner_size(1100.0, 720.0)
//!     );
//! }
//! ```

pub use pocopine_native::{dev_dir, NativeApp, DEV_DIR_ENV};

#[cfg(feature = "tauri")]
mod shell;

/// Launch the native window for `app`.
///
/// A macro, not a function, because Tauri's `generate_context!()` must
/// expand in the calling crate to read its `tauri.conf.json`. It expands
/// to a call to [`__run_with_context`]; on failure it prints the error
/// and exits non-zero, so `fn main()` can stay `-> ()`.
///
/// Requires the `tauri` feature (the app's `src-tauri` crate enables it).
#[macro_export]
macro_rules! run {
    ($app:expr) => {{
        if let Err(error) = $crate::__run_with_context(::tauri::generate_context!(), $app) {
            ::std::eprintln!("pocopine-native-tauri: failed to start native app: {error}");
            ::std::process::exit(1);
        }
    }};
}

/// Implementation behind the [`run!`] macro. Takes the Tauri context the
/// macro produced in the app crate plus the [`NativeApp`] description.
///
/// Application code calls [`run!`], not this function directly.
#[cfg(feature = "tauri")]
#[doc(hidden)]
pub fn __run_with_context(
    context: tauri::Context<tauri::Wry>,
    app: NativeApp,
) -> tauri::Result<()> {
    shell::run(app, context)
}
