//! RFC 061 Phase 4 — runtime template and plan lookup consults
//! the active `app!{}` phf registry before the thread-local
//! registration maps.

#![cfg(target_arch = "wasm32")]

use pocopine::ComponentVTable;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-lookup-home",
    template_inline = r#"<div class="phf-lookup-home"></div>"#
)]
struct PhfLookupHome {}

#[handlers]
impl PhfLookupHome {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-lookup-planned",
    template_inline = r#"<div><span pp-text="label"></span></div>"#
)]
struct PhfLookupPlanned {
    label: String,
}

#[handlers]
impl PhfLookupPlanned {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-lookup-fragment-plan",
    template_inline = r#"<div><template pp-if="visible"><span pp-text="label"></span></template></div>"#
)]
struct PhfLookupFragmentPlan {
    visible: bool,
    label: String,
}

#[handlers]
impl PhfLookupFragmentPlan {}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "phf-lookup-fallback",
    template_inline = r#"<section class="phf-lookup-fallback"></section>"#
)]
struct PhfLookupFallback {}

#[handlers]
impl PhfLookupFallback {}

#[wasm_bindgen_test]
fn phf_template_for_returns_static_html() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-lookup-home" => PhfLookupHome::__POCO_VTABLE,
    };

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine::__private::set_active_phf_registry(&REGISTRY);

    let html = pocopine_core::templates::template_for("phf-lookup-home")
        .expect("template comes from active phf registry");
    assert!(html.contains(r#"class="phf-lookup-home""#));
    assert!(
        html.contains(r#"data-pp-scope-id="phf-lookup-home""#),
        "phf template is the compiled HTML, not the raw template source",
    );
}

#[wasm_bindgen_test]
fn phf_overrides_thread_local_template_entry() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-lookup-home" => PhfLookupHome::__POCO_VTABLE,
    };

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine_core::templates::register_template(
        "phf-lookup-home",
        r#"<div class="thread-local-wrong"></div>"#,
    );
    pocopine::__private::set_active_phf_registry(&REGISTRY);

    let html = pocopine_core::templates::template_for("phf-lookup-home")
        .expect("template comes from active phf registry");
    assert!(html.contains(r#"class="phf-lookup-home""#));
    assert!(!html.contains("thread-local-wrong"));
}

#[wasm_bindgen_test]
fn clear_active_phf_registry_clears_template_clone_cache() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-lookup-home" => PhfLookupHome::__POCO_VTABLE,
    };

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine_core::templates::register_template(
        "phf-lookup-home",
        r#"<div class="cached-thread-local-wrong"></div>"#,
    );
    let wrong = pocopine_core::templates::template_clone_for("phf-lookup-home")
        .expect("thread-local template clone cached");
    assert_eq!(
        wrong
            .first_element_child()
            .and_then(|el| el.get_attribute("class"))
            .as_deref(),
        Some("cached-thread-local-wrong"),
    );

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine::__private::set_active_phf_registry(&REGISTRY);
    let cloned = pocopine_core::templates::template_clone_for("phf-lookup-home")
        .expect("phf template clone should not reuse stale thread-local cache");
    assert_eq!(
        cloned
            .first_element_child()
            .and_then(|el| el.get_attribute("class"))
            .as_deref(),
        Some("phf-lookup-home"),
    );
}

#[wasm_bindgen_test]
fn phf_template_plan_for_returns_static_plan() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-lookup-planned" => PhfLookupPlanned::__POCO_VTABLE,
    };

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine::__private::set_active_phf_registry(&REGISTRY);

    let plan = pocopine_core::templates_plan::template_plan_for("phf-lookup-planned")
        .expect("plan comes from active phf registry");
    assert_eq!(plan.bindings.len(), 1);
}

#[wasm_bindgen_test]
fn phf_template_plan_supports_fragment_helpers() {
    static REGISTRY: pocopine::__private::phf::Map<&'static str, &'static ComponentVTable> = pocopine::__private::phf::phf_map! {
        "phf-lookup-fragment-plan" => PhfLookupFragmentPlan::__POCO_VTABLE,
    };

    pocopine::__private::clear_active_phf_registry_for_test();
    pocopine::__private::set_active_phf_registry(&REGISTRY);

    let plan = pocopine_core::templates_plan::template_plan_for("phf-lookup-fragment-plan")
        .expect("fragment plan comes from active phf registry");
    assert_eq!(plan.if_plans.len(), 1);
    assert!(
        plan.if_plans[0].body.is_some(),
        "fragment helper fn is reachable through the phf plan",
    );
}

#[wasm_bindgen_test]
fn phf_falls_back_to_thread_local_template_entry() {
    pocopine::__private::clear_active_phf_registry_for_test();
    PhfLookupFallback::register();

    let html = pocopine_core::templates::template_for("phf-lookup-fallback")
        .expect("template falls back to thread-local registry");
    assert!(html.contains(r#"class="phf-lookup-fallback""#));
}
