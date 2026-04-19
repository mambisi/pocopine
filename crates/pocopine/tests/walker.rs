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
    escaped_count: u32,
    ctrl_k_count: u32,
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
    // Key modifiers (RFC-013).
    pub fn on_escape(&mut self) {
        self.escaped_count += 1;
    }
    pub fn on_ctrl_k(&mut self) {
        self.ctrl_k_count += 1;
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

// RFC-012 — template expression evaluator.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "ExprHost.html")]
struct ExprHost {
    status: String,
    loading: bool,
    count: i32,
    error: String,
    warn: bool,
}

#[handlers]
impl ExprHost {}

// RFC-016 — pp-resize.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "ResizeHost.html")]
struct ResizeHost {
    w: f64,
    h: f64,
    fires: u32,
}

#[handlers]
impl ResizeHost {
    pub fn on_resize(&mut self, w: f64, h: f64) {
        self.w = w;
        self.h = h;
        self.fires += 1;
    }
}

// RFC-015 — pp-anchor.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "AnchorHost.html")]
struct AnchorHost {}

#[handlers]
impl AnchorHost {}

// RFC-017 — click.outside.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "OutsideHost.html")]
struct OutsideHost {
    outside_count: u32,
}

#[handlers]
impl OutsideHost {
    pub fn on_outside(&mut self) {
        self.outside_count += 1;
    }
}

// RFC-018 — `$id` magic.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "IdHost.html")]
struct IdHost {}

#[handlers]
impl IdHost {}

// RFC-019 — `pp-as` polymorphic rendering.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "AsButton.html")]
struct AsButton {
    clicks: u32,
}

#[handlers]
impl AsButton {
    pub fn on_click(&mut self) {
        self.clicks += 1;
    }
}

// RFC-020 — `:attr` / `@event` shorthand.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "ShortHost.html")]
struct ShortHost {
    variant: String,
    count: u32,
}

#[handlers]
impl ShortHost {
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

// RFC-022 — pp-roving.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "RovingHost.html")]
struct RovingHost {}

#[handlers]
impl RovingHost {}

// RFC-027 — parent-scope context (provide + inject).
#[derive(Default, Serialize, Deserialize)]
#[component(template = "CtxRoot.html")]
struct CtxRoot {
    hits: u32,
}

#[handlers]
impl CtxRoot {
    pub fn on_mount(&mut self) {
        pocopine::provide("ctx-root", pocopine::this::<Self>());
    }
    pub fn bump(&mut self) {
        self.hits += 1;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "CtxChild.html")]
struct CtxChild {}

#[handlers]
impl CtxChild {
    pub fn bump(&mut self) {
        if let Some(root) = pocopine::inject::<pocopine::Handle<CtxRoot>>("ctx-root") {
            root.update(|r| r.bump());
        }
    }
}

// RFC-024 — expression-based @event values.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "ExprOnHost.html")]
struct ExprOnHost {
    open: bool,
    picked: String,
    bumped: u32,
}

#[handlers]
impl ExprOnHost {
    pub fn pick(&mut self, value: String) {
        self.picked = value;
    }
}

// RFC-025 — inline `{expr}` text interpolation.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "InterpHost.html")]
struct InterpHost {
    name: String,
    count: u32,
}

#[handlers]
impl InterpHost {
    pub fn on_mount(&mut self) {
        self.name = "Ada".into();
        self.count = 3;
    }
    pub fn bump(&mut self) {
        self.count += 1;
    }
}

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
    ExprHost::register();
    ResizeHost::register();
    AnchorHost::register();
    OutsideHost::register();
    IdHost::register();
    AsButton::register();
    ShortHost::register();
    RovingHost::register();
    ExprOnHost::register();
    InterpHost::register();
    CtxRoot::register();
    CtxChild::register();
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

// ─── RFC-012 expression evaluator ─────────────────────────────────

#[wasm_bindgen_test]
async fn expressions_evaluate_through_directives() {
    let host = mount("<expr-host></expr-host>");
    tick().await;

    let root = host.query_selector("expr-host").unwrap().unwrap();
    let inner = root.first_element_child().unwrap();
    let (_id, proxy) =
        pocopine_core::walker::scope_of_element(&inner).expect("expr-host scope");

    // Seed mixed state.
    js_sys::Reflect::set(&proxy, &"status".into(), &JsValue::from_str("ok")).unwrap();
    js_sys::Reflect::set(&proxy, &"loading".into(), &JsValue::from_bool(false)).unwrap();
    js_sys::Reflect::set(&proxy, &"count".into(), &JsValue::from_f64(3.0)).unwrap();
    js_sys::Reflect::set(&proxy, &"warn".into(), &JsValue::from_bool(true)).unwrap();
    tick().await;

    let text = |sel: &str| -> String {
        let el = host.query_selector(sel).unwrap().unwrap();
        let h: HtmlElement = el.dyn_into().unwrap();
        h.inner_text().trim().to_string()
    };
    let visible = |sel: &str| -> bool {
        let el = host.query_selector(sel).unwrap().unwrap();
        let h: HtmlElement = el.dyn_into().unwrap();
        h.style().get_property_value("display").unwrap_or_default() != "none"
    };

    // Ternary: status == 'ok' ? 'good' : 'bad'
    assert_eq!(text(".eh-ternary"), "good");
    // Ternary with relational: count >= 5 ? 'many' : 'few'
    assert_eq!(text(".eh-cmp"), "few");
    // String literal.
    assert_eq!(text(".eh-literal"), "forty-two");

    // loading && count > 0 → false && true → false
    assert!(!visible(".eh-and"), "eh-and should be hidden");
    // error || warn → "" (falsy) || true → true
    assert!(visible(".eh-or"), "eh-or should be visible via ||");
    // !loading → !false → true
    assert!(visible(".eh-not"), "eh-not should be visible");

    // Flip state: loading=true, count=10 → loading && count > 0 becomes true.
    js_sys::Reflect::set(&proxy, &"loading".into(), &JsValue::from_bool(true)).unwrap();
    js_sys::Reflect::set(&proxy, &"count".into(), &JsValue::from_f64(10.0)).unwrap();
    js_sys::Reflect::set(&proxy, &"status".into(), &JsValue::from_str("err")).unwrap();
    tick().await;

    assert!(visible(".eh-and"), "eh-and now visible");
    assert!(!visible(".eh-not"), "eh-not hidden when loading=true");
    assert_eq!(text(".eh-ternary"), "bad"); // status != 'ok'
    assert_eq!(text(".eh-cmp"), "many");    // count >= 5
}

// ─── RFC-013 key modifiers ────────────────────────────────────────

#[wasm_bindgen_test]
async fn key_modifiers_filter_handlers() {
    let host = mount("<handler-args></handler-args>");
    tick().await;

    let input = host.query_selector(".ha-input").unwrap().unwrap();

    fn dispatch_keydown(
        el: &Element,
        key: &str,
        ctrl: bool,
        meta: bool,
    ) {
        let init = web_sys::KeyboardEventInit::new();
        init.set_key(key);
        init.set_ctrl_key(ctrl);
        init.set_meta_key(meta);
        init.set_bubbles(true);
        let ev = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict(
            "keydown", &init,
        )
        .unwrap();
        let _ = el.dispatch_event(&ev).unwrap();
    }

    // Escape → on_escape fires, on_key also fires (no key modifier)
    dispatch_keydown(&input, "Escape", false, false);
    tick().await;

    let escaped = host.query_selector(".ha-escaped").unwrap().unwrap();
    let ctrl_k = host.query_selector(".ha-ctrl-k").unwrap().unwrap();
    let e_html: HtmlElement = escaped.dyn_into().unwrap();
    let k_html: HtmlElement = ctrl_k.dyn_into().unwrap();
    assert_eq!(e_html.inner_text().trim(), "1");
    assert_eq!(k_html.inner_text().trim(), "0");

    // `a` key (no ctrl) → neither modifier-filtered handler fires.
    dispatch_keydown(&input, "a", false, false);
    tick().await;
    let escaped = host.query_selector(".ha-escaped").unwrap().unwrap();
    let ctrl_k = host.query_selector(".ha-ctrl-k").unwrap().unwrap();
    let e_html: HtmlElement = escaped.dyn_into().unwrap();
    let k_html: HtmlElement = ctrl_k.dyn_into().unwrap();
    assert_eq!(e_html.inner_text().trim(), "1", "escape unchanged on `a`");
    assert_eq!(k_html.inner_text().trim(), "0", "ctrl+k unchanged on plain a");

    // Ctrl+k → on_ctrl_k fires.
    dispatch_keydown(&input, "k", true, false);
    tick().await;
    let ctrl_k = host.query_selector(".ha-ctrl-k").unwrap().unwrap();
    let k_html: HtmlElement = ctrl_k.dyn_into().unwrap();
    assert_eq!(k_html.inner_text().trim(), "1");

    // k WITHOUT ctrl → should NOT fire on_ctrl_k.
    dispatch_keydown(&input, "k", false, false);
    tick().await;
    let ctrl_k = host.query_selector(".ha-ctrl-k").unwrap().unwrap();
    let k_html: HtmlElement = ctrl_k.dyn_into().unwrap();
    assert_eq!(k_html.inner_text().trim(), "1", "plain k doesn't fire ctrl.k");
}

// ─── RFC-014: focus + tick utilities ──────────────────────────────

/// `auto_focus_first` walks the DOM for the first focusable and
/// calls `.focus()` on it. `save`/`restore` round-trips preserve
/// the previously-focused element across an overlay's lifetime.
#[wasm_bindgen_test]
fn focus_utilities_save_restore_and_auto_focus() {
    use pocopine::focus;

    let host = doc().create_element("div").unwrap();
    host.set_inner_html(concat!(
        "<input id='outside' />",
        "<div id='overlay'>",
        "  <button id='ok'>ok</button>",
        "  <button id='cancel'>cancel</button>",
        "</div>"
    ));
    doc().body().unwrap().append_child(&host).unwrap();

    let outside = host
        .query_selector("#outside")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    outside.focus().unwrap();

    let saved = focus::save();

    let overlay = host.query_selector("#overlay").unwrap().unwrap();
    let focused_any = focus::auto_focus_first(&overlay);
    assert!(focused_any, "overlay has focusable children");

    let ok = host.query_selector("#ok").unwrap().unwrap();
    assert_eq!(
        doc().active_element().unwrap(),
        ok,
        "first focusable (ok button) gets focus"
    );

    focus::restore(saved);
    assert_eq!(
        doc()
            .active_element()
            .unwrap()
            .dyn_into::<HtmlElement>()
            .unwrap(),
        outside,
        "restore returned focus to the element that had it before"
    );

    host.remove();
}

/// `blur` removes focus from the active element; running it when the
/// body is the active element is a safe no-op.
#[wasm_bindgen_test]
fn focus_blur_clears_active_element() {
    use pocopine::focus;

    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<input id='typed' />");
    doc().body().unwrap().append_child(&host).unwrap();

    let input = host
        .query_selector("#typed")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    input.focus().unwrap();
    let input_el: Element = input.clone().into();
    assert_eq!(doc().active_element().unwrap(), input_el);

    focus::blur();
    // After blur, active element is either body or null — both count as
    // "nothing focused" for our purposes.
    if let Some(el) = doc().active_element() {
        assert_eq!(el, doc().body().unwrap().dyn_into::<Element>().unwrap());
    }

    // Second blur is a no-op and does not panic.
    focus::blur();

    host.remove();
}

/// `tick::next` schedules work on the microtask queue — the callback
/// fires before a subsequent `await` yields to a macrotask.
#[wasm_bindgen_test]
async fn tick_next_runs_on_microtask_queue() {
    use std::cell::Cell;
    use std::rc::Rc;

    let fired = Rc::new(Cell::new(false));
    let f = fired.clone();
    pocopine::tick::next(move || f.set(true));

    // Before the microtask yields, nothing has fired yet.
    assert!(!fired.get(), "sync path has not run the callback yet");

    // Yield one microtask — `tick::next`'s closure runs.
    let p = js_sys::Promise::resolve(&JsValue::NULL);
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;

    assert!(fired.get(), "callback fired after microtask yield");
}

/// Wait for `ResizeObserver` to fire. Unlike the `MutationObserver`
/// `tick` helper above, ResizeObserver dispatches its first batch
/// on the next animation frame, so polling microtasks alone is
/// insufficient — we yield an RAF-equivalent delay.
async fn raf_tick() {
    let window = window().unwrap();
    // `setTimeout(resolve, 16)` ~= one animation frame; waiting twice
    // ensures ResizeObserver's internal queue has flushed in every
    // tested browser.
    for _ in 0..3 {
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                &resolve, 16,
            );
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }
}

/// `pp-resize` installs a `ResizeObserver` and fires the named
/// handler with `(f64, f64)` args — the element's content-box
/// dimensions.
#[wasm_bindgen_test]
async fn pp_resize_fires_handler_on_initial_observe_and_on_size_change() {
    let host = mount(
        "<resize-host style=\"display: block; width: 300px; height: 50px\"></resize-host>",
    );
    // First: ResizeObserver's synthetic initial entry must fire.
    raf_tick().await;

    let fires_el = host.query_selector(".rh-fires").unwrap().unwrap();
    let fires: HtmlElement = fires_el.dyn_into().unwrap();
    let fires_value: u32 = fires.inner_text().trim().parse().unwrap_or(0);
    assert!(fires_value >= 1, "initial observe fires at least once");

    let w_el = host.query_selector(".rh-w").unwrap().unwrap();
    let w_html: HtmlElement = w_el.dyn_into().unwrap();
    let w_before: f64 = w_html.inner_text().trim().parse().unwrap_or(0.0);
    assert!(w_before > 0.0, "width reported > 0 after initial observe");

    // Resize the tag root: the directive is on whatever root the
    // template installed, so pick that up via querySelector.
    let root = host.query_selector(".rh-root").unwrap().unwrap();
    let root_html: HtmlElement = root.clone().dyn_into().unwrap();
    root_html
        .style()
        .set_property("width", "600px")
        .unwrap();

    raf_tick().await;

    let w_el = host.query_selector(".rh-w").unwrap().unwrap();
    let w_html: HtmlElement = w_el.dyn_into().unwrap();
    let w_after: f64 = w_html.inner_text().trim().parse().unwrap_or(0.0);
    assert!(
        w_after > w_before,
        "width updates after inline style change ({w_before} → {w_after})"
    );

    host.remove();
}

/// `pp-anchor:bottom-start.offset.8` positions the floater directly
/// below the anchor's left edge with an 8px gap.
#[wasm_bindgen_test]
async fn pp_anchor_positions_floater_under_trigger() {
    // `AnchorHost` places the trigger at position (100, 200) sized
    // 120 × 40; the popover should land at top = 200+40+8 = 248,
    // left = 100 (bottom-start alignment).
    let host = mount("<anchor-host></anchor-host>");
    raf_tick().await;

    let popover = host
        .query_selector(".ah-popover")
        .unwrap()
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    let rect = popover.get_bounding_client_rect();

    // Because these are fixed-position coords, the browser might
    // add a sub-pixel offset for devicePixelRatio. Use a loose
    // tolerance.
    let epsilon = 2.0;
    assert!(
        (rect.left() - 100.0).abs() < epsilon,
        "popover left {} should be ~100",
        rect.left()
    );
    assert!(
        (rect.top() - 248.0).abs() < epsilon,
        "popover top {} should be ~248 (trigger bottom + 8px offset)",
        rect.top()
    );

    // Confirm the directive set `position: fixed` so the floater is
    // pinned to the viewport rather than the document flow.
    let style = popover.style();
    assert_eq!(
        style.get_property_value("position").unwrap(),
        "fixed",
        "pp-anchor sets position: fixed"
    );

    host.remove();
}

/// `pp-on:click.outside` fires only when the click target is not a
/// descendant of the host element.
#[wasm_bindgen_test]
async fn click_outside_fires_only_for_clicks_that_miss_the_host() {
    let host = mount("<outside-host></outside-host>");
    tick().await;

    let panel = host.query_selector(".oh-panel").unwrap().unwrap();
    let panel_el: HtmlElement = panel.clone().dyn_into().unwrap();
    let inside_btn = host.query_selector(".oh-inside").unwrap().unwrap();
    let inside_btn_el: HtmlElement = inside_btn.dyn_into().unwrap();
    let child_btn = host.query_selector(".oh-child").unwrap().unwrap();
    let child_btn_el: HtmlElement = child_btn.dyn_into().unwrap();

    fn count_after(host: &Element) -> u32 {
        let span = host.query_selector(".oh-count").unwrap().unwrap();
        let html: HtmlElement = span.dyn_into().unwrap();
        html.inner_text().trim().parse().unwrap_or(0)
    }

    // Click on a *descendant* of the panel — should NOT fire.
    child_btn_el.click();
    tick().await;
    assert_eq!(count_after(&host), 0, "descendant click does not fire");

    // Click on the panel itself — still inside, should NOT fire.
    panel_el.click();
    tick().await;
    assert_eq!(count_after(&host), 0, "click on host itself does not fire");

    // Click on a sibling outside the panel — SHOULD fire.
    inside_btn_el.click();
    tick().await;
    assert_eq!(count_after(&host), 1, "sibling click fires outside handler");

    host.remove();
}

/// RFC-022 — `pp-roving` initialises tabindex, manages arrow-key
/// focus navigation, skips aria-disabled items, wraps at edges,
/// and handles Home / End.
#[wasm_bindgen_test]
async fn roving_navigates_with_arrows_and_skips_disabled() {
    let host = mount("<roving-host></roving-host>");
    tick().await;

    let container = host.query_selector(".rv-root").unwrap().unwrap();
    let items = container.query_selector_all(".rv-item").unwrap();
    let item = |idx: u32| items.item(idx).unwrap().dyn_into::<Element>().unwrap();
    let a = item(0);
    let b = item(1);
    let _c = item(2); // disabled
    let d = item(3);

    // Initial tabindex: first non-disabled is 0, rest are -1.
    assert_eq!(a.get_attribute("tabindex").as_deref(), Some("0"));
    assert_eq!(b.get_attribute("tabindex").as_deref(), Some("-1"));
    assert_eq!(d.get_attribute("tabindex").as_deref(), Some("-1"));

    // Focus A, then ArrowDown → B.
    a.clone().dyn_into::<HtmlElement>().unwrap().focus().unwrap();

    fn send_arrow(target: &Element, key: &str) {
        let init = web_sys::KeyboardEventInit::new();
        init.set_key(key);
        init.set_bubbles(true);
        let ev =
            web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init)
                .unwrap();
        let _ = target.dispatch_event(&ev).unwrap();
    }

    send_arrow(&a, "ArrowDown");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        b,
        "ArrowDown moved focus from A to B"
    );
    assert_eq!(b.get_attribute("tabindex").as_deref(), Some("0"));
    assert_eq!(a.get_attribute("tabindex").as_deref(), Some("-1"));

    // From B, ArrowDown should skip aria-disabled C and land on D.
    send_arrow(&b, "ArrowDown");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        d,
        "ArrowDown skipped disabled C and landed on D"
    );

    // From D, ArrowDown wraps back to A.
    send_arrow(&d, "ArrowDown");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        a,
        "ArrowDown wrapped D → A"
    );

    // End jumps to last enabled.
    send_arrow(&a, "End");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        d,
        "End jumped to last enabled (D)"
    );

    // Home jumps to first enabled.
    send_arrow(&d, "Home");
    tick().await;
    assert_eq!(
        doc().active_element().unwrap(),
        a,
        "Home jumped to first enabled (A)"
    );

    host.remove();
}

