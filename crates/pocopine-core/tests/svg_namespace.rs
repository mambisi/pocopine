//! RFC 068 core SVG namespace support.
//!
//! These tests intentionally live in `pocopine-core`: the SVG
//! controller helpers should work before the `pocopine` macro crate
//! enters the picture.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

#[wasm_bindgen_test]
fn svg_for_template_captures_and_clears_inline_prototype() {
    let svg = doc().create_element_ns(Some(SVG_NS), "svg").unwrap();
    svg.set_inner_html(
        r#"<template pp-for="line in lines"><line class="prototype"></line></template>"#,
    );

    let template = svg
        .query_selector("template")
        .unwrap()
        .expect("SVG template anchor exists");
    assert_eq!(
        template.children().length(),
        1,
        "fixture starts with a live SVG prototype child",
    );

    let _controller = pocopine_core::directives::for_::ForTemplate::from_element(template.clone())
        .expect("SVG template anchor should become a pp-for controller");

    assert_eq!(
        template.children().length(),
        0,
        "installing the SVG controller clears live prototype children",
    );
    assert_eq!(
        template.get_attribute("aria-hidden").as_deref(),
        Some("true")
    );
}

#[wasm_bindgen_test]
fn body_fragment_parser_materializes_svg_roots_in_svg_namespace() {
    let root = pocopine_core::templates_plan::stamp_if_body_with(
        r#"<line class="core-svg-line" x1="1" y1="2"></line>"#,
        pocopine_core::ScopeId(7),
        &JsValue::NULL,
        pocopine_core::ScopeId(7),
        |root, _, _| {
            let _ = root.set_attribute("data-installed", "true");
        },
    )
    .expect("SVG body fragment should materialize");

    assert_eq!(root.local_name(), "line");
    assert_eq!(root.namespace_uri().as_deref(), Some(SVG_NS));
    assert_eq!(root.get_attribute("x1").as_deref(), Some("1"));
    assert_eq!(
        root.get_attribute("data-installed").as_deref(),
        Some("true"),
        "fragment install closure should receive the SVG root",
    );
}

#[wasm_bindgen_test]
fn svg_bind_canonicalizes_mixed_case_attributes() {
    let svg = doc().create_element_ns(Some(SVG_NS), "svg").unwrap();
    pocopine_core::directives::bind::install_eval(
        &svg,
        &JsValue::NULL,
        "viewbox",
        Rc::new(|_| JsValue::from_str("0 0 720 360")),
    );

    assert_eq!(
        svg.get_attribute("viewBox").as_deref(),
        Some("0 0 720 360"),
        "dynamic SVG attrs must use browser-recognized canonical casing",
    );
    assert!(
        svg.get_attribute("viewbox").is_none(),
        "lowercase viewbox is just an inert attribute in SVG",
    );
}
