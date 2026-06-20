//! RFC-099 Phase 2c — `resolve_node_path` walks element children only,
//! matching the macro's element-only `node_path` convention. This is
//! the runtime node resolver the hydration claim walk uses to attach
//! bindings to server-rendered DOM.
//!
//! Run with `wasm-pack test --firefox --headless crates/pocopine-core
//! --test resolve_node_path` (needs a real DOM).

#![cfg(target_arch = "wasm32")]

use pocopine_core::templates_plan::resolve_node_path;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

fn tag(el: &web_sys::Element) -> String {
    el.tag_name().to_lowercase()
}

#[wasm_bindgen_test]
fn walks_element_children_only() {
    let doc = window().unwrap().document().unwrap();
    let root = doc.create_element("div").unwrap();
    // A text node sits between the two element children; it must NOT
    // shift the element index.
    root.set_inner_html("<span></span>some text<p><b></b><i></i></p>");

    // Empty path → the root itself.
    assert_eq!(tag(&resolve_node_path(&root, &[]).unwrap()), "div");

    // Element child 0 = <span>, child 1 = <p> (text node skipped).
    assert_eq!(tag(&resolve_node_path(&root, &[0]).unwrap()), "span");
    assert_eq!(tag(&resolve_node_path(&root, &[1]).unwrap()), "p");

    // Nested: <p>'s element child 1 = <i>.
    assert_eq!(tag(&resolve_node_path(&root, &[1, 1]).unwrap()), "i");
    assert_eq!(tag(&resolve_node_path(&root, &[1, 0]).unwrap()), "b");

    // Out of range → None (a malformed/mismatched path doesn't panic).
    assert!(resolve_node_path(&root, &[5]).is_none());
    assert!(resolve_node_path(&root, &[0, 0]).is_none()); // <span> has no children
}