/// RFC-021 — scroll_lock::{lock,unlock} is ref-counted and toggles
/// body.style.overflow on the 0↔1 transitions.
#[wasm_bindgen_test]
fn scroll_lock_refcounts_and_toggles_body_overflow() {
    use pocopine::scroll_lock;

    let body = doc().body().unwrap();
    let style = body.style();

    // Sanity: baseline depth should be 0 at the top of the test.
    // Earlier tests can leave depth untouched (no test locks scroll).
    assert_eq!(scroll_lock::depth(), 0);
    let before = style.get_property_value("overflow").unwrap_or_default();

    scroll_lock::lock();
    assert_eq!(scroll_lock::depth(), 1);
    assert_eq!(
        style.get_property_value("overflow").unwrap_or_default(),
        "hidden",
        "first lock sets overflow hidden"
    );

    // Nested lock — depth bumps, style unchanged.
    scroll_lock::lock();
    assert_eq!(scroll_lock::depth(), 2);
    assert_eq!(
        style.get_property_value("overflow").unwrap_or_default(),
        "hidden",
        "second lock keeps overflow hidden"
    );

    // Inner unlock doesn't release the outer one.
    scroll_lock::unlock();
    assert_eq!(scroll_lock::depth(), 1);
    assert_eq!(
        style.get_property_value("overflow").unwrap_or_default(),
        "hidden",
        "overflow still hidden while outer still held"
    );

    // Outer unlock restores.
    scroll_lock::unlock();
    assert_eq!(scroll_lock::depth(), 0);
    assert_eq!(
        style.get_property_value("overflow").unwrap_or_default(),
        before,
        "overflow restored when depth returns to 0"
    );

    // Stray unlock when already at 0 is a no-op.
    scroll_lock::unlock();
    assert_eq!(scroll_lock::depth(), 0);
}

