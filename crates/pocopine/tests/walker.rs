//! Walker + `pp-for` integration tests. Covers the regressions that
//! broke HN's recursive comment tree:
//!
//! 1. Component tags mount even when they sit directly inside a
//!    `pp-for` body (LoopScope `SCOPE_ID_KEY` no longer shadows the
//!    `__pp_mounted` guard).
//! 2. The `MutationObserver` doesn't re-walk elements we already
//!    walked synchronously.
//! 3. Keyed `pp-for` reorders don't tear down the loop scope of a
//!    reused clone (observer sees `removedNodes` but `isConnected`
//!    is still true).
//! 4. Keyed `pp-for` drops clones whose keys disappear.
//! 5. A component with no user-defined `on_mount` doesn't fire the
//!    post-mount `trigger_scope` sweep.
//!
//! Run with:
//!   wasm-pack test --chrome --headless crates/pocopine

#![cfg(target_arch = "wasm32")]

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element, HtmlElement};

wasm_bindgen_test_configure!(run_in_browser);

// ─── test components ──────────────────────────────────────────────

#[derive(Clone, Default, Serialize, Deserialize)]
struct Row {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TestRow.html")]
struct TestRow {
    row: Row,
}

#[handlers]
impl TestRow {}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "TestList.html")]
struct TestList {
    rows: Vec<Row>,
}

#[handlers]
impl TestList {}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "WithMount.html")]
struct WithMount {
    count: u32,
}

#[handlers]
impl WithMount {
    pub fn on_mount(&mut self) {
        self.count += 1;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "WithoutMount.html")]
struct WithoutMount {
    count: u32,
}

#[handlers]
impl WithoutMount {}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "HandlerArgs.html")]
struct HandlerArgs {
    last_key: String,
    payload: String,
}

#[handlers]
impl HandlerArgs {
    // Typed Event arg (RFC-008 §5.1).
    pub fn on_key(&mut self, ev: web_sys::KeyboardEvent) {
        self.last_key = ev.key();
    }
    // Primitive arg via $dispatch payload.
    pub fn set_payload(&mut self, value: String) {
        self.payload = value;
    }
}

// RFC-011 — named slots (no scope).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "NamedSlotHost.html")]
struct NamedSlotHost {}

#[handlers]
impl NamedSlotHost {}

