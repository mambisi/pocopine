//! RFC-058 Phase 6.5 — Adopted-DOM bridge contract tests.
//!
//! The runtime walker is gone (`walker.rs` no longer scans
//! `pp-*` attributes). What survives is a narrow adopted-DOM
//! bridge — three helpers in `walker.rs` that handle the cases
//! where the macro never compiled the DOM tree (user-authored
//! slot content, runtime-injected partials, app-root mounts
//! whose host is a literal HTML string):
//!
//!   - [`mount_adopted_components`] — discover registered
//!     custom-component tags + mount each;
//!   - [`install_adopted_controllers`] — discover
//!     `<template pp-for>` / `<template pp-if>` /
//!     `<template pp-teleport>` and install controllers;
//!   - [`materialize_adopted_slot`] — splice runtime-captured
//!     slot content back when `slot_fragment::lookup` misses.
//!
//! This file pins the contract so the bridge can't silently
//! grow into a runtime walker again. The four cases below
//! together codify the surface:
//!
//! | # | Pattern | Expected |
//! |---|---|---|
//! | 1 | Custom component tag inside runtime slot content | mounts (template renders, scope wired) |
//! | 2 | `<template pp-for>` inside runtime slot content | structural controller installs (rows materialise) |
//! | 3 | `pp-text` / `:prop` / `@event` on plain HTML inside runtime slot content | does **not** bind (literal attr remains) |
//! | 4 | Same content authored inside `#[component(template_inline = ...)]` | binds (macro processed at compile time) |
//!
//! Cases 1 + 2 + 4 cover what the bridge supports. Case 3 is
//! the negative test that locks the architectural decision:
//! per-element directive binding on adopted DOM is **not** a
//! framework feature.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test adopted_dom_contract`

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use pocopine_core::templates::registered_template_names;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

// ─── fixtures ────────────────────────────────────────────────────

/// A minimal compiled child component the contract tests mount
/// from adopted DOM. The template emits a sentinel marker the
/// tests query for to confirm `mount_adopted_components` reached
/// it.
#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = r#"
<span class="adt-child" data-mounted="yes">child mounted</span>
"#)]
struct AdtChild {}

#[handlers]
impl AdtChild {}

/// Compiled host with a `<slot>` outlet — slot content is
/// captured at mount time via the runtime store and replayed by
/// `materialize_adopted_slot`. Used by tests #1, #2, #3 as the
/// boundary between "compiled host" and "adopted-DOM slot
/// content."
#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = r#"
<div class="adt-host">
  <slot></slot>
</div>
"#)]
struct AdtHost {
    /// Backing storage for the case-2 `pp-for` items expression.
    /// Declared as a real field so the host's reactive proxy
    /// triggers subscribers when the test seeds it via
    /// `Reflect::set` — bare JS properties don't ride the
    /// reactivity rails.
    tags: Vec<String>,
}

#[handlers]
impl AdtHost {}

/// Compiled wrapper used by test #4. Same DOM shape as the
/// adopted-DOM cases but authored inside the macro so directives
/// bind via the compiled plan. The `pp-text` / `@click` /
/// `:prop` references resolve against this wrapper's scope.
#[derive(Default, Serialize, Deserialize)]
#[component(template_inline = r#"
<div class="adt-compiled-wrapper">
  <span class="adt-bound-text" pp-text="message"></span>
  <button class="adt-bound-click" @click="bump">{{count}}</button>
</div>
"#)]
struct AdtCompiledWrapper {
    message: String,
    count: i32,
}

#[handlers]
impl AdtCompiledWrapper {
    pub fn on_setup(&mut self) {
        self.message = "compiled".into();
        self.count = 0;
    }

    pub fn bump(&mut self) {
        self.count += 1;
    }
}

// ─── harness ─────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn register_all() {
    AdtChild::register();
    AdtHost::register();
    AdtCompiledWrapper::register();
}

