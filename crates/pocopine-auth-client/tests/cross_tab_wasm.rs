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
use pocopine_auth_client::{cross_tab, AuthSession};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::BroadcastChannel;

wasm_bindgen_test_configure!(run_in_browser);

/// Yield through the task queue (NOT the microtask queue):
/// `BroadcastChannel` `MessageEvent` delivery is dispatched as a
/// task, so awaiting `Promise.resolve()` (which fires on the
/// microtask queue) is too eager — the listener hasn't run yet by
/// the time we resume. Schedule resolution via `setTimeout(0)`
/// instead so the test continuation lands AFTER pending message
/// tasks.
async fn next_task() {
    let promise = Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
        } else {
            // No window — just resolve immediately (test will likely
            // fail downstream, which is the right signal).
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
    // Fresh state.
    cross_tab::__teardown_for_test();
    pocopine_auth_client::storage::__reset_storage_for_test();

    let session = AuthSession::new();
    let initial_epoch = session.epoch();
    cross_tab::__install_for_test(session.clone());

    // Yield once after install so Firefox finishes wiring the
    // listener before we post.
    next_task().await;

    // Origin channel — distinct from the framework's, but on the
    // same name, so its posts are delivered to the framework's
    // listener.
    let origin =
        BroadcastChannel::new("pocopine-auth").expect("BroadcastChannel must be available");
    origin
        .post_message(&JsValue::from_str("session_changed"))
        .expect("post_message must succeed");

    // Give the browser plenty of task cycles to deliver and run the
    // listener — different browsers schedule MessageEvents
    // differently.
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
async fn local_broadcast_channel_does_not_echo_to_origin() {
    // Sanity check on the BroadcastChannel spec: a channel does not
    // receive its own posts. The framework relies on this — without
    // it, `broadcast_session_changed` would feed back into the
    // origin tab's listener and SUPPRESS_BROADCAST would need to
    // be load-bearing instead of dormant.
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