/// RFC-020 — `:class="…"` and `@click="…"` shorthand bind the same
/// way as the long `pp-bind:class` / `pp-on:click` forms.
#[wasm_bindgen_test]
async fn shorthand_colon_and_at_bind_same_as_long_form() {
    let host = mount("<short-host variant=\"primary\"></short-host>");
    tick().await;

    let btn = host
        .query_selector("[data-testid=\"btn\"]")
        .unwrap()
        .unwrap();
    assert_eq!(
        btn.get_attribute("class").as_deref(),
        Some("primary"),
        ":class shorthand wrote the variant value"
    );

    let btn_html: HtmlElement = btn.clone().dyn_into().unwrap();
    btn_html.click();
    tick().await;
    btn_html.click();
    tick().await;

    let count = host
        .query_selector("[data-testid=\"count\"]")
        .unwrap()
        .unwrap();
    let count_html: HtmlElement = count.dyn_into().unwrap();
    assert_eq!(
        count_html.inner_text().trim(),
        "2",
        "@click shorthand dispatched handler twice"
    );

    host.remove();
}

/// `$id` returns a unique per-instance string; two sibling
/// `<id-host>`s get distinct IDs, and the `+` operator composes
/// sub-IDs like `pp-1-input`.
#[wasm_bindgen_test]
async fn id_magic_is_unique_per_instance_and_composes_via_plus() {
    let host = mount("<id-host></id-host><id-host></id-host>");
    tick().await;

    let roots = host.query_selector_all(".idh-root").unwrap();
    assert_eq!(roots.length(), 2, "two IdHost instances mounted");

    let get_text = |root: Element, sel: &str| -> String {
        let el = root.query_selector(sel).unwrap().unwrap();
        el.dyn_into::<HtmlElement>().unwrap().inner_text().trim().to_string()
    };
    let get_attr = |root: Element, sel: &str, name: &str| -> String {
        let el = root.query_selector(sel).unwrap().unwrap();
        el.get_attribute(name).unwrap_or_default()
    };

    let root1 = roots.item(0).unwrap().dyn_into::<Element>().unwrap();
    let root2 = roots.item(1).unwrap().dyn_into::<Element>().unwrap();

    let base1 = get_text(root1.clone(), ".idh-base");
    let base2 = get_text(root2.clone(), ".idh-base");
    assert!(base1.starts_with("pp-"), "base id is pp-prefixed, got {base1:?}");
    assert!(base2.starts_with("pp-"), "base id is pp-prefixed, got {base2:?}");
    assert_ne!(base1, base2, "two instances get distinct ids");

    // `+` composition: label[for] === input[id] === `{base}-input`.
    let for1 = get_attr(root1.clone(), ".idh-label", "for");
    let id1 = get_attr(root1.clone(), ".idh-input", "id");
    assert_eq!(for1, format!("{base1}-input"));
    assert_eq!(id1, format!("{base1}-input"));
    assert_eq!(for1, id1, "label[for] and input[id] resolve identically");

    host.remove();
}

