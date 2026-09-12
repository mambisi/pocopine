//! RFC-112 browser coverage for `<pp-component :is>`.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;

use pocopine::flush_sync;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static ALPHA_MOUNTS: Cell<u32> = const { Cell::new(0) };
    static ALPHA_UNMOUNTS: Cell<u32> = const { Cell::new(0) };
    static BETA_MOUNTS: Cell<u32> = const { Cell::new(0) };
    static BETA_UNMOUNTS: Cell<u32> = const { Cell::new(0) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-dynamic-alpha",
    template = poco! {<article class="dc-alpha">
        <span class="dc-alpha-label" pp-text="label"></span>
        <span class="dc-alpha-setup" pp-text="setup_label"></span>
        <span class="dc-alpha-count" pp-text="count"></span>
        <button class="dc-alpha-bump" @click="bump">bump</button>
        <button class="dc-alpha-edit" @click="edit_label">edit label</button>
    </article>}
)]
struct DynamicAlpha {
    #[prop]
    label: String,
    setup_label: String,
    #[prop]
    note: String,
    count: u32,
}

#[handlers]
impl DynamicAlpha {
    pub fn on_setup(&mut self) {
        self.setup_label = self.label.clone();
        ALPHA_MOUNTS.with(|count| count.set(count.get() + 1));
    }

    pub fn on_unmount(&mut self) {
        ALPHA_UNMOUNTS.with(|count| count.set(count.get() + 1));
    }

    fn edit_label(&mut self) {
        self.label = "local draft".into();
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-dynamic-beta",
    template = poco! {<article class="dc-beta">
        <span class="dc-beta-label" pp-text="label"></span>
    </article>}
)]
struct DynamicBeta {
    #[prop]
    label: String,
}

#[handlers]
impl DynamicBeta {
    pub fn on_setup(&mut self) {
        BETA_MOUNTS.with(|count| count.set(count.get() + 1));
    }

    pub fn on_unmount(&mut self) {
        BETA_UNMOUNTS.with(|count| count.set(count.get() + 1));
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-dynamic-host",
    uses = [DynamicAlpha, DynamicBeta],
    template = poco! {<section class="dc-host">
        <pp-component
            :is="key"
            :label="label"
            pp-transition:enter-start="dc-enter-start"
        ></pp-component>
        <button class="dc-show-alpha" @click="show_alpha">alpha</button>
        <button class="dc-show-beta" @click="show_beta">beta</button>
        <button class="dc-show-empty" @click="show_empty">empty</button>
        <button class="dc-rename" @click="rename">rename</button>
    </section>}
)]
struct DynamicHost {
    key: Option<ComponentRef<DynamicHost>>,
    label: String,
}

#[handlers]
impl DynamicHost {
    pub fn on_setup(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicAlpha>());
        self.label = "initial".into();
    }

    pub fn show_alpha(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicAlpha>());
    }

    pub fn show_beta(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicBeta>());
    }

    pub fn show_empty(&mut self) {
        self.key = None;
    }

    pub fn rename(&mut self) {
        self.label = "updated".into();
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-keep-alive-host",
    uses = [DynamicAlpha, DynamicBeta],
    template = poco! {<section class="dc-keep-host">
        <pp-component :is="key" :label="label" keep-alive></pp-component>
        <button class="dc-keep-alpha" @click="show_alpha">alpha</button>
        <button class="dc-keep-beta" @click="show_beta">beta</button>
    </section>}
)]
struct KeepAliveHost {
    key: Option<ComponentRef<KeepAliveHost>>,
    label: String,
}

#[handlers]
impl KeepAliveHost {
    pub fn on_setup(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicAlpha>());
        self.label = "cached".into();
    }

    pub fn show_alpha(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicAlpha>());
    }

    pub fn show_beta(&mut self) {
        self.key = Some(ComponentRef::of::<DynamicBeta>());
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-data-driven-host",
    uses = [DynamicAlpha],
    template = poco! {<section>
        <pp-component :is="key" label="data-driven"></pp-component>
        <button class="dc-show-unknown" @click="show_unknown">unknown</button>
    </section>}
)]
struct DataDrivenHost {
    key: Option<ComponentRef<DataDrivenHost>>,
}

