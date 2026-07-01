//! Issue #260 — `#[computed]` readonly fields on `#[store]` singletons.
//!
//! Components already expose `#[computed]` synthetic fields; a `#[store]`
//! is just a singleton component scope, so the same machinery applies —
//! the only missing wire was installing the store's computed entries at
//! registration (stores have no DOM `setup` lifecycle) and making the
//! store's `ComponentState` computed-aware.
//!
//! These are the runtime halves of the acceptance criteria:
//! * `$store.<name>.<computed>` resolves in a template.
//! * it re-derives when a *source* field changes through
//!   `store::<T>().update(...)`.
//! * a computed can depend on another computed (`filtered_len` reads
//!   `filtered`).
//! * an *unrelated* field write does not re-run the computed body
//!   (lazy memoization) and does not serve a stale projection-cache
//!   value.
//! * the synthetic field is readonly through the `$store` proxy.
//! * the value is available immediately after `App::store::<T>()`.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;

use pocopine::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, HtmlElement, window};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn text_of(el: &Element) -> String {
    el.clone()
        .dyn_into::<HtmlElement>()
        .unwrap()
        .inner_text()
        .trim()
        .to_string()
}

thread_local! {
    /// How many times the `filtered` computed *body* has run. Proves
    /// memoization: an unrelated store write must leave this untouched.
    static FILTERED_RUNS: Cell<u32> = const { Cell::new(0) };
}

fn filtered_runs() -> u32 {
    FILTERED_RUNS.with(Cell::get)
}

// ── the store under test ───────────────────────────────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[store(name = "demo")]
struct DemoStore {
    items: Vec<String>,
    filter: String,
    /// Not read by any computed — writing it must not recompute.
    note: String,
}

#[handlers]
impl DemoStore {
    /// Raw-field computed: depends on `items` + `filter` by parameter.
    #[computed]
    fn filtered(items: &[String], filter: &str) -> Vec<String> {
        FILTERED_RUNS.with(|c| c.set(c.get() + 1));
        let q = filter.trim().to_lowercase();
        items
            .iter()
            .filter(|i| q.is_empty() || i.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// Computed-on-computed: depends on the `filtered` computed field.
    #[computed]
    fn filtered_len(filtered: Vec<String>) -> usize {
        filtered.len()
    }
}

// ── host component that binds the store computed ───────────────────

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[component(template_inline = r#"
<div class="demo-host">
  <span class="d-len" pp-text="$store.demo.filtered_len"></span>
</div>
"#)]
struct DemoHost {}

#[handlers]
impl DemoHost {}

/// Register the store (installing its computed entries), reset it to a
/// known-clean slate — the singleton persists across tests in the same
/// wasm module — and mount a host that binds `$store.demo.filtered_len`.
fn mount_host() -> Element {
    <DemoHost as pocopine::__private::Component>::register();
    // `App::store::<T>()` is the acceptance path — registration installs
    // the computed nodes before any read.
    let _ = App::new().store::<DemoStore>();
    store::<DemoStore>().update(|s| *s = DemoStore::default());
    pocopine_core::animate::disable_transitions();

    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    let root = doc()
        .create_element(<DemoHost as pocopine::__private::Component>::NAME)
        .unwrap();
    host.append_child(&root).unwrap();
    body.append_child(&host).unwrap();
    let mounted = pocopine::App::mount_subtree::<DemoHost>(&root);
    pocopine_core::mount::finalize_compiled_subtree(&root);
    mounted.leak();
    host
}

#[wasm_bindgen_test]
async fn store_computed_available_immediately_and_updates() {
    let host = mount_host();
    tick().await;

    let len = host.query_selector(".d-len").unwrap().expect("len span");
    // Available immediately after `App::store::<Demo>()` — the empty
    // store derives a length of 0 on the very first render.
    assert_eq!(text_of(&len), "0", "initial filtered_len for empty store");

    // A source-field write re-derives the (chained) computed.
    store::<DemoStore>().update(|s| {
        s.items = vec!["alpha".into(), "beta".into(), "gamma".into()];
    });
    tick().await;
    tick().await;
    assert_eq!(text_of(&len), "3", "filtered_len after items set");

    // Narrowing the other source field re-derives too. All three
    // contain 'a'.
    store::<DemoStore>().update(|s| s.filter = "a".into());
    tick().await;
    tick().await;
    assert_eq!(text_of(&len), "3", "filter 'a' matches all three");

    // Only "alpha" contains "al".
    store::<DemoStore>().update(|s| s.filter = "al".into());
    tick().await;
    tick().await;
    assert_eq!(text_of(&len), "1", "filter 'al' matches only alpha");

    host.remove();
}

#[wasm_bindgen_test]
async fn store_computed_skips_recompute_for_unrelated_writes() {
    let host = mount_host();
    tick().await;

    let len = host.query_selector(".d-len").unwrap().expect("len span");
    // Seed a source change so the computed has run and is memoized.
    store::<DemoStore>().update(|s| s.items = vec!["one".into(), "two".into()]);
    tick().await;
    tick().await;
    assert_eq!(text_of(&len), "2", "filtered_len after seeding items");
    let runs_before = filtered_runs();

    // Writing an unrelated field must NOT re-run the computed body...
    store::<DemoStore>().update(|s| s.note = "hello".into());
    tick().await;
    tick().await;
    assert_eq!(
        filtered_runs(),
        runs_before,
        "unrelated store write must not recompute the body",
    );
    // ...and the value stays correct (no stale projection cache served).
    assert_eq!(text_of(&len), "2", "value stable after unrelated write");

    host.remove();
}

#[wasm_bindgen_test]
async fn store_computed_is_readonly_through_proxy() {
    let host = mount_host();
    tick().await;

    store::<DemoStore>().update(|s| s.items = vec!["x".into(), "y".into()]);
    tick().await;
    tick().await;

    // Reach the `demo` proxy through the `$store` container and try to
    // overwrite the synthetic field directly.
    let stores = pocopine_core::store::stores_object();
    let demo = js_sys::Reflect::get(&stores, &JsValue::from_str("demo")).unwrap();
    let before = js_sys::Reflect::get(&demo, &JsValue::from_str("filtered_len")).unwrap();
    assert_eq!(before.as_f64(), Some(2.0), "computed reads through proxy");

    let _ = js_sys::Reflect::set(
        &demo,
        &JsValue::from_str("filtered_len"),
        &JsValue::from_f64(999.0),
    );
    let after = js_sys::Reflect::get(&demo, &JsValue::from_str("filtered_len")).unwrap();
    assert_eq!(
        after.as_f64(),
        Some(2.0),
        "computed field is readonly — the proxy write is a no-op",
    );

    host.remove();
}