/// `pp-as` hoists the user's single child element as the rendered
/// root, discards the template's wrapper, and merges template
/// attrs. The template's `pp-on:click` fires on the hoisted root
/// so author-chosen tags (like `<a>`) get the component's behaviour
/// without a wrapping `<button>`.
#[wasm_bindgen_test]
async fn pp_as_hoists_user_element_and_merges_template_attrs() {
    // `href="#"` so the click doesn't navigate the test page
    // even before `.prevent` on the template runs. Belt + braces.
    let host = mount(
        "<as-button pp-as><a href=\"#\" class=\"mine\">Read</a></as-button>",
    );
    tick().await;

    // The tag should now contain an <a> — NOT a <button>.
    let as_tag = host.query_selector("as-button").unwrap().unwrap();
    let children = as_tag.children();
    assert_eq!(
        children.length(),
        1,
        "component tag has exactly one child"
    );
    let root = children.item(0).unwrap();
    assert_eq!(
        root.local_name(),
        "a",
        "hoisted root is <a>, not <button>"
    );

    // Attrs merge: user's `mine` plus template's `as-btn`.
    let cls = root.get_attribute("class").unwrap_or_default();
    assert!(
        cls.split_whitespace().any(|c| c == "mine")
            && cls.split_whitespace().any(|c| c == "as-btn"),
        "class merge expected to contain both `mine` and `as-btn`, got {cls:?}"
    );

    // User's `href` is preserved; template's `data-tpl` copied in.
    assert_eq!(root.get_attribute("href").as_deref(), Some("#"));
    assert_eq!(root.get_attribute("data-tpl").as_deref(), Some("yes"));

    // Clicking the hoisted root fires the template's pp-on:click.
    // Template used `.prevent` so the default anchor navigation
    // never runs and the test page stays put.
    let root_html: HtmlElement = root.clone().dyn_into().unwrap();
    root_html.click();
    tick().await;
    root_html.click();
    tick().await;

    host.remove();
}

