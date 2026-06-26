//! Regression for issue #239 — a `#[computed]` over a component-local
//! field did not re-run when that field changed.
//!
//! The issue framed it as teleport-specific (the computed lived in a
//! Pine popover portal). It is NOT: the `without_teleport` control
//! below — a child component with no teleport at all — failed
//! identically before the fix. Root cause was the per-signal
//! projection cache holding a `#[computed]` field's first value
//! forever: that cache is only invalidated by a write to the field
//! itself, but a computed is never written, so an upstream change
//! recomputed the `Computed` yet kept serving the stale projection.
//! Fix: computed fields skip the projection cache (their own
//! `Computed<JsValue>` already memoizes them).
//!
//! Controls, in increasing nesting, each typing into a `pp-model`
//! input and asserting the derived count re-runs:
//! * `without_teleport` — child component, no teleport (the control)
//! * `bare_teleport` — child is a DIRECT child of a
//!   `pp-if + pp-teleport` body
//! * `popover_slot` — child is SLOT content of a Pine popover
//!   (the exact shape reported in the issue)
//!
//! `#[watch]` variants are included too: the watch source read bypasses
//! the projection cache, so watch was never actually broken — these
//! guard that it stays working in a teleport.

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, HtmlInputElement, window};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn mount_fixture<C: pocopine::__private::Component>() -> Element {
    pine::register_all();
    <MemberPicker as pocopine::__private::Component>::register();
    <MemberPickerWatch as pocopine::__private::Component>::register();
    pocopine_core::animate::disable_transitions();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    let root = doc().create_element(C::NAME).unwrap();
    host.append_child(&root).unwrap();
    body.append_child(&host).unwrap();
    let mounted = pocopine::App::mount_subtree::<C>(&root);
    pocopine_core::mount::finalize_compiled_subtree(&root);
    mounted.leak();
    host
}

fn text_of(el: &Element) -> String {
    el.clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

fn type_into(input: &Element, value: &str) {
    let inp = input.clone().dyn_into::<HtmlInputElement>().unwrap();
    inp.set_value(value);
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    let ev = web_sys::Event::new_with_event_init_dict("input", &init).unwrap();
    input.dispatch_event(&ev).unwrap();
}

fn dispatch_click(target: &Element) {
    for event in ["pointerdown", "pointerup"] {
        let init = web_sys::PointerEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        let ev = web_sys::PointerEvent::new_with_event_init_dict(event, &init).unwrap();
        target.dispatch_event(&ev).unwrap();
    }
    for event in ["mousedown", "mouseup", "click"] {
        let init = web_sys::MouseEventInit::new();
        init.set_bubbles(true);
        init.set_cancelable(true);
        let ev = web_sys::MouseEvent::new_with_mouse_event_init_dict(event, &init).unwrap();
        target.dispatch_event(&ev).unwrap();
    }
}

// ── the component under test ───────────────────────────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(
    display = "contents",
    template_inline = r#"
<root style="display:contents">
  <input class="mp-input" pp-model="search" />
  <span class="mp-q">{{ search }}</span>
  <span class="mp-n">{{ filtered.length }}</span>
</root>
"#
)]
struct MemberPicker {
    search: String,
}

#[handlers]
impl MemberPicker {
    #[computed]
    fn filtered(search: String) -> Vec<String> {
        let all = ["alice", "bob", "scott", "scarlett", "sam", "dave"];
        let q = search.trim().to_lowercase();
        all.iter()
            .filter(|m| q.is_empty() || m.to_lowercase().contains(&q))
            .map(|s| (*s).to_string())
            .collect()
    }
}

// ── control: no teleport ───────────────────────────────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="plain-host">
  <member-picker></member-picker>
</div>
"#)]
struct PlainHost {}

#[handlers]
impl PlainHost {}

#[wasm_bindgen_test]
async fn computed_reruns_without_teleport() {
    let host = mount_fixture::<PlainHost>();
    tick().await;

    let input = host.query_selector(".mp-input").unwrap().expect("input");
    let n = host.query_selector(".mp-n").unwrap().expect("n");
    assert_eq!(text_of(&n), "6", "initial count");

    type_into(&input, "sc");
    tick().await;
    tick().await;

    assert_eq!(
        text_of(&host.query_selector(".mp-q").unwrap().unwrap()),
        "sc",
        "interpolation of own field updates"
    );
    assert_eq!(
        text_of(&host.query_selector(".mp-n").unwrap().unwrap()),
        "2",
        "[control] computed re-runs without a teleport"
    );

    host.remove();
}

// ── bare teleport: member-picker is a direct child of the body ─────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <button class="toggle" @click="toggle">t</button>
  <template pp-if="open" pp-teleport="body">
    <div class="portal-b">
      <member-picker></member-picker>
    </div>
  </template>
</div>
"#)]
struct BareTeleportHost {
    open: bool,
}

