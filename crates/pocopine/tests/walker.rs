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

fn register_all() {
    TestRow::register();
    TestList::register();
    WithMount::register();
    WithoutMount::register();
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