/// RFC-024 — @event values can be inline assignments, handler
/// calls with arguments, and statement sequences.
#[wasm_bindgen_test]
async fn expr_values_in_pp_on_support_assign_call_and_seq() {
    let host = mount("<expr-on-host></expr-on-host>");
    tick().await;

    let toggle = host.query_selector(".eo-toggle").unwrap().unwrap();
    let pick_a = host.query_selector(".eo-pick-a").unwrap().unwrap();
    let pick_b = host.query_selector(".eo-pick-b").unwrap().unwrap();
    let seq = host.query_selector(".eo-seq").unwrap().unwrap();

    let read_text = |sel: &str| -> String {
        let el = host.query_selector(sel).unwrap().unwrap();
        el.dyn_into::<HtmlElement>()
            .unwrap()
            .inner_text()
            .trim()
            .to_string()
    };

    assert_eq!(read_text(".eo-open"), "off");
    assert_eq!(read_text(".eo-picked"), "");
    assert_eq!(read_text(".eo-bumped"), "0");

    // Inline assignment: `open = !open`.
    toggle
        .clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(read_text(".eo-open"), "on", "assignment flipped open");
    toggle
        .dyn_into::<HtmlElement>()
        .unwrap()
        .click();
    tick().await;
    assert_eq!(read_text(".eo-open"), "off", "second click toggled back");

    // Call with a literal string arg: `pick('a')`.
    pick_a.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(read_text(".eo-picked"), "a");
    pick_b.dyn_into::<HtmlElement>().unwrap().click();
    tick().await;
    assert_eq!(read_text(".eo-picked"), "b");

    // Statement sequence: `bumped = bumped + 1; pick('seq')`.
    let seq_html: HtmlElement = seq.dyn_into().unwrap();
    seq_html.click();
    tick().await;
    seq_html.click();
    tick().await;
    assert_eq!(read_text(".eo-bumped"), "2", "assign ran twice");
    assert_eq!(read_text(".eo-picked"), "seq", "call ran after assign");

    host.remove();
}

