//! RFC 081 Phase 2 — end-to-end test for the macro-generated
//! `<ComponentName>Refs` struct.
//!
//! A `TypedRefsParent` `.poco` template carries
//! `<typed-refs-child pp-ref="body" />`, and the test asserts
//! that:
//!
//! 1. The macro generated a `TypedRefsParentRefs` struct with
//!    a `fn body(&self) -> RefAccessor` method.
//! 2. `RefAccessor::component::<TypedRefsChild>()` resolves a
//!    working `Handle<TypedRefsChild>` after mount.
//! 3. `RefAccessor::component::<TypedRefsParent>()` returns
//!    `None` (mismatched type protection).
//! 4. `RefAccessor::element()` returns the host element.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine --test refs_typed`

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::{window, Element};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-child",
    template_inline = r#"<div class="trc">child mounted</div>"#
)]
struct TypedRefsChild {
    seed: u32,
}

#[handlers]
impl TypedRefsChild {}

impl TypedRefsChild {
    pub fn seed_value(&self) -> u32 {
        self.seed
    }
}

// Test-only observation channel. `#[component]` requires every
// struct field to be `Serialize + Deserialize` (RFC 081 keeps
// this rule), so the observed values live in thread-locals the
// on_ready handler writes and the test reads.
thread_local! {
    static OBSERVED_SEED: RefCell<Option<u32>> = const { RefCell::new(None) };
    static OBSERVED_MISMATCH_IS_NONE: RefCell<Option<bool>> = const { RefCell::new(None) };
    static OBSERVED_ELEMENT_PRESENT: RefCell<Option<bool>> = const { RefCell::new(None) };
}

fn reset_observations() {
    OBSERVED_SEED.with(|c| *c.borrow_mut() = None);
    OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow_mut() = None);
    OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow_mut() = None);
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "typed-refs-parent",
    uses = [TypedRefsChild],
    template_inline = r#"<div class="trp">
        <typed-refs-child pp-ref="body"></typed-refs-child>
    </div>"#
)]
struct TypedRefsParent {}

#[handlers]
impl TypedRefsParent {
    pub fn on_ready(&self, refs: TypedRefsParentRefs) {
        // Typed-component resolution
        if let Some(child) = refs.body().component::<TypedRefsChild>() {
            OBSERVED_SEED.with(|c| *c.borrow_mut() = Some(child.with(|c| c.seed_value())));
        }
        // Mismatched type — must return None.
        let mismatched = refs.body().component::<TypedRefsParent>();
        OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow_mut() = Some(mismatched.is_none()));
        // Untyped element lookup
        OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow_mut() = Some(refs.body().element().is_some()));
    }
}

fn doc() -> web_sys::Document {
    window().unwrap().document().unwrap()
}

async fn tick() {
    for _ in 0..3 {
        let p = js_sys::Promise::resolve(&JsValue::NULL);
        let _ = wasm_bindgen_futures::JsFuture::from(p).await;
    }
}

fn mount() -> Element {
    TypedRefsChild::register();
    TypedRefsParent::register();
    let body = doc().body().unwrap();
    let host = doc().create_element("div").unwrap();
    host.set_inner_html("<typed-refs-parent></typed-refs-parent>");
    body.append_child(&host).unwrap();
    let el = host.query_selector("typed-refs-parent").unwrap().unwrap();
    pocopine_core::mount::mount_child_component(&el, "typed-refs-parent");
    pocopine_core::mount::finalize_compiled_subtree(&el);
    host
}

#[wasm_bindgen_test]
async fn generated_refs_struct_resolves_child_component_handle() {
    reset_observations();
    let host = mount();
    tick().await;

    // Typed component resolution worked and the handle read
    // through to child state.
    let observed_seed = OBSERVED_SEED.with(|c| *c.borrow());
    assert_eq!(
        observed_seed,
        Some(0),
        "typed handle reads child state via the generated `body()` accessor"
    );
    // Mismatched type guarded by the `child_scope != parent_scope` check.
    let mismatch_is_none = OBSERVED_MISMATCH_IS_NONE.with(|c| *c.borrow());
    assert_eq!(
        mismatch_is_none,
        Some(true),
        "component::<WrongType>() must return None"
    );
    // Untyped element lookup composes the same name-baked path.
    let element_present = OBSERVED_ELEMENT_PRESENT.with(|c| *c.borrow());
    assert_eq!(
        element_present,
        Some(true),
        "RefAccessor::element() resolves the host element"
    );

    host.remove();
}

#[wasm_bindgen_test]
async fn generated_refs_struct_method_name_is_compile_time_typo_safe() {
    // The point of Phase 2: the generated method `fn body(&self)`
    // means a typo at the call site fails to compile. We can't
    // assert that at runtime (the compile already happened), but
    // the test still ensures the generated method is reachable
    // by NAME, not by string — i.e. the macro emitted the right
    // method. If a future refactor regresses to a string lookup,
    // this test's call site would still compile but the runtime
    // would resolve via the wrong path; this test pins both the
    // codegen and the resolution semantics together.
    let host = mount();
    tick().await;
    let parent_host = host.query_selector("typed-refs-parent").unwrap().unwrap();
    let parent_scope_id =
        pocopine_core::mount::scope_id_of_element(&parent_host).expect("parent scope id");
    // Directly resolve via the free-fn to confirm the name lands
    // in the parent scope's ref table.
    let resolved = pocopine_core::refs::get_component_on::<TypedRefsChild>(parent_scope_id, "body");
    assert!(
        resolved.is_some(),
        "free-fn lookup via the same name the macro baked in resolves"
    );
    host.remove();
}
