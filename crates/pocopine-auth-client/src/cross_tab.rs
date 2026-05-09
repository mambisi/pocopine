//! Cross-tab session coordination via `BroadcastChannel`.
//!
//! Without this, signing in or out in tab A leaves tab B oblivious:
//! tab B keeps issuing requests under the previous identity until the
//! server returns 401. This module wires a `BroadcastChannel` named
//! `"pocopine-auth"` so identity-change events propagate across tabs of
//! the same origin.
//!
//! ## How it works
//!
//! 1. `auth_plugin().with_cross_tab_sync(true)` opens a `BroadcastChannel`
//!    at boot and registers a listener.
//! 2. Whenever [`AuthSession::set_principal`] runs, it calls
//!    [`broadcast_session_changed`]. The local sender is suppressed
//!    via a re-entrancy flag so we don't loop.
//! 3. A peer tab's listener fires:
//!    - Hydrates the local in-memory token from configured
//!      [`crate::TokenStorage`] (the persistent tokens are how peer
//!      tabs see *what* the new credential is).
//!    - Bumps the local [`AuthSession`]'s epoch via
//!      [`AuthSession::bump_epoch`] so the bearer middleware's fence
//!      drops any in-flight responses captured under the old identity.
//!    - Apps observing the session can react (call `/me`, refresh
//!      reactive views, navigate, etc.).
//!
//! ## What's *not* synced
//!
//! The full `Principal` (user id, roles, claims) is **not** sent
//! across the channel. The peer tab only knows "something changed" —
//! it must call its own `/me` server fn to learn the new identity.
//! Reasons:
//! - Cross-tab messages are visible to `BroadcastChannel` listeners
//!   on the same origin; broadcasting principal payloads needlessly
//!   widens the data surface.
//! - `Principal` doesn't have a stable wire format defined here.
//! - The token + a bumped epoch are the minimum needed for the bearer
//!   middleware to switch identities cleanly; anything richer is an
//!   app-level concern.
//!
//! ## Falling back when unavailable
//!
//! `BroadcastChannel` ships in all evergreen browsers but can be
//! unavailable in test runners, `file://` contexts, or older Safari.
//! The install path silently degrades to a no-op when the constructor
//! fails — apps don't see a panic; they just don't get cross-tab sync.

#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use web_sys::{BroadcastChannel, MessageEvent};

#[cfg(target_arch = "wasm32")]
use crate::session::AuthSession;

/// Channel name used for the cross-tab handshake. Stable to keep the
/// surface predictable for app integrations that want to observe it
/// directly (e.g. devtools).
pub const CHANNEL_NAME: &str = "pocopine-auth";

#[cfg(target_arch = "wasm32")]
thread_local! {
    static CHANNEL: RefCell<Option<BroadcastChannel>> = const { RefCell::new(None) };
    /// Re-entrancy guard for inbound message handling.
    ///
    /// Currently dormant — the framework's listener calls
    /// [`crate::hydrate_from_storage`] (which writes only to the
    /// token slot and storage, never the session) and
    /// [`crate::session::AuthSession::bump_epoch`] (which is
    /// deliberately the broadcast-free epoch advance). Neither
    /// triggers `broadcast_session_changed`, so today the flag never
    /// fires.
    ///
    /// Kept as defensive scaffolding so app code that wraps inbound
    /// handling (e.g., calling `session.set_principal` from a custom
    /// listener) doesn't ping-pong messages between tabs. Per the
    /// `BroadcastChannel` spec, posts are not delivered to the
    /// origin tab, so the flag protects against
    /// `set_principal`-from-listener loops, not against self-echo.
    static SUPPRESS_BROADCAST: Cell<bool> = const { Cell::new(false) };
}

/// Wire up the cross-tab channel for `session`. Idempotent — repeat
/// installs are no-ops.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(session: AuthSession) {
    if CHANNEL.with(|c| c.borrow().is_some()) {
        return;
    }

    let Ok(channel) = BroadcastChannel::new(CHANNEL_NAME) else {
        // Browser doesn't support BroadcastChannel — silent fallback.
        return;
    };

    let session_for_listener = session;
    let listener = Closure::<dyn FnMut(MessageEvent)>::new(move |_evt: MessageEvent| {
        SUPPRESS_BROADCAST.with(|c| c.set(true));
        // 1. Re-read token from storage so this tab's bearer
        //    middleware switches credentials on its next outgoing
        //    request.
        crate::hydrate_from_storage();
        // 2. Bump epoch so any in-flight responses captured under the
        //    previous identity get fenced by `BearerMiddleware`.
        session_for_listener.bump_epoch();
        SUPPRESS_BROADCAST.with(|c| c.set(false));
    });
    channel.set_onmessage(Some(listener.as_ref().unchecked_ref()));
    // Leak the closure; it lives for the app's lifetime. Storing it
    // in the thread_local would tie its lifetime to module unload,
    // which never happens for wasm apps.
    listener.forget();

    CHANNEL.with(|c| *c.borrow_mut() = Some(channel));
}

/// Notify peer tabs that this tab's session changed. No-op when the
/// channel isn't installed or when we're already inside an inbound
/// message handler (re-entrancy guard prevents broadcast loops).
#[cfg(target_arch = "wasm32")]
pub fn broadcast_session_changed() {
    if SUPPRESS_BROADCAST.with(|c| c.get()) {
        return;
    }
    if let Some(channel) = CHANNEL.with(|c| c.borrow().clone()) {
        // Payload is intentionally minimal: receivers re-load token
        // from storage and bump epoch; they don't need a payload to
        // act on.
        let _ = channel.post_message(&JsValue::from_str("session_changed"));
    }
}

/// Tear down the channel. Used by tests; production doesn't need to
/// call this because the channel lives for the app's lifetime.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub fn __teardown_for_test() {
    if let Some(channel) = CHANNEL.with(|c| c.borrow_mut().take()) {
        channel.close();
    }
    SUPPRESS_BROADCAST.with(|c| c.set(false));
}

/// Install the cross-tab listener directly. Test seam — production
/// goes through `auth_plugin().with_cross_tab_sync(true)` which
/// validates the storage requirement first. Bypassing the builder
/// in production code is unsupported.
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub fn __install_for_test(session: AuthSession) {
    install(session);
}

// ─── Host stubs ─────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn install(_: crate::session::AuthSession) {}

#[cfg(not(target_arch = "wasm32"))]
pub fn broadcast_session_changed() {}

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub fn __teardown_for_test() {}
