//! Cross-tab sign-out broadcast bus.
//!
//! `SyncClient::sign_out` durably wipes the local sync cache by calling
//! [`SyncLocalStore::clear_all_streams`](crate::SyncLocalStore::clear_all_streams).
//! On wasm targets it also posts a message on a browser `BroadcastChannel`
//! named `pocopine.sync.sign_out` so other tabs sharing the same origin can
//! react to the sign-out without polling. Tabs subscribe via
//! [`SyncClient::watch_sign_outs`].
//!
//! On host targets the bus is an in-process notifier — useful for tests that
//! exercise the cascade contract without a browser, and harmless in single-
//! process binaries that have nothing to cascade to.
//!
//! The bus is the *transport* for sign-out events. The *data* wipe is the
//! `SyncLocalStore` contract; this module only delivers "another tab in your
//! origin signed out" notifications so the receiving tab can drop its
//! in-memory state and re-mount.

#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

/// Cancels a sign-out subscription when dropped.
///
/// Returned by [`SyncClient::watch_sign_outs`](crate::SyncClient::watch_sign_outs).
/// Dropping the guard unregisters the handler so it stops receiving cross-tab
/// sign-out events.
#[must_use = "the subscription cancels when dropped; bind it to a variable"]
pub struct SignOutSubscription {
    #[cfg(target_arch = "wasm32")]
    _inner: Option<WasmSubscriptionInner>,
}

impl SignOutSubscription {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn noop() -> Self {
        Self {}
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn wasm(inner: Option<WasmSubscriptionInner>) -> Self {
        Self { _inner: inner }
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) struct WasmSubscriptionInner {
    channel: web_sys::BroadcastChannel,
    _closure: wasm_bindgen::closure::Closure<dyn FnMut(web_sys::MessageEvent)>,
}

#[cfg(target_arch = "wasm32")]
impl Drop for WasmSubscriptionInner {
    fn drop(&mut self) {
        self.channel.close();
    }
}

/// BroadcastChannel name used by `SyncClient::sign_out` and
/// `SyncClient::watch_sign_outs`. Scoped to one browser origin by the platform.
#[cfg(target_arch = "wasm32")]
pub(crate) const SIGN_OUT_CHANNEL: &str = "pocopine.sync.sign_out";

/// Message payload posted on the sign-out channel. Kept as a literal string
/// rather than a JSON object so the broadcast wire format stays stable across
/// pocopine versions without committing to a serde schema yet.
#[cfg(target_arch = "wasm32")]
pub(crate) const SIGN_OUT_MESSAGE: &str = "signed_out";

/// Broadcast a sign-out event to other tabs in the same origin.
///
/// Best-effort: if the browser doesn't expose `BroadcastChannel` (older
/// privacy modes, locked-down embedded webviews), the broadcast silently
/// drops. The durable wipe is the source of truth; the broadcast is purely a
/// "tell other tabs" optimization.
#[cfg(target_arch = "wasm32")]
pub(crate) fn broadcast_sign_out() {
    use wasm_bindgen::JsValue;
    if let Ok(channel) = web_sys::BroadcastChannel::new(SIGN_OUT_CHANNEL) {
        let _ = channel.post_message(&JsValue::from_str(SIGN_OUT_MESSAGE));
        channel.close();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn broadcast_sign_out() {
    // Host has no cross-process cascade today. Tests exercise the path via
    // `SyncClient::sign_out_notify_handlers_for_test`.
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn subscribe_sign_out(handler: Rc<dyn Fn()>) -> Option<WasmSubscriptionInner> {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    let channel = web_sys::BroadcastChannel::new(SIGN_OUT_CHANNEL).ok()?;
    let closure: Closure<dyn FnMut(web_sys::MessageEvent)> =
        Closure::new(move |event: web_sys::MessageEvent| {
            if let Some(value) = event.data().as_string()
                && value == SIGN_OUT_MESSAGE {
                    handler();
                }
        });
    channel.set_onmessage(Some(closure.as_ref().unchecked_ref()));
    Some(WasmSubscriptionInner {
        channel,
        _closure: closure,
    })
}
