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
//! The "no `[pp-app]` root" boot-error path renders a fixed
//! overlay without clearing `document.body`, so it can be tested
//! end-to-end without deleting `wasm-bindgen-test`'s log
//! container.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test pp_app_root`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine::{App, SubtreeHandle};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

thread_local! {
    static GHOST_SETUP_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static GHOST_OUTER_SETUP_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static REMOUNT_MOUNT_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static REMOUNT_READY_COUNT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ───────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(
    name = "pap-home",
    template = poco! {<div class="pap-home" data-mounted="yes">home mounted</div>}
)]
struct PapHome {}

#[handlers]
impl PapHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-subtree",
    template = poco! {<span class="pap-subtree" data-mounted="yes">subtree mounted</span>}
)]
struct PapSubtree {}

#[handlers]
impl PapSubtree {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-remount",
    template = poco! {<span class="pap-remount">remount</span>}
)]
struct PapRemount {}

#[handlers]
impl PapRemount {
    pub fn on_mount(&mut self) {
        REMOUNT_MOUNT_COUNT.with(|count| count.set(count.get() + 1));
    }

    pub fn on_ready(&self) {
        REMOUNT_READY_COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-watched",
    template = poco! {<span class="pap-watched" pp-text="value"></span>}
)]
struct PapWatched {
    value: u32,
}

#[handlers]
impl PapWatched {
    #[watch(value)]
    fn value_changed(&mut self, _value: u32, _previous: Option<u32>) {}
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-observe-child",
    template = poco! {<span class="pap-observe-child" pp-text="value"></span>}
)]
struct PapObserveChild {
    #[observe(PAP_OBSERVE_ROOT)]
    value: String,
}

#[handlers]
impl PapObserveChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-observe-root",
    template = poco! {<div><pap-observe-child></pap-observe-child></div>},
    uses = [PapObserveChild],
)]
struct PapObserveRoot {
    value: String,
}

pocopine::create_context!(PAP_OBSERVE_ROOT: Handle<PapObserveRoot>);

#[handlers]
impl PapObserveRoot {
    pub fn on_setup(&mut self) {
        self.value = "seed".to_string();
        PAP_OBSERVE_ROOT.provide(this::<Self>());
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-ghost-inner",
    template = poco! {<span class="pap-ghost-inner">inner</span>}
)]
struct PapGhostInner {}

#[handlers]
impl PapGhostInner {
    pub fn on_setup(&mut self) {
        GHOST_SETUP_COUNT.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "pap-ghost-outer",
    template = poco! {<section class="pap-ghost-outer">outer</section>}
)]
struct PapGhostOuter {}

#[handlers]
impl PapGhostOuter {
    pub fn on_setup(&mut self) {
        GHOST_OUTER_SETUP_COUNT.with(|count| count.set(count.get() + 1));
    }
}

struct ManualNoopComponent;

impl Component for ManualNoopComponent {
    const NAME: &'static str = "manual-noop-component";

    fn register() {}
}

// ─── helpers ────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