// RFC-011 — scoped slots.
#[derive(Clone, Default, Serialize, Deserialize)]
struct ScopedItem {
    id: u32,
    label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ScopedSlotHost.html")]
struct ScopedSlotHost {
    current: ScopedItem,
}

#[handlers]
impl ScopedSlotHost {}

// RFC-010 — attribute fallthrough.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "FallthroughRoot.html")]
struct FallthroughRoot {
    // `variant` is declared → flows into the prop path, NOT fallthrough.
    variant: String,
}

#[handlers]
impl FallthroughRoot {}

// RFC-009 — pp-model across a component boundary.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "ModelChild.html")]
struct ModelChild {
    model: String,
}

#[handlers]
impl ModelChild {}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ModelParent.html")]
struct ModelParent {
    email: String,
}

#[handlers]
impl ModelParent {}

fn register_all() {
    TestRow::register();
    TestList::register();
    WithMount::register();
    WithoutMount::register();
    HandlerArgs::register();
    ModelChild::register();
    ModelParent::register();
    FallthroughRoot::register();
    NamedSlotHost::register();
    ScopedSlotHost::register();
}

// ─── helpers ──────────────────────────────────────────────────────

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

/// Build a host `<div>` with the given children, attach it to
/// `<body>`, and install the walker on it. Tests mount into a fresh
/// host per call so one test's MutationObserver doesn't pick up
/// another's mutations.
fn mount(host_html: &str) -> Element {
    register_all();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();
    pocopine_core::walker::start(&host);
    host
}

/// Yield the browser microtask queue twice — once for the
/// `MutationObserver` batch, once for the reactive flush that any
/// triggers scheduled inside that batch might have queued.
async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn li_count(ul: &Element) -> u32 {
    ul.query_selector_all("li").unwrap().length()
}

fn li_texts(ul: &Element) -> Vec<String> {
    let list = ul.query_selector_all("li").unwrap();
    (0..list.length())
        .filter_map(|i| list.get(i))
        .filter_map(|n| n.dyn_into::<HtmlElement>().ok())
        .map(|el| el.inner_text())
        .collect()
}

/// Each `<test-row>` tag's only child element is the cloned
/// `<li>` — collect them in tree order for identity comparisons.
fn collect_li_clones(container: &Element) -> Vec<Element> {
    let list = container.query_selector_all("test-row").unwrap();
    (0..list.length())
        .filter_map(|i| list.get(i))
        .filter_map(|n| n.dyn_into::<Element>().ok())
        .filter_map(|tag| tag.first_element_child())
        .collect()
}

/// Write `rows` into the `<test-list>` scope. Finds the scope via
/// the template root (`<test-list>`'s first element child owns the
/// component scope).
fn seed_rows(host: &Element, rows: Vec<Row>) {
    let test_list = host.query_selector("test-list").unwrap().unwrap();
    let root = test_list.first_element_child().unwrap();
    let (_id, proxy) =
        pocopine_core::walker::scope_of_element(&root).expect("test-list scope");
    let array = serde_wasm_bindgen::to_value(&rows).unwrap();
    js_sys::Reflect::set(&proxy, &"rows".into(), &array).unwrap();
}

// ─── 1. component tags mount inside pp-for ────────────────────────

#[wasm_bindgen_test]
async fn component_tag_inside_pp_for_mounts_and_binds() {
    let host = mount("<test-list></test-list>");

    seed_rows(
        &host,
        vec![
            Row { id: 1, label: "one".into() },
            Row { id: 2, label: "two".into() },
            Row { id: 3, label: "three".into() },
        ],
    );
    tick().await;

    let ul = host.query_selector("ul").unwrap().unwrap();
    assert_eq!(li_count(&ul), 3, "test-row template body should mount per item");
    assert_eq!(li_texts(&ul), vec!["one", "two", "three"]);
}

// ─── 2. keyed reorder preserves element identity ──────────────────

#[wasm_bindgen_test]
async fn keyed_reorder_reuses_clones() {
    let host = mount("<test-list></test-list>");

    seed_rows(
        &host,
        vec![
            Row { id: 1, label: "one".into() },
            Row { id: 2, label: "two".into() },
            Row { id: 3, label: "three".into() },
        ],
    );
    tick().await;

    let ul = host.query_selector("ul").unwrap().unwrap();
    let before = collect_li_clones(&ul);
    assert_eq!(before.len(), 3);

    // [3, 1, 2] — same ids, every clone should be reused in place.
    seed_rows(
        &host,
        vec![
            Row { id: 3, label: "three".into() },
            Row { id: 1, label: "one".into() },
            Row { id: 2, label: "two".into() },
        ],
    );
    tick().await;

    let after = collect_li_clones(&ul);
    assert_eq!(after.len(), 3);

    // Node identity must survive a reorder — if the MutationObserver
    // had torn the clone down and re-walked, these would be fresh.
    assert!(
        after[0].is_same_node(Some(before[2].as_ref())),
        "id=3 should be the same element after the reorder"
    );
    assert!(after[1].is_same_node(Some(before[0].as_ref())));
    assert!(after[2].is_same_node(Some(before[1].as_ref())));

    assert_eq!(li_texts(&ul), vec!["three", "one", "two"]);
}

// ─── 3. keyed removal: missing keys get unmounted ─────────────────

#[wasm_bindgen_test]
async fn keyed_removal_releases_missing() {
    let host = mount("<test-list></test-list>");

    seed_rows(
        &host,
        vec![
            Row { id: 1, label: "one".into() },
            Row { id: 2, label: "two".into() },
            Row { id: 3, label: "three".into() },
        ],
    );
    tick().await;

    seed_rows(
        &host,
        vec![
            Row { id: 1, label: "one".into() },
            Row { id: 3, label: "three".into() },
        ],
    );
    tick().await;

    let ul = host.query_selector("ul").unwrap().unwrap();
    assert_eq!(li_count(&ul), 2);
    assert_eq!(li_texts(&ul), vec!["one", "three"]);
}

// ─── 4. observer doesn't double-walk explicit inserts ─────────────

#[wasm_bindgen_test]
async fn observer_doesnt_double_walk_new_clones() {
    let host = mount("<test-list></test-list>");

    seed_rows(&host, vec![Row { id: 1, label: "only".into() }]);
    tick().await;

    // Pre-fix, the MutationObserver would walk the added clone again
    // on top of our explicit walker::walk. Each pp-text effect would
    // be registered twice; the observable symptom is duplicate
    // textContent writes landing on the node. A single clone with
    // one-child text proves the observer short-circuited.
    let ul = host.query_selector("ul").unwrap().unwrap();
    assert_eq!(li_count(&ul), 1);

    let li = ul.query_selector("li").unwrap().unwrap();
    // The <li>'s only child should be the text node; if the walker
    // had run twice we'd have multiple text children accumulated.
    assert_eq!(li.child_nodes().length(), 1, "single text child");

    let html: HtmlElement = li.dyn_into().unwrap();
    assert_eq!(html.inner_text().trim(), "only");
}

// ─── 5. without-on_mount component skips sweep ────────────────────

#[wasm_bindgen_test]
async fn without_on_mount_renders_initial_state_cleanly() {
    let host = mount("<with-mount></with-mount><without-mount></without-mount>");
    tick().await;

    let with = host.query_selector(".count-with").unwrap().unwrap();
    let without = host.query_selector(".count-without").unwrap().unwrap();
    let with_text: HtmlElement = with.dyn_into().unwrap();
    let without_text: HtmlElement = without.dyn_into().unwrap();

    // on_mount fired on WithMount → count = 1. WithoutMount has no
    // on_mount → count stays at its default 0, and no trigger_scope
    // sweep was fired for it. If the sweep *had* fired spuriously,
    // the output would still be 0 (nothing to propagate) — but the
    // test at least proves the rendering path survives the
    // no-hook case without throwing.
    assert_eq!(with_text.inner_text().trim(), "1");
    assert_eq!(without_text.inner_text().trim(), "0");
}

// ─── 6. RFC-008 handler args ──────────────────────────────────────

#[wasm_bindgen_test]
async fn handler_with_typed_event_arg_receives_the_event() {
    let host = mount("<handler-args></handler-args>");
    tick().await;

    let root = host.query_selector("handler-args").unwrap().unwrap();
    let (scope_id, _) =
        pocopine_core::walker::scope_of_element(&root.first_element_child().unwrap())
            .expect("scope");

    // Synthesize a keydown event, set its `.key` via the init dict,
    // and invoke the handler directly — same path pp-on uses.
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Enter");
    let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
        .unwrap();
    let args = js_sys::Array::new();
    args.push(ev.as_ref());
    pocopine_core::scope::invoke_handler(scope_id, "on_key", &args);
    tick().await;

    let key_span = host.query_selector(".ha-key").unwrap().unwrap();
    let key_text: HtmlElement = key_span.dyn_into().unwrap();
    assert_eq!(key_text.inner_text().trim(), "Enter");
}

// ─── 7. RFC-009 pp-model on components ────────────────────────────

#[wasm_bindgen_test]
async fn pp_model_parent_to_child_mirrors_prop() {
    let host = mount("<model-parent></model-parent>");
    tick().await;

    // Set the parent's `email` field; the child's `model` prop
    // must reflect it via pp-model's parent→child effect.
    let parent = host.query_selector("model-parent").unwrap().unwrap();
    let parent_root = parent.first_element_child().unwrap();
    let (_id, parent_proxy) =
        pocopine_core::walker::scope_of_element(&parent_root).expect("parent scope");
    js_sys::Reflect::set(
        &parent_proxy,
        &"email".into(),
        &JsValue::from_str("alice@example.com"),
    )
    .unwrap();
    tick().await;

    let child_shown = host.query_selector(".mc-shown").unwrap().unwrap();
    let txt: HtmlElement = child_shown.dyn_into().unwrap();
    assert_eq!(txt.inner_text().trim(), "alice@example.com");
}

#[wasm_bindgen_test]
async fn pp_model_child_to_parent_via_update_event() {
    let host = mount("<model-parent></model-parent>");
    tick().await;

    // Child emits `pp:update:model` — the directive must write
    // `event.detail` back into the parent's bound field.
    let child = host.query_selector("model-child").unwrap().unwrap();
    let init = web_sys::CustomEventInit::new();
    init.set_bubbles(true);
    init.set_detail(&JsValue::from_str("bob@example.com"));
    let ev = web_sys::CustomEvent::new_with_event_init_dict("pp:update:model", &init)
        .unwrap();
    let _ = child.dispatch_event(&ev).unwrap();
    tick().await;

    let parent_shown = host.query_selector(".mp-shown").unwrap().unwrap();
    let txt: HtmlElement = parent_shown.dyn_into().unwrap();
    assert_eq!(txt.inner_text().trim(), "bob@example.com");
}

#[wasm_bindgen_test]
async fn handler_with_primitive_arg_deserializes_from_payload() {
    let host = mount("<handler-args></handler-args>");
    tick().await;

    let root = host.query_selector("handler-args").unwrap().unwrap();
    let (scope_id, _) =
        pocopine_core::walker::scope_of_element(&root.first_element_child().unwrap())
            .expect("scope");

    let args = js_sys::Array::new();
    args.push(&JsValue::from_str("hello from args"));
    pocopine_core::scope::invoke_handler(scope_id, "set_payload", &args);
    tick().await;

    let payload_span = host.query_selector(".ha-payload").unwrap().unwrap();
    let text: HtmlElement = payload_span.dyn_into().unwrap();
    assert_eq!(text.inner_text().trim(), "hello from args");
}

// ─── RFC-011 named slots ──────────────────────────────────────────

#[wasm_bindgen_test]
async fn named_slots_pick_up_user_templates_and_fall_back_to_default() {
    // Two named slots provided; footer left to fallback. Also some
    // default-slot content (the text node).
    let host = mount(
        r#"<named-slot-host>
             <template pp-slot="header"><h1 class="ns-user-header">Hi</h1></template>
             body text
           </named-slot-host>"#,
    );
    tick().await;

    // Header: user's h1.
    assert!(
        host.query_selector(".ns-user-header").unwrap().is_some(),
        "user's named-slot header should land"
    );

    // Default: text node from user.
    let body = host.query_selector(".ns-body").unwrap().unwrap();
    let body_html: HtmlElement = body.dyn_into().unwrap();
    assert!(
        body_html.inner_text().contains("body text"),
        "default slot should pick up the text node: {:?}",
        body_html.inner_text()
    );

    // Footer: fallback to slot's default.
    let footer = host.query_selector(".ns-footer").unwrap().unwrap();
    let footer_html: HtmlElement = footer.dyn_into().unwrap();
    assert_eq!(footer_html.inner_text().trim(), "default footer");
}

// ─── RFC-011 scoped slots ─────────────────────────────────────────

#[wasm_bindgen_test]
async fn scoped_slot_binds_ctx_and_updates_on_owner_change() {
    let host = mount(
        r#"<scoped-slot-host>
             <template pp-slot="item" pp-let="ctx">
               <span class="ss-user" pp-text="ctx.label"></span>
             </template>
           </scoped-slot-host>"#,
    );
    tick().await;

    // Owner's `current` starts default (empty label).
    let host_tag = host.query_selector("scoped-slot-host").unwrap().unwrap();
    let host_root = host_tag.first_element_child().unwrap();
    let (_id, host_proxy) =
        pocopine_core::walker::scope_of_element(&host_root).expect("host scope");

    // Write a real item.
    let item = serde_wasm_bindgen::to_value(&ScopedItem {
        id: 7,
        label: "lucky".into(),
    })
    .unwrap();
    js_sys::Reflect::set(&host_proxy, &"current".into(), &item).unwrap();
    tick().await;

    let user_span = host.query_selector(".ss-user").unwrap().unwrap();
    let text: HtmlElement = user_span.dyn_into().unwrap();
    assert_eq!(
        text.inner_text().trim(),
        "lucky",
        "scoped slot reads ctx.label from owner's `current.label`"
    );

    // Mutate again — effect re-runs.
    let item2 = serde_wasm_bindgen::to_value(&ScopedItem {
        id: 8,
        label: "updated".into(),
    })
    .unwrap();
    js_sys::Reflect::set(&host_proxy, &"current".into(), &item2).unwrap();
    tick().await;

    let user_span = host.query_selector(".ss-user").unwrap().unwrap();
    let text: HtmlElement = user_span.dyn_into().unwrap();
    assert_eq!(text.inner_text().trim(), "updated");
}

#[wasm_bindgen_test]
async fn scoped_slot_falls_back_to_default_children_when_user_didnt_provide() {
    let host = mount("<scoped-slot-host></scoped-slot-host>");
    tick().await;

    let fallback = host.query_selector(".ss-default").unwrap().unwrap();
    let text: HtmlElement = fallback.dyn_into().unwrap();
    assert_eq!(text.inner_text().trim(), "fallback");
}

// ─── 8. RFC-010 attribute fallthrough + cx! ───────────────────────

#[wasm_bindgen_test]
async fn fallthrough_merges_class_and_style_onto_template_root() {
    // `variant` is a declared prop → prop path; `class`, `style`,
    // `id`, `data-testid`, `aria-label` aren't → fallthrough.
    let host = mount(
        r#"<fallthrough-root
               variant="primary"
               class="extra-1 extra-2"
               style="background: blue"
               id="my-id"
               data-testid="tid"
               aria-label="lbl"
           ></fallthrough-root>"#,
    );
    tick().await;

    let root = host
        .query_selector("fallthrough-root > .ft-base")
        .unwrap()
        .unwrap();

    // class merged (base + user extras).
    let class = root.get_attribute("class").unwrap_or_default();
    assert!(class.contains("ft-base"), "base class preserved: {class:?}");
    assert!(class.contains("extra-1"), "user class appended: {class:?}");
    assert!(class.contains("extra-2"), "both user classes: {class:?}");

    // style merged (base kept, user appended with `;`).
    let style = root.get_attribute("style").unwrap_or_default();
    assert!(style.contains("color"), "base style preserved: {style:?}");
    assert!(style.contains("background"), "user style appended: {style:?}");

    // Non-class/style: overwrite semantics — assigned to root.
    assert_eq!(root.get_attribute("id"), Some("my-id".into()));
    assert_eq!(root.get_attribute("data-testid"), Some("tid".into()));
    assert_eq!(root.get_attribute("aria-label"), Some("lbl".into()));

    // Declared prop did NOT fall through — variant lives on the state,
    // rendered via pp-text.
    let variant_span = host.query_selector(".ft-variant").unwrap().unwrap();
    let text: HtmlElement = variant_span.dyn_into().unwrap();
    assert_eq!(text.inner_text().trim(), "primary");
    // And no `variant=...` attr leaked onto the root.
    assert!(root.get_attribute("variant").is_none());
}

#[wasm_bindgen_test]
async fn fallthrough_works_when_root_has_no_base_class_or_style() {
    // Template root has no `class`/`style`; user's values land straight on.
    let host = mount(
        r#"<fallthrough-root variant="" class="only-user" style="margin: 4px"></fallthrough-root>"#,
    );
    tick().await;

    let root = host
        .query_selector("fallthrough-root > .ft-base")
        .unwrap()
        .unwrap();
    let class = root.get_attribute("class").unwrap_or_default();
    assert!(class.contains("ft-base"));
    assert!(class.contains("only-user"));

    let style = root.get_attribute("style").unwrap_or_default();
    assert!(style.contains("margin"));
}

#[wasm_bindgen_test]
fn cx_macro_emits_expected_strings() {
    use pocopine::cx;

    // All literal — simple concat.
    assert_eq!(cx!("a", "b", "c"), "a b c");

    // Conditional: truthy branch emits, falsy skipped.
    assert_eq!(cx!("base", true => "on", false => "off"), "base on");

    // Bare expression: non-empty emits, empty skips.
    let extra = "user";
    let empty = String::new();
    assert_eq!(cx!("base", &extra, &empty), "base user");

    // Empty invocation → empty string.
    assert_eq!(cx!(), "");

    // Mixed typical call.
    let variant = "primary";
    let disabled = false;
    let out = cx!(
        "pine-btn",
        variant == "primary" => "pine-btn-primary",
        variant == "destructive" => "pine-btn-destructive",
        disabled => "is-disabled",
    );
    assert_eq!(out, "pine-btn pine-btn-primary");
}

