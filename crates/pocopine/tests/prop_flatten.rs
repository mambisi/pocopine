//! RFC-044 §5.10 — `#[prop(flatten)]` / `#[model(flatten)]` leaf
//! plumbing: the `flatten_container_of` leaf→container map and the
//! dual-key watch triggering (§5.10.5) that lets a single
//! `#[watch(<container>)]` fire when any one leaf changes.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test prop_flatten`

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};

use pocopine::__private::ComponentState;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount(tag: &str, host_html: &str) -> Element {
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    let el = host.query_selector(tag).unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, tag);
    pocopine_core::mount::finalize_compiled_subtree(&el);
    host
}

async fn tick() {
    for _ in 0..4 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

async fn sleep_zero() {
    let p = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        let _ = window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), 0);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

/// Drain microtasks, one macrotask, then microtasks again — enough
/// for a click handler → bind effect → leaf write → watch effect to
/// settle.
async fn settle() {
    tick().await;
    sleep_zero().await;
    tick().await;
}

fn read(host: &Element, sel: &str) -> String {
    host.query_selector(sel)
        .unwrap()
        .unwrap_or_else(|| panic!("missing selector {sel}"))
        .dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

// ───────────────────────── leaf → container map ─────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct StyleLeaves {
    label: String,
    color: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SizeLeaves {
    width: f64,
    height: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "flatten-map-target", template_inline = r#"<div></div>"#)]
struct FlattenMapTarget {
    #[prop(flatten = ["label", "color"])]
    style: StyleLeaves,
    #[prop(flatten = ["width", "height"])]
    size: SizeLeaves,
    #[prop]
    plain: String,
}

#[handlers]
impl FlattenMapTarget {}

#[wasm_bindgen_test]
fn flatten_container_of_maps_each_leaf_to_its_own_container() {
    let c = FlattenMapTarget::default();
    // every leaf resolves to the field it was flattened from
    assert_eq!(c.flatten_container_of("label"), Some("style"));
    assert_eq!(c.flatten_container_of("color"), Some("style"));
    // a second flattened field on the same component stays disjoint
    assert_eq!(c.flatten_container_of("width"), Some("size"));
    assert_eq!(c.flatten_container_of("height"), Some("size"));
    // the container field itself is not a leaf
    assert_eq!(c.flatten_container_of("style"), None);
    assert_eq!(c.flatten_container_of("size"), None);
    // a plain `#[prop]` and an unknown key are not leaves
    assert_eq!(c.flatten_container_of("plain"), None);
    assert_eq!(c.flatten_container_of("nonexistent"), None);
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "flatten-free-target", template_inline = r#"<div></div>"#)]
struct FlattenFreeTarget {
    #[prop]
    title: String,
}

#[handlers]
impl FlattenFreeTarget {}

#[wasm_bindgen_test]
fn flatten_container_of_is_none_for_a_flatten_free_component() {
    // the macro emits `flatten_container_of` even with zero flatten
    // fields — it must fall through to the `None` default.
    let c = FlattenFreeTarget::default();
    assert_eq!(c.flatten_container_of("title"), None);
    assert_eq!(c.flatten_container_of("anything"), None);
}

#[derive(Default, Serialize, Deserialize)]
struct RangeLeaves {
    start: f64,
    end: f64,
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "model-flatten-map-target", template_inline = r#"<div></div>"#)]
struct ModelFlattenMapTarget {
    #[model(flatten = ["start", "end"])]
    range: RangeLeaves,
}

#[handlers]
impl ModelFlattenMapTarget {}

#[wasm_bindgen_test]
fn flatten_container_of_covers_model_flatten_leaves() {
    let c = ModelFlattenMapTarget::default();
    // role-agnostic — `#[model(flatten)]` leaves map to their
    // container exactly like `#[prop(flatten)]` leaves.
    assert_eq!(c.flatten_container_of("start"), Some("range"));
    assert_eq!(c.flatten_container_of("end"), Some("range"));
    assert_eq!(c.flatten_container_of("range"), None);
    // …and a model-flatten leaf still carries the model role
    // (unlike a `#[prop(flatten)]` leaf, which is inbound only).
    assert!(c.is_model("start"));
    assert!(c.is_prop("start"));
}

// ─────────────────────── dual-key watch triggering ──────────────────────

thread_local! {
    static COMMON_FIRES: Cell<u32> = const { Cell::new(0) };
    static LABEL_FIRES: Cell<u32> = const { Cell::new(0) };
    static LAST_COMMON_LABEL: RefCell<String> = const { RefCell::new(String::new()) };
}

#[derive(Default, Clone, PartialEq, Serialize, Deserialize)]
struct WatchLeaves {
    label: String,
    color: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "flatten-watch-child",
    template_inline = r#"<div><span class="lbl" pp-text="label"></span></div>"#
)]
struct FlattenWatchChild {
    #[prop(flatten = ["label", "color"])]
    common: WatchLeaves,
}

#[handlers]
impl FlattenWatchChild {
    // RFC-044 §5.10.5 — one watcher for the whole flattened struct.
    // Dual-key triggering must fire it when any leaf changes.
    #[watch(common)]
    fn on_common(&mut self, new: WatchLeaves, _: Option<WatchLeaves>) {
        COMMON_FIRES.with(|c| c.set(c.get() + 1));
        LAST_COMMON_LABEL.with(|s| *s.borrow_mut() = new.label.clone());
    }

    // the per-leaf watch must keep firing for the same write
    #[watch(label)]
    fn on_label(&mut self, _: String, _: Option<String>) {
        LABEL_FIRES.with(|c| c.set(c.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "flatten-watch-parent",
    template_inline = r#"<div>
        <flatten-watch-child pp-bind:label="child_label"></flatten-watch-child>
        <button class="bump" pp-on:click="bump"></button>
    </div>"#
)]
struct FlattenWatchParent {
    child_label: String,
}

#[handlers]
impl FlattenWatchParent {
    fn bump(&mut self) {
        self.child_label = "changed".into();
    }
}

#[wasm_bindgen_test]
async fn writing_a_leaf_fires_both_the_leaf_and_the_container_watch() {
    FlattenWatchChild::register();
    FlattenWatchParent::register();
    let host = mount(
        "flatten-watch-parent",
        r#"<flatten-watch-parent></flatten-watch-parent>"#,
    );
    settle().await;

    // Discard the watches' deferred initial fire so the assertions
    // below isolate the effect of the leaf write.
    COMMON_FIRES.with(|c| c.set(0));
    LABEL_FIRES.with(|c| c.set(0));
    LAST_COMMON_LABEL.with(|s| s.borrow_mut().clear());

    // Click → parent `child_label` flips → `pp-bind:label` writes the
    // child's `label` leaf through the child's proxy `set` trap — the
    // exact path RFC-044 §5.10.5 dual-key triggering hooks.
    host.query_selector(".bump")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    settle().await;

    assert_eq!(
        COMMON_FIRES.with(|c| c.get()),
        1,
        "#[watch(common)] fires once when a flattened leaf changes",
    );
    assert_eq!(
        LABEL_FIRES.with(|c| c.get()),
        1,
        "the per-leaf #[watch(label)] still fires for the same write",
    );
    assert_eq!(
        LAST_COMMON_LABEL.with(|s| s.borrow().clone()),
        "changed",
        "the container watch observes the updated leaf value",
    );
    assert_eq!(read(&host, ".lbl"), "changed");

    host.remove();
}

// ──────────── #[derive(Props)] + bare #[prop(flatten)] ────────────

#[derive(Props, Default, Clone, PartialEq, Serialize, Deserialize)]
struct BareLeaves {
    #[prop]
    heading: String,
    #[prop]
    code: String,
    #[prop]
    enabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "bare-flatten-target",
    template_inline = r#"<div>
        <span class="b-heading" pp-text="heading"></span>
        <span class="b-code" pp-text="code"></span>
        <span class="b-enabled" pp-text="enabled"></span>
    </div>"#
)]
struct BareFlattenTarget {
    #[prop(flatten)]
    leaves: BareLeaves,
}

#[handlers]
impl BareFlattenTarget {}

#[wasm_bindgen_test]
fn bare_flatten_via_derive_props_exposes_leaves_through_the_trait() {
    // No explicit leaf list on `#[prop(flatten)]` — the macro routes
    // through `<BareLeaves as Props>::prop_leaves()` at runtime.
    let c = BareFlattenTarget::default();
    // leaves are props (parent-writable)
    assert!(c.is_prop("heading"));
    assert!(c.is_prop("code"));
    assert!(c.is_prop("enabled"));
    // …but NOT models — `#[prop(flatten)]` is inbound-only
    assert!(!c.is_model("heading"));
    // leaf → container map resolved at runtime
    assert_eq!(c.flatten_container_of("heading"), Some("leaves"));
    assert_eq!(c.flatten_container_of("code"), Some("leaves"));
    assert_eq!(c.flatten_container_of("enabled"), Some("leaves"));
    // the container field itself is not a leaf
    assert_eq!(c.flatten_container_of("leaves"), None);
    // unknown key
    assert_eq!(c.flatten_container_of("nope"), None);
}

#[wasm_bindgen_test]
async fn bare_flatten_leaves_round_trip_through_static_attrs() {
    BareFlattenTarget::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(
        r#"<bare-flatten-target
            heading="Sales"
            code="2024"
            enabled="true"></bare-flatten-target>"#,
    );
    body.append_child(&host).unwrap();
    let el = host.query_selector("bare-flatten-target").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "bare-flatten-target");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    tick().await;

    // Each leaf reaches `self.leaves.<leaf>` and renders by its bare
    // wire name. `code="2024"` proves exact-type coercion: the derive
    // reports `<String as PropValue>::prop_static_kind() == String`
    // (no runtime probe), so the numeric-looking value stays a string
    // and is NOT coerced to a number — closing the §5.10.3 gap.
    assert_eq!(read(&host, ".b-heading"), "Sales");
    assert_eq!(read(&host, ".b-code"), "2024");
    assert_eq!(read(&host, ".b-enabled"), "true");

    host.remove();
}