async fn tick() {
    for _ in 0..3 {
        let promise = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
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

/// `App::run()` without a `[pp-app]` root renders the fatal
/// boot overlay, but leaves existing body content in place.
#[wasm_bindgen_test]
fn app_run_renders_boot_error_when_no_pp_app_root() {
    let sentinel = doc().create_element("div").unwrap();
    sentinel
        .set_attribute("data-pp-app-sentinel", "yes")
        .unwrap();
    doc().body().unwrap().append_child(&sentinel).unwrap();

    App::new().run();

    assert!(
        doc()
            .query_selector("[data-pp-app-sentinel=\"yes\"]")
            .unwrap()
            .is_some(),
        "missing-root boot error should not clear existing body content",
    );
    let banner = doc()
        .query_selector("[data-pocopine-boot-error=\"missing-pp-app\"]")
        .unwrap()
        .expect("missing-root boot overlay rendered");
    assert!(
        banner
            .text_content()
            .unwrap_or_default()
            .contains("no [pp-app] root found"),
        "banner explains the missing pp-app root",
    );

    banner.remove();
    sentinel.remove();
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

/// Dropping a bound `SubtreeHandle` without calling `unmount()`
/// also tears down the mounted subtree.
#[wasm_bindgen_test]
fn mount_subtree_handle_drop_releases_scope() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();

    {
        let _handle = App::mount_subtree::<PapSubtree>(&host);
        assert!(
            host.query_selector(".pap-subtree").unwrap().is_some(),
            "subtree mounted before handle drop",
        );
    }

    assert!(
        host.query_selector(".pap-subtree").unwrap().is_none(),
        "dropping SubtreeHandle must clear the subtree DOM",
    );
    assert_eq!(host.inner_html(), "");

    host.remove();
}

#[wasm_bindgen_test(async)]
async fn subtree_host_can_be_mounted_again_after_unmount() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    REMOUNT_MOUNT_COUNT.with(|count| count.set(0));
    REMOUNT_READY_COUNT.with(|count| count.set(0));

    App::mount_subtree::<PapRemount>(&host).unmount();
    tick().await;
    assert_eq!(REMOUNT_MOUNT_COUNT.with(std::cell::Cell::get), 1);
    // The first scope is already gone before its deferred ready callback.
    assert_eq!(REMOUNT_READY_COUNT.with(std::cell::Cell::get), 0);

    let second = App::mount_subtree::<PapRemount>(&host);
    tick().await;

    assert!(
        host.query_selector(".pap-remount").unwrap().is_some(),
        "subtree teardown must clear host mount stamps so the host can be reused"
    );
    assert_eq!(
        REMOUNT_MOUNT_COUNT.with(std::cell::Cell::get),
        2,
        "the second mount must run on_mount instead of stopping at the stale walk guard"
    );
    assert_eq!(
        REMOUNT_READY_COUNT.with(std::cell::Cell::get),
        1,
        "the second mount must schedule and run on_ready"
    );
    second.unmount();
    host.remove();
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[wasm_bindgen_test(async)]
async fn generated_watch_effect_releases_with_its_component_scope() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let baseline = pocopine_core::reactive::stats();

    let handle = App::mount_subtree::<PapWatched>(&host);
    tick().await;
    assert!(
        pocopine_core::reactive::stats().0 > baseline.0,
        "fixture must install its generated watch effect"
    );
    handle.unmount();
    tick().await;

    assert_eq!(
        pocopine_core::reactive::stats(),
        baseline,
        "#[watch] effects must release when their component unmounts"
    );
    host.remove();
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[wasm_bindgen_test(async)]
async fn generated_observe_effect_releases_with_the_consumer_scope() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let baseline = pocopine_core::reactive::stats();

    let handle = App::mount_subtree::<PapObserveRoot>(&host);
    tick().await;
    assert!(
        pocopine_core::reactive::stats().0 > baseline.0,
        "fixture must install its generated observe effect"
    );
    handle.unmount();
    tick().await;

    assert_eq!(
        pocopine_core::reactive::stats(),
        baseline,
        "#[observe] effects must be owned by and release with the consumer"
    );
    host.remove();
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[wasm_bindgen_test(async)]
async fn deferred_observe_install_does_not_race_consumer_unmount() {
    let host = doc().create_element("div").unwrap();
    doc().body().unwrap().append_child(&host).unwrap();
    let baseline = pocopine_core::reactive::stats();

    App::mount_subtree::<PapObserveRoot>(&host).unmount();
    tick().await;

    assert_eq!(
        pocopine_core::reactive::stats(),
        baseline,
        "a deferred observe install must not create an effect after unmount"
    );
    host.remove();
}

#[cfg(any(debug_assertions, feature = "devtools"))]
#[wasm_bindgen_test(async)]
async fn deferred_scoped_watcher_does_not_install_after_target_unmount() {
    let owner = Scope::new(std::rc::Rc::new(std::cell::RefCell::new(
        PapSubtree::default(),
    )));
    let target = Scope::new(std::rc::Rc::new(std::cell::RefCell::new(
        PapSubtree::default(),
    )));
    let baseline = pocopine_core::reactive::stats();
    pocopine_core::scope::with_current_scope_id(owner.id, || {
        pocopine_core::watch_scope_field_scoped::<String, _>(target.id, "missing", |_, _| {});
    });

    Scope::remove(target.id);
    tick().await;

    assert_eq!(
        pocopine_core::reactive::stats(),
        baseline,
        "a deferred scoped watcher must skip install when its target is already gone"
    );
    Scope::remove(owner.id);
}

#[wasm_bindgen_test(async)]
async fn handle_observe_reacts_to_typed_targeted_field_writes() {
    let state = std::rc::Rc::new(std::cell::RefCell::new(PapWatched::default()));
    let scope = Scope::new(state.clone());
    let handle = Handle::new(state, scope.id);
    let seen = std::rc::Rc::new(std::cell::Cell::new(u32::MAX));
    let hits = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let seen_for_cb = seen.clone();
    let hits_for_cb = hits.clone();
    pocopine_core::scope::with_current_scope_id(scope.id, || {
        handle.observe(
            |state| state.value,
            move |value, _| {
                seen_for_cb.set(*value);
                hits_for_cb.set(hits_for_cb.get() + 1);
            },
        );
    });
    tick().await;
    assert_eq!((seen.get(), hits.get()), (0, 1));

    let value = FieldHandle::<u32>::__new(scope.id, "value");
    value.set(7);
    pocopine_core::flush_sync();
    tick().await;

    assert_eq!(
        (seen.get(), hits.get()),
        (7, 2),
        "Handle::observe must rerun after a FieldHandle targeted write"
    );
    Scope::remove(scope.id);
}

#[wasm_bindgen_test]
fn app_root_discovery_does_not_mount_detached_nested_candidates() {
    PapGhostOuter::register();
    PapGhostInner::register();
    GHOST_SETUP_COUNT.with(|count| count.set(0));
    GHOST_OUTER_SETUP_COUNT.with(|count| count.set(0));
    let host = doc().create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<pap-ghost-outer><pap-ghost-inner></pap-ghost-inner></pap-ghost-outer>");
    doc().body().unwrap().append_child(&host).unwrap();

    App::new().run();

    assert_eq!(
        GHOST_OUTER_SETUP_COUNT.with(std::cell::Cell::get),
        1,
        "the outer candidate must mount before its stale child is skipped"
    );
    assert!(
        host.query_selector(".pap-ghost-outer").unwrap().is_some(),
        "outer replacement DOM must be present"
    );
    assert_eq!(
        GHOST_SETUP_COUNT.with(std::cell::Cell::get),
        0,
        "the inner candidate was detached when its parent mounted and must not run setup"
    );
    host.remove();
}

/// Manual `Component` impls do not have macro-emitted template
/// plans, so the default RFC 062 mount entry is intentionally a
/// no-op rather than a hidden static-plan fallback.
#[wasm_bindgen_test]
fn manual_component_default_mount_template_is_noop() {
    let host = doc().create_element("div").unwrap();

    <ManualNoopComponent as Component>::mount_template(&host, ScopeId(0), &JsValue::NULL);

    assert_eq!(
        host.inner_html(),
        "",
        "default Component::mount_template must leave manual component DOM untouched",
    );
}
