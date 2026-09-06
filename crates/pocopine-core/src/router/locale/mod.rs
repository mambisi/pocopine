//! Locale integration is optional and browser-only; ordinary routers retain
//! their existing synchronous path on every other configuration.
#[cfg(all(target_arch = "wasm32", feature = "locale"))]
mod client;
#[cfg(all(target_arch = "wasm32", feature = "locale"))]
pub(crate) use client::*;
#[cfg(not(all(target_arch = "wasm32", feature = "locale")))]
mod server;
#[cfg(not(all(target_arch = "wasm32", feature = "locale")))]
pub(crate) use server::*;