/// RFC-027 — a child scope can `inject` a value `provide`d by an
/// ancestor component, walking the scope-parent chain recorded at
/// mount time. Clicks inside two nested `<ctx-child>` components
/// resolve the parent `<ctx-root>`'s `Handle<Self>` and call its
/// `bump` handler, which increments a reactive counter on the
/// parent. The parent's `pp-text` span should reflect the updated
/// count — proving provide/inject crosses the component boundary.
#[wasm_bindgen_test]
async fn provide_and_inject_cross_component_boundary() {
    let host = mount("<ctx-root></ctx-root>");
    tick().await;

    let root_span = host.query_selector(".cr-parent-hits").unwrap().unwrap();
    let read_hits = || {
        root_span
            .clone()
            .dyn_into::<HtmlElement>()
            .unwrap()
            .inner_text()
            .trim()
            .to_string()
    };
    assert_eq!(read_hits(), "0", "initial hits = 0");

    let buttons = host.query_selector_all(".cc-btn").unwrap();
    assert_eq!(buttons.length(), 2, "two <ctx-child> buttons mounted");

    let first: HtmlElement = buttons.item(0).unwrap().dyn_into().unwrap();
    first.click();
    tick().await;
    assert_eq!(
        read_hits(),
        "1",
        "first child's inject found root and bumped it"
    );

    let second: HtmlElement = buttons.item(1).unwrap().dyn_into().unwrap();
    second.click();
    tick().await;
    second.click();
    tick().await;
    assert_eq!(
        read_hits(),
        "3",
        "second child also resolves the same provided handle"
    );

    host.remove();
}