#[handlers]
impl DataDrivenHost {
    pub fn on_setup(&mut self) {
        self.key = ComponentRef::<Self>::from_registered_name("dc-dynamic-alpha");
    }

    pub fn show_unknown(&mut self) {
        self.key = ComponentRef::<Self>::from_registered_name("dc-not-registered");
    }
}

/// Mirrors the accidental application pattern this contract must reject:
/// `$store` exposes a raw registered tag string instead of a `ComponentRef`.
#[derive(Serialize, Deserialize)]
#[store(name = "dc_untyped_selection")]
struct UntypedSelectionStore {
    key: String,
}

impl Default for UntypedSelectionStore {
    fn default() -> Self {
        Self {
            key: "dc-dynamic-alpha".into(),
        }
    }
}

#[handlers]
impl UntypedSelectionStore {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-untyped-selection-host",
    uses = [DynamicAlpha],
    template = poco! {<pp-component :is="$store.dc_untyped_selection.key"></pp-component>}
)]
struct UntypedSelectionHost {}

#[handlers]
impl UntypedSelectionHost {}

/// `$store` is outside the component macro's Rust type view, so retain a
/// runtime check that a typed token is consumed by its declared host.
#[derive(Serialize, Deserialize)]
#[store(name = "dc_foreign_selection")]
struct ForeignSelectionStore {
    key: Option<ComponentRef<DataDrivenHost>>,
}

impl Default for ForeignSelectionStore {
    fn default() -> Self {
        Self {
            key: Some(ComponentRef::of::<DynamicAlpha>()),
        }
    }
}

#[handlers]
impl ForeignSelectionStore {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-foreign-selection-host",
    uses = [DynamicAlpha],
    template = poco! {<pp-component :is="$store.dc_foreign_selection.key"></pp-component>},
)]
struct ForeignSelectionHost {}

#[handlers]
impl ForeignSelectionHost {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-keyed-host",
    uses = [DynamicAlpha, DynamicBeta],
    template = poco! {<section>
        <pp-component pp-key="identity" :is="active" :label="label" :note="note" :keep-alive="keep"></pp-component>
        <button class="dc-keyed-note" @click="change_note">note</button>
        <button class="dc-keyed-next" @click="next">next</button>
        <button class="dc-keyed-first" @click="first">first</button>
        <button class="dc-keyed-rename" @click="rename">rename</button>
        <button class="dc-keyed-keep" @click="enable_keep">keep</button>
        <button class="dc-keyed-number" @click="number">number</button>
        <button class="dc-keyed-invalid" @click="invalid">invalid</button>
        <button class="dc-keyed-empty" @click="empty">empty</button>
    </section>}
)]
struct KeyedHost {
    active: Option<ComponentRef<KeyedHost>>,
    identity: serde_json::Value,
    label: String,
    note: String,
    keep: bool,
}

#[handlers]
impl KeyedHost {
    fn on_setup(&mut self) {
        self.first();
    }
    fn first(&mut self) {
        self.identity = serde_json::json!("1");
        self.active = Some(ComponentRef::of::<DynamicAlpha>());
        self.label = "first".into();
    }
    fn next(&mut self) {
        self.identity = serde_json::json!("2");
        self.label = "second".into();
    }
    fn rename(&mut self) {
        self.label = "renamed".into();
    }
    fn change_note(&mut self) {
        self.note = "updated note".into();
    }
    fn enable_keep(&mut self) {
        self.keep = true;
    }
    fn number(&mut self) {
        self.identity = serde_json::json!(1);
        self.label = "numeric".into();
    }
    fn invalid(&mut self) {
        self.identity = serde_json::json!([1]);
    }
    fn empty(&mut self) {
        self.identity = serde_json::Value::Null;
        self.active = None;
    }
}

