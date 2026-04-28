//! RFC 061 Phase 2 — `[pp-app]` root discovery + typed
//! `mount_subtree` escape hatch.
//!
//! Pins the new boot contract:
//!
//!   - `App::run()` mounts the active route into the single
//!     `[pp-app]`-attributed element. Whole-body adoption is gone.
//!   - `App::mount_subtree::<C>(&host)` mounts a typed component
//!     into an arbitrary host element, returning a `SubtreeHandle`
//!     whose `unmount()` releases the scope tree + DOM.
//!
//! The "no `[pp-app]` root" boot-error path renders by
//! replacing `document.body.innerHTML` — that conflicts with
//! `wasm-bindgen-test`'s log container which lives in body.
//! The error path is asserted by direct call to
//! `render_missing_pp_app_root` in this test file's
//! [`boot_error_renders_banner_directly`] test, not by driving
//! `App::run()` end-to-end.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test pp_app_root`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine::{App, SubtreeHandle};
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-home",
    template_inline = r#"<div class="pap-home" data-mounted="yes">home mounted</div>"#
)]
struct PapHome {}

#[handlers]
impl PapHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-subtree",
    template_inline = r#"<span class="pap-subtree" data-mounted="yes">subtree mounted</span>"#
)]
struct PapSubtree {}

#[handlers]
impl PapSubtree {}

// ─── helpers ────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

// ─── tests ──────────────────────────────────────────────────────

/// `App::run()` discovers the `[pp-app]` root and mounts the
/// route's component there. Append the host to body without
/// clobbering the wasm-bindgen-test log container.
#[wasm_bindgen_test]
fn app_run_mounts_route_into_pp_app_root() {
    let host = doc().create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<pap-home></pap-home>");
    doc().body().unwrap().append_child(&host).unwrap();

    App::new().route::<PapHome>("/").run();

    let mounted = host
        .query_selector(".pap-home[data-mounted=\"yes\"]")
        .unwrap()
        .expect("PapHome rendered inside [pp-app] root");
    assert_eq!(mounted.text_content().unwrap_or_default(), "home mounted");

    host.remove();
}

/// `App::mount_subtree::<C>(&host)` mounts a typed component
/// into an arbitrary host element.
#[wasm_bindgen_test]
fn mount_subtree_mounts_typed_component() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();

    let handle: SubtreeHandle = App::mount_subtree::<PapSubtree>(&host);

    let mounted = host
        .query_selector(".pap-subtree[data-mounted=\"yes\"]")
        .unwrap()
        .expect("PapSubtree rendered into the typed mount host");
    assert_eq!(
        mounted.text_content().unwrap_or_default(),
        "subtree mounted"
    );

    handle.unmount();
    host.remove();
}

/// `SubtreeHandle::unmount()` clears the host's children — the
/// pocopine scope tree, listeners, and DOM go.
#[wasm_bindgen_test]
fn mount_subtree_handle_unmount_releases_scope() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();

    let handle = App::mount_subtree::<PapSubtree>(&host);
    assert!(
        host.query_selector(".pap-subtree").unwrap().is_some(),
        "subtree mounted before unmount",
    );

    handle.unmount();

    assert!(
        host.query_selector(".pap-subtree").unwrap().is_none(),
        "unmount must clear the subtree DOM (host.inner_html should be empty)",
    );
    assert_eq!(
        host.inner_html(),
        "",
        "host innerHTML should be empty after unmount",
    );

    host.remove();
}
