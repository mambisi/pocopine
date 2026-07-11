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
    template_inline = r#"<article class="dc-alpha">
        <span class="dc-alpha-label" pp-text="label"></span>
        <span class="dc-alpha-setup" pp-text="setup_label"></span>
        <span class="dc-alpha-count" pp-text="count"></span>
        <button class="dc-alpha-bump" @click="bump">bump</button>
    </article>"#
)]
struct DynamicAlpha {
    #[prop]
    label: String,
    setup_label: String,
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

    pub fn bump(&mut self) {
        self.count += 1;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "dc-dynamic-beta",
    template_inline = r#"<article class="dc-beta">
        <span class="dc-beta-label" pp-text="label"></span>
    </article>"#
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
    template_inline = r#"<section class="dc-host">
        <pp-component
            :is="key"
            :label="label"
            pp-transition:enter-start="dc-enter-start"
        ></pp-component>
        <button class="dc-show-alpha" @click="show_alpha">alpha</button>
        <button class="dc-show-beta" @click="show_beta">beta</button>
        <button class="dc-show-empty" @click="show_empty">empty</button>
        <button class="dc-rename" @click="rename">rename</button>
    </section>"#
)]
struct DynamicHost {
    key: Option<ComponentRef>,
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
    template_inline = r#"<section class="dc-keep-host">
        <pp-component :is="key" :label="label" keep-alive></pp-component>
        <button class="dc-keep-alpha" @click="show_alpha">alpha</button>
        <button class="dc-keep-beta" @click="show_beta">beta</button>
    </section>"#
)]
struct KeepAliveHost {
    key: Option<ComponentRef>,
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
    template_inline = r#"<section>
        <pp-component :is="key" label="data-driven"></pp-component>
        <button class="dc-show-unknown" @click="show_unknown">unknown</button>
    </section>"#
)]
struct DataDrivenHost {
    key: String,
}

#[handlers]
impl DataDrivenHost {
    pub fn on_setup(&mut self) {
        self.key = ComponentRef::of::<DynamicAlpha>().to_string();
    }

    pub fn show_unknown(&mut self) {
        self.key = "dc-not-registered".into();
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