#[handlers]
impl BareTeleportHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[wasm_bindgen_test]
async fn computed_reruns_in_bare_teleport() {
    let host = mount_fixture::<BareTeleportHost>();
    tick().await;

    dispatch_click(&host.query_selector(".toggle").unwrap().unwrap());
    tick().await;
    tick().await;

    let input = doc()
        .query_selector(".portal-b .mp-input")
        .unwrap()
        .expect("teleported input");
    assert_eq!(
        text_of(&doc().query_selector(".portal-b .mp-n").unwrap().unwrap()),
        "6",
        "initial count (teleported)"
    );

    type_into(&input, "sc");
    tick().await;
    tick().await;

    assert_eq!(
        text_of(&doc().query_selector(".portal-b .mp-q").unwrap().unwrap()),
        "sc",
        "interpolation of own field updates (teleported)"
    );
    assert_eq!(
        text_of(&doc().query_selector(".portal-b .mp-n").unwrap().unwrap()),
        "2",
        "[bare teleport] computed re-runs inside a pp-teleport body"
    );

    host.remove();
}

// ── popover-slotted: the exact shape from the issue ────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <pine-popover-root>
    <pine-popover-trigger class="pop-trig">open</pine-popover-trigger>
    <pine-popover-portal>
      <pine-popover-content>
        <member-picker></member-picker>
      </pine-popover-content>
    </pine-popover-portal>
  </pine-popover-root>
</div>
"#)]
struct PopoverSlottedHost {}

#[handlers]
impl PopoverSlottedHost {}

#[wasm_bindgen_test]
async fn computed_reruns_in_popover_slot() {
    let host = mount_fixture::<PopoverSlottedHost>();
    tick().await;

    host.query_selector(".pop-trig")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    tick().await;

    let input = doc()
        .query_selector(".pine-popover-content .mp-input")
        .unwrap()
        .expect("popover input teleported to body");
    assert_eq!(
        text_of(
            &doc()
                .query_selector(".pine-popover-content .mp-n")
                .unwrap()
                .unwrap()
        ),
        "6",
        "initial count (popover slot)"
    );

    type_into(&input, "sc");
    tick().await;
    tick().await;

    assert_eq!(
        text_of(
            &doc()
                .query_selector(".pine-popover-content .mp-q")
                .unwrap()
                .unwrap()
        ),
        "sc",
        "interpolation of own field updates (popover slot)"
    );
    assert_eq!(
        text_of(
            &doc()
                .query_selector(".pine-popover-content .mp-n")
                .unwrap()
                .unwrap()
        ),
        "2",
        "[popover slot] computed re-runs for a slotted component in a teleport"
    );

    host.remove();
}

// ── #[watch] variant — the issue claims watch fails identically ────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(
    display = "contents",
    template_inline = r#"
<root style="display:contents">
  <input class="mpw-input" pp-model="search" />
  <span class="mpw-q">{{ search }}</span>
  <span class="mpw-n">{{ count }}</span>
</root>
"#
)]
struct MemberPickerWatch {
    search: String,
    count: usize,
}

#[handlers]
impl MemberPickerWatch {
    #[watch(search)]
    fn on_search(&mut self, new: String, _prev: Option<String>) {
        let all = ["alice", "bob", "scott", "scarlett", "sam", "dave"];
        let q = new.trim().to_lowercase();
        self.count = all
            .iter()
            .filter(|m| q.is_empty() || m.to_lowercase().contains(&q))
            .count();
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="plain-watch-host">
  <member-picker-watch></member-picker-watch>
</div>
"#)]
struct PlainWatchHost {}

#[handlers]
impl PlainWatchHost {}

#[wasm_bindgen_test]
async fn watch_fires_without_teleport() {
    let host = mount_fixture::<PlainWatchHost>();
    tick().await;
    tick().await;

    let input = host.query_selector(".mpw-input").unwrap().expect("input");
    assert_eq!(
        text_of(&host.query_selector(".mpw-n").unwrap().unwrap()),
        "6",
        "initial watch-computed count"
    );

    type_into(&input, "sc");
    tick().await;
    tick().await;

    assert_eq!(
        text_of(&host.query_selector(".mpw-n").unwrap().unwrap()),
        "2",
        "[control] #[watch] re-runs without a teleport"
    );

    host.remove();
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div>
  <button class="toggle-w" @click="toggle">t</button>
  <template pp-if="open" pp-teleport="body">
    <div class="portal-w">
      <member-picker-watch></member-picker-watch>
    </div>
  </template>
</div>
"#)]
struct BareTeleportWatchHost {
    open: bool,
}

#[handlers]
impl BareTeleportWatchHost {
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
}

#[wasm_bindgen_test]
async fn watch_fires_in_bare_teleport() {
    let host = mount_fixture::<BareTeleportWatchHost>();
    tick().await;

    dispatch_click(&host.query_selector(".toggle-w").unwrap().unwrap());
    tick().await;
    tick().await;

    let input = doc()
        .query_selector(".portal-w .mpw-input")
        .unwrap()
        .expect("teleported input");
    assert_eq!(
        text_of(&doc().query_selector(".portal-w .mpw-n").unwrap().unwrap()),
        "6",
        "initial watch-computed count (teleported)"
    );

    type_into(&input, "sc");
    tick().await;
    tick().await;

    assert_eq!(
        text_of(&doc().query_selector(".portal-w .mpw-n").unwrap().unwrap()),
        "2",
        "[bare teleport] #[watch] re-runs inside a pp-teleport body"
    );

    host.remove();
}
