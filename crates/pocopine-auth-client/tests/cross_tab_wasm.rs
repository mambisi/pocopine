//! Wasm integration test for cross-tab `BroadcastChannel` routing.
//!
//! `BroadcastChannel` doesn't deliver messages to the origin tab, so
//! we simulate the peer-tab scenario from a single wasm runtime by:
//! 1. Installing the framework's cross-tab listener (it owns one
//!    `BroadcastChannel` named `"pocopine-auth"`) — this plays the
//!    role of "peer tab" in our test.
//! 2. Opening a SECOND `BroadcastChannel` ourselves with the same
//!    name — this plays the role of "origin tab".
//! 3. Posting from our (origin) channel.
//! 4. Awaiting microtasks so the listener has a chance to fire.
//! 5. Asserting the framework listener actually ran by observing
//!    `AuthSession::epoch` advanced.

#![cfg(target_arch = "wasm32")]

use js_sys::Promise;
use pocopine_auth::{AuthUser, Principal};
use pocopine_auth_client::{cross_tab, AuthSession};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::BroadcastChannel;

wasm_bindgen_test_configure!(run_in_browser);

/// Yield through the task queue. `MessageEvent` delivery is a task,
/// not a microtask, so `Promise.resolve()` resumes too eagerly.
async fn next_task() {
    let promise = Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
        } else {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    });
    let _ = JsFuture::from(promise).await;
}

async fn settle() {
    next_task().await;
    next_task().await;
}

#[wasm_bindgen_test(async)]
async fn outbound_post_triggers_inbound_listener_in_peer() {
    cross_tab::__teardown_for_test();
    pocopine_auth_client::storage::__reset_storage_for_test();

    let session = AuthSession::new();
    let initial_epoch = session.epoch();
    cross_tab::__install_for_test(session.clone());
    next_task().await;

    let origin =
        BroadcastChannel::new("pocopine-auth").expect("BroadcastChannel must be available");
    origin
        .post_message(&JsValue::from_str("session_changed"))
        .expect("post_message must succeed");

    // Bounded retry — different browsers schedule MessageEvents on
    // slightly different ticks; bail early on success so CI is fast.
    for _ in 0..5 {
        next_task().await;
        if session.epoch() > initial_epoch {
            break;
        }
    }

    assert!(
        session.epoch() > initial_epoch,
        "framework listener should have called bump_epoch \
         (initial={initial_epoch}, now={})",
        session.epoch()
    );

    origin.close();
    cross_tab::__teardown_for_test();
}

#[wasm_bindgen_test(async)]
async fn peer_sign_out_fences_in_flight_request_via_epoch() {
    // Simulates "request dispatched under user X; peer tab signs
    // out while it's in flight." The bearer middleware captures
    // `session.epoch()` at dispatch and compares against `epoch()`
    // on response — a mismatch is the fence that drops a stale
    // response from a now-invalidated identity (RFC-078 §5.10.5).
    //
    // This test asserts both halves of the fence:
    //   1. the epoch advances when a peer sign-out arrives, and
    //   2. the local principal is cleared so the UI stops rendering
    //      authenticated shells.
    cross_tab::__teardown_for_test();
    pocopine_auth_client::storage::__reset_storage_for_test();

    // Start authenticated locally — the "in-flight request" is sent
    // under this identity.
    let session = AuthSession::new();
    session.set_principal(Principal::from_user(AuthUser::new("u1")));
    assert!(session.is_authenticated());
    let epoch_before = session.epoch();

    cross_tab::__install_for_test(session.clone());
    next_task().await;

    // Peer tab signs out. Token storage is empty (no backend
    // installed), so the listener's `hydrate_from_storage()` sees
    // no token and routes to `apply_cross_tab_token_state(false)`
    // — the sign-out branch.
    let origin =
        BroadcastChannel::new("pocopine-auth").expect("BroadcastChannel must be available");
    origin
        .post_message(&JsValue::from_str("session_changed"))
        .expect("post_message must succeed");

    // Bounded retry — MessageEvent delivery is a task; settle until
    // the listener has run (or we've waited enough turns to fail
    // meaningfully).
    for _ in 0..5 {
        next_task().await;
        if !session.is_authenticated() {
            break;
        }
    }

    assert!(
        !session.is_authenticated(),
        "peer sign-out must clear the local principal so authenticated UI tears down"
    );
    assert!(
        session.epoch() > epoch_before,
        "peer sign-out must bump the epoch (fence for in-flight requests): \
         before={epoch_before}, now={}",
        session.epoch()
    );

    origin.close();
    cross_tab::__teardown_for_test();
}

#[wasm_bindgen_test(async)]
async fn local_broadcast_channel_does_not_echo_to_origin() {
    // Pins the spec invariant that a channel doesn't receive its own
    // posts; otherwise `SUPPRESS_BROADCAST` would have to be
    // load-bearing instead of dormant.
    let channel = BroadcastChannel::new("pocopine-auth-test-echo")
        .expect("BroadcastChannel must be available");

    let received = std::rc::Rc::new(std::cell::RefCell::new(false));
    let received_inner = received.clone();
    let listener =
        wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_evt| {
            *received_inner.borrow_mut() = true;
        });
    use wasm_bindgen::JsCast;
    channel.set_onmessage(Some(listener.as_ref().unchecked_ref()));

    channel
        .post_message(&JsValue::from_str("self"))
        .expect("post_message must succeed");
    settle().await;

    assert!(
        !*received.borrow(),
        "BroadcastChannel must not deliver to its own origin"
    );

    drop(listener);
    channel.close();
}