fn document() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount<C: Component>() -> (Element, pocopine::SubtreeHandle) {
    let host = document().create_element("div").unwrap();
    document().body().unwrap().append_child(&host).unwrap();
    let handle = App::mount_subtree::<C>(&host);
    (host, handle)
}

fn click(root: &Element, selector: &str) {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
}

fn text(root: &Element, selector: &str) -> String {
    root.query_selector(selector)
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap_or_default()
}

fn reset_counts() {
    ALPHA_MOUNTS.with(|count| count.set(0));
    ALPHA_UNMOUNTS.with(|count| count.set(0));
    BETA_MOUNTS.with(|count| count.set(0));
    BETA_UNMOUNTS.with(|count| count.set(0));
}

#[wasm_bindgen_test]
fn dynamic_component_swaps_forwards_props_and_cleans_empty_selection() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<DynamicHost>();

    assert_eq!(text(&host, ".dc-alpha-label"), "initial");
    assert_eq!(text(&host, ".dc-alpha-setup"), "initial");
    click(&host, ".dc-rename");
    // Parent binding effect writes the child prop in the first flush; the
    // child's text effect observes that prop write in the second.
    flush_sync();
    flush_sync();
    assert_eq!(text(&host, ".dc-alpha-label"), "updated");

    click(&host, ".dc-show-beta");
    flush_sync();
    assert!(host.query_selector(".dc-alpha").unwrap().is_none());
    assert_eq!(text(&host, ".dc-beta-label"), "updated");
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 1);

    click(&host, ".dc-show-alpha");
    flush_sync();
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 2);
    click(&host, ".dc-show-empty");
    flush_sync();
    assert!(host.query_selector(".dc-alpha").unwrap().is_none());

    handle.unmount();
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test]
fn unknown_data_driven_name_unmounts_the_current_component() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<DataDrivenHost>();

    assert!(host.query_selector(".dc-alpha").unwrap().is_some());
    assert_eq!(text(&host, ".dc-alpha-label"), "data-driven");
    click(&host, ".dc-show-unknown");
    flush_sync();
    assert!(host.query_selector(".dc-alpha").unwrap().is_none());
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 1);

    handle.unmount();
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test]
fn raw_store_string_is_not_a_dynamic_component_selection() {
    reset_counts();
    UntypedSelectionStore::__register_store();

    let (host, handle) = mount::<UntypedSelectionHost>();

    assert!(
        host.query_selector(".dc-alpha").unwrap().is_none(),
        "a registered tag string must not bypass the ComponentRef contract",
    );
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 0);

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
fn component_ref_cannot_cross_dynamic_hosts() {
    reset_counts();
    ForeignSelectionStore::__register_store();

    let (host, handle) = mount::<ForeignSelectionHost>();

    assert!(
        host.query_selector(".dc-alpha").unwrap().is_none(),
        "a ComponentRef minted for another host must be rejected",
    );
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 0);

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
fn keep_alive_reuses_the_same_instance_and_preserves_state() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<KeepAliveHost>();

    click(&host, ".dc-alpha-bump");
    flush_sync();
    assert_eq!(text(&host, ".dc-alpha-count"), "1");
    let alpha_host = host.query_selector("dc-dynamic-alpha").unwrap().unwrap();

    click(&host, ".dc-keep-beta");
    flush_sync();
    assert!(alpha_host.has_attribute("hidden"));
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 0);

    click(&host, ".dc-keep-alpha");
    flush_sync();
    let restored = host.query_selector("dc-dynamic-alpha").unwrap().unwrap();
    assert!(alpha_host.is_same_node(Some(restored.as_ref())));
    assert!(!restored.has_attribute("hidden"));
    assert_eq!(text(&host, ".dc-alpha-count"), "1");
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 1);

    handle.unmount();
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 1);
    assert_eq!(BETA_UNMOUNTS.with(Cell::get), 1);
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test]
fn transition_configuration_is_forwarded_to_each_dynamic_child() {
    reset_counts();
    let (host, handle) = mount::<DynamicHost>();
    let alpha = host.query_selector("dc-dynamic-alpha").unwrap().unwrap();
    assert_eq!(
        alpha.get_attribute("pp-transition:enter-start").as_deref(),
        Some("dc-enter-start"),
    );
    assert!(alpha.class_list().contains("dc-enter-start"));

    handle.unmount();
    host.remove();
}