fn mount(host_html: &str) -> Element {
    register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    pocopine_core::walker::start_compiled(&host);
    host
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

// ─── tests ───────────────────────────────────────────────────────

/// **Bridge contract — case 1.**
///
/// A custom component tag authored as adopted-DOM slot content
/// (the macro never sees it; the host's compiled template just
/// declares `<slot></slot>`) gets mounted by the adopted-DOM
/// bridge's `mount_adopted_components` discovery pass.
///
/// Asserts: the inner `<adt-child>` mounts and its template
/// renders the sentinel `data-mounted="yes"` marker.
#[wasm_bindgen_test]
async fn case1_custom_tag_in_adopted_slot_mounts() {
    let host = mount("<adt-host><adt-child></adt-child></adt-host>");
    tick().await;

    let child = host
        .query_selector(".adt-child[data-mounted=\"yes\"]")
        .unwrap()
        .expect("AdtChild's compiled template rendered inside the adopted slot");
    assert!(
        registered_template_names().iter().any(|t| t == "adt-child"),
        "AdtChild is registered",
    );
    assert_eq!(child.text_content().unwrap_or_default(), "child mounted");

    host.remove();
}

/// **Bridge contract — case 2.**
///
/// A `<template pp-for>` authored as adopted-DOM slot content
/// gets its for controller installed by the bridge's
/// `install_adopted_controllers` pass. Each row's body goes
/// through `clone_template_body` + a recursive
/// `mount_adopted_components` so `<adt-child>` rows mount.
///
/// Note: the `:value="tag"` on `<adt-child>` does **not** bind
/// (case 3 covers that). The test asserts STRUCTURE — three
/// rows materialise — not per-row prop forwarding.
#[wasm_bindgen_test]
async fn case2_template_pp_for_in_adopted_slot_materialises_rows() {
    let host = mount(
        r#"<adt-host>
              <template pp-for="tag in tags">
                <adt-child></adt-child>
              </template>
           </adt-host>"#,
    );
    tick().await;

    // Seed `tags` on the adt-host's scope by reaching through the
    // mounted host element (no parent scope authored these — the
    // adopted-DOM bridge's `capture_slots` fallback owns the slot
    // content with adt-host as the author scope, so the for_'s
    // items_expr resolves there).
    let host_tag = host.query_selector("adt-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) =
        pocopine_core::walker::scope_of_element(&host_root).expect("adt-host scope");
    let arr = js_sys::Array::new();
    for v in ["a", "b", "c"] {
        arr.push(&JsValue::from_str(v));
    }
    js_sys::Reflect::set(&host_proxy, &"tags".into(), &arr).unwrap();
    tick().await;

    let kids = host
        .query_selector_all(".adt-child[data-mounted=\"yes\"]")
        .unwrap();
    assert_eq!(
        kids.length(),
        3,
        "<template pp-for> in adopted slot must materialise three rows",
    );

    host.remove();
}

/// **Bridge contract — case 3 (negative).**
///
/// `pp-text` / `:prop` / `@event` on plain HTML hosts inside
/// adopted-DOM slot content are **not** bound. The bridge does
/// not parse runtime `pp-*` directives (that was the deleted
/// walker's job). The directives sit as literal HTML
/// attributes the browser ignores — this test asserts that
/// behaviour so a future regression that tries to "helpfully"
/// re-add a runtime dispatch loop is loud.
///
/// The contract: per-element directive binding on adopted DOM
/// is not a framework feature. Authors who need it wrap the
/// content in `#[component(template_inline = ...)]` (see case 4).
#[wasm_bindgen_test]
async fn case3_pp_text_on_plain_html_in_adopted_slot_does_not_bind() {
    // Mount adopted slot content that authors `pp-text`, `:title`
    // (the `:prop` shorthand), and `@click` on a plain `<span>` /
    // `<button>`. None of these should bind: the spans should
    // render with their literal source text (empty), the title
    // attribute should never appear, the click handler should be
    // inert.
    let host = mount(
        r#"<adt-host>
              <span class="adt-text" pp-text="message"></span>
              <span class="adt-bound-attr" :title="some_title">x</span>
              <button class="adt-button" @click="handler">b</button>
           </adt-host>"#,
    );
    tick().await;

    let text_el = host.query_selector(".adt-text").unwrap().unwrap();
    let attr_el = host.query_selector(".adt-bound-attr").unwrap().unwrap();
    let button = host.query_selector(".adt-button").unwrap().unwrap();

    // pp-text never wrote anything — the element stays empty
    // (the literal pp-text attr is ignored by the browser).
    assert_eq!(
        text_el.text_content().as_deref(),
        Some(""),
        "pp-text on adopted DOM must not bind — span stays empty",
    );
    // The literal `pp-text` attribute is still present (the
    // bridge doesn't strip it; only the macro does, and the
    // macro never saw this element).
    assert_eq!(
        text_el.get_attribute("pp-text").as_deref(),
        Some("message"),
        "the literal pp-text attribute remains on adopted DOM (proof the bridge never processed it)",
    );

    // :title shorthand isn't expanded — the `:title` attribute
    // is still literal and `title` is unset.
    assert_eq!(
        attr_el.get_attribute("title"),
        None,
        ":title shorthand must not bind — the title HTML attr is unset",
    );
    assert!(
        attr_el.get_attribute(":title").is_some(),
        "the literal `:title` attribute remains on adopted DOM",
    );

    // @click handler is inert. Clicking the button must not
    // throw and must not invoke any handler — the framework has
    // no handler registered. We can only verify the negative by
    // confirming the handler attribute is still literal.
    assert_eq!(
        button.get_attribute("@click").as_deref(),
        Some("handler"),
        "@click shorthand must not bind on adopted DOM",
    );
    let html_button: HtmlElement = button.dyn_into().unwrap();
    html_button.click(); // must not panic, must not invoke anything

    host.remove();
}

/// **Bridge contract — case 4 (positive comparison to case 3).**
///
/// The same authoring shape as case 3, but inside a
/// `#[component(template_inline = ...)]` wrapper, **does** bind.
/// The macro processes `pp-text` / `@click` at compile time;
/// the runtime applies them via the compiled plan. No bridge
/// involvement.
///
/// Together with case 3, this locks the architectural rule:
/// directive binding requires a `#[component]` boundary, the
/// only escape hatch is the compiled plan emitter.
#[wasm_bindgen_test]
async fn case4_pp_text_in_compiled_wrapper_does_bind() {
    let host = mount("<adt-compiled-wrapper></adt-compiled-wrapper>");
    tick().await;

    // pp-text bound — the wrapper's `on_setup` set message="compiled".
    let text_el = host.query_selector(".adt-bound-text").unwrap().unwrap();
    assert_eq!(
        text_el.text_content().as_deref(),
        Some("compiled"),
        "pp-text inside #[component] binds against the wrapper's scope",
    );

    // @click bound — clicking increments the wrapper's count,
    // which the {{count}} interpolation reflects.
    let button: HtmlElement = host
        .query_selector(".adt-bound-click")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let initial = button.text_content().unwrap_or_default();
    assert_eq!(initial, "0", "initial count is 0");
    button.click();
    tick().await;
    assert_eq!(
        button.text_content().as_deref(),
        Some("1"),
        "@click handler bumped count via the compiled binding",
    );

    host.remove();
}