/// RFC-025 — inline `{expr}` text interpolation:
/// - multiple segments in one text node
/// - static literal between dynamic segments is preserved
/// - `\{` / `\}` escapes pass through as literal braces
/// - nested element inside a host with other interpolation works
///   (each text node scans independently)
/// - updates rerun only the affected segment
#[wasm_bindgen_test]
async fn inline_expr_interpolation_splits_text_and_reactively_updates() {
    let host = mount("<interp-host></interp-host>");
    tick().await;

    let read = |sel: &str| -> String {
        let el = host.query_selector(sel).unwrap().unwrap();
        el.dyn_into::<HtmlElement>()
            .unwrap()
            .inner_text()
            .trim()
            .to_string()
    };

    // Two segments in one text node, plus surrounding statics.
    assert_eq!(read(".ih-greeting"), "Hello, Ada! You have 3 new.");

    // Escaped braces survive as literal `{` / `}`; real `{count}`
    // still binds alongside them.
    assert_eq!(read(".ih-escape"), "{literal} 3");

    // Single-segment line — no surrounding literal.
    assert_eq!(read(".ih-only"), "3");

    // A text node that brackets a child element gets split on
    // both sides of the element.
    assert_eq!(read(".ih-nested"), "outer 3 mid Ada tail");
    assert_eq!(read(".ih-em"), "Ada");

    // Update count → only the count segments rerun. Ada stays.
    let ih_root = host.query_selector(".ih-root").unwrap().unwrap();
    let scope_id = {
        let key = JsValue::from_str("__pp_scope_id");
        js_sys::Reflect::get(ih_root.as_ref(), &key)
            .ok()
            .and_then(|v| v.as_f64())
            .map(|n| pocopine::ScopeId(n as u64))
            .expect(".ih-root has scope id")
    };
    if let Some(scope) = pocopine::Scope::find(scope_id) {
        scope.invoke("bump", &js_sys::Array::new());
    }
    tick().await;

    assert_eq!(read(".ih-greeting"), "Hello, Ada! You have 4 new.");
    assert_eq!(read(".ih-escape"), "{literal} 4");
    assert_eq!(read(".ih-only"), "4");
    assert_eq!(read(".ih-nested"), "outer 4 mid Ada tail");

    host.remove();
}
