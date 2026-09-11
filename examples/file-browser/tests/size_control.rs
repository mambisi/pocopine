//! Browser coverage for the size control's computed display and input actions.
#![cfg(target_arch = "wasm32")]

use std::cell::Cell;

use file_browser_example::FileBrowserSizeControl;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

thread_local! {
    static OWNER: Cell<Option<ScopeId>> = const { Cell::new(None) };
}

#[derive(Default, Serialize, Deserialize)]
#[component(name = "size-control-owner", uses = [FileBrowserSizeControl], template = poco! {
    <div>
      <file-browser-size-control :value="value" min="1" :max="max" :disabled="disabled"
                                 @pp:update:value.self="accept"></file-browser-size-control>
    </div>
})]
struct SizeControlOwner {
    value: f64,
    max: f64,
    disabled: bool,
    commits: u32,
}

#[handlers]
impl SizeControlOwner {
    fn on_mount(&mut self) {
        self.value = 1.0;
        self.max = 1024.0;
    }

    fn on_ready(&self) {
        OWNER.with(|id| id.set(pocopine::current_scope_id()));
    }

    fn accept(&mut self, event: web_sys::CustomEvent) {
        self.value = event.detail().as_f64().unwrap();
        self.commits += 1;
    }
}

fn input(host: &web_sys::Element, selector: &str) -> web_sys::HtmlInputElement {
    host.query_selector(selector)
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap()
}

fn edit(input: &web_sys::HtmlInputElement, value: &str) {
    input.set_value(value);
    input
        .dispatch_event(&web_sys::Event::new("input").unwrap())
        .unwrap();
}

async fn settle() {
    for _ in 0..16 {
        let _ =
            wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL)).await;
    }
}

#[wasm_bindgen_test]
async fn inputs_commit_once_and_prop_changes_only_update_display() {
    let document = web_sys::window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    document.body().unwrap().append_child(&host).unwrap();
    let mounted = App::mount_subtree::<SizeControlOwner>(&host);
    settle().await;
    let id = OWNER.with(Cell::get).unwrap();
    let handle = Handle::new(
        Scope::find(id)
            .unwrap()
            .typed::<SizeControlOwner>()
            .unwrap(),
        id,
    );
    let amount = input(&host, ".cfe-size-control-number");
    let range = input(&host, ".cfe-size-control-range");
    let unit: web_sys::HtmlSelectElement = host
        .query_selector("select")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    assert_eq!(amount.value_as_number(), 1.0);
    assert_eq!(unit.value(), "MB");
    assert_eq!(handle.with(|s| s.commits), 0);

    unit.set_value("GB");
    unit.dispatch_event(&web_sys::Event::new("change").unwrap())
        .unwrap();
    settle().await;
    assert_eq!(handle.with(|s| (s.value, s.commits)), (1024.0, 1));
    edit(&amount, "0.5");
    settle().await;
    assert_eq!(handle.with(|s| (s.value, s.commits)), (512.0, 2));
    assert_eq!(range.value_as_number(), 512.0);
    assert_eq!(amount.value_as_number(), 0.5);

    // External constraints project the display without implicitly writing to the owner.
    handle.update(|s| s.max = 64.0);
    settle().await;
    assert_eq!(handle.with(|s| (s.value, s.commits)), (512.0, 2));
    assert_eq!(amount.value_as_number(), 64.0);
    assert_eq!(range.value_as_number(), 64.0);
    assert_eq!(unit.value(), "MB");
    edit(&amount, "32");
    settle().await;
    assert_eq!(handle.with(|s| (s.value, s.commits)), (32.0, 3));

    // Invalid and no-op input is restored even when there is no reactive change.
    edit(&amount, "");
    settle().await;
    assert_eq!(handle.with(|s| (s.value, s.commits)), (1.0, 4));
    edit(&amount, "0");
    settle().await;
    assert_eq!(amount.value_as_number(), 1.0);
    assert_eq!(handle.with(|s| s.commits), 4);
    handle.update(|s| s.disabled = true);
    settle().await;
    edit(&amount, "8");
    settle().await;
    assert_eq!(amount.value_as_number(), 1.0);
    assert_eq!(handle.with(|s| s.commits), 4);
    mounted.unmount();
    host.remove();
}
