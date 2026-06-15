//! Pine Layout browser tests. Run with
//! `wasm-pack test --firefox --headless crates/pine-layout`.
//!
//! The breakpoint *state machine* (nav-mode derivation, toggle
//! semantics, the `Breakpoint` enum) is covered by deterministic host
//! unit tests in `src/`; those don't need a viewport. These browser
//! tests cover the DOM contract that needs a real document: rendered
//! `data-*` hooks, ARIA wiring, and the drawer toggle.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen::JsCast;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{Element, window};

wasm_bindgen_test_configure!(run_in_browser);

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

/// Mount a fragment of custom-element markup and fire every
/// registered component's mount/ready hooks (the same shape Pine's
/// own browser tests use).
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

fn q(host: &Element, selector: &str) -> Element {
    host.query_selector(selector)
        .unwrap()
        .unwrap_or_else(|| panic!("no element for `{selector}`"))
}

fn dispatch_click(el: &Element) {
    el.dispatch_event(&web_sys::Event::new("click").unwrap())
        .unwrap();
}

// ── structural primitives — data-* hooks ──────────────────────────

#[wasm_bindgen_test]
async fn grid_reflects_cols_and_gap() {
    let host = mount(
        r#"<pine-grid cols="12" gap="4"><pine-grid-item span="6" start="3"></pine-grid-item></pine-grid>"#,
    );
    settle().await;
    let grid = q(&host, ".pine-grid");
    assert_eq!(grid.get_attribute("data-cols").as_deref(), Some("12"));
    assert_eq!(grid.get_attribute("data-gap").as_deref(), Some("4"));
    let item = q(&host, ".pine-grid-item");
    assert_eq!(item.get_attribute("data-span").as_deref(), Some("6"));
    assert_eq!(item.get_attribute("data-start").as_deref(), Some("3"));
}

#[wasm_bindgen_test]
async fn stack_and_inline_reflect_props() {
    let host = mount(
        r#"<pine-stack gap="6" align="center"></pine-stack>
           <pine-inline gap="2" justify="between" wrap="true"></pine-inline>"#,
    );
    settle().await;
    let stack = q(&host, ".pine-stack");
    assert_eq!(stack.get_attribute("data-gap").as_deref(), Some("6"));
    assert_eq!(stack.get_attribute("data-align").as_deref(), Some("center"));
    let inline = q(&host, ".pine-inline");
    assert_eq!(
        inline.get_attribute("data-justify").as_deref(),
        Some("between")
    );
    assert!(
        inline.has_attribute("data-wrap"),
        "data-wrap present when wrap=true"
    );
}

#[wasm_bindgen_test]
async fn container_reflects_size_and_safe_area() {
    let host = mount(r#"<pine-container size="lg" safe-area="true"></pine-container>"#);
    settle().await;
    let c = q(&host, ".pine-container");
    assert_eq!(c.get_attribute("data-size").as_deref(), Some("lg"));
    assert!(c.has_attribute("data-safe-area"));
}

// ── breakpoint provider ───────────────────────────────────────────

#[wasm_bindgen_test]
async fn breakpoint_provider_resolves_a_known_tier() {
    let host = mount(r#"<pine-breakpoint></pine-breakpoint>"#);
    settle().await;
    let bp = q(&host, ".pine-breakpoint");
    let token = bp.get_attribute("data-breakpoint").unwrap_or_default();
    assert!(
        ["base", "sm", "md", "lg", "xl", "2xl"].contains(&token.as_str()),
        "resolved tier `{token}` is not a known token"
    );
}

// ── app shell — ARIA wiring + drawer toggle ───────────────────────

#[wasm_bindgen_test]
async fn app_shell_wires_aria_controls_to_the_sidebar() {
    // Thresholds at 2xl keep the shell in drawer mode for any realistic
    // headless viewport (< 1536px).
    let host = mount(
        r#"<pine-app-shell rail-at="2xl" sidebar-at="2xl">
             <pine-app-shell-header>
               <pine-app-shell-trigger>menu</pine-app-shell-trigger>
             </pine-app-shell-header>
             <pine-app-shell-sidebar label="Main">nav</pine-app-shell-sidebar>
             <pine-app-shell-content>page</pine-app-shell-content>
           </pine-app-shell>"#,
    );
    settle().await;

    let sidebar = q(&host, ".pine-app-shell-sidebar");
    let trigger = q(&host, ".pine-app-shell-trigger");
    assert_eq!(sidebar.get_attribute("role").as_deref(), Some("navigation"));
    let nav_id = sidebar.get_attribute("id").unwrap_or_default();
    assert!(!nav_id.is_empty(), "sidebar has a generated id");
    assert_eq!(
        trigger.get_attribute("aria-controls").as_deref(),
        Some(nav_id.as_str()),
        "trigger aria-controls points at the sidebar id"
    );
}

#[wasm_bindgen_test]
async fn trigger_toggles_the_drawer_in_drawer_mode() {
    let host = mount(
        r#"<pine-app-shell rail-at="2xl" sidebar-at="2xl">
             <pine-app-shell-trigger>menu</pine-app-shell-trigger>
             <pine-app-shell-sidebar label="Main">nav</pine-app-shell-sidebar>
           </pine-app-shell>"#,
    );
    settle().await;

    let shell = q(&host, ".pine-app-shell");
    let trigger = q(&host, ".pine-app-shell-trigger");
    let sidebar = q(&host, ".pine-app-shell-sidebar");

    // Precondition: drawer mode, closed.
    assert_eq!(
        shell.get_attribute("data-nav-mode").as_deref(),
        Some("drawer")
    );
    assert_eq!(
        sidebar.get_attribute("data-state").as_deref(),
        Some("closed")
    );
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false")
    );

    dispatch_click(&trigger);
    settle().await;
    assert!(shell.has_attribute("data-nav-open"), "shell marked open");
    assert_eq!(sidebar.get_attribute("data-state").as_deref(), Some("open"));
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("true")
    );

    dispatch_click(&trigger);
    settle().await;
    assert_eq!(
        sidebar.get_attribute("data-state").as_deref(),
        Some("closed")
    );
    assert_eq!(
        trigger.get_attribute("aria-expanded").as_deref(),
        Some("false")
    );
}
