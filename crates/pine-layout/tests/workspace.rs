//! Pine Workspace (RFC-105) browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine-layout`.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

fn mount(host_html: &str) -> Element {
    pine_layout::register_all();
    pocopine_core::animate::disable_transitions();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html(host_html);
    body.append_child(&host).unwrap();

    let names = pocopine_core::templates::registered_template_names();
    if !names.is_empty() {
        let selector = names.join(",");
        if let Ok(matches) = host.query_selector_all(&selector) {
            for i in 0..matches.length() {
                let Some(node) = matches.item(i) else {
                    continue;
                };
                let Ok(el) = node.dyn_into::<Element>() else {
                    continue;
                };
                if !host.contains(Some(el.as_ref())) {
                    continue;
                }
                let tag = el.local_name();
                pocopine_core::mount::mount_child_component(&el, &tag);
                pocopine_core::mount::finalize_compiled_subtree(&el);
            }
        }
    }
    host
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&wasm_bindgen::JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}
async fn sleep_ms(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve: js_sys::Function, _reject| {
        let w = window().unwrap();
        let _ =
            w.set_timeout_with_callback_and_timeout_and_arguments_0(resolve.unchecked_ref(), ms);
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}
async fn settle() {
    tick().await;
    sleep_ms(0).await;
    tick().await;
}
fn q(host: &Element, sel: &str) -> Element {
    host.query_selector(sel)
        .unwrap()
        .unwrap_or_else(|| panic!("no element for `{sel}`"))
}

const FRAME: &str = r#"
<pine-workspace>
  <pine-workspace-header>head</pine-workspace-header>
  <pine-workspace-sidebar label="Nav" collapse-below="base">nav</pine-workspace-sidebar>
  <pine-workspace-main>main</pine-workspace-main>
  <pine-workspace-aside label="Detail" size="320">detail</pine-workspace-aside>
  <pine-workspace-footer>status</pine-workspace-footer>
</pine-workspace>
"#;

#[wasm_bindgen_test]
async fn workspace_regions_render_with_landmarks() {
    let host = mount(FRAME);
    settle().await;
    assert_eq!(
        q(&host, ".pine-workspace-header")
            .get_attribute("role")
            .as_deref(),
        Some("banner")
    );
    assert_eq!(
        q(&host, ".pine-workspace-main")
            .get_attribute("role")
            .as_deref(),
        Some("main")
    );
    assert_eq!(
        q(&host, ".pine-workspace-sidebar")
            .get_attribute("role")
            .as_deref(),
        Some("navigation")
    );
    assert_eq!(
        q(&host, ".pine-workspace-aside")
            .get_attribute("role")
            .as_deref(),
        Some("complementary")
    );
    assert!(q(&host, ".pine-workspace").has_attribute("data-breakpoint"));
}

#[wasm_bindgen_test]
async fn sidebar_publishes_size_var_and_resize_handle() {
    let host = mount(FRAME);
    settle().await;
    let sb = q(&host, ".pine-workspace-sidebar");
    assert_eq!(sb.get_attribute("data-state").as_deref(), Some("expanded"));
    let style = sb.get_attribute("style").unwrap_or_default();
    assert!(
        style.contains("--pine-sidebar-size:"),
        "sidebar publishes its width var, got `{style}`"
    );
    let handle = q(&host, ".pine-workspace-sidebar-resize");
    assert_eq!(handle.get_attribute("role").as_deref(), Some("separator"));
    assert!(handle.has_attribute("aria-valuenow"));
}

#[wasm_bindgen_test]
async fn aside_renders_state_side_and_size_var() {
    let host = mount(FRAME);
    settle().await;
    let aside = q(&host, ".pine-workspace-aside");
    // Closed until opened (app-controlled via pp-model:open).
    assert_eq!(
        aside.get_attribute("data-aside-state").as_deref(),
        Some("closed")
    );
    assert_eq!(aside.get_attribute("data-side").as_deref(), Some("end"));
    let style = aside.get_attribute("style").unwrap_or_default();
    assert!(style.contains("--pine-aside-size:"), "got `{style}`");
    assert!(!aside.get_attribute("id").unwrap_or_default().is_empty());
}

#[wasm_bindgen_test]
async fn sidebar_auto_collapses_below_threshold() {
    // Default collapse_below="lg": below lg the sidebar collapses to the rail.
    let host = mount(
        r#"<pine-workspace><pine-workspace-sidebar label="Nav">nav</pine-workspace-sidebar></pine-workspace>"#,
    );
    settle().await;
    let bp = q(&host, ".pine-workspace")
        .get_attribute("data-breakpoint")
        .unwrap_or_default();
    let sb = q(&host, ".pine-workspace-sidebar");
    if matches!(bp.as_str(), "base" | "sm" | "md") {
        assert_eq!(
            sb.get_attribute("data-state").as_deref(),
            Some("collapsed"),
            "below lg ({bp}) the sidebar auto-collapses to the rail"
        );
    }
}