#[wasm_bindgen_test]
fn keyed_replacement_seeds_current_props_and_releases_previous_state() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<KeyedHost>();
    let first = host.query_selector("dc-dynamic-alpha").unwrap().unwrap();
    assert_eq!(text(&host, ".dc-alpha-setup"), "first");
    assert!(!first.has_attribute("pp-key"));
    click(&host, ".dc-alpha-bump");
    click(&host, ".dc-keyed-rename");
    flush_sync();
    flush_sync();
    assert_eq!(text(&host, ".dc-alpha-count"), "1");
    assert_eq!(text(&host, ".dc-alpha-label"), "renamed");
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 1);

    click(&host, ".dc-alpha-edit");
    flush_sync();
    click(&host, ".dc-keyed-note");
    flush_sync();
    flush_sync();
    assert_eq!(
        text(&host, ".dc-alpha-label"),
        "local draft",
        "an unrelated prop must not replay an unchanged binding"
    );

    click(&host, ".dc-keyed-next");
    flush_sync();
    assert_eq!(text(&host, ".dc-alpha-setup"), "second");
    assert_eq!(text(&host, ".dc-alpha-count"), "0");
    assert!(!first.is_connected());
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 2);
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 1);

    click(&host, ".dc-keyed-empty");
    flush_sync();
    assert!(host.query_selector("dc-dynamic-alpha").unwrap().is_none());
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 2);
    handle.unmount();
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test]
fn keyed_keep_alive_caches_by_type_and_key_and_releases_every_instance() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<KeyedHost>();
    click(&host, ".dc-keyed-keep");
    click(&host, ".dc-alpha-bump");
    flush_sync();
    let first = host.query_selector("dc-dynamic-alpha").unwrap().unwrap();

    click(&host, ".dc-keyed-number");
    flush_sync();
    assert!(first.has_attribute("hidden"));
    assert_eq!(
        ALPHA_MOUNTS.with(Cell::get),
        2,
        "number and string keys are distinct"
    );
    click(&host, ".dc-keyed-next");
    flush_sync();
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 3);
    click(&host, ".dc-keyed-first");
    flush_sync();
    assert!(!first.has_attribute("hidden"));
    assert_eq!(
        first
            .query_selector(".dc-alpha-count")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap(),
        "1"
    );
    assert_eq!(ALPHA_MOUNTS.with(Cell::get), 3);
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 0);

    handle.unmount();
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 3);
    host.remove();
    pocopine::animate::enable_transitions();
}

#[wasm_bindgen_test]
fn object_keys_are_rejected_instead_of_using_unstable_reference_identity() {
    reset_counts();
    pocopine::animate::disable_transitions();
    let (host, handle) = mount::<KeyedHost>();
    click(&host, ".dc-keyed-invalid");
    flush_sync();
    assert!(host.query_selector("dc-dynamic-alpha").unwrap().is_none());
    assert_eq!(ALPHA_UNMOUNTS.with(Cell::get), 1);
    click(&host, ".dc-keyed-first");
    flush_sync();
    assert_eq!(text(&host, ".dc-alpha-setup"), "first");
    handle.unmount();
    host.remove();
    pocopine::animate::enable_transitions();
}
